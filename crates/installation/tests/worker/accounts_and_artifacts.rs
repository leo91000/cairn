use super::*;

#[tokio::test]
async fn native_worker_records_artifacts_and_completes_task() {
    let mut fixture = Fixture::new().await;
    let run = fixture.enqueue("Inspect the fixture").await;
    let id = text(&run, "id").to_owned();
    let complete = fixture.until_finished(&id).await;
    assert_eq!(complete["status"], RunStatus::Succeeded, "{complete}");
    assert_eq!(complete["sessionId"], "fixture-chat");
    let events = fixture
        .service
        .store
        .read(move |db| db.events_fixture(&id, 0, 500))
        .await
        .unwrap();
    assert!(events.iter().any(|e| e["payload"].is_object()));
    fixture.stop(false).await;
}

#[tokio::test]
async fn parallel_managed_tasks_resume_after_restart_without_refresh_credentials_in_runs() {
    let mut fixture = Fixture::new().await;
    fixture.stop(false).await;
    let s = fixture.service.clone();
    let mut account = serde_json::to_value(
        s.accounts
            .create(&s, Provider::Codex, "Shared")
            .await
            .unwrap(),
    )
    .unwrap();
    account["state"] = "ready".into();
    s.store.save_account_fixture(account.clone()).await.unwrap();
    let account_id = text(&account, "id");
    let tokens = json!({
        "access_token": "synthetic",
        "refresh_token": "secret-refresh",
        "account_id": "shared",
    });
    codex_credentials(&s, account_id, tokens).await;
    fixture.start().await;
    let first = fixture.enqueue("fixture:chat-hang first task").await;
    let second = fixture.enqueue("fixture:chat-hang second task").await;
    for run in [&first, &second] {
        let running = fixture
            .until(text(run, "id"), |r| r["sessionId"] == "fixture-chat")
            .await;
        assert_eq!(running["accountId"], account_id);
        assert!(
            !fixture
                .run_directory(text(run, "id"))
                .join("codex/auth.json")
                .exists()
        );
    }
    fixture.stop(true).await;
    fixture.start().await;
    for run in [&first, &second] {
        let completed = fixture.until_finished(text(run, "id")).await;
        assert_eq!(completed["status"], RunStatus::Succeeded, "{completed}");
        assert_eq!(completed["sessionId"], "fixture-chat");
        assert_eq!(completed["accountId"], account_id);
    }
    assert_eq!(
        s.vault
            .get(&format!("codex-account:{account_id}"))
            .await
            .unwrap()
            .unwrap()["tokens"]["refresh_token"],
        "secret-refresh"
    );
    fixture.stop(false).await;
}

#[tokio::test]
async fn abrupt_restart_fences_previous_process_and_resumes_workspace() {
    let mut fixture = Fixture::new().await;
    let run = fixture.enqueue("fixture:restart").await;
    let id = text(&run, "id");
    let running = fixture.until_restart_ready(id).await;
    fixture.stop(true).await;
    fixture.start().await;
    let completed = fixture.until_finished(id).await;
    assert_eq!(completed["status"], RunStatus::Succeeded, "{completed}");
    assert_eq!(completed["workspace"], running["workspace"]);
    assert_eq!(completed["sessionId"], running["sessionId"]);
    assert_eq!(completed["resumeCount"], 1);
    assert!(text(&completed, "summary").contains("saved conversation"));
    assert_eq!(completed["snapshot"]["agent"]["timeoutMinutes"], 0);
    assert_eq!(
        fixture.checkpoint(id).await.get("remainingMs"),
        Some(&Value::Null)
    );
    fixture.stop(false).await;
}

#[tokio::test]
async fn automatic_shared_process_turns_do_not_acquire_a_managed_node_owner() {
    let mut fixture = Fixture::new().await;
    let s = fixture.service.clone();
    s.agent_fixture(
        json!({ "access": { "backend": "auto" } }),
        Some(MAIN_AGENT_ID),
    )
    .await
    .unwrap();
    let chat = fixture.start_chat(message("First automatic message")).await;
    let run = fixture.chat_run(&chat).await;
    let first = fixture.until_finished(&run).await;
    assert_eq!(first["status"], RunStatus::Succeeded, "{first}");
    assert_eq!(
        first["snapshot"]["agent"]["access"]["backend"],
        "firecracker"
    );
    let checkpoint = fixture.checkpoint(&run).await;
    assert!(checkpoint["nodeId"].is_null(), "{checkpoint}");
    assert!(checkpoint["runnerId"].is_null(), "{checkpoint}");
    assert!(s.store.list("node-volumes").await.unwrap().is_empty());
    assert!(
        s.store
            .attempts_fixture()
            .await
            .unwrap()
            .iter()
            .all(|attempt| attempt["released"] == true)
    );
    s.chat_send(&chat, message("Second automatic message"))
        .await
        .unwrap();
    let second = fixture
        .until(&run, |r| {
            r["status"] == RunStatus::Succeeded && r["resumeCount"] == 1
        })
        .await;
    assert_eq!(second["sessionId"], first["sessionId"]);
    assert_eq!(second["workspace"], first["workspace"]);
    fixture.stop(false).await;
}

