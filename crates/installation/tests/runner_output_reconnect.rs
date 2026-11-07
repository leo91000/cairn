//! Exercise the shipped client, including stdout, across a truncated response.
use axum::{
    Json, Router,
    extract::Query,
    response::IntoResponse,
    routing::{get, post},
};
use serde_json::json;
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn output_reconnect_does_not_restart_stop_or_repeat_the_attempt() {
    let starts = Arc::new(AtomicUsize::new(0));
    let stops = Arc::new(AtomicUsize::new(0));
    let offsets = Arc::new(Mutex::new(Vec::new()));
    let drained = CancellationToken::new();
    let app = Router::new()
        .route("/runs/{id}", post({
            let starts = starts.clone();
            move || { let starts = starts.clone(); async move { starts.fetch_add(1, Ordering::SeqCst); Json(json!({})) } }
        }).delete({
            let stops = stops.clone();
            move || { let stops = stops.clone(); async move { stops.fetch_add(1, Ordering::SeqCst); Json(json!({})) } }
        }))
        .route("/runs/{id}/logs", get({
            let (offsets, drained) = (offsets.clone(), drained.clone());
            move |Query(query): Query<HashMap<String,String>>| {
                let (offsets, drained) = (offsets.clone(), drained.clone());
                async move {
                    assert_eq!(query.get("cursor").map(String::as_str), Some("1"));
                    let offset: u64 = query["offset"].parse().unwrap();
                    offsets.lock().unwrap().push(offset);
                    let body = if offset == 0 {
                        // The next packet is torn; only the complete first output
                        // has been acknowledged when the connection disappears.
                        "{\"type\":\"log\",\"offset\":100,\"event\":{\"type\":\"output\",\"stderr\":false,\"data\":\"Zmlyc3QK\"}}\n{\"type\":"
                    } else {
                        assert_eq!(offset, 100);
                        drained.cancel();
                        "{\"type\":\"log\",\"offset\":200,\"event\":{\"type\":\"output\",\"stderr\":false,\"data\":\"c2Vjb25kCg==\"}}\n{\"type\":\"end\",\"offset\":200}\n"
                    };
                    ([("content-type", "application/vnd.cairn.log-cursor+ndjson")],body).into_response()
                }
            }
        }))
        .route("/runs/{id}/wait", post(move || {
            let drained = drained.clone();
            async move { drained.cancelled().await; Json(json!({"StatusCode":0})) }
        }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let root = tempfile::tempdir().unwrap();
    let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_cairn"))
        .args(["runner-client", "00000000-0000-4000-8000-000000000123"])
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", root.path())
        .env("RUNNER_URL", format!("http://{address}"))
        .env("RUNNER_TOKEN", "synthetic-controller-credential")
        .kill_on_drop(true)
        .output();
    let output = tokio::time::timeout(Duration::from_secs(10), output)
        .await
        .unwrap()
        .unwrap();
    server.abort();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"first\nsecond\n");
    assert_eq!(*offsets.lock().unwrap(), [0, 100]);
    assert_eq!(starts.load(Ordering::SeqCst), 1);
    assert_eq!(stops.load(Ordering::SeqCst), 0);
}
