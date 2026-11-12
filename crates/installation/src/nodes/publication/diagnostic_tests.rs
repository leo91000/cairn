use super::*;
use crate::{
    object_storage::{
        HotS3,
        tests::{fixture_storage, fixture_storage_at},
    },
    storage::digest,
};
use axum::{
    Router,
    body::{Body, Bytes, to_bytes},
    extract::Request,
    http::StatusCode,
    response::IntoResponse,
};
use std::sync::atomic::{AtomicBool, Ordering};

async fn service(root: &Path) -> Arc<Service> {
    service_with_runner(root, String::new()).await
}

pub(super) async fn service_with_runner(root: &Path, runner_url: String) -> Arc<Service> {
    Service::new(crate::config::Config {
        data_dir: root.join("data"),
        home: root.join("home"),
        workspace_roots: vec![root.into()],
        public_url: "http://localhost:4310".into(),
        host: "127.0.0.1".into(),
        port: 0,
        codex_bin: "false".into(),
        claude_bin: "false".into(),
        opencode_bin: "false".into(),
        opencode_go_usage_url: crate::accounts::opencode::USAGE_URL.into(),
        concurrency: 1,
        logger: false,
        worker_enabled: false,
        runner_url,
    })
    .await
    .unwrap()
}

pub(super) async fn peer(app: Router) -> (String, tokio::task::AbortHandle) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (url, server.abort_handle())
}

#[tokio::test]
async fn transfer_diagnostics_isolate_concurrent_attempts_and_conserve_volumes() {
    let root = tempfile::tempdir().unwrap();
    let s = service(root.path()).await;
    let fail = Arc::new(AtomicBool::new(false));
    let body_release = Arc::new(tokio::sync::Semaphore::new(0));
    let node = Router::new().fallback({
        let fail = fail.clone();
        let body_release = body_release.clone();
        move |request: Request| {
            let fail = fail.clone();
            let body_release = body_release.clone();
            async move {
                let input: Value =
                    serde_json::from_slice(&to_bytes(request.into_body(), 16384).await.unwrap())
                        .unwrap();
                assert!(input["diagnostic"]["attempt"].is_string());
                tokio::time::sleep(Duration::from_millis(25)).await;
                if fail.load(Ordering::Relaxed) {
                    return StatusCode::SERVICE_UNAVAILABLE.into_response();
                }
                let count = input["hashes"].as_array().unwrap().len();
                let stream = futures_util::stream::unfold(
                    (count, body_release),
                    |(left, body_release)| async move {
                        if left == 0 {
                            return None;
                        }
                        body_release.acquire().await.unwrap().forget();
                        tokio::time::sleep(Duration::from_millis(25)).await;
                        Some((
                            Ok::<_, std::io::Error>(Bytes::from(vec![42; 4096])),
                            (left - 1, body_release),
                        ))
                    },
                );
                Body::from_stream(stream).into_response()
            }
        }
    });
    let (node_url, node_server) = peer(node).await;
    let (s3_url, s3_server) = peer(Router::new().fallback(|request: Request| async move {
        let bytes = to_bytes(request.into_body(), 8192).await.unwrap();
        assert!(!bytes.is_empty());
        tokio::time::sleep(Duration::from_millis(40)).await;
        StatusCode::OK
    }))
    .await;
    let storage = fixture_storage(&s3_url, "diagnostics", Arc::new(HotS3::new())).await;
    let run = json!({});
    let node = json!("remote");
    let snapshot = json!({ "publicationDiagnostics": true });
    let run_id = id();
    let snapshot_id = id();
    let attempt = id();
    let other_attempt = id();
    let objects = vec![
        shared_blocks::Object {
            id: id(),
            hash: digest::block(&vec![42; 4096]),
            size: 4096,
            ready: false,
        },
        shared_blocks::Object {
            id: id(),
            hash: digest::block(b"reused"),
            size: 6,
            ready: true,
        },
    ];
    let publication = Publication {
        authorize: crate::authorization::Authorization::default(),
        s: &s,
        run: &run,
        run_id: &run_id,
        attempt: "",
        publication_attempt: &attempt,
        node: &node,
        storage: &storage,
        base: &node_url,
        credential: "fixture",
        snapshot: &snapshot,
        snapshot_id: &snapshot_id,
        budget: 0,

        block_envelope: 2,
    };
    let first = diagnostic::Metrics::new(&attempt, &run_id, &snapshot_id, "manager");
    let second = diagnostic::Metrics::new(&other_attempt, &run_id, &snapshot_id, "manager");
    let manifest = json!({});
    let ((one, two), ()) = tokio::time::timeout(Duration::from_secs(10), async {
        tokio::join!(
            async {
                tokio::join!(
                    publication.transfer_observed(&first, &manifest, &objects),
                    publication.transfer_observed(&second, &manifest, &objects),
                )
            },
            async {
                // Server timers can expire before the client receives headers.
                // Start their delays only after both body measurements are active.
                while first.snapshot()["stages"]["nodeBody"]["inFlight"] != 1
                    || second.snapshot()["stages"]["nodeBody"]["inFlight"] != 1
                {
                    tokio::task::yield_now().await;
                }
                body_release.add_permits(2);
            }
        )
    })
    .await
    .expect("both node body measurements must start before releasing fixture responses");
    assert_eq!(one.unwrap(), 4096);
    assert_eq!(two.unwrap(), 4096);
    for metrics in [&first, &second] {
        let value = metrics.snapshot();
        let stages = &value["stages"];
        for stage in [
            "nodeHeaders",
            "nodeBody",
            "compress",
            "encrypt",
            "hash",
            "putRequest",
            "putChecksum",
        ] {
            assert_eq!(stages[stage]["completed"], 1, "{stage}: {value}");
            assert_eq!(stages[stage]["inFlight"], 0);
        }
        assert_eq!(stages["nodeBody"]["bytes"], 4096);
        assert_eq!(stages["compress"]["bytes"], 4096);
        let encoded = stages["putRequest"]["bytes"].as_u64().unwrap();
        assert!(encoded > 0 && encoded < 4096);
        assert_eq!(stages["putChecksum"]["bytes"], encoded);
        assert!(stages["nodeHeaders"]["wallUsSum"].as_u64().unwrap() >= 20_000);
        assert!(stages["nodeBody"]["wallUsSum"].as_u64().unwrap() >= 20_000);
        assert!(stages["uploadDrain"]["wallUsSum"].as_u64().unwrap() >= 30_000);
        assert_eq!(
            value["observedUnionUs"].as_u64().unwrap() + value["unattributedUs"].as_u64().unwrap(),
            value["wallUs"].as_u64().unwrap()
        );
    }
    assert_ne!(first.attempt, second.attempt);
    fail.store(true, Ordering::Relaxed);
    let failed = diagnostic::Metrics::new(&id(), &run_id, &snapshot_id, "manager");
    assert!(
        publication
            .transfer_observed(&failed, &manifest, &objects)
            .await
            .is_err()
    );
    assert_eq!(
        failed.snapshot()["stages"]["nodeHeaders"]["completed"],
        1,
        "HTTP header receipt is distinct from status acceptance"
    );
    assert_eq!(failed.snapshot()["stages"]["putRequest"]["started"], 0);
    assert_eq!(first.snapshot()["stages"]["putRequest"]["completed"], 1);
    fail.store(false, Ordering::Relaxed);
    let cancelled = diagnostic::Metrics::new(&id(), &run_id, &snapshot_id, "manager");
    {
        let transfer = publication.transfer_observed(&cancelled, &manifest, &objects);
        tokio::pin!(transfer);
        tokio::time::timeout(Duration::from_secs(5), async {
            tokio::select! {
                result = &mut transfer => panic!("transfer completed before cancellation: {result:?}"),
                () = async {
                    while cancelled.snapshot()["stages"]["nodeBody"]["inFlight"] != 1 { tokio::task::yield_now().await; }
                } => {}
            }
        }).await.unwrap();
    }
    let cancelled = cancelled.snapshot();
    assert_eq!(cancelled["stages"]["nodeBody"]["failedOrCancelled"], 1);
    assert_eq!(cancelled["stages"]["nodeBody"]["inFlight"], 0);
    assert_eq!(cancelled["stages"]["putRequest"]["started"], 0);
    node_server.abort();
    s3_server.abort();
}

