//! Credential-free benchmark of the node inventory used by HTTP, MCP and agents.
//! Run through scripts/test-backend.mjs with --ignored --nocapture --test-threads=1.
mod common;

use cairn_installation::{config::now, nodes, service::Service};
use common::{node_fixture::DbNodeFixture, run_fixture::DbRunFixture};
use serde_json::json;
use std::time::Instant;

const NODE: &str = "00000000-0000-4000-8000-000000000004";

#[tokio::test]
async fn inventory_preserves_active_and_moving_disk_fences() {
    let root = tempfile::tempdir().unwrap();
    common::managed_codex_home(&root.path().join("home"));
    let service = Service::new(common::config(root.path())).await.unwrap();
    service
        .store
        .transaction(|db| {
            db.put_node_fixture(&json!({ "id": NODE }))?;
            for name in [
                "moved",
                "older",
                "moving",
                "reserved",
                "executing",
                "unmaterialized",
                "unknown",
            ] {
                db.add_run_fixture(
                    &json!({
                        "id": name, "taskId": name, "status": "succeeded",
                        "moveRequest": if name == "moving" { json!({}) } else { json!(null) },
                        "moveReservation": if name == "reserved" { json!("lease") } else { json!(null) },
                        "snapshot": { "agent": { "instructions": "private frozen instructions" } },
                    }),
                    None,
                )?;
                let checkpoint = match name {
                    "older" => json!(NODE),
                    "unknown" => json!(null),
                    _ => json!(nodes::LOCAL_NODE_ID),
                };
                db.save_checkpoint_boundary(name, &json!({ "nodeId": checkpoint }))?;
            }
            for name in [
                "moved",
                "older",
                "moving",
                "reserved",
                "executing",
                "unmaterialized",
                "unknown",
                "missing",
            ] {
                db.put(
                    "node-volumes",
                    &json!({
                        "id": format!("volume-{name}"), "runId": name, "nodeId": NODE,
                        "materialized": name != "unmaterialized",
                        "diskMiB": if name == "older" { 300 } else { 100 }, "activeDiskMiB": 100,
                    }),
                )?;
            }
            // An execution on another node must not hide the stale source disk.
            for (run, node) in [("executing", NODE), ("moved", nodes::LOCAL_NODE_ID)] {
                db.put(
                    "node-attempts",
                    &json!({
                        "id": format!("attempt-{run}"), "runId": run,
                        "nodeId": node, "released": false,
                    }),
                )?;
            }
            Ok(())
        })
        .await
        .unwrap();
    let inventory = nodes::typed_inventory(&service).await.unwrap();
    let node = inventory
        .iter()
        .find(|node| node.id.to_string() == NODE)
        .unwrap();
    assert_eq!(node.stale_disks.count, 3);
    assert_eq!(node.stale_disks.disk_mi_b, 400);
    assert_eq!(node.occupied_slots, 1);
    service.shutdown.cancel();
}

