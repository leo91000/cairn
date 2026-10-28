//! Attempt-local, bounded transfer diagnostics. All clocks measure wall time.
use serde_json::{Value, json};
use std::{
    cell::RefCell,
    future::Future,
    sync::{Arc, Mutex},
    time::Instant,
};

#[derive(Clone, Copy)]
#[repr(usize)]
pub(crate) enum Stage {
    Inventory,
    NodeHeaders,
    NodeBody,
    BaseRead,
    RecoveryAdmission,
    CodecAdmission,
    BlockingQueue,
    Compress,
    Decompress,
    Encrypt,
    Decrypt,
    Hash,
    PutSlot,
    S3Client,
    PutChecksum,
    PutRequest,
    GetSlot,
    GetHeaders,
    GetBody,
    GetCoalesced,
    UploadBackpressure,
    UploadDrain,
    NodeOpen,
    NodeProduce,
    NodeBaseRead,
    NodeBackpressure,
    NodeFirstByte,
    NodeBaseRequest,
    NodeRetryWait,
    NodeCacheWait,
}

const NAMES: [&str; 30] = [
    "inventory",
    "nodeHeaders",
    "nodeBody",
    "baseRead",
    "recoveryAdmission",
    "codecAdmission",
    "blockingQueue",
    "compress",
    "decompress",
    "encrypt",
    "decrypt",
    "hash",
    "putSlot",
    "s3Client",
    "putChecksum",
    "putRequest",
    "getSlot",
    "getHeaders",
    "getBody",
    "getCoalesced",
    "uploadBackpressure",
    "uploadDrain",
    "nodeOpen",
    "nodeProduce",
    "nodeBaseRead",
    "nodeBackpressure",
    "nodeFirstByte",
    "nodeBaseRequest",
    "nodeRetryWait",
    "nodeCacheWait",
];

#[derive(Default, Clone, Copy)]
struct Totals {
    started: u64,
    completed: u64,
    failed_or_cancelled: u64,
    bytes: u64,
    elapsed_us: u64,
    active: u64,
    active_starts_us: u64,
    max_active: u64,
}

struct State {
    stages: [Totals; NAMES.len()],
    active: u64,
    union_start_us: u64,
    union_us: u64,
    inventory: [u64; 3],
}

pub(crate) struct Metrics {
    pub(crate) attempt: String,
    run: String,
    snapshot: String,
    side: &'static str,
    started: Instant,
    state: Mutex<State>,
}

impl Metrics {
    pub(crate) fn new(attempt: &str, run: &str, snapshot: &str, side: &'static str) -> Arc<Self> {
        Arc::new(Self {
            attempt: super::identity(attempt).into(),
            run: super::identity(run).into(),
            snapshot: super::identity(snapshot).into(),
            side,
            started: Instant::now(),
            state: Mutex::new(State {
                stages: [Totals::default(); NAMES.len()],
                active: 0,
                union_start_us: 0,
                union_us: 0,
                inventory: [0; 3],
            }),
        })
    }

    pub(crate) fn inventory(&self, blocks: u64, reused: u64, plaintext: u64) {
        self.state.lock().unwrap().inventory = [blocks, reused, plaintext];
    }

    fn now_us(&self) -> u64 {
        self.started.elapsed().as_micros().min(u64::MAX as u128) as u64
    }

    pub(crate) fn snapshot(&self) -> Value {
        let state = self.state.lock().unwrap();
        let wall = self.now_us();
        let covered = state.union_us
            + if state.active > 0 {
                wall - state.union_start_us
            } else {
                0
            };
        let mut stages = serde_json::Map::new();
        let mut cumulative = 0;
        for (name, value) in NAMES.iter().zip(state.stages) {
            let elapsed = value.elapsed_us + value.active * wall - value.active_starts_us;
            cumulative += elapsed;
            stages.insert(
                (*name).into(),
                json!({
                    "started": value.started,
                    "completed": value.completed,
                    "failedOrCancelled": value.failed_or_cancelled,
                    "bytes": value.bytes,
                    "wallUsSum": elapsed,
                    "inFlight": value.active,
                    "maxInFlight": value.max_active
                }),
            );
        }
        json!({
            "wallUs": wall,
            "observedUnionUs": covered,
            "unattributedUs": wall.saturating_sub(covered),
            "operationWallUsSum": cumulative,
            "nestedOrParallelOverlapUs": cumulative.saturating_sub(covered),
            "referencedBlocks": state.inventory[0],
            "reusedBlocks": state.inventory[1],
            "plannedPlaintextBytes": state.inventory[2],
            "stages": stages
        })
    }