#[tokio::test]
async fn transfer_diagnostics_measure_base_get_decode_and_failed_put_without_losing_volume() {
    let root = tempfile::tempdir().unwrap();
    let s = service(root.path()).await;
    let object = shared_blocks::Object {
        id: id(),
        hash: digest::block(&vec![42; 4096]),
        size: 4096,
        ready: false,
    };
    let scope = shared_blocks::key(&object.hash, &object.id);
    let encoded = s
        .hot_s3
        .block_codec
        .encode(s.vault.clone(), scope, vec![42; 4096])
        .await
        .unwrap();
    let encoded_len = encoded.len();
    let fail = Arc::new(AtomicBool::new(false));
    let body_release = Arc::new(tokio::sync::Notify::new());
    let (url, server) = peer(Router::new().fallback({
        let fail = fail.clone();
        let body_release = body_release.clone();
        move |request: Request| {
            let encoded = encoded.clone();
            let fail = fail.clone();
            let body_release = body_release.clone();
            async move {
                tokio::time::sleep(Duration::from_millis(20)).await;
                if request.method() == "GET" {
                    return Body::from_stream(futures_util::stream::once(async move {
                        body_release.notified().await;
                        Ok::<_, std::io::Error>(Bytes::from(encoded))
                    }))
                    .into_response();
                }
                if fail.load(Ordering::Relaxed) {
                    StatusCode::SERVICE_UNAVAILABLE.into_response()
                } else {
                    StatusCode::OK.into_response()
                }
            }
        }
    }))
    .await;
    let logical_endpoint = "https://diagnostic-fixture.invalid";
    let storage = fixture_storage_at(&url, logical_endpoint, "diagnostics", s.hot_s3.clone()).await;
    tokio::fs::write(
        s.config.data_dir.join("storage-s3.json"),
        json!({ "bucket": "diagnostics", "region": "us-east-1", "endpoint": logical_endpoint })
            .to_string(),
    )
    .await
    .unwrap();
    let run = json!({});
    let node = json!("remote");
    let snapshot = json!({});
    let run_id = id();
    let snapshot_id = id();
    let attempt = id();
    let publication = Publication {
        authorize: crate::authorization::Authorization::default(),
        s: &s,
        run: &run,
        run_id: &run_id,
        attempt: "",
        publication_attempt: &attempt,
        node: &node,
        storage: &storage,
        base: "http://127.0.0.1:1",
        credential: "fixture",
        snapshot: &snapshot,
        snapshot_id: &snapshot_id,
        budget: 0,

        block_envelope: 2,
    };
    let backup = Arc::new(
        json!({ "destination": "s3", "endpoint": logical_endpoint, "bucket": "diagnostics", "runId": run_id }),
    );
    let block = json!({ "hash": object.hash, "object": object.id, "size": object.size });
    let sources = HashMap::from([(object.hash.clone(), (backup, block))]);
    let metrics = diagnostic::Metrics::new(&attempt, &run_id, &snapshot_id, "manager");
    for fails in [false, true] {
        fail.store(fails, Ordering::Relaxed);
        let metrics = if fails {
            diagnostic::Metrics::new(&id(), &run_id, &snapshot_id, "manager")
        } else {
            metrics.clone()
        };
        let transfer = diagnostic::scope(Some(metrics.clone()), async {
            let mut reads = snapshots::Fetch::new(
                s.disk_http.clone(),
                "http://127.0.0.1:1".into(),
                "fixture".into(),
                vec![],
            );
            let mut uploads = JoinSet::new();
            assert_eq!(
                publication
                    .start_uploads(
                        std::slice::from_ref(&object),
                        &sources,
                        &mut reads,
                        &mut uploads
                    )
                    .await
                    .unwrap(),
                4096
            );
            let result = uploads.join_next().await.unwrap().unwrap();
            assert_eq!(result.is_err(), fails);
        });
        tokio::time::timeout(Duration::from_secs(10), async {
            tokio::join!(transfer, async {
                // A server-side sleep can expire before the SDK returns headers.
                // Hold the body until its client-side measurement is active.
                while metrics.snapshot()["stages"]["getBody"]["inFlight"] != 1 {
                    tokio::task::yield_now().await;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
                let live = metrics.snapshot();
                assert_eq!(live["stages"]["getHeaders"]["completed"], 1);
                assert_eq!(live["stages"]["getBody"]["inFlight"], 1);
                assert_eq!(live["stages"]["getBody"]["completed"], 0);
                assert!(live["stages"]["getBody"]["wallUsSum"].as_u64().unwrap() >= 15_000);
                body_release.notify_one();
            });
        })
        .await
        .expect("GET body measurement must start before releasing the fixture response");
        let value = metrics.snapshot();
        let stages = &value["stages"];
        for stage in [
            "baseRead",
            "getBody",
            "decrypt",
            "decompress",
            "compress",
            "encrypt",
        ] {
            assert_eq!(stages[stage]["completed"], 1, "{stage}: {value}");
        }
        assert_eq!(stages["baseRead"]["bytes"], 4096);
        assert_eq!(stages["getBody"]["bytes"], encoded_len);
        assert_eq!(stages["decompress"]["bytes"], 4096);
        assert_eq!(stages["nodeHeaders"]["started"], 0);
        assert_eq!(stages["putRequest"]["failedOrCancelled"], u64::from(fails));
        assert!(stages["getHeaders"]["wallUsSum"].as_u64().unwrap() >= 15_000);
        assert!(stages["getBody"]["wallUsSum"].as_u64().unwrap() >= 15_000);
    }
    server.abort();
}

#[derive(Clone)]
struct LogWriter(Arc<std::sync::Mutex<Vec<u8>>>);

impl std::io::Write for LogWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn default_logging_filter_correlates_pretransfer_failure_and_cancelled_summary() {
    isolated_logging_test("nodes::publication::diagnostic_tests::default_logging_filter_child")
        .await;
}

async fn isolated_logging_test(name: &str) {
    // Tracing callsite interest is process-global. Other publication tests emit
    // the same events without a subscriber, racing these scoped log assertions.
    let output = tokio::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", name, "--ignored"])
        .output()
        .await
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success() && stdout.contains("1 passed; 0 failed;"),
        "{stdout}\n{stderr}"
    );
}

