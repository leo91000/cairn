use super::*;

#[test]
fn worker_fixture_uses_the_matching_compilation_target() {
    let root = TempDir::new().unwrap();
    let first = root.path().join("first/debug");
    let second = root.path().join("second/debug");
    let fixtures = std::ffi::OsString::from(
        json!([
            { "target": first, "binary": root.path().join("first-fixture") },
            { "target": second, "binary": root.path().join("second-fixture") },
        ])
        .to_string(),
    );
    assert_eq!(
        worker_fixture_binary(&first, Some(&fixtures), false),
        root.path().join("first-fixture")
    );
    assert_eq!(
        worker_fixture_binary(&second, Some(&fixtures), false),
        root.path().join("second-fixture")
    );
    assert_eq!(
        worker_fixture_binary(
            &root.path().join("remapped/first/debug"),
            Some(&fixtures),
            true
        ),
        root.path().join("first-fixture")
    );
    let single = std::ffi::OsString::from(
        json!([{ "target": first, "binary": root.path().join("first-fixture") }]).to_string(),
    );
    assert_eq!(
        worker_fixture_binary(&root.path().join("remapped"), Some(&single), true),
        root.path().join("first-fixture")
    );
}

#[test]
#[should_panic(expected = "launcher supplied no worker fixture for this target")]
fn worker_fixture_rejects_a_missing_compilation_target() {
    worker_fixture_binary(
        Path::new("missing"),
        Some(std::ffi::OsStr::new("[]")),
        false,
    );
}

#[test]
#[should_panic(expected = "launcher supplied no worker fixture for this target")]
fn worker_fixture_does_not_remap_an_ordinary_build() {
    let fixtures = std::ffi::OsStr::new(r#"[{"target":"first","binary":"first-fixture"}]"#);
    worker_fixture_binary(Path::new("second"), Some(fixtures), false);
}

#[test]
#[should_panic(expected = "launcher supplied no worker fixture for this target")]
fn worker_fixture_rejects_an_ambiguous_remap() {
    let fixtures = std::ffi::OsStr::new(
        r#"[{"target":"one/triple/debug","binary":"one"},{"target":"two/triple/debug","binary":"two"}]"#,
    );
    worker_fixture_binary(Path::new("other/triple/debug"), Some(fixtures), true);
}

#[test]
#[should_panic(
    expected = "fixture clock requires worker hooks; run this test with pnpm test:backend"
)]
fn fixture_clock_requires_the_backend_launcher() {
    require_fixture_hooks(None);
}

