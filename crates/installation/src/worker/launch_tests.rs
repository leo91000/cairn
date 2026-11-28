use super::*;
use crate::{capabilities::plugin_packages::Authority, plugins::run_credentials::RunCredentials};

async fn preparation_fixture(root: &Path) -> (Arc<Service>, Value) {
    use crate::run_fixture::DbRunFixture;
    let config = serde_json::from_value(json!({
        "dataDir": root.join("data"),
        "home": root.join("home"),
        "workspaceRoots": [root],
        "publicUrl": "http://localhost:4310",
        "host": "127.0.0.1",
        "port": 0,
        "codexBin": "false",
        "concurrency": 1,
        "logger": false,
        "workerEnabled": false,
        "runnerUrl": "http://runner.invalid",
    }))
    .unwrap();
    let service = Service::new(config).await.unwrap();
    let agent = service
        .agent_snapshot(crate::config::MAIN_AGENT_ID)
        .await
        .unwrap();
    let id = crate::config::id();
    let record = json!({
        "id": id, "taskId": id, "status": "running",
        "snapshot": { "agent": agent, "task": { "id": id }, "projects": [] },
    });
    let run = service
        .store
        .transaction(move |db| db.add_run_fixture(&record, None))
        .await
        .unwrap();
    (service, run)
}

#[test]
fn first_negotiated_turn_cannot_fall_back_to_automatic_account_replacement() {
    let saved = RunCheckpoint::fresh(None);
    let mut env = Environment::new();
    assert!(!has_live_environment(&saved, false, &env));
    env.insert("CAIRN_PROVIDER_TURN".into(), "first".into());
    assert!(has_live_environment(&saved, false, &env));
}

#[test]
fn explicit_session_restart_and_provider_switch_require_environment_fencing() {
    let mut run = json!({"snapshot":{"agent":{"provider":"codex"}},"sessionId":"session"});
    assert!(can_reuse_environment(Provider::Codex, &run).unwrap());
    run["sessionId"] = Value::Null;
    assert!(!can_reuse_environment(Provider::Codex, &run).unwrap());
    run["sessionId"] = "session".into();
    assert!(!can_reuse_environment(Provider::Claude, &run).unwrap());
}

