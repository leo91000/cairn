//! Lifecycle writes keep admission constraints and partial failures atomic.
use cairn_contracts::{RunState, RunStatus};
use cairn_installation::{runs::RunChanges, store::Store};
use serde_json::json;

fn state(id: &str, task: &str) -> RunState {
    serde_json::from_value(json!({
        "id": id,
        "taskId": task,
        "status": "queued",
        "createdAt": 1,
    }))
    .unwrap()
}

#[tokio::test]
async fn admission_uniqueness_and_dedupe_survive_terminal_transitions() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(root.path()).unwrap();
    store
        .transaction(|db| {
            db.admit_run(&state("first", "mission"), None, &json!({}), Some("one"))?;
            assert!(
                db.admit_run(&state("other", "mission"), None, &json!({}), None)
                    .is_err()
            );
            db.change_run(
                "first",
                RunChanges {
                    status: Some(RunStatus::Succeeded),
                    finished_at: Some(Some(2)),
                    ..Default::default()
                },
            )?;
            assert!(
                db.admit_run(
                    &state("duplicate", "another"),
                    None,
                    &json!({}),
                    Some("one")
                )
                .is_err()
            );
            db.admit_run(&state("next", "mission"), None, &json!({}), Some("two"))?;
            assert!(
                db.change_run(
                    "first",
                    RunChanges {
                        status: Some(RunStatus::Queued),
                        ..Default::default()
                    }
                )
                .is_err()
            );
            assert_eq!(db.run_state("first")?.unwrap().status, RunStatus::Succeeded);
            Ok(())
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn a_caught_transition_error_cannot_commit_partial_writes() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(root.path()).unwrap();
    store
        .transaction(|db| {
            db.admit_run(
                &state("run", "mission"),
                None,
                &json!({"nodeId": "before"}),
                None,
            )?;
            let error = db
                .change_run(
                    "run",
                    RunChanges {
                        status: Some(RunStatus::Cancelled),
                        cancel_requested_at: Some(Some(2)),
                        boundary: Some(json!({"nodeId": "after", "status": "running"})),
                        ..Default::default()
                    },
                )
                .unwrap_err();
            assert!(error.message.contains("typed run transitions"));
            // Deliberately commit the enclosing transaction after handling the error.
            Ok(())
        })
        .await
        .unwrap();
    let run = store.run("run").await.unwrap();
    assert_eq!(run["status"], "queued");
    assert!(run["cancelRequestedAt"].is_null());
    assert_eq!(run["nodeId"], "before");
}

#[tokio::test]
async fn independent_admissions_serialize_on_the_active_mission_constraint() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(root.path()).unwrap();
    let left =
        store.transaction(|db| db.admit_run(&state("left", "mission"), None, &json!({}), None));
    let right =
        store.transaction(|db| db.admit_run(&state("right", "mission"), None, &json!({}), None));
    let (left, right) = tokio::join!(left, right);
    assert_ne!(left.is_ok(), right.is_ok());
    assert_eq!(
        store
            .read(|db| Ok(db.active_run_states()?.len()))
            .await
            .unwrap(),
        1
    );
}
