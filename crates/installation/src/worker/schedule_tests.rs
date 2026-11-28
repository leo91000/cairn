use crate::account_fixture::AccountFixtures;

use super::*;
use crate::agent_fixture::AgentFixture;
use std::{future::Future, task::Poll};

#[tokio::test]
async fn maintenance_removes_checkpoints_after_run_history_expires() {
    let (_root, service, _chat, run, _workspace) = fixture().await;
    service
        .store
        .event(
            &run,
            &cairn_contracts::EventKind::TurnFailed,
            "expired secret",
            Some(json!({"error":{"message":"expired secret"}})),
        )
        .await
        .unwrap();
    let id = run.clone();
    service
        .store
        .write(move |db| {
            db.0.execute(
                "UPDATE runs SET status='failed', trigger='manual' WHERE id=?",
                [&id],
            )?;
            db.0.execute("UPDATE events SET created_at=1 WHERE run_id=?", [&id])?;
            Ok(())
        })
        .await
        .unwrap();
    crate::worker::Worker::default()
        .maintain(&service)
        .await
        .unwrap();
    service
        .store
        .read(move |db| {
            let remaining: i64 = db.0.query_row(
                "SELECT count(*) FROM timeline_state WHERE run_id=?",
                [&run],
                |row| row.get(0),
            )?;
            assert_eq!(remaining, 0);
            Ok(())
        })
        .await
        .unwrap();
}

async fn fixture() -> (
    tempfile::TempDir,
    Arc<Service>,
    String,
    String,
    std::path::PathBuf,
) {
    fixture_with_runner(String::new()).await
}

async fn fixture_with_runner(
    runner_url: String,
) -> (
    tempfile::TempDir,
    Arc<Service>,
    String,
    String,
    std::path::PathBuf,
) {
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
        runner_url,
    })
    .await
    .unwrap();
    let mut chat = service.chat_create(json!({})).await.unwrap();
    let chat_id = text(&chat, "id").to_owned();
    let run_id = crate::config::id();
    service.store.transaction({
        let run_id = run_id.clone();
        move |db| {
            let run = json!({
                "id": run_id,
                "taskId": run_id,
                "status": RunStatus::Queued,
                "snapshot": { "agent": { "id": crate::config::MAIN_AGENT_ID, "provider": "codex" } },
            });
            db.add_run_fixture(&run, None)?;
            Ok(())
        }
    }).await.unwrap();
    chat["runId"] = run_id.clone().into();
    service
        .store
        .save_conversation_boundary(chat)
        .await
        .unwrap();
    let workspace = service.config.data_dir.join("runs").join(&run_id);
    tokio::fs::create_dir_all(&workspace).await.unwrap();
    tokio::fs::write(workspace.join("work.txt"), "Private work")
        .await
        .unwrap();

    (root, service, chat_id, run_id, workspace)
}

