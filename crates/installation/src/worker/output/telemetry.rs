//! Routine history RPC observations never enter the canonical output queue.
//! Stderr is untrusted: closed-schema numbers still require current redactions.
use super::{AGENT_PERFORMANCE_RECORD, DIAGNOSTIC_FRAGMENT_LIMIT};
use crate::{accounts::Lease, run_output, service::Service};
use serde::Serialize;
use std::{
    collections::HashSet,
    future::Future,
    str::SplitAsciiWhitespace,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{sync::watch, task::JoinHandle, time::Instant};
use tokio_util::sync::CancellationToken;
use tracing::instrument::WithSubscriber;

const PERIOD: Duration = Duration::from_secs(1);
const MAX_DURATION_MS: u64 = 86_400_000;

#[derive(Clone, Copy, Default, Serialize)]
struct Counts {
    started: u64,
    completed: u64,
    failed: u64,
    slow: u64,
    elapsed_ms_total: u64,
    elapsed_ms_min: Option<u64>,
    elapsed_ms_max: u64,
    queue_ms_total: u64,
    queue_ms_max: u64,
    queue_unknown: u64,
}

#[derive(Clone, Copy, Default, Serialize)]
struct Snapshot {
    items: Counts,
    turns: Counts,
    recognized_records: u64,
    unsupported_records: u64,
    overflow: bool,
    coverage_ms: u64,
    reader_closed: bool,
    eof: bool,
}

#[derive(Clone, Copy)]
enum Method {
    Items,
    Turns,
}

#[derive(Clone, Copy)]
enum Sample {
    Started(Method),
    Completed {
        method: Method,
        elapsed_ms: u64,
        queue_ms: Option<u64>,
        succeeded: bool,
    },
}

fn add(value: &mut u64, amount: u64, overflow: &mut bool) {
    *overflow |= value.checked_add(amount).is_none();
    *value = value.saturating_add(amount);
}

fn field<'a>(fields: &mut SplitAsciiWhitespace<'a>, name: &str) -> Option<&'a str> {
    let value = fields.next()?.strip_prefix(name)?.strip_prefix('=')?;
    if let Some(quoted) = value.strip_prefix('"') {
        return quoted.strip_suffix('"');
    }
    Some(value)
}

fn duration(value: &str) -> Option<u64> {
    let value = value.parse::<u64>().ok()?;
    (value <= MAX_DURATION_MS).then_some(value)
}

fn parse(line: &str) -> Option<Sample> {
    // Never recognize a shortened fragment or a partial EOF record as a schema.
    if line.len() > DIAGNOSTIC_FRAGMENT_LIMIT || !line.ends_with(['\r', '\n']) {
        return None;
    }
    let prefix = AGENT_PERFORMANCE_RECORD.captures(line)?;
    if prefix.name("level")?.as_str() != "INFO" {
        return None;
    }
    let mut fields = line[prefix.get(0)?.end()..].split_ascii_whitespace();
    if field(&mut fields, "operation")? != "codex_rpc" {
        return None;
    }
    let method = match field(&mut fields, "method")? {
        "thread/items/list" => Method::Items,
        "thread/turns/list" => Method::Turns,
        _ => return None,
    };
    // Validate but never retain a child-provided identity.
    field(&mut fields, "request_id")?.parse::<u64>().ok()?;
    let sample = match field(&mut fields, "event")? {
        "started" => Sample::Started(method),
        "completed" => {
            let elapsed_ms = duration(field(&mut fields, "elapsed_ms")?)?;
            let mut next = fields.clone();
            let queue_ms = if let Some(value) = field(&mut next, "queue_ms") {
                let value = duration(value)?;
                fields = next;
                Some(value)
            } else {
                None
            };
            if field(&mut fields, "timeout_ms")? != "20000" {
                return None;
            }
            let succeeded = match field(&mut fields, "succeeded")? {
                "true" => true,
                "false" => false,
                _ => return None,
            };
            if fields.next().is_some() {
                return None;
            }
            return Some(Sample::Completed {
                method,
                elapsed_ms,
                queue_ms,
                succeeded,
            });
        }
        _ => return None,
    };
    if field(&mut fields, "timeout_ms")? != "20000" || fields.next().is_some() {
        return None;
    }
    Some(sample)
}