#[tokio::test]
async fn a_fresh_chat_turn_executes_while_its_old_manager_cleanup_keeps_failing() {
    let mut fixture = Fixture::new().await;
    let s = fixture.service.clone();
    let chat = fixture.start_chat(message("Before expiration")).await;
    let run = fixture.chat_run(&chat).await;
    let first = fixture.until_finished(&run).await;
    assert_eq!(first["status"], RunStatus::Succeeded, "{first}");
    fixture.until_worker_quiescent().await;
    fixture.stop(false).await;
    let manager = s.config.data_dir.join("runs").join(&run);
    tokio::fs::write(manager.join("old.txt"), b"old working files")
        .await
        .unwrap();
    let (chat_id, run_id, invalid) = (chat.clone(), run.clone(), fixture.root.path().to_owned());
    s.store
        .transaction(move |db| {
            let mut chat = db.conversation(&chat_id)?.unwrap();
            let old = now() - 8 * 86_400_000;
            chat.created_at = old;
            chat.updated_at = old;
            chat.last_activity_at = Some(old);
            db.save_conversation(&chat)?;
            db.change_run(
                &run_id,
                cairn_installation::runs::RunChanges {
                    finished_at: Some(Some(old)),
                    ..Default::default()
                },
            )?;
            db.update_run_boundary(
                &run_id,
                &json!({ "workspaces":[{ "kind":"worktree", "path":invalid }] }),
            )
        })
        .await
        .unwrap();
    s.save_conversation_retention(json!({ "inactivityDays":7 }))
        .await
        .unwrap();
    s.cleanup_conversations().await.unwrap();
    s.retire_environments().await.unwrap();
    let pending = s.store.list("environment-retirements").await.unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0]["fenced"], true);
    assert_eq!(pending[0]["local"], true);
    assert_eq!(pending[0]["cleaned"], false);
    assert!(pending[0]["error"].is_string());
    s.chat_send(&chat, message("Fresh message while old cleanup fails"))
        .await
        .unwrap();
    fixture.start().await;
    let resumed = fixture
        .until(&run, |run| {
            run["status"] == RunStatus::Succeeded
                && text(run, "summary").contains("Fresh message while old cleanup fails")
        })
        .await;
    assert!(resumed["environmentExpiredAt"].is_number());
    assert!(!manager.join("old.txt").exists());
    assert_eq!(
        s.store.list("environment-retirements").await.unwrap()[0]["cleaned"],
        false
    );
    fixture.stop(false).await;
    tokio::fs::write(manager.join("new.txt"), b"fresh working files")
        .await
        .unwrap();
    s.store
        .transaction(|db| {
            for mut retired in db.list("environment-retirements")? {
                retired["workspaces"] = json!([]);
                retired["retryAfter"] = Value::Null;
                db.put("environment-retirements", &retired)?;
            }
            Ok(())
        })
        .await
        .unwrap();
    s.retire_environments().await.unwrap();
    assert!(
        s.store
            .list("environment-retirements")
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        tokio::fs::read(manager.join("new.txt")).await.unwrap(),
        b"fresh working files"
    );
}

#[tokio::test]
async fn native_chat_turns_reuse_the_same_run_and_conversation() {
    let mut fixture = Fixture::new().await;
    let s = fixture.service.clone();
    let chat_id = fixture.start_chat(message("First message")).await;
    let run_id = fixture.chat_run(&chat_id).await;
    let first = fixture.until_finished(&run_id).await;
    assert_eq!(first["status"], RunStatus::Succeeded, "{first}");
    s.chat_send(&chat_id, message("Second message"))
        .await
        .unwrap();
    let second = fixture
        .until(&run_id, |r| {
            r["status"] == RunStatus::Succeeded && text(r, "summary").contains("Second message")
        })
        .await;
    assert_eq!(second["sessionId"], "fixture-chat");
    assert_eq!(s.chat_detail(&chat_id).await.unwrap()["runId"], run_id);
    s.chat_send(&chat_id, message("fixture:disconnect"))
        .await
        .unwrap();
    let failed = fixture
        .until(&run_id, |r| r["status"] == RunStatus::Failed)
        .await;
    assert!(
        !text(&failed, "summary").contains("Second message"),
        "A failed attempt reused the previous result: {failed}"
    );
    assert!(text(&failed, "error").contains("Codex"), "{failed}");
    fixture.stop(false).await;
}