#[tokio::test]
async fn manual_resume_resets_the_budget_but_still_fences_the_old_attempt() {
    use axum::{Router, http::StatusCode, routing::delete};
    use std::sync::atomic::{AtomicBool, Ordering};

    let fail = Arc::new(AtomicBool::new(true));
    let app = Router::new().route(
        "/runs/old-attempt",
        delete({
            let fail = fail.clone();
            move || async move {
                if fail.swap(false, Ordering::SeqCst) {
                    StatusCode::SERVICE_UNAVAILABLE
                } else {
                    StatusCode::NO_CONTENT
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    for (minutes, budget) in [(0, None), (10, Some(600_000))] {
        fail.store(true, Ordering::SeqCst);
        let (_root, service, _chat, run, workspace) =
            fixture_with_runner(format!("http://{address}")).await;
        service
            .store
            .transaction({
                let run = run.clone();
                move |db| {
                    crate::run_fixture::change_run_fixture(
                        db,
                        &run,
                        &json!({
                            "status": RunStatus::Failed,
                            "trigger": "chat",
                            "sessionId": "saved-session",
                            "resumeAvailable": true,
                            "snapshot": {
                                "agent": {
                                    "id": crate::config::MAIN_AGENT_ID,
                                    "provider": "codex",
                                    "timeoutMinutes": minutes,
                                }
                            },
                            "chatExecution": { "messageId": "original-message" },
                        }),
                    )?;
                    let checkpoint = RunCheckpoint::from_value(json!({
                        "launched": true,
                        "prepared": { "backend": "native", "isolated": true },
                        "runnerId": "old-attempt",
                        "remainingMs": 0,
                        "heldEnvironment": {
                            "attempt": "old-attempt",
                            "generation": "old-generation",
                            "expiresAt": now() - 1,
                        },
                    }))?;
                    checkpoint.store_in(db, &run)?;
                    db.0.execute(
                        "INSERT INTO background_jobs(request_id,run_id,attempt_id,payload_hash,outcome) VALUES
                        ('pending',?1,'old-attempt','hash',NULL),
                        ('running',?1,'old-attempt','hash',json_object('state','running')),
                        ('finished',?1,'old-attempt','hash',json_object('state','finished')),
                        ('unrelated',?1,'another-attempt','hash',json_object('state','running'))",
                        [&run],
                    )?;
                    Ok(())
                }
            })
            .await
            .unwrap();

        let resumed = service.worker.resume(&service, &run).await.unwrap();
        assert!(recover(&service, &resumed).await.is_err());
        let blocked = RunCheckpoint::load(&service.store, &run)
            .await
            .unwrap()
            .unwrap();
        assert!(blocked.held_environment.is_none());
        assert_eq!(blocked.runner_id(), Some("old-attempt"));
        assert_eq!(blocked.remaining_ms, Some(budget));
        assert!(
            service
                .store
                .run_state(&run)
                .await
                .unwrap()
                .recovery_pending
        );

        assert!(recover(&service, &resumed).await.unwrap());
        let saved = RunCheckpoint::load(&service.store, &run)
            .await
            .unwrap()
            .unwrap();
        assert!(saved.held_environment.is_none());
        assert_eq!(saved.remaining_ms, Some(budget));
        assert_eq!(saved.runner_id(), Some("old-attempt"));
        let checkpoint =
            super::super::checkpoint::Checkpoint::new(service.store.clone(), run.clone(), saved);
        assert!(!checkpoint.expired());
        let outcomes = service
            .store
            .read({
                let run = run.clone();
                move |db| {
                    Ok(db
                        .0
                        .prepare(
                            "SELECT json_extract(outcome,'$.state') FROM background_jobs
                     WHERE run_id=? ORDER BY request_id",
                        )?
                        .query_map([run], |row| row.get::<_, String>(0))?
                        .collect::<rusqlite::Result<Vec<_>>>()?)
                }
            })
            .await
            .unwrap();
        assert_eq!(outcomes, ["finished", "unknown", "unknown", "running"]);
        let state = service.store.run_state(&run).await.unwrap();
        assert_eq!(state.session_id.as_deref(), Some("saved-session"));
        assert!(!state.recovery_pending);
        assert_eq!(
            tokio::fs::read_to_string(workspace.join("work.txt"))
                .await
                .unwrap(),
            "Private work"
        );
        service.shutdown.cancel();
    }
    server.abort();
}

#[tokio::test]
async fn shutdown_during_active_observation_keeps_pending_checkpoint() {
    use axum::{Router, routing::any};
    let entered = Arc::new(tokio::sync::Notify::new());
    let seen = entered.clone();
    let app = Router::new().fallback(any(move || {
        let seen = seen.clone();
        async move {
            seen.notify_one();
            std::future::pending::<axum::http::StatusCode>().await
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let (_root, service, _chat, run, _workspace) =
        fixture_with_runner(format!("http://{address}")).await;
    let saved = RunCheckpoint::from_value(json!({
        "prepared": { "backend": "native", "isolated": true },
        "runnerId": "old-attempt", "completed": true, "controllerRecoveries": 3,
        "remainingMs": null,
    }))
    .unwrap();
    saved.store(&service.store, &run).await.unwrap();
    let checkpoint = Arc::new(super::super::checkpoint::Checkpoint::new(
        service.store.clone(),
        run.clone(),
        saved,
    ));
    let (s, cp) = (service.clone(), checkpoint.clone());
    let task = tokio::spawn(async move {
        super::super::execution::reconcile(&s, &cp, &CancellationToken::new()).await
    });
    entered.notified().await;
    service.shutdown.cancel();
    task.await.unwrap().unwrap();
    let saved = RunCheckpoint::load(&service.store, &run)
        .await
        .unwrap()
        .unwrap();
    assert!(saved.terminal_code.is_none());
    let observation = saved.controller_observation.unwrap();
    assert_eq!(observation.requests, 1);
    assert!(!observation.in_flight);
    server.abort();
}

#[tokio::test]
async fn active_observation_cannot_overwrite_replacement() {
    use axum::{Router, routing::any};
    let entered = Arc::new(tokio::sync::Notify::new());
    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    let app = Router::new().fallback(any({
        let (entered, gate) = (entered.clone(), gate.clone());
        move || {
            let (entered, gate) = (entered.clone(), gate.clone());
            async move {
                entered.notify_one();
                gate.acquire().await.unwrap().forget();
                axum::Json(json!({ "StatusCode": 0 }))
            }
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let (_root, service, _chat, run, _workspace) =
        fixture_with_runner(format!("http://{address}")).await;
    let original = RunCheckpoint::from_value(json!({
        "prepared": { "backend": "native", "isolated": true },
        "runnerId": "old-attempt", "remainingMs": null,
    }))
    .unwrap();
    original.store(&service.store, &run).await.unwrap();
    let checkpoint = Arc::new(super::super::checkpoint::Checkpoint::new(
        service.store.clone(),
        run.clone(),
        original.clone(),
    ));
    let (s, cp) = (service.clone(), checkpoint.clone());
    let task = tokio::spawn(async move {
        super::super::execution::reconcile(&s, &cp, &CancellationToken::new()).await
    });
    entered.notified().await;
    let mut replacement = original;
    replacement.runner_id = Some(Some("new-attempt".into()));
    replacement.controller_observation = None;
    replacement.store(&service.store, &run).await.unwrap();
    gate.add_permits(1);
    assert!(task.await.unwrap().is_err());
    let current = RunCheckpoint::load(&service.store, &run)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(current.runner_id(), Some("new-attempt"));
    assert!(current.terminal_code.is_none());
    assert!(current.controller_observation.is_none());
    service.shutdown.cancel();
    server.abort();
}

#[tokio::test]
async fn restart_provider_failure_reserves_each_retry_once_and_keeps_its_ceiling() {
    let (_root, service, _chat, run, _workspace) = fixture().await;
    service
        .store
        .change_run(
            &run,
            crate::runs::RunChanges {
                session_id: Some(Some("session".into())),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let cause = crate::run_retry::Cause::classify(
        &json!({"code":"server_error","message":"Provider unavailable"}),
    )
    .unwrap();
    for attempt in 1..=crate::run_retry::MAX_ATTEMPTS + 1 {
        let id = run.clone();
        let cause = cause.clone();
        service
            .store
            .transaction(move |db| {
                let mut previous = db.run_state(&id)?.unwrap().retry;
                if let Some(retry) = &mut previous {
                    retry.next_attempt_at = None;
                }
                db.change_run(
                    &id,
                    crate::runs::RunChanges {
                        status: Some(RunStatus::Queued),
                        retry: Some(previous),
                        ..Default::default()
                    },
                )?;
                let saved = RunCheckpoint::from_value(json!({
                    "prepared": {"backend":"native","isolated":true},
                    "runnerId":format!("attempt-{attempt}"), "remainingMs":null,
                    "terminalCode":1, "providerFailed":true,
                    "providerFailureAttempt":format!("attempt-{attempt}"),
                    "lastError":"Provider unavailable", "retryCause":cause
                }))?;
                saved.store_in(db, &id)?;
                assert!(!finish_recovery(db, &id, Some(&saved))?);
                let current = db.run_state(&id)?.unwrap();
                assert_eq!(
                    RunCheckpoint::load_in(db, &id)?
                        .unwrap()
                        .controller_recoveries(),
                    0
                );
                if attempt <= crate::run_retry::MAX_ATTEMPTS {
                    assert_eq!(current.retry.unwrap().attempt, attempt);
                    assert!(finish_recovery(db, &id, Some(&saved))?);
                    assert_eq!(
                        db.run_state(&id)?.unwrap().retry.unwrap().attempt,
                        attempt,
                        "repeat settlement must not reserve twice"
                    );
                } else {
                    assert_eq!(current.status, RunStatus::Failed);
                }
                Ok(())
            })
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn expired_provider_retry_during_settlement_keeps_the_expiry_outcome() {
    let (_root, service, _chat, run, _workspace) = fixture().await;
    service
        .store
        .transaction(move |db| {
            let cause = crate::run_retry::Cause::classify(
                &json!({"code": "server_error", "message": "Provider unavailable"}),
            )
            .unwrap();
            let retry = crate::run_retry::Retry::next(
                None,
                cause.clone(),
                now() - crate::run_retry::MAX_RECOVERY_MS - 1,
            )
            .unwrap();
            db.change_run(
                &run,
                crate::runs::RunChanges {
                    status: Some(RunStatus::Queued),
                    session_id: Some(Some("session".into())),
                    retry: Some(Some(retry)),
                    ..Default::default()
                },
            )?;
            let saved = RunCheckpoint::from_value(json!({
                "prepared": {"backend": "native", "isolated": true},
                "runnerId": "fenced-attempt",
                "remainingMs": null,
                "providerFailed": true,
                "lastError": "Provider unavailable",
                "retryCause": cause,
            }))?;
            saved.store_in(db, &run)?;
            assert!(!finish_recovery(db, &run, Some(&saved))?);
            let current = db.run_state(&run)?.unwrap();
            assert_eq!(current.status, RunStatus::Failed);
            assert!(current.error.unwrap().contains("10-minute limit"));
            assert!(current.retry.unwrap().next_attempt_at.is_none());
            Ok(())
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn confirmed_terminal_provider_failure_never_becomes_success_from_controller_zero() {
    let (_root, service, _chat, run, _workspace) = fixture().await;
    for code in [None, Some(0)] {
        let id = run.clone();
        service
            .store
            .transaction(move |db| {
                db.change_run(
                    &id,
                    crate::runs::RunChanges {
                        status: Some(RunStatus::Queued),
                        ..Default::default()
                    },
                )?;
                let saved = RunCheckpoint::from_value(json!({
                    "prepared":{"backend":"native","isolated":true},"runnerId":"same-attempt",
                    "remainingMs":null,"terminalCode":code,"providerFailed":true,
                    "lastError":"Permission denied","retryCause":null
                }))?;
                saved.store_in(db, &id)?;
                assert!(!finish_recovery(db, &id, Some(&saved))?);
                assert_eq!(db.run_state(&id)?.unwrap().status, RunStatus::Failed);
                assert_eq!(
                    RunCheckpoint::load_in(db, &id)?
                        .unwrap()
                        .controller_recoveries(),
                    0
                );
                Ok(())
            })
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn interrupted_observations_keep_a_durable_per_attempt_limit() {
    use axum::{Router, extract::Request, response::IntoResponse, routing::any};
    use std::sync::atomic::{AtomicUsize, Ordering};
    let entered = Arc::new(tokio::sync::Notify::new());
    let reads = Arc::new(AtomicUsize::new(0));
    let (seen, count) = (entered.clone(), reads.clone());
    let app = Router::new().fallback(any(move |request: Request| {
        let (seen, count) = (seen.clone(), count.clone());
        async move {
            if request.method() == "DELETE" {
                return axum::http::StatusCode::NO_CONTENT.into_response();
            }
            count.fetch_add(1, Ordering::SeqCst);
            seen.notify_one();
            std::future::pending::<axum::response::Response>().await
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let (_root, service, _chat, run, _workspace) =
        fixture_with_runner(format!("http://{address}")).await;
    let saved = RunCheckpoint::from_value(json!({
        "prepared": { "backend": "native", "isolated": true },
        "runnerId": "old-attempt", "completed": true, "controllerRecoveries": 3,
        "remainingMs": null,
    }))
    .unwrap();
    saved.store(&service.store, &run).await.unwrap();
    let (s, id) = (service.clone(), run.clone());
    let task = tokio::spawn(async move { recover(&s, &s.store.run(&id).await.unwrap()).await });
    entered.notified().await;
    task.abort();
    let _ = task.await;
    let saved = RunCheckpoint::load(&service.store, &run)
        .await
        .unwrap()
        .unwrap();
    assert!(saved.terminal_code.is_none());
    let observation = saved.controller_observation.unwrap();
    assert_eq!(observation.requests, 1);
    assert!(observation.in_flight);
    assert!(
        !recover(&service, &service.store.run(&run).await.unwrap())
            .await
            .unwrap()
    );
    assert_eq!(reads.load(Ordering::SeqCst), 1);
    assert_eq!(
        service.store.run_state(&run).await.unwrap().status,
        RunStatus::Failed
    );
    service.shutdown.cancel();
    server.abort();
}

#[tokio::test]
async fn recovery_deadline_stops_further_controller_observations() {
    use axum::{Router, extract::Request, response::IntoResponse, routing::any};
    use std::sync::atomic::{AtomicUsize, Ordering};
    let reads = Arc::new(AtomicUsize::new(0));
    let count = reads.clone();
    let app = Router::new().fallback(any(move |request: Request| {
        let count = count.clone();
        async move {
            if request.method() == "DELETE" {
                return axum::http::StatusCode::NO_CONTENT.into_response();
            }
            if request.uri().path().ends_with("/administrative-pause") {
                return axum::http::StatusCode::NOT_FOUND.into_response();
            }
            count.fetch_add(1, Ordering::SeqCst);
            std::future::pending::<axum::response::Response>().await
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let (_root, service, _chat, run, _workspace) =
        fixture_with_runner(format!("http://{address}")).await;
    let saved = RunCheckpoint::from_value(json!({
        "prepared": { "backend": "native", "isolated": true }, "runnerId": "old-attempt",
        "completed": false, "remainingMs": 200,
    }))
    .unwrap();
    saved.store(&service.store, &run).await.unwrap();
    assert!(
        !recover(&service, &service.store.run(&run).await.unwrap())
            .await
            .unwrap()
    );
    // The deadline can expire before the first request is sent; either way
    // it must prevent a second observation.
    assert!(reads.load(Ordering::SeqCst) <= 1);
    let state = service.store.run_state(&run).await.unwrap();
    assert_eq!(state.status, RunStatus::Failed);
    assert_eq!(state.error.as_deref(), Some("Run exceeded its time limit."));
    service.shutdown.cancel();
    server.abort();
}

#[tokio::test]
async fn expired_recovery_deadline_never_observes_controller() {
    use axum::{Router, extract::Request, response::IntoResponse, routing::any};
    use std::sync::atomic::{AtomicUsize, Ordering};
    let reads = Arc::new(AtomicUsize::new(0));
    let pause_reads = Arc::new(AtomicUsize::new(0));
    let count = reads.clone();
    let pause_count = pause_reads.clone();
    let app = Router::new().fallback(any(move |request: Request| {
        let count = count.clone();
        let pause_count = pause_count.clone();
        async move {
            if request.method() == "DELETE" {
                return axum::http::StatusCode::NO_CONTENT.into_response();
            }
            if request.method() == "GET"
                && request.uri().path() == "/runs/old-attempt/administrative-pause"
            {
                pause_count.fetch_add(1, Ordering::SeqCst);
                return axum::http::StatusCode::NOT_FOUND.into_response();
            }
            count.fetch_add(1, Ordering::SeqCst);
            std::future::pending::<axum::response::Response>().await
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let (_root, service, _chat, run, _workspace) =
        fixture_with_runner(format!("http://{address}")).await;
    let mut saved = RunCheckpoint::from_value(json!({
        "prepared": { "backend": "native", "isolated": true }, "runnerId": "old-attempt",
        "completed": false, "remainingMs": 200,
    }))
    .unwrap();
    saved.recovery_deadline_ms = Some(now() - 1);
    saved.store(&service.store, &run).await.unwrap();
    assert!(
        !recover(&service, &service.store.run(&run).await.unwrap())
            .await
            .unwrap()
    );
    assert!(pause_reads.load(Ordering::SeqCst) > 0);
    assert_eq!(reads.load(Ordering::SeqCst), 0);
    let state = service.store.run_state(&run).await.unwrap();
    assert_eq!(state.status, RunStatus::Failed);
    assert_eq!(state.error.as_deref(), Some("Run exceeded its time limit."));
    service.shutdown.cancel();
    server.abort();
}

#[tokio::test]
async fn recovery_pause_counter_is_durable_monotone_and_attempt_scoped() {
    use axum::{Router, response::IntoResponse, routing::any};
    use std::sync::atomic::{AtomicU64, Ordering};
    let counter = Arc::new(AtomicU64::new(100));
    let value = counter.clone();
    let app = Router::new().fallback(any(move || {
        let value = value.clone();
        async move {
            let total = value.load(Ordering::SeqCst);
            if total == u64::MAX {
                return axum::http::StatusCode::SERVICE_UNAVAILABLE.into_response();
            }
            axum::Json(json!({ "administrativePausedMs": total })).into_response()
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let (_root, service, _chat, run, _workspace) =
        fixture_with_runner(format!("http://{address}")).await;
    let deadline = now() + 10_000;
    let mut saved = RunCheckpoint::from_value(json!({
        "prepared": { "backend": "native", "isolated": true },
        "runnerId": "old-attempt", "remainingMs": 10000,
        "recoveryDeadlineMs": deadline,
        "administrativePauseObservation": { "attempt": "old-attempt", "totalMs": 40 }
    }))
    .unwrap();
    saved.store(&service.store, &run).await.unwrap();
    for (total, expected) in [
        (100, deadline + 60),
        (100, deadline + 60),
        (50, deadline + 60),
        (u64::MAX, deadline + 60),
        (130, deadline + 90),
    ] {
        counter.store(total, Ordering::SeqCst);
        assert_eq!(
            refresh_recovery_pause(&service, &run, Some(&saved))
                .await
                .unwrap(),
            Some(expected)
        );
        saved = RunCheckpoint::load(&service.store, &run)
            .await
            .unwrap()
            .unwrap();
    }
    let stale = saved.clone();
    saved.runner_id = Some(Some("new-attempt".into()));
    saved.store(&service.store, &run).await.unwrap();
    counter.store(20, Ordering::SeqCst);
    assert_eq!(
        refresh_recovery_pause(&service, &run, Some(&stale))
            .await
            .unwrap(),
        None
    );
    assert_eq!(
        refresh_recovery_pause(&service, &run, Some(&saved))
            .await
            .unwrap(),
        Some(deadline + 110)
    );
    let reloaded = RunCheckpoint::load(&service.store, &run)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        refresh_recovery_pause(&service, &run, Some(&reloaded))
            .await
            .unwrap(),
        Some(deadline + 110)
    );
    service.shutdown.cancel();
    server.abort();
}

#[tokio::test]
async fn admin_pause_during_recovery_does_not_spend_budget() {
    use axum::{Router, extract::Request, response::IntoResponse, routing::any};
    let started = std::time::Instant::now();
    let app = Router::new().fallback(any(move |request: Request| async move {
        if request.uri().path().ends_with("/administrative-pause") {
            let paused_ms = started.elapsed().as_millis() as u64;
            return axum::Json(json!({ "administrativePausedMs": paused_ms })).into_response();
        }
        if request.uri().path().ends_with("/wait") {
            return std::future::pending::<axum::response::Response>().await;
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
        axum::http::StatusCode::NO_CONTENT.into_response()
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let (_root, service, _chat, run, _workspace) =
        fixture_with_runner(format!("http://{address}")).await;
    let saved = RunCheckpoint::from_value(json!({
        "prepared": { "backend": "native", "isolated": true }, "runnerId": "old-attempt",
        "completed": false, "remainingMs": 200,
        "administrativePauseObservation": { "attempt": "old-attempt", "totalMs": 0 },
    }))
    .unwrap();
    saved.store(&service.store, &run).await.unwrap();
    service
        .store
        .change_run(
            &run,
            crate::runs::RunChanges {
                session_id: Some(Some("saved-session".into())),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let ready = recover(&service, &service.store.run(&run).await.unwrap())
        .await
        .unwrap();
    let state = service.store.run_state(&run).await.unwrap();
    let final_checkpoint = RunCheckpoint::load(&service.store, &run).await.unwrap();
    assert!(ready, "state={state:?} checkpoint={final_checkpoint:?}");
    assert_eq!(state.status, RunStatus::Queued);
    service.shutdown.cancel();
    server.abort();
}

#[tokio::test]
async fn failed_fences_spend_the_same_durable_deadline() {
    use axum::{Router, routing::any};
    use std::sync::atomic::{AtomicBool, Ordering};
    let allow = Arc::new(AtomicBool::new(false));
    let enabled = allow.clone();
    let app = Router::new().fallback(any(move || {
        let enabled = enabled.clone();
        async move {
            tokio::time::sleep(Duration::from_millis(200)).await;
            if enabled.load(Ordering::SeqCst) {
                axum::http::StatusCode::NO_CONTENT
            } else {
                axum::http::StatusCode::SERVICE_UNAVAILABLE
            }
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let (_root, service, _chat, run, _workspace) =
        fixture_with_runner(format!("http://{address}")).await;
    let saved = RunCheckpoint::from_value(json!({
        "prepared": { "backend": "native", "isolated": true },
        "runnerId": "old-attempt", "completed": true, "terminalCode": 0,
        "remainingMs": 350,
    }))
    .unwrap();
    saved.store(&service.store, &run).await.unwrap();
    let mut previous = 350;
    for index in 0..2 {
        assert!(
            recover(&service, &service.store.run(&run).await.unwrap())
                .await
                .is_err()
        );
        let saved = RunCheckpoint::load(&service.store, &run)
            .await
            .unwrap()
            .unwrap();
        let remaining = saved.remaining_ms.flatten().unwrap();
        assert!(remaining <= previous && (index != 0 || remaining < previous));
        assert!(saved.recovery_deadline_ms.is_some());
        assert_eq!(
            service.store.run_state(&run).await.unwrap().status,
            RunStatus::Queued
        );
        previous = remaining;
    }
    assert_eq!(previous, 0);
    allow.store(true, Ordering::SeqCst);
    assert!(
        !recover(&service, &service.store.run(&run).await.unwrap())
            .await
            .unwrap()
    );
    let state = service.store.run_state(&run).await.unwrap();
    assert_eq!(state.status, RunStatus::Failed);
    assert_eq!(state.error.as_deref(), Some("Run exceeded its time limit."));
    service.shutdown.cancel();
    server.abort();
}

#[tokio::test]
async fn recovery_fences_completed_failed_turn_before_pending_move() {
    use axum::{Router, routing::any};
    let app = Router::new().fallback(any(|| async { axum::http::StatusCode::NO_CONTENT }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let (_root, service, _chat, run, _workspace) =
        fixture_with_runner(format!("http://{address}")).await;
    for (terminal_code, idle, cancelled) in [
        (70, false, false),
        (70, true, false),
        (0, false, false),
        (70, true, true),
        (0, true, true),
    ] {
        let saved = RunCheckpoint::from_value(json!({
            "prepared": { "backend": "native", "isolated": true },
            "runnerId": "old-attempt", "completed": true, "terminalCode": terminal_code,
            "controllerRecoveries": 3, "remainingMs": null,
        }))
        .unwrap();
        saved.store(&service.store, &run).await.unwrap();
        service
            .store
            .change_run(
                &run,
                crate::runs::RunChanges {
                    status: Some(RunStatus::Queued),
                    cancel_requested_at: Some(cancelled.then(now)),
                    placement: Some(Box::new(crate::nodes::run_placement::PlacementChanges {
                        move_request: Some(Some(
                            serde_json::from_value(json!({
                                "nodeId": "destination", "reservation": "pending-destination",
                                "idle": idle, "automatic": false,
                                "resources": { "cpu": 1, "memoryMiB": 128, "diskMiB": 128 },
                            }))
                            .unwrap(),
                        )),
                        ..Default::default()
                    })),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert!(
            !recover(&service, &service.store.run(&run).await.unwrap())
                .await
                .unwrap()
        );
        let state = service.store.run_state(&run).await.unwrap();
        assert_eq!(
            state.status,
            if cancelled {
                RunStatus::Cancelled
            } else if terminal_code == 0 {
                RunStatus::Succeeded
            } else {
                RunStatus::Failed
            }
        );
        let current = RunCheckpoint::load(&service.store, &run)
            .await
            .unwrap()
            .unwrap();
        assert!(current.completed());
        assert_eq!(current.terminal_code(), Some(terminal_code));
    }
    service.shutdown.cancel();
    server.abort();
}

#[tokio::test]
async fn failed_observation_commit_does_not_repeat_network_reads() {
    use axum::{Router, extract::Request, response::IntoResponse, routing::any};
    use std::sync::atomic::{AtomicUsize, Ordering};
    let reads = Arc::new(AtomicUsize::new(0));
    let seen = reads.clone();
    let app = Router::new().fallback(any(move |request: Request| {
        let seen = seen.clone();
        async move {
            if request.method() == "DELETE" {
                return axum::http::StatusCode::NO_CONTENT.into_response();
            }
            seen.fetch_add(1, Ordering::SeqCst);
            axum::Json(json!({ "StatusCode": 0 })).into_response()
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let (_root, service, _chat, run, _workspace) =
        fixture_with_runner(format!("http://{address}")).await;
    let saved = RunCheckpoint::from_value(json!({
        "prepared": { "backend": "native", "isolated": true },
        "runnerId": "old-attempt", "completed": true, "controllerRecoveries": 3,
        "remainingMs": null,
    }))
    .unwrap();
    saved.store(&service.store, &run).await.unwrap();
    service.store.write(|db| {
        db.0.execute_batch("CREATE TRIGGER refuse_checkpoint BEFORE UPDATE ON run_checkpoints WHEN json_extract(OLD.state, '$.controllerObservation.requests') >= 1 BEGIN SELECT RAISE(ABORT, 'fixture write refused'); END")?;
        Ok(())
    }).await.unwrap();
    for _ in 0..2 {
        let _ = recover(&service, &service.store.run(&run).await.unwrap()).await;
    }
    service
        .store
        .write(|db| {
            db.0.execute_batch("DROP TRIGGER refuse_checkpoint")?;
            Ok(())
        })
        .await
        .unwrap();
    assert!(
        !recover(&service, &service.store.run(&run).await.unwrap())
            .await
            .unwrap()
    );
    assert_eq!(
        reads.load(Ordering::SeqCst),
        1,
        "a refused write must stop further observations"
    );
    service.shutdown.cancel();
    server.abort();
}

#[tokio::test]
async fn shutdown_during_recovery_observation_preserves_pending_result() {
    use axum::{Router, routing::any};
    let entered = Arc::new(tokio::sync::Notify::new());
    let seen = entered.clone();
    let app = Router::new().fallback(any(move || {
        let seen = seen.clone();
        async move {
            seen.notify_one();
            std::future::pending::<()>().await;
            axum::http::StatusCode::OK
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let (_root, service, _chat, run, _workspace) =
        fixture_with_runner(format!("http://{address}")).await;
    let saved = RunCheckpoint::from_value(json!({
        "prepared": { "backend": "native", "isolated": true },
        "runnerId": "old-attempt", "completed": true, "controllerRecoveries": 3,
        "remainingMs": null,
    }))
    .unwrap();
    saved.store(&service.store, &run).await.unwrap();
    let (s, id) = (service.clone(), run.clone());
    let task = tokio::spawn(async move { recover(&s, &s.store.run(&id).await.unwrap()).await });
    entered.notified().await;
    service.shutdown.cancel();
    let _ = task.await.unwrap();
    let saved = RunCheckpoint::load(&service.store, &run)
        .await
        .unwrap()
        .unwrap();
    assert!(
        saved.terminal_code.is_none(),
        "shutdown must leave observation pending"
    );
    let observation = saved.controller_observation.unwrap();
    assert_eq!(observation.attempt, "old-attempt");
    assert_eq!(observation.requests, 1);
    assert!(!observation.in_flight);
    server.abort();
}

#[tokio::test]
async fn scheduled_provider_retries_do_not_spend_controller_recovery_reservations() {
    let (_root, service, _chat, run, _workspace) = fixture().await;
    service.store.transaction(move |db| {
        let cause = crate::run_retry::Cause::classify(&json!({"code":"request_timeout","message":"fixture provider timeout"})).unwrap();
        let mut retry = None;
        for attempt in 1..=crate::run_retry::MAX_ATTEMPTS {
            retry = crate::run_retry::Retry::next(retry.as_ref(), cause.clone(), now());
            assert_eq!(retry.as_ref().unwrap().attempt, attempt);
            db.change_run(&run, crate::runs::RunChanges { retry: Some(retry.clone()), ..Default::default() })?;
            let saved = RunCheckpoint::from_value(json!({
                "prepared": {"backend":"native","isolated":true}, "runnerId":format!("attempt-{attempt}"),
                "remainingMs":null,"completed":false,"terminalCode":1,"controllerRecoveries":3,
                "providerFailureAttempt":format!("attempt-{attempt}"),
                "providerRetryReservation":format!("attempt-{attempt}"),
                "retryCause":cause,
            }))?;
            saved.store_in(db, &run)?;
            assert!(finish_recovery(db, &run, Some(&saved))?);
            let saved = RunCheckpoint::load_in(db, &run)?.unwrap();
            assert_eq!(saved.controller_recoveries(), 3);
            assert!(!saved.has_controller_recovery_reservation());
        }
        Ok(())
    }).await.unwrap();
}

#[test]
fn recovery_observation_is_capped_by_the_remaining_execution_deadline() {
    assert_eq!(recovery_observation_limit(None), Duration::from_secs(5));
    assert_eq!(recovery_observation_limit(Some(now() - 1)), Duration::ZERO);
    assert!(recovery_observation_limit(Some(now() + 100)) <= Duration::from_millis(100));
}

#[tokio::test]
async fn cancellation_interrupts_a_pending_scheduler_observation() {
    use axum::{Router, routing::any};
    let entered = Arc::new(tokio::sync::Notify::new());
    let app = Router::new().fallback(any({
        let entered = entered.clone();
        move || {
            let entered = entered.clone();
            async move {
                entered.notify_one();
                std::future::pending::<axum::http::StatusCode>().await
            }
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let (_root, service, _chat, run, _workspace) =
        fixture_with_runner(format!("http://{address}")).await;
    RunCheckpoint::from_value(json!({
        "prepared":{"backend":"native","isolated":true},"runnerId":"old-attempt","remainingMs":null
    }))
    .unwrap()
    .store(&service.store, &run)
    .await
    .unwrap();
    let (s, id) = (service.clone(), run.clone());
    let task = tokio::spawn(async move { recover(&s, &s.store.run(&id).await.unwrap()).await });
    tokio::time::timeout(Duration::from_secs(3), entered.notified())
        .await
        .unwrap();
    service
        .store
        .change_run(
            &run,
            crate::runs::RunChanges {
                cancel_requested_at: Some(Some(now())),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    // Fencing may fail against this deliberately silent fake controller. We
    // assert the pending observation itself was released promptly.
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let saved = RunCheckpoint::load(&service.store, &run)
                .await
                .unwrap()
                .unwrap();
            if saved
                .controller_observation
                .as_ref()
                .is_some_and(|observation| !observation.in_flight)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("cancellation must interrupt the read before its five-second deadline");
    task.abort();
    let _ = task.await;
    server.abort();
}

#[tokio::test]
async fn recovered_interruption_respects_controller_recovery_ceiling() {
    let (_root, service, _chat, run, _workspace) = fixture().await;
    for recoveries in [2, 3] {
        for code in [75, 143] {
            for (cancel, remaining) in [(false, None), (true, None), (false, Some(0))] {
                let id = run.clone();
                service
                    .store
                    .transaction(move |db| {
                        db.change_run(
                            &id,
                            crate::runs::RunChanges {
                                status: Some(RunStatus::Queued),
                                cancel_requested_at: Some(cancel.then(now)),
                                wakeup: Some(Some(serde_json::from_value(json!({
                                    "id": "old",
                                    "messageId": null,
                                    "reason": "Waiting",
                                    "seconds": 3600,
                                    "createdAt": now(),
                                    "wakeAt": now() + 3_600_000,
                                }))?)),
                                ..Default::default()
                            },
                        )?;
                        let saved = RunCheckpoint::from_value(json!({
                            "prepared": { "backend": "native", "isolated": true },
                            "runnerId": "old-attempt", "completed": false,
                            "terminalCode": code, "controllerRecoveries": recoveries,
                            "remainingMs": remaining,
                        }))?;
                        saved.store_in(db, &id)?;
                        let may_launch = finish_recovery(db, &id, Some(&saved))?;
                        let persisted = RunCheckpoint::load_in(db, &id)?.unwrap();
                        if cancel || remaining == Some(0) || recoveries == 3 {
                            assert_eq!(persisted.controller_recoveries(), recoveries);
                            assert!(!persisted.has_controller_recovery_reservation());
                        } else {
                            assert_eq!(persisted.controller_recoveries(), recoveries + 1);
                            assert!(persisted.has_controller_recovery_reservation());
                        }
                        assert_eq!(
                            may_launch,
                            recoveries < 3 && !cancel && remaining.is_none(),
                            "code {code}, recoveries {recoveries}"
                        );
                        let current = db.run_state(&id)?.unwrap();
                        if cancel {
                            assert_eq!(current.status, RunStatus::Cancelled);
                        } else if remaining == Some(0) {
                            assert_eq!(current.status, RunStatus::Failed);
                            assert_eq!(
                                current.error.as_deref(),
                                Some("Run exceeded its time limit.")
                            );
                        }
                        if !may_launch {
                            assert!(db.run(&id)?.unwrap()["wakeup"].is_null());
                        }
                        Ok(())
                    })
                    .await
                    .unwrap();
            }
        }
    }
    service.shutdown.cancel();
}

#[tokio::test]
async fn clean_controller_exit_without_provider_completion_fails_on_restart() {
    let (_root, service, _chat, run, _workspace) = fixture().await;
    for backend in ["native", "firecracker", "shared"] {
        for completed in [false, true] {
            let id = run.clone();
            service
                .store
                .transaction(move |db| {
                    db.change_run(
                        &id,
                        crate::runs::RunChanges {
                            status: Some(RunStatus::Queued),
                            cancel_requested_at: Some(None),
                            ..Default::default()
                        },
                    )?;
                    let saved = RunCheckpoint::from_value(json!({
                        "prepared": { "backend": backend, "isolated": true },
                        "runnerId": "attempt", "completed": completed,
                        "terminalCode": 0, "remainingMs": null,
                    }))?;
                    assert!(!finish_recovery(db, &id, Some(&saved))?);
                    let state = db.run_state(&id)?.unwrap();
                    assert_eq!(
                        state.status,
                        if completed || backend == "shared" {
                            RunStatus::Succeeded
                        } else {
                            RunStatus::Failed
                        }
                    );
                    if !completed && backend != "shared" {
                        assert!(state.error.unwrap().contains("completion was recorded"));
                    }
                    Ok(())
                })
                .await
                .unwrap();
        }
    }
    service.shutdown.cancel();
}

#[tokio::test]
async fn provider_retry_authorization_is_bound_to_attempt_and_confirmed_cause() {
    let (_root, service, _chat, run, _workspace) = fixture().await;
    let cause = crate::run_retry::Cause::classify(&json!({
        "code": "server_overloaded", "message": "Provider busy"
    }))
    .unwrap();
    for (same_attempt, same_cause, pending_turn, cancelled, expired) in [
        (true, true, false, false, false),
        (false, true, false, false, false),
        (true, false, false, false, false),
        (true, true, true, false, false),
        (true, true, false, true, false),
        (true, true, false, false, true),
    ] {
        let (id, cause) = (run.clone(), cause.clone());
        service
            .store
            .transaction(move |db| {
                let retry = crate::run_retry::Retry::next(None, cause.clone(), now()).unwrap();
                db.change_run(
                    &id,
                    crate::runs::RunChanges {
                        status: Some(RunStatus::Queued),
                        retry: Some(Some(retry)),
                        cancel_requested_at: Some(cancelled.then(now)),
                        placement: Some(Box::new(crate::nodes::run_placement::PlacementChanges {
                            native_mode_interruption: Some(None),
                            move_request: Some(None),
                            ..Default::default()
                        })),
                        ..Default::default()
                    },
                )?;
                let mut saved = RunCheckpoint::from_value(json!({
                    "prepared": { "backend": "firecracker", "isolated": true },
                    "runnerId": if same_attempt { "old" } else { "replacement" },
                    "completed": false, "terminalCode": 1,
                    "remainingMs": if expired { Some(0) } else { None },
                    "controllerRecoveries": 3, "providerRetryReservation": "old",
                    "turnPending": pending_turn,
                }))?;
                saved.retry_cause = Some(if same_cause { Some(cause) } else { None });
                saved.store_in(db, &id)?;
                let may_launch = finish_recovery(db, &id, Some(&saved))?;
                assert_eq!(
                    may_launch,
                    same_attempt && same_cause && !pending_turn && !cancelled && !expired
                );
                if may_launch {
                    let durable = RunCheckpoint::load_in(db, &id)?.unwrap();
                    assert_eq!(durable.controller_recoveries(), 3);
                    assert!(durable.has_provider_retry_reservation());
                    assert!(finish_recovery(db, &id, Some(&durable))?);
                    assert_eq!(
                        RunCheckpoint::load_in(db, &id)?
                            .unwrap()
                            .controller_recoveries(),
                        3
                    );
                }
                Ok(())
            })
            .await
            .unwrap();
    }
    service.shutdown.cancel();
}

#[tokio::test]
async fn legacy_move_metadata_cannot_bypass_the_controller_recovery_ceiling() {
    let (_root, service, _chat, run, _workspace) = fixture().await;
    for (movement, same_attempt) in [(true, true), (false, true), (true, false)] {
        let id = run.clone();
        service
            .store
            .transaction(move |db| {
                db.change_run(
                    &id,
                    crate::runs::RunChanges {
                        status: Some(RunStatus::Queued),
                        cancel_requested_at: Some(None),
                        placement: Some(Box::new(crate::nodes::run_placement::PlacementChanges {
                            move_request: Some(movement.then(|| {
                                serde_json::from_value(json!({
                                    "nodeId": "destination",
                                    "reservation": "pending-destination",
                                    "resources": {
                                        "cpu": 1,
                                        "memoryMiB": 128,
                                        "diskMiB": 128
                                    },
                                }))
                                .unwrap()
                            })),
                            ..Default::default()
                        })),
                        ..Default::default()
                    },
                )?;
                let saved = RunCheckpoint::from_value(json!({
                    "prepared": { "backend": "firecracker", "isolated": true },
                    "runnerId": if same_attempt { "attempt" } else { "replacement" },
                    "completed": false, "terminalCode": 75, "remainingMs": null,
                    "controllerRecoveries": 4, "controllerRecoveryReservation": "attempt",
                }))?;
                saved.store_in(db, &id)?;
                // Node changes expire their environment. A legacy move flag
                // cannot fund another retry of an exhausted old controller.
                assert!(!finish_recovery(db, &id, Some(&saved))?);
                let durable = RunCheckpoint::load_in(db, &id)?.unwrap();
                assert_eq!(durable.controller_recoveries(), 4);
                Ok(())
            })
            .await
            .unwrap();
    }
    service.shutdown.cancel();
}

#[tokio::test]
async fn transferred_move_authorization_is_attempt_scoped_and_consumed() {
    let (_root, service, _chat, run, _workspace) = fixture().await;
    for (replace, consume, cancelled, expired) in [
        (false, false, false, false),
        (true, false, false, false),
        (false, true, false, false),
        (false, false, true, false),
        (false, false, false, true),
    ] {
        let id = run.clone();
        service
            .store
            .transaction(move |db| {
                db.change_run(
                    &id,
                    crate::runs::RunChanges {
                        status: Some(RunStatus::Queued),
                        cancel_requested_at: Some(cancelled.then(now)),
                        placement: Some(Box::new(crate::nodes::run_placement::PlacementChanges {
                            move_request: Some(None),
                            ..Default::default()
                        })),
                        ..Default::default()
                    },
                )?;
                let mut saved = RunCheckpoint::from_value(json!({
                    "prepared": { "backend": "firecracker", "isolated": true },
                    "runnerId": "attempt", "completed": false, "terminalCode": 75,
                    "remainingMs": if expired { Some(0) } else { None },
                    "controllerRecoveries": 4, "controllerRecoveryReservation": "attempt",
                    "moveRecoveryReservation": "attempt",
                }))?;
                if replace {
                    saved.runner_id = Some(Some("replacement".into()));
                }
                if consume {
                    saved.bind_paid_admission("new-attempt", &db.run_state(&id)?.unwrap());
                }
                saved.store_in(db, &id)?;
                assert_eq!(
                    finish_recovery(db, &id, Some(&saved))?,
                    !replace && !consume && !cancelled && !expired
                );
                assert_eq!(
                    RunCheckpoint::load_in(db, &id)?
                        .unwrap()
                        .controller_recoveries(),
                    4
                );
                Ok(())
            })
            .await
            .unwrap();
    }
    service.shutdown.cancel();
}

#[tokio::test]
async fn paid_native_mode_recovery_can_settle_at_and_above_controller_ceiling() {
    let (_root, service, _chat, run, _workspace) = fixture().await;
    for count in [3, 4, 5] {
        for (mode, cancelled, expired) in [
            (true, false, false),
            (false, false, false),
            (true, true, false),
            (true, false, true),
        ] {
            let id = run.clone();
            service
                .store
                .transaction(move |db| {
                    db.change_run(
                        &id,
                        crate::runs::RunChanges {
                            status: Some(RunStatus::Queued),
                            session_id: Some(None),
                            cancel_requested_at: Some(cancelled.then(now)),
                            placement: Some(Box::new(
                                crate::nodes::run_placement::PlacementChanges {
                                    native_mode_interruption: Some(mode.then(crate::config::id)),
                                    ..Default::default()
                                },
                            )),
                            ..Default::default()
                        },
                    )?;

                    let saved = RunCheckpoint::from_value(json!({
                        "prepared": {
                            "backend": "native",
                            "isolated": true,
                        },
                        "runnerId": "attempt",
                        "completed": false,
                        "terminalCode": 75,
                        "remainingMs": if expired { Some(0) } else { None },
                        "controllerRecoveries": count,
                        "controllerRecoveryReservation": "attempt",
                    }))?;
                    saved.store_in(db, &id)?;

                    let admitted = finish_recovery(db, &id, Some(&saved))?;
                    let should_admit = (mode || count == 3) && !cancelled && !expired;
                    assert_eq!(
                        admitted, should_admit,
                        "count={count}, mode={mode}, cancelled={cancelled}, expired={expired}"
                    );

                    let current = db.run_state(&id)?.unwrap();
                    if cancelled {
                        assert_eq!(current.status, RunStatus::Cancelled);
                    }
                    if expired && !cancelled {
                        assert_eq!(current.status, RunStatus::Failed);
                    }
                    if admitted {
                        assert!(
                            RunCheckpoint::load_in(db, &id)?
                                .unwrap()
                                .has_controller_recovery_reservation()
                        );
                    }
                    Ok(())
                })
                .await
                .unwrap();
        }
    }
    service.shutdown.cancel();
}

#[tokio::test]
async fn recovery_reservation_is_paid_once_and_bound_to_attempt() {
    let (_root, service, _chat, run, _workspace) = fixture().await;
    for count in 0..=3 {
        let id = run.clone();
        service
            .store
            .transaction(move |db| {
                db.change_run(
                    &id,
                    crate::runs::RunChanges {
                        status: Some(RunStatus::Queued),
                        cancel_requested_at: Some(None),
                        ..Default::default()
                    },
                )?;
                let saved = RunCheckpoint::from_value(json!({
                    "prepared": { "backend": "native", "isolated": true },
                    "runnerId": "attempt", "completed": false,
                    "terminalCode": 75, "remainingMs": null,
                    "controllerRecoveries": count,
                }))?;
                saved.store_in(db, &id)?;
                let allowed = finish_recovery(db, &id, Some(&saved))?;
                assert_eq!(allowed, count < 3);
                if allowed {
                    let paid = RunCheckpoint::load_in(db, &id)?.unwrap();
                    assert_eq!(paid.controller_recoveries(), count + 1);
                    assert!(paid.has_controller_recovery_reservation());
                    assert!(finish_recovery(db, &id, Some(&paid))?);
                    let paid_again = RunCheckpoint::load_in(db, &id)?.unwrap();
                    assert_eq!(paid_again.controller_recoveries(), count + 1);
                    let mut replacement = paid_again;
                    replacement.runner_id = Some(Some("replacement".into()));
                    assert!(!replacement.has_controller_recovery_reservation());
                    if count == 2 {
                        assert!(!finish_recovery(db, &id, Some(&replacement))?);
                    }
                }
                Ok(())
            })
            .await
            .unwrap();
    }
    service.shutdown.cancel();
}

#[tokio::test]
async fn failed_flush_recovery_is_terminal_only_for_its_attempt() {
    let (_root, service, _chat, run, _workspace) = fixture().await;
    for (same, completed, cancelled) in [
        (true, false, false),
        (true, true, false),
        (false, false, false),
        (true, false, true),
    ] {
        let id = run.clone();
        service
            .store
            .transaction(move |db| {
                db.change_run(
                    &id,
                    crate::runs::RunChanges {
                        status: Some(RunStatus::Queued),
                        cancel_requested_at: Some(cancelled.then(now)),
                        retry: Some(None),
                        ..Default::default()
                    },
                )?;
                let saved = RunCheckpoint::from_value(json!({
                    "prepared": { "backend": "native", "isolated": true }, "runnerId": "current",
                    "finalizationFailureAttempt": if same { "current" } else { "old" },
                    "completed": completed, "terminalCode": 70, "remainingMs": null,
                    "controllerRecoveries": 0,
                }))?;
                saved.store_in(db, &id)?;
                let admitted = finish_recovery(db, &id, Some(&saved))?;
                assert_eq!(admitted, !same && !completed && !cancelled);
                let state = db.run_state(&id)?.unwrap();
                if same {
                    assert_eq!(
                        state.status,
                        if cancelled {
                            RunStatus::Cancelled
                        } else {
                            RunStatus::Failed
                        }
                    );
                }
                Ok(())
            })
            .await
            .unwrap();
    }
    service.shutdown.cancel();
}

#[tokio::test]
async fn a_queued_fresh_turn_cannot_hide_the_old_controller_flush_failure() {
    use axum::{Json, Router, routing::delete};

    let app = Router::new().route(
        "/runs/old-attempt",
        delete(|| async { Json(json!({ "StatusCode": 70, "FinalizationFailed": true })) }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let (_root, service, _chat, run, _workspace) =
        fixture_with_runner(format!("http://{address}")).await;
    let checkpoint = service
        .store
        .transaction({
            let run = run.clone();
            move |db| {
                db.change_run(
                    &run,
                    crate::runs::RunChanges {
                        status: Some(RunStatus::Queued),
                        recovery_pending: Some(true),
                        ..Default::default()
                    },
                )?;
                let checkpoint = RunCheckpoint::from_value(json!({
                    "runnerId": "old-attempt", "turnPending": true,
                    "prepared": {"backend": "firecracker", "isolated": true}
                }))?;
                checkpoint.store_in(db, &run)?;
                Ok(checkpoint)
            }
        })
        .await
        .unwrap();
    let current = service.store.run(&run).await.unwrap();
    recovery::fence_attempt(&service, &current, Some(&checkpoint))
        .await
        .unwrap();
    let acknowledged = service
        .store
        .transaction({
            let run = run.clone();
            move |db| {
                let mut saved = RunCheckpoint::load_in(db, &run)?.unwrap();
                assert_eq!(
                    saved.finalization_failure_attempt.as_deref(),
                    Some("old-attempt")
                );
                assert!(!finish_recovery(db, &run, Some(&saved))?);
                assert_eq!(db.run_state(&run)?.unwrap().status, RunStatus::Failed);
                saved.acknowledge_finalization_observation();
                saved.terminal_code = Some(None);
                saved.store_in(db, &run)?;
                db.change_run(
                    &run,
                    crate::runs::RunChanges {
                        status: Some(RunStatus::Queued),
                        recovery_pending: Some(true),
                        error: Some(None),
                        ..Default::default()
                    },
                )?;
                Ok(saved)
            }
        })
        .await
        .unwrap();
    let current = service.store.run(&run).await.unwrap();
    recovery::fence_attempt(&service, &current, Some(&acknowledged))
        .await
        .unwrap();
    service
        .store
        .transaction(move |db| {
            let saved = RunCheckpoint::load_in(db, &run)?.unwrap();
            assert!(saved.finalization_failure_attempt.is_none());
            assert!(finish_recovery(db, &run, Some(&saved))?);
            assert_eq!(db.run_state(&run)?.unwrap().status, RunStatus::Queued);
            Ok(())
        })
        .await
        .unwrap();
    server.abort();
}

#[tokio::test]
async fn expiry_keeps_a_control_fault_received_only_by_the_background_observer() {
    use axum::{Json, Router, http::StatusCode, routing::post};

    let app = Router::new().route(
        "/runs/attempt/environment",
        post(|| async {
            (
                StatusCode::BAD_GATEWAY,
                Json(json!({ "code": "environment_invalid_control_frame" })),
            )
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let (_root, service, _chat, run, _workspace) =
        fixture_with_runner(format!("http://{address}")).await;
    let deadline = now() - 1;
    service.store.transaction({
        let run = run.clone();
        move |db| {
            db.change_run(&run, crate::runs::RunChanges { status: Some(RunStatus::Succeeded), ..Default::default() })?;
            let saved = RunCheckpoint::from_value(json!({
                "runnerId": "attempt", "completed": true,
                "heldEnvironment": {"attempt": "attempt", "generation": "generation", "expiresAt": deadline}
            }))?;
            saved.store_in(db, &run)?;
            db.0.execute("INSERT INTO background_jobs(request_id,run_id,attempt_id,payload_hash) VALUES('job',?,'attempt','hash')", [&run])?;
            Ok(())
        }
    }).await.unwrap();
    super::super::jobs::refresh_all(&service).await.unwrap();
    server.abort();
    service
        .store
        .transaction(move |db| {
            let mut saved = RunCheckpoint::load_in(db, &run)?.unwrap();
            assert!(saved.held_environment.take().is_some());
            super::super::held::settle_retirement(
                db,
                &run,
                &mut saved,
                super::super::held::Retirement::Deadline,
                deadline,
                Some(deadline),
            )?;
            assert_eq!(saved.settled().unwrap().status, RunStatus::Failed);
            assert_eq!(
                saved.settled().unwrap().error.as_deref(),
                Some("Execution controller failed to complete the control request.")
            );
            Ok(())
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn expiry_keeps_a_control_fault_delivered_late_only_while_its_turn_is_current() {
    use crate::background_jobs::{
        Outcome,
        protocol::{Event, Request},
    };
    use axum::{Json, Router, response::IntoResponse, routing::post};

    for replacement_queued in [false, true] {
        let entered = Arc::new(tokio::sync::Notify::new());
        let release = CancellationToken::new();
        let app = Router::new().route(
            "/runs/attempt/environment",
            post({
                let (entered, release) = (entered.clone(), release.clone());
                move |Json(request): Json<Request>| {
                    let (entered, release) = (entered.clone(), release.clone());
                    async move {
                        let Request::JobStatus { request_id, job_id } = request else {
                            panic!("unexpected request");
                        };
                        entered.notify_one();
                        release.cancelled().await;
                        Json(Event::JobStatus {
                            request_id,
                            job_id,
                            outcome: Outcome::Unknown {
                                message: "Fixture confidential detail".into(),
                            },
                        })
                        .into_response()
                    }
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let (_root, service, _chat, run, _workspace) =
            fixture_with_runner(format!("http://{address}")).await;
        let deadline = now() - 1;
        service.store.transaction({
            let run = run.clone();
            move |db| {
                db.0.execute("UPDATE runs SET trigger='chat' WHERE id=?", [&run])?;
                db.change_run(&run, crate::runs::RunChanges { status: Some(RunStatus::Succeeded), ..Default::default() })?;
                RunCheckpoint::from_value(json!({
                    "runnerId": "attempt", "completed": true,
                    "heldEnvironment": {"attempt": "attempt", "generation": "generation", "expiresAt": deadline}
                }))?.store_in(db, &run)
            }
        }).await.unwrap();
        let observed_run = service.store.run(&run).await.unwrap();
        let observer = tokio::spawn({
            let service = service.clone();
            async move {
                super::super::held::request_bound(
                    &service,
                    &observed_run,
                    Some("attempt"),
                    &Request::JobStatus {
                        request_id: "request".into(),
                        job_id: "job".into(),
                    },
                )
                .await
                .unwrap()
            }
        });
        tokio::time::timeout(Duration::from_secs(5), entered.notified())
            .await
            .unwrap();
        service
            .store
            .transaction({
                let run = run.clone();
                move |db| {
                    let mut saved = RunCheckpoint::load_in(db, &run)?.unwrap();
                    assert!(saved.held_environment.take().is_some());
                    super::super::held::settle_retirement(
                        db,
                        &run,
                        &mut saved,
                        super::super::held::Retirement::Deadline,
                        deadline,
                        Some(deadline),
                    )?;
                    saved.store_in(db, &run)?;
                    finish_recovery(db, &run, Some(&saved))?;
                    let settled = db.run_state(&run)?.unwrap();
                    assert_eq!(settled.status, RunStatus::Succeeded);
                    assert!(!settled.recovery_pending);
                    if replacement_queued {
                        // Chat preparation can queue the next turn before it gets a new runner.
                        saved.settled = None;
                        saved.retired_environment_deadline = None;
                        saved.turn_pending = Some(true);
                        saved.store_in(db, &run)?;
                        db.change_run(
                            &run,
                            crate::runs::RunChanges {
                                status: Some(RunStatus::Queued),
                                ..Default::default()
                            },
                        )?;
                    }
                    Ok(())
                }
            })
            .await
            .unwrap();
        release.cancel();
        assert!(matches!(observer.await.unwrap(), Event::JobStatus { .. }));
        let current = service.store.run_state(&run).await.unwrap();
        if replacement_queued {
            assert_eq!(current.status, RunStatus::Queued);
            assert!(current.error.is_none());
        } else {
            assert_eq!(current.status, RunStatus::Failed);
            assert_eq!(
                current.error.as_deref(),
                Some("Workload ownership is unknown; environment fencing is required.")
            );
        }
        server.abort();
    }
}

#[tokio::test]
async fn recovered_provider_failure_is_paid_once_and_attempt_scoped() {
    let (_root, service, _chat, run, _workspace) = fixture().await;
    for (interrupted, same, code, cancelled, expired, flush) in [
        (true, true, 1, false, false, false),
        (false, true, 1, false, false, false),
        (false, false, 1, false, false, false),
        (false, true, 70, false, false, false),
        (false, true, 1, true, false, false),
        (false, true, 1, false, true, false),
        (false, true, 1, false, false, true),
    ] {
        let id = run.clone();
        service.store.transaction(move |db| {
            db.change_run(&id, crate::runs::RunChanges {
                status: Some(RunStatus::Queued), session_id: Some(Some("session".into())),
                cancel_requested_at: Some(cancelled.then(now)), retry: Some(None),
                ..Default::default()
            })?;
            let cause = crate::run_retry::Cause::classify(&json!({ "code": "server_overloaded", "message": "Provider busy" })).unwrap();
            let mut saved = RunCheckpoint::from_value(json!({
                "prepared": { "backend": "firecracker", "isolated": true },
                "runnerId": "attempt", "providerFailureAttempt": if same { "attempt" } else { "old" },
                "completed": false, "terminalCode": code, "outputInterrupted": interrupted,
                "finalizationFailureAttempt": if flush { Some("attempt") } else { None },
                "remainingMs": if expired { Some(0) } else { None }, "controllerRecoveries": 3,
            }))?;
            saved.retry_cause = Some(Some(cause));
            saved.store_in(db, &id)?;
            assert!(!finish_recovery(db, &id, Some(&saved))?);
            let state = db.run_state(&id)?.unwrap();
            if same && code == 1 && !cancelled && !expired && !flush {
                assert_eq!(state.status, RunStatus::Queued);
                assert_eq!(state.retry.as_ref().unwrap().attempt, 1);
                let paid = RunCheckpoint::load_in(db, &id)?.unwrap();
                assert!(paid.has_provider_retry_reservation());
                assert_eq!(paid.controller_recoveries(), 3);
                assert!(finish_recovery(db, &id, Some(&paid))?);
                assert_eq!(db.run_state(&id)?.unwrap().retry.unwrap().attempt, 1);
            } else {
                assert_eq!(state.status, if cancelled { RunStatus::Cancelled } else { RunStatus::Failed });
                assert!(state.retry.is_none());
            }
            Ok(())
        }).await.unwrap();
    }
    service.shutdown.cancel();
}

#[tokio::test]
async fn intact_provider_failure_is_paid_after_restart_without_controller_charge() {
    use axum::{Json, Router, http::StatusCode, response::IntoResponse, routing::any};
    use std::sync::atomic::{AtomicUsize, Ordering};
    let waits = Arc::new(AtomicUsize::new(0));
    let fences = Arc::new(AtomicUsize::new(0));
    let app = Router::new().fallback(any({
        let (waits, fences) = (waits.clone(), fences.clone());
        move |request: axum::http::Request<axum::body::Body>| {
            let (waits, fences) = (waits.clone(), fences.clone());
            async move {
                if request.uri().path().ends_with("/wait") {
                    waits.fetch_add(1, Ordering::SeqCst);
                    return Json(json!({ "StatusCode": 1 })).into_response();
                }
                if request.method() == axum::http::Method::DELETE {
                    fences.fetch_add(1, Ordering::SeqCst);
                }
                StatusCode::NO_CONTENT.into_response()
            }
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    for count in [0, 3] {
        let (_root, service, _chat, run, _workspace) =
            fixture_with_runner(format!("http://{address}")).await;
        service
            .store
            .change_run(
                &run,
                crate::runs::RunChanges {
                    session_id: Some(Some("provider-session".into())),
                    recovery_pending: Some(true),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let saved = RunCheckpoint::from_value(json!({
            "prepared": { "backend": "firecracker", "isolated": true },
            "runnerId": "attempt", "completed": false, "remainingMs": null,
            "controllerRecoveries": count,
        }))
        .unwrap();
        saved.store(&service.store, &run).await.unwrap();
        let checkpoint =
            super::super::checkpoint::Checkpoint::new(service.store.clone(), run.clone(), saved);
        let cause = crate::run_retry::Cause::classify(
            &json!({ "code": "server_overloaded", "message": "Provider busy" }),
        )
        .unwrap();
        checkpoint
            .record_event(
                cairn_contracts::EventKind::TurnFailed,
                "Provider busy".into(),
                json!({ "type": "turn.failed" }),
                super::super::checkpoint::EventChanges {
                    run: None,
                    failure: Some(("Provider busy".into(), Some(cause), true)),
                    antigravity_quota_refusal: false,
                    antigravity_probe_failed: false,
                    completed: false,
                    progress_complete: true,
                    last_message: None,
                },
            )
            .await
            .unwrap();
        let before = RunCheckpoint::load(&service.store, &run)
            .await
            .unwrap()
            .unwrap();
        assert!(before.terminal_code.is_none());
        assert!(before.output_interrupted.is_none());
        assert!(before.provider_retry_reservation.is_none());
        assert!(
            !recover(&service, &service.store.run(&run).await.unwrap())
                .await
                .unwrap()
        );
        let waiting = service.store.run_state(&run).await.unwrap();
        assert_eq!(waiting.status, RunStatus::Queued, "count={count}");
        let retry = waiting.retry.unwrap();
        assert_eq!(retry.attempt, 1);
        assert!(retry.next_attempt_at.is_some_and(|at| at > now()));
        let paid = RunCheckpoint::load(&service.store, &run)
            .await
            .unwrap()
            .unwrap();
        assert!(paid.has_provider_retry_reservation());
        assert_eq!(paid.controller_recoveries(), count);
        assert!(
            recover(&service, &service.store.run(&run).await.unwrap())
                .await
                .unwrap()
        );
        let again = service.store.run_state(&run).await.unwrap().retry.unwrap();
        assert_eq!(again.attempt, 1);
        assert_eq!(again.next_attempt_at, retry.next_attempt_at);
        assert_eq!(
            RunCheckpoint::load(&service.store, &run)
                .await
                .unwrap()
                .unwrap()
                .controller_recoveries(),
            count
        );
        service.shutdown.cancel();
    }
    assert_eq!(waits.load(Ordering::SeqCst), 2);
    assert!(fences.load(Ordering::SeqCst) >= 2);
    server.abort();
}

#[tokio::test]
async fn recovered_output_loss_fails_only_when_completion_is_missing() {
    let (_root, service, _chat, run, _workspace) = fixture().await;
    for completed in [false, true] {
        let id = run.clone();
        service
            .store
            .transaction(move |db| {
                db.change_run(
                    &id,
                    crate::runs::RunChanges {
                        status: Some(RunStatus::Queued),
                        cancel_requested_at: Some(None),
                        ..Default::default()
                    },
                )?;
                let saved = RunCheckpoint::from_value(json!({
                    "prepared": { "backend": "native", "isolated": true },
                    "runnerId": "attempt", "completed": completed,
                    "outputInterrupted": true, "terminalCode": 0, "remainingMs": null,
                }))?;
                assert!(!finish_recovery(db, &id, Some(&saved))?);
                let state = db.run_state(&id)?.unwrap();
                assert_eq!(
                    state.status,
                    if completed {
                        RunStatus::Succeeded
                    } else {
                        RunStatus::Failed
                    }
                );
                if !completed {
                    assert!(state.error.unwrap().contains("output was interrupted"));
                }
                Ok(())
            })
            .await
            .unwrap();
    }
    service.shutdown.cancel();
}

#[tokio::test]
async fn recovery_may_spend_a_reservation_already_charged_to_the_attempt() {
    let (_root, service, _chat, run, _workspace) = fixture().await;
    service
        .store
        .transaction(move |db| {
            let mut saved = RunCheckpoint::from_value(json!({
                "prepared": { "backend": "native", "isolated": true },
                "runnerId": "old-attempt", "completed": false, "terminalCode": 143,
                "controllerRecoveries": 3, "controllerRecoveryReservation": "old-attempt",
                "remainingMs": null,
            }))?;
            assert!(finish_recovery(db, &run, Some(&saved))?);
            saved.runner_id = Some(Some("replacement".into()));
            assert!(!finish_recovery(db, &run, Some(&saved))?);
            assert_eq!(db.run_state(&run)?.unwrap().status, RunStatus::Failed);
            Ok(())
        })
        .await
        .unwrap();
    service.shutdown.cancel();
}

#[tokio::test]
async fn failed_finalization_removes_typed_mission_wakeup() {
    let (_root, service, _chat, run, _workspace) = fixture().await;
    service
        .store
        .transaction(move |db| {
            let wakeup = serde_json::from_value(json!({
                "id": "wake",
                "messageId": null,
                "reason": "Waiting",
                "seconds": 3600,
                "createdAt": now(),
                "wakeAt": now() + 3_600_000,
            }))?;
            db.change_run(
                &run,
                crate::runs::RunChanges {
                    status: Some(RunStatus::Queued),
                    trigger: Some("manual".into()),
                    wakeup: Some(Some(wakeup)),
                    ..Default::default()
                },
            )?;
            let saved = RunCheckpoint::from_value(json!({
                "prepared": { "backend": "native", "isolated": true },
                "completed": true,
                "terminalCode": 70,
                "remainingMs": null,
            }))?;
            assert!(!finish_recovery(db, &run, Some(&saved))?);
            let record = db.run_record(&run)?.unwrap();
            assert_eq!(record.state.status, RunStatus::Failed);
            assert!(record.wakeup.is_none());
            Ok(())
        })
        .await
        .unwrap();
    service.shutdown.cancel();
}

#[tokio::test]
async fn managed_completed_recovery_requires_healthy_finalization_and_cancellation_wins() {
    let (_root, service, _chat, run, _workspace) = fixture().await;
    for backend in ["native", "firecracker"] {
        for code in [None, Some(0), Some(70), Some(75)] {
            for cancel in [false, true] {
                let run = run.clone();
                service.store.transaction(move |db| {
                    let saved = RunCheckpoint::from_value(json!({
                        "prepared": { "backend": backend, "isolated": true },
                        "completed": true, "terminalCode": code,
                        "lastError": if code == Some(70) { "Finalization failed: EIO/EROFS" } else { "" },
                        "controllerRecoveries": 3,
                    }))?;
                    db.change_run(&run, crate::runs::RunChanges {
                        status: Some(RunStatus::Queued), cancel_requested_at: Some(cancel.then(now)),
                        ..Default::default()
                    })?;
                    assert!(!finish_recovery(db, &run, Some(&saved))?);
                    let current = db.run_state(&run)?.unwrap();
                    let expected = if cancel { RunStatus::Cancelled }
                        else if code == Some(0) { RunStatus::Succeeded } else { RunStatus::Failed };
                    assert_eq!(current.status, expected, "{backend} {code:?}");
                    assert_eq!(saved.controller_recoveries(), 3);
                    Ok(())
                }).await.unwrap();
            }
        }
    }
    service.shutdown.cancel();
}

#[tokio::test]
async fn fencing_rejects_replacement_before_and_during_delete() {
    use axum::{Router, extract::Request, response::IntoResponse, routing::any};
    use std::sync::atomic::{AtomicUsize, Ordering};
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Semaphore::new(0));
    let deletes = Arc::new(AtomicUsize::new(0));
    let app = Router::new().fallback(any({
        let (entered, release, deletes) = (entered.clone(), release.clone(), deletes.clone());
        move |request: Request| {
            let (entered, release, deletes) = (entered.clone(), release.clone(), deletes.clone());
            async move {
                assert_eq!(request.uri().path(), "/runs/old-attempt");
                deletes.fetch_add(1, Ordering::SeqCst);
                entered.notify_one();
                release.acquire().await.unwrap().forget();
                axum::http::StatusCode::NO_CONTENT.into_response()
            }
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let (_root, service, _chat, run, _workspace) =
        fixture_with_runner(format!("http://{address}")).await;
    let original = RunCheckpoint::from_value(json!({
        "prepared": { "backend": "native", "isolated": true }, "runnerId": "old-attempt",
        "completed": true, "terminalCode": 0, "remainingMs": null,
    }))
    .unwrap();
    let mut replacement = original.clone();
    replacement.runner_id = Some(Some("new-attempt".into()));
    replacement.process = Some(Some(crate::recovery::ProcessIdentity {
        pid: 99,
        start: "new".into(),
        boot: "new".into(),
    }));
    replacement.store(&service.store, &run).await.unwrap();
    let run_value = service.store.run(&run).await.unwrap();
    assert!(
        recovery::fence_attempt(&service, &run_value, Some(&original))
            .await
            .is_err()
    );
    assert_eq!(deletes.load(Ordering::SeqCst), 0);
    let mut changed_process = replacement.clone();
    changed_process.runner_id = original.runner_id.clone();
    changed_process.store(&service.store, &run).await.unwrap();
    assert!(
        recovery::fence_attempt(&service, &run_value, Some(&original))
            .await
            .is_err()
    );
    assert_eq!(deletes.load(Ordering::SeqCst), 0);
    original.store(&service.store, &run).await.unwrap();
    let s = service.clone();
    let task = tokio::spawn(async move { recover(&s, &run_value).await });
    entered.notified().await;
    replacement.store(&service.store, &run).await.unwrap();
    release.add_permits(1);
    assert!(task.await.unwrap().is_err());
    let current = RunCheckpoint::load(&service.store, &run)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(current.runner_id(), Some("new-attempt"));
    assert_eq!(current.process(), replacement.process());
    assert_eq!(deletes.load(Ordering::SeqCst), 1);
    service.shutdown.cancel();
    server.abort();
}

#[tokio::test]
async fn restart_reconciliation_cannot_fence_a_replacement_and_honors_concurrent_cancel() {
    use axum::{Router, extract::Request, response::IntoResponse, routing::any};
    use std::sync::atomic::{AtomicUsize, Ordering};
    for replace in [true, false] {
        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let deletes = Arc::new(AtomicUsize::new(0));
        let (read, reply, stops) = (entered.clone(), release.clone(), deletes.clone());
        let app = Router::new().fallback(any(move |request: Request| {
            let (read, reply, stops) = (read.clone(), reply.clone(), stops.clone());
            async move {
                assert!(request.uri().path().starts_with("/runs/old-attempt"));
                if request.method() == "DELETE" {
                    stops.fetch_add(1, Ordering::SeqCst);
                    return axum::http::StatusCode::NO_CONTENT.into_response();
                }
                assert_eq!(request.uri().path(), "/runs/old-attempt/wait");
                read.notify_one();
                reply.notified().await;
                axum::Json(json!({ "StatusCode": 0 })).into_response()
            }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let (_root, service, _chat, run, _workspace) =
            fixture_with_runner(format!("http://{address}")).await;
        let saved = RunCheckpoint::from_value(json!({
            "prepared": { "backend": "native", "isolated": true },
            "runnerId": "old-attempt", "completed": true, "controllerRecoveries": 3,
        }))
        .unwrap();
        saved.store(&service.store, &run).await.unwrap();
        let (s, id) = (service.clone(), run.clone());
        let recovering =
            tokio::spawn(async move { recover(&s, &s.store.run(&id).await.unwrap()).await });
        entered.notified().await;
        if replace {
            let mut replacement = saved.clone();
            replacement.runner_id = Some(Some("new-attempt".into()));
            replacement.completed = Some(false);
            replacement.store(&service.store, &run).await.unwrap();
        } else {
            service
                .store
                .change_run(
                    &run,
                    crate::runs::RunChanges {
                        cancel_requested_at: Some(Some(now())),
                        ..Default::default()
                    },
                )
                .await
                .unwrap();
        }
        release.notify_one();
        assert!(!recovering.await.unwrap().unwrap());
        if replace {
            let current = RunCheckpoint::load(&service.store, &run)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(current.runner_id(), Some("new-attempt"));
            assert_eq!(current.terminal_code(), None);
            assert_eq!(deletes.load(Ordering::SeqCst), 0);
        } else {
            assert_eq!(
                service.store.run_state(&run).await.unwrap().status,
                RunStatus::Cancelled
            );
            assert_eq!(deletes.load(Ordering::SeqCst), 1);
        }
        server.abort();
        service.shutdown.cancel();
    }
}

#[tokio::test]
async fn a_new_turn_cannot_reuse_the_previous_attempts_healthy_exit() {
    use axum::{Router, extract::Request, response::IntoResponse, routing::any};
    use std::sync::atomic::{AtomicUsize, Ordering};
    let waits = Arc::new(AtomicUsize::new(0));
    let count = waits.clone();
    let app = Router::new().fallback(any(move |request: Request| {
        let count = count.clone();
        async move {
            if request.method() == "DELETE" {
                return axum::http::StatusCode::NO_CONTENT.into_response();
            }
            count.fetch_add(1, Ordering::SeqCst);
            axum::Json(json!({ "StatusCode": 0 })).into_response()
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let (_root, service, _chat, run, _workspace) =
        fixture_with_runner(format!("http://{address}")).await;
    let saved = RunCheckpoint::from_value(json!({
        "prepared": { "backend": "native", "isolated": true },
        "runnerId": "old-attempt", "completed": false, "terminalCode": null,
    }))
    .unwrap();
    saved.store(&service.store, &run).await.unwrap();
    assert!(
        recover(&service, &service.store.run(&run).await.unwrap())
            .await
            .unwrap()
    );
    assert_eq!(waits.load(Ordering::SeqCst), 0);
    assert_eq!(
        service.store.run_state(&run).await.unwrap().status,
        RunStatus::Queued
    );
    server.abort();
    service.shutdown.cancel();
}

#[tokio::test]
async fn waiting_mission_restart_releases_attempt_before_deadline_even_when_cancelled() {
    for (cancel, remote, queued) in [
        (false, false, false),
        (true, false, false),
        (false, true, false),
        (true, true, false),
        (false, false, true),
        (true, false, true),
        (false, true, true),
        (true, true, true),
    ] {
        let (_root, original, chat_id, run_id, _workspace) = fixture().await;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let requests = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let observed = requests.clone();
        let remote_observed = requests.clone();
        let app = axum::Router::new()
            .route(
                "/runs/{attempt}",
                axum::routing::delete(move || async move {
                    observed.fetch_add(1, Ordering::Relaxed);
                    axum::http::StatusCode::NOT_FOUND
                }),
            )
            .route(
                "/internal/execution/{node}/runs/{attempt}",
                axum::routing::delete(move || async move {
                    remote_observed.fetch_add(1, Ordering::Relaxed);
                    axum::http::StatusCode::SERVICE_UNAVAILABLE
                }),
            );
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let mut config = original.config.clone();
        config.runner_url = format!("http://{address}");
        config.public_url = config.runner_url.clone();
        let node = if remote {
            "00000000-0000-4000-8000-0000000000ef"
        } else {
            crate::nodes::LOCAL_NODE_ID
        };
        original
            .store
            .transaction(move |db| db.remove_conversation(&chat_id))
            .await
            .unwrap();
        drop(original);
        let service = Service::new(config).await.unwrap();
        let wakeup = json!({
            "id": "wake", "messageId": null, "reason": "Waiting", "seconds": 3600,
            "createdAt": now(), "wakeAt": now() + 3_600_000,
        });
        service
            .store
            .change_run(
                &run_id,
                crate::runs::RunChanges {
                    status: Some(if queued {
                        RunStatus::Queued
                    } else {
                        RunStatus::Waiting
                    }),
                    recovery_pending: Some(queued),
                    trigger: Some(("manual").to_owned()),
                    session_id: Some(Some("saved".into())),
                    wakeup: Some(Some(serde_json::from_value(wakeup.clone()).unwrap())),
                    boundary: Some(json!({"isolated": true})),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let checkpoint = RunCheckpoint::from_value(json!({
            "completed": true, "terminalCode": 0, "launched": true, "runnerId": "old-attempt",
            "nodeId": node,
            "prepared": { "isolated": true, "backend": "native" },
        }))
        .unwrap();
        checkpoint.store(&service.store, &run_id).await.unwrap();
        if remote {
            use crate::node_fixture::StoreNodeFixture;

            service
                .store
                .put_node_fixture(json!({
                    "id": node, "accepting": false, "lastSeen": 0,
                    "capabilities": { "backend": "native", "kvm": false },
                }))
                .await
                .unwrap();
        }
        service
            .store
            .put_attempt_fixture(json!({
                "id": "old-attempt", "runId": run_id,
                "nodeId": node, "released": false,
            }))
            .await
            .unwrap();

        if remote {
            // The previous remote execution lease has expired; its node
            // cannot serve requests and has no available placement record.
            service.node_lease_deadlines.lock().await.insert(
                "old-attempt".into(),
                tokio::time::Instant::now() - Duration::from_secs(30),
            );
        }
        service.worker.initialize(&service).await.unwrap();
        if cancel {
            service.worker.cancel(&service, &run_id).await.unwrap();
        }
        service.worker.tick(&service).await.unwrap();

        assert_eq!(requests.load(Ordering::Relaxed), 1);
        let attempt = service
            .store
            .attempt_fixture("old-attempt")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(attempt["released"], true);
        let current = service.store.run(&run_id).await.unwrap();
        assert_eq!(current["recoveryPending"], false);
        assert_eq!(
            current["status"],
            if cancel { "cancelled" } else { "waiting" }
        );
        if cancel {
            assert!(current["wakeup"].is_null());
        } else {
            assert_eq!(current["wakeup"], wakeup);
            assert!(current["missionTurnId"].is_null());
        }
        assert!(service.worker.active.lock().await.is_empty());
        server.abort();
    }
}

#[tokio::test]
async fn completed_conversation_recovery_preserves_outcome_and_respects_cancellation() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(root.path()).unwrap();
    store
        .transaction(|db| {
            let record = json!({
                "id": "run", "taskId": "chat", "trigger": "chat", "status": "queued", "createdAt": 0,
                "recoveryPending": true, "sessionId": "saved", "resumeAvailable": false,
                "outcome": { "status": "completed" },
            });
            db.add_run_fixture(&record, None)?;
            let checkpoint = RunCheckpoint::from_value(json!({ "completed": true }))?;
            assert!(!finish_recovery(db, "run", Some(&checkpoint))?);
            let finished = db.run("run")?.unwrap();
            assert_eq!(finished["status"], "succeeded");
            assert_eq!(finished["resumeAvailable"], false);
            assert!(finished["finishedAt"].is_number());
            assert_eq!(finished["outcome"]["status"], "completed");
            for settled in [false, true] {
                // A stale recovery snapshot preceded this cancellation. Completion
                // must read it in the same transaction that writes the next state.
                db.change_run(
                    "run",
                    crate::runs::RunChanges {
                        status: Some(cairn_contracts::RunStatus::Queued),
                        recovery_pending: Some(true),
                        cancel_requested_at: Some(Some(now())),
                        wakeup: Some(None),
                        ..Default::default()
                    },
                )?;
                let mut checkpoint = checkpoint.clone();
                if settled {
                    checkpoint.settled = Some(Some(Settled::from_state(
                        &db.run_state("run")?.unwrap(),
                    )));
                }
                assert!(!finish_recovery(db, "run", Some(&checkpoint))?);
                let cancelled = db.run("run")?.unwrap();
                assert_eq!(cancelled["status"], "cancelled");
                assert!(cancelled["wakeup"].is_null());
                assert_eq!(cancelled["recoveryPending"], false);
                assert!(db.active()?.is_empty());
            }
            Ok(())
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn compaction_completed_before_restart_keeps_the_last_reply_and_outcome() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(root.path()).unwrap();
    store
        .transaction(|db| {
            let outcome = json!({ "status": "completed", "messageId": "first" });
            let record = json!({
                "id": "chat-run",
                "taskId": "chat",
                "trigger": "chat",
                "status": "queued",
                "createdAt": 0,
                "recoveryPending": true,
                "sessionId": "saved",
                "summary": "Last reply",
                "finishedAt": 500,
                // Resuming the interrupted compaction cleared the run's outcome.
                "outcome": null,
                "chatExecution": {
                    "messageId": "compact:fixture",
                    "text": "",
                    "attachments": [],
                    "recovery": false,
                    "operation": "compact",
                    "previous": { "summary": "Last reply", "finishedAt": 500, "outcome": outcome },
                },
            });
            db.add_run_fixture(&record, None)?;

            // The compaction's `turn.completed` was saved; the manager stopped before
            // settling the run, and a compaction leaves no reply in the checkpoint.
            let checkpoint = RunCheckpoint::from_value(json!({ "completed": true }))?;
            assert_eq!(checkpoint.last_message(), "");
            assert!(!finish_recovery(db, "chat-run", Some(&checkpoint))?);

            let recovered = db.run("chat-run")?.unwrap();
            assert_eq!(recovered["status"], "succeeded");
            assert_eq!(recovered["recoveryPending"], false);
            assert_eq!(recovered["summary"], "Last reply");
            assert_eq!(recovered["finishedAt"], 500);
            assert_eq!(recovered["outcome"], outcome);
            Ok(())
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn recovery_commits_the_fenced_failure_before_requesting_optional_retirement() {
    check_cleanup_observation(false, false).await;
    check_cleanup_observation(true, false).await;
}

#[tokio::test]
async fn graceful_shutdown_during_cleanup_probe_keeps_the_receipt_for_restart() {
    check_cleanup_observation(false, true).await;
}

async fn check_cleanup_observation(restart_before_publication: bool, shutdown_during_probe: bool) {
    use crate::run_fixture::DbRunFixture;
    use axum::{
        Json, Router,
        http::Uri,
        routing::{delete, post},
    };
    use std::sync::atomic::{AtomicBool, Ordering};

    let attempt = crate::config::id();
    let evidence = Arc::new(tokio::sync::OnceCell::<(Arc<Service>, String)>::new());
    let probed = Arc::new(AtomicBool::new(false));
    let stopped = json!({
        "StatusCode": 70,
        "FinalizationFailed": true,
        "NativeCleanup": {
            "attemptId": attempt,
            "workloadFenced": true,
            "message": "Legacy stop still reports conversation cleanup pending."
        }
    });
    let app = Router::new()
        .route(
            "/runs/{attempt}",
            delete(move || async move { Json(stopped) }),
        )
        .route(
            "/runs/{attempt}/wait",
            post({
                let (attempt, evidence, probed) =
                    (attempt.clone(), evidence.clone(), probed.clone());
                move |uri: Uri| {
                    let (attempt, evidence, probed) =
                        (attempt.clone(), evidence.clone(), probed.clone());
                    async move {
                        let mut receipt = json!({
                            "StatusCode": 70,
                            "FinalizationFailed": true,
                            "NativeCleanup": {
                                "attemptId": attempt,
                                "workloadFenced": true,
                                "message": "Native cleanup was pending."
                            }
                        });
                        if uri.query() == Some(crate::runner::NATIVE_CLEANUP_RETIREMENT_QUERY) {
                            let (service, run) = evidence.get().unwrap();
                            let store = &service.store;
                            let saved = RunCheckpoint::load(store, run).await.unwrap().unwrap();
                            assert_eq!(
                                saved.finalization_failure_attempt.as_deref(),
                                Some(attempt.as_str())
                            );
                            assert_eq!(
                                saved.finalization_error(),
                                Some("Native cleanup was pending.")
                            );
                            let run = run.clone();
                            let events = store
                                .read(move |db| db.events_fixture(&run, 0, 100))
                                .await
                                .unwrap();
                            assert!(events.iter().all(|event| event["type"] != "error"));
                            probed.store(true, Ordering::Release);
                            if shutdown_during_probe {
                                service.shutdown.cancel();
                            }
                            receipt["NativeCleanup"]["retired"] = true.into();
                        }
                        Json(receipt)
                    }
                }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let (_root, service, _chat, run, _workspace) = fixture_with_runner(url).await;
    assert!(evidence.set((service.clone(), run.clone())).is_ok());
    let mut saved = RunCheckpoint::from_value(json!({
        "prepared": { "backend": "native", "isolated": true },
        "runnerId": attempt,
        "remainingMs": restart_before_publication.then_some(0)
    }))
    .unwrap();
    if restart_before_publication {
        let id = run.clone();
        service
            .store
            .transaction(move |db| {
                saved.record_controller_observation(
                    db,
                    &id,
                    &attempt,
                    Some(crate::runner::ControllerResult {
                        code: crate::runner::CONTROLLER_FAILED,
                        finalization_failed: true,
                        physical_finalized: false,
                        terminal_reason: None,
                        native_cleanup: Some("Native cleanup was pending.".into()),
                    }),
                )?;
                assert!(saved.cleanup_observation_pending());
                saved.store_in(db, &id)
            })
            .await
            .unwrap();
    } else {
        saved.store(&service.store, &run).await.unwrap();
    }

    let snapshot = service.store.run(&run).await.unwrap();
    let recovered = recover(&service, &snapshot).await;
    assert!(probed.load(Ordering::Acquire));
    if shutdown_during_probe {
        assert_eq!(recovered.unwrap_err().message, "Worker is restarting.");
        let saved = RunCheckpoint::load(&service.store, &run)
            .await
            .unwrap()
            .unwrap();
        assert!(saved.cleanup_observation_pending());
        assert_eq!(
            saved.finalization_error(),
            Some("Native cleanup was pending.")
        );
        let events = service
            .store
            .read(move |db| db.events_fixture(&run, 0, 100))
            .await
            .unwrap();
        assert!(events.iter().all(|event| event["type"] != "error"));
        server.abort();
        return;
    }
    assert!(!recovered.unwrap());
    assert_eq!(service.store.run(&run).await.unwrap()["status"], "failed");
    let saved = RunCheckpoint::load(&service.store, &run)
        .await
        .unwrap()
        .unwrap();
    assert!(!saved.cleanup_observation_pending());
    assert!(
        saved
            .finalization_error()
            .unwrap()
            .contains("Native disk cleanup completed")
    );
    let events = service
        .store
        .read(move |db| db.events_fixture(&run, 0, 100))
        .await
        .unwrap();
    let errors = events
        .iter()
        .filter(|event| event["type"] == "error")
        .collect::<Vec<_>>();
    assert_eq!(errors.len(), 1);
    assert!(
        errors[0]["text"]
            .as_str()
            .unwrap()
            .contains("Native disk cleanup completed")
    );
    server.abort();
}

#[tokio::test]
async fn settled_success_never_requires_readmission_before_retirement() {
    let (_root, service, _chat, run, _workspace) = fixture().await;
    service
        .store
        .transaction(move |db| {
            let state = db.run_state(&run)?.unwrap();
            let record = db.run(&run)?.unwrap();
            let mut saved = RunCheckpoint::fresh(None);
            saved.settled = Some(Some(Settled::from_state(&cairn_contracts::RunState {
                status: RunStatus::Succeeded,
                ..state.clone()
            })));
            assert!(recovery_cannot_replay(Some(&saved), None, &state, &record));
            Ok(())
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn first_wait_after_native_cleanup_retirement_cannot_replay_an_incomplete_turn() {
    let (_root, service, _chat_id, run_id, _workspace) = fixture().await;
    let attempt = crate::config::id();
    let result = crate::native::retired_cleanup_completion(&attempt).await;
    assert_eq!(result.code, crate::runner::CONTROLLER_FAILED);
    assert!(result.physical_finalized);
    assert!(!result.finalization_failed);
    let message = result.native_cleanup.clone().unwrap();
    assert!(message.contains("Native disk cleanup completed"));
    service
        .store
        .transaction(move |db| {
            let mut saved = RunCheckpoint::from_value(json!({
                "prepared": { "backend": "native", "isolated": true },
                "runnerId": attempt,
                "remainingMs": null
            }))?;
            assert!(!saved.completed());
            saved.record_controller_result(db, &run_id, &attempt, Some(result))?;
            saved.store_in(db, &run_id)?;

            let saved = RunCheckpoint::load_in(db, &run_id)?.unwrap();
            let state = db.run_state(&run_id)?.unwrap();
            let run = db.run(&run_id)?.unwrap();
            assert_eq!(saved.finalization_error(), Some(message.as_str()));
            assert!(recovery_cannot_replay(Some(&saved), None, &state, &run));
            assert!(!finish_recovery(db, &run_id, Some(&saved))?);
            assert_eq!(db.run(&run_id)?.unwrap()["status"], "failed");
            Ok(())
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn completed_turn_with_late_controller_failure_settles_without_relaunch() {
    let (_root, service, _chat_id, run_id, _workspace) = fixture().await;
    let checkpoint = super::super::checkpoint::Checkpoint::new(
        service.store.clone(),
        run_id.clone(),
        RunCheckpoint::fresh(None),
    );
    let mut activity =
        crate::performance::Activity::new(&run_id, "fixture", "host", Provider::Codex);
    let mut log_total = 500_000;
    for event in [
        json!({ "type": "turn.completed" }),
        json!({ "type": "runner.failed", "error": { "message": "Workspace sealing failed" } }),
    ] {
        super::super::output::record(
            &service,
            &super::super::output::Output {
                kind: super::super::output::OutputKind::Stdout,
                raw: event.to_string(),
                received: tokio::time::Instant::now(),
            },
            &checkpoint,
            super::super::output::Attempt::default(),
            &[],
            &mut log_total,
            &mut activity,
        )
        .await
        .unwrap();
    }
    let saved = RunCheckpoint::load(&service.store, &run_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(saved.last_error(), "Workspace sealing failed");
    let run = service.store.run(&run_id).await.unwrap();
    assert!(!recover(&service, &run).await.unwrap());
    assert_eq!(
        service.store.run(&run_id).await.unwrap()["status"],
        "succeeded"
    );
}

#[tokio::test]
async fn deletion_during_account_preparation_leaves_no_orphan_files() {
    let (_root, service, chat_id, run_id, workspace) = fixture().await;
    service.accounts.initialize(&service).await.unwrap();
    let mut account = serde_json::to_value(
        service
            .accounts
            .create(&service, Provider::Codex, "Ready")
            .await
            .unwrap(),
    )
    .unwrap();
    account["state"] = "ready".into();
    account["usage"] = json!({
        "allowed": true,
        "checkedAt": now(),
        "windows": [{ "id": "w", "usedPercent": 20, "models": [] }],
        "resets": null
    });
    service
        .store
        .save_account_fixture(account.clone())
        .await
        .unwrap();
    let account_guard = service.accounts.lock(text(&account, "id")).await;
    let run = service.store.run(&run_id).await.unwrap();
    let mut projects = HashSet::new();
    let state = service.store.run_state(&run_id).await.unwrap();
    let admission = service
        .worker
        .try_launch(&service, run, state, &mut projects);
    tokio::pin!(admission);
    let account_mutex = tokio::sync::OwnedMutexGuard::mutex(&account_guard);
    let owners = Arc::strong_count(account_mutex);
    tokio::time::timeout(
        Duration::from_secs(2),
        std::future::poll_fn(|cx| {
            assert!(admission.as_mut().poll(cx).is_pending());
            if Arc::strong_count(account_mutex) > owners {
                Poll::Ready(())
            } else {
                Poll::Pending
            }
        }),
    )
    .await
    .unwrap();

    service.chat_trash(&chat_id, true).await.unwrap();
    let other = service.chat_create(json!({})).await.unwrap();
    let other_id = text(&other, "id");
    service.chat_trash(other_id, true).await.unwrap();
    service.cleanup_conversations().await.unwrap();
    service.cleanup_conversations().await.unwrap();
    assert!(
        service
            .store
            .conversation_boundary(other_id)
            .await
            .unwrap()
            .is_none(),
        "Blocked preparation must not stall unrelated deletion"
    );

    drop(account_guard);
    admission.await.unwrap();
    assert!(service.worker.active.lock().await.is_empty());
    assert!(
        service
            .accounts
            .lease(&service, &run_id)
            .await
            .unwrap()
            .is_none()
    );
    service.cleanup_conversations().await.unwrap();
    assert!(
        service
            .store
            .conversation_boundary(&chat_id)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(service.store.run(&run_id).await.unwrap_err().status, 404);
    assert!(
        service
            .store
            .checkpoint_boundary(&run_id)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        !workspace.exists(),
        "Account preparation must not recreate a purged run directory"
    );
}

#[tokio::test]
async fn a_retry_that_expires_during_preparation_cannot_register() {
    let (_root, service, _chat_id, run_id, workspace) = fixture().await;
    let _prepared = service
        .store
        .change_run(
            &run_id,
            crate::runs::RunChanges {
                retry: Some(Some(
                    serde_json::from_value(json!({
                        "attempt": 1,
                        "limit": 5,
                        "nextAttemptAt": now() - 1,
                        "deadlineAt": now() + 60_000,
                        "cause": {"kind":"timeout","message":"temporary"},
                    }))
                    .unwrap(),
                )),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let prepared = service.store.run(&run_id).await.unwrap();
    // Admission captured an eligible run, then waits behind asynchronous work.
    let active = service.worker.active.lock().await;
    let registration = service
        .worker
        .register(&service, &prepared, CancellationToken::new());
    tokio::pin!(registration);
    std::future::poll_fn(|cx| {
        assert!(registration.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    service
        .store
        .change_run(
            &run_id,
            crate::runs::RunChanges {
                retry: Some({
                    let mut retry = service.store.run_state(&run_id).await.unwrap().retry;
                    retry.as_mut().unwrap().deadline_at = Some(now() - 1);
                    retry
                }),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    drop(active);
    assert!(!registration.await.unwrap());
    let stopped = service.store.run(&run_id).await.unwrap();
    assert_eq!(stopped["status"], RunStatus::Failed);
    assert!(stopped["retry"]["nextAttemptAt"].is_null());
    assert!(service.worker.active.lock().await.is_empty());
    assert_eq!(
        tokio::fs::read_to_string(workspace.join("work.txt"))
            .await
            .unwrap(),
        "Private work"
    );
}

#[tokio::test]
async fn deletion_committed_before_registration_prevents_stale_admission() {
    let (_root, service, chat_id, run_id, workspace) = fixture().await;
    let prepared = service.store.run(&run_id).await.unwrap();

    assert!(
        service
            .worker
            .register(&service, &prepared, CancellationToken::new())
            .await
            .unwrap()
    );
    assert!(
        !service
            .worker
            .register(&service, &prepared, CancellationToken::new())
            .await
            .unwrap()
    );
    service.worker.active.lock().await.remove(&run_id);

    let active = service.worker.active.lock().await;
    let registration = service
        .worker
        .register(&service, &prepared, CancellationToken::new());
    tokio::pin!(registration);
    std::future::poll_fn(|cx| {
        assert!(registration.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    let deleting = service.clone();
    let deleting_id = chat_id.clone();
    let deletion = tokio::spawn(async move { deleting.chat_trash(&deleting_id, true).await });
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let chat = service
                .store
                .conversation_boundary(&chat_id)
                .await
                .unwrap()
                .unwrap();
            let run = service.store.run(&run_id).await.unwrap();
            if chat["lifecycle"] == "trash" && !run["cancelRequestedAt"].is_null() {
                assert_eq!(run["status"], RunStatus::Queued);
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    drop(active);

    assert!(
        !registration.await.unwrap(),
        "A deleted conversation must not gain an execution"
    );
    deletion.await.unwrap().unwrap();
    assert!(service.worker.active.lock().await.is_empty());
    service.cleanup_conversations().await.unwrap();
    assert!(
        service
            .store
            .conversation_boundary(&chat_id)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(service.store.run(&run_id).await.unwrap_err().status, 404);
    assert!(
        service
            .store
            .checkpoint_boundary(&run_id)
            .await
            .unwrap()
            .is_none()
    );
    assert!(!workspace.exists());
    assert!(
        !service
            .worker
            .register(&service, &prepared, CancellationToken::new())
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn an_admission_prepared_before_merge_cannot_register_after_resume() {
    let (_root, service, chat_id, run_id, _workspace) = fixture().await;
    let source = service
        .agent_fixture(json!({ "name": "Source" }), None)
        .await
        .unwrap();
    let run_id_copy = run_id.clone();
    let source_copy = source.clone();
    let prepared = service
        .store
        .transaction(move |db| {
            crate::run_fixture::change_run_fixture(
                db,
                &run_id_copy,
                &json!({
                    "snapshot": { "agent": source_copy },
                    "chatExecution": { "messageId": "original-message" },
                }),
            )
        })
        .await
        .unwrap();
    service
        .store
        .change_run(
            &run_id,
            crate::runs::RunChanges {
                status: Some(RunStatus::Cancelled),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    service
        .store
        .transaction({
            let chat_id = chat_id.clone();
            let source = source.clone();
            move |db| {
                let mut chat = db.conversation_boundary(&chat_id)?.unwrap();
                chat["agentId"] = source["id"].clone();
                db.save_conversation_boundary(&chat)?;
                Ok(())
            }
        })
        .await
        .unwrap();
    service
        .merge_agents(
            serde_json::from_value(json!({
                "sourceId": source["id"], "targetId": crate::config::MAIN_AGENT_ID, "confirm": true,
            }))
            .unwrap(),
        )
        .await
        .unwrap();
    let resumed = service.worker.resume(&service, &run_id).await.unwrap();
    assert!(
        !service
            .worker
            .register(&service, &prepared, CancellationToken::new())
            .await
            .unwrap()
    );
    assert!(service.worker.active.lock().await.is_empty());
    assert!(
        service
            .worker
            .register(&service, &resumed, CancellationToken::new())
            .await
            .unwrap()
    );
}
