//! Local runner journals reopen without a manager origin or remote credentials.
mod common;

use cairn_installation::{
    config::{Config, id},
    nodes::connector,
    service::Service,
    storage::{bootstrap, policy::Policy, runtime},
};
use serde_json::json;
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

/// The manager origin written by `deploy/installations/host.py`.
const INSTALLATION_ORIGIN: &str = "http://manager:4310";

#[tokio::test]
async fn installation_runner_reopens_its_local_disk_without_remote_storage() {
    let root = TempDir::new().unwrap();
    let data = root.path().join("data");
    // SAFETY: this crate's only test sets the variable before starting any thread.
    unsafe { std::env::set_var("DATA_DIR", &data) };
    std::fs::create_dir(root.path().join("home")).unwrap();
    let _service = Service::new(Config {
        public_url: INSTALLATION_ORIGIN.into(),
        ..common::config(root.path())
    })
    .await
    .unwrap();

    // The manager's plan for its local node carries its own origin; the
    // runner prepares the conversation disk from it.
    let run = json!({ "id": id() });
    let storage = json!({
        "owner": run["id"],
        "policy": Policy {
            reserve_mi_b: 64,
            reserve_percent: 1,
            ..Policy::default()
        },
    });
    let directory = root
        .path()
        .join("runner-state/disks")
        .join(run["id"].as_str().unwrap());
    let stop = CancellationToken::new();
    bootstrap::prepare(&directory, 128 * 1024 * 1024, &storage, &stop)
        .await
        .unwrap();
    // A restarted controller reopens the disk from its local owner and policy.
    drop(runtime::load(&directory).await.unwrap());

    // Enrollment of a remote execution node
    // keeps requiring HTTPS even for the manager's own private origin.
    let node = root.path().join("node");
    let enrollment = connector::enroll(INSTALLATION_ORIGIN, &node)
        .await
        .unwrap_err();
    assert!(
        enrollment.message.contains("HTTPS"),
        "{}",
        enrollment.message
    );
    assert!(!node.join("identity.json").exists());

    // An identity saved with that origin cannot connect a node either, so no
    // node session can hand it to a remote runner.
    std::fs::create_dir(&node).unwrap();
    let identity = json!({
        "master": INSTALLATION_ORIGIN,
        "nodeId": id(),
        "token": "x".repeat(43),
    });
    std::fs::write(node.join("identity.json"), identity.to_string()).unwrap();
    let connection = connector::connect(&node, CancellationToken::new())
        .await
        .unwrap_err();
    assert!(
        connection.message.contains("HTTPS"),
        "{}",
        connection.message
    );
}
