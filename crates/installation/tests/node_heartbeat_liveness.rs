//! Real connector process against synthetic peers: no workload or host privilege.
use axum::{
    Json, Router,
    extract::Request,
    response::{IntoResponse, Response},
    routing::any,
};
use serde_json::{Value, json};
use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio_util::sync::CancellationToken;

async fn exercise(blocked: &'static str) {
    let root = tempfile::tempdir().unwrap();
    let data = root.path().join("data");
    let state = root.path().join("state");
    let identity = root.path().join("identity");
    for directory in [&data, &state, &identity] {
        tokio::fs::create_dir(directory).await.unwrap();
    }
    tokio::fs::write(
        data.join("runner-secret"),
        "synthetic-controller-credential",
    )
    .await
    .unwrap();
    let health_calls = Arc::new(AtomicUsize::new(0));
    let config_calls = Arc::new(AtomicUsize::new(0));
    let leases = Arc::new(AtomicUsize::new(0));
    let renewed = CancellationToken::new();
    let controller = Router::new().fallback(any({
        let (health_calls, config_calls, leases, renewed, data) = (health_calls.clone(), config_calls.clone(), leases.clone(), renewed.clone(), data.clone());
        move |request: Request| {
            let (health_calls, config_calls, leases, renewed, data) = (health_calls.clone(), config_calls.clone(), leases.clone(), renewed.clone(), data.clone());
            async move {
                let path = request.uri().path();
                if path == "/health" {
                    if health_calls.fetch_add(1, Ordering::SeqCst) > 0 && blocked == "health" {
                        return std::future::pending::<Response>().await;
                    }
                    let slots = if blocked == "budget" { 1 } else { 2 };
                    return Json(json!({
                        "status": "ok", "backend": "native", "runtimeId": "fixture-version", "nodeProtocol": 2,
                        "dataRoot": data, "capabilities": {}, "pool": {"capacity": 2},
                        "sharedResources": true, "exclusiveReservations": true,
                        "budget": {"slots": slots, "limits": {"cpu": 2,"memoryMiB": 2048,"diskMiB": 4096}},
                        "nativeMode": {"mode": "protected", "generation": null,"status": "applied"},
                        "storagePolicy": {"generation": 1, "policy": {}},
                    })).into_response();
                }
                if path.ends_with("/lease") && leases.fetch_add(1, Ordering::SeqCst) + 1 >= 3 {
                    renewed.cancel();
                }
                if (path == "/node-budget" && blocked == "budget") || (path == "/native-mode" && blocked == "mode") {
                    config_calls.fetch_add(1, Ordering::SeqCst);
                    return std::future::pending::<Response>().await;
                }
                Json(json!({})).into_response()
            }
        }
    }));
    let listener = tokio::net::UnixListener::bind(state.join("controller.sock")).unwrap();
    let controller = tokio::spawn(async move {
        axum::serve(listener, controller).await.unwrap();
    });
    let version_errors = Arc::new(AtomicUsize::new(0));
    let manager = Router::new().fallback(any({
        let version_errors = version_errors.clone();
        move |request: Request| {
            let version_errors = version_errors.clone();
            async move {
                if request.uri().path() == "/internal/nodes/heartbeat" {
                    let bytes = axum::body::to_bytes(request.into_body(), 100000).await.unwrap();
                    let report: Value = serde_json::from_slice(&bytes).unwrap();
                    if report["nodeProtocol"] != 2 {
                        version_errors.fetch_add(1, Ordering::SeqCst);
                        return axum::http::StatusCode::CONFLICT.into_response();
                    }
                    return Json(json!({
                        "slots": 2,"limits": {"cpu": 2,"memoryMiB": 2048,"diskMiB": 4096},
                        "leases": [{"id":"00000000-0000-4000-8000-000000000123","remainingMs":60000}],
                        "nativeMode": {"mode":"protected","generation": if blocked == "mode" {json!(1)} else {Value::Null}},
                        "storagePolicy": {"generation":1,"policy":{}}, "projectArchives": [],
                    })).into_response();
                }
                if request.uri().path() == "/internal/nodes/project-catalog" {
                    return Json(json!([])).into_response();
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
                Json(Value::Null).into_response()
            }
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let master = format!("http://{}", listener.local_addr().unwrap());
    let manager = tokio::spawn(async move {
        axum::serve(listener, manager).await.unwrap();
    });
    tokio::fs::write(identity.join("identity.json"), json!({"master":master,"nodeId":"00000000-0000-4000-8000-000000000456","token":"x".repeat(43)}).to_string()).await.unwrap();
    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_cairn"))
        .arg("node-connect")
        .arg(identity)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("APP_RUNTIME_ID", "fixture-version")
        .env("DATA_DIR", data)
        .env("RUNNER_STATE_DIR", state)
        .env("RUNNER_URL", "http://127.0.0.1:1")
        .env("CAIRN_NODE_BACKEND", "native")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let result = tokio::time::timeout(Duration::from_secs(20), renewed.cancelled()).await;
    let exited = child.try_wait().unwrap();
    child.kill().await.unwrap();
    child.wait().await.unwrap();
    controller.abort();
    manager.abort();
    result.expect("three renewals must arrive while diagnostics or configuration is blocked");
    assert!(exited.is_none(), "connector exited: {blocked}");
    assert_eq!(version_errors.load(Ordering::SeqCst), 0);
    if blocked != "health" {
        assert_eq!(
            config_calls.load(Ordering::SeqCst),
            1,
            "configuration must remain serialized"
        );
    }
}

#[tokio::test]
async fn stalled_diagnostics_and_configuration_do_not_interrupt_ongoing_leases() {
    tokio::join!(exercise("health"), exercise("budget"), exercise("mode"));
}
