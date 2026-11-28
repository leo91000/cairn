use super::*;

#[tokio::test]
async fn failed_provider_startup_can_retry_without_replaying_an_uncertain_launch() {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    for (provider, rejected, deferred_fence) in [
        ("claude", true, true),
        ("claude", true, false),
        ("claude", false, false),
        ("codex", true, false),
    ] {
        let mut fixture = Fixture::new().await;
        fixture.stop(false).await;
        let controller = Arc::new(Controller {
            data: fixture.service.config.data_dir.clone(),
            plans: tokio::sync::Mutex::new(Vec::new()),
            failures: 0,
            slots: 4,
            hold: AtomicBool::new(false),
        });
        let failing = Arc::new(AtomicBool::new(false));
        let failures = failing.clone();
        let fence_failures = Arc::new(AtomicUsize::new(0));
        let app = Router::new()
            .fallback(any(move |state: State<Arc<Controller>>, request: Request| {
                let failing = failures.clone();
                let fence_failures = fence_failures.clone();
                async move {
                    let path = request.uri().path();
                    let starting = request.method() == "POST"
                        && path.starts_with("/runs/")
                        && path.trim_matches('/').split('/').count() == 2;
                    let reject_start = rejected && starting;
                    let lose_output = !rejected && path.ends_with("/logs");
                    let fence_failed = request.method() == "DELETE"
                        && fence_failures
                            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| {
                                remaining.checked_sub(1)
                            })
                            .is_ok();
                    if fence_failed {
                        return StatusCode::SERVICE_UNAVAILABLE.into_response();
                    }
                    if failing.load(Ordering::SeqCst) && (reject_start || lose_output) {
                        if deferred_fence && reject_start {
                            // Client cleanup and the first worker fence fail;
                            // execution cleanup must settle the rejection later.
                            fence_failures.store(2, Ordering::SeqCst);
                        }
                        return (
                            StatusCode::SERVICE_UNAVAILABLE,
                            Json(json!({
                                "error": "Temporary admission failure (Bearer fixture-private-token).",
                            })),
                        ).into_response();
                    }
                    serve_controller(state, request).await
                }
            }))
            .with_state(controller.clone());
        let (listener, address) = common::bind().await;
        common::reconfigure(&mut fixture.service, |config| {
            config.runner_url = format!("http://{address}");
        })
        .await;
        let server = common::serve(listener, app);
        let s = fixture.service.clone();
        claude_account(&s, 4).await;
        let home = s.config.home.join(".codex");
        tokio::fs::create_dir_all(&home).await.unwrap();
        tokio::fs::write(home.join("auth.json"), "{}")
            .await
            .unwrap();
        fixture.start().await;

        let original = message("Keep the existing workspace and decisions.");
        let chat_id = fixture.start_chat(original).await;
        let run_id = fixture.chat_run(&chat_id).await;
        let first = fixture
            .until(&run_id, |run| run["status"] == RunStatus::Succeeded)
            .await;
        let marker = Path::new(text(&first, "workspace")).join("preserved.txt");
        std::fs::write(&marker, "completed work").unwrap();

        failing.store(true, Ordering::SeqCst);
        let mut next = message("Continue the same work.");
        next["provider"] = provider.into();
        s.chat_send(&chat_id, next.clone()).await.unwrap();
        let failed = fixture
            .until(&run_id, |run| run["status"] == RunStatus::Failed)
            .await;
        let new_session = provider == "claude";
        assert_eq!(failed["sessionId"].is_null(), new_session);
        assert_eq!(
            fixture.checkpoint(&run_id).await["launched"],
            !new_session || !rejected
        );
        assert_eq!(failed["workspace"], first["workspace"]);
        assert_eq!(std::fs::read_to_string(&marker).unwrap(), "completed work");
        if rejected {
            let id = run_id.clone();
            let events = s
                .store
                .read(move |db| db.events_fixture(&id, 0, 500))
                .await
                .unwrap();
            assert!(events.iter().any(|event| {
                text(event, "text").contains("HTTP 503")
                    && text(event, "text").contains("Temporary admission failure")
            }));
            assert!(
                !events
                    .iter()
                    .any(|event| text(event, "text").contains("fixture-private-token"))
            );
        }
        fixture
            .until_chat(&chat_id, async |chat| {
                chat["paused"] == true && chat["messages"][1]["status"] == "queued"
            })
            .await;

        failing.store(false, Ordering::SeqCst);
        let router = common::relay_fixture::router(s.clone()).await.unwrap();
        let session = common::RelayContext::new(&common::relay_fixture::context(&s).await);
        let request = session
            .authorize(common::request(
                "POST",
                &format!("/api/chats/{chat_id}/pause"),
            ))
            .header("content-type", "application/json")
            .body(Body::from(json!({ "paused": false }).to_string()))
            .unwrap();
        let response = common::send(&router, request).await;
        if new_session && !rejected {
            assert_eq!(response.status(), StatusCode::CONFLICT);
            assert_eq!(s.chat_detail(&chat_id).await.unwrap()["paused"], true);
        } else {
            assert_eq!(
                response.status(),
                StatusCode::OK,
                "{}",
                common::read_json(response).await
            );
            let completed = fixture
                .until(&run_id, |run| {
                    run["status"] == RunStatus::Succeeded
                        && run["chatExecution"]["messageId"] == next["id"]
                })
                .await;
            assert_eq!(completed["workspace"], first["workspace"]);
            assert_eq!(std::fs::read_to_string(&marker).unwrap(), "completed work");

            let chat = s.chat_detail(&chat_id).await.unwrap();
            assert_eq!(chat["messages"].as_array().unwrap().len(), 2);
            assert_eq!(chat["messages"][1]["id"], next["id"]);
            assert_eq!(chat["messages"][1]["status"], "delivered");

            let plans = controller.plans.lock().await;
            let accepted_attempts = plans
                .iter()
                .filter(|plan| plan["chat"]["execution"]["messageId"] == next["id"])
                .count();
            assert_eq!(
                accepted_attempts, 1,
                "The interrupted message must be accepted exactly once",
            );
            let retried = plans.last().unwrap();
            assert_eq!(retried["chat"]["sessionId"].is_null(), new_session);
            assert_eq!(retried["chat"]["execution"]["recovery"], !new_session);
            if new_session {
                assert!(
                    retried["chat"]["execution"]["context"]
                        .as_str()
                        .unwrap()
                        .contains("Keep the existing workspace")
                );
            }
        }
        fixture.stop(false).await;
        server.abort();
    }
}

#[tokio::test]
async fn controller_interruptions_resume_saved_threads_and_stop_after_three_recoveries() {
    for failures in [1, usize::MAX] {
        let mut fixture = Fixture::new().await;
        fixture.stop(false).await;
        let controller = Arc::new(Controller {
            data: fixture.service.config.data_dir.clone(),
            plans: tokio::sync::Mutex::new(Vec::new()),
            failures,
            slots: 4,
            hold: std::sync::atomic::AtomicBool::new(false),
        });
        let app = Router::new()
            .fallback(any(serve_controller))
            .with_state(controller.clone());
        let (listener, address) = common::bind().await;
        common::reconfigure(&mut fixture.service, |config| {
            config.runner_url = format!("http://{address}");
        })
        .await;
        let server = common::serve(listener, app);
        let home = fixture.service.config.home.join(".codex");
        tokio::fs::create_dir_all(&home).await.unwrap();
        tokio::fs::write(home.join("auth.json"), "{}")
            .await
            .unwrap();
        fixture.start().await;
        let run = fixture.enqueue("Inspect the VM fixture").await;
        let run_id = text(&run, "id");
        let completed = fixture.until_finished(run_id).await;
        let (expected, attempts) = if failures == 1 {
            (RunStatus::Succeeded, 2)
        } else {
            (RunStatus::Failed, 4)
        };
        assert_eq!(completed["status"], expected, "{completed}");
        assert_eq!(completed["sessionId"], "fixture-session");
        let plans = controller.plans.lock().await;
        assert_eq!(plans.len(), attempts);
        for plan in plans.iter().skip(1) {
            assert_eq!(plan["chat"]["sessionId"], "fixture-session", "{plan}");
            assert_eq!(plan["runId"], run_id);
            assert_eq!(plan["cwd"], plans[0]["cwd"]);
        }
        drop(plans);
        fixture.stop(false).await;
        server.abort();
    }
}