    pub(crate) fn record(&self, outcome: &'static str) {
        tracing::info!(
            target: "cairn_performance",
            operation = "disk_publication_transfer",
            publication_attempt = self.attempt,
            run_id = self.run,
            snapshot_id = self.snapshot,
            side = self.side,
            outcome,
            metrics = %self.snapshot()
        );
    }
}

tokio::task_local! { static CURRENT: Arc<Metrics>; }

thread_local! { static BLOCKING: RefCell<Option<Arc<Metrics>>> = const { RefCell::new(None) }; }

pub(crate) fn current() -> Option<Arc<Metrics>> {
    BLOCKING
        .with(|value| value.borrow().clone())
        .or_else(|| CURRENT.try_with(Arc::clone).ok())
}

pub(crate) async fn scope<T>(metrics: Option<Arc<Metrics>>, future: impl Future<Output = T>) -> T {
    match metrics {
        Some(metrics) => CURRENT.scope(metrics, future).await,
        None => future.await,
    }
}

pub(crate) fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> T + Send + 'static,
) -> tokio::task::JoinHandle<T> {
    let metrics = current();
    let queued = start(Stage::BlockingQueue);
    tokio::task::spawn_blocking(move || {
        struct Reset(Option<Arc<Metrics>>);
        impl Drop for Reset {
            fn drop(&mut self) {
                BLOCKING.with(|value| *value.borrow_mut() = self.0.take());
            }
        }
        let _reset = Reset(BLOCKING.with(|value| value.replace(metrics)));
        queued.finish(0);
        work()
    })
}

pub(crate) struct Sample(Option<(Arc<Metrics>, Stage, u64)>);

pub(crate) fn start(stage: Stage) -> Sample {
    let Some(metrics) = current() else {
        return Sample(None);
    };
    let mut state = metrics.state.lock().unwrap();
    let now = metrics.now_us();
    if state.active == 0 {
        state.union_start_us = now;
    }
    state.active += 1;
    let value = &mut state.stages[stage as usize];
    value.started += 1;
    value.active += 1;
    value.active_starts_us += now;
    value.max_active = value.max_active.max(value.active);
    drop(state);
    Sample(Some((metrics, stage, now)))
}

impl Sample {
    pub(crate) fn finish(mut self, bytes: usize) {
        self.close(Some(bytes));
    }

    fn close(&mut self, bytes: Option<usize>) {
        let Some((metrics, stage, started)) = self.0.take() else {
            return;
        };
        let mut state = metrics.state.lock().unwrap();
        let now = metrics.now_us();
        let value = &mut state.stages[stage as usize];
        value.elapsed_us += now - started;
        value.active -= 1;
        value.active_starts_us -= started;
        if let Some(bytes) = bytes {
            value.completed += 1;
            value.bytes += bytes as u64;
        } else {
            value.failed_or_cancelled += 1;
        }
        state.active -= 1;
        if state.active == 0 {
            state.union_us += now - state.union_start_us;
        }
    }
}

impl Drop for Sample {
    fn drop(&mut self) {
        self.close(None);
    }
}

pub(crate) struct Summary {
    pub(crate) metrics: Arc<Metrics>,
    pub(crate) outcome: &'static str,
}

impl Drop for Summary {
    fn drop(&mut self) {
        self.metrics.record(self.outcome);
    }
}

/// Diagnostic metadata travels in the existing authenticated JSON request, including relay.
/// Older nodes ignore it. It conveys no authorization and invalid identities disable it.
#[derive(serde::Serialize, serde::Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Request {
    attempt: String,
    run: String,
    final_batch: bool,
}

pub(crate) fn request(final_batch: bool) -> Option<Request> {
    current().map(|metrics| Request {
        attempt: metrics.attempt.clone(),
        run: metrics.run.clone(),
        final_batch,
    })
}

