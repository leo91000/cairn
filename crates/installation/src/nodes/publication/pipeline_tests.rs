//! Real direct-transfer ordering witnesses. Remote delays are controlled fixture
//! effects, never measurements of an actual storage provider.
use super::{diagnostic_tests::service_with_runner, *};
use crate::{
    node_fixture::StoreNodeFixture,
    object_storage::tests::fixture_storage_at,
    storage::{block_codec, direct},
};
use axum::{
    Json, Router,
    body::to_bytes,
    extract::Request,
    http::{Method, StatusCode},
    response::IntoResponse,
};
use futures_util::StreamExt;
use rand::{RngCore, SeedableRng};
use std::{
    collections::BTreeMap,
    sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    time::Instant,
};
use tokio::{io::AsyncWriteExt, sync::Semaphore};

mod measurement;
mod safety;

#[derive(Clone, Copy)]
enum ReceiptFault {
    WrongPublication,
    MissingObject,
    DuplicateObject,
    WrongPlaintext,
    OversizedEncoding,
    MissingFields,
    LostResponse,
}

struct ControlEffects {
    posts: AtomicUsize,
    active: AtomicUsize,
    maximum: AtomicUsize,
    ordinary: AtomicUsize,
    terminal_overlap: AtomicBool,
    reader_disconnect: tokio_util::sync::CancellationToken,
    hold_reply: std::sync::Mutex<Option<usize>>,
    fault: std::sync::Mutex<Option<(usize, ReceiptFault)>>,
    reply_ready: Semaphore,
    release_reply: Semaphore,
}

impl Default for ControlEffects {
    fn default() -> Self {
        Self {
            posts: AtomicUsize::new(0),
            active: AtomicUsize::new(0),
            maximum: AtomicUsize::new(0),
            ordinary: AtomicUsize::new(0),
            terminal_overlap: AtomicBool::new(false),
            reader_disconnect: tokio_util::sync::CancellationToken::new(),
            hold_reply: std::sync::Mutex::default(),
            fault: std::sync::Mutex::default(),
            reply_ready: Semaphore::new(0),
            release_reply: Semaphore::new(0),
        }
    }
}

struct ControlJob<'a> {
    effects: &'a ControlEffects,
    ordinary: bool,
}

impl Drop for ControlJob<'_> {
    fn drop(&mut self) {
        self.effects.active.fetch_sub(1, Ordering::SeqCst);
        if self.ordinary {
            self.effects.ordinary.fetch_sub(1, Ordering::SeqCst);
        }
    }
}

impl ControlEffects {
    fn begin(&self, terminal: bool) -> ControlJob<'_> {
        let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.maximum.fetch_max(active, Ordering::SeqCst);
        if terminal {
            if self.ordinary.load(Ordering::SeqCst) != 0 {
                self.terminal_overlap.store(true, Ordering::SeqCst);
            }
        } else {
            self.ordinary.fetch_add(1, Ordering::SeqCst);
        }
        ControlJob {
            effects: self,
            ordinary: !terminal,
        }
    }

    async fn reply(&self, ordinal: usize, result: Result<Value>) -> axum::response::Response {
        let hold = *self.hold_reply.lock().unwrap() == Some(ordinal);
        if hold {
            self.reply_ready.add_permits(1);
            self.release_reply.acquire().await.unwrap().forget();
        }
        let mut receipt = match result {
            Ok(receipt) => receipt,
            Err(error) => return error.into_response(),
        };
        let fault = *self.fault.lock().unwrap();
        if let Some((target, fault)) = fault
            && target == ordinal
        {
            match fault {
                ReceiptFault::WrongPublication => receipt["publicationId"] = id().into(),
                ReceiptFault::MissingObject => {
                    receipt["objects"].as_array_mut().unwrap().pop();
                }
                ReceiptFault::DuplicateObject => {
                    receipt["objects"][1] = receipt["objects"][0].clone()
                }
                ReceiptFault::WrongPlaintext => {
                    receipt["plaintextBytes"] =
                        (receipt["plaintextBytes"].as_u64().unwrap() + 1).into()
                }
                ReceiptFault::OversizedEncoding => {
                    receipt["encodedBytes"] =
                        (direct::BATCH as u64 * block_codec::MAX_ENCODED_BYTES + 1).into()
                }
                ReceiptFault::MissingFields => receipt = json!({}),
                ReceiptFault::LostResponse => {
                    return StatusCode::SERVICE_UNAVAILABLE.into_response();
                }
            }
        }
        Json(receipt).into_response()
    }
}