/// Exercises the real manager -> runner client -> disposable HTTP controller.
/// Even at the replay ceiling a read of the same attempt is permitted.
#[tokio::test]
async fn controller_evidence_is_reconciled_before_replay_or_terminal_failure() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    for (completed, terminal, expected) in [
        (true, 0, RunStatus::Succeeded),
        (true, 70, RunStatus::Failed),
        (false, 75, RunStatus::Failed),
    ] {
        let mut fixture = Fixture::new().await;
        fixture.stop(false).await;
        let controller = Arc::new(Controller {
            data: fixture.service.config.data_dir.clone(),
            plans: tokio::sync::Mutex::new(Vec::new()),
            failures: if completed { 0 } else { usize::MAX },
            slots: 4,
            hold: std::sync::atomic::AtomicBool::new(false),
        });
        let waits = Arc::new(AtomicUsize::new(0));
        let count = waits.clone();
        let app = Router::new()
            .fallback(any(
                move |state: State<Arc<Controller>>, request: Request| {
                    let count = count.clone();
                    async move {
                        if request.uri().path().ends_with("/wait") {
                            let observation = count.fetch_add(1, Ordering::SeqCst);
                            // The client's completion channel was cut; the manager can
                            // subsequently read the already available controller exit.
                            let code = if observation == 0 { 75 } else { terminal };
                            return Json(json!({ "StatusCode": code })).into_response();
                        }
                        serve_controller(state, request).await
                    }
                },
            ))
            .with_state(controller.clone());
        let (listener, address) = common::bind().await;
        common::reconfigure(&mut fixture.service, |config| {
            config.runner_url = format!("http://{address}");
        })
        .await;
        let server = common::serve(listener, app);
        let home = fixture.service.config.home.join(".codex");
        tokio::fs::create_dir_all(&home).await.unwrap();
        tokio::fs::write(home.join("auth.json"), "{}")
            .await
            .unwrap();
        let run = fixture
            .enqueue("Reconcile only this disposable controller attempt")
            .await;
        let run_id = text(&run, "id");
        fixture
            .service
            .store
            .transaction({
                let run_id = run_id.to_owned();
                move |db| {
                    db.save_checkpoint_boundary(
                        &run_id,
                        &json!({ "controllerRecoveries": 3, "remainingMs": null }),
                    )
                }
            })
            .await
            .unwrap();
        fixture.start().await;
        let result = fixture.until_finished(run_id).await;
        assert_eq!(result["status"], expected, "{result}");
        assert_eq!(
            controller.plans.lock().await.len(),
            1,
            "A read must never submit another attempt"
        );
        assert_eq!(fixture.checkpoint(run_id).await["controllerRecoveries"], 3);
        assert!(waits.load(Ordering::SeqCst) >= 2);
        if completed && terminal != 0 {
            assert!(text(&result, "error").contains("finalization"), "{result}");
        }
        fixture.stop(false).await;
        server.abort();
    }
}

/// The real runner client and manager must retain a broken output stream even
/// when the controller reports a healthy physical completion.
#[tokio::test]
async fn interrupted_controller_output_after_healthy_exit_never_replays_the_turn() {
    use std::sync::atomic::{AtomicBool, Ordering};

    for (completed, truncated, clean_eof) in [
        (false, true, false),
        (true, true, false),
        (false, false, false),
        (true, false, false),
        (false, false, true),
        (true, false, true),
    ] {
        let mut fixture = Fixture::new().await;
        fixture.stop(false).await;
        let controller = Arc::new(Controller {
            data: fixture.service.config.data_dir.clone(),
            plans: tokio::sync::Mutex::new(Vec::new()),
            failures: 0,
            slots: 4,
            hold: AtomicBool::new(false),
        });
        let home = fixture.service.config.home.join(".codex");
        tokio::fs::create_dir_all(&home).await.unwrap();
        tokio::fs::write(home.join("auth.json"), "{}")
            .await
            .unwrap();
        let run = fixture.enqueue("Test interrupted controller output").await;
        let run_id = text(&run, "id").to_owned();
        let delivery =
            json!({ "type": "chat.delivered", "messageId": run["chatExecution"]["messageId"] });
        let wait_returned = Arc::new(AtomicBool::new(false));
        let app = Router::new()
            .fallback(any({
                let store = fixture.service.store.clone();
                let id = run_id.clone();
                let wait_returned = wait_returned.clone();
                move |state: State<Arc<Controller>>, request: Request| {
                    let store = store.clone();
                    let id = id.clone();
                    let wait_returned = wait_returned.clone();
                    let delivery = delivery.clone();
                    async move {
                        if request.uri().path().ends_with("/wait") {
                            wait_returned.store(true, Ordering::SeqCst);
                            return Json(json!({ "StatusCode": 0 })).into_response();
                        }
                        if request.uri().path().ends_with("/logs") {
                            let mut output = format!(
                                "{}\n{delivery}\n",
                                json!({
                                    "type": "thread.started",
                                    "thread_id": "fixture-session",
                                })
                            );
                            if completed {
                                output.push_str(&format!(
                                    "{}\n{}\n",
                                    json!({
                                        "type": "item.completed",
                                        "item": {
                                            "id": "reply",
                                            "type": "agent_message",
                                            "text": "committed answer",
                                        },
                                    }),
                                    json!({ "type": "turn.completed", "usage": {} })
                                ));
                            }
                            let frame = format!(
                                "{}\n",
                                json!({
                                    "type": "output",
                                    "stderr": false,
                                    "data": STANDARD.encode(output),
                                })
                            );
                            let stream = futures_util::stream::unfold(
                                (0, store, id, wait_returned, frame),
                                move |(stage, store, id, wait_returned, frame)| async move {
                                    if stage == 0 {
                                        let bytes = axum::body::Bytes::from(frame.clone());
                                        return Some((
                                            Ok::<_, std::io::Error>(bytes),
                                            (1, store, id, wait_returned, frame),
                                        ));
                                    }
                                    if stage == 1 && clean_eof {
                                        return None;
                                    }
                                    if stage == 1 && truncated {
                                        loop {
                                            let completion_committed = !completed
                                                || store
                                                    .checkpoint_boundary(&id)
                                                    .await
                                                    .unwrap()
                                                    .is_some_and(|saved| {
                                                        saved["completed"] == true
                                                    });
                                            if wait_returned.load(Ordering::SeqCst)
                                                && completion_committed
                                            {
                                                break;
                                            }
                                            tokio::time::sleep(Duration::from_millis(10)).await;
                                        }
                                        // Let the immediate /wait response reach the client
                                        // before cutting the next output frame.
                                        tokio::time::sleep(Duration::from_millis(250)).await;
                                        return Some((
                                            Ok(axum::body::Bytes::from_static(b"{\"type\":")),
                                            (2, store, id, wait_returned, frame),
                                        ));
                                    }
                                    if stage == 1 {
                                        return std::future::pending().await;
                                    }
                                    None
                                },
                            );
                            return Body::from_stream(stream).into_response();
                        }
                        serve_controller(state, request).await
                    }
                }
            }))
            .with_state(controller.clone());
        let (listener, address) = common::bind().await;
        common::reconfigure(&mut fixture.service, |config| {
            config.runner_url = format!("http://{address}");
        })
        .await;
        let server = common::serve(listener, app);
        fixture.start().await;
        let result = fixture.until_finished(&run_id).await;
        assert_eq!(
            result["status"],
            if completed {
                RunStatus::Succeeded
            } else {
                RunStatus::Failed
            },
            "completed={completed}, truncated={truncated}: {result}"
        );
        let saved = fixture.checkpoint(&run_id).await;
        assert_eq!(saved["terminalCode"], 0);
        if clean_eof {
            assert!(saved["outputInterrupted"].is_null());
        } else {
            assert_eq!(saved["outputInterrupted"], true);
        }
        assert_eq!(saved["completed"], completed);
        if completed && !clean_eof {
            let id = run_id.clone();
            let events = fixture
                .service
                .store
                .read(move |db| db.events_fixture(&id, 0, 1000))
                .await
                .unwrap();
            let notices = events
                .iter()
                .filter(|event| text(event, "text").contains("Execution output was interrupted"))
                .count();
            assert_eq!(notices, 1, "{events:?}");
            assert!(
                text(&result, "summary").contains("committed answer"),
                "{result}"
            );
        }
        if !completed {
            let expected_error = if clean_eof {
                "completion was recorded"
            } else {
                "output was interrupted"
            };
            assert!(text(&result, "error").contains(expected_error), "{result}");
        }
        assert_eq!(
            controller.plans.lock().await.len(),
            1,
            "must not replay the turn"
        );
        fixture.stop(false).await;
        server.abort();
    }
}