#[tokio::test]
async fn sleeping_chat_survives_worker_restart_and_releases_capacity_before_waking() {
    let mut fixture = Fixture::with_concurrency(1).await;
    let s = fixture.service.clone();
    let chat = fixture
        .start_chat(message(
            "fixture:slow-finish Wait for CI then finish the original task",
        ))
        .await;
    let run = fixture.chat_run(&chat).await;
    let current = fixture
        .until(&run, |r| {
            r["status"] == "running" && r["sessionId"].is_string()
        })
        .await;
    fixture
        .until_chat(&chat, async |detail| {
            detail["messages"][0]["status"] == "delivered"
        })
        .await;
    // Host-only fixtures use a synthetic workspace grant; production VMs receive
    // this same turn-bound grant through run_configuration.
    let token = "synthetic-sleep-token";
    s.store
        .transaction({
            let grant: cairn_installation::mcps::record::RunGrant = serde_json::from_value(json!({
                "runId": run,
                "messageId": current["chatExecution"]["messageId"],
                "servers": {},
                "installation": false, "workspace": true,
            }))
            .unwrap();
            move |db| {
                db.save_mcp_run_grant(&cairn_installation::auth::hex_digest(token), &grant, None)
            }
        })
        .await
        .unwrap();
    let wake = cairn_installation::chats::wait_for::schedule(
        &s,
        token,
        &json!({ "conditions": [{ "type": "delay", "seconds": 15 }], "reason": "Wait for CI" }),
    )
    .await
    .unwrap()["wakeup"]
        .clone();
    let first = fixture.until_finished(&run).await;
    assert_eq!(first["status"], "succeeded");
    let conversation = fixture
        .run_directory(&run)
        .join("codex/fixture-conversation.json");
    let first_thread: Value =
        serde_json::from_slice(&std::fs::read(&conversation).unwrap()).unwrap();
    // Use the actual workspace: preparation may select a generation directory.
    let preserved = Path::new(text(&first, "workspace")).join("sleep-preserved.txt");
    std::fs::write(&preserved, "work before the wait").unwrap();
    fixture.stop(false).await;
    fixture.start().await;
    let other = fixture
        .enqueue("Another conversation uses capacity during the wait")
        .await;
    assert_eq!(
        fixture.until_finished(text(&other, "id")).await["status"],
        "succeeded"
    );
    assert_eq!(s.chat_detail(&chat).await.unwrap()["wakeup"], wake);
    let resumed = fixture
        .until(&run, |r| {
            r["status"] == "succeeded"
                && text(r, "summary").contains("Cairn resumed this conversation")
        })
        .await;
    assert_eq!(resumed["sessionId"], first["sessionId"]);
    let run_id = run.clone();
    let events = s
        .store
        .read(move |db| db.events_fixture(&run_id, 0, 100))
        .await
        .unwrap();
    let wake_event = events
        .iter()
        .find(|event| event["type"] == "chat.wakeup")
        .unwrap();
    assert!(wake_event["createdAt"].as_i64().unwrap() >= wake["wakeAt"].as_i64().unwrap());
    assert_eq!(resumed["workspace"], first["workspace"]);
    let resumed_thread: Value =
        serde_json::from_slice(&std::fs::read(conversation).unwrap()).unwrap();
    assert_eq!(
        resumed_thread["fixtureGeneration"],
        first_thread["fixtureGeneration"]
    );
    assert_eq!(resumed_thread["turns"].as_array().unwrap().len(), 2);
    assert_eq!(resumed_thread["turns"][0], first_thread["turns"][0]);
    assert_eq!(
        std::fs::read_to_string(preserved).unwrap(),
        "work before the wait"
    );
    let detail = s.chat_detail(&chat).await.unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|event| event["type"] == "chat.user")
            .count(),
        1
    );
    assert!(detail["wakeup"].is_null());
    fixture.stop(false).await;
}