#[tokio::test]
#[ignore = "isolated process-global tracing callsite child"]
async fn default_logging_filter_child() {
    use tracing::instrument::WithSubscriber;
    let root = tempfile::tempdir().unwrap();
    let s = service(root.path()).await;
    let bytes = Arc::new(std::sync::Mutex::new(Vec::new()));
    let output = bytes.clone();
    let subscriber = tracing_subscriber::fmt()
        .json()
        .with_env_filter("warn,cairn_performance=info")
        .with_writer(move || LogWriter(output.clone()))
        .finish();
    async {
        assert!(
            capture_owned(
                &s,
                &json!({ "id": id() }),
                CaptureProtocol::Direct,
                crate::authorization::Authorization::default()
            )
            .await
            .is_err()
        );
        let metrics =
            diagnostic::Metrics::new(&id(), "credential-not-a-uuid", "path-not-a-uuid", "manager");
        let _summary = diagnostic::Summary {
            metrics,
            outcome: "cancelled",
        };
    }
    .with_subscriber(subscriber)
    .await;
    let logs = String::from_utf8(bytes.lock().unwrap().clone()).unwrap();
    let rows: Vec<Value> = logs
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let phase = rows
        .iter()
        .find(|row| {
            row["fields"]["operation"] == "disk_publication"
                && row["fields"]["event"] == "incomplete"
        })
        .unwrap();
    let attempt = phase["span"]["publication_attempt"].as_str().unwrap();
    crate::validation::uuid(attempt).unwrap();
    assert_eq!(
        rows.iter()
            .find(|row| row["fields"]["operation"] == "s3_totals")
            .unwrap()["fields"]["publication_attempt"],
        attempt
    );
    let summary = rows
        .iter()
        .find(|row| row["fields"]["operation"] == "disk_publication_transfer")
        .unwrap();
    assert_eq!(summary["fields"]["outcome"], "cancelled");
    assert_eq!(summary["fields"]["run_id"], "unknown");
    assert!(!logs.contains("credential-not-a-uuid"));
    assert!(!logs.contains("path-not-a-uuid"));
}

