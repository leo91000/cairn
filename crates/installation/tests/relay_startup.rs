mod common;

use axum::{Router, http::StatusCode, routing::post};
use std::{process::Stdio, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::Command,
};

#[tokio::test]
async fn failed_claims_leave_the_installation_running_without_a_relay_identity() {
    let (listener, address) = common::bind().await;
    let server = common::serve(
        listener,
        Router::new().route(
            "/api/relay/claim",
            post(|| async { StatusCode::UNAUTHORIZED }),
        ),
    );
    let refused_origin = format!("http://{address}");
    let (unreachable, address) = common::bind().await;
    let unreachable_origin = format!("http://{address}");
    drop(unreachable);

    for beacon in [Some(refused_origin), Some(unreachable_origin), None] {
        let root = tempfile::tempdir().unwrap();
        tokio::fs::create_dir(root.path().join("data"))
            .await
            .unwrap();
        tokio::fs::create_dir(root.path().join("home"))
            .await
            .unwrap();
        let mut command = Command::new(env!("CARGO_BIN_EXE_cairn"));
        command
            .arg("serve")
            .env("DATA_DIR", root.path().join("data"))
            .env("AGENT_HOME", root.path().join("home"))
            .env("WORKSPACE_ROOTS", root.path())
            .env("WORKER_ENABLED", "false")
            .env("RUNNER_URL", "")
            .env("NODE_ENV", "test")
            .env("HOST", "127.0.0.1")
            .env("PORT", "0")
            .env("CAIRN_INSTALLATION_CLAIM_CODE", "fixture-expired-code")
            .env_remove("CAIRN_BEACON_ORIGIN")
            .env_remove("CAIRN_CONFIG")
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        if let Some(beacon) = beacon {
            command.env("CAIRN_BEACON_ORIGIN", beacon);
        }
        let mut child = command.spawn().unwrap();
        let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
        let url = tokio::time::timeout(Duration::from_secs(10), async {
            while let Some(line) = lines.next_line().await.unwrap() {
                if let Some(url) = line.strip_prefix("Listening on ") {
                    return url.to_owned();
                }
            }
            panic!("failed claim must not make cairn serve exit before opening its API");
        })
        .await
        .unwrap();
        assert_eq!(
            reqwest::get(format!("{url}/health"))
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );
        assert!(child.try_wait().unwrap().is_none());
        assert!(
            !root
                .path()
                .join("data/installation-relay/identity.json")
                .exists()
        );
        child.kill().await.unwrap();
        child.wait().await.unwrap();
    }
    server.abort();
}