/// One fixed cumulative snapshot; replacing it coalesces observations, not counts.
pub(in crate::worker) struct Publisher {
    snapshot: Snapshot,
    sender: watch::Sender<Snapshot>,
    started: Instant,
}

impl Publisher {
    pub(in crate::worker) fn observe(&mut self, line: &str) -> bool {
        let Some(sample) = parse(line) else {
            add(
                &mut self.snapshot.unsupported_records,
                1,
                &mut self.snapshot.overflow,
            );
            self.publish();
            return false;
        };
        add(
            &mut self.snapshot.recognized_records,
            1,
            &mut self.snapshot.overflow,
        );
        let (Sample::Started(method) | Sample::Completed { method, .. }) = sample;
        let counts = match method {
            Method::Items => &mut self.snapshot.items,
            Method::Turns => &mut self.snapshot.turns,
        };
        match sample {
            Sample::Started(_) => add(&mut counts.started, 1, &mut self.snapshot.overflow),
            Sample::Completed {
                elapsed_ms,
                queue_ms,
                succeeded,
                ..
            } => {
                add(&mut counts.completed, 1, &mut self.snapshot.overflow);
                add(
                    &mut counts.failed,
                    u64::from(!succeeded),
                    &mut self.snapshot.overflow,
                );
                add(
                    &mut counts.slow,
                    u64::from(elapsed_ms >= 100),
                    &mut self.snapshot.overflow,
                );
                add(
                    &mut counts.elapsed_ms_total,
                    elapsed_ms,
                    &mut self.snapshot.overflow,
                );
                counts.elapsed_ms_min = Some(
                    counts
                        .elapsed_ms_min
                        .map_or(elapsed_ms, |old| old.min(elapsed_ms)),
                );
                counts.elapsed_ms_max = counts.elapsed_ms_max.max(elapsed_ms);
                if let Some(queue_ms) = queue_ms {
                    add(
                        &mut counts.queue_ms_total,
                        queue_ms,
                        &mut self.snapshot.overflow,
                    );
                    counts.queue_ms_max = counts.queue_ms_max.max(queue_ms);
                } else {
                    add(&mut counts.queue_unknown, 1, &mut self.snapshot.overflow);
                }
            }
        }
        self.publish();
        true
    }

    fn publish(&mut self) {
        self.snapshot.coverage_ms = self.started.elapsed().as_millis().min(u64::MAX.into()) as u64;
        self.sender.send_replace(self.snapshot);
    }

    pub(in crate::worker) fn finish(&mut self, eof: bool) {
        self.snapshot.reader_closed = true;
        self.snapshot.eof = eof;
        self.publish();
    }
}

/// The same-attempt union of secrets actually observed on either refresh path.
#[derive(Default)]
struct Redactions(Mutex<KnownSecrets>);

#[derive(Default)]
struct KnownSecrets {
    values: Vec<Arc<str>>,
    membership: HashSet<Arc<str>>,
}

impl Redactions {
    fn merge(&self, observed: &mut Vec<String>) {
        let present: HashSet<&str> = observed.iter().map(String::as_str).collect();
        let additional: Vec<Arc<str>> = {
            let mut known = self
                .0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            for value in observed.iter() {
                if !known.membership.contains(value.as_str()) {
                    let value: Arc<str> = Arc::from(value.as_str());
                    known.membership.insert(value.clone());
                    known.values.push(value);
                }
            }
            known
                .values
                .iter()
                .filter(|value| !present.contains(value.as_ref()))
                .cloned()
                .collect()
        };
        drop(present);
        observed.extend(additional.iter().map(ToString::to_string));
    }
}

/// Scoped to one supervised attempt. Cancellation cannot leave a historical
/// reporter waiting on the Store, and never waits for it on the canonical path.
pub(in crate::worker) struct Diagnostics {
    redactions: Arc<Redactions>,
    stop: CancellationToken,
    task: JoinHandle<()>,
}

impl Drop for Diagnostics {
    fn drop(&mut self) {
        self.stop.cancel();
        self.task.abort();
    }
}