/// Hold the SQLite writer after the manager reserves an observation and before
/// its healthy response can be committed. The live deadline must still win.
#[tokio::test]
async fn active_observation_expiring_during_result_persistence_fails_without_replay() {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    let mut fixture = Fixture::new().await;
    fixture.stop(false).await;
    let controller = Arc::new(Controller {
        data: fixture.service.config.data_dir.clone(),
        plans: tokio::sync::Mutex::new(Vec::new()),
        failures: 0,
        slots: 4,
        hold: AtomicBool::new(false),
    });
    let waits = Arc::new(AtomicUsize::new(0));
    let response_gate = Arc::new(tokio::sync::Semaphore::new(0));
    let app = Router::new()
        .fallback(any({
            let waits = waits.clone();
            let gate = response_gate.clone();
            move |state: State<Arc<Controller>>, request: Request| {
                let waits = waits.clone();
                let gate = gate.clone();
                async move {
                    if request.uri().path().ends_with("/wait") {
                        let index = waits.fetch_add(1, Ordering::SeqCst);
                        if index == 0 {
                            return Json(json!({ "StatusCode": 75 })).into_response();
                        }
                        gate.acquire().await.unwrap().forget();
                        return Json(json!({ "StatusCode": 0 })).into_response();
                    }
                    serve_controller(state, request).await
                }
            }
        }))
        .with_state(controller.clone());
    let (listener, address) = common::bind().await;
    common::reconfigure(&mut fixture.service, |config| {
        config.runner_url = format!("http://{address}");
    })
    .await;
    let server = common::serve(listener, app);
    let home = fixture.service.config.home.join(".codex");
    tokio::fs::create_dir_all(&home).await.unwrap();
    tokio::fs::write(home.join("auth.json"), "{}")
        .await
        .unwrap();
    let run = fixture
        .enqueue("Test expiry during controller result persistence")
        .await;
    let run_id = text(&run, "id").to_owned();
    fixture
        .service
        .store
        .save_checkpoint_boundary(&run_id, json!({ "remainingMs": 15000 }))
        .await
        .unwrap();
    fixture.start().await;
    let saved = eventually(
        Duration::from_secs(12),
        Duration::from_millis(10),
        async || {
            let saved = fixture.checkpoint(&run_id).await;
            (waits.load(Ordering::SeqCst) >= 2
                && saved["completed"] == true
                && saved["controllerObservation"]["inFlight"] == true)
                .then_some(saved)
        },
    )
    .await;
    let remaining = saved["remainingMs"].as_u64().unwrap();
    assert!((300..=15000).contains(&remaining), "{saved}");
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let store = fixture.service.store.clone();
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
    response_gate.add_permits(1);
    tokio::time::sleep(Duration::from_millis(remaining + 250)).await;
    release_tx.send(()).unwrap();
    blocker.await.unwrap();
    let result = fixture.until_finished(&run_id).await;
    assert_eq!(result["status"], RunStatus::Failed, "{result}");
    assert!(text(&result, "error").contains("time limit"), "{result}");
    let saved = fixture.checkpoint(&run_id).await;
    assert_eq!(saved["terminalCode"], 0, "{saved}");
    assert_eq!(saved["remainingMs"], 0, "{saved}");
    assert_eq!(
        controller.plans.lock().await.len(),
        1,
        "must not replay the turn"
    );
    fixture.stop(false).await;
    server.abort();
}

/// Manager SIGTERM is a local stop, not a controller completion observation.
/// A recorded turn must retain the original attempt for reconciliation on restart.
#[tokio::test]
async fn manager_shutdown_between_turn_completion_and_wait_preserves_same_attempt_evidence() {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    for interruption in ["shutdown", "cancel", "signal", "client-error", "timeout"] {
        let cancel = interruption == "cancel";
        let stops_manager = matches!(interruption, "shutdown" | "cancel");
        let mut fixture = Fixture::new().await;
        fixture.stop(false).await;
        let controller = Arc::new(Controller {
            data: fixture.service.config.data_dir.clone(),
            plans: tokio::sync::Mutex::new(Vec::new()),
            failures: 0,
            slots: 4,
            hold: std::sync::atomic::AtomicBool::new(false),
        });
        let waits = Arc::new(AtomicUsize::new(0));
        let count = waits.clone();
        // For the manager stop cases, expose terminal evidence only to the
        // restarted process, even if the first process gets time to probe again.
        let evidence_available = Arc::new(AtomicBool::new(!stops_manager));
        let evidence = evidence_available.clone();
        let app = Router::new()
            .fallback(any(
                move |state: State<Arc<Controller>>, request: Request| {
                    let count = count.clone();
                    let evidence = evidence.clone();
                    async move {
                        if request.uri().path().ends_with("/wait") {
                            let observation = count.fetch_add(1, Ordering::SeqCst);
                            if interruption == "client-error" && observation == 0 {
                                // The initial wait fails decoding. Only the manager's
                                // durably reserved observation may read again.
                                return Json(json!({})).into_response();
                            }
                            if observation == 0 || !evidence.load(Ordering::SeqCst) {
                                // Keep the initial read open. A stopped manager
                                // cannot see evidence reserved for its replacement.
                                return std::future::pending::<Response>().await;
                            }
                            return Json(json!({ "StatusCode": 0 })).into_response();
                        }
                        serve_controller(state, request).await
                    }
                },
            ))
            .with_state(controller.clone());
        let (listener, address) = common::bind().await;
        common::reconfigure(&mut fixture.service, |config| {
            config.runner_url = format!("http://{address}");
        })
        .await;
        let server = common::serve(listener, app);
        let home = fixture.service.config.home.join(".codex");
        tokio::fs::create_dir_all(&home).await.unwrap();
        tokio::fs::write(home.join("auth.json"), "{}")
            .await
            .unwrap();
        let run = fixture
            .enqueue("Stop only this disposable manager after turn.completed")
            .await;
        let run_id = text(&run, "id");
        fixture
            .service
            .store
            .transaction({
                let run_id = run_id.to_owned();
                move |db| {
                    db.save_checkpoint_boundary(
                        &run_id,
                        &json!({
                            "controllerRecoveries": 3,
                            "remainingMs": if interruption == "timeout" { json!(4000) } else { Value::Null },
                        }),
                    )
                }
            })
            .await
            .unwrap();
        fixture.start().await;
        eventually(
            Duration::from_secs(10),
            Duration::from_millis(10),
            async || {
                let saved = fixture
                    .service
                    .store
                    .checkpoint_boundary(run_id)
                    .await
                    .unwrap()?;
                (saved["completed"] == true && waits.load(Ordering::SeqCst) >= 1).then_some(())
            },
        )
        .await;
        if cancel {
            // Persisted user cancellation takes precedence even when shutdown
            // interrupts the client before the controller's healthy exit arrives.
            fixture
                .service
                .store
                .change_run(
                    run_id,
                    cairn_installation::runs::RunChanges {
                        cancel_requested_at: Some(Some(now())),
                        ..Default::default()
                    },
                )
                .await
                .unwrap();
        }
        if interruption == "signal" {
            let saved = fixture.checkpoint(run_id).await;
            kill_fixture_runner_child(&saved).await;
        }
        let mut waits_before_restart = 1;
        if stops_manager {
            fixture.stop(false).await;
            waits_before_restart = waits.load(Ordering::SeqCst);
            let stopped = fixture.service.store.run(run_id).await.unwrap();
            let checkpoint = fixture.checkpoint(run_id).await;
            assert_eq!(checkpoint["completed"], true);
            assert!(
                checkpoint["terminalCode"].is_null(),
                "Local stop must not fabricate terminal proof: {checkpoint}"
            );
            if !cancel {
                assert_eq!(stopped["status"], RunStatus::Queued, "{stopped}");
                assert!(checkpoint["settled"].is_null(), "{checkpoint}");
            }
            evidence_available.store(true, Ordering::SeqCst);
            fixture.start().await;
        }
        if cancel {
            let result = fixture
                .until(run_id, |run| run["status"] == "cancelled")
                .await;
            assert_eq!(result["status"], RunStatus::Cancelled);
            assert_eq!(waits.load(Ordering::SeqCst), waits_before_restart);
        } else if interruption == "timeout" {
            let result = fixture.until_finished(run_id).await;
            assert_eq!(result["status"], RunStatus::Failed, "{result}");
            assert!(text(&result, "error").contains("time limit"), "{result}");
            assert!(fixture.checkpoint(run_id).await["terminalCode"].is_null());
            assert_eq!(waits.load(Ordering::SeqCst), 1);
        } else {
            let result = fixture.until_finished(run_id).await;
            assert_eq!(
                result["status"],
                RunStatus::Succeeded,
                "{interruption}: {result}"
            );
            assert_eq!(fixture.checkpoint(run_id).await["terminalCode"], 0);
            assert_eq!(waits.load(Ordering::SeqCst), waits_before_restart + 1);
        }
        assert_eq!(
            controller.plans.lock().await.len(),
            1,
            "Reconciliation must never launch another agent"
        );
        assert_eq!(fixture.checkpoint(run_id).await["controllerRecoveries"], 3);
        fixture.stop(false).await;
        server.abort();
    }
}

