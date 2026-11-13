use super::*;
use crate::config::id;
use axum::{Router, extract::Request, response::IntoResponse};
use std::{
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
};

async fn service(root: &Path) -> Arc<Service> {
    service_with_runner(root, String::new()).await
}

async fn service_with_runner(root: &Path, runner_url: String) -> Arc<Service> {
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

async fn peer(app: Router) -> (String, tokio::task::AbortHandle) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (url, server.abort_handle())
}

#[test]
fn missing_native_journal_marks_last_good_metrics_stale_without_zeroing_them() {
    let previous = json!({
        "mode": "native",
        "dirtyBytes": 4096,
        "localBytes": 8192,
        "activeLocalBytes": 1024,
    });
    let response = json!({
        "mode": "native",
        "availability": "unavailable",
        "reason": crate::native::MISSING_NATIVE_JOURNAL_CODE
    });

    let recorded = storage_with_missing_native_journal(&previous, &response, 123);

    assert_eq!(recorded["dirtyBytes"], 4096);
    assert_eq!(recorded["localBytes"], 8192);
    assert_eq!(recorded["activeLocalBytes"], 1024);
    assert_eq!(recorded["availability"], "unavailable");
    assert_eq!(recorded["availabilityReason"], "missing_native_journal");
    assert_eq!(recorded["metricsAvailability"], "stale");
    assert_eq!(recorded["availabilityObservedAt"], 123);
}

#[tokio::test]
async fn native_storage_refresh_marks_missing_metrics_stale_then_clears_on_recovery() {
    use crate::{node_fixture::StoreNodeFixture, run_fixture::DbRunFixture};

    let root = tempfile::tempdir().unwrap();
    let run_id = id();
    let runner_id = id();
    let node_id = crate::nodes::LOCAL_NODE_ID;
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
    refresh(&s, &missing).await.unwrap();
    let recorded = s.store.run(&run_id).await.unwrap();
    assert_eq!(recorded["storage"]["dirtyBytes"], 4096);
    assert_eq!(recorded["storage"]["localBytes"], 8192);
    assert_eq!(recorded["storage"]["metricsAvailability"], "stale");
    assert_eq!(
        recorded["storage"]["availabilityReason"],
        "missing_native_journal"
    );

    missing_journal.store(false, Ordering::Relaxed);
    refresh(&s, &recorded).await.unwrap();
    let recovered = s.store.run(&run_id).await.unwrap();
    assert_eq!(recovered["storage"]["dirtyBytes"], 0);
    assert_eq!(recovered["storage"]["localBytes"], 16384);
    assert!(recovered["storage"].get("availability").is_none());
    assert!(recovered["storage"].get("metricsAvailability").is_none());

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
    let mut first = Box::pin(refresh_with_status(s, run, first_status));

    assert!(futures_util::poll!(first.as_mut()).is_pending());
    wait_until_started.await.unwrap();

    let mut second = Box::pin(refresh_with_status(
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
    let node_id = crate::nodes::LOCAL_NODE_ID;
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

    s.shutdown.cancel();
}