#[tokio::test]
async fn worker_readiness_waits_for_output_and_reports_blocked_or_exited_children() {
    let mut delayed = Command::new("sh")
        .args(["-c", "sleep 0.1; echo 'Listening on http://127.0.0.1:1234'"])
        .stdout(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    assert_eq!(
        wait_for_worker_ready(&mut delayed, Duration::from_secs(2))
            .await
            .unwrap(),
        "http://127.0.0.1:1234"
    );

    let mut blocked = Command::new("sh")
        .args(["-c", "exec sleep 5"])
        .stdout(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    assert!(
        wait_for_worker_ready(&mut blocked, Duration::from_millis(100))
            .await
            .unwrap_err()
            .contains("did not become ready")
    );

    let mut exited = Command::new("sh")
        .args(["-c", "exit 1"])
        .stdout(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    assert!(
        wait_for_worker_ready(&mut exited, Duration::from_secs(2))
            .await
            .unwrap_err()
            .contains("closed stdout before readiness")
    );
}

#[tokio::test]
async fn transient_agent_failure_resumes_same_session_and_workspace_after_restart() {
    let mut fixture = Fixture::new().await;
    let prompt = "fixture:agent-failures:[{\"codexErrorInfo\":\"other\",\"message\":\"workspace routing discovery timed out\"}]";
    let chat_id = fixture.start_chat(message(prompt)).await;
    let run_id = fixture.chat_run(&chat_id).await;
    let mut waiting = fixture
        .until(&run_id, |run| run["retry"].is_object() || finished(run))
        .await;
    assert_eq!(waiting["status"], RunStatus::Queued, "{waiting}");
    assert_eq!(waiting["retry"]["attempt"], 1);
    assert_eq!(waiting["retry"]["limit"], 5);
    assert_eq!(waiting["retry"]["cause"]["kind"], "timeout");
    assert!(waiting["retry"]["nextAttemptAt"].as_i64().unwrap() > now());
    assert_eq!(waiting["sessionId"], "fixture-chat");
    waiting["retry"]["nextAttemptAt"] = (now() + 60_000).into();
    fixture
        .service
        .store
        .change_run(
            &run_id,
            cairn_installation::runs::RunChanges {
                retry: Some(Some(
                    serde_json::from_value(waiting["retry"].clone()).unwrap(),
                )),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    fixture.stop(false).await;
    fixture.start().await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    let retained = fixture.service.store.run(&run_id).await.unwrap();
    assert_eq!(retained["status"], RunStatus::Queued);
    assert_eq!(retained["retry"], waiting["retry"]);
    let mut retry = retained["retry"].clone();
    retry["nextAttemptAt"] = now().into();
    fixture
        .service
        .store
        .change_run(
            &run_id,
            cairn_installation::runs::RunChanges {
                retry: Some(Some(serde_json::from_value(retry.clone()).unwrap())),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let completed = fixture.until_finished(&run_id).await;
    assert_eq!(completed["status"], RunStatus::Succeeded, "{completed}");
    assert_eq!(completed["sessionId"], waiting["sessionId"]);
    assert_eq!(completed["workspace"], waiting["workspace"]);
    let bytes =
        tokio::fs::read(Path::new(text(&completed, "workspace")).join("fixture-retry-work.json"))
            .await
            .unwrap();
    let marker: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(marker["attempts"], 2);
    assert_eq!(marker["cwd"], completed["workspace"]);
    let chat = fixture.service.chat_detail(&chat_id).await.unwrap();
    assert_eq!(chat["paused"], false);
    fixture.stop(false).await;
}

#[tokio::test]
async fn startup_disconnect_before_first_turn_retries_without_losing_the_pending_message() {
    let mut fixture = Fixture::new().await;
    fixture.stop(false).await;
    let chat = fixture
        .start_chat(message("Complete the pending request once."))
        .await;
    fixture
        .service
        .chat_tick(&std::collections::HashSet::default())
        .await
        .unwrap();
    let run_id = fixture.chat_run(&chat).await;
    let home = fixture.run_directory(&run_id).join("codex");
    tokio::fs::create_dir_all(&home).await.unwrap();
    let marker = home.join("fixture-startup-failures.json");
    tokio::fs::write(&marker, br#"{"method":"initialize","remaining":1}"#)
        .await
        .unwrap();
    fixture.start().await;
    let waiting = fixture
        .until(&run_id, |run| run["retry"].is_object() || finished(run))
        .await;
    assert_eq!(waiting["status"], RunStatus::Queued, "{waiting}");
    assert_eq!(waiting["retry"]["cause"]["startup"], true);
    assert!(!waiting["sessionId"].is_string());
    let mut retry = waiting["retry"].clone();
    retry["nextAttemptAt"] = now().into();
    fixture
        .service
        .store
        .change_run(
            &run_id,
            cairn_installation::runs::RunChanges {
                retry: Some(Some(serde_json::from_value(retry.clone()).unwrap())),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let completed = fixture.until_finished(&run_id).await;
    assert_eq!(completed["status"], RunStatus::Succeeded, "{completed}");
    assert_eq!(completed["sessionId"], "fixture-chat");
    assert!(completed["retry"].is_null());
    let attempts: Value = serde_json::from_slice(&tokio::fs::read(marker).await.unwrap()).unwrap();
    assert_eq!(attempts["remaining"], 0);
    assert_eq!(attempts["pids"].as_array().unwrap().len(), 1);
    let chat = fixture.service.chat_detail(&chat).await.unwrap();
    assert_eq!(chat["messages"].as_array().unwrap().len(), 1);
    fixture.stop(false).await;
}

#[tokio::test]
async fn first_start_recovery_remains_manually_resumable_after_stop_or_exhaustion() {
    for ending in ["cancel", "deadline", "exhaustion"] {
        let mut fixture = Fixture::new().await;
        fixture.stop(false).await;
        let chat = fixture
            .start_chat(message("Keep the original pending request."))
            .await;
        fixture
            .service
            .chat_tick(&std::collections::HashSet::default())
            .await
            .unwrap();
        let run_id = fixture.chat_run(&chat).await;
        let home = fixture.run_directory(&run_id).join("codex");
        tokio::fs::create_dir_all(&home).await.unwrap();
        let marker = home.join("fixture-startup-failures.json");
        let failures = if ending == "exhaustion" { 6 } else { 1 };
        tokio::fs::write(
            &marker,
            serde_json::to_vec(&json!({ "method": "initialize", "remaining": failures })).unwrap(),
        )
        .await
        .unwrap();
        // Terminal state is visible before the old worker releases its cached checkpoint.
        let retirement_socket = fixture
            .run_directory(&run_id)
            .join("fixture-retirement.sock");
        let retirement_listener = (ending == "exhaustion")
            .then(|| tokio::net::UnixListener::bind(&retirement_socket).unwrap());
        fixture.start().await;
        let mut waiting = fixture
            .until(&run_id, |run| run["retry"].is_object() || finished(run))
            .await;
        assert_eq!(waiting["status"], RunStatus::Queued, "{waiting}");
        let workspace = waiting["workspace"].clone();
        if ending == "cancel" {
            fixture
                .service
                .worker
                .cancel(&fixture.service, &run_id)
                .await
                .unwrap();
            fixture
                .until(&run_id, |run| run["status"] == RunStatus::Cancelled)
                .await;
        } else {
            for attempt in 1..=5 {
                assert_eq!(waiting["retry"]["attempt"], attempt);
                let mut retry = waiting["retry"].clone();
                retry["nextAttemptAt"] = now().into();
                if ending == "deadline" {
                    retry["deadlineAt"] = (now() - 1).into();
                }
                fixture
                    .service
                    .store
                    .change_run(
                        &run_id,
                        cairn_installation::runs::RunChanges {
                            retry: Some(Some(serde_json::from_value(retry.clone()).unwrap())),
                            ..Default::default()
                        },
                    )
                    .await
                    .unwrap();
                waiting = fixture
                    .until(&run_id, |run| {
                        finished(run)
                            || (run["status"] == RunStatus::Queued
                                && run["retry"]["attempt"]
                                    .as_u64()
                                    .is_some_and(|n| n > attempt))
                    })
                    .await;
                if finished(&waiting) {
                    break;
                }
            }
            assert_eq!(waiting["status"], RunStatus::Failed, "{waiting}");
        }
        let retirement = if let Some(listener) = retirement_listener {
            let (gate, _) = tokio::time::timeout(Duration::from_secs(60), listener.accept())
                .await
                .expect("fixture worker did not reach its retirement barrier")
                .unwrap();
            tokio::fs::remove_file(&retirement_socket).await.unwrap();
            assert!(fixture.worker_active_runs().await.unwrap() > 0);
            let startup: Value =
                serde_json::from_slice(&tokio::fs::read(&marker).await.unwrap()).unwrap();
            assert_eq!(startup["remaining"], 0);
            assert_eq!(startup["pids"].as_array().unwrap().len(), 6);
            let context = common::relay_fixture::context(&fixture.service).await;
            let response = fixture
                .service
                .http
                .post(format!("{}/api/runs/{run_id}/resume", fixture.url))
                .header("x-test-relay-token", text(&context, "value"))
                .json(&json!({}))
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::CONFLICT);
            assert_eq!(
                response.json::<Value>().await.unwrap()["error"],
                "Wait for this run to finish stopping before resuming."
            );
            Some(gate)
        } else {
            None
        };
        let quiescence = Arc::new(tokio::sync::Notify::new());
        fixture.quiescence_observed = Some(quiescence.clone());
        let resumed = {
            let resume = async {
                fixture.until_worker_quiescent().await;
                fixture
                    .service
                    .worker
                    .resume(&fixture.service, &run_id)
                    .await
            };
            tokio::pin!(resume);
            if let Some(mut gate) = retirement {
                use tokio::io::AsyncWriteExt;
                tokio::select! {
                    // Without the retirement wait, resume completes first and the
                    // released old checkpoint deterministically restores the failure.
                    result = &mut resume => {
                        gate.write_all(&[1]).await.unwrap();
                        result
                    },
                    () = quiescence.notified() => {
                        gate.write_all(&[1]).await.unwrap();
                        resume.await
                    },
                }
            } else {
                resume.await
            }
        }
        .unwrap();
        assert!(
            resumed["retry"].is_null(),
            "Manual resume starts a new budget"
        );
        let completed = fixture.until_finished(&run_id).await;
        assert_eq!(
            completed["status"],
            RunStatus::Succeeded,
            "{ending}: {completed}"
        );
        assert_eq!(completed["workspace"], workspace);
        let detail = fixture.service.chat_detail(&chat).await.unwrap();
        assert_eq!(detail["messages"].as_array().unwrap().len(), 1);
        assert_eq!(detail["messages"][0]["status"], "delivered");
        fixture.stop(false).await;
    }
}

#[tokio::test]
async fn native_recovery_preflight_disconnect_consumes_the_same_retry_budget() {
    let mut fixture = Fixture::new().await;
    let chat = fixture
        .start_chat(message(
            "fixture:agent-failures:[{\"code\":\"server_overloaded\"}]",
        ))
        .await;
    let run_id = fixture.chat_run(&chat).await;
    let waiting = fixture
        .until(&run_id, |run| run["retry"].is_object() || finished(run))
        .await;
    assert_eq!(waiting["status"], RunStatus::Queued, "{waiting}");
    fixture.stop(false).await;
    let marker = fixture
        .run_directory(&run_id)
        .join("codex/fixture-startup-failures.json");
    tokio::fs::write(&marker, br#"{"method":"thread/read","remaining":1}"#)
        .await
        .unwrap();
    let mut retry = waiting["retry"].clone();
    retry["nextAttemptAt"] = now().into();
    fixture
        .service
        .store
        .change_run(
            &run_id,
            cairn_installation::runs::RunChanges {
                retry: Some(Some(serde_json::from_value(retry.clone()).unwrap())),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    fixture.start().await;
    let recovering = fixture
        .until(&run_id, |run| {
            (run["status"] == RunStatus::Queued && run["retry"]["attempt"] == 2) || finished(run)
        })
        .await;
    assert_eq!(recovering["status"], RunStatus::Queued, "{recovering}");
    assert_eq!(recovering["retry"]["cause"]["startup"], true);
    assert_eq!(
        recovering["retry"]["startedAt"],
        waiting["retry"]["startedAt"]
    );
    let mut retry = recovering["retry"].clone();
    retry["nextAttemptAt"] = now().into();
    fixture
        .service
        .store
        .change_run(
            &run_id,
            cairn_installation::runs::RunChanges {
                retry: Some(Some(serde_json::from_value(retry.clone()).unwrap())),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let completed = fixture.until_finished(&run_id).await;
    assert_eq!(completed["status"], RunStatus::Succeeded, "{completed}");
    assert_eq!(completed["workspace"], waiting["workspace"]);
    assert_eq!(completed["sessionId"], waiting["sessionId"]);
    fixture.stop(false).await;
}

#[tokio::test]
async fn an_expired_retry_window_does_not_admit_another_agent() {
    let mut fixture = Fixture::new().await;
    let chat = fixture
        .start_chat(message(
            "fixture:agent-failures:[{\"code\":\"server_overloaded\"}]",
        ))
        .await;
    let run_id = fixture.chat_run(&chat).await;
    let waiting = fixture
        .until(&run_id, |run| run["retry"].is_object() || finished(run))
        .await;
    assert_eq!(waiting["status"], RunStatus::Queued, "{waiting}");
    let mut retry = waiting["retry"].clone();
    retry["nextAttemptAt"] = (now() - 100).into();
    retry["deadlineAt"] = (now() - 1).into();
    fixture
        .service
        .store
        .change_run(
            &run_id,
            cairn_installation::runs::RunChanges {
                retry: Some(Some(serde_json::from_value(retry.clone()).unwrap())),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let stopped = fixture.until_finished(&run_id).await;
    assert_eq!(stopped["status"], RunStatus::Failed);
    assert!(text(&stopped, "error").contains("10-minute limit"));
    let marker: Value = serde_json::from_slice(
        &tokio::fs::read(Path::new(text(&stopped, "workspace")).join("fixture-retry-work.json"))
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(marker["attempts"], 1);
    fixture.stop(false).await;
}

#[tokio::test]
async fn agent_failure_without_a_message_keeps_its_code_for_the_retry() {
    let mut fixture = Fixture::new().await;
    let prompt = "fixture:agent-failures:[{\"codexErrorInfo\":\"serverOverloaded\"}]";
    let chat_id = fixture.start_chat(message(prompt)).await;
    let run_id = fixture.chat_run(&chat_id).await;
    let waiting = fixture
        .until(&run_id, |run| run["retry"].is_object() || finished(run))
        .await;
    assert_eq!(waiting["status"], RunStatus::Queued, "{waiting}");
    assert_eq!(waiting["retry"]["cause"]["kind"], "server");
    let id = run_id.clone();
    let events = fixture
        .service
        .store
        .read(move |db| db.events_fixture(&id, 0, 1000))
        .await
        .unwrap();
    let failures = events
        .iter()
        .filter(|event| event["type"] == "turn.failed")
        .map(|event| event["payload"]["error"].clone())
        .collect::<Vec<_>>();
    assert_eq!(
        failures,
        [json!({ "codexErrorInfo": "serverOverloaded", "message": "Conversation interrupted." })]
    );
    fixture.stop(false).await;
}

#[tokio::test]
async fn agent_retries_stop_after_five_attempts_or_when_the_error_becomes_terminal() {
    for terminal in [false, true] {
        let mut fixture = Fixture::new().await;
        // Codex's report for a model at capacity.
        let capacity = "Selected model is at capacity. Please try a different model.";
        let transient = json!({ "codexErrorInfo": "serverOverloaded", "message": capacity });
        let failures = if terminal {
            vec![
                transient,
                json!({ "code": "authentication_error", "message": "Reconnect account" }),
            ]
        } else {
            vec![transient; 6]
        };
        let prompt = format!("fixture:agent-failures:{}", json!(failures));
        let chat = fixture.start_chat(message(&prompt)).await;
        let run_id = fixture.chat_run(&chat).await;
        let attempts = if terminal { 1 } else { 5 };
        for attempt in 1..=attempts {
            let waiting = fixture
                .until(&run_id, |run| {
                    (run["status"] == RunStatus::Queued && run["retry"]["attempt"] == attempt)
                        || finished(run)
                })
                .await;
            assert_eq!(waiting["status"], RunStatus::Queued, "{waiting}");
            assert_eq!(waiting["retry"]["attempt"], attempt);
            let mut retry = waiting["retry"].clone();
            retry["nextAttemptAt"] = now().into();
            fixture
                .service
                .store
                .change_run(
                    &run_id,
                    cairn_installation::runs::RunChanges {
                        retry: Some(Some(serde_json::from_value(retry.clone()).unwrap())),
                        ..Default::default()
                    },
                )
                .await
                .unwrap();
        }
        let failed = fixture.until_finished(&run_id).await;
        assert_eq!(failed["status"], RunStatus::Failed);
        assert_eq!(failed["retry"]["attempt"], attempts);
        let expected = if terminal {
            "Reconnect account"
        } else {
            capacity
        };
        assert_eq!(failed["summary"], expected);
        let id = run_id.clone();
        let events = fixture
            .service
            .store
            .read(move |db| db.events_fixture(&id, 0, 1000))
            .await
            .unwrap();
        let failures = events
            .iter()
            .filter(|event| event["type"] == "turn.failed")
            .map(|event| text(&event["payload"]["error"], "message"))
            .collect::<Vec<_>>();
        let mut reported = vec![capacity; attempts as usize];
        reported.push(expected);
        assert_eq!(
            failures, reported,
            "Each failed attempt reports its reason once"
        );
        let marker: Value = serde_json::from_slice(
            &tokio::fs::read(Path::new(text(&failed, "workspace")).join("fixture-retry-work.json"))
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(marker["attempts"], attempts + 1);
        fixture.stop(false).await;
    }
}

#[tokio::test]
async fn cancellation_during_retry_wait_does_not_launch_another_agent() {
    let mut fixture = Fixture::new().await;
    let chat = fixture.start_chat(message("fixture:agent-failures:[{\"code\":\"request_timeout\",\"message\":\"Request timed out\"}]")).await;
    let run_id = fixture.chat_run(&chat).await;
    let waiting = fixture
        .until(&run_id, |run| run["retry"].is_object() || finished(run))
        .await;
    assert_eq!(waiting["status"], RunStatus::Queued, "{waiting}");
    fixture
        .service
        .worker
        .cancel(&fixture.service, &run_id)
        .await
        .unwrap();
    fixture
        .until(&run_id, |run| run["status"] == RunStatus::Cancelled)
        .await;
    // The conversation shows the stop: it ends the retry Cairn had scheduled.
    let id = run_id.clone();
    let events = fixture
        .service
        .store
        .read(move |db| db.events_fixture(&id, 0, 1000))
        .await
        .unwrap();
    let after_retry = events
        .iter()
        .skip_while(|event| event["type"] != "retry.scheduled")
        .filter(|event| event["type"] == "status")
        .map(|event| text(event, "text"))
        .collect::<Vec<_>>();
    assert_eq!(after_retry, ["cancelled"], "{events:?}");
    let marker: Value = serde_json::from_slice(
        &tokio::fs::read(Path::new(text(&waiting, "workspace")).join("fixture-retry-work.json"))
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(marker["attempts"], 1);
    fixture.stop(false).await;
}

#[tokio::test]
async fn managed_task_retry_releases_its_account_during_backoff() {
    let mut fixture = Fixture::new().await;
    fixture.stop(false).await;
    let s = fixture.service.clone();
    let mut account = serde_json::to_value(
        s.accounts
            .create(&s, Provider::Codex, "Retry fixture")
            .await
            .unwrap(),
    )
    .unwrap();
    account["state"] = "ready".into();
    account["maxConcurrentRuns"] = 1.into();
    s.store.save_account_fixture(account.clone()).await.unwrap();
    let account_id = text(&account, "id");
    codex_credentials(
        &s,
        account_id,
        json!({
            "access_token": "synthetic",
            "refresh_token": "synthetic-refresh",
            "account_id": "retry-fixture",
        }),
    )
    .await;
    fixture.start().await;
    let run = fixture
        .enqueue("fixture:agent-failures:[{\"message\":\"workspace routing discovery timed out\"}]")
        .await;
    let run_id = text(&run, "id");
    let waiting = fixture
        .until(run_id, |run| run["retry"].is_object() || finished(run))
        .await;
    assert_eq!(waiting["status"], RunStatus::Queued, "{waiting}");
    let mut retry = waiting["retry"].clone();
    retry["nextAttemptAt"] = (now() + 120_000).into();
    s.store
        .change_run(
            run_id,
            cairn_installation::runs::RunChanges {
                retry: Some(Some(serde_json::from_value(retry.clone()).unwrap())),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let other = fixture
        .enqueue("Inspect another workspace during backoff")
        .await;
    let completed = fixture.until_finished(text(&other, "id")).await;
    assert_eq!(completed["status"], RunStatus::Succeeded, "{completed}");
    assert_eq!(completed["accountId"], account_id);
    assert_eq!(
        s.store.run(run_id).await.unwrap()["status"],
        RunStatus::Queued
    );
    retry["nextAttemptAt"] = now().into();
    s.store
        .change_run(
            run_id,
            cairn_installation::runs::RunChanges {
                retry: Some(Some(serde_json::from_value(retry.clone()).unwrap())),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let completed = fixture.until_finished(run_id).await;
    assert_eq!(completed["status"], RunStatus::Succeeded, "{completed}");
    assert_eq!(completed["sessionId"], waiting["sessionId"]);
    assert_eq!(completed["workspace"], waiting["workspace"]);
    fixture.stop(false).await;
}

#[tokio::test]
async fn preparing_runs_keep_their_original_cancellation_reservation() {
    let root = TempDir::new().unwrap();
    std::fs::create_dir(root.path().join("home")).unwrap();
    let service = Service::new(Config {
        concurrency: 2,
        codex_bin: common::fixture("codex.mjs"),
        claude_bin: common::fixture("claude.mjs"),
        ..common::config(root.path())
    })
    .await
    .unwrap();
    common::relay_fixture::claimed(&service).await.unwrap();
    let (_, run) = service
        .chat_run_fixture(MAIN_AGENT_ID, None, "Inspect the workspace.")
        .await
        .unwrap();
    let run_id = text(&run, "id");
    service.worker.initialize(&service).await.unwrap();

    // Admission reserves the run before asynchronous preparation marks it running.
    // Another tick in that window must preserve the original execution and its stop token.
    let reservation = tokio_util::sync::CancellationToken::new();
    service
        .worker
        .active
        .lock()
        .await
        .insert(run_id.to_owned(), reservation.clone());
    service.worker.tick(&service).await.unwrap();
    reservation.cancel();
    let reservation_retained = service.worker.active.lock().await[run_id].is_cancelled();

    service.shutdown.cancel();
    service.worker.active.lock().await.remove(run_id);
    service.worker.close().await;
    assert!(
        reservation_retained,
        "A queued run already in preparation must not launch a second execution"
    );
}

#[tokio::test]
async fn verbose_tools_do_not_hide_chat_answers_or_failures() {
    for fail in [false, true] {
        let mut fixture = Fixture::new().await;
        let s = &fixture.service;
        let prompt = if fail {
            "fixture:verbose-tools-fail"
        } else {
            "fixture:verbose-tools"
        };
        let chat_id = fixture.start_chat(message(prompt)).await;
        let run_id = fixture.chat_run(&chat_id).await;
        let run = fixture.until_finished(&run_id).await;
        let expected = if fail {
            RunStatus::Failed
        } else {
            RunStatus::Succeeded
        };
        assert_eq!(run["status"], expected);
        let (events, tail) = s
            .store
            .read(move |db| {
                Ok((
                    db.events_fixture(&run_id, 0, 500)?,
                    serde_json::to_value(db.events_before(&run_id, i64::MAX)?.0)?,
                ))
            })
            .await
            .unwrap();
        let tools: Vec<_> = events
            .iter()
            .filter(|e| text(&e["payload"]["item"], "id").starts_with("verbose-"))
            .collect();
        assert_eq!(
            tools.len(),
            60,
            "Every tool outcome must survive verbose output"
        );
        assert!(
            tools.iter().all(|event| {
                event["payload"]["item"]["details_truncated"] == true
                    && serde_json::to_vec(event).unwrap().len() < 16 * 1024
            }),
            "Tool results must be abbreviated instead of dropping later calls"
        );
        if fail {
            assert!(events.iter().any(|e| e["type"] == "turn.failed"
                && text(&e["payload"]["error"], "message") == "Failure after verbose tools"));
        } else {
            assert!(text(&run, "summary").contains("Ready for the next step."));
            assert!(
                tail.as_array().unwrap().iter().any(|event| {
                    let item = &event["payload"]["item"];
                    item["type"] == "agent_message"
                        && text(item, "text").contains("Ready for the next step.")
                }),
                "The completed answer must reach the newest conversation page even after verbose tools"
            );
            assert!(events.iter().any(|e| e["type"] == "item.updated"
                && e["payload"]["item"]["text"] == "Still responding after verbose tools."));
            assert!(events.iter().any(|event| event["type"] == "turn.completed"));
        }
        fixture.stop(false).await;
    }
}

#[tokio::test]
async fn chat_switches_codex_claude_and_back_without_losing_workspace_or_replaying_turns() {
    let mut fixture = Fixture::new().await;
    let s = &fixture.service;
    claude_account(s, 4).await;
    let chat_id = fixture
        .start_chat(message(
            "Keep the existing design and inspect the workspace.",
        ))
        .await;
    let run_id = fixture.chat_run(&chat_id).await;
    let delivered = |message: &Value| {
        let id = message["id"].clone();
        move |run: &Value| {
            run["status"] == RunStatus::Succeeded && run["chatExecution"]["messageId"] == id
        }
    };
    let first = fixture
        .until(&run_id, |r| r["status"] == RunStatus::Succeeded)
        .await;
    let marker = Path::new(text(&first, "workspace")).join("preserved.txt");
    std::fs::write(&marker, "completed work").unwrap();
    let mut to_claude = message("Continue with Claude.");
    to_claude["provider"] = "claude".into();
    to_claude["model"] = "opus[1m]".into();
    s.chat_send(&chat_id, to_claude.clone()).await.unwrap();
    let second = fixture.until(&run_id, delivered(&to_claude)).await;
    assert_eq!(second["snapshot"]["agent"]["provider"], "claude");
    // The native session belongs to the run, not to the account.
    let claude_home = fixture.run_directory(&run_id).join("home/.claude");
    assert_eq!(second["workspace"], first["workspace"]);
    assert_eq!(std::fs::read_to_string(&marker).unwrap(), "completed work");
    let messages = std::fs::read_to_string(claude_home.join("user-messages.jsonl")).unwrap();
    assert!(messages.contains("Keep the existing design"));
    assert!(messages.contains("The approach looks good"));
    assert!(messages.contains("Continue with Claude"));
    let invocations = || std::fs::read_to_string(claude_home.join("invocations.jsonl")).unwrap();
    assert!(!invocations().contains("--resume"));
    // Retry the same delivery after the provider changed: it stays one message.
    s.chat_send(&chat_id, to_claude.clone()).await.unwrap();
    assert_eq!(
        s.chat_detail(&chat_id).await.unwrap()["messages"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    let continuation = message("Keep using Claude.");
    s.chat_send(&chat_id, continuation.clone()).await.unwrap();
    fixture.until(&run_id, delivered(&continuation)).await;
    assert!(invocations().contains("--resume"));
    let mut back = message("Return to Codex and preserve the decisions.");
    back["provider"] = "codex".into();
    s.chat_send(&chat_id, back.clone()).await.unwrap();
    let last = fixture.until(&run_id, delivered(&back)).await;
    assert_eq!(last["snapshot"]["agent"]["provider"], "codex");
    assert_eq!(last["workspace"], first["workspace"]);
    assert!(text(&last, "summary").contains("Claude fixture completed"));
    assert!(text(&last, "summary").contains("Keep the existing design"));
    assert_eq!(std::fs::read_to_string(&marker).unwrap(), "completed work");
    assert_eq!(s.chat_detail(&chat_id).await.unwrap()["runId"], run_id);
    fixture.stop(false).await;
}

#[tokio::test]
async fn chats_continue_with_equivalent_access_but_not_reduced_access() {
    let mut fixture = Fixture::new().await;
    let s = fixture.service.clone();
    let chat_id = fixture.start_chat(message("Inspect the workspace.")).await;
    let run_id = fixture.chat_run(&chat_id).await;
    fixture
        .until(&run_id, |r| r["status"] == RunStatus::Succeeded)
        .await;
    let follow_up = message("Continue with the same access.");
    s.chat_send(&chat_id, follow_up.clone()).await.unwrap();
    fixture
        .until(&run_id, |r| {
            r["status"] == RunStatus::Succeeded
                && r["chatExecution"]["messageId"] == follow_up["id"]
        })
        .await;

    let mut agent = s.agent_snapshot(MAIN_AGENT_ID).await.unwrap();
    agent["access"]["sandbox"] = "read-only".into();
    s.store.put_agent_fixture(agent).await.unwrap();
    s.chat_send(&chat_id, message("Continue with less access."))
        .await
        .unwrap();
    // The queue pauses with the reason instead of reusing the wider workspace.
    let error = eventually(
        Duration::from_secs(10),
        Duration::from_millis(30),
        async || {
            let chat = s.chat_detail(&chat_id).await.unwrap();
            chat["error"].as_str().map(str::to_owned)
        },
    )
    .await;
    assert!(error.contains("Agent access was reduced"), "{error}");
    fixture.stop(false).await;
}

#[tokio::test]
async fn chat_messages_invoke_dollar_skills_without_changing_the_visible_text() {
    let mut fixture = Fixture::new().await;
    let s = &fixture.service;
    s.skills
        .save(
            "review",
            "---\nname: review\ndescription: Review the current changes\n---\nReview carefully.\n",
            None,
        )
        .await
        .unwrap();
    let request = "$review the workspace and keep $HOME untouched.";
    let chat_id = fixture.start_chat(message(request)).await;
    let run_id = fixture.chat_run(&chat_id).await;
    let run = fixture
        .until(&run_id, |r| r["status"] == RunStatus::Succeeded)
        .await;
    let execution = text(&run["chatExecution"], "text");
    assert!(execution.starts_with(request), "{execution}");
    assert!(execution.contains("<invoked_skills>"), "{execution}");
    assert!(execution.contains("\n- review\n"), "{execution}");
    assert!(!execution.contains("- HOME"), "{execution}");
    assert_eq!(
        s.chat_detail(&chat_id).await.unwrap()["messages"][0]["text"],
        request
    );
    fixture.stop(false).await;
}

#[tokio::test]
async fn usage_exhaustion_switches_accounts_and_preserves_the_conversation() {
    let mut fixture = Fixture::new().await;
    fixture.stop(false).await;
    let s = &fixture.service;
    let mut accounts = Vec::new();
    let mut usage = json!({});
    for (name, used) in [("More capacity", 10), ("Backup", 30)] {
        let mut account =
            serde_json::to_value(s.accounts.create(s, Provider::Codex, name).await.unwrap())
                .unwrap();
        let id = text(&account, "id").to_owned();
        let limits = json!({
            "ordinaryUsageAllowed": true,
            "rateLimits": {
                "limitId": "codex",
                "primary": {
                    "usedPercent": used,
                    "windowDurationMins": 300,
                    "resetsAt": now() / 1000 + 7200,
                },
            },
        });
        account["state"] = "ready".into();
        account["usage"] = accounts::codex::normalize(&limits);
        s.store.save_account_fixture(account).await.unwrap();
        let tokens = json!({
            "account_id": id,
            "access_token": "synthetic-access",
            "refresh_token": "synthetic-refresh",
        });
        codex_credentials(s, &id, tokens).await;
        usage[&id] = limits;
        accounts.push(id);
    }
    tokio::fs::write(fixture.root.path().join("usage.json"), usage.to_string())
        .await
        .unwrap();
    fixture.start().await;
    let run = fixture.enqueue("fixture:exhaust").await;
    let completed = fixture.until_finished(text(&run, "id")).await;
    assert_eq!(completed["status"], RunStatus::Succeeded, "{completed}");
    assert_eq!(completed["accountId"], accounts[1]);
    assert_eq!(completed["accountName"], "Backup");
    assert_eq!(completed["sessionId"], "fixture-chat");
    fixture.stop(false).await;
}

#[tokio::test]
async fn claude_individual_spend_limit_waits_for_capacity_and_remains_cancellable() {
    let mut fixture = Fixture::new().await;
    let s = fixture.service.clone();
    let account = claude_account(&s, 4).await;
    let mut sent = message("fixture:spend-limit");
    sent["provider"] = "claude".into();
    let chat = fixture.start_chat(sent).await;
    let run_id = fixture.chat_run(&chat).await;
    let waiting = fixture
        .until(&run_id, |run| {
            run["accountWaitReason"].as_str().is_some_and(|reason| {
                reason.contains("available usage") || reason.starts_with("Usage exhausted.")
            })
        })
        .await;
    assert_eq!(waiting["status"], RunStatus::Running, "{waiting}");
    assert!(waiting["sessionId"].is_string(), "{waiting}");
    assert!(waiting["error"].is_null(), "{waiting}");
    let saved = s.store.account_fixture(&account).await.unwrap().unwrap();
    assert!(saved["exhausted"].is_object(), "{saved}");
    let session = common::relay_fixture::context(&s).await;
    reqwest::Client::new()
        .post(format!("{}/api/runs/{run_id}/cancel", fixture.url))
        .header("x-test-relay-token", text(&session, "value"))
        .json(&json!({}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    let cancelled = fixture
        .until(&run_id, |run| run["status"] == RunStatus::Cancelled)
        .await;
    assert_eq!(cancelled["status"], RunStatus::Cancelled, "{cancelled}");
    assert_eq!(cancelled["sessionId"], waiting["sessionId"]);
    assert_eq!(cancelled["workspace"], waiting["workspace"]);
    fixture.stop(false).await;
}