#[tokio::test]
async fn managed_provider_retries_do_not_spend_controller_recoveries() {
    for ending in ["exhausted", "changed", "cancel"] {
        let mut fixture = Fixture::new().await;
        fixture.stop(false).await;
        let controller = Arc::new(Controller {
            data: fixture.service.config.data_dir.clone(),
            plans: tokio::sync::Mutex::new(Vec::new()),
            failures: usize::MAX,
            slots: 4,
            hold: std::sync::atomic::AtomicBool::new(false),
        });
        let app = Router::new().fallback(any(move |state: State<Arc<Controller>>, request: Request| async move {
            let path = request.uri().path().to_owned();
            if path.ends_with("/logs") {
                let attempt = path.split('/').nth(2).unwrap();
                let index = state.attempt_index(attempt).await;
                let error = if ending == "changed" && index >= 1 {
                    json!({ "code": "authentication_error", "message": "Reconnect account" })
                } else {
                    json!({ "code": "server_overloaded", "message": "Provider busy" })
                };
                let output = format!("{}\n{}\n", json!({ "type": "thread.started", "thread_id": "fixture-session" }),
                                     json!({ "type": "turn.failed", "error": error }));
                return Body::from(format!("{}\n", json!({ "type": "output", "stderr": false, "data": STANDARD.encode(output) }))).into_response();
            }
            if path.ends_with("/wait") {
                return Json(json!({ "StatusCode": 1 })).into_response();
            }
            serve_controller(state, request).await
        })).with_state(controller.clone());
        let (listener, address) = common::bind().await;
        common::reconfigure(&mut fixture.service, |config| {
            config.runner_url = format!("http://{address}");
        })
        .await;
        let server = common::serve(listener, app);
        let home = fixture.service.config.home.join(".codex");
        tokio::fs::create_dir_all(&home).await.unwrap();
        tokio::fs::write(home.join("auth.json"), "{}")
            .await
            .unwrap();
        fixture.start().await;
        let chat = fixture
            .start_chat(message("Exercise a confirmed provider retry"))
            .await;
        let run_id = fixture.chat_run(&chat).await;
        let attempts = if ending == "exhausted" { 5 } else { 1 };
        for retry_number in 1..=attempts {
            let waiting = fixture
                .until(&run_id, |run| {
                    (run["status"] == RunStatus::Queued && run["retry"]["attempt"] == retry_number)
                        || finished(run)
                })
                .await;
            assert_eq!(waiting["status"], RunStatus::Queued, "{waiting}");
            let saved = fixture.checkpoint(&run_id).await;
            assert_eq!(saved["prepared"]["backend"], "firecracker");
            assert_eq!(
                saved["controllerRecoveries"].as_u64().unwrap_or(0),
                0,
                "{saved}"
            );
            if ending == "cancel" {
                fixture
                    .service
                    .worker
                    .cancel(&fixture.service, &run_id)
                    .await
                    .unwrap();
                let stopped = fixture
                    .until(&run_id, |run| run["status"] == RunStatus::Cancelled)
                    .await;
                assert_eq!(stopped["status"], RunStatus::Cancelled);
                break;
            }
            let mut retry = waiting["retry"].clone();
            retry["nextAttemptAt"] = now().into();
            fixture
                .service
                .store
                .change_run(
                    &run_id,
                    cairn_installation::runs::RunChanges {
                        retry: Some(Some(serde_json::from_value(retry).unwrap())),
                        ..Default::default()
                    },
                )
                .await
                .unwrap();
        }
        if ending != "cancel" {
            let failed = fixture.until_finished(&run_id).await;
            assert_eq!(failed["status"], RunStatus::Failed, "{failed}");
            assert_eq!(failed["retry"]["attempt"], attempts);
            assert_eq!(
                failed["summary"],
                if ending == "changed" {
                    "Reconnect account"
                } else {
                    "Provider busy"
                }
            );
        }
        assert_eq!(
            controller.plans.lock().await.len(),
            if ending == "exhausted" {
                6
            } else if ending == "changed" {
                2
            } else {
                1
            }
        );
        fixture.stop(false).await;
        server.abort();
    }
}

