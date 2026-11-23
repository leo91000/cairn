use super::*;
use crate::run_fixture::DbRunFixture;
use serde_json::json;

#[tokio::test]
async fn native_cleanup_cause_survives_checkpoint_roundtrip_without_claiming_physical_release() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(root.path()).unwrap();
    let message = "Native disk is held by valkey pid 42; ownership retained.";
    let saved = store.transaction(move |db| {
        db.add_run_fixture(&json!({"id":"cleanup-run", "taskId":"cleanup-run", "status":"running"}), None)?;
        let mut saved = RunCheckpoint::from_value(json!({
            "runnerId":"cleanup-attempt", "prepared":{"backend":"native", "isolated":true}, "remainingMs":null,
        }))?;
        saved.record_controller_result(db, "cleanup-run", "cleanup-attempt", Some(crate::runner::ControllerResult {
            code:70, finalization_failed:true, physical_finalized:false, terminal_reason:None,
            native_cleanup:Some(message.into()),
        }))?;
        saved.record_controller_result(db, "cleanup-run", "cleanup-attempt", Some(crate::runner::ControllerResult {
            code:70, finalization_failed:true, physical_finalized:false, terminal_reason:None,
            native_cleanup:Some("Updated holder diagnostic".into()),
        }))?;
        saved.store_in(db, "cleanup-run")?;
        assert_eq!(db.events_fixture("cleanup-run", 0, 100)?.len(), 1);
        RunCheckpoint::load_in(db, "cleanup-run")
    }).await.unwrap().unwrap();
    assert_eq!(
        saved.finalization_error(),
        Some("Updated holder diagnostic")
    );
    assert_eq!(saved.terminal_code(), Some(70));
    assert_eq!(
        saved.finalization_failure_attempt.as_deref(),
        Some("cleanup-attempt")
    );
    assert!(saved.progress.is_none());
}