#[tokio::test]
async fn condition_wait_yields_the_only_slot_to_its_dependency_and_resumes_the_same_session() {
    let mut fixture = Fixture::with_concurrency(1).await;
    let s = fixture.service.clone();
    s.agent_fixture(
        json!({ "name": "Main agent", "access": { "installation": true } }),
        Some(MAIN_AGENT_ID),
    )
    .await
    .unwrap();
    let chat = fixture
        .start_chat(message("fixture:slow-finish Finish after other work"))
        .await;
    let run = fixture.chat_run(&chat).await;
    let current = fixture
        .until(&run, |r| {
            r["status"] == "running" && r["sessionId"].is_string()
        })
        .await;
    fixture
        .until_chat(&chat, async |detail| {
            detail["messages"][0]["status"] == "delivered"
        })
        .await;
    let other = fixture
        .enqueue("Dependency runs while the conversation waits")
        .await;
    let token = "synthetic-condition-token";
    s.store
        .transaction({
            let grant: cairn_installation::mcps::record::RunGrant = serde_json::from_value(json!({
                "runId": run,
                "messageId": current["chatExecution"]["messageId"],
                "servers": {},
                "workspace": true,
                "installation": true,
            }))
            .unwrap();
            move |db| {
                db.save_mcp_run_grant(&cairn_installation::auth::hex_digest(token), &grant, None)
            }
        })
        .await
        .unwrap();
    let wait = cairn_installation::chats::wait_for::schedule(
        &s,
        token,
        &json!({
            "conditions": [{ "type": "run_finished", "runId": other["id"] }],
            "reason": "Join the dependency",
            "timeoutSeconds": 60,
        }),
    )
    .await
    .unwrap();
    assert_eq!(wait["deferred"], true);
    assert_eq!(
        fixture.until_finished(text(&other, "id")).await["status"],
        "succeeded"
    );
    let resumed = fixture
        .until(&run, |r| {
            r["status"] == "succeeded"
                && text(r, "summary").contains("Cairn resumed this conversation")
        })
        .await;
    assert_eq!(resumed["sessionId"], current["sessionId"]);
    assert!(
        now() < wait["wakeup"]["wakeAt"].as_i64().unwrap(),
        "The result resumes the conversation before the fallback timer"
    );
    assert!(text(&resumed, "summary").contains(text(&other, "id")));
    let thread: Value = serde_json::from_slice(
        &std::fs::read(
            fixture
                .run_directory(&run)
                .join("codex/fixture-conversation.json"),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(thread["turns"].as_array().unwrap().len(), 2);
    let detail = s.chat_detail(&chat).await.unwrap();
    assert!(detail["wakeup"].is_null());
    let run_id = run.clone();
    let events = s
        .store
        .read(move |db| db.events_fixture(&run_id, 0, 100))
        .await
        .unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|event| event["type"] == "chat.user")
            .count(),
        1
    );
    fixture.stop(false).await;
}

#[tokio::test]
async fn chat_attachments_survive_worker_restart_and_reach_codex() {
    let mut fixture = Fixture::new().await;
    let s = fixture.service.clone();
    let chat = s.chat_create(json!({})).await.unwrap();
    let chat_id = text(&chat, "id");
    let attachment_id = id();
    let upload = axum::http::Request::builder()
        .method("PUT")
        .uri("/?name=design.png")
        .body(Body::from(PNG))
        .unwrap();
    s.attachment_http(chat_id, &attachment_id, upload)
        .await
        .unwrap();
    let mut hanging = message("fixture:chat-hang");
    hanging["attachmentIds"] = json!([attachment_id]);
    s.chat_send(chat_id, hanging).await.unwrap();
    let run_id = fixture.chat_run(chat_id).await;
    fixture
        .until(&run_id, |r| r["sessionId"] == "fixture-chat")
        .await;
    // Wait for the original input receipt, so recovery resumes the accepted turn.
    fixture
        .until_chat(chat_id, async |chat: &Value| {
            chat["messages"][0]["status"] == "delivered"
        })
        .await;
    fixture.stop(true).await;
    fixture.start().await;
    let completed = fixture.until_finished(&run_id).await;
    assert_eq!(completed["status"], RunStatus::Succeeded, "{completed}");
    assert_eq!(completed["resumeCount"], 1);
    let conversation = fixture
        .run_directory(&run_id)
        .join("codex/fixture-conversation.json");
    let thread: Value = serde_json::from_slice(&std::fs::read(conversation).unwrap()).unwrap();
    let turns = thread["turns"].as_array().unwrap();
    assert_eq!(turns.len(), 2);
    for turn in turns {
        let find = |items: &Value, kind: &str| {
            items
                .as_array()
                .unwrap()
                .iter()
                .find(|item| item["type"] == kind)
                .unwrap()
                .clone()
        };
        let user = find(&turn["items"], "userMessage");
        let image = find(&user["content"], "localImage");
        assert_eq!(std::fs::read(text(&image, "path")).unwrap(), PNG);
    }
    fixture.stop(false).await;
}