#[tokio::test]
async fn confirmed_provider_failure_survives_later_output_truncation_and_restart() {
    use std::sync::atomic::AtomicBool;
    for terminal in [1, 70] {
        let mut fixture = Fixture::new().await;
        fixture.stop(false).await;
        let controller = Arc::new(Controller {
            data: fixture.service.config.data_dir.clone(),
            plans: tokio::sync::Mutex::new(Vec::new()),
            failures: 0,
            slots: 4,
            hold: AtomicBool::new(false),
        });
        let app = Router::new().fallback(any(move |state: State<Arc<Controller>>, request: Request| async move {
        let path = request.uri().path().to_owned();
        if path.ends_with("/logs") || path.ends_with("/wait") {
            let attempt = path.split('/').nth(2).unwrap();
            if state.attempt_index(attempt).await == 0 {
                if path.ends_with("/wait") {
                    return Json(json!({ "StatusCode": terminal })).into_response();
                }
                let mut output = format!("{}\n{}\n",
                    json!({ "type": "thread.started", "thread_id": "fixture-session" }),
                    json!({ "type": "turn.failed", "error": { "code": "server_overloaded", "message": "Provider busy" } }));
                if terminal == 70 {
                    output.push_str(&format!("{}\n", json!({ "type": "runner.failed", "error": { "message": "Finalization failed: EIO/EROFS" } })));
                }
                let frame = json!({ "type": "output", "stderr": false, "data": STANDARD.encode(output) });
                return Body::from(format!("{frame}\n{{\"type\":" )).into_response();
            }
        }
        serve_controller(state, request).await
    })).with_state(controller.clone());
        let (listener, address) = common::bind().await;
        common::reconfigure(&mut fixture.service, |config| {
            config.runner_url = format!("http://{address}");
        })
        .await;
        let server = common::serve(listener, app);
        let home = fixture.service.config.home.join(".codex");
        tokio::fs::create_dir_all(&home).await.unwrap();
        tokio::fs::write(home.join("auth.json"), "{}")
            .await
            .unwrap();
        fixture.start().await;
        let chat = fixture
            .start_chat(message("Retry after a confirmed provider failure"))
            .await;
        let run_id = fixture.chat_run(&chat).await;
        let waiting = fixture
            .until(&run_id, |run| {
                finished(run)
                    || (run["status"] == RunStatus::Queued && run["retry"]["attempt"] == 1)
            })
            .await;
        if terminal == 70 {
            assert_eq!(waiting["status"], RunStatus::Failed, "{waiting}");
            assert!(waiting["retry"].is_null());
            assert_eq!(controller.plans.lock().await.len(), 1);
            let saved = fixture.checkpoint(&run_id).await;
            assert_eq!(saved["terminalCode"], 70);
            assert_eq!(saved["outputInterrupted"], true);
            fixture.stop(false).await;
            server.abort();
            continue;
        }
        assert_eq!(waiting["status"], RunStatus::Queued, "{waiting}");
        assert_eq!(waiting["retry"]["cause"]["message"], "Provider busy");
        fixture.stop(false).await;
        let mut saved = fixture.checkpoint(&run_id).await;
        assert_eq!(saved["outputInterrupted"], true);
        assert_eq!(saved["retryCause"]["message"], "Provider busy");
        assert_eq!(saved["providerRetryReservation"], saved["runnerId"]);
        assert_eq!(saved["controllerRecoveries"].as_u64().unwrap_or(0), 0);
        // Model a crash after the observed failure and broken output were committed,
        // but before record_failure paid for the retry. Recovery must fence and pay it.
        saved
            .as_object_mut()
            .unwrap()
            .remove("providerRetryReservation");
        fixture
            .service
            .store
            .save_checkpoint_boundary(&run_id, saved)
            .await
            .unwrap();
        fixture
            .service
            .store
            .change_run(
                &run_id,
                cairn_installation::runs::RunChanges {
                    retry: Some(None),
                    recovery_pending: Some(true),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        fixture.start().await;
        let waiting = fixture
            .until(&run_id, |run| {
                finished(run)
                    || (run["status"] == RunStatus::Queued && run["retry"]["attempt"] == 1)
            })
            .await;
        assert_eq!(waiting["status"], RunStatus::Queued, "{waiting}");
        fixture.stop(false).await;
        let recovered = fixture.checkpoint(&run_id).await;
        assert_eq!(recovered["providerRetryReservation"], recovered["runnerId"]);
        assert_eq!(recovered["controllerRecoveries"].as_u64().unwrap_or(0), 0);
        let mut retry = waiting["retry"].clone();
        retry["nextAttemptAt"] = now().into();
        fixture
            .service
            .store
            .change_run(
                &run_id,
                cairn_installation::runs::RunChanges {
                    retry: Some(Some(serde_json::from_value(retry).unwrap())),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        fixture.start().await;
        let result = fixture.until_finished(&run_id).await;
        assert_eq!(result["status"], RunStatus::Succeeded, "{result}");
        assert_eq!(controller.plans.lock().await.len(), 2);
        let id = run_id.clone();
        let events = fixture
            .service
            .store
            .read(move |db| db.events_fixture(&id, 0, 1000))
            .await
            .unwrap();
        assert_eq!(
            events
                .iter()
                .filter(|event| text(event, "text").contains("Execution output was interrupted"))
                .count(),
            1
        );
        fixture.stop(false).await;
        server.abort();
    }
}

#[tokio::test]
async fn native_failed_flush_after_provider_failure_never_replays_with_intact_logs() {
    use std::sync::atomic::AtomicBool;
    let mut fixture = Fixture::new().await;
    fixture.stop(false).await;
    let controller = Arc::new(Controller {
        data: fixture.service.config.data_dir.clone(),
        plans: tokio::sync::Mutex::new(Vec::new()),
        failures: 0,
        slots: 4,
        hold: AtomicBool::new(false),
    });
    let app = Router::new().fallback(any(move |state: State<Arc<Controller>>, request: Request| async move {
        let path = request.uri().path();
        if path.ends_with("/wait") { return Json(json!({ "StatusCode": 70, "FinalizationFailed": true })).into_response(); }
        if path.ends_with("/logs") {
            let output = format!("{}\n{}\n", json!({ "type": "thread.started", "thread_id": "fixture-session" }), json!({ "type": "turn.failed", "error": { "code": "server_overloaded", "message": "Provider busy" } }));
            return Body::from(format!("{}\n", json!({ "type": "output", "stderr": false, "data": STANDARD.encode(output) }))).into_response();
        }
        serve_controller(state, request).await
    })).with_state(controller.clone());
    let (listener, address) = common::bind().await;
    common::reconfigure(&mut fixture.service, |config| {
        config.runner_url = format!("http://{address}");
    })
    .await;
    let server = common::serve(listener, app);
    let home = fixture.service.config.home.join(".codex");
    tokio::fs::create_dir_all(&home).await.unwrap();
    tokio::fs::write(home.join("auth.json"), "{}")
        .await
        .unwrap();
    fixture.start().await;
    let chat = fixture
        .start_chat(message("Do not replay a failed flush"))
        .await;
    let id = fixture.chat_run(&chat).await;
    let result = fixture
        .until(&id, |run| {
            finished(run)
                || controller
                    .plans
                    .try_lock()
                    .is_ok_and(|plans| plans.len() > 1)
        })
        .await;
    assert_eq!(result["status"], RunStatus::Failed, "{result}");
    assert_eq!(controller.plans.lock().await.len(), 1);
    assert!(text(&result, "error").contains("flush"), "{result}");
    let saved = fixture.checkpoint(&id).await;
    assert!(saved["outputInterrupted"].is_null());
    assert_eq!(saved["finalizationFailureAttempt"], saved["runnerId"]);
    let events = fixture
        .service
        .store
        .read(move |db| db.events_fixture(&id, 0, 1000))
        .await
        .unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|event| text(event, "text").contains("filesystem could not be flushed"))
            .count(),
        1
    );
    fixture.stop(false).await;
    server.abort();
}

/// Exercises the real manager -> runner client -> disposable HTTP controller.
/// Even at the replay ceiling a read of the same attempt is permitted.
#[tokio::test]
async fn controller_evidence_is_reconciled_before_replay_or_terminal_failure_reconciliation() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    for (completed, terminal, expected) in [
        (true, 0, RunStatus::Succeeded),
        (true, 70, RunStatus::Failed),
        (false, 75, RunStatus::Failed),
    ] {
        let mut fixture = Fixture::new().await;
        fixture.stop(false).await;
        let controller = Arc::new(Controller {
            data: fixture.service.config.data_dir.clone(),
            plans: tokio::sync::Mutex::new(Vec::new()),
            failures: if completed { 0 } else { usize::MAX },
            slots: 4,
            hold: std::sync::atomic::AtomicBool::new(false),
        });
        let waits = Arc::new(AtomicUsize::new(0));
        let count = waits.clone();
        let app = Router::new()
            .fallback(any(
                move |state: State<Arc<Controller>>, request: Request| {
                    let count = count.clone();
                    async move {
                        if request.uri().path().ends_with("/wait") {
                            let observation = count.fetch_add(1, Ordering::SeqCst);
                            // The client's completion channel was cut; the manager can
                            // subsequently read the already available controller exit.
                            let code = if observation == 0 { 75 } else { terminal };
                            return Json(json!({ "StatusCode": code })).into_response();
                        }
                        serve_controller(state, request).await
                    }
                },
            ))
            .with_state(controller.clone());
        let (listener, address) = common::bind().await;
        common::reconfigure(&mut fixture.service, |config| {
            config.runner_url = format!("http://{address}");
        })
        .await;
        let server = common::serve(listener, app);
        let home = fixture.service.config.home.join(".codex");
        tokio::fs::create_dir_all(&home).await.unwrap();
        tokio::fs::write(home.join("auth.json"), "{}")
            .await
            .unwrap();
        let run = fixture
            .enqueue("Reconcile only this disposable controller attempt")
            .await;
        let run_id = text(&run, "id");
        fixture
            .service
            .store
            .transaction({
                let run_id = run_id.to_owned();
                move |db| {
                    db.save_checkpoint_boundary(
                        &run_id,
                        &json!({ "controllerRecoveries": 3, "remainingMs": null }),
                    )
                }
            })
            .await
            .unwrap();
        fixture.start().await;
        let result = fixture.until_finished(run_id).await;
        assert_eq!(result["status"], expected, "{result}");
        assert_eq!(
            controller.plans.lock().await.len(),
            1,
            "A read must never submit another attempt"
        );
        assert_eq!(fixture.checkpoint(run_id).await["controllerRecoveries"], 3);
        assert!(waits.load(Ordering::SeqCst) >= 2);
        if completed && terminal != 0 {
            assert!(text(&result, "error").contains("finalization"), "{result}");
        }
        fixture.stop(false).await;
        server.abort();
    }
}

/// The real runner client and manager must retain a broken output stream even
/// when the controller reports a healthy physical completion.
#[tokio::test]
async fn interrupted_controller_output_after_healthy_exit_never_replays_the_turn_reconciliation() {
    use std::sync::atomic::{AtomicBool, Ordering};

    for (completed, truncated, clean_eof) in [
        (false, true, false),
        (true, true, false),
        (false, false, false),
        (true, false, false),
        (false, false, true),
        (true, false, true),
    ] {
        let mut fixture = Fixture::new().await;
        fixture.stop(false).await;
        let controller = Arc::new(Controller {
            data: fixture.service.config.data_dir.clone(),
            plans: tokio::sync::Mutex::new(Vec::new()),
            failures: 0,
            slots: 4,
            hold: AtomicBool::new(false),
        });
        let home = fixture.service.config.home.join(".codex");
        tokio::fs::create_dir_all(&home).await.unwrap();
        tokio::fs::write(home.join("auth.json"), "{}")
            .await
            .unwrap();
        let run = fixture.enqueue("Test interrupted controller output").await;
        let run_id = text(&run, "id").to_owned();
        let delivery =
            json!({ "type": "chat.delivered", "messageId": run["chatExecution"]["messageId"] });
        let wait_returned = Arc::new(AtomicBool::new(false));
        let app = Router::new()
            .fallback(any({
                let store = fixture.service.store.clone();
                let id = run_id.clone();
                let wait_returned = wait_returned.clone();
                move |state: State<Arc<Controller>>, request: Request| {
                    let store = store.clone();
                    let id = id.clone();
                    let wait_returned = wait_returned.clone();
                    let delivery = delivery.clone();
                    async move {
                        if request.uri().path().ends_with("/wait") {
                            wait_returned.store(true, Ordering::SeqCst);
                            return Json(json!({ "StatusCode": 0 })).into_response();
                        }
                        if request.uri().path().ends_with("/logs") {
                            let mut output = format!(
                                "{}\n{delivery}\n",
                                json!({
                                    "type": "thread.started",
                                    "thread_id": "fixture-session",
                                })
                            );
                            if completed {
                                output.push_str(&format!(
                                    "{}\n{}\n",
                                    json!({
                                        "type": "item.completed",
                                        "item": {
                                            "id": "reply",
                                            "type": "agent_message",
                                            "text": "committed answer",
                                        },
                                    }),
                                    json!({ "type": "turn.completed", "usage": {} })
                                ));
                            }
                            let frame = format!(
                                "{}\n",
                                json!({
                                    "type": "output",
                                    "stderr": false,
                                    "data": STANDARD.encode(output),
                                })
                            );
                            let stream = futures_util::stream::unfold(
                                (0, store, id, wait_returned, frame),
                                move |(stage, store, id, wait_returned, frame)| async move {
                                    if stage == 0 {
                                        let bytes = axum::body::Bytes::from(frame.clone());
                                        return Some((
                                            Ok::<_, std::io::Error>(bytes),
                                            (1, store, id, wait_returned, frame),
                                        ));
                                    }
                                    if stage == 1 && clean_eof {
                                        return None;
                                    }
                                    if stage == 1 && truncated {
                                        loop {
                                            let completion_committed = !completed
                                                || store
                                                    .checkpoint_boundary(&id)
                                                    .await
                                                    .unwrap()
                                                    .is_some_and(|saved| {
                                                        saved["completed"] == true
                                                    });
                                            if wait_returned.load(Ordering::SeqCst)
                                                && completion_committed
                                            {
                                                break;
                                            }
                                            tokio::time::sleep(Duration::from_millis(10)).await;
                                        }
                                        // Let the immediate /wait response reach the client
                                        // before cutting the next output frame.
                                        tokio::time::sleep(Duration::from_millis(250)).await;
                                        return Some((
                                            Ok(axum::body::Bytes::from_static(b"{\"type\":")),
                                            (2, store, id, wait_returned, frame),
                                        ));
                                    }
                                    if stage == 1 {
                                        return std::future::pending().await;
                                    }
                                    None
                                },
                            );
                            return Body::from_stream(stream).into_response();
                        }
                        serve_controller(state, request).await
                    }
                }
            }))
            .with_state(controller.clone());
        let (listener, address) = common::bind().await;
        common::reconfigure(&mut fixture.service, |config| {
            config.runner_url = format!("http://{address}");
        })
        .await;
        let server = common::serve(listener, app);
        fixture.start().await;
        let result = fixture.until_finished(&run_id).await;
        assert_eq!(
            result["status"],
            if completed {
                RunStatus::Succeeded
            } else {
                RunStatus::Failed
            },
            "completed={completed}, truncated={truncated}: {result}"
        );
        let saved = fixture.checkpoint(&run_id).await;
        assert_eq!(saved["terminalCode"], 0);
        if clean_eof {
            assert!(saved["outputInterrupted"].is_null());
        } else {
            assert_eq!(saved["outputInterrupted"], true);
        }
        assert_eq!(saved["completed"], completed);
        if completed && !clean_eof {
            let id = run_id.clone();
            let events = fixture
                .service
                .store
                .read(move |db| db.events_fixture(&id, 0, 1000))
                .await
                .unwrap();
            let notices = events
                .iter()
                .filter(|event| text(event, "text").contains("Execution output was interrupted"))
                .count();
            assert_eq!(notices, 1, "{events:?}");
            assert!(
                text(&result, "summary").contains("committed answer"),
                "{result}"
            );
        }
        if !completed {
            let expected_error = if clean_eof {
                "completion was recorded"
            } else {
                "output was interrupted"
            };
            assert!(text(&result, "error").contains(expected_error), "{result}");
        }
        assert_eq!(
            controller.plans.lock().await.len(),
            1,
            "must not replay the turn"
        );
        fixture.stop(false).await;
        server.abort();
    }
}

/// Hold the SQLite writer after the manager reserves an observation and before
/// its healthy response can be committed. The live deadline must still win.
#[tokio::test]
async fn active_observation_expiring_during_result_persistence_fails_without_replay_reconciliation()
{
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    let mut fixture = Fixture::new().await;
    fixture.stop(false).await;
    let controller = Arc::new(Controller {
        data: fixture.service.config.data_dir.clone(),
        plans: tokio::sync::Mutex::new(Vec::new()),
        failures: 0,
        slots: 4,
        hold: AtomicBool::new(false),
    });
    let waits = Arc::new(AtomicUsize::new(0));
    let response_gate = Arc::new(tokio::sync::Semaphore::new(0));
    let app = Router::new()
        .fallback(any({
            let waits = waits.clone();
            let gate = response_gate.clone();
            move |state: State<Arc<Controller>>, request: Request| {
                let waits = waits.clone();
                let gate = gate.clone();
                async move {
                    if request.uri().path().ends_with("/wait") {
                        let index = waits.fetch_add(1, Ordering::SeqCst);
                        if index == 0 {
                            return Json(json!({ "StatusCode": 75 })).into_response();
                        }
                        gate.acquire().await.unwrap().forget();
                        return Json(json!({ "StatusCode": 0 })).into_response();
                    }
                    serve_controller(state, request).await
                }
            }
        }))
        .with_state(controller.clone());
    let (listener, address) = common::bind().await;
    common::reconfigure(&mut fixture.service, |config| {
        config.runner_url = format!("http://{address}");
    })
    .await;
    let server = common::serve(listener, app);
    let home = fixture.service.config.home.join(".codex");
    tokio::fs::create_dir_all(&home).await.unwrap();
    tokio::fs::write(home.join("auth.json"), "{}")
        .await
        .unwrap();
    let run = fixture
        .enqueue("Test expiry during controller result persistence")
        .await;
    let run_id = text(&run, "id").to_owned();
    fixture
        .service
        .store
        .save_checkpoint_boundary(&run_id, json!({ "remainingMs": 15000 }))
        .await
        .unwrap();
    fixture.start().await;
    let saved = eventually(
        Duration::from_secs(12),
        Duration::from_millis(10),
        async || {
            let saved = fixture.checkpoint(&run_id).await;
            (waits.load(Ordering::SeqCst) >= 2
                && saved["completed"] == true
                && saved["controllerObservation"]["inFlight"] == true)
                .then_some(saved)
        },
    )
    .await;
    let remaining = saved["remainingMs"].as_u64().unwrap();
    assert!((300..=15000).contains(&remaining), "{saved}");
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let store = fixture.service.store.clone();
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
    response_gate.add_permits(1);
    tokio::time::sleep(Duration::from_millis(remaining + 250)).await;
    release_tx.send(()).unwrap();
    blocker.await.unwrap();
    let result = fixture.until_finished(&run_id).await;
    assert_eq!(result["status"], RunStatus::Failed, "{result}");
    assert!(text(&result, "error").contains("time limit"), "{result}");
    let saved = fixture.checkpoint(&run_id).await;
    assert_eq!(saved["terminalCode"], 0, "{saved}");
    assert_eq!(saved["remainingMs"], 0, "{saved}");
    assert_eq!(
        controller.plans.lock().await.len(),
        1,
        "must not replay the turn"
    );
    fixture.stop(false).await;
    server.abort();
}

/// Manager SIGTERM is a local stop, not a controller completion observation.
/// A recorded turn must retain the original attempt for reconciliation on restart.
#[tokio::test]
async fn manager_shutdown_between_turn_completion_and_wait_preserves_same_attempt_evidence_reconciliation()
 {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    for interruption in ["shutdown", "cancel", "signal", "client-error", "timeout"] {
        let cancel = interruption == "cancel";
        let stops_manager = matches!(interruption, "shutdown" | "cancel");
        let mut fixture = Fixture::new().await;
        fixture.stop(false).await;
        let controller = Arc::new(Controller {
            data: fixture.service.config.data_dir.clone(),
            plans: tokio::sync::Mutex::new(Vec::new()),
            failures: 0,
            slots: 4,
            hold: std::sync::atomic::AtomicBool::new(false),
        });
        let waits = Arc::new(AtomicUsize::new(0));
        let count = waits.clone();
        // For the manager stop cases, expose terminal evidence only to the
        // restarted process, even if the first process gets time to probe again.
        let evidence_available = Arc::new(AtomicBool::new(!stops_manager));
        let evidence = evidence_available.clone();
        let app = Router::new()
            .fallback(any(
                move |state: State<Arc<Controller>>, request: Request| {
                    let count = count.clone();
                    let evidence = evidence.clone();
                    async move {
                        if request.uri().path().ends_with("/wait") {
                            let observation = count.fetch_add(1, Ordering::SeqCst);
                            if interruption == "client-error" && observation == 0 {
                                // The initial wait fails decoding. Only the manager's
                                // durably reserved observation may read again.
                                return Json(json!({})).into_response();
                            }
                            if observation == 0 || !evidence.load(Ordering::SeqCst) {
                                // Keep the initial read open. A stopped manager
                                // cannot see evidence reserved for its replacement.
                                return std::future::pending::<Response>().await;
                            }
                            return Json(json!({ "StatusCode": 0 })).into_response();
                        }
                        serve_controller(state, request).await
                    }
                },
            ))
            .with_state(controller.clone());
        let (listener, address) = common::bind().await;
        common::reconfigure(&mut fixture.service, |config| {
            config.runner_url = format!("http://{address}");
        })
        .await;
        let server = common::serve(listener, app);
        let home = fixture.service.config.home.join(".codex");
        tokio::fs::create_dir_all(&home).await.unwrap();
        tokio::fs::write(home.join("auth.json"), "{}")
            .await
            .unwrap();
        let run = fixture
            .enqueue("Stop only this disposable manager after turn.completed")
            .await;
        let run_id = text(&run, "id");
        fixture
            .service
            .store
            .transaction({
                let run_id = run_id.to_owned();
                move |db| {
                    db.save_checkpoint_boundary(
                        &run_id,
                        &json!({
                            "controllerRecoveries": 3,
                            "remainingMs": if interruption == "timeout" { json!(4000) } else { Value::Null },
                        }),
                    )
                }
            })
            .await
            .unwrap();
        fixture.start().await;
        eventually(
            Duration::from_secs(10),
            Duration::from_millis(10),
            async || {
                let saved = fixture
                    .service
                    .store
                    .checkpoint_boundary(run_id)
                    .await
                    .unwrap()?;
                (saved["completed"] == true && waits.load(Ordering::SeqCst) >= 1).then_some(())
            },
        )
        .await;
        if cancel {
            // Persisted user cancellation takes precedence even when shutdown
            // interrupts the client before the controller's healthy exit arrives.
            fixture
                .service
                .store
                .change_run(
                    run_id,
                    cairn_installation::runs::RunChanges {
                        cancel_requested_at: Some(Some(now())),
                        ..Default::default()
                    },
                )
                .await
                .unwrap();
        }
        if interruption == "signal" {
            let saved = fixture.checkpoint(run_id).await;
            kill_fixture_runner_child(&saved).await;
        }
        let mut waits_before_restart = 1;
        if stops_manager {
            fixture.stop(false).await;
            waits_before_restart = waits.load(Ordering::SeqCst);
            let stopped = fixture.service.store.run(run_id).await.unwrap();
            let checkpoint = fixture.checkpoint(run_id).await;
            assert_eq!(checkpoint["completed"], true);
            assert!(
                checkpoint["terminalCode"].is_null(),
                "Local stop must not fabricate terminal proof: {checkpoint}"
            );
            if !cancel {
                assert_eq!(stopped["status"], RunStatus::Queued, "{stopped}");
                assert!(checkpoint["settled"].is_null(), "{checkpoint}");
            }
            evidence_available.store(true, Ordering::SeqCst);
            fixture.start().await;
        }
        if cancel {
            let result = fixture
                .until(run_id, |run| run["status"] == "cancelled")
                .await;
            assert_eq!(result["status"], RunStatus::Cancelled);
            assert_eq!(waits.load(Ordering::SeqCst), waits_before_restart);
        } else if interruption == "timeout" {
            let result = fixture.until_finished(run_id).await;
            assert_eq!(result["status"], RunStatus::Failed, "{result}");
            assert!(text(&result, "error").contains("time limit"), "{result}");
            assert!(fixture.checkpoint(run_id).await["terminalCode"].is_null());
            assert_eq!(waits.load(Ordering::SeqCst), 1);
        } else {
            let result = fixture.until_finished(run_id).await;
            assert_eq!(
                result["status"],
                RunStatus::Succeeded,
                "{interruption}: {result}"
            );
            assert_eq!(fixture.checkpoint(run_id).await["terminalCode"], 0);
            assert_eq!(waits.load(Ordering::SeqCst), waits_before_restart + 1);
        }
        assert_eq!(
            controller.plans.lock().await.len(),
            1,
            "Reconciliation must never launch another agent"
        );
        assert_eq!(fixture.checkpoint(run_id).await["controllerRecoveries"], 3);
        fixture.stop(false).await;
        server.abort();
    }
}

#[tokio::test]
async fn vm_chat_protocol_runs_receive_one_frame_with_plugin_instructions() {
    let mut fixture = Fixture::new().await;
    fixture.stop(false).await;
    let controller = Arc::new(Controller {
        data: fixture.service.config.data_dir.clone(),
        plans: tokio::sync::Mutex::new(Vec::new()),
        failures: 0,
        slots: 4,
        hold: std::sync::atomic::AtomicBool::new(false),
    });
    let app = Router::new()
        .fallback(any(serve_controller))
        .with_state(controller.clone());
    let (listener, address) = common::bind().await;
    common::reconfigure(&mut fixture.service, |config| {
        config.runner_url = format!("http://{address}");
    })
    .await;
    let server = common::serve(listener, app);
    let s = fixture.service.clone();
    claude_account(&s, 4).await;
    let agent = s
        .agent_fixture(json!({ "name": "Claude", "provider": "claude" }), None)
        .await
        .unwrap();
    common::save_plugin(
        &s,
        "onepassword",
        &json!({
            "name": "Production", "enabled": true,
            "secrets": { "token": "ops_fixture" }, "agentIds": [agent["id"]],
        }),
        None,
    )
    .await
    .unwrap();
    let plugin = "cairn plugin onepassword read --account ACCOUNT_ID";
    fixture.start().await;

    // Chat turns stay interactive and receive plugin instructions in one frame.
    let chat = s
        .chat_create(json!({ "agentId": agent["id"] }))
        .await
        .unwrap();
    let chat_id = text(&chat, "id").to_owned();
    s.chat_send(&chat_id, message("Hello")).await.unwrap();
    let chat_run = fixture.chat_run(&chat_id).await;
    fixture
        .until(&chat_run, |run| run["status"] == RunStatus::Succeeded)
        .await;
    let plan = controller.plans.lock().await.last().unwrap()["chat"].clone();
    let instructions = text(&plan, "instructions");
    assert!(instructions.contains(plugin), "{instructions}");
    assert!(instructions.contains("This is an interactive chat"));
    assert!(!instructions.contains("This unattended task"));
    assert_eq!(plan["execution"]["text"], "Hello");
    assert_eq!(plan["execution"]["recovery"], false);
    fixture.stop(false).await;
    server.abort();
}

#[tokio::test]
async fn claude_conversations_run_together_and_lowering_limit_does_not_cancel_them() {
    let mut fixture = Fixture::new().await;
    let s = fixture.service.clone();
    let account = claude_account(&s, 2).await;
    let mut chats = Vec::new();
    let mut runs = Vec::new();
    for prompt in [
        "fixture:batch",
        "fixture:batch",
        "Complete third conversation",
    ] {
        let mut sent = message(prompt);
        sent["provider"] = "claude".into();
        sent["model"] = "sonnet".into();
        let chat_id = fixture.start_chat(sent).await;
        runs.push(fixture.chat_run(&chat_id).await);
        chats.push(chat_id);
    }
    for run in &runs[..2] {
        fixture
            .until(run, |r| {
                r["status"] == RunStatus::Running && r["sessionId"].is_string()
            })
            .await;
    }
    // Both real fixture subprocesses hold independent turns.
    for run in &runs[..2] {
        let credentials = std::fs::read_to_string(
            fixture
                .run_directory(run)
                .join("home/.claude/.credentials.json"),
        )
        .unwrap();
        assert!(!credentials.contains("refresh"));
        assert!(credentials.contains("fixture-access"));
    }
    common::account_fixture::update_account_fixture(
        &s,
        &account,
        &json!({ "maxConcurrentRuns": 1 }),
    )
    .await
    .unwrap();
    for run in &runs[..2] {
        assert_eq!(
            s.store.run(run).await.unwrap()["status"],
            RunStatus::Running
        );
    }
    let answer = async |chat: &str| {
        let mut sent = message("fixture:batch Finish the independent work");
        sent["mode"] = "steer".into();
        s.chat_send(chat, sent).await.unwrap();
    };
    answer(&chats[0]).await;
    fixture
        .until(&runs[0], |r| r["status"] == RunStatus::Succeeded)
        .await;
    fixture
        .until(&runs[2], |r| {
            text(r, "accountWaitReason").contains("free Claude Code account slot")
        })
        .await;
    assert_eq!(
        s.store.run(&runs[1]).await.unwrap()["status"],
        RunStatus::Running
    );
    answer(&chats[1]).await;
    for run in &runs[1..] {
        fixture
            .until(run, |r| r["status"] == RunStatus::Succeeded)
            .await;
    }
    fixture.stop(false).await;
}

#[tokio::test]
async fn manual_resume_cancels_a_staged_switch_during_worker_fencing() {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    let mut fixture = Fixture::new().await;
    fixture.stop(false).await;
    let controller = Arc::new(Controller {
        data: fixture.service.config.data_dir.clone(),
        plans: tokio::sync::Mutex::new(Vec::new()),
        failures: 0,
        slots: 4,
        hold: AtomicBool::new(true),
    });
    let block_fence = Arc::new(AtomicBool::new(false));
    let fenced_attempt = Arc::new(std::sync::Mutex::new(None::<String>));
    let blocked_delete_count = Arc::new(AtomicUsize::new(0));
    let fence_entered = Arc::new(tokio::sync::Notify::new());
    let fence_release = Arc::new(tokio::sync::Notify::new());
    let app =
        Router::new()
            .fallback(any({
                let (
                    block_fence,
                    fenced_attempt,
                    blocked_delete_count,
                    fence_entered,
                    fence_release,
                ) = (
                    block_fence.clone(),
                    fenced_attempt.clone(),
                    blocked_delete_count.clone(),
                    fence_entered.clone(),
                    fence_release.clone(),
                );
                move |state: State<Arc<Controller>>, request: Request| {
                    let (
                        block_fence,
                        fenced_attempt,
                        blocked_delete_count,
                        fence_entered,
                        fence_release,
                    ) = (
                        block_fence.clone(),
                        fenced_attempt.clone(),
                        blocked_delete_count.clone(),
                        fence_entered.clone(),
                        fence_release.clone(),
                    );
                    async move {
                        let attempt_to_fence = fenced_attempt.lock().unwrap().clone();
                        let is_target_attempt =
                            request.uri().path().strip_prefix("/runs/").is_some_and(
                                |request_attempt| {
                                    attempt_to_fence.as_deref() == Some(request_attempt)
                                },
                            );
                        if request.method() == axum::http::Method::DELETE
                            && is_target_attempt
                            && block_fence.swap(false, Ordering::SeqCst)
                        {
                            blocked_delete_count.fetch_add(1, Ordering::SeqCst);
                            fence_entered.notify_one();
                            fence_release.notified().await;
                        }
                        serve_controller(state, request).await
                    }
                }
            }))
            .with_state(controller.clone());
    let (listener, address) = common::bind().await;
    common::reconfigure(&mut fixture.service, |config| {
        config.runner_url = format!("http://{address}");
    })
    .await;
    let s = fixture.service.clone();
    let server = common::serve(listener, app);
    let phase = |name: &'static str| {
        eprintln!(
            "CAIRN_RESUME_TEST_PHASE test=manual_resume_cancels_a_staged_switch_during_worker_fencing phase={name} blockedDeleteCount={}",
            blocked_delete_count.load(Ordering::SeqCst).min(16)
        );
    };
    let home = s.config.home.join(".codex");
    tokio::fs::create_dir_all(&home).await.unwrap();
    tokio::fs::write(home.join("auth.json"), "{}")
        .await
        .unwrap();
    cairn_installation::nodes::refresh_local(&s).await.unwrap();
    s.store
        .set(
            "codex-models:",
            json!({
                "checkedAt": now(),
                "models": [
                    {
                        "model": "gpt-6-sol",
                        "isDefault": true,
                        "supportedReasoningEfforts": [{ "reasoningEffort": "high" }],
                        "serviceTiers": [{ "id": "priority" }],
                    },
                    {
                        "model": "gpt-6-luna",
                        "isDefault": false,
                        "supportedReasoningEfforts": [{ "reasoningEffort": "xhigh" }],
                        "serviceTiers": [{ "id": "priority" }],
                    },
                ],
            }),
            None,
        )
        .await
        .unwrap();

    let chat = s.chat_create(json!({})).await.unwrap();
    let chat_id = text(&chat, "id").to_owned();
    let mut sent = message("fixture:chat-hang");
    sent["provider"] = "codex".into();
    sent["model"] = "gpt-6-sol".into();
    sent["reasoning"] = "high".into();
    sent["speed"] = "priority".into();
    let turn_id = text(&sent, "id").to_owned();
    s.chat_send(&chat_id, sent).await.unwrap();
    fixture.start().await;
    phase("chat_run_lookup_before");
    let run_id = fixture.chat_run(&chat_id).await;
    phase("chat_run_lookup_after");
    phase("initial_run_wait_before");
    let active = fixture
        .until(&run_id, |run| {
            run["status"] == RunStatus::Running && run["sessionId"].is_string()
        })
        .await;
    phase("initial_run_wait_after");
    phase("initial_controller_plan_wait_before");
    eventually(
        Duration::from_secs(20),
        Duration::from_millis(30),
        async || {
            controller
                .plans
                .lock()
                .await
                .iter()
                .any(|plan| plan["runId"] == run_id)
                .then_some(())
        },
    )
    .await;
    phase("initial_controller_plan_wait_after");
    let initial_attempt = controller
        .plans
        .lock()
        .await
        .iter()
        .find(|plan| plan["runId"] == run_id)
        .and_then(|plan| plan["id"].as_str())
        .expect("the initial run must have a controller attempt")
        .to_owned();
    *fenced_attempt.lock().unwrap() = Some(initial_attempt);
    let relay = common::relay_fixture::context(&s).await;
    let relay_token = text(&relay, "value");
    let client = reqwest::Client::new();
    let stopped = client
        .post(format!("{}/api/chats/{chat_id}/stop", fixture.url))
        .header("x-test-relay-token", relay_token)
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    stopped.error_for_status().unwrap();
    phase("stopped_run_wait_before");
    let terminal = fixture
        .until(&run_id, |run| {
            run["status"] == RunStatus::Cancelled && run["resumeAvailable"] == true
        })
        .await;
    phase("stopped_run_wait_after");
    assert_eq!(terminal["sessionId"], active["sessionId"]);
    fixture.stop(false).await;
    s.store
        .change_run(
            &run_id,
            cairn_installation::runs::RunChanges {
                status: Some(RunStatus::Failed),
                finished_at: Some(Some(now())),
                error: Some(Some("Usage limit has been reached".into())),
                cancel_requested_at: Some(None),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let failed_quota_run = s.store.run(&run_id).await.unwrap();
    assert_eq!(failed_quota_run["status"], RunStatus::Failed);
    assert_eq!(failed_quota_run["cancelRequestedAt"], Value::Null);
    assert!(
        failed_quota_run["error"]
            .as_str()
            .unwrap()
            .contains("Usage limit")
    );

    let mut alternative = serde_json::to_value(
        s.accounts
            .create(&s, Provider::Claude, "Switch target")
            .await
            .unwrap(),
    )
    .unwrap();
    let alternative_id = text(&alternative, "id").to_owned();
    alternative["state"] = "ready".into();
    s.store.save_account_fixture(alternative).await.unwrap();
    let claude_home = accounts::claude::account_home(&s.config, &alternative_id);
    std::fs::create_dir_all(&claude_home).unwrap();
    std::fs::write(
        claude_home.join(".credentials.json"),
        json!({
            "claudeAiOauth": {
                "accessToken": "fixture-access",
                "expiresAt": now() + 3_600_000,
                "scopes": ["user:inference"],
            },
        })
        .to_string(),
    )
    .unwrap();
    let api = common::relay_fixture::router(s.clone()).await.unwrap();
    let switch_id = id();
    let switch = json!({
        "runId": run_id,
        "turnId": turn_id,
        "requestId": switch_id,
        "provider": "claude",
    });
    let (stage_status, stage_response) = api_post(
        &api,
        relay_token,
        &format!("/api/chats/{chat_id}/coding-agent"),
        switch,
    )
    .await;
    assert_eq!(stage_status, StatusCode::OK, "{stage_response}");
    assert!(
        s.store
            .read({
                let chat_id = chat_id.clone();
                move |db| Ok(db.engine_switch(&chat_id)?.unwrap().pending)
            })
            .await
            .unwrap()
    );

    block_fence.store(true, Ordering::SeqCst);
    controller
        .hold
        .store(false, std::sync::atomic::Ordering::Relaxed);
    fixture.start().await;
    phase("first_fence_wait_before");
    tokio::time::timeout(Duration::from_secs(10), fence_entered.notified())
        .await
        .expect("the production switch worker should reach runner fencing");
    phase("first_fence_wait_after");

    let cancelled = client
        .post(format!("{}/api/chats/{chat_id}/stop", fixture.url))
        .header("x-test-relay-token", relay_token)
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    cancelled.error_for_status().unwrap();
    assert!(
        !s.store
            .read({
                let chat_id = chat_id.clone();
                move |db| Ok(db.engine_switch(&chat_id)?.unwrap().pending)
            })
            .await
            .unwrap()
    );

    fence_release.notify_one();
    let resumed = client
        .post(format!("{}/api/chats/{chat_id}/resume", fixture.url))
        .header("x-test-relay-token", relay_token)
        .json(&json!({
            "runId": run_id,
            "turnId": turn_id,
            "selection": {
                "model": "gpt-6-luna",
                "reasoning": "xhigh",
                "speed": "",
            },
        }))
        .send()
        .await
        .unwrap();
    assert!(resumed.status().is_success());
    phase("resumed_execution_wait_before");
    let completed = fixture.until_finished(&run_id).await;
    phase("resumed_execution_wait_after");
    assert_eq!(blocked_delete_count.load(Ordering::SeqCst), 1);
    assert_eq!(completed["status"], RunStatus::Succeeded, "{completed}");
    assert_eq!(completed["snapshot"]["agent"]["provider"], "codex");
    assert_eq!(completed["snapshot"]["agent"]["speed"], "priority");
    assert_eq!(completed["chatExecution"]["messageId"], turn_id);
    assert_eq!(completed["sessionId"], terminal["sessionId"]);
    let resumed_plan = controller
        .plans
        .lock()
        .await
        .iter()
        .find(|plan| {
            plan["runId"] == run_id
                && plan["chat"]["execution"]["messageId"] == turn_id
                && plan["chat"]["model"] == "gpt-6-luna"
        })
        .cloned()
        .expect("the selected resume must launch after the stale switch is fenced");
    assert_eq!(resumed_plan["chat"]["provider"], "codex");
    assert_eq!(resumed_plan["chat"]["serviceTier"], "priority");
    assert!(controller.plans.lock().await.iter().all(|plan| {
        plan["runId"] != run_id
            || plan["chat"]["execution"]["messageId"] != turn_id
            || plan["chat"]["provider"] != "claude"
    }));
    let receipt = s
        .store
        .read({
            let chat_id = chat_id.clone();
            move |db| db.engine_switch(&chat_id)
        })
        .await
        .unwrap()
        .unwrap();
    assert_eq!(receipt.request_id, switch_id);
    assert!(!receipt.pending);

    fixture.stop(false).await;
    server.abort();
}