#[tokio::test]
async fn accepted_cancellation_blocks_publication_of_deferred_cleanup() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(root.path()).unwrap();
    store
        .transaction(|db| {
            db.add_run_fixture(
                &json!({"id":"cancelled-cleanup", "taskId":"task", "status":"running"}),
                None,
            )?;
            let mut saved = RunCheckpoint::from_value(json!({
                "runnerId":"attempt", "prepared":{"backend":"native", "isolated":true},
            }))?;
            let pending = crate::runner::ControllerResult {
                code: crate::runner::CONTROLLER_FAILED,
                finalization_failed: true,
                physical_finalized: false,
                terminal_reason: None,
                native_cleanup: Some("Pending cleanup.".into()),
            };
            saved.record_controller_observation(
                db,
                "cancelled-cleanup",
                "attempt",
                Some(pending.clone()),
            )?;
            db.change_run(
                "cancelled-cleanup",
                crate::runs::RunChanges {
                    cancel_requested_at: Some(Some(now())),
                    ..Default::default()
                },
            )?;
            saved.publish_cleanup_observation(
                db,
                "cancelled-cleanup",
                "attempt",
                Some(crate::runner::ControllerResult {
                    physical_finalized: true,
                    finalization_failed: false,
                    native_cleanup: Some("Cleanup completed.".into()),
                    ..pending
                }),
            )?;
            assert_eq!(saved.finalization_error(), Some("Pending cleanup."));
            assert!(saved.cleanup_observation_pending());
            assert!(db.events_fixture("cancelled-cleanup", 0, 100)?.is_empty());
            Ok(())
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn unrelated_provider_error_does_not_replace_vm_flush_failure() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(root.path()).unwrap();
    let saved = store
        .transaction(|db| {
            db.add_run_fixture(
                &json!({"id":"vm-flush", "taskId":"vm-flush", "status":"running"}),
                None,
            )?;
            let mut saved = RunCheckpoint::from_value(json!({
                "runnerId":"attempt", "prepared":{"backend":"firecracker", "isolated":true},
                "remainingMs":null, "lastError":"Provider unavailable",
            }))?;
            saved.record_controller_result(
                db,
                "vm-flush",
                "attempt",
                Some(crate::runner::ControllerResult {
                    code: 70,
                    finalization_failed: true,
                    physical_finalized: false,
                    native_cleanup: None,
                    terminal_reason: None,
                }),
            )?;
            Ok(saved)
        })
        .await
        .unwrap();
    assert!(
        saved
            .finalization_error()
            .unwrap()
            .contains("filesystem could not be flushed")
    );
    assert_eq!(saved.last_error(), "Provider unavailable");
}

#[tokio::test]
async fn controller_reason_preserves_provider_finalization_completion_and_cancellation_policy() {
    use crate::runner::{ControllerResult, TerminalReason::NativeOutputLimit};

    let root = tempfile::tempdir().unwrap();
    let store = Store::open(root.path()).unwrap();
    let reported = ControllerResult {
        code: 70,
        physical_finalized: true,
        finalization_failed: false,
        native_cleanup: None,
        terminal_reason: Some(NativeOutputLimit),
    };
    for (name, prior, cancelled, result, expected_error, records_reason) in [
        (
            "provider",
            json!({ "providerFailed": true, "lastError": "Provider unavailable" }),
            false,
            reported.clone(),
            "Provider unavailable",
            true,
        ),
        (
            "earlier",
            json!({ "lastError": "Earlier failure" }),
            false,
            reported.clone(),
            "Earlier failure",
            true,
        ),
        (
            "completed",
            json!({ "completed": true }),
            false,
            reported.clone(),
            NativeOutputLimit.message(),
            true,
        ),
        (
            "cancelled",
            json!({}),
            true,
            reported.clone(),
            NativeOutputLimit.message(),
            true,
        ),
        (
            "flush",
            json!({ "completed": true }),
            false,
            ControllerResult {
                code: 70,
                physical_finalized: false,
                finalization_failed: true,
                ..reported.clone()
            },
            "",
            false,
        ),
        (
            "legacy",
            json!({}),
            false,
            ControllerResult {
                native_cleanup: None,
                terminal_reason: None,
                ..reported.clone()
            },
            "",
            false,
        ),
        (
            "success",
            json!({}),
            false,
            ControllerResult {
                code: 0,
                ..reported.clone()
            },
            "",
            false,
        ),
        (
            "unfinalized",
            json!({}),
            false,
            ControllerResult {
                physical_finalized: false,
                ..reported.clone()
            },
            "",
            false,
        ),
        (
            "vm",
            json!({ "prepared": { "backend": "firecracker", "isolated": true } }),
            false,
            reported.clone(),
            "",
            false,
        ),
    ] {
        let mut value = json!({
            "prepared": { "backend": "native", "isolated": true },
            "runnerId": "attempt",
            "remainingMs": null,
        });
        crate::store::merge(&mut value, &prior);
        let mut saved = RunCheckpoint::from_value(value).unwrap();
        let provider_failed = saved.provider_failed();
        let completed = saved.completed();
        if provider_failed {
            saved.retry_cause = Some(Some(
                crate::run_retry::Cause::classify(&json!({
                    "code": "server_error",
                    "message": "Provider unavailable",
                }))
                .unwrap(),
            ));
            saved.provider_failure_attempt = Some("attempt".into());
        }
        let retry_cause = saved.retry_cause.clone();
        let stored = store
            .transaction(move |db| {
                db.add_run_fixture(
                    &json!({
                        "id": name,
                        "taskId": name,
                        "status": "running",
                        "cancelRequestedAt": cancelled.then_some(1),
                    }),
                    None,
                )?;
                saved.record_controller_result(db, name, "attempt", Some(result))?;
                saved.store_in(db, name)?;
                if cancelled || completed {
                    assert!(!super::super::schedule::finish_recovery(
                        db,
                        name,
                        Some(&saved)
                    )?);
                }
                Ok(saved)
            })
            .await
            .unwrap();
        let loaded = RunCheckpoint::load(&store, name).await.unwrap().unwrap();
        assert_eq!(loaded.last_error(), expected_error, "{name}");
        assert_eq!(loaded.provider_failed(), provider_failed, "{name}");
        assert_eq!(loaded.completed(), completed, "{name}");
        assert_eq!(loaded.retry_cause, retry_cause, "{name}");
        assert_eq!(
            loaded.controller_failure.is_some(),
            records_reason,
            "{name}"
        );
        assert_eq!(stored.terminal_code(), loaded.terminal_code());
        if cancelled {
            assert_eq!(store.run(name).await.unwrap()["status"], "cancelled");
        }
        if completed {
            assert_eq!(store.run(name).await.unwrap()["status"], "failed");
            let expected = if name == "flush" {
                "The filesystem could not be flushed. Working files were preserved; automatic replay was stopped."
            } else {
                expected_error
            };
            assert_eq!(loaded.finalization_error(), Some(expected));
            assert!(loaded.output_error().is_none());
        }
    }
}

#[tokio::test]
async fn controller_reason_rejects_a_different_attempt_without_committing_evidence() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(root.path()).unwrap();
    let result = store
        .transaction(|db| {
            let mut saved = RunCheckpoint::from_value(json!({ "runnerId": "current" }))?;
            saved.record_controller_result(
                db,
                "unused",
                "stale",
                Some(crate::runner::ControllerResult {
                    code: 70,
                    physical_finalized: true,
                    finalization_failed: false,
                    native_cleanup: None,
                    terminal_reason: Some(crate::runner::TerminalReason::NativeOutputLimit),
                }),
            )
        })
        .await;
    assert!(result.unwrap_err().is_conflict());
}