#[tokio::test]
async fn old_controller_drain_retries_lost_put_and_ack_across_restart_without_direct_fallback() {
    use crate::{node_fixture::StoreNodeFixture, run_fixture::DbRunFixture};
    use std::collections::BTreeMap;
    use std::sync::atomic::AtomicUsize;

    let root = tempfile::tempdir().unwrap();
    let run = id();
    let attempt = id();
    let grant = id();
    let bytes = vec![37; 4096];
    let hash = digest::block(&bytes);
    let manifest = json!({
        "version": 1,
        "size": bytes.len(),
        "blockSize": snapshots::BLOCK,
        "blocks": [{ "offset": 0, "size": bytes.len(), "hash": hash }],
        "onDemand": true,
        "generation": 1,
        "capturedAt": now()
    });
    let retained = Arc::new(AtomicBool::new(true));
    let lose_ack = Arc::new(AtomicBool::new(true));
    let legacy_reads = Arc::new(AtomicUsize::new(0));
    let unsupported = Arc::new(AtomicUsize::new(0));
    let baselines = Arc::new(std::sync::Mutex::new(Vec::new()));
    let controller = Router::new().fallback({
        let baselines = baselines.clone();
        let (manifest, grant, bytes, retained, lose_ack, legacy_reads, unsupported) = (
            manifest.clone(),
            grant.clone(),
            bytes.clone(),
            retained.clone(),
            lose_ack.clone(),
            legacy_reads.clone(),
            unsupported.clone(),
        );
        move |request: Request| {
            let baselines = baselines.clone();
            let (manifest, grant, bytes, retained, lose_ack, legacy_reads, unsupported) = (
                manifest.clone(),
                grant.clone(),
                bytes.clone(),
                retained.clone(),
                lose_ack.clone(),
                legacy_reads.clone(),
                unsupported.clone(),
            );
            async move {
                let path = request.uri().path();
                if path == "/health" {
                    return axum::Json(json!({ "status": "ok", "nodeProtocol": 2 }))
                        .into_response();
                }
                if path.ends_with("/direct-reader")
                    || path.ends_with("/direct-upload")
                    || path.ends_with("/publication-fence")
                {
                    unsupported.fetch_add(1, Ordering::Relaxed);
                    return StatusCode::METHOD_NOT_ALLOWED.into_response();
                }
                if path.ends_with("/snapshot") {
                    let body = to_bytes(request.into_body(), 4096).await.unwrap();
                    let input: Value = serde_json::from_slice(&body).unwrap();
                    baselines.lock().unwrap().push(input["baseline"].clone());
                    return axum::Json(
                        json!({ "id": id(), "manifest": manifest, "grantId": grant }),
                    )
                    .into_response();
                }
                if path.ends_with("/publication") {
                    legacy_reads.fetch_add(1, Ordering::Relaxed);
                    return bytes.into_response();
                }
                if path.ends_with("/published") {
                    if lose_ack.load(Ordering::Relaxed) {
                        return StatusCode::SERVICE_UNAVAILABLE.into_response();
                    }
                    retained.store(false, Ordering::Relaxed);
                }
                axum::Json(json!({ "ok": true })).into_response()
            }
        }
    });
    let (controller, controller_task) = peer(controller).await;
    let lost_put = Arc::new(AtomicBool::new(true));
    let objects = Arc::new(tokio::sync::Mutex::new(BTreeMap::<String, Vec<u8>>::new()));
    let provider = Router::new().fallback({
        let (objects, lost_put) = (objects.clone(), lost_put.clone());
        move |request: Request| {
            let (objects, lost_put) = (objects.clone(), lost_put.clone());
            async move {
                let key = request.uri().path().to_owned();
                if request.method() == axum::http::Method::DELETE {
                    objects.lock().await.remove(&key);
                    return StatusCode::NO_CONTENT.into_response();
                }
                if request.method() == axum::http::Method::PUT {
                    let body = to_bytes(request.into_body(), 1024 * 1024).await.unwrap();
                    objects.lock().await.insert(key, body.to_vec());
                    if lost_put.swap(false, Ordering::Relaxed) {
                        return StatusCode::SERVICE_UNAVAILABLE.into_response();
                    }
                    return StatusCode::OK.into_response();
                }
                if let Some(bytes) = objects.lock().await.get(&key) {
                    return bytes.clone().into_response();
                }
                StatusCode::NOT_FOUND.into_response()
            }
        }
    });
    let (provider, provider_task) = peer(provider).await;
    let logical = "https://upgrade-fixture.example";
    let s = service_with_runner(root.path(), controller.clone()).await;
    std::fs::write(
        s.config.data_dir.join("storage-s3.json"),
        json!({ "bucket": "fixture", "endpoint": logical, "region": "us-east-1" }).to_string(),
    )
    .unwrap();
    fixture_storage_at(&provider, logical, "fixture", s.hot_s3.clone()).await;
    s.store
        .put_node_fixture(json!({ "id": super::super::LOCAL_NODE_ID, "revoked": false }))
        .await
        .unwrap();
    let (saved_run, saved_attempt, saved_grant) = (run.clone(), attempt.clone(), grant.clone());
    s.store
        .transaction(move |db| {
            let record = json!({
                "id": saved_run,
                "taskId": id(),
                "status": "succeeded",
                "createdAt": 0,
                "nodeId": super::super::LOCAL_NODE_ID,
                "sessionId": "old-controller-session",
                "snapshot": { "agent": { "id": crate::config::MAIN_AGENT_ID } }
            });
            db.add_run_fixture(&record, None)?;
            db.save_checkpoint_boundary(
                &saved_run,
                &json!({
                    "nodeId": super::super::LOCAL_NODE_ID,
                    "runnerId": saved_attempt
                }),
            )?;
            db.put(
                "node-disk-grants",
                &json!({
                    "id": saved_grant,
                    "runId": saved_run,
                    "nodeId": super::super::LOCAL_NODE_ID,
                    "backups": []
                }),
            )?;
            Ok(())
        })
        .await
        .unwrap();
    // A provider response is lost after accepting bytes; no pointer/ACK is committed.
    assert!(
        capture_for_update(
            &s,
            &s.store.run(&run).await.unwrap(),
            super::super::LOCAL_NODE_ID
        )
        .await
        .is_err()
    );
    assert!(retained.load(Ordering::Relaxed));
    assert!(s.store.run(&run).await.unwrap()["backup"]["id"].is_null());
    // The next upload commits, but the old controller loses its ACK response.
    assert!(
        capture_for_update(
            &s,
            &s.store.run(&run).await.unwrap(),
            super::super::LOCAL_NODE_ID
        )
        .await
        .is_err()
    );
    assert!(retained.load(Ordering::Relaxed));
    let first = s.store.run(&run).await.unwrap()["backup"]["id"].clone();
    let previous_snapshot = s.store.run(&run).await.unwrap()["backup"]["snapshotId"].clone();
    assert!(first.is_string());
    assert_eq!(unsupported.load(Ordering::Relaxed), 0);
    assert_eq!(
        s.store
            .read(|db| Ok(db.0.query_row(
                "SELECT count(*) FROM direct_journal_acknowledgements",
                [],
                |r| r.get::<_, i64>(0)
            )?))
            .await
            .unwrap(),
        0
    );
    drop(s);
    let s = service_with_runner(root.path(), controller).await;
    fixture_storage_at(&provider, logical, "fixture", s.hot_s3.clone()).await;
    lose_ack.store(false, Ordering::Relaxed);
    let point = capture_for_update(
        &s,
        &s.store.run(&run).await.unwrap(),
        super::super::LOCAL_NODE_ID,
    )
    .await
    .unwrap();
    assert_ne!(point["blockEnvelope"], 3);
    assert!(!retained.load(Ordering::Relaxed));
    assert_eq!(
        s.store.run(&run).await.unwrap()["backup"]["status"],
        "ready"
    );
    assert_eq!(unsupported.load(Ordering::Relaxed), 0);
    let saved = s
        .get("node-backups", point["id"].as_str().unwrap())
        .await
        .unwrap();
    assert_eq!(
        baselines.lock().unwrap().as_slice(),
        &[Value::Null, Value::Null, previous_snapshot]
    );
    let manifest = super::manifest(&s, &saved).await.unwrap();
    assert_eq!(
        read_manifest_block(&s, &saved, &manifest["blocks"][0])
            .await
            .unwrap(),
        bytes
    );
    let reads = legacy_reads.load(Ordering::Relaxed);
    // An ordinary direct capture still fails on this old controller; it cannot
    // retry its payload through the explicit maintenance compatibility route.
    assert!(
        capture(&s, &s.store.run(&run).await.unwrap())
            .await
            .is_err()
    );
    assert_eq!(unsupported.load(Ordering::Relaxed), 1);
    assert_eq!(legacy_reads.load(Ordering::Relaxed), reads);
    controller_task.abort();
    provider_task.abort();
}

#[tokio::test]
async fn snapshot_fallback_rechecks_after_the_completed_capture_response() {
    let root = tempfile::tempdir().unwrap();
    let s = service(root.path()).await;
    let allowed = Arc::new(AtomicBool::new(true));
    let fallback = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let (origin, server) = peer(Router::new().fallback({
        let allowed = allowed.clone();
        let fallback = fallback.clone();
        move |request: Request| {
            let allowed = allowed.clone();
            let fallback = fallback.clone();
            async move {
                if request.uri().path().ends_with("snapshot-completed") {
                    allowed.store(false, Ordering::SeqCst);
                    StatusCode::PRECONDITION_FAILED
                } else {
                    fallback.fetch_add(1, Ordering::SeqCst);
                    StatusCode::OK
                }
            }
        }
    }))
    .await;
    let authority = crate::authorization::Authorization::new(&s, move |_| {
        if allowed.load(Ordering::SeqCst) {
            Ok(())
        } else {
            Err(Error::forbidden("Revoked"))
        }
    });
    let run = json!({
        "id": id(),
        "status": "succeeded",
        "backup": {
            "requestedRevision": 2,
            "acknowledgedRevision": 1,
            "blockEnvelope": 3,
        },
    });
    let error = request_snapshot(
        &s,
        &run,
        &origin,
        "attempt",
        "synthetic",
        CaptureProtocol::Direct,
        &authority,
    )
    .await
    .unwrap_err();
    assert_eq!(error.status, 403);
    assert_eq!(fallback.load(Ordering::SeqCst), 0);
    server.abort();
    s.shutdown.cancel();
}

