//! Lifecycle writes keep admission constraints and partial failures atomic.
use cairn_contracts::{RunState, RunStatus};
use cairn_installation::{nodes::run_placement::PlacementChanges, runs::RunChanges, store::Store};
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
async fn unchanged_lifecycle_assignments_do_not_wake_subscribers() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(root.path()).unwrap();
    let expected = state("run", "mission");
    store
        .transaction(|db| {
            db.admit_run(&state("run", "mission"), None, &json!({}), None)?;
            Ok(())
        })
        .await
        .unwrap();
    let changes = store.subscribe();
    let revision = store.revision();
    for change in [
        RunChanges::default(),
        RunChanges {
            summary: Some(String::new()),
            ..Default::default()
        },
        RunChanges {
            status: Some(RunStatus::Queued),
            ..Default::default()
        },
        RunChanges {
            resume_count: Some(0),
            ..Default::default()
        },
        RunChanges {
            placement: Some(Box::new(PlacementChanges::default())),
            ..Default::default()
        },
        RunChanges {
            placement: Some(Box::new(PlacementChanges {
                node_state: Some(None),
                required_tags: Some(None),
                ..Default::default()
            })),
            ..Default::default()
        },
    ] {
        assert_eq!(store.change_run("run", change).await.unwrap(), expected);
        assert_eq!(store.revision(), revision);
        assert!(!changes.has_changed().unwrap());
    }
}

#[tokio::test]
async fn unchanged_boundary_patches_preserve_null_presence_without_notifications() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(root.path()).unwrap();
    store
        .transaction(|db| {
            db.admit_run(
                &state("run", "mission"),
                None,
                &json!({ "storage": { "mode": "on-demand", "dirtyBytes": 0 } }),
                None,
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let mut changes = store.subscribe();
    let revision = store.revision();
    for patch in [
        json!({}),
        json!({ "storage": { "mode": "on-demand", "dirtyBytes": 0 } }),
    ] {
        let result = store.update_run_boundary("run", patch).await.unwrap();
        assert_eq!(result["storage"]["dirtyBytes"], 0);
        assert_eq!(store.revision(), revision);
        assert!(!changes.has_changed().unwrap());
    }

    let result = store
        .update_run_boundary("run", json!({ "backup": null }))
        .await
        .unwrap();
    assert!(result.as_object().unwrap().contains_key("backup"));
    assert!(changes.has_changed().unwrap());
    changes.borrow_and_update();
    let revision = store.revision();
    let repeated = store
        .update_run_boundary("run", json!({ "backup": null }))
        .await
        .unwrap();
    assert_eq!(repeated, result);
    assert_eq!(store.revision(), revision);
    assert!(!changes.has_changed().unwrap());

    // Boundary patches replace whole root fields, including nested objects.
    let replaced = store
        .update_run_boundary("run", json!({ "storage": { "dirtyBytes": 0 } }))
        .await
        .unwrap();
    assert!(replaced["storage"].get("mode").is_none());
    assert_eq!(replaced["storage"]["dirtyBytes"], 0);
    assert!(changes.has_changed().unwrap());
}

#[tokio::test]
async fn domain_changes_still_commit_when_lifecycle_fields_are_unchanged() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(root.path()).unwrap();
    store
        .transaction(|db| {
            db.admit_run(&state("run", "mission"), None, &json!({}), None)?;
            Ok(())
        })
        .await
        .unwrap();
    let mut changes = store.subscribe();
    let wakeup = serde_json::from_value(json!({
        "id": "wait", "messageId": null, "reason": "Synthetic wait",
        "seconds": 60, "createdAt": 0, "wakeAt": 60000,
    }))
    .unwrap();
    store
        .change_run(
            "run",
            RunChanges {
                wakeup: Some(Some(wakeup)),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert!(
        store
            .read(|db| db.wakeup(false, "run"))
            .await
            .unwrap()
            .is_some()
    );
    assert!(changes.has_changed().unwrap());
    changes.borrow_and_update();

    store
        .change_run(
            "run",
            RunChanges {
                wakeup: Some(None),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert!(
        store
            .read(|db| db.wakeup(false, "run"))
            .await
            .unwrap()
            .is_none()
    );
    assert!(changes.has_changed().unwrap());
    changes.borrow_and_update();

    let lifecycle = store
        .change_run(
            "run",
            RunChanges {
                boundary: Some(json!({ "storage": { "mode": "on-demand", "dirtyBytes": 0 } })),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(lifecycle, state("run", "mission"));
    assert_eq!(store.run("run").await.unwrap()["storage"]["dirtyBytes"], 0);
    assert!(changes.has_changed().unwrap());

    changes.borrow_and_update();
    let placement = PlacementChanges {
        node_state: Some(Some("saving".into())),
        ..Default::default()
    };
    let placed = store
        .update_run_placement("run", placement.clone())
        .await
        .unwrap();
    assert_eq!(placed["nodeState"], "saving");
    assert!(changes.has_changed().unwrap());
    changes.borrow_and_update();
    let revision = store.revision();

    store
        .change_run(
            "run",
            RunChanges {
                placement: Some(Box::new(placement.clone())),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    store.update_run_placement("run", placement).await.unwrap();
    assert_eq!(store.revision(), revision);
    assert!(!changes.has_changed().unwrap());
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
                        placement: Some(Box::new(
                            cairn_installation::nodes::run_placement::PlacementChanges {
                                node_id: Some(
                                    serde_json::from_value(serde_json::json!("after")).unwrap(),
                                ),
                                ..Default::default()
                            },
                        )),
                        boundary: Some(serde_json::json!({"status": "running"})),
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

#[tokio::test]
async fn summaries_keep_frozen_names_and_accept_runs_without_a_snapshot() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(root.path()).unwrap();
    store
        .transaction(|db| {
            db.admit_run(&state("without", "one"), None, &json!({}), None)?;
            let mut snapshot: cairn_contracts::RunSnapshot = serde_json::from_value(json!({
                "task": {
                    "id": "00000000-0000-4000-8000-000000000003",
                    "agentId": "00000000-0000-4000-8000-000000000001",
                    "name": "Frozen mission",
                    "prompt": "Inspect",
                },
                "agent": {
                    "id": "00000000-0000-4000-8000-000000000001",
                    "name": "Frozen agent",
                    "revision": "fixture",
                    "createdAt": 0,
                    "access": cairn_contracts::AccessPolicy::default(),
                    "exclusiveNodes": [],
                    "plugins": [],
                },
                "project": null,
            }))?;
            db.admit_run(&state("with", "two"), Some(&snapshot), &json!({}), None)?;
            let summaries = db.runs(None, None, 20, 0, false)?;
            for summary in &summaries {
                assert!(summary.get("snapshot").is_none());
                serde_json::from_value::<cairn_contracts::Run>(summary.clone()).unwrap();
            }
            let summary = summaries.iter().find(|run| run["id"] == "with").unwrap();
            assert_eq!(summary["taskName"], "Frozen mission");
            assert_eq!(summary["agentName"], "Frozen agent");
            snapshot.agent.settings.name = "Replacement snapshot".into();
            db.change_run(
                "with",
                RunChanges {
                    snapshot: Some(snapshot),
                    ..Default::default()
                },
            )?;
            let summaries = db.runs(None, Some("two"), 20, 0, false)?;
            assert_eq!(summaries[0]["agentName"], "Replacement snapshot");
            Ok(())
        })
        .await
        .unwrap();
}