#[tokio::test]
async fn native_output_limit_survives_guard_authenticated_wait_and_checkpoint_reopen() {
    use crate::runner::{CONTROLLER_FAILED, TerminalReason::NativeOutputLimit};

    let root = tempfile::tempdir().unwrap();
    let store = Store::open(root.path()).unwrap();
    let run_id = "native-output-limit";
    let attempt = crate::config::id();
    let result = crate::native::output_limit_completion(&attempt).await;

    assert_eq!(result.code, CONTROLLER_FAILED);
    assert!(result.physical_finalized);
    assert!(!result.finalization_failed);
    assert_eq!(result.terminal_reason, Some(NativeOutputLimit));

    let saved = RunCheckpoint::from_value(json!({
        "prepared": { "backend": "native", "isolated": true },
        "runnerId": attempt,
        "remainingMs": null,
    }))
    .unwrap();
    store
        .transaction(move |db| {
            db.add_run_fixture(
                &json!({ "id": run_id, "taskId": "task", "status": "running" }),
                None,
            )?;
            Ok(())
        })
        .await
        .unwrap();
    saved.store(&store, run_id).await.unwrap();

    let checkpoint = Checkpoint::new(store.clone(), run_id.into(), saved);
    let decision_attempt = attempt.clone();
    checkpoint
        .decide(move |saved, db| {
            saved.record_controller_result(db, run_id, &decision_attempt, Some(result.clone()))?;
            saved.record_controller_result(db, run_id, &decision_attempt, Some(result))?;
            Ok(())
        })
        .await
        .unwrap();

    let reopened = RunCheckpoint::load(&store, run_id).await.unwrap().unwrap();
    assert_eq!(reopened.prepared().unwrap()["backend"], "native");
    assert_eq!(reopened.terminal_code(), Some(CONTROLLER_FAILED));
    assert_eq!(
        reopened.last_error(),
        "Native output exceeded its bounded log budget."
    );
    let failure = reopened.controller_failure.as_ref().unwrap();
    assert_eq!(failure.attempt, attempt);
    assert_eq!(failure.reason, NativeOutputLimit);

    let events = store
        .read(move |db| db.events_fixture(run_id, 0, 100))
        .await
        .unwrap();
    let failures = events
        .iter()
        .filter(|event| event["payload"]["error"]["code"] == "native_output_limit")
        .collect::<Vec<_>>();
    assert_eq!(failures.len(), 1, "{events:#?}");
    assert_eq!(failures[0]["type"], "runner.failed");
    assert_eq!(failures[0]["payload"]["attemptId"], attempt);
    assert_eq!(
        failures[0]["payload"]["error"]["message"],
        "Native output exceeded its bounded log budget."
    );

    let (timeline, more) = store
        .read(move |db| crate::timeline::page(db, run_id, i64::MAX, 100))
        .await
        .unwrap();
    assert!(!more);
    let failure_notice = timeline
        .iter()
        .find_map(|entry| match &entry.content {
            cairn_contracts::TimelineContent::Notice { notice }
                if notice.code == cairn_contracts::NoticeCode::Failure =>
            {
                Some(notice)
            }
            _ => None,
        })
        .expect("A native controller failure should become a failure notice.");
    assert_eq!(
        failure_notice.origin,
        Some(cairn_contracts::FailureOrigin::Runner)
    );
}

#[test]
fn healthy_held_turn_is_not_a_missing_physical_finalization() {
    for backend in ["native", "firecracker"] {
        let mut saved = RunCheckpoint::fresh(None);
        saved.prepared = Some(Some(json!({"backend":backend,"isolated":true})));
        saved.runner_id = Some(Some("attempt".into()));
        saved.completed = Some(true);
        assert!(saved.finalization_error().is_some());
        saved.held_environment = Some(HeldEnvironment {
            attempt: "attempt".into(),
            generation: "generation".into(),
            expires_at: now() + 60_000,
        });
        assert!(saved.finalization_error().is_none());
        saved.finalization_failure_attempt = Some("attempt".into());
        assert!(saved.finalization_error().is_some());
        saved.finalization_failure_attempt = None;
        saved.terminal_code = Some(None);
        assert!(saved.finalization_error().is_some());
        saved.terminal_code = Some(Some(70));
        assert!(saved.finalization_error().is_some());
        saved.terminal_code = None;
        saved.runner_id = Some(Some("replacement".into()));
        assert!(saved.finalization_error().is_some());
    }
}
