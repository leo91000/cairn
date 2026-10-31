use super::*;
use crate::{
    config::{Config, id},
    node_fixture::StoreNodeFixture,
    object_storage::tests::fixture_storage_at,
    run_fixture::DbRunFixture,
};
use axum::{Router, http::StatusCode, response::IntoResponse};
use std::{
    io::{self, Write},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

use tracing_subscriber::{Layer, layer::SubscriberExt};

#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl Write for Capture {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn maintenance_reports_actual_native_capture_retries_and_preserves_ownership() {
    for child in [
        "nodes::maintenance::diagnostic_tests::native_capture_retry_report_child",
        "nodes::maintenance::diagnostic_tests::disabled_summary_span_child",
    ] {
        let output = tokio::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", child, "--ignored", "--nocapture"])
            .output()
            .await
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
    }
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "isolated tracing dispatcher for actual maintenance path"]
async fn native_capture_retry_report_child() {
    let capture = Capture::default();
    tracing::subscriber::set_global_default(
        tracing_subscriber::fmt()
            .json()
            .with_ansi(false)
            .with_env_filter("info")
            .with_writer({
                let capture = capture.clone();
                move || capture.clone()
            })
            .finish(),
    )
    .unwrap();
    let private_span = tracing::info_span!(
        "private_fixture_parent",
        private = "private-context-must-not-escape"
    );
    let _entered = private_span.enter();
    let root = tempfile::tempdir().unwrap();
    let captures = Arc::new(AtomicUsize::new(0));
    let observed = captures.clone();
    let mode = Arc::new(AtomicUsize::new(0));
    let pause_entered = Arc::new(tokio::sync::Notify::new());
    let peer_mode = mode.clone();
    let peer_pause_entered = pause_entered.clone();
    let app = Router::new().fallback(move |request: Request| {
        let observed = observed.clone();
        let mode = peer_mode.clone();
        let entered = peer_pause_entered.clone();
        async move {
            if request.method() == "DELETE" {
                match mode.load(Ordering::SeqCst) {
                    1 => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
                    2 => {
                        entered.notify_one();
                        return std::future::pending().await;
                    }
                    _ => {}
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
                return StatusCode::NO_CONTENT.into_response();
            }
            if request.uri().path().ends_with("/health") {
                return axum::Json(json!({
                    "status": "ok",
                    "nodeProtocol": 2,
                    "directStorageProtocol": 1,
                }))
                .into_response();
            }
            if request.uri().path().ends_with("/snapshot-completed") {
                return if observed.fetch_add(1, Ordering::SeqCst) < 2 {
                    StatusCode::CONFLICT.into_response()
                } else {
                    StatusCode::SERVICE_UNAVAILABLE.into_response()
                };
            }
            StatusCode::NOT_FOUND.into_response()
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let peer = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let service = Service::new(Config {
        data_dir: root.path().join("data"),
        home: root.path().join("home"),
        workspace_roots: vec![root.path().into()],
        public_url: url.clone(),
        host: "127.0.0.1".into(),
        port: 0,
        codex_bin: "false".into(),
        claude_bin: "false".into(),
        opencode_bin: "false".into(),
        opencode_go_usage_url: crate::accounts::opencode::USAGE_URL.into(),
        concurrency: 1,
        logger: false,
        worker_enabled: false,
        runner_url: url.clone(),
    })
    .await
    .unwrap();
    let (run, attempt, node) = add_attempt(&service, "native", true, false).await;
    let logical = "https://maintenance-storage.example";
    std::fs::write(
        service.config.data_dir.join("storage-s3.json"),
        json!({
            "bucket": "fixture",
            "endpoint": logical,
            "region": "us-east-1",
        })
        .to_string(),
    )
    .unwrap();
    let _storage = fixture_storage_at(&url, logical, "fixture", service.hot_s3.clone()).await;
    let reply = request(&service, &node, &json!({ "action": "drain" }))
        .await
        .unwrap();
    assert_eq!(reply["maintenance"], "draining");
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if !service.node_maintenance_tasks.lock().await.contains(&node) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(captures.load(Ordering::SeqCst), 3);
    assert_ne!(
        service
            .store
            .get("node-attempts", &attempt)
            .await
            .unwrap()
            .unwrap()["released"],
        true
    );
    let logs = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
    let reports: Vec<Value> = logs
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|row| row["fields"]["operation"] == "node_maintenance_drain")
        .collect();
    assert_eq!(
        reports.len(),
        1,
        "Actual maintenance must report its bounded phase accounting"
    );
    let report = &reports[0]["fields"];
    assert_eq!(report["outcome"], "error");
    assert_eq!(report["pause_requests"], 1);
    assert_eq!(report["capture_calls"], 3);
    assert_eq!(report["capture_waits"], 2);
    assert_eq!(report["capture_failures"], 1);
    assert_eq!(report["placement_releases"], 0);
    assert!(report["capture_wait_ms"].as_u64().unwrap() >= 200);
    let encoded = reports[0].to_string();
    for (index, private) in [
        run.as_str(),
        attempt.as_str(),
        node.as_str(),
        url.as_str(),
        logical,
        "private-session",
        "private-context-must-not-escape",
    ]
    .into_iter()
    .enumerate()
    {
        assert!(
            !encoded.contains(private),
            "private fixture category {index}"
        );
    }

    // Both existing release paths retain their order and durable ownership change.
    for backend in ["native", "firecracker"] {
        capture.0.lock().unwrap().clear();
        let (_, attempt, node) = add_attempt(&service, backend, false, false).await;
        assert!(
            prepare(&service, &node, Duration::from_secs(5))
                .await
                .is_null()
        );
        assert_eq!(
            service
                .store
                .get("node-attempts", &attempt)
                .await
                .unwrap()
                .unwrap()["released"],
            true
        );
        let report = drain_report(&capture);
        assert_eq!(report["outcome"], "completed");
        assert_eq!(report["pause_requests"], 1);
        assert_eq!(report["placement_releases"], 1);
        assert_eq!(report["capture_calls"], 0);
    }

    capture.0.lock().unwrap().clear();
    mode.store(1, Ordering::SeqCst);
    let (_, attempt, node) = add_attempt(&service, "native", false, false).await;
    assert!(
        !prepare(&service, &node, Duration::from_secs(5))
            .await
            .is_null()
    );
    assert_ne!(
        service
            .store
            .get("node-attempts", &attempt)
            .await
            .unwrap()
            .unwrap()["released"],
        true
    );
    let failed = drain_report(&capture);
    assert_eq!(failed["outcome"], "error");
    assert_eq!(failed["pause_failures"], 1);
    assert_eq!(failed["placement_releases"], 0);

    capture.0.lock().unwrap().clear();
    let (_, _, node) = add_attempt(&service, "native", false, true).await;
    assert!(
        prepare(&service, &node, Duration::from_secs(5))
            .await
            .is_null()
    );
    let stale = drain_report(&capture);
    assert_eq!(stale["stale_attempts"], 1);
    assert_eq!(stale["pause_requests"], 0);
    assert_eq!(stale["placement_releases"], 0);

    for cancel in [false, true] {
        capture.0.lock().unwrap().clear();
        mode.store(2, Ordering::SeqCst);
        let (_, attempt, node) = add_attempt(&service, "native", false, false).await;
        let owner = service.clone();
        let task =
            tokio::spawn(async move { prepare(&owner, &node, Duration::from_secs(5)).await });
        tokio::time::timeout(Duration::from_secs(3), pause_entered.notified())
            .await
            .unwrap();
        if cancel {
            task.abort();
            assert!(task.await.unwrap_err().is_cancelled());
        } else {
            assert!(!task.await.unwrap().is_null());
        }
        let stopped = drain_report(&capture);
        assert_eq!(
            stopped["outcome"],
            if cancel { "cancelled" } else { "deadline" }
        );
        assert_eq!(stopped["phase"], "pause_request");
        assert_eq!(stopped["pause_requests"], 1);
        assert_eq!(stopped["pause_failures"], 0);
        assert_eq!(stopped["placement_releases"], 0);
        if !cancel {
            assert!(stopped["pause_request_ms"].as_u64().unwrap() >= 4000);
        }
        assert_ne!(
            service
                .store
                .get("node-attempts", &attempt)
                .await
                .unwrap()
                .unwrap()["released"],
            true
        );
    }
    service.shutdown.cancel();
    peer.abort();
}

fn drain_report(capture: &Capture) -> Value {
    let bytes = capture.0.lock().unwrap().clone();
    let logs = String::from_utf8(bytes).unwrap();
    let reports: Vec<Value> = logs
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|row| row["fields"]["operation"] == "node_maintenance_drain")
        .collect();
    assert_eq!(reports.len(), 1);
    let encoded = reports[0].to_string();
    assert!(!encoded.contains("private-context-must-not-escape"));
    let fields = reports[0]["fields"].clone();
    let phase_total: u64 = [
        "inventory_ms",
        "preparation_ms",
        "pause_request_ms",
        "capture_ms",
        "capture_wait_ms",
        "placement_release_ms",
    ]
    .into_iter()
    .map(|key| fields[key].as_u64().unwrap())
    .sum();
    let elapsed = fields["elapsed_ms"].as_u64().unwrap();
    assert!(elapsed >= phase_total && elapsed - phase_total <= 5);
    fields
}

async fn add_attempt(
    service: &Service,
    backend: &str,
    session: bool,
    stale: bool,
) -> (String, String, String) {
    let node = id();
    service
        .store
        .put_node_fixture(json!({
            "id": node,
            "capabilities": { "backend": backend },
        }))
        .await
        .unwrap();
    let run = id();
    let attempt = id();
    let backend = backend.to_owned();
    let checkpoint_attempt = if stale { id() } else { attempt.clone() };
    let (run_id, attempt_id, node_id) = (run.clone(), attempt.clone(), node.clone());
    service
        .store
        .transaction(move |db| {
            db.add_run_fixture(
                &json!({
                    "id": run_id,
                    "taskId": id(),
                    "createdAt": 0,
                    "status": "running",
                    "nodeId": node_id,
                    "sessionId": if session { json!("private-session") } else { Value::Null },
                    "backup": {
                        "requestedRevision": 1,
                        "acknowledgedRevision": 0,
                        "blockEnvelope": 3,
                    },
                    "snapshot": { "agent": { "access": { "backend": backend } } },
                }),
                None,
            )?;
            db.save_checkpoint_boundary(
                &run_id,
                &json!({
                    "nodeId": node_id,
                    "runnerId": checkpoint_attempt,
                }),
            )?;
            db.put(
                "node-attempts",
                &json!({
                    "id": attempt_id,
                    "runId": run_id,
                    "nodeId": node_id,
                    "role": "execution",
                    "released": false,
                }),
            )?;
            Ok(())
        })
        .await
        .unwrap();
    (run, attempt, node)
}

#[test]
#[ignore = "isolated tracing filter that disables only the summary span"]
fn disabled_summary_span_child() {
    let capture = Capture::default();
    let formatting = tracing_subscriber::fmt::layer()
        .json()
        .with_ansi(false)
        .with_writer({
            let capture = capture.clone();
            move || capture.clone()
        })
        .with_filter(tracing_subscriber::filter::filter_fn(|metadata| {
            metadata.is_event() || metadata.name() == "private_fixture_parent"
        }));
    tracing::subscriber::set_global_default(tracing_subscriber::registry().with(formatting))
        .unwrap();
    assert!(
        tracing::info_span!(target: "cairn_performance", parent: None, "maintenance_summary")
            .is_disabled()
    );
    assert!(tracing::event_enabled!(target: "cairn_performance", tracing::Level::INFO));
    let private = tracing::info_span!(
        "private_fixture_parent",
        private = "private-context-must-not-escape"
    );
    let _entered = private.enter();
    Drain::new().finish(Outcome::Completed);
    let bytes = capture.0.lock().unwrap().clone();
    let logs = String::from_utf8(bytes).unwrap();
    assert!(
        !logs.contains("node_maintenance_drain"),
        "Disabled root span must suppress the summary before JSON can inherit private context"
    );
}