#[derive(Clone, Copy, Default)]
struct RemoteConditions {
    control_ms: u64,
    put_ms: u64,
    tail_ms: u64,
    bytes_per_second: u64,
}

#[derive(Clone, Copy)]
enum Contents {
    Sparse,
    Mixed,
}

#[derive(Default)]
struct SharedPutBudget {
    active: AtomicUsize,
    maximum: AtomicUsize,
}

impl Contents {
    fn block(self, index: usize) -> Vec<u8> {
        let mut bytes = vec![0; snapshots::BLOCK as usize];
        if matches!(self, Self::Mixed) {
            let random_bytes = bytes.len() * 18 / 100;
            rand::rngs::StdRng::seed_from_u64(0x5eed ^ index as u64)
                .fill_bytes(&mut bytes[..random_bytes]);
        }
        bytes[..8].copy_from_slice(&(index as u64 + 1).to_le_bytes());
        bytes
    }
}

struct ObjectServer {
    bodies: tokio::sync::Mutex<BTreeMap<String, Vec<u8>>>,
    blocked_object: std::sync::Mutex<Option<String>>,
    indices: std::sync::Mutex<BTreeMap<String, usize>>,
    conditions: std::sync::Mutex<RemoteConditions>,
    next_byte: tokio::sync::Mutex<Option<Instant>>,
    release: Semaphore,
    completed: Semaphore,
    fifth_started: Semaphore,
    started: AtomicUsize,
    in_flight: AtomicUsize,
    maximum: AtomicUsize,
    encoded_bytes: AtomicU64,
    preconditions: AtomicUsize,
    fail_index: std::sync::Mutex<Option<usize>>,
    shared_budget: std::sync::Mutex<Option<Arc<SharedPutBudget>>>,
}

impl Default for ObjectServer {
    fn default() -> Self {
        Self {
            bodies: tokio::sync::Mutex::default(),
            blocked_object: std::sync::Mutex::default(),
            indices: std::sync::Mutex::default(),
            conditions: std::sync::Mutex::default(),
            next_byte: tokio::sync::Mutex::default(),
            release: Semaphore::new(0),
            completed: Semaphore::new(0),
            fifth_started: Semaphore::new(0),
            started: AtomicUsize::new(0),
            in_flight: AtomicUsize::new(0),
            maximum: AtomicUsize::new(0),
            encoded_bytes: AtomicU64::new(0),
            preconditions: AtomicUsize::new(0),
            fail_index: std::sync::Mutex::default(),
            shared_budget: std::sync::Mutex::default(),
        }
    }
}