struct NodeEntry {
    metrics: Arc<Metrics>,
    touched: Instant,
    logged: Instant,
}

fn registry() -> &'static Mutex<Vec<NodeEntry>> {
    static ENTRIES: std::sync::OnceLock<Mutex<Vec<NodeEntry>>> = std::sync::OnceLock::new();
    ENTRIES.get_or_init(|| Mutex::new(Vec::new()))
}

pub(crate) struct NodeSummary {
    pub(crate) metrics: Arc<Metrics>,
    final_batch: bool,
    complete: bool,
}

impl NodeSummary {
    pub(crate) fn begin(request: Option<Request>, snapshot: &str) -> Option<Self> {
        let request = request?;
        if [&request.attempt, &request.run, snapshot]
            .into_iter()
            .any(|id| uuid::Uuid::parse_str(id).is_err())
        {
            return None;
        }
        let mut entries = registry().lock().unwrap();
        let now = Instant::now();
        entries.retain(|entry| {
            if now.duration_since(entry.touched).as_secs() < 600 {
                return true;
            }
            entry.metrics.record("expired_partial");
            false
        });
        let metrics = if let Some(entry) = entries.iter_mut().find(|entry| {
            entry.metrics.attempt == request.attempt
                && entry.metrics.snapshot == snapshot
                && entry.metrics.run == request.run
        }) {
            entry.touched = now;
            entry.metrics.clone()
        } else {
            if entries.len() >= 64 {
                let oldest = entries
                    .iter()
                    .enumerate()
                    .min_by_key(|(_, entry)| entry.touched)
                    .unwrap()
                    .0;
                entries
                    .swap_remove(oldest)
                    .metrics
                    .record("evicted_partial");
            }
            let metrics = Metrics::new(&request.attempt, &request.run, snapshot, "node");
            entries.push(NodeEntry {
                metrics: metrics.clone(),
                touched: now,
                logged: now,
            });
            metrics
        };
        Some(Self {
            metrics,
            final_batch: request.final_batch,
            complete: false,
        })
    }

    pub(crate) fn complete(&mut self) {
        self.complete = true;
    }
}

impl Drop for NodeSummary {
    fn drop(&mut self) {
        let mut entries = registry().lock().unwrap();
        if !self.complete || self.final_batch {
            entries.retain(|entry| !Arc::ptr_eq(&entry.metrics, &self.metrics));
            self.metrics.record(if self.complete {
                "success"
            } else {
                "error_or_cancelled"
            });
        } else if let Some(entry) = entries
            .iter_mut()
            .find(|entry| Arc::ptr_eq(&entry.metrics, &self.metrics))
        {
            entry.touched = Instant::now();
            if entry.logged.elapsed().as_secs() >= 60 {
                entry.logged = Instant::now();
                self.metrics.record("progress");
            }
        }
    }
}

fn active_managers() -> &'static Mutex<Vec<std::sync::Weak<Metrics>>> {
    static ACTIVE: std::sync::OnceLock<Mutex<Vec<std::sync::Weak<Metrics>>>> =
        std::sync::OnceLock::new();
    ACTIVE.get_or_init(|| Mutex::new(Vec::new()))
}

pub(crate) struct Registration(Arc<Metrics>);

pub(crate) fn register(metrics: &Arc<Metrics>) -> Registration {
    let mut active = active_managers().lock().unwrap();
    active.retain(|entry| entry.strong_count() > 0);
    if active.len() < 64 {
        active.push(Arc::downgrade(metrics));
    }
    Registration(metrics.clone())
}

impl Drop for Registration {
    fn drop(&mut self) {
        let weak = Arc::downgrade(&self.0);
        active_managers()
            .lock()
            .unwrap()
            .retain(|entry| !entry.ptr_eq(&weak));
    }
}

pub(crate) fn restore_context(
    headers: &axum::http::HeaderMap,
    authorized_run: &str,
) -> Option<Arc<Metrics>> {
    let attempt = headers.get("x-cairn-publication-attempt")?.to_str().ok()?;
    let snapshot = headers.get("x-cairn-publication-snapshot")?.to_str().ok()?;
    active_managers()
        .lock()
        .unwrap()
        .iter()
        .filter_map(std::sync::Weak::upgrade)
        .find(|metrics| {
            metrics.attempt == attempt
                && metrics.snapshot == snapshot
                && metrics.run == authorized_run
        })
}