impl Diagnostics {
    pub(in crate::worker) fn start(
        service: Arc<Service>,
        account: Option<Lease>,
        mut secrets: Vec<String>,
        run: &str,
        attempt: &str,
    ) -> (Self, Publisher) {
        let redactions = Arc::new(Redactions::default());
        redactions.merge(&mut secrets);
        let stop = CancellationToken::new();
        let (sender, receiver) = watch::channel(Snapshot::default());
        let refresh_service = service.clone();
        let reporter = Reporter {
            service,
            secrets,
            redactions: redactions.clone(),
            receiver,
            stop: stop.clone(),
            run: crate::performance::identity(run).to_owned(),
            attempt: crate::performance::identity(attempt).to_owned(),
        };
        let task = tokio::spawn(
            reporter
                .run(move || {
                    let service = refresh_service.clone();
                    let account = account.clone();
                    async move {
                        match account {
                            Some(account) => service.accounts.redactions(&service, &account).await,
                            None => Ok(Vec::new()),
                        }
                    }
                })
                .with_current_subscriber(),
        );
        (
            Self {
                redactions,
                stop,
                task,
            },
            Publisher {
                snapshot: Snapshot::default(),
                sender,
                started: Instant::now(),
            },
        )
    }

    pub(in crate::worker) fn redactions(&self, known: &mut Vec<String>) {
        self.redactions.merge(known);
    }

    #[cfg(test)]
    pub(in crate::worker) async fn completed(&mut self) {
        (&mut self.task).await.unwrap();
    }
}

struct Reporter {
    service: Arc<Service>,
    secrets: Vec<String>,
    redactions: Arc<Redactions>,
    receiver: watch::Receiver<Snapshot>,
    stop: CancellationToken,
    run: String,
    attempt: String,
}

impl Reporter {
    async fn run<F, R>(self, mut refresh: F)
    where
        F: FnMut() -> R,
        R: Future<Output = crate::error::Result<Vec<String>>>,
    {
        let Self {
            service,
            mut secrets,
            redactions,
            mut receiver,
            stop,
            run,
            attempt,
        } = self;
        let mut next = Instant::now();
        let mut rejected = 0_u64;
        loop {
            let changed = tokio::select! {
                () = stop.cancelled() => return,
                () = service.shutdown.cancelled() => return,
                changed = receiver.changed() => changed,
            };
            if changed.is_err() {
                // No EOF marker means read/consumer cancellation, not a complete drain.
                return;
            }
            let mut snapshot = *receiver.borrow_and_update();
            if !snapshot.reader_closed {
                tokio::select! {
                    () = stop.cancelled() => return,
                    () = service.shutdown.cancelled() => return,
                    () = tokio::time::sleep_until(next) => {},
                    changed = receiver.wait_for(|latest| latest.reader_closed) => {
                        if changed.is_err() {
                            return;
                        }
                    },
                }
                snapshot = *receiver.borrow_and_update();
            }
            if snapshot.recognized_records == 0 || !crate::performance::enabled() {
                if snapshot.reader_closed {
                    return;
                }
                next = Instant::now() + PERIOD;
                continue;
            }
            let fresh = tokio::select! {
                () = stop.cancelled() => return,
                () = service.shutdown.cancelled() => return,
                fresh = refresh() => fresh,
            };
            let Ok(fresh) = fresh else {
                // Privacy fails closed; no raw summary or error text fallback.
                crate::performance::try_record(|_| {
                    tracing::warn!(target: "cairn_performance", parent: None,
                        operation = "agent_rpc_summary", event = "redaction_unavailable");
                });
                return;
            };
            for value in fresh {
                if !secrets.contains(&value) {
                    secrets.push(value);
                }
            }
            redactions.merge(&mut secrets);
            let summary = serde_json::json!({
                "runId": run,
                "attemptId": attempt,
                "snapshot": snapshot,
                "rejectedReports": rejected,
                "counts": "cumulative; overflow means lower bounds",
                "coverage": "recognized untrusted stderr schema; no authoritative agent progress",
            });
            let summary = run_output::redact(&summary.to_string(), &secrets);
            if !crate::performance::try_record(move |delivery| {
                tracing::info!(target: "cairn_performance", parent: None,
                    operation = "agent_rpc_summary", source = "agent_process",
                    record = %summary, diagnostics_queue_us = delivery.queue_us,
                    dropped_reports_total = delivery.dropped_reports_total);
            }) {
                rejected = rejected.saturating_add(1);
            }
            if snapshot.reader_closed {
                return;
            }
            next = Instant::now() + PERIOD;
        }
    }
}

#[cfg(test)]
mod tests;