impl ObjectServer {
    async fn body(&self, body: axum::body::Body, rate: u64) -> Vec<u8> {
        let mut stream = body.into_data_stream();
        let mut bytes = Vec::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.unwrap();
            assert!(
                chunk.len()
                    <= (block_codec::MAX_ENCODED_BYTES as usize).saturating_sub(bytes.len())
            );
            if rate != 0 {
                for piece in chunk.chunks(16_384) {
                    let until = {
                        let mut next = self.next_byte.lock().await;
                        let now = Instant::now();
                        let start = next.unwrap_or(now).max(now);
                        let until =
                            start + Duration::from_secs_f64(piece.len() as f64 / rate as f64);
                        *next = Some(until);
                        until
                    };
                    tokio::time::sleep_until(tokio::time::Instant::from_std(until)).await;
                }
            }
            bytes.extend_from_slice(&chunk);
        }
        bytes
    }

    async fn respond(&self, request: Request) -> axum::response::Response {
        let path = request.uri().path().to_owned();
        if request.method() == Method::GET {
            return self.bodies.lock().await.get(&path).cloned().map_or_else(
                || StatusCode::NOT_FOUND.into_response(),
                IntoResponse::into_response,
            );
        }
        if request.method() == Method::DELETE {
            self.bodies.lock().await.remove(&path);
            return StatusCode::NO_CONTENT.into_response();
        }
        if request.method() != Method::PUT {
            return StatusCode::METHOD_NOT_ALLOWED.into_response();
        }
        if !path.contains("/shared-blocks/v3/") && !path.contains("/direct-probes/") {
            let bytes = self.body(request.into_body(), 0).await;
            self.bodies.lock().await.insert(path, bytes);
            return StatusCode::OK.into_response();
        }
        assert_eq!(request.headers()["if-none-match"], "*");
        if path.contains("/direct-probes/") {
            let bytes = self.body(request.into_body(), 0).await;
            let mut bodies = self.bodies.lock().await;
            if let std::collections::btree_map::Entry::Vacant(entry) = bodies.entry(path) {
                entry.insert(bytes);
                return StatusCode::OK.into_response();
            } else {
                return StatusCode::PRECONDITION_FAILED.into_response();
            }
        }
        let active = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
        self.maximum.fetch_max(active, Ordering::SeqCst);
        let shared_budget = self.shared_budget.lock().unwrap().clone();
        if let Some(shared) = &shared_budget {
            let active = shared.active.fetch_add(1, Ordering::SeqCst) + 1;
            shared.maximum.fetch_max(active, Ordering::SeqCst);
        }
        let ordinal = self.started.fetch_add(1, Ordering::SeqCst) + 1;
        if ordinal == 5 {
            self.fifth_started.add_permits(1);
        }
        let conditions = *self.conditions.lock().unwrap();
        let bytes = self
            .body(request.into_body(), conditions.bytes_per_second)
            .await;
        self.encoded_bytes
            .fetch_add(bytes.len() as u64, Ordering::SeqCst);
        let index = self
            .indices
            .lock()
            .unwrap()
            .iter()
            .find_map(|(hash, index)| path.split('/').any(|part| part == hash).then_some(*index))
            .unwrap();
        let delay = conditions.put_ms
            + if index.is_multiple_of(direct::BATCH) {
                conditions.tail_ms
            } else {
                0
            };
        if delay != 0 {
            tokio::time::sleep(Duration::from_millis(delay)).await;
        }
        let blocked = self
            .blocked_object
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|object| path.split('/').any(|part| part == object));
        if blocked {
            self.release.acquire().await.unwrap().forget();
        }
        let status = if *self.fail_index.lock().unwrap() == Some(index) {
            StatusCode::BAD_REQUEST
        } else {
            let mut bodies = self.bodies.lock().await;
            if let std::collections::btree_map::Entry::Vacant(entry) = bodies.entry(path) {
                entry.insert(bytes);
                StatusCode::OK
            } else {
                self.preconditions.fetch_add(1, Ordering::SeqCst);
                StatusCode::PRECONDITION_FAILED
            }
        };
        self.in_flight.fetch_sub(1, Ordering::SeqCst);
        if let Some(shared) = shared_budget {
            shared.active.fetch_sub(1, Ordering::SeqCst);
        }
        self.completed.add_permits(1);
        status.into_response()
    }
}

struct Fixture {
    root: tempfile::TempDir,
    service: Arc<Service>,
    run: Value,
    attempt: String,
    node: Value,
    snapshot: Value,
    snapshot_id: String,
    objects: Vec<shared_blocks::Object>,
    storage: Storage,
    controller: String,
    object_server: Arc<ObjectServer>,
    node_metrics: Arc<std::sync::Mutex<Option<Arc<diagnostic::Metrics>>>>,
    manager_metrics: std::sync::Mutex<Option<Arc<diagnostic::Metrics>>>,
    effects: Arc<ControlEffects>,
    capture_reply: Arc<std::sync::Mutex<Value>>,
    servers: Vec<tokio::task::AbortHandle>,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        for server in &self.servers {
            server.abort();
        }
    }
}

impl Fixture {
    async fn new(blocks: usize) -> Self {
        Self::with_contents(blocks, Contents::Sparse).await
    }

