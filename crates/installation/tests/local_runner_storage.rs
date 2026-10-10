//! The installation's own runner reads disks through the manager's private
//! Docker origin, while every other plain-HTTP origin stays refused (#159).
//!
//! The controller finds its data root in `DATA_DIR`, like the generated Compose
//! services. This crate holds a single test so that no other test shares the
//! process environment it sets.
mod common;

use cairn_installation::{
    config::{Config, id},
    nodes::{LOCAL_NODE_ID, connector, disk_grants},
    service::Service,
    storage::{bootstrap, policy::Policy, remote::RemoteSource, runtime},
};
use serde_json::json;
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

/// The manager origin written by `deploy/installations/host.py`.
const INSTALLATION_ORIGIN: &str = "http://manager:4310";

fn refused(context: &serde_json::Value) -> String {
    RemoteSource::new(
        context,
        tokio::runtime::Handle::current(),
        CancellationToken::new(),
    )
    .err()
    .expect("plain-HTTP origin must be refused")
    .message
}

#[tokio::test]
async fn installation_runner_reads_disks_from_the_private_manager_origin_only() {
    let root = TempDir::new().unwrap();
    let data = root.path().join("data");
    // SAFETY: this crate's only test sets the variable before starting any thread.
    unsafe { std::env::set_var("DATA_DIR", &data) };
    std::fs::create_dir(root.path().join("home")).unwrap();
    let service = Service::new(Config {
        public_url: INSTALLATION_ORIGIN.into(),
        ..common::config(root.path())
    })
    .await
    .unwrap();

    // The manager's plan for its local node carries its own origin; the
    // runner prepares the conversation disk from it.
    let run = json!({ "id": id() });
    let grant = disk_grants::new_disk(&service, &run, LOCAL_NODE_ID)
        .await
        .unwrap();
    let storage = json!({
        "master": service.config.public_url,
        "grant": grant,
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
    // A restarted controller reopens the disk from its persisted authorization.
    drop(runtime::load(&directory).await.unwrap());

    // Another plain-HTTP origin, as a remote node's master could supply, is
    // refused even on the installation's own runner.
    for origin in [
        "http://master.internal:4310/",
        "http://10.0.0.2:4310/",
        "http://manager:4311/",
        "https://manager:4310/path",
    ] {
        let message = refused(&json!({ "master": origin, "grant": "fixture" }));
        assert!(message.contains("HTTPS"), "{origin}: {message}");
    }

    // Enrollment, and through it a remote node's connection and disk reads,
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
    let identity =
        json!({ "master": INSTALLATION_ORIGIN, "nodeId": id(), "token": "x".repeat(43) });
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
