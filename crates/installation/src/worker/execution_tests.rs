use super::*;

async fn fence_service(
    app: axum::Router,
) -> (tempfile::TempDir, Arc<Service>, tokio::task::JoinHandle<()>) {
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
    (root, service, server)
}

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
    let (_root, service, server) = fence_service(app).await;
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
async fn stop_and_expiration_during_native_delete_preserve_the_fence() {
    for (stop, response) in [
        (true, NativeDeleteResponse::Deferred),
        (false, NativeDeleteResponse::Deferred),
        (false, NativeDeleteResponse::Stopped),
        (true, NativeDeleteResponse::Stopped),
        (false, NativeDeleteResponse::Aborted),
        (false, NativeDeleteResponse::FlushFailed),
        (false, NativeDeleteResponse::Flushed),
    ] {
        request_during_native_delete(stop, response).await;
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum NativeDeleteResponse {
    Deferred,
    Stopped,
    Aborted,
    FlushFailed,
    Flushed,
}

async fn request_during_native_delete(stop: bool, response: NativeDeleteResponse) {
    use crate::environment_expiration::Expiration;
    use axum::{
        Json, Router,
        routing::{delete, post},
    };
    use tokio::sync::Notify;

    let attempt = crate::config::id();
    let arrived = Arc::new(Notify::new());
    let reply = Arc::new(Notify::new());
    let receipt = match response {
        NativeDeleteResponse::Deferred => json!({
            "StatusCode": crate::runner::CONTROLLER_FAILED,
            "FinalizationFailed": true,
            "NativeCleanup": {
                "attemptId": attempt, "workloadFenced": true,
                "cleanupStatusProtocol": crate::runner::NATIVE_CLEANUP_STATUS_PROTOCOL,
                "message": "Native disk cleanup is still in progress."
            }
        }),
        NativeDeleteResponse::FlushFailed => json!({
            "StatusCode": crate::runner::CONTROLLER_FAILED,
            "FinalizationFailed": true,
        }),
        NativeDeleteResponse::Stopped => json!({}),
        NativeDeleteResponse::Aborted => json!({ "StatusCode": 0, "PhysicalFinalized": true }),
        NativeDeleteResponse::Flushed => json!({
            "StatusCode": 0,
            "PhysicalFinalized": true,
            "NativeCleanupRetired": { "attemptId": attempt },
        }),
    };
    let app = Router::new().route(
        "/runs/{attempt}",
        delete({
            let arrived = arrived.clone();
            let reply = reply.clone();
            move || async move {
                arrived.notify_one();
                reply.notified().await;
                Json(receipt)
            }
        }),
    );
    let app = if response == NativeDeleteResponse::Aborted {
        app.route(
            "/runs/{attempt}/wait",
            post(|| async { Json(json!({ "StatusCode": 0, "PhysicalFinalized": true })) }),
        )
    } else {
        app
    };
    let (_root, service, server) = fence_service(app).await;
    let id = crate::config::id();
    let fixture = id.clone();
    let saved = service
        .store
        .transaction(move |db| {
            db.add_run_fixture(
                &json!({
                    "id": fixture, "taskId": fixture, "status": "succeeded", "trigger": "chat",
                    "isolated": true, "recoveryPending": true, "finishedAt": 42,
                }),
                None,
            )?;
            let saved = RunCheckpoint::from_value(json!({
                "runnerId": attempt,
                "prepared": { "backend": "native", "isolated": true },
                "settled": { "status": "succeeded", "finishedAt": 42 },
            }))?;
            saved.store_in(db, &fixture)?;
            Ok(saved)
        })
        .await
        .unwrap();
    let checkpoint = Checkpoint::new(service.store.clone(), id.clone(), saved);
    let run = service.store.run(&id).await.unwrap();
    service
        .worker
        .active
        .lock()
        .await
        .insert(id.clone(), CancellationToken::new());
    let revision_before_fence = tokio::time::timeout(Duration::from_secs(5), async {
        let (fence, revision) = tokio::join!(checkpoint.fence(&service, &run), async {
            arrived.notified().await;
            service
                .store
                .change_run(
                    &id,
                    crate::runs::RunChanges {
                        placement: Some(Box::new(crate::nodes::run_placement::PlacementChanges {
                            execution_backend: Some(Some("firecracker".to_owned())),
                            ..Default::default()
                        })),
                        ..Default::default()
                    },
                )
                .await
                .unwrap();
            assert_eq!(
                service.store.run(&id).await.unwrap()["executionBackend"],
                "firecracker"
            );
            let migrating = id.clone();
            service
                .store
                .read(move |db| {
                    assert_eq!(db.run(&migrating)?.unwrap()["nativeCleanupPending"], true);
                    assert_eq!(
                        db.conversation_run(&migrating)?.unwrap()["nativeCleanupPending"],
                        true
                    );
                    Ok(())
                })
                .await
                .unwrap();
            if !stop || response != NativeDeleteResponse::Deferred {
                service
                    .expire_environment(
                        &id,
                        Expiration::new(cairn_contracts::EnvironmentExpirationCause::NodeChange),
                        |_| Ok(()),
                    )
                    .await
                    .unwrap();
            }
            if stop {
                service.worker.cancel(&service, &id).await.unwrap();
            }
            let current = service.store.run(&id).await.unwrap();
            assert_eq!(current["cancelRequestedAt"].is_number(), stop);
            reply.notify_one();
            current["environmentAdmissionRevision"].clone()
        });
        fence.unwrap();
        revision
    })
    .await
    .unwrap();
    let fenced = service.store.run(&id).await.unwrap();
    assert_eq!(
        fenced["environmentAdmissionRevision"] != revision_before_fence,
        response == NativeDeleteResponse::Deferred,
    );
    release(&service, &checkpoint, None, true).await.unwrap();

    super::super::schedule::observe_native_cleanup(&service)
        .await
        .unwrap();
    let stopped = service.store.run_state(&id).await.unwrap();
    let expected = if stop {
        RunStatus::Cancelled
    } else if response == NativeDeleteResponse::Flushed {
        RunStatus::Succeeded
    } else {
        RunStatus::Failed
    };
    assert_eq!(stopped.status, expected);
    if !stop {
        match response {
            NativeDeleteResponse::Deferred => assert_eq!(
                stopped.error.as_deref(),
                Some("Native disk cleanup is still in progress."),
            ),
            NativeDeleteResponse::Stopped | NativeDeleteResponse::Aborted => assert_eq!(
                stopped.error.as_deref(),
                Some("Native disk cleanup could not be confirmed before the environment expired."),
            ),
            NativeDeleteResponse::FlushFailed => assert!(
                stopped
                    .error
                    .as_deref()
                    .unwrap()
                    .contains("filesystem could not be flushed"),
            ),
            NativeDeleteResponse::Flushed => assert!(stopped.error.is_none()),
        }
    }
    assert_eq!(stopped.finished_at, Some(42));
    assert!(!stopped.recovery_pending);
    let retained = RunCheckpoint::load(&service.store, &id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        retained
            .pending_native_cleanup()
            .is_some_and(crate::recovery::PendingNativeCleanup::awaiting_retirement),
        response == NativeDeleteResponse::Deferred,
    );
    service.worker.active.lock().await.remove(&id);
    service.detach_stopped_environments().await.unwrap();
    let detached = service.store.run_state(&id).await.unwrap();
    assert_eq!(detached.status, expected);
    assert_eq!(detached.finished_at, Some(42));
    server.abort();
}

#[tokio::test]
async fn restarted_recovery_hands_off_before_waiting_on_the_account() {
    use axum::{Json, Router, routing::delete};

    let attempt = crate::config::id();
    let receipt = json!({
        "StatusCode": crate::runner::CONTROLLER_FAILED,
        "FinalizationFailed": true,
        "NativeCleanup": {
            "attemptId": attempt, "workloadFenced": true,
            "cleanupStatusProtocol": crate::runner::NATIVE_CLEANUP_STATUS_PROTOCOL,
            "message": "Native disk cleanup is still in progress."
        }
    });
    let app = Router::new().route(
        "/runs/{attempt}",
        delete(move || async move { Json(receipt) }),
    );
    let (_root, service, server) = fence_service(app).await;
    let id = crate::config::id();
    let fixture_id = id.clone();
    let account = crate::config::id();
    let credentials = service.accounts.lock(&account).await;
    service
        .store
        .transaction(move |db| {
            db.add_run_fixture(&json!({
            "id": fixture_id, "taskId": fixture_id, "status": "succeeded", "trigger": "chat",
            "isolated": true, "recoveryPending": true, "finishedAt": 42, "accountId": account,
        }), None)?;
            RunCheckpoint::from_value(json!({
                "runnerId": attempt,
                "prepared": { "backend": "native", "isolated": true },
                "settled": { "status": "succeeded", "finishedAt": 42 },
            }))?
            .store_in(db, &fixture_id)
        })
        .await
        .unwrap();

    tokio::time::timeout(
        Duration::from_secs(5),
        super::super::schedule::recover_completed_environments(&service),
    )
    .await
    .expect("the deferred fence should release ordinary recovery")
    .unwrap();
    let saved = RunCheckpoint::load(&service.store, &id)
        .await
        .unwrap()
        .unwrap();
    assert!(saved.pending_native_cleanup().unwrap().needs_observation());
    assert!(
        service
            .store
            .run_state(&id)
            .await
            .unwrap()
            .account_wait_reason
            .is_none()
    );
    drop(credentials);
    server.abort();
}

#[tokio::test]
async fn a_live_native_handoff_settles_while_credential_release_is_blocked() {
    use crate::environment_expiration::Expiration;

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Settlement {
        CleanFlush,
        Deadline,
        Stop,
        Detach,
    }

    for settlement in [
        Settlement::CleanFlush,
        Settlement::Deadline,
        Settlement::Stop,
        Settlement::Detach,
    ] {
        let (root, service, server) = fence_service(axum::Router::new()).await;
        let id = crate::config::id();
        let account = crate::config::id();
        tokio::fs::create_dir_all(&service.config.home)
            .await
            .unwrap();
        let chat = service.chat_create(json!({})).await.unwrap()["id"]
            .as_str()
            .unwrap()
            .to_owned();
        let conversation = chat.clone();
        let fixture = id.clone();
        let saved =
            service
                .store
                .transaction(move |db| {
                    let attempt = crate::config::id();
                    let agent = db.agent_snapshot(crate::config::MAIN_AGENT_ID)?.unwrap();
                    db.add_run_fixture(&json!({
                "id": fixture, "taskId": fixture, "status": "succeeded", "trigger": "chat",
                "isolated": true, "recoveryPending": true, "finishedAt": 42,
                "snapshot": { "agent": agent }, "sessionId": "saved-session",
            }), None)?;
                    let mut chat = db.conversation(&conversation)?.unwrap();
                    chat.run_id = Some(fixture.clone());
                    db.save_conversation(&chat)?;
                    let until = if settlement == Settlement::Deadline {
                        0
                    } else {
                        now() + 60_000
                    };
                    let state = if settlement == Settlement::CleanFlush {
                        "retired"
                    } else {
                        "observing"
                    };
                    let saved = RunCheckpoint::from_value(json!({
                        "runnerId": attempt,
                        "prepared": { "backend": "native", "isolated": true },
                        "settled": { "status": "succeeded", "finishedAt": 42 },
                        "pendingNativeCleanup": {
                            "attempt": attempt, "state": state, "until": until,
                            "message": "Native disk cleanup is still in progress."
                        },
                    }))?;
                    saved.store_in(db, &fixture)?;
                    Ok(saved)
                })
                .await
                .unwrap();
        service
            .worker
            .active
            .lock()
            .await
            .insert(id.clone(), CancellationToken::new());
        service.worker.held.lock().await.insert(id.clone());
        let credentials = service.accounts.lock(&account).await;
        let lease = Lease {
            account_id: account,
            provider: Provider::Codex,
            run_id: id.clone(),
            home: root.path().join("provider"),
            model: String::new(),
        };
        let active_service = service.clone();
        let checkpoint = Checkpoint::new(service.store.clone(), id.clone(), saved);
        let releasing =
            tokio::spawn(
                async move { release(&active_service, &checkpoint, Some(&lease), true).await },
            );
        tokio::time::timeout(Duration::from_secs(5), async {
            while !service
                .worker
                .deferred_cleanup_observers
                .lock()
                .await
                .has_handoff(&id)
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();

        match settlement {
            Settlement::CleanFlush => {
                service
                    .chat_send(
                        &chat,
                        json!({ "id": crate::config::id(), "text": "Later input" }),
                    )
                    .await
                    .unwrap();
            }
            Settlement::Deadline => {}
            Settlement::Stop => service.worker.cancel(&service, &id).await.unwrap(),
            Settlement::Detach => service
                .expire_environment(
                    &id,
                    Expiration::new(cairn_contracts::EnvironmentExpirationCause::NodeChange),
                    |_| Ok(()),
                )
                .await
                .unwrap(),
        }
        super::super::schedule::observe_native_cleanup(&service)
            .await
            .unwrap();
        assert!(!releasing.is_finished());
        assert!(service.worker.active.lock().await.contains_key(&id));
        let result = service.store.run_state(&id).await.unwrap();
        assert_eq!(result.finished_at, Some(42));
        assert!(!result.recovery_pending);
        if settlement == Settlement::CleanFlush {
            assert_eq!(result.status, RunStatus::Succeeded);
            // Model a scheduler scan whose provider-active snapshot excluded
            // this held worker just before it handed off its checkpoint.
            service
                .chat_tick(&std::collections::HashSet::new())
                .await
                .unwrap();
            service
                .store
                .read(move |db| {
                    assert_eq!(db.messages(&chat)?[0]["status"], "queued");
                    Ok(())
                })
                .await
                .unwrap();
        } else if settlement == Settlement::Stop {
            assert_eq!(result.status, RunStatus::Cancelled);
        } else {
            assert_eq!(result.status, RunStatus::Failed);
            assert_eq!(
                result.error.as_deref(),
                Some("Native disk cleanup is still in progress.")
            );
        }
        drop(credentials);
        releasing.await.unwrap().unwrap();
        server.abort();
    }
}

#[tokio::test]
async fn late_native_flush_preserves_settled_success_when_quarantine_clears() {
    use axum::{Json, Router, http::Request, routing::any};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Retirement {
        WithinGrace,
        FirstProbeUnavailable,
        Deferred,
        Expired,
    }

    async fn wait_for_observer(service: &Service) {
        tokio::time::timeout(Duration::from_secs(10), async {
            while service
                .worker
                .deferred_cleanup_observers
                .lock()
                .await
                .is_observing()
            {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
    }

    let attempt = crate::config::id();
    let receipt = json!({
        "StatusCode": crate::runner::CONTROLLER_FAILED,
        "FinalizationFailed": true,
        "NativeCleanup": {
            "attemptId": attempt,
            "workloadFenced": true,
            "message": "Native disk cleanup is still in progress."
        }
    });
    for (final_stop, cleared, retirement) in [
        (
            json!({ "NativeCleanupRetired": { "attemptId": attempt } }),
            true,
            Retirement::WithinGrace,
        ),
        (json!({}), false, Retirement::WithinGrace),
        (receipt.clone(), false, Retirement::WithinGrace),
        (
            json!({ "NativeCleanupRetired": { "attemptId": crate::config::id() } }),
            false,
            Retirement::WithinGrace,
        ),
        (
            json!({ "NativeCleanupRetired": { "attemptId": attempt } }),
            true,
            Retirement::FirstProbeUnavailable,
        ),
        (
            json!({ "NativeCleanupRetired": { "attemptId": attempt } }),
            true,
            Retirement::Expired,
        ),
        (
            json!({ "NativeCleanupRetired": { "attemptId": attempt } }),
            true,
            Retirement::Deferred,
        ),
    ] {
        let after_grace = retirement != Retirement::WithinGrace;
        let mut pending = receipt.clone();
        if after_grace {
            pending["NativeCleanup"]["cleanupStatusProtocol"] =
                crate::runner::NATIVE_CLEANUP_STATUS_PROTOCOL.into();
        }
        let stops = Arc::new(AtomicUsize::new(0));
        let observations = Arc::new(AtomicUsize::new(0));
        let retired_after_grace = Arc::new(AtomicBool::new(false));
        let route_retired = retired_after_grace.clone();
        let route_stops = stops.clone();
        let route_observations = observations.clone();
        let response_attempt = attempt.clone();
        let url = format!("/runs/{attempt}");
        let app = Router::new().fallback(any(move |request: Request<axum::body::Body>| {
            let mut result = pending.clone();
            let mut status = axum::http::StatusCode::OK;
            let mut slow_proof = false;
            match request.method().as_str() {
                "GET" => {
                    assert_eq!(request.uri().path(), format!("{url}/cleanup-status"));
                    assert!(after_grace);
                    let count = route_observations.fetch_add(1, Ordering::Relaxed);
                    if count == 0 && retirement == Retirement::FirstProbeUnavailable {
                        status = axum::http::StatusCode::SERVICE_UNAVAILABLE;
                    }
                    let reusable = route_retired.load(Ordering::Relaxed);
                    result = json!({ "attemptId": response_attempt, "flushed": reusable, "reusable": reusable });
                    slow_proof = retirement == Retirement::Deferred && reusable;
                }
                "DELETE" => {
                    assert_eq!(request.uri().path(), url);
                    let previous = route_stops.fetch_add(1, Ordering::Relaxed);
                    if previous > 0 {
                        assert!(!after_grace);
                        result = final_stop.clone();
                    }
                }
                "POST" => {
                    assert_eq!(request.uri().path(), format!("{url}/wait"));
                    assert_eq!(
                        request.uri().query(),
                        Some(crate::runner::NATIVE_CLEANUP_RETIREMENT_QUERY)
                    );
                    let observed = route_observations.fetch_add(1, Ordering::Relaxed);
                    assert!(!after_grace);
                    if observed > 0 {
                        result["NativeCleanup"]["retired"] = true.into();
                    }
                }
                method => panic!("Unexpected fixture request: {method}"),
            }
            async move {
                if slow_proof {
                    tokio::time::sleep(Duration::from_secs(3)).await;
                }
                (status, Json(result))
            }
        }));
        let (_root, service, server) = fence_service(app).await;
        let id = crate::config::id();
        let fixture_id = id.clone();
        let attempt = attempt.clone();
        let saved = service
            .store
            .transaction(move |db| {
                db.add_run_fixture(
                    &json!({
                        "id": fixture_id, "taskId": fixture_id, "trigger": "chat",
                        "status": "succeeded", "summary": "Completed reply", "isolated": true,
                        "recoveryPending": true, "finishedAt": 42
                    }),
                    None,
                )?;
                let mut saved = RunCheckpoint::from_value(json!({
                    "runnerId": attempt, "prepared": { "backend": "native", "isolated": true },
                    "remainingMs": null,
                }))?;
                let state = db.run_state(&fixture_id)?.unwrap();
                saved.settled = Some(Some(Settled::from_state(&state)));
                saved.store_in(db, &fixture_id)?;
                Ok(saved)
            })
            .await
            .unwrap();
        let checkpoint = Checkpoint::new(service.store.clone(), id.clone(), saved);
        checkpoint
            .fence(&service, &service.store.run(&id).await.unwrap())
            .await
            .unwrap();
        if !after_grace && cleared {
            assert!(
                RunCheckpoint::load(&service.store, &id)
                    .await
                    .unwrap()
                    .unwrap()
                    .native_flush_confirmed()
            );
        }
        if after_grace {
            let saved = RunCheckpoint::load(&service.store, &id)
                .await
                .unwrap()
                .unwrap();
            assert!(saved.pending_native_cleanup().unwrap().needs_observation());
            assert_eq!(observations.load(Ordering::Relaxed), 0);
        }
        release(&service, &checkpoint, None, true).await.unwrap();
        let state = service.store.run_state(&id).await.unwrap();
        if cleared {
            assert_eq!(state.status, RunStatus::Succeeded);
            assert_eq!(state.summary, "Completed reply");
            assert!(state.error.is_none());
        } else {
            assert_eq!(state.status, RunStatus::Failed);
            assert_eq!(
                state.error.as_deref(),
                Some("Native disk cleanup is still in progress.")
            );
        }
        if after_grace {
            assert!(state.recovery_pending);
            assert_eq!(state.finished_at, Some(42));
            assert_eq!(stops.load(Ordering::Relaxed), 1);
            super::super::schedule::observe_native_cleanup(&service)
                .await
                .unwrap();
            wait_for_observer(&service).await;
            let still_pending = service.store.run_state(&id).await.unwrap();
            assert_eq!(still_pending.status, RunStatus::Succeeded);
            assert!(still_pending.recovery_pending);
            assert_eq!(stops.load(Ordering::Relaxed), 1);
            assert_eq!(observations.load(Ordering::Relaxed), 1);

            if retirement == Retirement::Expired {
                let saved = RunCheckpoint::load(&service.store, &id)
                    .await
                    .unwrap()
                    .unwrap();
                let mut saved = serde_json::to_value(saved).unwrap();
                saved["pendingNativeCleanup"]["until"] = json!(0);
                RunCheckpoint::from_value(saved)
                    .unwrap()
                    .store(&service.store, &id)
                    .await
                    .unwrap();
                let run = id.clone();
                service
                    .store
                    .transaction(move |db| {
                        db.save_conversation_boundary(&json!({ "id": "chat", "runId": run }))?;
                        db.put_message(&json!({
                            "id": "follow-up", "chatId": "chat", "status": "queued",
                            "createdAt": 43, "text": "A later message",
                        }))?;
                        crate::conversation_writers::record_message(
                            db,
                            "chat",
                            "follow-up",
                            &crate::conversation_writers::EntryWriter::Person("author".into()),
                        )?;
                        Ok(())
                    })
                    .await
                    .unwrap();
            }
            let before_verdict = observations.load(Ordering::Relaxed);
            retired_after_grace.store(true, Ordering::Relaxed);
            if retirement != Retirement::Expired {
                let preparation = service.worker.run_preparation_lock(&id).await;
                let _preparation = preparation.lock().await;
                tokio::time::timeout(
                    Duration::from_secs(1),
                    super::super::schedule::observe_native_cleanup(&service),
                )
                .await
                .unwrap()
                .unwrap();
                wait_for_observer(&service).await;
            }
            let after_verdict = observations.load(Ordering::Relaxed);

            if retirement == Retirement::Expired {
                assert_eq!(before_verdict, after_verdict);
            } else {
                assert_eq!(before_verdict + 1, after_verdict);
            }
            assert_eq!(stops.load(Ordering::Relaxed), 1);
            // A restart after storing the verdict must settle from durable evidence
            // even if the controller has disconnected.
            assert!(service.store.run_state(&id).await.unwrap().recovery_pending);
            server.abort();
            let unrelated = crate::config::id();
            let fixture = unrelated.clone();
            service.store.transaction(move |db| {
                db.add_run_fixture(&json!({
                    "id": fixture, "taskId": fixture, "status": "succeeded", "recoveryPending": true,
                }), None)?;
                Ok(())
            }).await.unwrap();
            let preparation = service.worker.run_preparation_lock(&unrelated).await;
            let _unrelated_preparation = preparation.lock().await;
            let normal = service.clone();
            let ordinary = tokio::spawn(async move {
                super::super::schedule::recover_completed_environments(&normal).await
            });
            tokio::time::timeout(Duration::from_secs(5), async {
                super::super::schedule::observe_native_cleanup(&service)
                    .await
                    .unwrap();
                let preparing = service.worker.run_preparation_lock(&id).await;
                drop(preparing.lock().await);
            })
            .await
            .unwrap();
            assert!(!ordinary.is_finished());
            ordinary.abort();
            let settled = service.store.run_state(&id).await.unwrap();
            if retirement == Retirement::Expired {
                assert_eq!(settled.status, RunStatus::Failed);
                assert_eq!(
                    settled.error.as_deref(),
                    Some("Native disk cleanup is still in progress.")
                );
                assert_eq!(settled.finished_at, Some(42));
                service
                    .chat_tick(&std::collections::HashSet::new())
                    .await
                    .unwrap();
                service
                    .store
                    .read(|db| {
                        let queued = &db.messages("chat")?[0];
                        assert_eq!(queued["status"], "queued");
                        assert!(queued["returnedTo"].is_null());
                        Ok(())
                    })
                    .await
                    .unwrap();
            } else {
                assert_eq!(settled.status, RunStatus::Succeeded);
                assert_eq!(settled.summary, "Completed reply");
                assert_eq!(settled.finished_at, Some(42));
                assert!(settled.error.is_none());
            }
            assert!(!settled.recovery_pending);
            assert_eq!(observations.load(Ordering::Relaxed), after_verdict);
            let saved = RunCheckpoint::load(&service.store, &id)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(
                saved
                    .pending_native_cleanup()
                    .is_some_and(crate::recovery::PendingNativeCleanup::awaiting_retirement,),
                retirement == Retirement::Expired,
            );
        } else {
            assert_eq!(observations.load(Ordering::Relaxed), 2);
            assert_eq!(stops.load(Ordering::Relaxed), 2);
        }
        service.shutdown.cancel();
        server.abort();
    }
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
            super::super::output::Attempt::default(),
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