    async fn with_contents(blocks: usize, contents: Contents) -> Self {
        let root = tempfile::tempdir().unwrap();
        let service = service_with_runner(root.path(), String::new()).await;
        let snapshot_id = id();
        let directory = root.path().join("snapshots").join(&snapshot_id);
        tokio::fs::create_dir_all(&directory).await.unwrap();
        let mut file = tokio::fs::File::create(directory.join("disk"))
            .await
            .unwrap();
        for index in 0..blocks {
            let bytes = contents.block(index);
            file.write_all(&bytes).await.unwrap();
        }
        file.flush().await.unwrap();
        drop(file);
        let mut manifest = snapshots::index(&directory.join("disk")).await.unwrap();
        manifest["capturedAt"] = now().into();
        tokio::fs::write(directory.join("manifest.json"), manifest.to_string())
            .await
            .unwrap();
        let run_id = id();
        let attempt = id();
        let node = json!(super::super::LOCAL_NODE_ID);
        service
            .store
            .put_node_fixture(json!({ "id": super::super::LOCAL_NODE_ID }))
            .await
            .unwrap();
        let saved_run = run_id.clone();
        let saved_attempt = attempt.clone();
        service
            .store
            .transaction(move |db| {
                db.add_run_fixture(
                    &json!({
                        "id": saved_run,
                        "taskId": id(),
                        "status": "succeeded",
                        "createdAt": 0,
                        "nodeId": super::super::LOCAL_NODE_ID,
                        "snapshot": { "agent": { "id": crate::config::MAIN_AGENT_ID } }
                    }),
                    None,
                )?;
                db.save_checkpoint_boundary(
                    &saved_run,
                    &json!({
                        "nodeId": super::super::LOCAL_NODE_ID,
                        "runnerId": saved_attempt
                    }),
                )
            })
            .await
            .unwrap();
        let object_server = Arc::new(ObjectServer::default());
        let object_app = Router::new().fallback({
            let state = object_server.clone();
            move |request: Request| {
                let state = state.clone();
                async move { state.respond(request).await }
            }
        });
        let (object_url, object_task) = diagnostic_tests::peer(object_app).await;
        let storage = fixture_storage_at(
            &object_url,
            "https://pipeline-fixture.invalid",
            "pipeline-fixture",
            service.hot_s3.clone(),
        )
        .await;
        let mut reserve_manifest = manifest.clone();
        let point = json!({
            "id": id(),
            "runId": run_id,
            "destination": "s3",
            "bucket": "pipeline-fixture",
            "endpoint": storage.endpoint,
            "blockEnvelope": 3
        });
        let objects = service
            .store
            .transaction(move |db| shared_blocks::reserve(db, &point, &mut reserve_manifest))
            .await
            .unwrap();
        *object_server.blocked_object.lock().unwrap() = Some(objects[0].id.clone());
        *object_server.indices.lock().unwrap() = objects
            .iter()
            .enumerate()
            .map(|(index, object)| (object.hash.clone(), index))
            .collect();
        let node_metrics = Arc::new(std::sync::Mutex::new(None));
        let server_metrics = node_metrics.clone();
        let remote_conditions = object_server.clone();
        let effects = Arc::new(ControlEffects::default());
        let server_effects = effects.clone();
        let snapshot = json!({ "id": snapshot_id, "manifest": manifest });
        let capture_reply = Arc::new(std::sync::Mutex::new(snapshot.clone()));
        let server_capture = capture_reply.clone();
        let controller_app = Router::new().fallback(move |request: Request| {
            let directory = directory.clone();
            let server_metrics = server_metrics.clone();
            let remote_conditions = remote_conditions.clone();
            let server_effects = server_effects.clone();
            let server_capture = server_capture.clone();
            async move {
                assert_eq!(request.headers()["authorization"], "Bearer fixture");
                if request.uri().path().ends_with("/snapshot")
                    || request.uri().path().ends_with("/snapshot-completed")
                {
                    return Json(server_capture.lock().unwrap().clone()).into_response();
                }
                if request.uri().path().ends_with("/discard") {
                    return match snapshots::discard(&directory).await {
                        Ok(()) => Json(json!({ "ok": true })).into_response(),
                        Err(error) => error.into_response(),
                    };
                }
                if request.uri().path().ends_with("/direct-reader") {
                    if request.method() == Method::DELETE {
                        return match snapshots::close_direct_reader(&directory).await {
                            Ok(()) => Json(json!({ "closed": true })).into_response(),
                            Err(error) => error.into_response(),
                        };
                    }
                    let bytes = to_bytes(request.into_body(), 4096).await.unwrap();
                    return match snapshots::direct_reader(&directory, &bytes).await {
                        Ok(body) => {
                            axum::body::Body::from_stream(body.into_data_stream().take_until(
                                server_effects.reader_disconnect.clone().cancelled_owned(),
                            ))
                            .into_response()
                        }
                        Err(error) => error.into_response(),
                    };
                }
                assert!(request.uri().path().ends_with("/direct-upload"));
                let bytes = to_bytes(request.into_body(), 131_072).await.unwrap();
                let plan: Value = serde_json::from_slice(&bytes).unwrap();
                let ordinal = server_effects.posts.fetch_add(1, Ordering::SeqCst) + 1;
                let _active = server_effects.begin(plan["diagnostics"]["finalBatch"] == true);
                let delay = remote_conditions.conditions.lock().unwrap().control_ms;
                if delay != 0 {
                    tokio::time::sleep(Duration::from_millis(delay)).await;
                }
                let result = snapshots::upload_direct(&directory, &bytes).await;
                if let Some(attempt) = plan["diagnostics"]["attempt"].as_str()
                    && let Some(metrics) = diagnostic::node_metrics_for_test(attempt)
                {
                    *server_metrics.lock().unwrap() = Some(metrics);
                }
                server_effects.reply(ordinal, result).await
            }
        });
        let (controller, controller_task) = diagnostic_tests::peer(controller_app).await;
        let run = service.store.run(&run_id).await.unwrap();
        Self {
            root,
            service,
            run,
            attempt,
            node,
            snapshot,
            snapshot_id,
            objects,
            storage,
            controller,
            object_server,
            node_metrics,
            manager_metrics: std::sync::Mutex::default(),
            effects,
            capture_reply,
            servers: vec![object_task, controller_task],
        }
    }

