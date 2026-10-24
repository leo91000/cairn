mod common;

use axum::{body::Body, http::StatusCode};
use cairn_installation::{
    auth::{InstallationIdentity, InstallationRole},
    config::{MAIN_AGENT_ID, id},
    run_status::RunStatus,
    service::Service,
};
use serde_json::{Value, json};
use std::sync::Arc;
use tempfile::TempDir;

async fn fixture() -> (TempDir, Arc<Service>, Value) {
    let root = TempDir::new().unwrap();
    std::fs::create_dir(root.path().join("home")).unwrap();
    let s = Service::new(common::config(root.path())).await.unwrap();
    common::relay_fixture::claimed(&s).await.unwrap();
    let source = s
        .agent(json!({ "name": "Claude", "provider": "claude" }), None)
        .await
        .unwrap();
    (root, s, source)
}

fn input(source: &Value) -> Value {
    json!({ "sourceId": source["id"], "targetId": MAIN_AGENT_ID, "confirm": true })
}

async fn task(s: &Service, source: &Value) -> Value {
    s.task(
        json!({ "name": "Existing task", "prompt": "Inspect", "agentId": source["id"] }),
        None,
    )
    .await
    .unwrap()
}

async fn mcp(s: &Arc<Service>, endpoint: &str, token: &str, args: Value) -> (StatusCode, Value) {
    let app = common::relay_fixture::router(s.clone()).await.unwrap();
    let request = common::request("POST", endpoint)
        .header("authorization", format!("Bearer {token}"))
        .body(Body::from(
            json!({
                "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                "params": common::mcp_arguments("merge_agents", &args),
            })
            .to_string(),
        ))
        .unwrap();
    let response = common::send(&app, request).await;
    (response.status(), common::read_json(response).await)
}

#[tokio::test]
async fn merge_requires_the_destinations_own_administrator_node_review() {
    let (_root, s, source) = fixture().await;
    let task = task(&s, &source).await;
    let run = s
        .enqueue(task["id"].as_str().unwrap(), "manual", None)
        .await
        .unwrap();
    let run_id = run["id"].as_str().unwrap();
    let node_id = id();
    s.store
        .put(
            "nodes",
            json!({
                "id": cairn_installation::nodes::LOCAL_NODE_ID, "name": "VM host",
                "capabilities": { "backend": "firecracker" },
            }),
        )
        .await
        .unwrap();
    s.store
        .put(
            "nodes",
            json!({
                "id": node_id, "name": "Administrative host", "nativeEnabled": true,
                "capabilities": { "backend": "native", "nativeModeProtocol": 1 },
                "nativeModeRequest": { "mode": "administrator", "generation": id() },
                "nativeReviewedAgents": [],
            }),
        )
        .await
        .unwrap();
    let source = cairn_installation::native::policy::CALLER.scope(Some("fixture-owner".into()), s.agent(json!({
        "name": source["name"], "access": { "nodes": [cairn_installation::nodes::LOCAL_NODE_ID, node_id] },
        "nativeNodeReviews": [node_id],
    }), source["id"].as_str())).await.unwrap();
    s.agent(
        json!({ "name": "Omni", "access": { "nodes": null } }),
        Some(MAIN_AGENT_ID),
    )
    .await
    .unwrap();
    s.store
        .patch_run(
            run_id,
            json!({ "status": "failed", "executionBackend": "native" }),
        )
        .await
        .unwrap();

    let error = s.merge_agents(input(&source)).await.unwrap_err();
    assert_eq!(error.status, 409);
    assert!(error.message.contains("administrative"), "{error}");
    assert_eq!(
        s.store.run(run_id).await.unwrap()["snapshot"]["agent"]["id"],
        source["id"]
    );
    assert_eq!(
        s.get("agents", source["id"].as_str().unwrap())
            .await
            .unwrap(),
        source
    );
    let node = s.get("nodes", &node_id).await.unwrap();
    assert_eq!(node["nativeReviewedAgents"], json!([source["id"]]));

    cairn_installation::native::policy::CALLER
        .scope(
            Some("fixture-owner".into()),
            s.agent(
                json!({ "name": "Omni", "nativeNodeReviews": [node_id] }),
                Some(MAIN_AGENT_ID),
            ),
        )
        .await
        .unwrap();
    let result = s.merge_agents(input(&source)).await.unwrap();
    assert_eq!(result["transferred"]["runs"], 1);
    let transferred = s.store.run(run_id).await.unwrap();
    assert_eq!(transferred["snapshot"]["agent"]["id"], MAIN_AGENT_ID);
    assert_eq!(transferred["executionBackend"], "native");
    let node = s.get("nodes", &node_id).await.unwrap();
    assert!(
        node["nativeReviewedAgents"]
            .as_array()
            .unwrap()
            .contains(&json!(MAIN_AGENT_ID))
    );
}