#[tokio::test]
async fn direct_storage_probe_rechecks_before_its_second_write_and_cleans_its_own_key() {
    let root = tempfile::tempdir().unwrap();
    let s = service(root.path()).await;
    let allowed = Arc::new(AtomicBool::new(true));
    let puts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let deletes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let (origin, server) = peer(Router::new().fallback({
        let allowed = allowed.clone();
        let puts = puts.clone();
        let deletes = deletes.clone();
        move |request: Request| {
            let allowed = allowed.clone();
            let puts = puts.clone();
            let deletes = deletes.clone();
            async move {
                assert!(request.uri().path().contains("/direct-probes/"));
                if request.method() == axum::http::Method::PUT {
                    puts.fetch_add(1, Ordering::SeqCst);
                    allowed.store(false, Ordering::SeqCst);
                } else if request.method() == axum::http::Method::DELETE {
                    deletes.fetch_add(1, Ordering::SeqCst);
                }
                StatusCode::OK
            }
        }
    }))
    .await;
    let storage = fixture_storage(&origin, "private", s.hot_s3.clone()).await;
    let authority = crate::authorization::Authorization::new(&s, move |_| {
        if allowed.load(Ordering::SeqCst) {
            Ok(())
        } else {
            Err(Error::forbidden("Revoked"))
        }
    });
    assert_eq!(
        storage
            .require_direct_authorized(&authority)
            .await
            .unwrap_err()
            .status,
        403
    );
    assert_eq!(puts.load(Ordering::SeqCst), 1);
    assert_eq!(deletes.load(Ordering::SeqCst), 1);
    server.abort();
    s.shutdown.cancel();
}

#[test]
fn publication_failure_context_keeps_status_only_412_unknown_without_error_text() {
    let run = json!({ "backup": { "requestedRevision": 9 } });
    let error = crate::error::Error::new(412, "untrusted detail must not be copied");

    let context = super::failure_context(&run, &error);

    assert_eq!(context["phase"], "publication");
    assert_eq!(context["category"], "unknown");
    assert_eq!(context["managerStatus"], 412);
    assert_eq!(context["inputRequestedRevision"], 9);
    assert!(context.get("message").is_none());
}

#[tokio::test]
async fn completed_capture_distinguishes_missing_journal_from_legacy_conflict() {
    let root = tempfile::tempdir().unwrap();
    let missing_journal = Arc::new(AtomicBool::new(true));
    let node = Router::new().fallback({
        let missing_journal = missing_journal.clone();
        move |request: Request| {
            let missing_journal = missing_journal.clone();
            async move {
                assert!(
                    request.uri().path().ends_with("/snapshot-completed")
                        || request.uri().path().ends_with("/snapshot")
                );
                let body = if missing_journal.load(Ordering::Relaxed) {
                    json!({
                        "error": "Conversation has no native journal.",
                        "code": "missing_native_journal"
                    })
                } else {
                    json!({ "error": "legacy node conflict" })
                };
                (StatusCode::CONFLICT, axum::Json(body)).into_response()
            }
        }
    });
    let (base, server) = peer(node).await;
    let s = service(root.path()).await;
    let run = json!({
        "id": id(),
        "status": "cancelled",
        "backup": {
            "requestedRevision": 2,
            "acknowledgedRevision": 1,
            "blockEnvelope": 3
        }
    });

    let missing = super::request_snapshot(
        &s,
        &run,
        &base,
        &id(),
        "fixture",
        super::CaptureProtocol::Direct,
        &crate::authorization::Authorization::default(),
    )
    .await
    .unwrap_err();
    assert_eq!(missing.status, 409);
    assert_eq!(
        missing.code.as_deref(),
        Some(crate::native::MISSING_NATIVE_JOURNAL_CODE)
    );
    assert_eq!(missing.message, "Conversation has no native journal.");

    missing_journal.store(false, Ordering::Relaxed);
    let legacy = super::request_snapshot(
        &s,
        &run,
        &base,
        &id(),
        "fixture",
        super::CaptureProtocol::Direct,
        &crate::authorization::Authorization::default(),
    )
    .await
    .unwrap_err();
    assert_eq!(legacy.status, 425);
    assert_eq!(legacy.message, "Waiting for completed generation capture.");

    missing_journal.store(true, Ordering::Relaxed);
    let mut older_envelope = run.clone();
    older_envelope["backup"]["id"] = id().into();
    older_envelope["backup"]["blockEnvelope"] = 2.into();
    let missing_legacy_protocol = super::request_snapshot(
        &s,
        &older_envelope,
        &base,
        &id(),
        "fixture",
        super::CaptureProtocol::Direct,
        &crate::authorization::Authorization::default(),
    )
    .await
    .unwrap_err();
    assert_eq!(missing_legacy_protocol.status, 409);
    assert_eq!(
        missing_legacy_protocol.code.as_deref(),
        Some(crate::native::MISSING_NATIVE_JOURNAL_CODE)
    );

    server.abort();
    s.shutdown.cancel();
}

