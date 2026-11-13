//! Synthetic retained local environment qualification; no real workspaces are opened.
mod common;

use cairn_installation::{
    config::now,
    nodes,
    nodes::run_placement::{MoveRequest, PlacementChanges},
    service::Service,
};
use common::{
    attempt_fixture::DbAttemptFixture, node_fixture::DbNodeFixture, run_fixture::DbRunFixture,
};
use serde_json::{Map, json};
use std::time::Instant;

const NODE: &str = "00000000-0000-4000-8000-000000000004";

async fn seed_retained_environments(service: &Service, kib: usize, count: u32) {
    service
        .store
        .transaction(move |db| {
            let instructions = "x".repeat(kib * 1024);
            let finished = now() - 60_000;
            let mut retained = Map::new();
            db.put_node_fixture(&json!({ "id": NODE }))?;
            for index in 0..count {
                let run = format!("run-{index}");
                let attempt = format!("attempt-{index}");
                                                db.add_run_fixture(
                    &json!({
                        "id": run, "taskId": run, "status": "succeeded", "finishedAt": finished,
                        "nodeId": NODE, "storage": { "mode": "native", "dirtyBytes": 0 },
                        "snapshot": { "agent": { "instructions": instructions } },
                    }),
                    None,
                )?;
                db.save_checkpoint_boundary(
                    &run,
                    &json!({
                        "nodeId": NODE, "runnerId": attempt,
                        "prepared": { "instructions": instructions },
                    }),
                )?;
                db.put_attempt_fixture(&json!({
                        "id": attempt, "runId": run, "nodeId": NODE,
                        "released": true, "releasedAt": finished, "createdAt": finished - 1000,
                    }))?;
                db.put(
                    "node-volumes",
                    &json!({
                        "id": format!("{run}:{NODE}"), "runId": run, "nodeId": NODE,
                        "materialized": true, "diskMiB": 1024, "activeDiskMiB": 1024,
                    }),
                )?;
                retained.insert(run, json!(1_048_576));
            }
            db.put_node_fixture(
                &json!({
                    "id": NODE, "name": "Retained native node", "lastSeen": now(),
                    "capabilities": { "backend": "native" }, "diskGrowthProtocol": 1,
                    "diskCapacity": {
                        "protocol": 1, "capturedAt": now(), "usedBytes": u64::from(count) * 1_048_576,
                        "freeBytes": 1_000_000_000, "ownedBytes": {}, "retainedBytes": retained, "requests": [],
                    },
                }),
            )?;
            Ok(())
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn retained_local_accounting_preserves_stale_ownership_and_queued_resumes() {
    let root = tempfile::tempdir().unwrap();
    common::managed_codex_home(&root.path().join("home"));
    let service = Service::new(common::config(root.path())).await.unwrap();
    seed_retained_environments(&service, 0, 9).await;
    let inventory = nodes::typed_inventory(&service).await.unwrap();
    assert_eq!(
        serde_json::to_value(&inventory[0]).unwrap()["diskAccounting"]["unconsolidatedBytes"],
        0
    );
    service
        .store
        .transaction(|db| {
            common::run_fixture::change_run_fixture(
                db,
                "run-0",
                &json!({ "status": "running", "finishedAt": null }),
            )?;
            common::run_fixture::change_run_fixture(
                db,
                "run-1",
                &json!({ "recoveryPending": true }),
            )?;
            db.update_run_placement(
                "run-2",
                PlacementChanges {
                    move_request: Some(Some(MoveRequest {
                        backend: None,
                        source_read_ready: None,
                        reservation: "destination-attempt".into(),
                        node_id: nodes::LOCAL_NODE_ID.into(),
                        resources: serde_json::from_value(
                            json!({ "cpu": 1, "memoryMiB": 128, "diskMiB": 128 }),
                        )?,
                        requested_at: None,
                        automatic: false,
                        idle: None,
                        idle_status: None,
                        required_tags: None,
                        backup_id: None,
                    })),
                    ..Default::default()
                },
            )?;
            db.save_checkpoint_boundary(
                "run-3",
                &json!({ "nodeId": nodes::LOCAL_NODE_ID, "runnerId": "attempt-3" }),
            )?;
            db.put_attempt_fixture(&json!({
                "id": "attempt-4", "runId": "run-4", "nodeId": NODE, "released": false,
            }))?;
            db.update_run_boundary(
                "run-5",
                &json!({ "storage": { "mode": "native", "dirtyBytes": 1 } }),
            )?;
            common::run_fixture::change_run_fixture(
                db,
                "run-8",
                &json!({
                    "status": "queued", "finishedAt": null, "recoveryPending": true,
                }),
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let inventory = nodes::typed_inventory(&service).await.unwrap();
    let node = serde_json::to_value(&inventory[0]).unwrap();
    // The inventory reports the local journal and stale ownership without
    // treating retained environments as remotely recoverable caches.
    assert_eq!(node["diskAccounting"]["unconsolidatedBytes"], 1);
    assert_eq!(node["staleDisks"]["count"], 1);
    service.shutdown.cancel();
}

#[tokio::test]
#[ignore = "explicit retained native inventory benchmark"]
async fn inventory_with_clean_retained_native_caches() {
    for kib in [0, 128, 512] {
        let root = tempfile::tempdir().unwrap();
        common::managed_codex_home(&root.path().join("home"));
        let service = Service::new(common::config(root.path())).await.unwrap();
        seed_retained_environments(&service, kib, 100).await;
        let mut elapsed = Vec::new();
        for _ in 0..6 {
            let started = Instant::now();
            let inventory = nodes::typed_inventory(&service).await.unwrap();
            elapsed.push(started.elapsed().as_micros());
            let node = serde_json::to_value(&inventory[0]).unwrap();
            assert_eq!(node["diskAccounting"]["unconsolidatedBytes"], 0);
            assert_eq!(node["staleDisks"]["count"], 0);
        }
        println!(
            "{}",
            json!({
                "benchmark": "retained_native_inventory", "retainedDisks": 100,
                "snapshotKiB": kib, "checkpointKiB": kib, "elapsedMicros": elapsed,
            })
        );
        service.shutdown.cancel();
    }
}