#[tokio::test]
async fn storage_accounting_preserves_unsigned_saturation_and_scope() {
    let root = tempfile::tempdir().unwrap();
    common::managed_codex_home(&root.path().join("home"));
    let service = Service::new(common::config(root.path())).await.unwrap();
    service.store.transaction(|db| {
        db.put_node_fixture(&json!({
            "id": NODE, "capabilities": { "backend": "native" }, "diskGrowthProtocol": 1,
            "diskCapacity": { "protocol": 1, "capturedAt": now(), "usedBytes": 0,
                "freeBytes": 1000000, "ownedBytes": {}, "retainedBytes": {}, "requests": [] },
        }))?;
        for (index, dirty) in [json!(1), json!(5), json!(-1), json!(1.5), json!("20"), json!(null)].into_iter().enumerate() {
            db.add_run_fixture(&json!({
                "id": format!("run-{index}"), "taskId": format!("task-{index}"), "status": "succeeded",
                "nodeId": NODE, "storage": { "mode": "on-demand", "dirtyBytes": dirty },
            }), None)?;
        }
        for (index, node, mode) in [(0, nodes::LOCAL_NODE_ID, "on-demand"), (1, NODE, "other")] {
            db.add_run_fixture(&json!({
                "id": format!("excluded-{index}"), "taskId": format!("excluded-{index}"), "status": "succeeded",
                "nodeId": node, "storage": { "mode": mode, "dirtyBytes": 999 },
            }), None)?;
        }
        Ok(())
    }).await.unwrap();
    let inventory = nodes::typed_inventory(&service).await.unwrap();
    assert_eq!(
        serde_json::to_value(&inventory[0]).unwrap()["diskAccounting"]["unsynchronizedBytes"],
        6
    );
    service
        .store
        .update_run_boundary(
            "run-1",
            json!({ "storage": { "mode": "on-demand", "dirtyBytes": u64::MAX } }),
        )
        .await
        .unwrap();
    let inventory = nodes::typed_inventory(&service).await.unwrap();
    assert_eq!(
        serde_json::to_value(&inventory[0]).unwrap()["diskAccounting"]["unsynchronizedBytes"],
        u64::MAX
    );
    service.shutdown.cancel();
}

#[tokio::test]
#[ignore = "explicit node inventory read benchmark"]
async fn inventory_by_saved_execution_size() {
    for kib in [0, 128, 512] {
        let root = tempfile::tempdir().unwrap();
        common::managed_codex_home(&root.path().join("home"));
        let service = Service::new(common::config(root.path())).await.unwrap();
        service.store.transaction(move |db| {
            let instructions = "x".repeat(kib * 1024);
            for index in 0..300 {
                let run = format!("run-{index}");
                db.add_run_fixture(&json!({
                    "id": run, "taskId": run, "status": "succeeded",
                    "nodeId": NODE, "storage": { "mode": "on-demand", "dirtyBytes": index + 1 },
                    "snapshot": { "agent": { "instructions": instructions } },
                }), None)?;
                db.save_checkpoint_boundary(&run, &json!({
                    "nodeId": if index % 5 == 0 { nodes::LOCAL_NODE_ID } else { NODE },
                    "prepared": { "instructions": instructions },
                }))?;
                db.put("node-volumes", &json!({
                    "id": format!("volume-{index}"), "runId": run, "nodeId": NODE,
                    "materialized": true, "diskMiB": 1024, "activeDiskMiB": 1024,
                }))?;
            }
            db.put_node_fixture(&json!({
                "id": NODE, "name": "Benchmark node", "capabilities": { "backend": "native" },
                "diskGrowthProtocol": 1, "diskCapacity": {
                    "protocol": 1, "capturedAt": now(), "usedBytes": 0,
                    "freeBytes": 1_000_000_000, "ownedBytes": {}, "retainedBytes": {}, "requests": [],
                },
            }))?;
            Ok(())
        }).await.unwrap();
        let mut samples = Vec::new();
        let mut response_bytes = 0;
        for _ in 0..6 {
            let started = Instant::now();
            let inventory = nodes::typed_inventory(&service).await.unwrap();
            samples.push(started.elapsed().as_micros());
            let node = inventory
                .iter()
                .find(|node| node.id.to_string() == NODE)
                .unwrap();
            assert_eq!(node.stale_disks.count, 60);
            assert_eq!(node.stale_disks.disk_mi_b, 60 * 1024);
            let wire = serde_json::to_value(node).unwrap();
            assert_eq!(wire["diskAccounting"]["unsynchronizedBytes"], 45150);
            response_bytes = serde_json::to_vec(&inventory).unwrap().len();
        }
        println!(
            "{}",
            json!({
                "benchmark": "node_inventory", "runs": 300, "snapshotKiB": kib,
                "checkpointKiB": kib, "elapsedMicros": samples, "responseBytes": response_bytes,
            })
        );
        service.shutdown.cancel();
    }
}
