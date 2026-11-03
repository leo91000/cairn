use super::*;
use crate::run_fixture::DbRunFixture;
use serde_json::json;

#[tokio::test]
async fn controller_reason_preserves_provider_finalization_completion_and_cancellation_policy() {
    use crate::runner::{ControllerResult, TerminalReason::NativeOutputLimit};

    let root = tempfile::tempdir().unwrap();
    let store = Store::open(root.path()).unwrap();
    let reported = ControllerResult {
        code: 70,
        physical_finalized: true,
        finalization_failed: false,
        terminal_reason: Some(NativeOutputLimit),
    };
    for (name, prior, cancelled, result, expected_error, records_reason) in [
        (
            "provider",
            json!({ "providerFailed": true, "lastError": "Provider unavailable" }),
            false,
            reported,
            "Provider unavailable",
            true,
        ),
        (
            "earlier",
            json!({ "lastError": "Earlier failure" }),
            false,
            reported,
            "Earlier failure",
            true,
        ),
        (
            "completed",
            json!({ "completed": true }),
            false,
            reported,
            NativeOutputLimit.message(),
            true,
        ),
        (
            "cancelled",
            json!({}),
            true,
            reported,
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
                ..reported
            },
            "",
            false,
        ),
        (
            "legacy",
            json!({}),
            false,
            ControllerResult {
                terminal_reason: None,
                ..reported
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
                ..reported
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
                ..reported
            },
            "",
            false,
        ),
        (
            "vm",
            json!({ "prepared": { "backend": "firecracker", "isolated": true } }),
            false,
            reported,
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
            saved.record_controller_result(db, run_id, &decision_attempt, Some(result))?;
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
