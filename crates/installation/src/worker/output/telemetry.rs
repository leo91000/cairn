//! Routine history RPC declarations never enter the canonical output queue.
//! Child payload numbers are validated and discarded, never timing measurements.
use super::{AGENT_PERFORMANCE_RECORD, DIAGNOSTIC_FRAGMENT_LIMIT};
use serde::Serialize;
use std::str::SplitAsciiWhitespace;
use tokio::time::{Duration, Instant};

const PERIOD: Duration = Duration::from_secs(1);
const MAX_DURATION_MS: u64 = 86_400_000;

/// Counts of locally observed schema declarations, not authoritative RPC outcomes.
#[derive(Clone, Copy, Default, Serialize)]
struct Counts {
    started_records: u64,
    completed_records: u64,
    declared_failure_records: u64,
    missing_queue_field_records: u64,
}

#[derive(Clone, Copy, Default, Serialize)]
struct Snapshot {
    items: Counts,
    turns: Counts,
    recognized_records: u64,
    unsupported_records: u64,
    overflow: bool,
    local_reader_elapsed_ms: u64,
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
        queue_field_present: bool,
        declared_success: bool,
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
            duration(field(&mut fields, "elapsed_ms")?)?;
            let mut next = fields.clone();
            let queue_field_present = if let Some(value) = field(&mut next, "queue_ms") {
                duration(value)?;
                fields = next;
                true
            } else {
                false
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
                queue_field_present,
                declared_success: succeeded,
            });
        }
        _ => return None,
    };
    if field(&mut fields, "timeout_ms")? != "20000" || fields.next().is_some() {
        return None;
    }
    Some(sample)
}

/// Fixed observation state. No child IDs, timestamps, latency values, spans or
/// strings survive parsing; only local counts of fixed schema classifications.
pub(in crate::worker) struct Publisher {
    snapshot: Snapshot,
    started: Instant,
    next_report: Instant,
    run: String,
    attempt: String,
    dispatch: tracing::Dispatch,
    rejected_reports: u64,
    #[cfg(test)]
    accepted: Option<tokio_util::sync::CancellationToken>,
}

impl Publisher {
    pub(in crate::worker) fn new(run: &str, attempt: &str) -> Self {
        let now = Instant::now();
        Self {
            snapshot: Snapshot::default(),
            started: now,
            next_report: now,
            run: crate::performance::identity(run).to_owned(),
            attempt: crate::performance::identity(attempt).to_owned(),
            dispatch: tracing::dispatcher::get_default(Clone::clone),
            rejected_reports: 0,
            #[cfg(test)]
            accepted: None,
        }
    }

    /// Acknowledge classification at the actual reader seam, without redaction effects.
    #[cfg(test)]
    pub(in crate::worker) fn accepted_fixture(
        &mut self,
        accepted: tokio_util::sync::CancellationToken,
    ) {
        self.accepted = Some(accepted);
    }

    pub(in crate::worker) fn observe(&mut self, line: &str) -> bool {
        if self.snapshot.reader_closed {
            return false;
        }
        let Some(sample) = parse(line) else {
            add(
                &mut self.snapshot.unsupported_records,
                1,
                &mut self.snapshot.overflow,
            );
            self.report_if_due();
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
            Sample::Started(_) => add(&mut counts.started_records, 1, &mut self.snapshot.overflow),
            Sample::Completed {
                queue_field_present,
                declared_success,
                ..
            } => {
                add(
                    &mut counts.completed_records,
                    1,
                    &mut self.snapshot.overflow,
                );
                add(
                    &mut counts.declared_failure_records,
                    u64::from(!declared_success),
                    &mut self.snapshot.overflow,
                );
                add(
                    &mut counts.missing_queue_field_records,
                    u64::from(!queue_field_present),
                    &mut self.snapshot.overflow,
                );
            }
        }
        self.report_if_due();
        #[cfg(test)]
        if let Some(accepted) = &self.accepted {
            accepted.cancel();
        }
        true
    }

    fn report_if_due(&mut self) {
        if Instant::now() < self.next_report {
            return;
        }
        self.report();
        self.next_report = Instant::now() + PERIOD;
    }

    fn report(&mut self) {
        if self.snapshot.recognized_records == 0 {
            return;
        }
        self.snapshot.local_reader_elapsed_ms =
            self.started.elapsed().as_millis().min(u64::MAX.into()) as u64;
        // This record contains only managed UUIDs, static labels, local event
        // counts and a host-monotonic reader duration. Child numeric payloads
        // cannot bypass rotated-secret redaction: none are retained or emitted.
        // Unknown records still use the original fresh-redacted output path.
        let dispatch = self.dispatch.clone();
        let accepted = tracing::dispatcher::with_default(&dispatch, || {
            if !crate::performance::enabled() {
                return None;
            }
            let summary = serde_json::json!({
                "runId": self.run,
                "attemptId": self.attempt,
                "snapshot": self.snapshot,
                "rejectedReports": self.rejected_reports,
                "counts": "local observations of untrusted schema declarations; overflow means lower bounds",
                "timing": "host-monotonic elapsed since reader observer creation; child RPC timings and IDs are discarded",
                "coverage": "cumulative reader observations, not authoritative agent progress",
            }).to_string();
            Some(crate::performance::try_record(move |delivery| {
                tracing::info!(target: "cairn_performance", parent: None,
                    operation = "agent_rpc_summary", source = "agent_process",
                    record = %summary, diagnostics_queue_us = delivery.queue_us,
                    dropped_reports_total = delivery.dropped_reports_total);
            }))
        });
        if accepted == Some(false) {
            add(&mut self.rejected_reports, 1, &mut self.snapshot.overflow);
        }
    }

    pub(in crate::worker) fn finish(&mut self, eof: bool) {
        if self.snapshot.reader_closed {
            return;
        }
        self.snapshot.reader_closed = true;
        self.snapshot.eof = eof;
        self.report();
    }
}

#[cfg(test)]
mod tests;