#[tokio::test]
async fn merge_transfers_archived_tasks_trash_chats_and_resumable_run_identities() {
    let (root, s, source) = fixture().await;
    let target = s.get("agents", MAIN_AGENT_ID).await.unwrap();
    let task = task(&s, &source).await;
    let chat = s
        .chat_create(json!({ "agentId": source["id"] }))
        .await
        .unwrap();
    let run = s
        .enqueue(task["id"].as_str().unwrap(), "manual", None)
        .await
        .unwrap();
    let mut original_snapshot = run["snapshot"].clone();
    s.store
        .patch_run(
            run["id"].as_str().unwrap(),
            json!({
                "status": "failed", "chatExecution": { "messageId": id() },
            }),
        )
        .await
        .unwrap();
    s.store.transaction({
        let mut task = task.clone();
        let mut chat = chat.clone();
        let source = source.clone();
        let run = run.clone();
        move |db| {
            task["archived"] = true.into();
            task["enabled"] = false.into();
            db.put("tasks", &task)?;
            chat["deletedAt"] = 1.into();
            db.put("chats", &chat)?;
            db.put_message(&json!({ "id": id(), "chatId": chat["id"], "createdAt": 1, "text": "Keep this message" }))?;
            db.event(run["id"].as_str().unwrap(), "status", "Keep this history", None)?;
            db.set(&format!("agent-avatar:{}", source["id"].as_str().unwrap()), &json!({ "fixture": true }), None)
        }
    }).await.unwrap();

    let result = s.merge_agents(input(&source)).await.unwrap();
    assert_eq!(
        result["transferred"],
        json!({ "tasks": 1, "chats": 1, "runs": 1 })
    );
    assert!(
        s.store
            .get("agents", source["id"].as_str().unwrap())
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(s.get("agents", MAIN_AGENT_ID).await.unwrap(), target);
    let transferred_task = s.get("tasks", task["id"].as_str().unwrap()).await.unwrap();
    assert_eq!(transferred_task["agentId"], MAIN_AGENT_ID);
    assert_eq!(transferred_task["archived"], true);
    let transferred_chat = s.get("chats", chat["id"].as_str().unwrap()).await.unwrap();
    assert_eq!(transferred_chat["agentId"], MAIN_AGENT_ID);
    assert_eq!(transferred_chat["deletedAt"], 1);
    let transferred = s.store.run(run["id"].as_str().unwrap()).await.unwrap();
    original_snapshot["agent"]["id"] = MAIN_AGENT_ID.into();
    original_snapshot["task"]["agentId"] = MAIN_AGENT_ID.into();
    assert_eq!(transferred["snapshot"], original_snapshot);
    assert_eq!(transferred["snapshot"]["agent"]["provider"], "claude");
    assert!(
        s.store
            .kv(&format!("agent-avatar:{}", source["id"].as_str().unwrap()))
            .await
            .unwrap()
            .is_none()
    );
    s.store
        .read({
            let chat = chat.clone();
            let run = run.clone();
            move |db| {
                assert_eq!(db.messages(chat["id"].as_str().unwrap())?.len(), 1);
                assert_eq!(
                    db.events(run["id"].as_str().unwrap(), 0, 100)?
                        .last()
                        .unwrap()["text"],
                    "Keep this history"
                );
                let audit =
                    db.json_rows("SELECT detail FROM audit WHERE action='agent.merged'", [])?;
                assert_eq!(audit.len(), 1);
                Ok(())
            }
        })
        .await
        .unwrap();
    let resumed = s
        .worker
        .resume(&s, run["id"].as_str().unwrap())
        .await
        .unwrap();
    assert_eq!(resumed["snapshot"]["agent"]["id"], MAIN_AGENT_ID);
    assert_eq!(resumed["snapshot"]["agent"]["provider"], "claude");
    let reopened = Service::new(common::config(root.path())).await.unwrap();
    assert_eq!(
        reopened
            .get("tasks", task["id"].as_str().unwrap())
            .await
            .unwrap()["agentId"],
        MAIN_AGENT_ID
    );
    assert_eq!(
        reopened
            .store
            .run(run["id"].as_str().unwrap())
            .await
            .unwrap()["snapshot"],
        original_snapshot
    );
}

#[tokio::test]
async fn legacy_restricted_sessions_keep_their_mcp_denial_after_merge_and_resume() {
    let (_root, s, source) = fixture().await;
    let task = task(&s, &source).await;
    let mut run = s.snapshot(task, "manual").await.unwrap();
    run["status"] = "failed".into();
    run["chatExecution"] = json!({ "messageId": id() });
    run["snapshot"]["agent"]["access"] = json!({
        "projects": [], "skills": [], "sandbox": "read-only",
    });
    let legacy = s
        .store
        .transaction(move |db| db.add_run(&run, None))
        .await
        .unwrap();
    let original_policy = cairn_installation::service::policy(&legacy["snapshot"]["agent"]);
    assert_eq!(original_policy["mcps"], json!([]));
    s.merge_agents(input(&source)).await.unwrap();
    let resumed = s
        .worker
        .resume(&s, legacy["id"].as_str().unwrap())
        .await
        .unwrap();
    assert_eq!(resumed["snapshot"]["agent"]["id"], MAIN_AGENT_ID);
    assert_eq!(
        cairn_installation::service::policy(&resumed["snapshot"]["agent"]),
        original_policy
    );
    assert_eq!(resumed["snapshot"]["agent"]["access"]["mcps"], json!([]));
}

#[tokio::test]
async fn same_engine_merge_keeps_the_conversation_model_on_implicit_follow_up() {
    let (_root, s, original) = fixture().await;
    let source = s.agent(json!({ "name": "Claude", "provider": "claude", "model": "opus", "reasoning": "medium" }), original["id"].as_str()).await.unwrap();
    s.agent(
        json!({ "name": "Omni", "provider": "claude", "model": "haiku", "reasoning": "low" }),
        Some(MAIN_AGENT_ID),
    )
    .await
    .unwrap();
    let task = task(&s, &source).await;
    let chat = s
        .chat_create(json!({ "agentId": source["id"] }))
        .await
        .unwrap();
    let mut run = s.snapshot(task, "chat").await.unwrap();
    run["taskId"] = chat["id"].clone();
    run["snapshot"]["task"]["id"] = chat["id"].clone();
    run["status"] = "succeeded".into();
    s.store
        .transaction({
            let mut chat = chat.clone();
            let run = run.clone();
            move |db| {
                db.add_run(&run, None)?;
                db.set(
                    &format!("run-checkpoint:{}", run["id"].as_str().unwrap()),
                    &json!({}),
                    None,
                )?;
                chat["runId"] = run["id"].clone();
                db.put("chats", &chat)?;
                Ok(())
            }
        })
        .await
        .unwrap();
    s.chat_send(
        chat["id"].as_str().unwrap(),
        json!({ "id": id(), "text": "Continue" }),
    )
    .await
    .unwrap();
    s.merge_agents(input(&source)).await.unwrap();
    s.chat_tick(&std::collections::HashSet::new())
        .await
        .unwrap();
    let continued = s.store.run(run["id"].as_str().unwrap()).await.unwrap();
    assert_eq!(continued["status"], "queued");
    assert_eq!(continued["snapshot"]["agent"]["id"], MAIN_AGENT_ID);
    assert_eq!(continued["snapshot"]["agent"]["model"], "opus");
    assert_eq!(continued["snapshot"]["agent"]["reasoning"], "medium");
    assert_eq!(
        s.chat_detail(chat["id"].as_str().unwrap()).await.unwrap()["paused"],
        false
    );
}

#[tokio::test]
async fn merge_waits_for_scheduled_admission_instead_of_dropping_a_due_occurrence() {
    use std::{future::Future, task::Poll};

    let (_root, s, source) = fixture().await;
    let mut task = task(&s, &source).await;
    task["cron"] = "* * * * *".into();
    task["nextRun"] = 1.into();
    s.store.put("tasks", task.clone()).await.unwrap();
    let authors = s.task_author_lock.lock().await;
    let scheduled = s.schedule();
    tokio::pin!(scheduled);
    std::future::poll_fn(|cx| {
        assert!(scheduled.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    let merging = s.merge_agents(input(&source));
    tokio::pin!(merging);
    std::future::poll_fn(|cx| {
        assert!(merging.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    drop(authors);
    scheduled.await.unwrap();
    assert_eq!(merging.await.unwrap_err().status, 409);
    let runs = s.store.read(|db| db.active()).await.unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0]["snapshot"]["agent"]["id"], source["id"]);
    assert_eq!(runs[0]["taskId"], task["id"]);
    assert!(
        s.get("tasks", task["id"].as_str().unwrap()).await.unwrap()["nextRun"]
            .as_i64()
            .unwrap()
            > 1
    );
}

#[tokio::test]
async fn merge_rejects_busy_recovering_and_unsettled_workers_without_mutation() {
    for state in ["queued", "running", "recovering", "settling"] {
        let (_root, s, source) = fixture().await;
        let task = task(&s, &source).await;
        let run = s
            .enqueue(task["id"].as_str().unwrap(), "manual", None)
            .await
            .unwrap();
        s.store.patch_run(run["id"].as_str().unwrap(), json!({
            "status": if state == "queued" || state == "running" { state } else { "succeeded" },
            "recoveryPending": state == "recovering",
        })).await.unwrap();
        if state == "settling" {
            s.worker.active.lock().await.insert(
                run["id"].as_str().unwrap().to_owned(),
                tokio_util::sync::CancellationToken::new(),
            );
        }
        let error = s.merge_agents(input(&source)).await.unwrap_err();
        assert_eq!(error.status, 409, "{state}");
        assert!(error.message.contains("active work"), "{state}: {error}");
        assert_eq!(
            s.get("tasks", task["id"].as_str().unwrap()).await.unwrap()["agentId"],
            source["id"]
        );
        assert_eq!(
            s.get("agents", source["id"].as_str().unwrap())
                .await
                .unwrap(),
            source
        );
    }
}

#[tokio::test]
async fn merge_rejects_invalid_targets_permissions_credentials_and_delegations() {
    let (_root, s, source) = fixture().await;
    for args in [
        json!({ "sourceId": source["id"], "targetId": MAIN_AGENT_ID }),
        json!({ "sourceId": source["id"], "targetId": MAIN_AGENT_ID, "confirm": false }),
        json!({ "sourceId": "invalid", "targetId": MAIN_AGENT_ID, "confirm": true }),
        json!({ "sourceId": source["id"], "targetId": source["id"], "confirm": true }),
        json!({ "sourceId": MAIN_AGENT_ID, "targetId": source["id"], "confirm": true }),
        json!({ "sourceId": source["id"], "targetId": id(), "confirm": true }),
    ] {
        assert!(s.merge_agents(args).await.is_err());
    }
    for change in [
        json!({ "access": { "installation": true } }),
        json!({ "access": { "nodes": null } }),
        json!({ "plugins": [id()] }),
        json!({ "exclusiveNodes": [id()] }),
    ] {
        let mut changed = source.clone();
        cairn_installation::store::merge(&mut changed, &change);
        s.store.put("agents", changed).await.unwrap();
        assert_eq!(
            s.merge_agents(input(&source)).await.unwrap_err().status,
            409
        );
    }
    s.store.put("agents", source.clone()).await.unwrap();
    s.store
        .put(
            "plugin-connections",
            json!({ "id": id(), "agentId": source["id"], "enabled": false }),
        )
        .await
        .unwrap();
    assert!(
        s.merge_agents(input(&source))
            .await
            .unwrap_err()
            .message
            .contains("delegations")
    );
    assert!(
        s.store
            .get("agents", source["id"].as_str().unwrap())
            .await
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn merge_keeps_preapproved_live_grants_and_rolls_back_a_failed_delete() {
    let (_root, s, source) = fixture().await;
    let task = task(&s, &source).await;
    let instance = id();
    s.store.transaction({
        let mut source = source.clone();
        let instance = instance.clone();
        move |db| {
            let mut target = db.get("agents", MAIN_AGENT_ID)?.unwrap();
            source["plugins"] = json!([instance]);
            target["plugins"] = json!([instance]);
            db.put("agents", &source)?;
            db.put("agents", &target)?;
            db.put("plugin-connections", &json!({ "id": instance, "agentIds": [source["id"], MAIN_AGENT_ID] }))?;
            db.0.execute_batch("CREATE TRIGGER reject_merge BEFORE DELETE ON records WHEN old.kind='agents' BEGIN SELECT RAISE(ABORT, 'fixture failure'); END;")?;
            Ok(())
        }
    }).await.unwrap();
    assert!(s.merge_agents(input(&source)).await.is_err());
    assert_eq!(
        s.get("tasks", task["id"].as_str().unwrap()).await.unwrap()["agentId"],
        source["id"]
    );
    assert_eq!(
        s.get("plugin-connections", &instance).await.unwrap()["agentIds"],
        json!([source["id"], MAIN_AGENT_ID])
    );
    s.store
        .write(|db| {
            db.0.execute_batch("DROP TRIGGER reject_merge")?;
            Ok(())
        })
        .await
        .unwrap();
    s.merge_agents(input(&source)).await.unwrap();
    assert_eq!(
        s.get("agents", MAIN_AGENT_ID).await.unwrap()["plugins"],
        json!([instance])
    );
    assert_eq!(
        s.get("plugin-connections", &instance).await.unwrap()["agentIds"],
        json!([MAIN_AGENT_ID])
    );
}

#[tokio::test]
async fn merge_http_is_owner_only_and_both_mcp_interfaces_have_the_same_effect() {
    for transport in ["http", "public", "installation"] {
        let (_root, s, source) = fixture().await;
        let existing = task(&s, &source).await;
        let args = input(&source);
        let response = if transport == "http" {
            let app = cairn_installation::http::router(s.clone()).await.unwrap();
            for role in [InstallationRole::Member, InstallationRole::Owner] {
                let request = common::request(
                    "POST",
                    &format!("/api/agents/{}/merge", source["id"].as_str().unwrap()),
                )
                .extension(InstallationIdentity::trusted(role, "fixture-account"))
                .body(Body::from(args.to_string()))
                .unwrap();
                let response = common::send(&app, request).await;
                let expected = if role == InstallationRole::Member {
                    StatusCode::FORBIDDEN
                } else {
                    StatusCode::OK
                };
                assert_eq!(response.status(), expected);
                if role == InstallationRole::Owner {
                    assert_eq!(common::read_json(response).await["deleted"], true);
                }
            }
            json!({ "deleted": true })
        } else {
            let (endpoint, token) = if transport == "public" {
                let read = common::relay_fixture::grant(&s, &["read"]).await;
                let denied = mcp(&s, "/mcp", &read, args.clone()).await;
                assert_ne!(
                    denied.1["result"]["structuredContent"]["result"]["deleted"],
                    true
                );
                assert!(
                    s.store
                        .get("agents", source["id"].as_str().unwrap())
                        .await
                        .unwrap()
                        .is_some()
                );
                ("/mcp", common::relay_fixture::grant(&s, &["manage"]).await)
            } else {
                let main = s
                    .agent(
                        json!({ "name": "Omni", "access": { "installation": true } }),
                        Some(MAIN_AGENT_ID),
                    )
                    .await
                    .unwrap();
                let caller_task = task(&s, &main).await;
                let caller = s
                    .enqueue(caller_task["id"].as_str().unwrap(), "manual", None)
                    .await
                    .unwrap();
                let caller = s
                    .store
                    .patch_run(
                        caller["id"].as_str().unwrap(),
                        json!({ "status": RunStatus::Running }),
                    )
                    .await
                    .unwrap();
                let configuration = s.mcps.run_configuration(&s, &caller).await.unwrap();
                (
                    "/mcp-installation",
                    configuration["env"]["CAIRN_MCP_RUN_TOKEN"]
                        .as_str()
                        .unwrap()
                        .to_owned(),
                )
            };
            let (status, response) = mcp(&s, endpoint, &token, args).await;
            assert_eq!(status, StatusCode::OK, "{response}");
            assert_ne!(response["result"]["isError"], true, "{response}");
            response["result"]["structuredContent"]["result"].clone()
        };
        assert_eq!(response["deleted"], true, "{transport}: {response}");
        assert_eq!(
            s.get("tasks", existing["id"].as_str().unwrap())
                .await
                .unwrap()["agentId"],
            MAIN_AGENT_ID
        );
        assert!(
            s.store
                .get("agents", source["id"].as_str().unwrap())
                .await
                .unwrap()
                .is_none()
        );
    }
}
