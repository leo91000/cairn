use crate::{attempt_fixture::DbAttemptFixture, run_fixture::DbRunFixture, store::Store};
use serde_json::json;

#[tokio::test]
async fn storage_projection_keeps_decision_fields_without_frozen_execution_data() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(root.path()).unwrap();
    let statuses = [
        "queued",
        "running",
        "waiting",
        "succeeded",
        "failed",
        "cancelled",
        "interrupted",
    ];
    store
        .transaction(move |db| {
            for (index, status) in statuses.into_iter().enumerate() {
                let id = format!("run-{index}");
                db.add_run_fixture(
                    &json!({
                        "id": id, "taskId": id, "status": status,
                        "finishedAt": if index < 3 { None } else { Some(123) },
                        "recoveryPending": index % 2 == 0,
                        "summary": "private display summary",
                        "snapshot": { "agent": { "instructions": "private frozen instructions".repeat(4096) } },
                        "nodeId": "native-node", "nodeState": "saving",
                        "storage": { "mode": "on-demand", "dirtyBytes": u64::MAX },
                        "backup": { "status": "ready", "requestedRevision": 1, "acknowledgedRevision": 1 },
                        "moveRequest": {
                            "reservation": format!("reservation-{index}"),
                            "nodeId": crate::nodes::LOCAL_NODE_ID,
                            "resources": { "cpu": 1, "memoryMiB": 128, "diskMiB": 128 },
                        },
                    }),
                    None,
                )?;
                db.put_attempt_fixture(&json!({
                    "id": format!("reservation-{index}"), "runId": id,
                    "role": "destination", "nodeId": crate::nodes::LOCAL_NODE_ID,
                    "resources": { "cpu": 1, "memoryMiB": 128, "diskMiB": 128 },
                }))?;
                db.change_run_placement(&id, crate::nodes::run_placement::PlacementChanges {
                    move_reservation: Some(Some(format!("reservation-{index}"))),
                    ..Default::default()
                })?;
                let full = db.run(&id)?.unwrap();
                let compact = db.node_storage_run(&id)?.unwrap();
                let fields = [
                    "id", "status", "finishedAt", "recoveryPending", "nodeId", "nodeState",
                    "storage", "backup", "moveRequest", "moveReservation",
                ];
                for field in fields {
                    assert_eq!(compact[field], full[field], "{status}: {field}");
                }
                assert!(compact.get("snapshot").is_none());
                assert!(compact.get("summary").is_none());
            }
            assert!(db.node_storage_run("missing")?.is_none());
            Ok(())
        })
        .await
        .unwrap();
}
