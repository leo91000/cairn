use super::*;

#[tokio::test]
async fn delete_cleanup_evidence_survives_live_checkpoint_persistence() {
    use axum::{Json, Router, routing::any};
    let attempt = crate::config::id();
    let response_attempt = attempt.clone();
    let app = Router::new().fallback(any(move || {
        let attempt = response_attempt.clone();
        async move {
            Json(json!({
                "StatusCode": 70, "FinalizationFailed": true,
                "NativeCleanup": { "attemptId": attempt, "workloadFenced": true,
                    "message": "Disk held by valkey pid 42" }
            }))
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let root = tempfile::tempdir().unwrap();
    let service = Service::new(
        serde_json::from_value(json!({
            "dataDir": root.path().join("data"), "home": root.path().join("home"),
            "workspaceRoots": [root.path()], "publicUrl": "http://localhost:4310",
            "host": "127.0.0.1", "port": 0, "codexBin": "unused", "concurrency": 1,
            "runnerUrl": format!("http://{address}"), "workerEnabled": false, "logger": false,
        }))
        .unwrap(),
    )
    .await
    .unwrap();
    let id = crate::config::id();
    let fixture_id = id.clone();
    service
        .store
        .transaction(move |db| {
            db.add_run_fixture(
                &json!({ "id": fixture_id, "taskId": fixture_id,
            "status": "running", "isolated": true }),
                None,
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let saved = RunCheckpoint::from_value(json!({
        "runnerId": attempt, "prepared": {"backend":"native", "isolated":true},
        "remainingMs": null,
    }))
    .unwrap();
    saved.store(&service.store, &id).await.unwrap();
    let checkpoint = Checkpoint::new(service.store.clone(), id.clone(), saved);
    checkpoint
        .fence(&service, &service.store.run(&id).await.unwrap())
        .await
        .unwrap();
    checkpoint.persist().await.unwrap();
    release(&service, &checkpoint, None, true).await.unwrap();
    let state = service.store.run_state(&id).await.unwrap();
    assert_eq!(state.status, RunStatus::Failed);
    assert!(state.summary.contains("Disk held by valkey pid 42"));
    let persisted = RunCheckpoint::load(&service.store, &id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        persisted.finalization_error(),
        Some("Disk held by valkey pid 42")
    );
    assert_eq!(persisted.terminal_code(), Some(70));
    assert_eq!(
        checkpoint.snapshot().await.finalization_error(),
        persisted.finalization_error()
    );
    assert!(!persisted.has_controller_recovery_reservation());
    checkpoint
        .update(|saved| {
            saved.settled = Some(Some(Settled::failure(
                RunStatus::Failed,
                "Earlier transport failure",
            )));
        })
        .await
        .unwrap();
    release(&service, &checkpoint, None, true).await.unwrap();
    assert!(
        service
            .store
            .run_state(&id)
            .await
            .unwrap()
            .summary
            .contains("Disk held by valkey pid 42")
    );
    service
        .store
        .change_run(
            &id,
            crate::runs::RunChanges {
                cancel_requested_at: Some(Some(now())),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    release(&service, &checkpoint, None, true).await.unwrap();
    assert_eq!(
        service.store.run_state(&id).await.unwrap().status,
        RunStatus::Cancelled
    );
    service.shutdown.cancel();
    server.abort();
}

#[tokio::test]
async fn live_shutdown_defers_cleanup_observation_even_after_fencing() {
    let root = tempfile::tempdir().unwrap();
    let service = Service::new(
        serde_json::from_value(json!({
            "dataDir": root.path().join("data"), "home": root.path().join("home"),
            "workspaceRoots": [root.path()], "publicUrl": "http://localhost:4310",
            "host": "127.0.0.1", "port": 0, "codexBin": "unused", "concurrency": 1,
            "runnerUrl": "", "workerEnabled": false, "logger": false
        }))
        .unwrap(),
    )
    .await
    .unwrap();
    let id = crate::config::id();
    let owner = id.clone();
    let saved = service
        .store
        .transaction(move |db| {
            db.add_run_fixture(
                &json!({ "id": owner, "taskId": owner, "status": "running", "isolated": true }),
                None,
            )?;
            let mut saved = RunCheckpoint::from_value(json!({
                "runnerId": "old", "prepared": { "backend": "native", "isolated": true },
                "remainingMs": 0
            }))?;
            saved.record_controller_observation(
                db,
                &owner,
                "old",
                Some(crate::runner::ControllerResult {
                    code: crate::runner::CONTROLLER_FAILED,
                    finalization_failed: true,
                    physical_finalized: false,
                    terminal_reason: None,
                    native_cleanup: Some("Disk cleanup is pending.".into()),
                }),
            )?;
            saved.store_in(db, &owner)?;
            Ok(saved)
        })
        .await
        .unwrap();
    let checkpoint = Checkpoint::new(service.store.clone(), id.clone(), saved);
    service.shutdown.cancel();
    record_failure_fixture(
        &service,
        &checkpoint,
        &CancellationToken::new(),
        &Error::conflict("Worker is restarting"),
        &[],
    )
    .await
    .unwrap();
    release(&service, &checkpoint, None, true).await.unwrap();

    let state = service.store.run_state(&id).await.unwrap();
    assert_eq!(state.status, RunStatus::Queued);
    assert!(state.recovery_pending);
    let saved = RunCheckpoint::load(&service.store, &id)
        .await
        .unwrap()
        .unwrap();
    assert!(saved.cleanup_observation_pending());
    assert_eq!(saved.finalization_error(), Some("Disk cleanup is pending."));
    let events = service
        .store
        .read(move |db| db.events_fixture(&id, 0, 100))
        .await
        .unwrap();
    assert!(events.iter().all(|event| event["type"] != "error"));
}

#[tokio::test]
async fn a_superseded_preparation_error_retries_the_current_choice_unless_stopped() {
    let root = tempfile::tempdir().unwrap();
    let service = Service::new(
        serde_json::from_value(json!({
            "dataDir": root.path().join("data"), "home": root.path().join("home"),
            "workspaceRoots": [root.path()], "publicUrl": "http://localhost:4310",
            "host": "127.0.0.1", "port": 0, "codexBin": "unused", "concurrency": 1, "runnerUrl": "", "workerEnabled": false, "logger": false,
        }))
        .unwrap(),
    )
    .await
    .unwrap();
    for stopped in [false, true] {
        let id = crate::config::id();
        let owner = id.clone();
        service.store.transaction(move |db| {
            db.add_run_fixture(&json!({ "id": owner, "taskId": owner, "status": "running", "createdAt": now(), "environmentState": "expired", "environmentAdmissionRevision": "new-selection" }), None)?;
            Ok(())
        }).await.unwrap();
        let checkpoint = Checkpoint::new(
            service.store.clone(),
            id.clone(),
            RunCheckpoint::fresh(Some(60_000)),
        );
        checkpoint.persist().await.unwrap();
        let cancel = CancellationToken::new();
        if stopped {
            cancel.cancel();
        }
        record_failure_for_revision(
            &service,
            &checkpoint,
            &cancel,
            &Error::forbidden("The calling execution ended or changed."),
            &[],
            Some(json!("old-selection")),
        )
        .await
        .unwrap();
        let run = service.store.run(&id).await.unwrap();
        assert_eq!(run["status"], if stopped { "cancelled" } else { "queued" });
        assert_eq!(run["environmentAdmissionRevision"], "new-selection");
        assert_eq!(run["environmentState"], "expired");
        assert!(checkpoint.snapshot().await.remaining_ms.flatten().unwrap() > 0);
    }
    service.shutdown.cancel();
}

#[tokio::test]
async fn provider_failure_with_lost_terminal_result_never_replays() {
    let root = tempfile::tempdir().unwrap();
    let service = Service::new(crate::config::Config {
        data_dir: root.path().join("data"),
        home: root.path().join("home"),
        workspace_roots: vec![root.path().into()],
        public_url: "http://localhost:4310".into(),
        host: "127.0.0.1".into(),
        port: 0,
        codex_bin: "unused".into(),
        claude_bin: "unused".into(),
        opencode_bin: "unused".into(),
        opencode_go_usage_url: crate::accounts::opencode::USAGE_URL.into(),
        antigravity_bin: "agy".into(),
        concurrency: 1,
        logger: false,
        worker_enabled: false,
        runner_url: String::new(),
    })
    .await
    .unwrap();
    for code in ["server_error", "authentication_error"] {
        let id = crate::config::id();
        let record = json!({
            "id": id,
            "taskId": id,
            "status": "running",
            "sessionId": "session",
            "snapshot": {"agent": {"id": crate::config::MAIN_AGENT_ID, "provider": "codex"}},
        });
        service
            .store
            .transaction(move |db| db.add_run_fixture(&record, None).map(|_| ()))
            .await
            .unwrap();
        let saved = RunCheckpoint::from_value(json!({
            "prepared": {"backend": "native", "isolated": true},
            "runnerId": "same-attempt",
            "remainingMs": null,
            "terminalCode": null,
        }))
        .unwrap();
        saved.store(&service.store, &id).await.unwrap();
        let checkpoint = Checkpoint::new(service.store.clone(), id.clone(), saved);
        let mut activity =
            crate::performance::Activity::new(&id, "fixture", "host", Provider::Codex);
        let mut total = 0;
        let event = json!({
            "type": "turn.failed",
            "error": {"code": code, "message": "Confirmed provider failure"},
        });
        super::super::output::record(
            &service,
            &super::super::output::Output {
                kind: super::super::output::OutputKind::Stdout,
                raw: event.to_string(),
                received: tokio::time::Instant::now(),
            },
            &checkpoint,
            false,
            &[],
            &mut total,
            &mut activity,
        )
        .await
        .unwrap();
        record_failure(
            &service,
            &checkpoint,
            &CancellationToken::new(),
            &Error::unavailable("Controller interrupted execution"),
            &[],
        )
        .await
        .unwrap();
        let saved = checkpoint.snapshot().await;
        assert_eq!(
            saved.controller_recoveries(),
            0,
            "a confirmed provider outcome does not spend controller recoveries"
        );
        let state = service.store.run_state(&id).await.unwrap();
        assert!(state.retry.is_none());
        assert_eq!(saved.settled().unwrap().status, RunStatus::Failed);
        assert_eq!(
            saved.settled().unwrap().summary.as_deref(),
            Some("Confirmed provider failure")
        );
    }
    service.shutdown.cancel();
}

#[tokio::test]
async fn failure_settlement_rechecks_deadline_after_waiting_for_store() {
    let root = tempfile::tempdir().unwrap();
    let service = Service::new(crate::config::Config {
        data_dir: root.path().join("data"),
        home: root.path().join("home"),
        workspace_roots: vec![root.path().into()],
        public_url: "http://localhost:4310".into(),
        host: "127.0.0.1".into(),
        port: 0,
        codex_bin: "codex".into(),
        claude_bin: "claude".into(),
        opencode_bin: "opencode".into(),
        opencode_go_usage_url: crate::accounts::opencode::USAGE_URL.into(),
        antigravity_bin: "agy".into(),
        concurrency: 1,
        logger: false,
        worker_enabled: false,
        runner_url: String::new(),
    })
    .await
    .unwrap();
    let id = crate::config::id();
    let run = json!({ "id": id, "taskId": id, "status": "running" });
    service
        .store
        .transaction(move |db| db.add_run_fixture(&run, None).map(|_| ()))
        .await
        .unwrap();
    let saved = RunCheckpoint::from_value(json!({
        "prepared": { "backend": "native", "isolated": true }, "runnerId": "attempt",
        "completed": true, "terminalCode": 0, "remainingMs": 200,
    }))
    .unwrap();
    saved.store(&service.store, &id).await.unwrap();
    let checkpoint = Arc::new(Checkpoint::new(service.store.clone(), id.clone(), saved));
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let store = service.store.clone();
    let blocker = tokio::spawn(async move {
        store
            .transaction(move |_| {
                let _ = entered_tx.send(());
                release_rx.recv().unwrap();
                Ok(())
            })
            .await
            .unwrap();
    });
    entered_rx.await.unwrap();
    let active_service = service.clone();
    let active_checkpoint = checkpoint.clone();
    let settlement = tokio::spawn(async move {
        record_failure(
            &active_service,
            &active_checkpoint,
            &CancellationToken::new(),
            &Error::unavailable("transport interrupted"),
            &[],
        )
        .await
        .unwrap();
    });
    tokio::time::sleep(Duration::from_millis(300)).await;
    release_tx.send(()).unwrap();
    blocker.await.unwrap();
    settlement.await.unwrap();
    let state = service.store.run_state(&id).await.unwrap();
    assert_eq!(state.status, RunStatus::Failed);
    assert_eq!(state.error.as_deref(), Some("Run exceeded its time limit."));
    service.shutdown.cancel();
}

#[tokio::test]
async fn generic_failure_settlement_rechecks_cancel_and_deadline_in_transaction() {
    let root = tempfile::tempdir().unwrap();
    let service = Service::new(crate::config::Config {
        data_dir: root.path().join("data"),
        home: root.path().join("home"),
        workspace_roots: vec![root.path().into()],
        public_url: "http://localhost:4310".into(),
        host: "127.0.0.1".into(),
        port: 0,
        codex_bin: "codex".into(),
        claude_bin: "claude".into(),
        opencode_bin: "opencode".into(),
        opencode_go_usage_url: crate::accounts::opencode::USAGE_URL.into(),
        antigravity_bin: "agy".into(),
        concurrency: 1,
        logger: false,
        worker_enabled: false,
        runner_url: String::new(),
    })
    .await
    .unwrap();
    for can_recover in [false, true] {
        for cancelled in [false, true] {
            let id = crate::config::id();
            let run = json!({ "id": id, "taskId": id, "status": "running", "sessionId": if can_recover { Some("session") } else { None } });
            service
                .store
                .transaction(move |db| db.add_run_fixture(&run, None).map(|_| ()))
                .await
                .unwrap();
            let saved = RunCheckpoint::from_value(json!({
                "prepared": { "backend": "native", "isolated": true }, "runnerId": "attempt",
                "completed": false, "terminalCode": null,
                "remainingMs": if cancelled { None } else { Some(200) },
            }))
            .unwrap();
            saved.store(&service.store, &id).await.unwrap();
            let checkpoint = Arc::new(Checkpoint::new(service.store.clone(), id.clone(), saved));
            let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
            let (release_tx, release_rx) = std::sync::mpsc::channel();
            let store = service.store.clone();
            let blocked_id = id.clone();
            let blocker = tokio::spawn(async move {
                store
                    .transaction(move |db| {
                        let _ = entered_tx.send(());
                        release_rx.recv().unwrap();
                        if cancelled {
                            db.change_run(
                                &blocked_id,
                                crate::runs::RunChanges {
                                    cancel_requested_at: Some(Some(now())),
                                    ..Default::default()
                                },
                            )?;
                        }
                        Ok(())
                    })
                    .await
                    .unwrap();
            });
            entered_rx.await.unwrap();
            let active_service = service.clone();
            let active_checkpoint = checkpoint.clone();
            let settlement = tokio::spawn(async move {
                record_failure(
                    &active_service,
                    &active_checkpoint,
                    &CancellationToken::new(),
                    &Error::unavailable("controller unavailable"),
                    &[],
                )
                .await
                .unwrap();
            });
            tokio::time::sleep(Duration::from_millis(300)).await;
            release_tx.send(()).unwrap();
            blocker.await.unwrap();
            settlement.await.unwrap();
            let persisted = RunCheckpoint::load(&service.store, &id)
                .await
                .unwrap()
                .unwrap();
            let settled = persisted.settled().unwrap();
            assert_eq!(
                settled.status,
                if cancelled {
                    RunStatus::Cancelled
                } else {
                    RunStatus::Failed
                }
            );
            if !cancelled {
                assert!(
                    settled
                        .summary
                        .as_deref()
                        .unwrap_or_default()
                        .contains("time limit")
                );
            }
            assert!(!persisted.has_controller_recovery_reservation());
            assert_eq!(
                service.store.run_state(&id).await.unwrap().status,
                RunStatus::Queued
            );
        }
    }
    service.shutdown.cancel();
}

#[tokio::test]
async fn transport_failure_schedules_only_confirmed_provider_ending_for_current_attempt() {
    let root = tempfile::tempdir().unwrap();
    let service = Service::new(crate::config::Config {
        data_dir: root.path().join("data"),
        home: root.path().join("home"),
        workspace_roots: vec![root.path().into()],
        public_url: "http://localhost:4310".into(),
        host: "127.0.0.1".into(),
        port: 0,
        codex_bin: "codex".into(),
        claude_bin: "claude".into(),
        opencode_bin: "opencode".into(),
        opencode_go_usage_url: crate::accounts::opencode::USAGE_URL.into(),
        antigravity_bin: "agy".into(),
        concurrency: 1,
        logger: false,
        worker_enabled: false,
        runner_url: String::new(),
    })
    .await
    .unwrap();
    for same in [true, false] {
        let id = crate::config::id();
        let run = json!({ "id": id, "taskId": id, "status": "running", "sessionId": "session" });
        service
            .store
            .transaction(move |db| db.add_run_fixture(&run, None).map(|_| ()))
            .await
            .unwrap();
        let cause = crate::run_retry::Cause::classify(
            &json!({ "code": "server_overloaded", "message": "Provider busy" }),
        )
        .unwrap();
        let mut saved = RunCheckpoint::from_value(json!({
            "prepared": { "backend": "firecracker", "isolated": true },
            "runnerId": "current", "providerFailureAttempt": if same { "current" } else { "old" },
            "completed": false, "terminalCode": 1, "outputInterrupted": true,
            "remainingMs": null, "controllerRecoveries": 3,
        }))
        .unwrap();
        saved.retry_cause = Some(Some(cause));
        saved.store(&service.store, &id).await.unwrap();
        let checkpoint = Checkpoint::new(service.store.clone(), id.clone(), saved);
        record_failure(
            &service,
            &checkpoint,
            &CancellationToken::new(),
            &Error::unavailable("Transport lost"),
            &[],
        )
        .await
        .unwrap();
        let state = service.store.run_state(&id).await.unwrap();
        let saved = RunCheckpoint::load(&service.store, &id)
            .await
            .unwrap()
            .unwrap();
        if same {
            assert_eq!(state.status, RunStatus::Queued);
            assert_eq!(state.retry.unwrap().cause.message, "Provider busy");
            assert!(saved.has_provider_retry_reservation());
        } else {
            assert_eq!(state.status, RunStatus::Failed);
            assert!(state.retry.is_none());
            assert!(!saved.has_provider_retry_reservation());
        }
        assert_eq!(saved.controller_recoveries(), 3);
    }
    service.shutdown.cancel();
}

#[tokio::test]
async fn capacity_refunds_only_paid_rejected_current_attempt() {
    let root = tempfile::tempdir().unwrap();
    let service = Service::new(crate::config::Config {
        data_dir: root.path().join("data"),
        home: root.path().join("home"),
        workspace_roots: vec![root.path().into()],
        public_url: "http://localhost:4310".into(),
        host: "127.0.0.1".into(),
        port: 0,
        codex_bin: "codex".into(),
        claude_bin: "claude".into(),
        opencode_bin: "opencode".into(),
        opencode_go_usage_url: crate::accounts::opencode::USAGE_URL.into(),
        antigravity_bin: "agy".into(),
        concurrency: 1,
        logger: false,
        worker_enabled: false,
        runner_url: String::new(),
    })
    .await
    .unwrap();
    for (count, rejected, same_attempt, cancelled, expired, paid) in [
        (3, true, true, false, false, true),
        (4, true, true, false, false, true),
        (3, false, true, false, false, true),
        (3, true, false, false, false, true),
        (3, true, true, true, false, true),
        (3, true, true, false, true, true),
        (3, true, true, false, false, false),
    ] {
        let id = crate::config::id();
        let run = json!({ "id": id, "taskId": id, "status": "running" });
        service
            .store
            .transaction(move |db| db.add_run_fixture(&run, None).map(|_| ()))
            .await
            .unwrap();
        let mut saved = RunCheckpoint::from_value(json!({
            "prepared": { "backend": "native", "isolated": true }, "runnerId": "current",
            "completed": false, "terminalCode": null, "remainingMs": if expired { Some(0) } else { None },
            "startRejected": rejected, "controllerRecoveries": count,
        })).unwrap();
        if paid {
            saved.pending_paid_admission = Some(super::super::checkpoint::PaidAdmission {
                attempt: if same_attempt { "current" } else { "old" }.into(),
                controller: true,
                capacity_refused: false,
                provider_cause: None,
            });
        }
        saved.store(&service.store, &id).await.unwrap();
        let checkpoint = Checkpoint::new(service.store.clone(), id.clone(), saved);
        let cancel = CancellationToken::new();
        if cancelled {
            cancel.cancel();
        }
        let error = Error::unavailable(format!(
            "{} has no capacity",
            crate::nodes::placement::NO_CAPACITY
        ));
        record_failure(&service, &checkpoint, &cancel, &error, &[])
            .await
            .unwrap();
        let durable = RunCheckpoint::load(&service.store, &id)
            .await
            .unwrap()
            .unwrap();
        let expected = rejected && same_attempt && !cancelled && !expired && paid;
        assert_eq!(
            durable.has_controller_recovery_reservation(),
            expected,
            "count={count}, rejected={rejected}, same_attempt={same_attempt}, cancelled={cancelled}, expired={expired}, paid={paid}"
        );
        assert_eq!(durable.controller_recoveries(), count);
        if expected {
            let run = service.store.run_state(&id).await.unwrap();
            assert_eq!(run.status, RunStatus::Queued);
            let key = id.clone();
            assert!(
                service
                    .store
                    .transaction(move |db| super::super::schedule::finish_recovery(
                        db,
                        &key,
                        Some(&durable)
                    ))
                    .await
                    .unwrap()
            );
        }
    }
    service.shutdown.cancel();
}

#[tokio::test]
async fn provider_capacity_refund_requires_same_attempt_and_retry_cause() {
    let root = tempfile::tempdir().unwrap();
    let service = Service::new(crate::config::Config {
        data_dir: root.path().join("data"),
        home: root.path().join("home"),
        workspace_roots: vec![root.path().into()],
        public_url: "http://localhost:4310".into(),
        host: "127.0.0.1".into(),
        port: 0,
        codex_bin: "codex".into(),
        claude_bin: "claude".into(),
        opencode_bin: "opencode".into(),
        opencode_go_usage_url: crate::accounts::opencode::USAGE_URL.into(),
        antigravity_bin: "agy".into(),
        concurrency: 1,
        logger: false,
        worker_enabled: false,
        runner_url: String::new(),
    })
    .await
    .unwrap();
    let cause = crate::run_retry::Cause::classify(
        &json!({ "code": "server_overloaded", "message": "busy" }),
    )
    .unwrap();
    for (same_attempt, same_cause, rejected, cancelled, expired) in [
        (true, true, true, false, false),
        (false, true, true, false, false),
        (true, false, true, false, false),
        (true, true, false, false, false),
        (true, true, true, true, false),
        (true, true, true, false, true),
    ] {
        let id = crate::config::id();
        let run = json!({ "id": id, "taskId": id, "status": "running" });
        service
            .store
            .transaction(move |db| db.add_run_fixture(&run, None).map(|_| ()))
            .await
            .unwrap();
        let retry_cause = if same_cause {
            cause.clone()
        } else {
            crate::run_retry::Cause::classify(
                &json!({ "code": "request_timeout", "message": "different" }),
            )
            .unwrap()
        };
        let mut retry = crate::run_retry::Retry::next(None, retry_cause, now()).unwrap();
        retry.next_attempt_at = None;
        service
            .store
            .change_run(
                &id,
                crate::runs::RunChanges {
                    retry: Some(Some(retry)),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let mut saved = RunCheckpoint::from_value(json!({
            "prepared": { "backend": "firecracker", "isolated": true }, "runnerId": "current",
            "completed": false, "terminalCode": null, "remainingMs": if expired { Some(0) } else { None },
            "startRejected": rejected, "controllerRecoveries": 3,
        })).unwrap();
        saved.pending_paid_admission = Some(super::super::checkpoint::PaidAdmission {
            attempt: if same_attempt { "current" } else { "old" }.into(),
            controller: false,
            capacity_refused: false,
            provider_cause: Some(cause.clone()),
        });
        saved.store(&service.store, &id).await.unwrap();
        let checkpoint = Checkpoint::new(service.store.clone(), id.clone(), saved);
        let cancel = CancellationToken::new();
        if cancelled {
            cancel.cancel();
        }
        let error = Error::unavailable(format!(
            "{} has no capacity",
            crate::nodes::placement::NO_CAPACITY
        ));
        record_failure(&service, &checkpoint, &cancel, &error, &[])
            .await
            .unwrap();
        let durable = RunCheckpoint::load(&service.store, &id)
            .await
            .unwrap()
            .unwrap();
        let expected = same_attempt && same_cause && rejected && !cancelled && !expired;
        assert_eq!(durable.has_provider_retry_reservation(), expected);
        assert!(!durable.has_controller_recovery_reservation());
        assert_eq!(durable.controller_recoveries(), 3);
        if expected {
            assert!(
                service
                    .store
                    .run_state(&id)
                    .await
                    .unwrap()
                    .retry
                    .unwrap()
                    .next_attempt_at
                    .is_some()
            );
            let key = id.clone();
            assert!(
                service
                    .store
                    .transaction(move |db| super::super::schedule::finish_recovery(
                        db,
                        &key,
                        Some(&durable)
                    ))
                    .await
                    .unwrap()
            );
        }
    }
    service.shutdown.cancel();
}

#[tokio::test]
async fn failure_settlement_rechecks_deadline_after_waiting_for_store_reconciliation() {
    let root = tempfile::tempdir().unwrap();
    let service = Service::new(crate::config::Config {
        data_dir: root.path().join("data"),
        home: root.path().join("home"),
        workspace_roots: vec![root.path().into()],
        public_url: "http://localhost:4310".into(),
        host: "127.0.0.1".into(),
        port: 0,
        codex_bin: "codex".into(),
        claude_bin: "claude".into(),
        opencode_bin: "opencode".into(),
        opencode_go_usage_url: crate::accounts::opencode::USAGE_URL.into(),
        antigravity_bin: "agy".into(),
        concurrency: 1,
        logger: false,
        worker_enabled: false,
        runner_url: String::new(),
    })
    .await
    .unwrap();
    let id = crate::config::id();
    let run = json!({ "id": id, "taskId": id, "status": "running" });
    service
        .store
        .transaction(move |db| db.add_run_fixture(&run, None).map(|_| ()))
        .await
        .unwrap();
    let saved = RunCheckpoint::from_value(json!({
        "prepared": { "backend": "native", "isolated": true }, "runnerId": "attempt",
        "completed": true, "terminalCode": 0, "remainingMs": 200,
    }))
    .unwrap();
    saved.store(&service.store, &id).await.unwrap();
    let checkpoint = Arc::new(Checkpoint::new(service.store.clone(), id.clone(), saved));
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let store = service.store.clone();
    let blocker = tokio::spawn(async move {
        store
            .transaction(move |_| {
                let _ = entered_tx.send(());
                release_rx.recv().unwrap();
                Ok(())
            })
            .await
            .unwrap();
    });
    entered_rx.await.unwrap();
    let active_service = service.clone();
    let active_checkpoint = checkpoint.clone();
    let settlement = tokio::spawn(async move {
        record_failure(
            &active_service,
            &active_checkpoint,
            &CancellationToken::new(),
            &Error::unavailable("transport interrupted"),
            &[],
        )
        .await
        .unwrap();
    });
    tokio::time::sleep(Duration::from_millis(300)).await;
    release_tx.send(()).unwrap();
    blocker.await.unwrap();
    settlement.await.unwrap();
    let state = service.store.run_state(&id).await.unwrap();
    assert_eq!(state.status, RunStatus::Failed);
    assert_eq!(state.error.as_deref(), Some("Run exceeded its time limit."));

    let resumed_id = crate::config::id();
    let run = json!({ "id": resumed_id, "taskId": resumed_id, "status": "running" });
    service
        .store
        .transaction(move |db| db.add_run_fixture(&run, None).map(|_| ()))
        .await
        .unwrap();
    let saved = RunCheckpoint::from_value(json!({
        "prepared": { "backend": "native", "isolated": true }, "runnerId": "attempt",
        "completed": true, "terminalCode": 0, "remainingMs": 60_000,
    }))
    .unwrap();
    saved.store(&service.store, &resumed_id).await.unwrap();
    let checkpoint = Checkpoint::new(service.store.clone(), resumed_id.clone(), saved);
    checkpoint.account_pause("attempt", 30_000);
    record_failure(
        &service,
        &checkpoint,
        &CancellationToken::new(),
        &Error::unavailable("transport interrupted"),
        &[],
    )
    .await
    .unwrap();
    let persisted = RunCheckpoint::load(&service.store, &resumed_id)
        .await
        .unwrap()
        .unwrap();
    let observation = persisted.administrative_pause_observation.unwrap();
    assert_eq!(
        (observation.attempt.as_str(), observation.total_ms),
        ("attempt", 30_000)
    );
    assert!(persisted.remaining_ms.flatten().unwrap() > 89_000);
    assert_eq!(
        service.store.run_state(&resumed_id).await.unwrap().status,
        RunStatus::Succeeded
    );
    // If the durable attempt loses its terminal evidence while settlement
    // waits, finish_recovery must retain its newly paid reservation.
    for existing_reservation in [false, true] {
        let retry_id = crate::config::id();
        let run = json!({ "id": retry_id, "taskId": retry_id, "status": "running" });
        service
            .store
            .transaction(move |db| db.add_run_fixture(&run, None).map(|_| ()))
            .await
            .unwrap();
        let saved = RunCheckpoint::from_value(json!({
            "prepared": { "backend": "native", "isolated": true }, "runnerId": "attempt",
            "completed": true, "terminalCode": 0, "remainingMs": 60_000,
        }))
        .unwrap();
        saved.store(&service.store, &retry_id).await.unwrap();
        let checkpoint = Arc::new(Checkpoint::new(
            service.store.clone(),
            retry_id.clone(),
            saved,
        ));
        checkpoint.account_pause("attempt", 30_000);
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let (store, changed_id) = (service.store.clone(), retry_id.clone());
        let blocker = tokio::spawn(async move {
            store
                .transaction(move |db| {
                    let _ = entered_tx.send(());
                    release_rx.recv().unwrap();
                    let mut current = RunCheckpoint::load_in(db, &changed_id)?.unwrap();
                    current.completed = Some(false);
                    current.terminal_code = Some(None);
                    if existing_reservation {
                        current.controller_recoveries = Some(1);
                        current.controller_recovery_reservation = Some("attempt".into());
                    }
                    current.store_in(db, &changed_id)
                })
                .await
                .unwrap();
        });
        entered_rx.await.unwrap();
        let (running_service, running_checkpoint) = (service.clone(), checkpoint.clone());
        let settlement = tokio::spawn(async move {
            record_failure(
                &running_service,
                &running_checkpoint,
                &CancellationToken::new(),
                &Error::unavailable("transport interrupted"),
                &[],
            )
            .await
            .unwrap();
        });
        tokio::time::sleep(Duration::from_millis(100)).await;
        release_tx.send(()).unwrap();
        blocker.await.unwrap();
        settlement.await.unwrap();
        let paid = RunCheckpoint::load(&service.store, &retry_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(paid.controller_recoveries(), 1);
        assert!(paid.has_controller_recovery_reservation());
        assert_eq!(
            paid.administrative_pause_observation.unwrap().total_ms,
            30_000
        );
        assert!(paid.remaining_ms.flatten().unwrap() > 89_000);
    }

    service.shutdown.cancel();
}

#[tokio::test]
async fn generic_failure_settlement_rechecks_cancel_and_deadline_in_transaction_reconciliation() {
    let root = tempfile::tempdir().unwrap();
    let service = Service::new(crate::config::Config {
        data_dir: root.path().join("data"),
        home: root.path().join("home"),
        workspace_roots: vec![root.path().into()],
        public_url: "http://localhost:4310".into(),
        host: "127.0.0.1".into(),
        port: 0,
        codex_bin: "codex".into(),
        claude_bin: "claude".into(),
        opencode_bin: "opencode".into(),
        opencode_go_usage_url: crate::accounts::opencode::USAGE_URL.into(),
        antigravity_bin: "agy".into(),
        concurrency: 1,
        logger: false,
        worker_enabled: false,
        runner_url: String::new(),
    })
    .await
    .unwrap();
    for can_recover in [false, true] {
        for cancelled in [false, true] {
            let id = crate::config::id();
            let run = json!({ "id": id, "taskId": id, "status": "running", "sessionId": if can_recover { Some("session") } else { None } });
            service
                .store
                .transaction(move |db| db.add_run_fixture(&run, None).map(|_| ()))
                .await
                .unwrap();
            let saved = RunCheckpoint::from_value(json!({
                "prepared": { "backend": "native", "isolated": true }, "runnerId": "attempt",
                "completed": false, "terminalCode": null,
                "remainingMs": if cancelled { None } else { Some(200) },
            }))
            .unwrap();
            saved.store(&service.store, &id).await.unwrap();
            let checkpoint = Arc::new(Checkpoint::new(service.store.clone(), id.clone(), saved));
            let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
            let (release_tx, release_rx) = std::sync::mpsc::channel();
            let store = service.store.clone();
            let blocked_id = id.clone();
            let blocker = tokio::spawn(async move {
                store
                    .transaction(move |db| {
                        let _ = entered_tx.send(());
                        release_rx.recv().unwrap();
                        if cancelled {
                            db.change_run(
                                &blocked_id,
                                crate::runs::RunChanges {
                                    cancel_requested_at: Some(Some(now())),
                                    ..Default::default()
                                },
                            )?;
                        }
                        Ok(())
                    })
                    .await
                    .unwrap();
            });
            entered_rx.await.unwrap();
            let active_service = service.clone();
            let active_checkpoint = checkpoint.clone();
            let settlement = tokio::spawn(async move {
                record_failure(
                    &active_service,
                    &active_checkpoint,
                    &CancellationToken::new(),
                    &Error::unavailable("controller unavailable"),
                    &[],
                )
                .await
                .unwrap();
            });
            tokio::time::sleep(Duration::from_millis(300)).await;
            release_tx.send(()).unwrap();
            blocker.await.unwrap();
            settlement.await.unwrap();
            let persisted = RunCheckpoint::load(&service.store, &id)
                .await
                .unwrap()
                .unwrap();
            let settled = persisted.settled().unwrap();
            assert_eq!(
                settled.status,
                if cancelled {
                    RunStatus::Cancelled
                } else {
                    RunStatus::Failed
                }
            );
            if !cancelled {
                assert!(
                    settled
                        .summary
                        .as_deref()
                        .unwrap_or_default()
                        .contains("time limit")
                );
            }
            assert!(!persisted.has_controller_recovery_reservation());
            assert_eq!(
                service.store.run_state(&id).await.unwrap().status,
                RunStatus::Queued
            );
        }
    }
    service.shutdown.cancel();
}

#[tokio::test]
async fn remote_preparation_failures_stop_after_three_recoveries() {
    let root = tempfile::tempdir().unwrap();
    let service = Service::new(crate::config::Config {
        data_dir: root.path().join("data"),
        home: root.path().join("home"),
        workspace_roots: vec![root.path().into()],
        public_url: "http://localhost:4310".into(),
        host: "127.0.0.1".into(),
        port: 0,
        codex_bin: "codex".into(),
        claude_bin: "claude".into(),
        opencode_bin: "opencode".into(),
        opencode_go_usage_url: crate::accounts::opencode::USAGE_URL.into(),
        antigravity_bin: "agy".into(),
        concurrency: 1,
        logger: false,
        worker_enabled: false,
        runner_url: String::new(),
    })
    .await
    .unwrap();
    let run = crate::config::id();
    service
        .store
        .transaction({
            let run = run.clone();
            move |db| {
                let record = json!({
                    "id": run,
                    "taskId": run,
                    "status": "running",
                    "sessionId": "saved-session",
                });
                db.add_run_fixture(&record, None)?;
                Ok(())
            }
        })
        .await
        .unwrap();
    let checkpoint = Checkpoint::new(
        service.store.clone(),
        run.clone(),
        RunCheckpoint::from_value(json!({
            "prepared": { "backend": "firecracker", "isolated": true },
            "nodeId": crate::config::id(),
            "runnerId": crate::config::id(),
            "remainingMs": null,
            "controllerRecoveries": MAX_CONTROLLER_RECOVERIES - 1,
        }))
        .unwrap(),
    );
    checkpoint.persist().await.unwrap();
    let cancel = CancellationToken::new();
    let error =
        Error::unavailable("Remote workspace preparation failed; source files are preserved.");
    record_failure(&service, &checkpoint, &cancel, &error, &[])
        .await
        .unwrap();
    assert_eq!(
        checkpoint.snapshot().await.controller_recoveries(),
        MAX_CONTROLLER_RECOVERIES
    );
    assert!(checkpoint.snapshot().await.settled().is_none());
    // The paid recovery is consumed by the replacement attempt. Its own
    // failure must settle at the ceiling even when a session exists.
    checkpoint
        .update(|saved| {
            saved.runner_id = Some(Some(crate::config::id()));
            saved.controller_recovery_reservation = None;
        })
        .await
        .unwrap();
    record_failure(&service, &checkpoint, &cancel, &error, &[])
        .await
        .unwrap();
    assert_eq!(
        checkpoint
            .snapshot()
            .await
            .settled()
            .unwrap()
            .run_changes()
            .status,
        Some(RunStatus::Failed)
    );
    assert_eq!(
        checkpoint.snapshot().await.controller_recoveries(),
        MAX_CONTROLLER_RECOVERIES
    );
    service.shutdown.cancel();
}

#[tokio::test]
async fn native_mode_recovery_is_paid_and_admitted_after_fencing() {
    use axum::{Router, routing::any};
    let app = Router::new().fallback(any(|| async { axum::http::StatusCode::NO_CONTENT }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let root = tempfile::tempdir().unwrap();
    let service = Service::new(crate::config::Config {
        data_dir: root.path().join("data"),
        home: root.path().join("home"),
        workspace_roots: vec![root.path().into()],
        public_url: "http://localhost:4310".into(),
        host: "127.0.0.1".into(),
        port: 0,
        codex_bin: "codex".into(),
        claude_bin: "claude".into(),
        opencode_bin: "opencode".into(),
        opencode_go_usage_url: crate::accounts::opencode::USAGE_URL.into(),
        antigravity_bin: "agy".into(),
        concurrency: 1,
        logger: false,
        worker_enabled: false,
        runner_url: format!("http://{address}"),
    })
    .await
    .unwrap();
    for (count, forbidden, cancelled) in [
        (3, false, false),
        (4, false, false),
        (3, true, false),
        (3, false, true),
    ] {
        let id = crate::config::id();
        let run = json!({ "id": id, "taskId": id, "status": "running", "createdAt": now(),
            "nativeModeInterruption": crate::config::id(),
            "snapshot": { "agent": { "id": crate::config::MAIN_AGENT_ID, "provider": "codex" } } });
        service
            .store
            .transaction(move |db| db.add_run_fixture(&run, None).map(|_| ()))
            .await
            .unwrap();
        let saved = RunCheckpoint::from_value(json!({
            "prepared": { "backend": "native", "isolated": true }, "runnerId": crate::config::id(),
            "completed": false, "terminalCode": null, "remainingMs": null,
            "controllerRecoveries": count,
        }))
        .unwrap();
        saved.store(&service.store, &id).await.unwrap();
        let checkpoint = Checkpoint::new(service.store.clone(), id.clone(), saved);
        let cancel = CancellationToken::new();
        if cancelled {
            cancel.cancel();
        }
        let error = if forbidden {
            Error::forbidden("Native admission revoked")
        } else {
            Error::unavailable("Controlled native stop")
        };
        record_failure(&service, &checkpoint, &cancel, &error, &[])
            .await
            .unwrap();
        let reserved = RunCheckpoint::load(&service.store, &id)
            .await
            .unwrap()
            .unwrap();
        if forbidden || cancelled {
            assert!(!reserved.has_controller_recovery_reservation());
            assert!(reserved.settled().is_some());
        } else {
            assert_eq!(reserved.controller_recoveries(), count + 1);
            assert!(reserved.has_controller_recovery_reservation());
        }
        // Fencing uses only the disposable controller; tick then settles the
        // durable reservation before any new placement can be admitted.
        service.worker.tick(&service).await.unwrap();
        let current = service.store.run_state(&id).await.unwrap();
        if forbidden || cancelled {
            assert_eq!(
                current.status,
                if cancelled {
                    RunStatus::Cancelled
                } else {
                    RunStatus::Failed
                }
            );
        } else {
            assert_eq!(current.status, RunStatus::Queued);
            assert!(!current.recovery_pending);
            let saved = RunCheckpoint::load(&service.store, &id)
                .await
                .unwrap()
                .unwrap();
            assert!(saved.has_controller_recovery_reservation());
            assert_eq!(saved.controller_recoveries(), count + 1);
        }
    }
    service.shutdown.cancel();
    server.abort();
}

#[tokio::test]
async fn native_mode_recovery_retries_pre_session_interruptions_but_not_revocations() {
    let root = tempfile::tempdir().unwrap();
    let service = Service::new(crate::config::Config {
        data_dir: root.path().join("data"),
        home: root.path().join("home"),
        workspace_roots: vec![root.path().into()],
        public_url: "http://localhost:4310".into(),
        host: "127.0.0.1".into(),
        port: 0,
        codex_bin: "codex".into(),
        claude_bin: "claude".into(),
        opencode_bin: "opencode".into(),
        opencode_go_usage_url: crate::accounts::opencode::USAGE_URL.into(),
        antigravity_bin: "agy".into(),
        concurrency: 1,
        logger: false,
        worker_enabled: false,
        runner_url: String::new(),
    })
    .await
    .unwrap();
    let run = crate::config::id();
    let record = json!({
        "id": run, "taskId": crate::config::id(), "status": "running", "createdAt": now(),
        "nativeModeInterruption": crate::config::id(),
    });
    service
        .store
        .transaction(move |db| db.add_run_fixture(&record, None).map(|_| ()))
        .await
        .unwrap();
    let saved = RunCheckpoint::from_value(json!({
        "prepared": { "backend": "native", "isolated": true },
        "nodeId": crate::config::id(), "runnerId": crate::config::id(),
        "controllerRecoveries": MAX_CONTROLLER_RECOVERIES + 1, "remainingMs": null,
    }))
    .unwrap();
    let checkpoint = Checkpoint::new(service.store.clone(), run, saved.clone());
    let cancel = CancellationToken::new();
    assert!(
        recovers_controller(
            &service,
            &checkpoint,
            &saved,
            &cancel,
            &Error::unavailable("Controlled native stop")
        )
        .await
        .unwrap()
    );
    for error in [
        Error::forbidden("Native admission revoked"),
        Error::conflict("Saved session unavailable"),
    ] {
        assert!(
            !recovers_controller(&service, &checkpoint, &saved, &cancel, &error)
                .await
                .unwrap()
        );
    }
    cancel.cancel();
    assert!(
        !recovers_controller(
            &service,
            &checkpoint,
            &saved,
            &cancel,
            &Error::unavailable("Controlled native stop")
        )
        .await
        .unwrap()
    );
    service.shutdown.cancel();
}

#[tokio::test]
async fn completed_turns_never_request_remote_publication() {
    let root = tempfile::tempdir().unwrap();
    let service = Service::new(crate::config::Config {
        data_dir: root.path().join("data"),
        home: root.path().join("home"),
        workspace_roots: vec![root.path().into()],
        public_url: "http://localhost:4310".into(),
        host: "127.0.0.1".into(),
        port: 0,
        codex_bin: "codex".into(),
        claude_bin: "claude".into(),
        opencode_bin: "opencode".into(),
        opencode_go_usage_url: crate::accounts::opencode::USAGE_URL.into(),
        antigravity_bin: "agy".into(),
        concurrency: 1,
        logger: false,
        worker_enabled: false,
        runner_url: String::new(),
    })
    .await
    .unwrap();
    for (backend, status, fenced) in [
        ("native", "succeeded", true),
        ("native", "failed", true),
        ("native", "cancelled", true),
        ("native", "queued", true),
        ("firecracker", "waiting", true),
        ("native", "failed", false),
        ("firecracker", "failed", true),
        ("firecracker", "cancelled", true),
    ] {
        let run = crate::config::id();
        service
            .store
            .transaction({
                let run = run.clone();
                move |db| {
                    let record = json!({
                        "id": run,
                        "taskId": run,
                        "status": status,
                        "isolated": true,
                        "storage": { "mode": "on-demand" },
                    });
                    db.add_run_fixture(&record, None)?;
                    Ok(())
                }
            })
            .await
            .unwrap();
        let checkpoint = Checkpoint::new(
            service.store.clone(),
            run.clone(),
            RunCheckpoint::from_value(json!({
                "prepared": { "backend": backend, "isolated": true },
                "runnerId": crate::config::id(),
            }))
            .unwrap(),
        );
        checkpoint.persist().await.unwrap();
        release(&service, &checkpoint, None, fenced).await.unwrap();
        let current = service.store.run(&run).await.unwrap();
        assert!(current["backup"].is_null());
    }
    service.shutdown.cancel();
}