#[tokio::test]
async fn missing_journal_publication_failure_preserves_the_last_good_baseline_and_request() {
    use crate::{
        agent_fixture::StoreAgentFixture, node_fixture::StoreNodeFixture, run_fixture::DbRunFixture,
    };

    let root = tempfile::tempdir().unwrap();
    let run_id = id();
    let runner_id = id();
    let last_good_backup = id();
    let last_good_snapshot = id();
    let expected_last_good_backup = last_good_backup.clone();
    let expected_last_good_snapshot = last_good_snapshot.clone();
    let controller = Router::new().fallback(|request: Request| async move {
        assert!(request.uri().path().ends_with("/snapshot-completed"));
        (
            StatusCode::CONFLICT,
            axum::Json(json!({
                "error": "Conversation has no native journal.",
                "code": "missing_native_journal"
            })),
        )
            .into_response()
    });
    let (controller, controller_task) = peer(controller).await;
    let s = service_with_runner(root.path(), controller).await;
    let node_id = super::super::LOCAL_NODE_ID;
    let logical_endpoint = "https://missing-journal-fixture.invalid";
    tokio::fs::write(
        s.config.data_dir.join("storage-s3.json"),
        json!({
            "bucket": "diagnostics",
            "endpoint": logical_endpoint,
            "region": "us-east-1"
        })
        .to_string(),
    )
    .await
    .unwrap();
    s.store
        .put_node_fixture(json!({ "id": node_id, "revoked": false }))
        .await
        .unwrap();
    s.store
        .put_agent_fixture(json!({
            "id": crate::config::MAIN_AGENT_ID,
            "access": { "nodes": [node_id] }
        }))
        .await
        .unwrap();
    let (
        saved_run,
        saved_node,
        saved_runner,
        saved_checkpoint_run,
        saved_point_run,
        saved_point_node,
        saved_backup_id,
        saved_snapshot_id,
        saved_endpoint,
    ) = (
        run_id.clone(),
        node_id.to_owned(),
        runner_id.clone(),
        run_id.clone(),
        run_id.clone(),
        node_id.to_owned(),
        last_good_backup.clone(),
        last_good_snapshot.clone(),
        logical_endpoint.to_owned(),
    );
    s.store
        .transaction(move |db| {
            db.add_run_fixture(
                &json!({
                    "id": saved_run,
                    "taskId": id(),
                    "status": "cancelled",
                    "createdAt": 0,
                    "nodeId": saved_node,
                    "isolated": true,
                    "snapshot": { "agent": { "id": crate::config::MAIN_AGENT_ID } },
                    "storage": { "mode": "on-demand", "dirtyBytes": 4096, "localBytes": 8192 },
                    "backup": {
                        "status": "ready",
                        "id": last_good_backup,
                        "snapshotId": last_good_snapshot,
                        "capturedAt": now(),
                        "blockEnvelope": 3,
                        "requestedRevision": 2,
                        "acknowledgedRevision": 1
                    }
                }),
                None,
            )?;
            db.put(
                "node-backups",
                &json!({
                    "id": saved_backup_id,
                    "runId": saved_point_run,
                    "nodeId": saved_point_node,
                    "destination": "s3",
                    "endpoint": saved_endpoint,
                    "bucket": "diagnostics",
                    "snapshotId": saved_snapshot_id,
                    "capturedAt": now()
                }),
            )?;
            db.save_checkpoint_boundary(
                &saved_checkpoint_run,
                &json!({
                    "nodeId": saved_node,
                    "runnerId": saved_runner
                }),
            )?;
            Ok(())
        })
        .await
        .unwrap();

    let attempt_run = s.store.run(&run_id).await.unwrap();
    super::attempt(&s, &attempt_run).await;

    let backup = s.store.run(&run_id).await.unwrap()["backup"].clone();
    assert_eq!(backup["status"], "error");
    assert_eq!(backup["error"], "Conversation has no native journal.");
    assert_eq!(
        backup["lastFailureContext"]["category"],
        "missing_native_journal"
    );
    assert_eq!(backup["lastFailureContext"]["managerStatus"], 409);
    assert_eq!(backup["requestedRevision"], 2);
    assert_eq!(backup["acknowledgedRevision"], 1);
    assert_eq!(backup["id"], expected_last_good_backup);
    assert_eq!(backup["snapshotId"], expected_last_good_snapshot);

    controller_task.abort();
    s.shutdown.cancel();
}

#[tokio::test]
async fn native_storage_refresh_marks_missing_metrics_stale_then_clears_on_recovery() {
    use crate::{node_fixture::StoreNodeFixture, run_fixture::DbRunFixture};

    let root = tempfile::tempdir().unwrap();
    let run_id = id();
    let runner_id = id();
    let node_id = super::super::LOCAL_NODE_ID;
    let missing_journal = Arc::new(AtomicBool::new(true));
    let controller = Router::new().fallback({
        let missing_journal = missing_journal.clone();
        let run_id = run_id.clone();
        move |request: Request| {
            let (missing_journal, run_id) = (missing_journal.clone(), run_id.clone());
            async move {
                assert_eq!(
                    request.uri().path(),
                    format!("/disks/{run_id}/storage-status")
                );
                let status = if missing_journal.load(Ordering::Relaxed) {
                    json!({
                        "mode": "native",
                        "availability": "unavailable",
                        "reason": "missing_native_journal"
                    })
                } else {
                    json!({
                        "mode": "native",
                        "dirtyBytes": 0,
                        "localBytes": 16384,
                        "activeLocalBytes": 2048
                    })
                };
                axum::Json(status).into_response()
            }
        }
    });
    let (controller, server) = peer(controller).await;
    let s = service_with_runner(root.path(), controller).await;
    s.store
        .put_node_fixture(json!({ "id": node_id, "revoked": false }))
        .await
        .unwrap();

    let (saved_run, saved_checkpoint_run, saved_node, saved_runner) = (
        run_id.clone(),
        run_id.clone(),
        node_id.to_owned(),
        runner_id.clone(),
    );
    s.store
        .transaction(move |db| {
            db.add_run_fixture(
                &json!({
                    "id": saved_run,
                    "taskId": id(),
                    "status": "cancelled",
                    "createdAt": 0,
                    "nodeId": saved_node,
                    "isolated": true,
                    "snapshot": { "agent": { "id": crate::config::MAIN_AGENT_ID } },
                    "storage": {
                        "mode": "native",
                        "availability": "unavailable",
                        "availabilityReason": "missing_native_journal",
                        "metricsAvailability": "stale",
                        "dirtyBytes": 4096,
                        "localBytes": 8192
                    },
                    "backup": {
                        "status": "error",
                        "id": "last-good-backup",
                        "snapshotId": "last-good-snapshot",
                        "requestedRevision": 2,
                        "acknowledgedRevision": 1
                    }
                }),
                None,
            )?;
            db.save_checkpoint_boundary(
                &saved_checkpoint_run,
                &json!({
                    "nodeId": saved_node,
                    "runnerId": saved_runner
                }),
            )?;
            Ok(())
        })
        .await
        .unwrap();

    let missing = s.store.run(&run_id).await.unwrap();
    super::super::storage::refresh(&s, &missing).await.unwrap();
    let recorded = s.store.run(&run_id).await.unwrap();
    assert_eq!(recorded["storage"]["dirtyBytes"], 4096);
    assert_eq!(recorded["storage"]["localBytes"], 8192);
    assert_eq!(recorded["storage"]["metricsAvailability"], "stale");
    assert_eq!(
        recorded["storage"]["availabilityReason"],
        "missing_native_journal"
    );
    assert_eq!(recorded["backup"]["requestedRevision"], 2);
    assert_eq!(recorded["backup"]["acknowledgedRevision"], 1);

    missing_journal.store(false, Ordering::Relaxed);
    super::super::storage::refresh(&s, &recorded).await.unwrap();
    let recovered = s.store.run(&run_id).await.unwrap();
    assert_eq!(recovered["storage"]["dirtyBytes"], 0);
    assert_eq!(recovered["storage"]["localBytes"], 16384);
    assert!(recovered["storage"].get("availability").is_none());
    assert!(recovered["storage"].get("metricsAvailability").is_none());
    assert_eq!(recovered["backup"]["requestedRevision"], 2);
    assert_eq!(recovered["backup"]["acknowledgedRevision"], 1);
    assert_eq!(recovered["backup"]["id"], "last-good-backup");
    assert_eq!(recovered["backup"]["snapshotId"], "last-good-snapshot");

    server.abort();
    s.shutdown.cancel();
}

