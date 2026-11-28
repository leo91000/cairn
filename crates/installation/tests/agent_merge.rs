use cairn_contracts::EventKind;
#[allow(unused_imports)]
use common::{PluginServiceFixture as _, PluginStoreFixture as _};

use crate::common::run_fixture::DbRunFixture;
use common::AgentFixture;
use common::ChatRunFixture;
use common::StoreAgentFixture;

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
        .agent_fixture(json!({ "name": "Claude", "provider": "claude" }), None)
        .await
        .unwrap();
    (root, s, source)
}

fn input(source: &Value) -> Value {
    json!({ "sourceId": source["id"], "targetId": MAIN_AGENT_ID, "confirm": true })
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
async fn merge_transfers_trash_chats_and_resumable_run_identities() {
    let (root, s, source) = fixture().await;
    let target = s.agent_snapshot(MAIN_AGENT_ID).await.unwrap();
    let (chat_detail, run) = s
        .chat_run_fixture(source["id"].as_str().unwrap(), None, "Inspect")
        .await
        .unwrap();
    let chat = s
        .conversation_boundary(chat_detail["id"].as_str().unwrap())
        .await
        .unwrap();
    let mut original_snapshot = run["snapshot"].clone();
    s.store
        .change_run(
            run["id"].as_str().unwrap(),
            cairn_installation::runs::RunChanges {
                status: Some(cairn_contracts::RunStatus::Failed),
                boundary: Some(serde_json::json!({"chatExecution": { "messageId": id() }})),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    s.store.transaction({
        let mut chat = chat.clone();
        let source = source.clone();
        let run = run.clone();
        move |db| {
            chat["lifecycle"] = "trash".into();
            chat["trashedAt"] = 1.into();
            chat["purgeAt"] = (cairn_installation::config::now() + 60_000).into();
            db.save_conversation_boundary(&chat)?;
            db.put_message(&json!({ "id": id(), "chatId": chat["id"], "createdAt": 1, "text": "Keep this message" }))?;
            db.event(run["id"].as_str().unwrap(), &EventKind::Status, "Keep this history", None)?;
            db.set(&format!("agent-avatar:{}", source["id"].as_str().unwrap()), &json!({ "fixture": true }), None)
        }
    }).await.unwrap();

    let result = s.merge_agents_fixture(input(&source)).await.unwrap();
    assert_eq!(result["transferred"], json!({ "chats": 1, "runs": 1 }));
    assert!(
        s.store
            .agent_snapshot(source["id"].as_str().unwrap())
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(s.agent_snapshot(MAIN_AGENT_ID).await.unwrap(), target);
    let transferred_chat = s
        .conversation_boundary(chat["id"].as_str().unwrap())
        .await
        .unwrap();
    assert_eq!(transferred_chat["agentId"], MAIN_AGENT_ID);
    assert_eq!(transferred_chat["lifecycle"], "trash");
    assert_eq!(transferred_chat["trashedAt"], 1);
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
                let messages = db.messages(chat["id"].as_str().unwrap())?;
                assert!(messages.iter().any(|m| m["text"] == "Keep this message"));
                assert_eq!(
                    db.events_fixture(run["id"].as_str().unwrap(), 0, 100)?
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
    let run_id = run["id"].as_str().unwrap();
    assert_eq!(
        s.worker.resume(&s, run_id).await.unwrap_err().status,
        StatusCode::CONFLICT
    );
    s.chat_restore(chat["id"].as_str().unwrap()).await.unwrap();
    let resumed = s.worker.resume(&s, run_id).await.unwrap();
    assert_eq!(resumed["snapshot"]["agent"]["id"], MAIN_AGENT_ID);
    assert_eq!(resumed["snapshot"]["agent"]["provider"], "claude");
    let reopened = Service::new(common::config(root.path())).await.unwrap();
    assert_eq!(
        reopened
            .conversation_boundary(chat["id"].as_str().unwrap())
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
async fn same_engine_merge_keeps_the_conversation_model_on_implicit_follow_up() {
    let (_root, s, original) = fixture().await;
    let source = s.agent_fixture(json!({ "name": "Claude", "provider": "claude", "model": "opus", "reasoning": "medium" }), original["id"].as_str()).await.unwrap();
    s.agent_fixture(
        json!({ "name": "Omni", "provider": "claude", "model": "haiku", "reasoning": "low" }),
        Some(MAIN_AGENT_ID),
    )
    .await
    .unwrap();
    let chat = s
        .chat_create(json!({ "agentId": source["id"] }))
        .await
        .unwrap();
    let task = json!({
        "id": chat["id"],
        "name": "Existing task",
        "prompt": "Inspect",
        "agentId": source["id"],
    });
    let mut run = s.snapshot(task, "chat").await.unwrap();
    run["taskId"] = chat["id"].clone();
    run["snapshot"]["task"]["id"] = chat["id"].clone();
    run["status"] = "succeeded".into();
    s.store
        .transaction({
            let mut chat = chat.clone();
            let run = run.clone();
            move |db| {
                db.add_run_fixture(&run, None)?;
                db.save_checkpoint_boundary(run["id"].as_str().unwrap(), &json!({}))?;
                chat["runId"] = run["id"].clone();
                db.save_conversation_boundary(&chat)?;
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
    s.merge_agents_fixture(input(&source)).await.unwrap();
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
async fn merge_rejects_busy_recovering_and_unsettled_workers_without_mutation() {
    for state in ["queued", "running", "recovering", "settling"] {
        let (_root, s, source) = fixture().await;
        let (chat, run) = s
            .chat_run_fixture(source["id"].as_str().unwrap(), None, "Inspect")
            .await
            .unwrap();
        s.store
            .change_run(
                run["id"].as_str().unwrap(),
                cairn_installation::runs::RunChanges {
                    status: Some(
                        cairn_contracts::RunStatus::parse(
                            if state == "queued" || state == "running" {
                                state
                            } else {
                                "succeeded"
                            },
                        )
                        .unwrap(),
                    ),
                    recovery_pending: Some(state == "recovering"),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        if state == "settling" {
            s.worker.active.lock().await.insert(
                run["id"].as_str().unwrap().to_owned(),
                tokio_util::sync::CancellationToken::new(),
            );
        }
        let error = s.merge_agents_fixture(input(&source)).await.unwrap_err();
        assert_eq!(error.status, 409, "{state}");
        assert!(error.message.contains("active work"), "{state}: {error}");
        assert_eq!(
            s.conversation_boundary(chat["id"].as_str().unwrap())
                .await
                .unwrap()["agentId"],
            source["id"]
        );
        assert_eq!(
            s.agent_snapshot(source["id"].as_str().unwrap())
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
        assert!(s.merge_agents_fixture(args).await.is_err());
    }
    let granted_instance = id();
    s.store
        .put_instance_fixture(json!({ "id": granted_instance, "pluginId": "fixture" }))
        .await
        .unwrap();
    for change in [
        json!({ "access": { "installation": true } }),
        json!({ "access": { "nodes": null } }),
        json!({ "access": { "backend": "native" } }),
        json!({ "plugins": [granted_instance] }),
        json!({ "exclusiveNodes": [id()] }),
    ] {
        let mut changed = source.clone();
        cairn_installation::store::merge(&mut changed, &change);
        s.store.put_agent_fixture(changed).await.unwrap();
        assert_eq!(
            s.merge_agents_fixture(input(&source))
                .await
                .unwrap_err()
                .status,
            409
        );
    }
    s.store.put_agent_fixture(source.clone()).await.unwrap();
    s.store
        .put_instance_fixture(json!({ "id": id(), "agentId": source["id"], "enabled": false }))
        .await
        .unwrap();
    assert!(
        s.merge_agents_fixture(input(&source))
            .await
            .unwrap_err()
            .message
            .contains("delegations")
    );
    assert!(
        s.store
            .agent_snapshot(source["id"].as_str().unwrap())
            .await
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn merge_keeps_preapproved_live_grants_and_rolls_back_a_failed_delete() {
    let (_root, s, source) = fixture().await;
    let chat = s
        .chat_create(json!({ "agentId": source["id"] }))
        .await
        .unwrap();
    let instance = id();
    s.store.transaction({
        let mut source = source.clone();
        let instance = instance.clone();
        move |db| {
            let mut target = db.agent_snapshot(MAIN_AGENT_ID)?.unwrap();
            source["plugins"] = json!([instance]);
            target["plugins"] = json!([instance]);
            let mut account = cairn_installation::plugins::connections::Connection::draft(&instance,"fixture",json!({}),Vec::new());
            account.name = "Fixture".into();
            account.revision = "r1".into();
            { db.save_plugin_version(&account.plugin_id,"fixture",&"0".repeat(64))?; db.save_plugin_instance(&account) }?;
            db.save_agent(&serde_json::from_value(source.clone())?)?;
            db.save_agent(&serde_json::from_value(target.clone())?)?;
            db.0.execute_batch("CREATE TRIGGER reject_merge BEFORE DELETE ON agents BEGIN SELECT RAISE(ABORT, 'fixture failure'); END;")?;
            Ok(())
        }
    }).await.unwrap();
    assert!(s.merge_agents_fixture(input(&source)).await.is_err());
    assert_eq!(
        s.conversation_boundary(chat["id"].as_str().unwrap())
            .await
            .unwrap()["agentId"],
        source["id"]
    );
    assert_eq!(
        s.instance_fixture(&instance).await.unwrap()["agentIds"],
        json!([MAIN_AGENT_ID, source["id"]])
    );
    s.store
        .write(|db| {
            db.0.execute_batch("DROP TRIGGER reject_merge")?;
            Ok(())
        })
        .await
        .unwrap();
    s.merge_agents_fixture(input(&source)).await.unwrap();
    assert_eq!(
        s.agent_snapshot(MAIN_AGENT_ID).await.unwrap()["plugins"],
        json!([instance])
    );
    assert_eq!(
        s.instance_fixture(&instance).await.unwrap()["agentIds"],
        json!([MAIN_AGENT_ID])
    );
}

#[tokio::test]
async fn merge_http_is_owner_only_and_both_mcp_interfaces_have_the_same_effect() {
    for transport in ["http", "public", "installation"] {
        let (_root, s, source) = fixture().await;
        let existing = s
            .chat_create(json!({ "agentId": source["id"] }))
            .await
            .unwrap();
        let args = input(&source);
        let response = if transport == "http" {
            let app = cairn_installation::http::router(s.clone()).await.unwrap();
            for role in [InstallationRole::Member, InstallationRole::Owner] {
                let request = common::request(
                    "POST",
                    &format!("/api/agents/{}/merge", source["id"].as_str().unwrap()),
                )
                .extension(InstallationIdentity::trusted(role, "fixture-owner"))
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
                        .agent_snapshot(source["id"].as_str().unwrap())
                        .await
                        .unwrap()
                        .is_some()
                );
                ("/mcp", common::relay_fixture::grant(&s, &["manage"]).await)
            } else {
                let main = s
                    .agent_fixture(
                        json!({ "name": "Omni", "access": { "installation": true } }),
                        Some(MAIN_AGENT_ID),
                    )
                    .await
                    .unwrap();
                let (_caller_chat, caller) = s
                    .chat_run_fixture(main["id"].as_str().unwrap(), None, "Inspect")
                    .await
                    .unwrap();
                let caller = s
                    .store
                    .change_run(
                        caller["id"].as_str().unwrap(),
                        cairn_installation::runs::RunChanges {
                            status: Some(RunStatus::Running),
                            ..Default::default()
                        },
                    )
                    .await
                    .unwrap();
                let caller = s.store.run(&caller.id).await.unwrap();
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
            s.conversation_boundary(existing["id"].as_str().unwrap())
                .await
                .unwrap()["agentId"],
            MAIN_AGENT_ID
        );
        assert!(
            s.store
                .agent_snapshot(source["id"].as_str().unwrap())
                .await
                .unwrap()
                .is_none()
        );
    }
}