#[tokio::test]
async fn queued_turn_wins_over_an_idle_environment_close() {
    let root = tempfile::tempdir().unwrap();
    let (service, mut run) = preparation_fixture(root.path()).await;
    let id = text(&run, "id").to_owned();
    let mut saved = RunCheckpoint::fresh(None);
    saved.runner_id = Some(Some("attempt".into()));
    saved.completed = Some(true);
    saved.held_environment = Some(super::super::checkpoint::HeldEnvironment {
        attempt: "attempt".into(),
        generation: "generation".into(),
        expires_at: now() + 60_000,
    });
    let checkpoint = Checkpoint::new(service.store.clone(), id.clone(), saved.clone());
    checkpoint.persist().await.unwrap();
    service
        .store
        .transaction(move |db| {
            saved.completed = Some(false);
            saved.turn_pending = Some(true);
            saved.store_in(db, &id)?;
            db.change_run(
                &id,
                crate::runs::RunChanges {
                    status: Some(RunStatus::Queued),
                    ..Default::default()
                },
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let mut account = None;
    let cancel = CancellationToken::new();
    let sensitive = History::default();
    let execution = Execution {
        s: &service,
        id: text(&run, "id").into(),
        admission_revision: run["environmentAdmissionRevision"].clone(),
        run: &mut run,
        account: &mut account,
        preparation: None,
        cancel: &cancel,
        checkpoint: &checkpoint,
        sensitive: &sensitive,
        observation: None,
        provider: Provider::Codex,
        directory: root.path().join("run"),
        attempt_id: String::new(),
        reservation: None,
        retained: false,
        turn: String::new(),
        output_probe: None,
    };
    assert!(execution.close_idle_environment().await.unwrap().is_none());
    let saved = checkpoint.snapshot().await;
    assert!(!saved.completed());
    assert_eq!(saved.turn_pending, Some(true));
    assert!(saved.held_environment.is_some());
}

#[tokio::test]
async fn ready_job_wakeup_prevents_idle_close_before_scheduler_adoption() {
    let root = tempfile::tempdir().unwrap();
    let (service, mut run) = preparation_fixture(root.path()).await;
    let id = text(&run, "id").to_owned();
    service.store.transaction({
        let id = id.clone();
        move |db| {
            db.change_run(&id, crate::runs::RunChanges { status: Some(RunStatus::Waiting), ..Default::default() })?;
            db.save_wakeup(false, &id, Some(&serde_json::from_value(json!({
                "id":"wake","messageId":null,"reason":"job","seconds":60,
                "createdAt":now(),"wakeAt":now()+60_000,
                "condition":{"type":"job_finished","jobId":"77d595dc-97b7-4c13-923d-e163baf49ea8"}
            }))?))?;
            db.0.execute("INSERT INTO background_jobs VALUES('request',?,'attempt','77d595dc-97b7-4c13-923d-e163baf49ea8','hash',?)",
                rusqlite::params![id, serde_json::to_string(&crate::background_jobs::Outcome::Finished { receipt: crate::background_jobs::Receipt { exit_code:Some(0),signal:None,elapsed_ms:1,stopped:false,timed_out:false,log_tail:"done".into(),log_truncated:false } })?])?;
            Ok(())
        }
    }).await.unwrap();
    run = service.store.run(&id).await.unwrap();
    let mut saved = RunCheckpoint::fresh(None);
    saved.runner_id = Some(Some("attempt".into()));
    saved.completed = Some(true);
    saved.held_environment = Some(super::super::checkpoint::HeldEnvironment {
        attempt: "attempt".into(),
        generation: "generation".into(),
        expires_at: now() + 60_000,
    });
    let checkpoint = Checkpoint::new(service.store.clone(), id.clone(), saved);
    checkpoint.persist().await.unwrap();
    let mut account = None;
    let cancel = CancellationToken::new();
    let sensitive = History::default();
    let execution = Execution {
        s: &service,
        id: id.clone(),
        admission_revision: run["environmentAdmissionRevision"].clone(),
        run: &mut run,
        account: &mut account,
        preparation: None,
        cancel: &cancel,
        checkpoint: &checkpoint,
        sensitive: &sensitive,
        observation: None,
        provider: Provider::Codex,
        directory: root.path().join("run"),
        attempt_id: String::new(),
        reservation: None,
        retained: false,
        turn: String::new(),
        output_probe: None,
    };
    assert!(execution.close_idle_environment().await.unwrap().is_none());
    assert!(checkpoint.snapshot().await.held_environment.is_some());
    service
        .store
        .write(move |db| db.save_wakeup(false, &id, None))
        .await
        .unwrap();
    checkpoint
        .update(|saved| saved.held_environment.as_mut().unwrap().expires_at = now() - 1)
        .await
        .unwrap();
    assert!(matches!(
        execution.close_idle_environment().await.unwrap(),
        Some(HeldTurn::Expired)
    ));
    assert!(checkpoint.snapshot().await.held_environment.is_some());
    checkpoint
        .update(|saved| saved.held_environment.as_mut().unwrap().expires_at = now() + 60_000)
        .await
        .unwrap();
    assert!(matches!(
        execution.close_idle_environment().await.unwrap(),
        Some(HeldTurn::Close)
    ));
}

#[tokio::test]
async fn held_deadline_failure_survives_successful_physical_close() {
    for credential_clear_failure in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let (service, mut run) = preparation_fixture(root.path()).await;
        let id = text(&run, "id").to_owned();
        let authority = crate::capabilities::plugin_packages::Authority::for_run(&service, &run)
            .await
            .unwrap();
        let mut workspace = Workspace {
            prepared: json!({}),
            state_home: root.path().into(),
            env: Environment::new(),
            mcp: json!({}),
            authorization: crate::plugins::run_credentials::Authorization::new(
                &service,
                authority,
                &RunCredentials::default(),
            ),
        };
        let mut saved = RunCheckpoint::fresh(None);
        saved.completed = Some(true);
        saved.runner_id = Some(Some("attempt".into()));
        saved.held_environment = Some(super::super::checkpoint::HeldEnvironment {
            attempt: "attempt".into(),
            generation: "generation".into(),
            expires_at: now() - 1,
        });
        let checkpoint = Checkpoint::new(service.store.clone(), id.clone(), saved);
        checkpoint.persist().await.unwrap();
        let mut account = if credential_clear_failure {
            let home = root.path().join("account");
            tokio::fs::create_dir_all(home.join("auth.json"))
                .await
                .unwrap();
            Some(Lease {
                account_id: crate::config::id(),
                provider: Provider::Codex,
                run_id: id.clone(),
                home,
                model: "fixture".into(),
            })
        } else {
            None
        };
        let cancel = CancellationToken::new();
        let sensitive = History::default();
        let mut execution = Execution {
            s: &service,
            id: id.clone(),
            admission_revision: run["environmentAdmissionRevision"].clone(),
            run: &mut run,
            account: &mut account,
            preparation: None,
            cancel: &cancel,
            checkpoint: &checkpoint,
            sensitive: &sensitive,
            observation: None,
            provider: Provider::Codex,
            directory: root.path().join("run"),
            attempt_id: String::new(),
            reservation: None,
            retained: false,
            turn: String::new(),
            output_probe: None,
        };
        assert!(!execution.wait_for_next_turn(&mut workspace).await.unwrap());
        let pending = service.store.run_state(&id).await.unwrap();
        assert_eq!(pending.status, RunStatus::Queued);
        assert!(pending.recovery_pending);
        checkpoint
            .update(|saved| saved.terminal_code = Some(Some(0)))
            .await
            .unwrap();
        let saved = checkpoint.snapshot().await;
        assert!(saved.held_environment.is_none());
        assert_eq!(saved.settled().unwrap().status, RunStatus::Failed);
        let expected_message = saved.settled().unwrap().error.clone().unwrap();
        assert_eq!(
            expected_message.contains("time limit"),
            !credential_clear_failure
        );
        service
            .store
            .transaction(move |db| {
                assert!(!super::super::schedule::finish_recovery(
                    db,
                    &id,
                    Some(&saved)
                )?);
                let run = db.run_state(&id)?.unwrap();
                assert_eq!(run.status, RunStatus::Failed);
                assert_eq!(run.error.unwrap(), expected_message);
                Ok(())
            })
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn confirmed_turn_keeps_success_when_finish_crosses_the_budget() {
    for timed_out in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let (service, mut run) = preparation_fixture(root.path()).await;
        let id = text(&run, "id").to_owned();
        let authority = Authority::for_run(&service, &run).await.unwrap();
        run["chatExecution"] = json!({ "messageId": "turn" });
        let workspace = Workspace {
            prepared: json!({}),
            state_home: root.path().into(),
            env: Environment::new(),
            mcp: json!({}),
            authorization: crate::plugins::run_credentials::Authorization::new(
                &service,
                authority,
                &RunCredentials::default(),
            ),
        };
        let mut saved = RunCheckpoint::fresh(Some(0));
        let finished_at = now() - 1_000;
        saved.runner_id = Some(Some("attempt".into()));
        saved.completed = Some(true);
        saved.last_message = Some("Completed answer".into());
        saved.held_environment = Some(super::super::checkpoint::HeldEnvironment {
            attempt: "attempt".into(),
            generation: "generation".into(),
            expires_at: now() - 1,
        });
        saved.retained_completion = Some(super::super::checkpoint::RetainedCompletion {
            message_id: "turn".into(),
            finished_at,
            output_summary: None,
        });
        let checkpoint = Checkpoint::new(service.store.clone(), id.clone(), saved.clone());
        assert!(checkpoint.expired());
        let cancel = CancellationToken::new();
        let sensitive = History::default();
        let execution = Execution {
            s: &service,
            id: id.clone(),
            admission_revision: Value::Null,
            run: &mut run,
            account: &mut None,
            preparation: None,
            cancel: &cancel,
            checkpoint: &checkpoint,
            sensitive: &sensitive,
            observation: None,
            provider: Provider::Codex,
            directory: root.path().join("run"),
            attempt_id: "attempt".into(),
            reservation: None,
            retained: true,
            turn: String::new(),
            output_probe: None,
        };
        let exit = Exit {
            code: Some(if timed_out { STOPPED } else { 0 }),
            timed_out,
            exhausted: false,
        };
        execution
            .finish(&workspace, &exit, &saved, true)
            .await
            .unwrap();
        let state = service.store.run_state(&id).await.unwrap();
        if timed_out {
            assert_eq!(state.status, RunStatus::Failed);
            assert_eq!(state.error.as_deref(), Some("Run exceeded its time limit."));
        } else {
            assert_eq!(state.status, RunStatus::Succeeded);
            assert_eq!(state.finished_at, Some(finished_at));
            assert_eq!(state.summary, "Completed answer");
            assert!(state.error.is_none());
        }
    }
}

#[tokio::test]
async fn pending_native_cleanup_survives_expiry_during_observation_without_hiding_a_real_timeout() {
    use crate::run_fixture::DbRunFixture;
    for timed_out in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let (service, mut run) = preparation_fixture(root.path()).await;
        let id = text(&run, "id").to_owned();
        let authority = Authority::for_run(&service, &run).await.unwrap();
        let workspace = Workspace {
            prepared: json!({}),
            state_home: root.path().into(),
            env: Environment::new(),
            mcp: json!({}),
            authorization: crate::plugins::run_credentials::Authorization::new(
                &service,
                authority,
                &RunCredentials::default(),
            ),
        };
        let saved = RunCheckpoint::from_value(json!({
            "prepared": { "backend": "native", "isolated": true },
            "runnerId": "attempt",
            "finalizationFailureAttempt": "attempt",
            "lastError": "Native cleanup: Disk cleanup is pending.",
            "remainingMs": 0
        }))
        .unwrap();
        let checkpoint = Checkpoint::new(service.store.clone(), id.clone(), saved.clone());
        assert!(checkpoint.expired());
        let cancel = CancellationToken::new();
        let sensitive = History::default();
        let execution = Execution {
            s: &service,
            id: id.clone(),
            admission_revision: run["environmentAdmissionRevision"].clone(),
            run: &mut run,
            account: &mut None,
            preparation: None,
            cancel: &cancel,
            checkpoint: &checkpoint,
            sensitive: &sensitive,
            observation: None,
            provider: Provider::Codex,
            directory: root.path().join("run"),
            attempt_id: "attempt".into(),
            reservation: None,
            retained: false,
            turn: String::new(),
            output_probe: None,
        };
        let exit = Exit {
            code: Some(crate::runner::CONTROLLER_FAILED),
            timed_out,
            exhausted: false,
        };
        execution
            .finish(&workspace, &exit, &saved, true)
            .await
            .unwrap();

        let expected = if timed_out {
            "Run exceeded its time limit."
        } else {
            "Disk cleanup is pending."
        };
        let state = service.store.run_state(&id).await.unwrap();
        assert_eq!(state.error.as_deref(), Some(expected));
        let events = service
            .store
            .read(move |db| db.events_fixture(&id, 0, 100))
            .await
            .unwrap();
        let status = events
            .iter()
            .find(|event| event["type"] == "status")
            .unwrap();
        assert_eq!(status["payload"]["error"]["message"], expected);
    }
}

#[tokio::test]
async fn unmanaged_account_selection_completes_without_a_lease() {
    let root = tempfile::tempdir().unwrap();
    let (service, mut run) = preparation_fixture(root.path()).await;
    let mut account = None;
    let cancel = CancellationToken::new();
    let checkpoint = Checkpoint::new(
        service.store.clone(),
        text(&run, "id").into(),
        RunCheckpoint::fresh(None),
    );
    let sensitive = History::default();
    let mut execution = Execution {
        s: &service,
        id: text(&run, "id").into(),
        admission_revision: run["environmentAdmissionRevision"].clone(),
        run: &mut run,
        account: &mut account,
        preparation: None,
        cancel: &cancel,
        checkpoint: &checkpoint,
        sensitive: &sensitive,
        observation: None,
        provider: Provider::Codex,
        directory: root.path().join("run"),
        attempt_id: String::new(),
        reservation: None,
        retained: true,
        turn: String::new(),
        output_probe: None,
    };
    tokio::time::timeout(Duration::from_secs(2), execution.wait_for_account(""))
        .await
        .unwrap()
        .unwrap();
    assert!(execution.account.is_none());
    // The environment cap applies even when the provider budget is unlimited.
    assert!(!checkpoint.expired());
    checkpoint
        .update(|saved| {
            saved.held_environment = Some(super::super::checkpoint::HeldEnvironment {
                attempt: "attempt".into(),
                generation: "generation".into(),
                expires_at: now() - 1,
            });
        })
        .await
        .unwrap();
    let error = execution.wait_for_account("").await.unwrap_err();
    assert_eq!(error.code.as_deref(), Some(HELD_DEADLINE_CODE));
    assert!(error.message.contains("time limit"));
}

#[tokio::test]
async fn expiration_retires_materialized_admission_rejected_before_checkpoint_publication() {
    use crate::{agent_fixture::StoreAgentFixture, node_fixture::StoreNodeFixture};
    use axum::{Json, Router, http::StatusCode, response::IntoResponse};
    use std::sync::atomic::{AtomicUsize, Ordering};
    let root = tempfile::tempdir().unwrap();
    let (original, mut run) = preparation_fixture(root.path()).await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let deleted = Arc::new(AtomicUsize::new(0));
    let calls = deleted.clone();
    let router = Router::new().fallback(move |request: axum::extract::Request| {
        let calls = calls.clone();
        async move {
            if request.uri().path() == "/health" {
                return Json(json!({ "diskRetirementProtocol":2 })).into_response();
            }
            if request.method() == axum::http::Method::DELETE {
                calls.fetch_add(1, Ordering::SeqCst);
            }
            StatusCode::NOT_FOUND.into_response()
        }
    });
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let mut cloned = (*original).clone();
    cloned.config.public_url = format!("http://{address}");
    cloned.config.runner_url = format!("http://{address}");
    let service = Arc::new(cloned);
    let (node, replacement, attempt) = (
        crate::config::id(),
        crate::config::id(),
        crate::config::id(),
    );
    for id in [&node, &replacement] {
        service
            .store
            .put_node_fixture(json!({
                "id": id, "status": "online", "accepting": true, "executionReady": true,
                "runtimeId": nodes::component_runtime(), "lastSeen": now(), "slots": 2,
            }))
            .await
            .unwrap();
    }
    let mut agent = service
        .agent_snapshot(crate::config::MAIN_AGENT_ID)
        .await
        .unwrap();
    agent["access"]["nodes"] = json!([node, replacement]);
    service.store.put_agent_fixture(agent).await.unwrap();
    let mut snapshot = run["snapshot"].clone();
    snapshot["agent"]["access"]["nodes"] = json!([node, replacement]);
    let id = text(&run, "id").to_owned();
    service
        .store
        .change_run(
            &id,
            crate::runs::RunChanges {
                snapshot: Some(serde_json::from_value(snapshot).unwrap()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    service
        .store
        .update_run_placement(
            &id,
            nodes::run_placement::PlacementChanges {
                pinned_node_id: Some(Some(node.clone())),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    run = service.store.run(&id).await.unwrap();
    assert_ne!(run["snapshot"]["agent"]["access"]["backend"], "auto");
    let saved = RunCheckpoint::from_value(json!({ "prepared": { "backend": "firecracker", "isolated": true }, "completed": false, "remainingMs": 60000 })).unwrap();
    saved.store(&service.store, &id).await.unwrap();
    let checkpoint = Checkpoint::new(service.store.clone(), id.clone(), saved);
    let authority = Authority::for_run(&service, &run).await.unwrap();
    let publication = crate::plugins::run_credentials::Authorization::new(
        &service,
        authority,
        &RunCredentials::default(),
    );
    placement::reserve(&service, &run, &attempt).await.unwrap();
    placement::materialize(&service, &attempt).await.unwrap();
    service
        .worker
        .active
        .lock()
        .await
        .insert(id.clone(), CancellationToken::new());
    service
        .expire_environment(
            &id,
            crate::environment_expiration::Expiration::new(
                cairn_contracts::EnvironmentExpirationCause::NodeChange,
            ),
            |_| Ok(()),
        )
        .await
        .unwrap();
    let state = service.store.run_state(&id).await.unwrap();
    let result = checkpoint
        .update_and_publish(
            |saved| {
                saved.bind_paid_admission(&attempt, &state);
                saved.node_id = Some(Some(node.clone()));
            },
            move |db| publication.authorize_in(db),
        )
        .await;
    assert!(result.is_err());
    assert!(checkpoint.snapshot().await.runner_id().is_none());
    service.worker.active.lock().await.remove(&id);
    service.detach_stopped_environments().await.unwrap();
    let queued = service.store.list("environment-retirements").await.unwrap();
    assert!(
        queued[0]["attempts"]
            .as_array()
            .unwrap()
            .iter()
            .any(|owned| owned["id"] == attempt)
    );
    let mut revoked = service.node_boundary(&node).await.unwrap();
    revoked["revoked"] = true.into();
    service.store.put_node_fixture(revoked).await.unwrap();
    service.retire_environments().await.unwrap();
    assert_eq!(
        deleted.load(Ordering::SeqCst),
        1,
        "the orphan admission was fenced"
    );
    assert!(
        service
            .store
            .attempt(&attempt)
            .await
            .unwrap()
            .unwrap()
            .released
    );
    let next = service.store.run(&id).await.unwrap();
    let admitted = crate::config::id();
    assert_eq!(
        placement::reserve(&service, &next, &admitted)
            .await
            .unwrap()["nodeId"],
        replacement
    );
    placement::release(&service, &admitted).await.unwrap();
    server.abort();
    service.shutdown.cancel();
}

#[tokio::test]
async fn a_stale_preparation_cannot_overwrite_a_new_backend_selection() {
    let root = tempfile::tempdir().unwrap();
    let (service, mut run) = preparation_fixture(root.path()).await;
    let id = text(&run, "id").to_owned();
    run["environmentAdmissionRevision"] = "old-selection".into();
    let mut snapshot = run["snapshot"].clone();
    snapshot["agent"]["access"]["backend"] = "native".into();
    service.store.change_run(&id, crate::runs::RunChanges {
        snapshot: Some(serde_json::from_value(snapshot.clone()).unwrap()),
        boundary: Some(json!({ "environmentState": "expired", "environmentAdmissionRevision": "new-selection" })),
        ..Default::default()
    }).await.unwrap();
    let checkpoint = Checkpoint::new(
        service.store.clone(),
        id.clone(),
        RunCheckpoint::fresh(Some(60_000)),
    );
    let cancel = CancellationToken::new();
    let mut account = None;
    let sensitive = History::default();
    let mut execution = Execution {
        s: &service,
        id: id.clone(),
        admission_revision: "old-selection".into(),
        run: &mut run,
        account: &mut account,
        preparation: None,
        cancel: &cancel,
        checkpoint: &checkpoint,
        sensitive: &sensitive,
        observation: None,
        provider: Provider::Codex,
        directory: root.path().into(),
        attempt_id: String::new(),
        reservation: None,
        retained: false,
        turn: String::new(),
        output_probe: None,
    };
    let error = execution.refresh_access().await.unwrap_err();
    assert!(placement::is_no_capacity(&error), "{error:?}");
    assert_eq!(service.store.run(&id).await.unwrap()["snapshot"], snapshot);
    assert!(service.store.attempts().await.unwrap().is_empty());
    service.shutdown.cancel();
}

#[tokio::test]
async fn initial_preparation_rechecks_revocation_between_remote_lookup_and_clone() {
    use axum::{Router, routing::get};
    use std::sync::atomic::{AtomicUsize, Ordering};
    let root = tempfile::tempdir().unwrap();
    let (service, mut run) = preparation_fixture(root.path()).await;
    let authority = crate::capabilities::plugin_packages::Authority::for_run(&service, &run)
        .await
        .unwrap();
    let authorization = crate::plugins::run_credentials::Authorization::new(
        &service,
        authority,
        &RunCredentials::default(),
    );
    let requests = Arc::new(AtomicUsize::new(0));
    let count = requests.clone();
    let revoked = service.clone();
    let run_id = text(&run, "id").to_owned();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = Router::new()
        .route(
            "/repo/info/refs",
            get(move || {
                let (s, id, count) = (revoked.clone(), run_id.clone(), count.clone());
                async move {
                    count.fetch_add(1, Ordering::SeqCst);
                    s.store
                        .change_run(
                            &id,
                            crate::runs::RunChanges {
                                cancel_requested_at: Some(Some(now())),
                                ..Default::default()
                            },
                        )
                        .await
                        .unwrap();
                    "0123456789012345678901234567890123456789\trefs/heads/main\n"
                }
            }),
        )
        .route("/repo/HEAD", get(|| async { "ref: refs/heads/main\n" }));
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let source = root.path().join("source");
    std::fs::create_dir(&source).unwrap();
    for args in [
        vec!["init".to_owned(), "-q".into()],
        vec![
            "remote".into(),
            "add".into(),
            "origin".into(),
            format!("http://{address}/repo"),
        ],
    ] {
        assert!(
            std::process::Command::new("git")
                .args(args)
                .current_dir(&source)
                .status()
                .unwrap()
                .success()
        );
    }
    let project = crate::config::id();
    run["snapshot"]["task"]["projectId"] = project.clone().into();
    run["snapshot"]["projects"] = json!([{
        "id": project, "name": "Revoked project", "path": source,
        "sourceMode": "remote", "baseBranch": "main",
    }]);
    let error = execution::prepare_authorized(
        &run,
        &service.config,
        &RunCredentials::default(),
        None,
        None,
        &authorization,
    )
    .await
    .unwrap_err();
    assert_eq!(error.status, 403, "{error:?}");
    assert_eq!(
        requests.load(Ordering::SeqCst),
        1,
        "no clone may contact the remote after ls-remote revokes the run"
    );
    assert!(
        !service
            .config
            .data_dir
            .join("runs")
            .join(text(&run, "id"))
            .join("workspace")
            .join(project)
            .exists()
    );
    server.abort();
    service.shutdown.cancel();
}

#[tokio::test]
async fn remote_preparation_revocation_blocks_publication_and_every_launch() {
    use axum::{Router, routing::post};
    let root = tempfile::tempdir().unwrap();
    let (service, mut run) = preparation_fixture(root.path()).await;
    let authority = crate::capabilities::plugin_packages::Authority::for_run(&service, &run)
        .await
        .unwrap();
    let authorization = crate::plugins::run_credentials::Authorization::new(
        &service,
        authority,
        &RunCredentials::default(),
    );
    let revoked = service.clone();
    let run_id = text(&run, "id").to_owned();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = Router::new().route(
        "/prepare/attempt",
        post(move || {
            let (s, id) = (revoked.clone(), run_id.clone());
            async move {
                s.store
                    .change_run(
                        &id,
                        crate::runs::RunChanges {
                            cancel_requested_at: Some(Some(now())),
                            ..Default::default()
                        },
                    )
                    .await
                    .unwrap();
                axum::Json(json!({}))
            }
        }),
    );
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let id = text(&run, "id").to_owned();
    let checkpoint = Checkpoint::new(
        service.store.clone(),
        id.clone(),
        RunCheckpoint::fresh(Some(60_000)),
    );
    let cancel = CancellationToken::new();
    let mut account = None;
    let sensitive = History::default();
    let mut execution = Execution {
        s: &service,
        id,
        admission_revision: run["environmentAdmissionRevision"].clone(),
        run: &mut run,
        account: &mut account,
        preparation: None,
        cancel: &cancel,
        checkpoint: &checkpoint,
        sensitive: &sensitive,
        observation: None,
        provider: Provider::Codex,
        directory: root.path().into(),
        attempt_id: String::new(),
        reservation: None,
        retained: false,
        turn: String::new(),
        output_probe: None,
    };
    let error = execution
        .prepare_remote(
            &format!("http://{address}"),
            "attempt",
            "remote",
            &authorization,
        )
        .await
        .unwrap_err();
    assert_eq!(error.status, 403);
    let workspace = Workspace {
        prepared: json!({ "cwd": root.path() }),
        state_home: root.path().into(),
        env: Environment::new(),
        mcp: json!({}),
        authorization,
    };
    // A separate/retried launch must also recheck, even if a caller already
    // prepared a workspace. The nonexistent binary must never be spawned.
    for _ in 0..2 {
        let launch = Launch {
            binary: "/nonexistent-revoked-agent".into(),
            args: vec![],
            prompt: String::new(),
        };
        let error = execution
            .supervise(&workspace, launch, &mut 0, false)
            .await
            .err()
            .unwrap();
        assert_eq!(error.status, 403);
        assert!(!checkpoint.read(RunCheckpoint::launched).await);
    }
    server.abort();
    service.shutdown.cancel();
}

#[tokio::test]
async fn active_clean_exit_requires_managed_provider_completion() {
    use crate::run_fixture::DbRunFixture;
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
    for backend in ["native", "firecracker", "shared"] {
        for completed in [false, true] {
            let id = crate::config::id();
            let run_fixture = json!({ "id": id, "taskId": id, "status": "running" });
            service
                .store
                .transaction(move |db| db.add_run_fixture(&run_fixture, None).map(|_| ()))
                .await
                .unwrap();
            let saved = RunCheckpoint::from_value(json!({
                "prepared": { "backend": backend, "isolated": backend != "shared" },
                "completed": completed, "terminalCode": 0, "remainingMs": null,
            }))
            .unwrap();
            let checkpoint = Checkpoint::new(service.store.clone(), id.clone(), saved.clone());
            let mut run = service.store.run(&id).await.unwrap();
            let mut account = None;
            let cancel = CancellationToken::new();
            let sensitive = History::default();
            let execution = Execution {
                s: &service,
                id: id.clone(),
                admission_revision: run["environmentAdmissionRevision"].clone(),
                run: &mut run,
                account: &mut account,
                preparation: None,
                cancel: &cancel,
                checkpoint: &checkpoint,
                sensitive: &sensitive,
                observation: None,
                provider: Provider::Codex,
                directory: root.path().into(),
                attempt_id: String::new(),
                reservation: None,
                retained: false,
                turn: String::new(),
                output_probe: None,
            };
            let workspace = Workspace {
                prepared: json!({ "output": root.path().join("missing-result").to_string_lossy() }),
                state_home: root.path().into(),
                env: Environment::new(),
                mcp: json!({}),
                authorization: crate::plugins::run_credentials::Authorization::new(
                    &service,
                    Authority::default(),
                    &RunCredentials::default(),
                ),
            };
            execution
                .finish(
                    &workspace,
                    &Exit {
                        code: Some(0),
                        ..Default::default()
                    },
                    &saved,
                    false,
                )
                .await
                .unwrap();
            let state = service.store.run_state(&id).await.unwrap();
            let expected = if completed || backend == "shared" {
                RunStatus::Succeeded
            } else {
                RunStatus::Failed
            };
            assert_eq!(
                state.status, expected,
                "backend={backend}, completed={completed}"
            );
            if expected == RunStatus::Failed {
                assert!(state.error.unwrap().contains("completion was recorded"));
            }
        }
    }
    service.shutdown.cancel();
}

#[test]
fn controller_messages_follow_the_execution_backend() {
    assert_eq!(
        controller_name(&json!({ "snapshot": { "agent": { "access": { "backend": "native" } } } })),
        "Native controller"
    );
    assert_eq!(
        controller_name(
            &json!({ "snapshot": { "agent": { "access": { "backend": "firecracker" } } } })
        ),
        "VM controller"
    );
    assert_eq!(controller_name(&json!({})), "VM controller");
}

#[tokio::test]
async fn antigravity_preparation_preserves_retry_authorization_until_handoff() {
    let root = tempfile::tempdir().unwrap();
    let (service, run) = preparation_fixture(root.path()).await;
    let id = text(&run, "id").to_owned();
    let retry = crate::run_retry::Retry::next(
        None,
        crate::run_retry::Cause {
            kind: crate::run_retry::FailureKind::Server,
            message: "The Antigravity model is temporarily unavailable.".into(),
            startup: false,
        },
        now() - 10_000,
    )
    .unwrap();
    service
        .store
        .change_run(
            &id,
            crate::runs::RunChanges {
                session_id: Some(Some("saved-session".into())),
                resume_available: Some(true),
                retry: Some(Some(retry.clone())),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let mut run = service.store.run(&id).await.unwrap();
    let checkpoint = Checkpoint::new(
        service.store.clone(),
        id.clone(),
        RunCheckpoint::fresh(None),
    );
    let cancel = CancellationToken::new();
    let sensitive = History::default();
    let execution = Execution {
        s: &service,
        id: id.clone(),
        admission_revision: run["environmentAdmissionRevision"].clone(),
        run: &mut run,
        account: &mut None,
        preparation: None,
        cancel: &cancel,
        checkpoint: &checkpoint,
        sensitive: &sensitive,
        observation: None,
        provider: Provider::Antigravity,
        directory: root.path().join("run"),
        attempt_id: String::new(),
        reservation: None,
        retained: false,
        turn: String::new(),
        output_probe: None,
    };
    execution.mark_running().await.unwrap();
    assert_eq!(
        service.store.run_state(&id).await.unwrap().retry,
        Some(retry)
    );
    assert!(
        !checkpoint
            .read(|saved| saved.antigravity_replay_refused(Some(&id)))
            .await
    );
    execution.mark_paid_retry_started().await.unwrap();
    let state = service.store.run_state(&id).await.unwrap();
    assert!(state.retry.unwrap().next_attempt_at.is_none());
    assert!(!state.resume_available);
    assert!(
        checkpoint
            .read(|saved| saved.antigravity_replay_refused(Some(&id)))
            .await
    );
    assert!(execution.mark_paid_retry_started().await.is_err());
}

#[tokio::test]
async fn an_unrecorded_antigravity_session_durably_requires_new_input() {
    let root = tempfile::tempdir().unwrap();
    let (service, mut run) = preparation_fixture(root.path()).await;
    let authority = Authority::for_run(&service, &run).await.unwrap();
    run["snapshot"]["agent"]["provider"] = "antigravity".into();
    run["sessionId"] = Value::Null;
    let workspace = Workspace {
        prepared: json!({ "cwd": root.path() }),
        state_home: root.path().into(),
        env: Environment::new(),
        mcp: json!({}),
        authorization: crate::plugins::run_credentials::Authorization::new(
            &service,
            authority,
            &RunCredentials::default(),
        ),
    };
    let id = text(&run, "id").to_owned();
    let mut saved = RunCheckpoint::fresh(None);
    saved.record_launch(Some("first-turn".into()));
    let checkpoint = Checkpoint::new(service.store.clone(), id.clone(), saved);
    let cancel = CancellationToken::new();
    let sensitive = History::default();
    let execution = Execution {
        s: &service,
        id,
        admission_revision: run["environmentAdmissionRevision"].clone(),
        run: &mut run,
        account: &mut None,
        preparation: None,
        cancel: &cancel,
        checkpoint: &checkpoint,
        sensitive: &sensitive,
        observation: None,
        provider: Provider::Antigravity,
        directory: root.path().join("run"),
        attempt_id: String::new(),
        reservation: None,
        retained: false,
        turn: String::new(),
        output_probe: None,
    };
    let error = execution.resume_session(&workspace).await.unwrap_err();
    assert_eq!(
        error.code.as_deref(),
        Some("antigravity_session_unconfirmed")
    );
    let saved = checkpoint.snapshot().await;
    assert!(saved.antigravity_replay_refused(Some("first-turn")));
}

#[test]
fn a_fresh_antigravity_mission_turn_is_distinct_from_recovering_it() {
    let run = json!({ "id": "run", "missionTurnId": "wakeup" });
    let mut saved = RunCheckpoint {
        turn_pending: Some(true),
        ..Default::default()
    };
    saved.record_launch(Some("previous-wakeup".into()));
    let prompt = "Continue after the wait.";
    let fresh = scheduled_execution(&run, Some(prompt), Provider::Antigravity, &saved, false);
    assert_eq!(fresh["messageId"], "wakeup");
    assert_eq!(fresh["text"], prompt);
    assert_eq!(fresh["recovery"], false);
    assert_eq!(
        scheduled_execution(&run, Some(prompt), Provider::Codex, &saved, false)["recovery"],
        true
    );

    saved.record_launch(launched_turn(&run, Provider::Antigravity));
    assert_eq!(
        scheduled_execution(&run, Some(prompt), Provider::Antigravity, &saved, false)["recovery"],
        true
    );
    let wakeup_retry = scheduled_execution(&run, Some(prompt), Provider::Antigravity, &saved, true);
    assert_eq!(wakeup_retry["text"], prompt);
    let first = json!({ "id": "run", "snapshot": { "task": { "prompt": "Prepare a report." } } });
    let first_retry =
        scheduled_execution(&first, Some(prompt), Provider::Antigravity, &saved, true);
    assert_eq!(first_retry["text"], "Task brief:\nPrepare a report.");
}

#[tokio::test]
async fn mode_restart_before_first_session_preserves_available_sessions_and_other_recovery_rules() {
    let root = tempfile::tempdir().unwrap();
    let s = Service::new(crate::config::Config {
        data_dir: root.path().join("data"),
        home: root.path().join("home"),
        workspace_roots: vec![root.path().into()],
        public_url: "http://localhost:4310".into(),
        host: "127.0.0.1".into(),
        port: 0,
        codex_bin: Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/codex.mjs")
            .to_string_lossy()
            .into_owned(),
        claude_bin: "/bin/false".into(),
        opencode_bin: "/bin/false".into(),
        opencode_go_usage_url: crate::accounts::opencode::USAGE_URL.into(),
        antigravity_bin: "agy".into(),
        concurrency: 1,
        logger: false,
        worker_enabled: false,
        runner_url: String::new(),
    })
    .await
    .unwrap();
    for provider in ["codex", "claude", "opencode"] {
        let mut run = json!({
            "isolated": true, "nativeModeInterruption": crate::config::id(),
            "snapshot": { "agent": { "provider": provider, "access": { "backend": "native" } } },
        });
        assert!(
            saved_session(&s, &run, root.path(), root.path(), true)
                .await
                .unwrap()
                .is_none(),
            "{provider}"
        );
        run["sessionId"] = if provider == "opencode" {
            "ses_saved"
        } else {
            "saved-session"
        }
        .into();
        assert_eq!(
            saved_session(&s, &run, root.path(), root.path(), true)
                .await
                .unwrap()
                .as_deref(),
            run["sessionId"].as_str(),
            "{provider}"
        );
        run["sessionId"] = Value::Null;
        run["nativeModeInterruption"] = Value::Null;
        assert!(
            saved_session(&s, &run, root.path(), root.path(), true)
                .await
                .is_err(),
            "{provider}"
        );
        run["nativeModeInterruption"] = crate::config::id().into();
        run["snapshot"]["agent"]["access"]["backend"] = "firecracker".into();
        assert!(
            saved_session(&s, &run, root.path(), root.path(), true)
                .await
                .is_err(),
            "{provider}"
        );
    }
    let run = json!({
        "isolated": true, "nativeModeInterruption": crate::config::id(),
        "snapshot": { "agent": { "provider": "antigravity", "access": { "backend": "native" } } },
    });
    assert!(
        saved_session(&s, &run, root.path(), root.path(), false)
            .await
            .is_err()
    );
}