async fn apply_delayed_storage_refresh_pair(
    s: &Service,
    run: &Value,
    first_status: Value,
    second_status: Value,
) {
    let (started, wait_until_started) = tokio::sync::oneshot::channel();
    let (release, wait_until_released) = tokio::sync::oneshot::channel();
    let first_status = async move {
        let _ = started.send(());
        wait_until_released
            .await
            .map_err(crate::error::Error::internal)?;
        Ok(first_status)
    };
    let mut first = Box::pin(super::super::storage::refresh_with_status(
        s,
        run,
        first_status,
    ));

    assert!(futures_util::poll!(first.as_mut()).is_pending());
    wait_until_started.await.unwrap();

    let mut second = Box::pin(super::super::storage::refresh_with_status(
        s,
        run,
        std::future::ready(Ok(second_status)),
    ));
    let second_completed = match futures_util::poll!(second.as_mut()) {
        std::task::Poll::Ready(result) => {
            result.unwrap();
            true
        }
        std::task::Poll::Pending => false,
    };

    release.send(()).unwrap();
    if second_completed {
        first.await.unwrap();
    } else {
        let (first, second) = tokio::join!(first.as_mut(), second.as_mut());
        first.unwrap();
        second.unwrap();
    }
}

#[tokio::test]
async fn concurrent_native_storage_refreshes_preserve_observation_order() {
    use crate::run_fixture::DbRunFixture;

    let root = tempfile::tempdir().unwrap();
    let s = service(root.path()).await;
    let run_id = id();
    let saved_run = run_id.clone();
    let node_id = super::super::LOCAL_NODE_ID;
    s.store
        .transaction(move |db| {
            db.add_run_fixture(
                &json!({
                    "id": saved_run,
                    "taskId": id(),
                    "status": "cancelled",
                    "createdAt": 0,
                    "nodeId": node_id,
                    "isolated": true,
                    "snapshot": { "agent": { "id": crate::config::MAIN_AGENT_ID } },
                    "storage": {
                        "mode": "native",
                        "availability": "unavailable",
                        "availabilityReason": "missing_native_journal",
                        "metricsAvailability": "stale",
                        "dirtyBytes": 4096,
                        "localBytes": 8192
                    },
                    "backup": {
                        "status": "error",
                        "id": "last-good-backup",
                        "snapshotId": "last-good-snapshot",
                        "requestedRevision": 2,
                        "acknowledgedRevision": 1
                    }
                }),
                None,
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let run = s.store.run(&run_id).await.unwrap();

    apply_delayed_storage_refresh_pair(
        &s,
        &run,
        json!({
            "mode": "native",
            "availability": "unavailable",
            "reason": "missing_native_journal"
        }),
        json!({
            "mode": "native",
            "dirtyBytes": 0,
            "localBytes": 16384,
            "activeLocalBytes": 2048
        }),
    )
    .await;
    let recovered = s.store.run(&run_id).await.unwrap();
    assert_eq!(recovered["storage"]["localBytes"], 16384);
    assert!(recovered["storage"].get("availability").is_none());
    assert!(recovered["storage"].get("metricsAvailability").is_none());
    assert_eq!(recovered["backup"]["id"], "last-good-backup");
    assert_eq!(recovered["backup"]["snapshotId"], "last-good-snapshot");
    assert_eq!(recovered["backup"]["requestedRevision"], 2);
    assert_eq!(recovered["backup"]["acknowledgedRevision"], 1);

    apply_delayed_storage_refresh_pair(
        &s,
        &run,
        json!({
            "mode": "native",
            "dirtyBytes": 0,
            "localBytes": 32768,
            "activeLocalBytes": 4096
        }),
        json!({
            "mode": "native",
            "availability": "unavailable",
            "reason": "missing_native_journal"
        }),
    )
    .await;
    let unavailable = s.store.run(&run_id).await.unwrap();
    assert_eq!(unavailable["storage"]["localBytes"], 32768);
    assert_eq!(unavailable["storage"]["availability"], "unavailable");
    assert_eq!(unavailable["storage"]["metricsAvailability"], "stale");
    assert_eq!(unavailable["backup"]["id"], "last-good-backup");
    assert_eq!(unavailable["backup"]["snapshotId"], "last-good-snapshot");
    assert_eq!(unavailable["backup"]["requestedRevision"], 2);
    assert_eq!(unavailable["backup"]["acknowledgedRevision"], 1);

    s.shutdown.cancel();
}

#[tokio::test]
async fn attempt_persists_bounded_failure_context_and_retains_it_after_recovery() {
    use crate::{
        agent_fixture::StoreAgentFixture, node_fixture::StoreNodeFixture, run_fixture::DbRunFixture,
    };

    let root = tempfile::tempdir().unwrap();
    let run_id = id();
    let runner_id = id();
    let snapshot_id = id();
    let node_id = super::super::LOCAL_NODE_ID;
    let manifest = json!({
        "version": 1,
        "size": 4096,
        "blockSize": snapshots::BLOCK,
        "blocks": [{ "offset": 0, "size": 4096, "hash": null }],
        "onDemand": false,
        "generation": 1,
        "capturedAt": now()
    });
    let wait_for_snapshot = Arc::new(AtomicBool::new(false));
    let controller = Router::new().fallback({
        let (manifest, run_id, snapshot_id, wait_for_snapshot) = (
            manifest.clone(),
            run_id.clone(),
            snapshot_id.clone(),
            wait_for_snapshot.clone(),
        );
        move |request: Request| {
            let (manifest, run_id, snapshot_id, wait_for_snapshot) = (
                manifest.clone(),
                run_id.clone(),
                snapshot_id.clone(),
                wait_for_snapshot.clone(),
            );
            async move {
                let path = request.uri().path();
                if path == format!("/disks/{run_id}/snapshot") {
                    if wait_for_snapshot.load(Ordering::Relaxed) {
                        return StatusCode::CONFLICT.into_response();
                    }
                    return axum::Json(json!({
                        "id": snapshot_id,
                        "manifest": manifest,
                        "grantId": id()
                    }))
                    .into_response();
                }
                if path == format!("/snapshots/{snapshot_id}/discard") {
                    return StatusCode::NO_CONTENT.into_response();
                }
                panic!("unexpected publication fixture route: {path}");
            }
        }
    });
    let (controller, controller_task) = peer(controller).await;

    let fail_upload = Arc::new(AtomicBool::new(true));
    let provider = Router::new().fallback({
        let fail_upload = fail_upload.clone();
        move |request: Request| {
            let fail_upload = fail_upload.clone();
            async move {
                assert_eq!(request.method(), "PUT");
                let _ = to_bytes(request.into_body(), 1_000_000).await.unwrap();
                if fail_upload.load(Ordering::Relaxed) {
                    StatusCode::SERVICE_UNAVAILABLE.into_response()
                } else {
                    StatusCode::OK.into_response()
                }
            }
        }
    });
    let (provider, provider_task) = peer(provider).await;

    let s = service_with_runner(root.path(), controller).await;
    let logical_endpoint = "https://publication-context-fixture.invalid";
    tokio::fs::write(
        s.config.data_dir.join("storage-s3.json"),
        json!({
            "bucket": "diagnostics",
            "endpoint": logical_endpoint,
            "region": "us-east-1"
        })
        .to_string(),
    )
    .await
    .unwrap();
    let _storage =
        fixture_storage_at(&provider, logical_endpoint, "diagnostics", s.hot_s3.clone()).await;
    s.store
        .put_node_fixture(json!({ "id": node_id, "revoked": false }))
        .await
        .unwrap();
    s.store
        .put_agent_fixture(json!({
            "id": crate::config::MAIN_AGENT_ID,
            "access": { "nodes": [node_id] }
        }))
        .await
        .unwrap();

    let (saved_run, saved_runner, saved_node) =
        (run_id.clone(), runner_id.clone(), node_id.to_owned());
    s.store
        .transaction(move |db| {
            let record_run = saved_run.clone();
            let record_node = saved_node.clone();
            db.add_run_fixture(
                &json!({
                    "id": record_run,
                    "taskId": id(),
                    "status": "succeeded",
                    "createdAt": 0,
                    "nodeId": record_node,
                    "isolated": true,
                    "snapshot": { "agent": { "id": crate::config::MAIN_AGENT_ID } },
                    "backup": {
                        "status": "ready",
                        "requestedRevision": 8,
                        "acknowledgedRevision": 8
                    }
                }),
                None,
            )?;
            db.save_checkpoint_boundary(
                &saved_run,
                &json!({
                    "nodeId": saved_node,
                    "runnerId": saved_runner
                }),
            )?;
            Ok(())
        })
        .await
        .unwrap();

    let mut attempt_run = s.store.run(&run_id).await.unwrap();
    assert!(attempt_run["backup"].get("lastFailureContext").is_none());
    attempt_run["backup"]["requestedRevision"] = 7.into();

    // The attempt receives revision 7 while its fresh publication read sees revision 8.
    // Context must identify the attempt input, never sample a newer live value.
    super::attempt(&s, &attempt_run).await;
    let failed = s.store.run(&run_id).await.unwrap();
    let backup = &failed["backup"];
    assert_eq!(backup["status"], "error");
    assert!(backup["error"].as_str().is_some());
    assert_eq!(backup["requestedRevision"], 8);
    assert_eq!(backup["acknowledgedRevision"], 8);
    let first_context = backup["lastFailureContext"].clone();
    assert_eq!(first_context["phase"], "publication");
    assert_eq!(first_context["category"], "unknown");
    assert_eq!(first_context["managerStatus"], 503);
    assert_eq!(first_context["inputRequestedRevision"], 7);
    assert!(
        first_context["observedAtMs"]
            .as_i64()
            .is_some_and(|value| value > 0)
    );
    let context = first_context.as_object().unwrap();
    assert_eq!(context.len(), 5);
    for key in context.keys() {
        assert!(
            [
                "observedAtMs",
                "phase",
                "category",
                "managerStatus",
                "inputRequestedRevision"
            ]
            .contains(&key.as_str())
        );
    }

    let events_after_failure = {
        let run_id = run_id.clone();
        s.store
            .read(move |db| Ok(db.events_fixture(&run_id, 0, 100)?.len()))
            .await
            .unwrap()
    };

    s.store
        .update_run_boundary(&run_id, json!({ "environmentState": "expired" }))
        .await
        .unwrap();
    // This manager 409 is caused by expiration before snapshot capture, so status
    // alone must leave the category unknown and must not produce a new alert.
    super::attempt(&s, &attempt_run).await;
    let conflicted = s.store.run(&run_id).await.unwrap();
    let conflict_context = conflicted["backup"]["lastFailureContext"].clone();
    assert_eq!(conflicted["backup"]["status"], "error");
    assert_eq!(conflicted["backup"]["requestedRevision"], 8);
    assert_eq!(conflicted["backup"]["acknowledgedRevision"], 8);
    assert_eq!(conflict_context["phase"], "publication");
    assert_eq!(conflict_context["category"], "unknown");
    assert_eq!(conflict_context["managerStatus"], 409);
    assert_eq!(conflict_context["inputRequestedRevision"], 7);
    let events_after_conflict = {
        let run_id = run_id.clone();
        s.store
            .read(move |db| Ok(db.events_fixture(&run_id, 0, 100)?.len()))
            .await
            .unwrap()
    };
    assert_eq!(events_after_conflict, events_after_failure);

    s.store
        .update_run_boundary(&run_id, json!({ "environmentState": null }))
        .await
        .unwrap();
    wait_for_snapshot.store(true, Ordering::Relaxed);
    super::attempt(&s, &attempt_run).await;
    let pending = s.store.run(&run_id).await.unwrap();
    assert_eq!(pending["backup"]["status"], "pending");
    assert_eq!(pending["backup"]["requestedRevision"], 8);
    assert_eq!(pending["backup"]["acknowledgedRevision"], 8);
    assert!(pending["backup"]["error"].is_null());
    assert_eq!(pending["backup"]["lastFailureContext"], conflict_context);
    let events_after_pending = {
        let run_id = run_id.clone();
        s.store
            .read(move |db| Ok(db.events_fixture(&run_id, 0, 100)?.len()))
            .await
            .unwrap()
    };
    assert_eq!(events_after_pending, events_after_failure);

    wait_for_snapshot.store(false, Ordering::Relaxed);
    fail_upload.store(false, Ordering::Relaxed);
    super::attempt(&s, &attempt_run).await;
    let recovered = s.store.run(&run_id).await.unwrap();
    assert_eq!(recovered["backup"]["status"], "ready");
    assert_eq!(recovered["backup"]["requestedRevision"], 8);
    assert_eq!(recovered["backup"]["acknowledgedRevision"], 8);
    assert!(recovered["backup"]["error"].is_null());
    assert_eq!(recovered["backup"]["lastFailureContext"], conflict_context);

    controller_task.abort();
    provider_task.abort();
}