pub(crate) fn restore_request(mut request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
    if let Some(metrics) = current() {
        request = request
            .header("x-cairn-publication-attempt", &metrics.attempt)
            .header("x-cairn-publication-snapshot", &metrics.snapshot);
    }
    request
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn cancelled_samples_include_active_time_and_release_their_context() {
        let metrics = Metrics::new(
            &crate::config::id(),
            &crate::config::id(),
            &crate::config::id(),
            "manager",
        );
        let worker = metrics.clone();
        let task = tokio::spawn(scope(Some(worker), async {
            let _sample = start(Stage::PutSlot);
            std::future::pending::<()>().await;
        }));
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while metrics.snapshot()["stages"]["putSlot"]["inFlight"] != 1 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        let live = metrics.snapshot();
        assert!(live["stages"]["putSlot"]["wallUsSum"].as_u64().unwrap() >= 15_000);
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        let value = metrics.snapshot();
        assert_eq!(value["stages"]["putSlot"]["failedOrCancelled"], 1);
        assert_eq!(value["stages"]["putSlot"]["inFlight"], 0);
        assert!(current().is_none());
    }

    #[tokio::test]
    async fn nested_and_parallel_intervals_have_a_union_and_a_residual() {
        let metrics = Metrics::new("invalid", "secret-not-an-id", "path", "manager");
        scope(Some(metrics.clone()), async {
            let envelope = start(Stage::BaseRead);
            let first = start(Stage::GetHeaders);
            let second = start(Stage::PutRequest);
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            first.finish(0);
            second.finish(100);
            envelope.finish(50);
        })
        .await;
        let value = metrics.snapshot();
        assert!(value["nestedOrParallelOverlapUs"].as_u64().unwrap() >= 30_000);
        assert_eq!(metrics.run, "unknown");
        assert_eq!(metrics.snapshot, "unknown");
        assert_eq!(value["stages"]["putRequest"]["bytes"], 100);
    }

    #[test]
    fn restore_correlation_requires_an_active_attempt_and_the_authorized_run() {
        let run = crate::config::id();
        let snapshot = crate::config::id();
        let metrics = Metrics::new(&crate::config::id(), &run, &snapshot, "manager");
        let mut headers = axum::http::HeaderMap::new();
        headers.insert(
            "x-cairn-publication-attempt",
            metrics.attempt.parse().unwrap(),
        );
        headers.insert("x-cairn-publication-snapshot", snapshot.parse().unwrap());
        assert!(restore_context(&headers, &run).is_none());
        let registration = register(&metrics);
        assert!(Arc::ptr_eq(
            &restore_context(&headers, &run).unwrap(),
            &metrics
        ));
        assert!(restore_context(&headers, &crate::config::id()).is_none());
        drop(registration);
        assert!(restore_context(&headers, &run).is_none());
    }
}

#[cfg(test)]
mod node_tests {
    use super::*;
    use crate::nodes::snapshots;