    async fn transfer_measured(&self, window: Option<usize>) -> Result<(u64, Value)> {
        let attempt = id();
        let publication = Publication {
            s: &self.service,
            run: &self.run,
            run_id: text(&self.run, "id"),
            attempt: &self.attempt,
            publication_attempt: &attempt,
            node: &self.node,
            storage: &self.storage,
            base: &self.controller,
            credential: "fixture",
            snapshot: &self.snapshot,
            snapshot_id: &self.snapshot_id,
            budget: 0,
            block_envelope: 3,
        };
        let metrics = diagnostic::Metrics::new(
            &attempt,
            text(&self.run, "id"),
            &self.snapshot_id,
            "manager",
        );
        *self.manager_metrics.lock().unwrap() = Some(metrics.clone());
        let mut manifest = self.snapshot["manifest"].clone();
        manifest["blockEnvelope"] = 3.into();
        let bytes = match window {
            Some(window) => {
                publication
                    .observe_transfer(
                        &metrics,
                        &self.objects,
                        publication.transfer_direct_window(&self.objects, window),
                    )
                    .await?
            }
            None => {
                publication
                    .transfer_observed(&metrics, &manifest, &self.objects)
                    .await?
            }
        };
        Ok((bytes, metrics.snapshot()))
    }
}

#[tokio::test]
async fn direct_window_reuses_completed_put_slots_before_a_straggler_finishes() {
    isolated("nodes::publication::pipeline_tests::direct_window_straggler_child").await;
}

#[tokio::test]
async fn direct_serial_reference_preserves_the_observed_barrier() {
    isolated("nodes::publication::pipeline_tests::direct_serial_straggler_child").await;
}

#[tokio::test]
async fn experimental_direct_window_fills_free_put_slots() {
    isolated("nodes::publication::pipeline_tests::direct_candidate_straggler_child").await;
}

async fn isolated(child: &str) {
    // The shipped PUT semaphore is process-global. Keep unrelated fixture
    // publications from occupying its permits during this ordering witness.
    let output = tokio::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", child, "--ignored", "--nocapture"])
        .output()
        .await
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stdout}\n{stderr}");
}

#[tokio::test]
#[ignore = "isolated process-global PUT budget child"]
async fn direct_window_straggler_child() {
    straggler(None, true).await;
}

#[tokio::test]
#[ignore = "isolated process-global PUT budget child"]
async fn direct_serial_straggler_child() {
    straggler(Some(1), false).await;
}

#[tokio::test]
#[ignore = "isolated process-global PUT budget child"]
async fn direct_candidate_straggler_child() {
    straggler(Some(2), true).await;
}

async fn straggler(window: Option<usize>, expected_overlap: bool) {
    // Two ordinary batches and one terminal block exercise the steady-state
    // scheduling separately from the existing final-summary ordering contract.
    let fixture = Fixture::new(9).await;
    let state = fixture.object_server.clone();
    let (transfer, fifth_before_release) = tokio::time::timeout(Duration::from_secs(20), async {
        tokio::join!(fixture.transfer_measured(window), async {
            // All three unblocked members of the first batch must finish before
            // checking that a later block can use the already-free PUT capacity.
            state.completed.acquire_many(3).await.unwrap().forget();
            let fifth = tokio::time::timeout(Duration::from_secs(2), state.fifth_started.acquire())
                .await
                .is_ok();
            state.release.add_permits(1);
            fifth
        })
    })
    .await
    .expect("fixture transfer must drain even when the ordering witness fails");
    assert_eq!(transfer.unwrap().0, 9 * snapshots::BLOCK);
    assert_eq!(state.started.load(Ordering::SeqCst), 9);
    assert!(state.maximum.load(Ordering::SeqCst) <= direct::BATCH);
    assert_eq!(state.in_flight.load(Ordering::SeqCst), 0);
    assert_eq!(
        fifth_before_release, expected_overlap,
        "the sequential batch barrier left three completed PUT slots unused behind one straggler"
    );
}