    #[tokio::test]
    async fn node_batches_aggregate_real_reads_and_drop_partial_streams() {
        let root = tempfile::tempdir().unwrap();
        let disk = root.path().join("disk");
        let mut bytes = vec![42; snapshots::BLOCK as usize + 4096];
        bytes[snapshots::BLOCK as usize..].fill(43);
        tokio::fs::write(&disk, &bytes).await.unwrap();
        let manifest = snapshots::index(&disk).await.unwrap();
        tokio::fs::write(root.path().join("manifest.json"), manifest.to_string())
            .await
            .unwrap();
        let hashes: Vec<String> = manifest["blocks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|block| block["hash"].as_str().unwrap().to_owned())
            .collect();
        let attempt = crate::config::id();
        let run = crate::config::id();
        let snapshot = crate::config::id();
        let request = Request {
            attempt: attempt.clone(),
            run,
            final_batch: false,
        };
        let (_, body) = snapshots::served_publication_observed(
            root.path(),
            vec![hashes[0].clone()],
            Some(request.clone()),
            &snapshot,
        )
        .await
        .unwrap();
        let metrics = registry()
            .lock()
            .unwrap()
            .iter()
            .find(|entry| entry.metrics.attempt == attempt)
            .unwrap()
            .metrics
            .clone();
        let received = axum::body::to_bytes(body, snapshots::BLOCK as usize)
            .await
            .unwrap();
        assert_eq!(received.as_ref(), &bytes[..snapshots::BLOCK as usize]);
        assert_eq!(metrics.snapshot()["stages"]["nodeProduce"]["completed"], 1);
        let (_, body) = snapshots::served_publication_observed(
            root.path(),
            vec![hashes[1].clone()],
            Some(Request {
                final_batch: true,
                ..request.clone()
            }),
            &snapshot,
        )
        .await
        .unwrap();
        assert_eq!(
            axum::body::to_bytes(body, 4096).await.unwrap().as_ref(),
            &bytes[snapshots::BLOCK as usize..]
        );
        let value = metrics.snapshot();
        assert_eq!(value["stages"]["nodeProduce"]["completed"], 2);
        assert_eq!(value["stages"]["nodeProduce"]["bytes"], bytes.len());
        assert_eq!(value["stages"]["nodeOpen"]["completed"], 2);
        assert!(
            !registry()
                .lock()
                .unwrap()
                .iter()
                .any(|entry| entry.metrics.attempt == attempt)
        );
        let (_, body) =
            snapshots::served_publication_observed(root.path(), hashes, Some(request), &snapshot)
                .await
                .unwrap();
        drop(body);
        assert!(
            !registry()
                .lock()
                .unwrap()
                .iter()
                .any(|entry| entry.metrics.attempt == attempt)
        );
        assert!(
            NodeSummary::begin(
                Some(Request {
                    attempt: "arbitrary-secret".into(),
                    run: crate::config::id(),
                    final_batch: true
                }),
                &snapshot
            )
            .is_none()
        );
    }
}

#[cfg(test)]
mod remote_tests {
    use super::*;
    use crate::storage::{BlockSource, remote::RemoteSource};
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[tokio::test]
    async fn node_base_retries_preserve_attempt_context_through_blocking_io() {
        let metrics = Metrics::new(
            &crate::config::id(),
            &crate::config::id(),
            &crate::config::id(),
            "node",
        );
        let calls = Arc::new(AtomicUsize::new(0));
        let count = calls.clone();
        let expected_attempt = metrics.attempt.clone();
        let expected_snapshot = metrics.snapshot.clone();
        let app = axum::Router::new().fallback(move |headers: axum::http::HeaderMap| {
            let count = count.clone();
            let attempt = expected_attempt.clone();
            let snapshot = expected_snapshot.clone();
            async move {
                assert_eq!(headers["x-cairn-publication-attempt"], attempt);
                assert_eq!(headers["x-cairn-publication-snapshot"], snapshot);
                if count.fetch_add(1, Ordering::Relaxed) == 0 {
                    (axum::http::StatusCode::SERVICE_UNAVAILABLE, "")
                } else {
                    (axum::http::StatusCode::OK, "block")
                }
            }
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let source = RemoteSource::new(
            &json!({ "master": endpoint, "grant": "synthetic-fixture" }),
            tokio::runtime::Handle::current(),
            tokio_util::sync::CancellationToken::new(),
        )
        .unwrap();
        let bytes = scope(Some(metrics.clone()), async move {
            blocking(move || source.fetch(&format!("b3-{}", "a".repeat(64))))
                .await
                .unwrap()
                .unwrap()
        })
        .await;
        assert_eq!(bytes, b"block");
        let value = metrics.snapshot();
        assert_eq!(value["stages"]["nodeBaseRead"]["completed"], 1);
        assert_eq!(value["stages"]["nodeBaseRead"]["bytes"], 5);
        assert_eq!(value["stages"]["nodeBaseRequest"]["started"], 2);
        assert_eq!(value["stages"]["nodeBaseRequest"]["failedOrCancelled"], 1);
        assert_eq!(value["stages"]["nodeRetryWait"]["completed"], 1);
        assert!(
            value["stages"]["nodeRetryWait"]["wallUsSum"]
                .as_u64()
                .unwrap()
                >= 90_000
        );
        server.abort();
    }
}
