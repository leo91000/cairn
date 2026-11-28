use super::*;

#[tokio::test]
async fn plugin_setup_is_available_on_both_endpoints_without_granting_private_accounts() {
    let root = TempDir::new().unwrap();
    let s = service(&root).await;
    let main = enabled_main(&s).await;
    let (_, configuration) = active_run(&s, &main).await;
    let saved = common::save_plugin(
        &s,
        "onepassword",
        &json!({
            "name": "Private", "enabled": true, "secrets": { "token": "ops_fixture_private" },
        }),
        None,
    )
    .await
    .unwrap();
    for internal in [false, true] {
        let list = if internal {
            call(&s, &configuration, "list_plugins", json!({})).await
        } else {
            public_call(&s, &["read"], "list_plugins", json!({})).await
        };
        assert!(
            result(&list)["packages"]
                .as_array()
                .unwrap()
                .iter()
                .any(|v| v["id"] == "gmail")
        );
        assert!(!list.to_string().contains("ops_fixture_private"));
        let args = json!({ "pluginId": "gmail", "kind": "setup" });
        let setup = if internal {
            call(&s, &configuration, "manage_plugin", args).await
        } else {
            public_call(&s, &["manage"], "manage_plugin", args).await
        };
        assert!(
            result(&setup)["callbackUrl"]
                .as_str()
                .unwrap()
                .ends_with("/plugins/oauth/callback")
        );
        let forbidden = json!({
            "pluginId": "onepassword",
            "kind": "save",
            "instanceId": saved["id"],
            "data": { "agentIds": [MAIN_AGENT_ID] },
        });
        let denied = if internal {
            call(&s, &configuration, "manage_plugin", forbidden).await
        } else {
            public_call(&s, &["manage"], "manage_plugin", forbidden).await
        };
        assert_eq!(denied["result"]["isError"], true, "{denied}");
    }
    let denied = call(
        &s,
        &configuration,
        "manage_plugin",
        json!({ "pluginId": "onepassword", "kind": "view", "instanceId": saved["id"] }),
    )
    .await;
    assert_eq!(denied["result"]["isError"], true, "{denied}");
    let bypass = call(
        &s,
        &configuration,
        "manage_plugin",
        json!({ "pluginId": "onepassword", "kind": "form", "data": { "instance": saved["id"] } }),
    )
    .await;
    assert_eq!(bypass["result"]["isError"], true, "{bypass}");
    assert_eq!(
        s.instance_fixture(saved["id"].as_str().unwrap())
            .await
            .unwrap()["agentIds"],
        json!([])
    );
}

#[tokio::test]
async fn installation_agents_can_turn_owner_only_switches_off_but_never_on() {
    let root = TempDir::new().unwrap();
    let s = service(&root).await;
    let main = enabled_main(&s).await;
    let (_, configuration) = active_run(&s, &main).await;
    let saved = common::save_plugin(
        &s,
        "onepassword",
        &json!({
            "name": "Shared", "enabled": true, "secrets": { "token": "ops_fixture" },
            "agentIds": [MAIN_AGENT_ID],
        }),
        None,
    )
    .await
    .unwrap();
    let save = |allow_write: bool, revision: &Value| {
        json!({
            "pluginId": "onepassword", "kind": "save", "instanceId": saved["id"],
            "data": {
                "name": "Shared", "enabled": true, "revision": revision,
                "settings": { "allowWrite": allow_write },
            },
        })
    };

    // Writing widens what granted agents may do: only the owner turns it on.
    let widened = call(
        &s,
        &configuration,
        "manage_plugin",
        save(true, &saved["revision"]),
    )
    .await;
    assert_eq!(widened["result"]["isError"], true, "{widened}");
    assert!(widened.to_string().contains("Only the owner"), "{widened}");
    let created = call(
        &s,
        &configuration,
        "manage_plugin",
        json!({
            "pluginId": "onepassword", "kind": "save",
            "data": {
                "name": "Another", "enabled": true, "secrets": { "token": "ops_fixture" },
                "settings": { "allowWrite": true },
            },
        }),
    )
    .await;
    assert_eq!(created["result"]["isError"], true, "{created}");
    assert!(created.to_string().contains("Only the owner"), "{created}");
    let current = s
        .instance_fixture(saved["id"].as_str().unwrap())
        .await
        .unwrap();
    assert_eq!(current["settings"]["allowWrite"], false);

    let allowed = public_call(
        &s,
        &["manage"],
        "manage_plugin",
        save(true, &current["revision"]),
    )
    .await;
    let allowed = &result(&allowed)["instance"];
    assert_eq!(allowed["settings"]["allowWrite"], true, "{allowed}");
    // Nor may it point the owner's approval at another service account.
    let mut swapped = save(true, &allowed["revision"]);
    swapped["data"]["secrets"] = json!({ "token": "ops_fixture_wider" });
    let swapped = call(&s, &configuration, "manage_plugin", swapped).await;
    assert_eq!(swapped["result"]["isError"], true, "{swapped}");
    assert!(
        swapped.to_string().contains("change the credential"),
        "{swapped}"
    );
    let unchanged = call(
        &s,
        &configuration,
        "manage_plugin",
        save(true, &allowed["revision"]),
    )
    .await;
    let unchanged = &result(&unchanged)["instance"];
    assert_eq!(unchanged["settings"]["allowWrite"], true, "{unchanged}");

    let reduced = call(
        &s,
        &configuration,
        "manage_plugin",
        save(false, &unchanged["revision"]),
    )
    .await;
    assert_eq!(
        result(&reduced)["instance"]["settings"]["allowWrite"],
        false,
        "{reduced}"
    );
}

#[tokio::test]
async fn both_mcp_endpoints_can_pause_and_delete_instances_pending_package_reapproval() {
    use cairn_installation::{
        plugins::{connections, registry},
        validation::text,
    };
    let root = TempDir::new().unwrap();
    let s = service(&root).await;
    s.plugins
        .use_worker(std::path::PathBuf::from(env!("CARGO_BIN_EXE_cairn")));
    let main = enabled_main(&s).await;
    let (_, configuration) = active_run(&s, &main).await;
    let installed = common::package_install(
        &s,
        &json!({
            "manifest": {
                "id": "pausable", "name": "Pausable", "version": "1.0.0",
                "apiVersion": 2, "capabilities": [],
            },
            "source": "return {}",
        }),
    )
    .await
    .unwrap();
    common::package_approve(&s, "pausable", "1.0.0", text(&installed, "digest"))
        .await
        .unwrap();
    let saved = common::save_plugin(
        &s,
        "pausable",
        &json!({
            "name": "Pause me", "enabled": true,
        }),
        None,
    )
    .await
    .unwrap();
    std::fs::write(
        root.path().join("data/plugins/pausable/1.0.0/main.luau"),
        "return {} -- changed after approval",
    )
    .unwrap();
    assert!(registry::active(&s, "pausable").await.is_err());
    for internal in [false, true] {
        let args = json!({
            "pluginId": "pausable", "kind": "enabled", "instanceId": saved["id"],
            "data": { "enabled": false },
        });
        let paused = if internal {
            call(&s, &configuration, "manage_plugin", args).await
        } else {
            public_call(&s, &["manage"], "manage_plugin", args).await
        };
        assert_eq!(result(&paused)["instance"]["enabled"], false, "{paused}");
    }
    let removed = call(
        &s,
        &configuration,
        "manage_plugin",
        json!({
            "pluginId": "pausable", "kind": "delete", "instanceId": saved["id"],
        }),
    )
    .await;
    assert_ne!(removed["result"]["isError"], true, "{removed}");
    assert!(
        connections::get(&s, "pausable", text(&saved, "id"))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn both_mcp_endpoints_list_current_chat_account_waits() {
    let root = TempDir::new().unwrap();
    let s = service(&root).await;
    let main = enabled_main(&s).await;
    let (target, configuration) = active_run(&s, &main).await;
    let chat_id = text(&target, "taskId");
    for required in [None, Some(cairn_contracts::AgentProvider::Codex)] {
        s.store
            .change_run(
                text(&target, "id"),
                cairn_installation::runs::RunChanges {
                    account_wait_reason: Some(Some(
                        "Waiting for a Codex account with available usage.".into(),
                    )),
                    account_required: Some(required),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let detail = s.chat_detail(chat_id).await.unwrap();
        for internal in [false, true] {
            let response = if internal {
                call(&s, &configuration, "list_chats", json!({})).await
            } else {
                public_call(&s, &["read"], "list_chats", json!({})).await
            };
            let listed = &result(&response)["chats"][0];
            assert_eq!(
                listed["accountWaitReason"],
                detail["run"]["accountWaitReason"]
            );
            assert_eq!(listed["accountRequired"], detail["run"]["accountRequired"]);
            assert_eq!(listed["accountWaitReason"], detail["accountWaitReason"]);
            assert_eq!(listed["accountRequired"], detail["accountRequired"]);
            assert!(listed["run"].is_null());
            assert_eq!(listed["messages"], json!([]));
        }
    }
    s.store
        .change_run(
            text(&target, "id"),
            cairn_installation::runs::RunChanges {
                account_wait_reason: Some(None),
                account_required: Some(None),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let cleared = s.chat_detail(chat_id).await.unwrap();
    assert!(cleared["accountWaitReason"].is_null());
    assert!(cleared["accountRequired"].is_null());
}

#[tokio::test]
async fn a_renamed_conversation_keeps_its_title_through_its_first_message() {
    for internal in [false, true] {
        let root = TempDir::new().unwrap();
        let s = service(&root).await;
        let main = enabled_main(&s).await;
        let (_, configuration) = active_run(&s, &main).await;
        let invoke = |name, args| {
            let s = s.clone();
            let configuration = configuration.clone();
            async move {
                if internal {
                    call(&s, &configuration, name, args).await
                } else {
                    public_call(&s, &["read", "manage", "run"], name, args).await
                }
            }
        };
        let created = invoke("create_chat", json!({})).await;
        let chat_id = result(&created)["id"].as_str().unwrap().to_owned();
        invoke(
            "set_chat_paused",
            json!({ "chatId": chat_id, "paused": true }),
        )
        .await;
        let revision = s
            .store
            .conversation_boundary(&chat_id)
            .await
            .unwrap()
            .unwrap()["revision"]
            .clone();
        let renamed = invoke(
            "rename_chat",
            json!({ "chatId": chat_id, "title": "Release notes", "revision": revision }),
        )
        .await;
        assert_eq!(result(&renamed)["title"], "Release notes");
        let sent = invoke(
            "send_chat_message",
            json!({
                "chatId": chat_id,
                "message": {
                    "id": uuid::Uuid::new_v4(),
                    "text": "Draft the notes for 0.53",
                },
            }),
        )
        .await;
        assert_eq!(result(&sent)["status"], "queued", "{sent}");
        // The first message no longer names a conversation a person already named.
        assert_eq!(
            s.store
                .conversation_boundary(&chat_id)
                .await
                .unwrap()
                .unwrap()["title"],
            "Release notes"
        );
        assert!(
            s.store
                .kv(&format!("chat-title-manual:{chat_id}"))
                .await
                .unwrap()
                .is_some(),
            "a manual title ends automatic titles"
        );
    }
}

#[tokio::test]
async fn both_mcp_endpoints_manage_chat_queues_and_lifecycle() {
    for internal in [false, true] {
        let root = TempDir::new().unwrap();
        let s = service(&root).await;
        let main = enabled_main(&s).await;
        let (_, configuration) = active_run(&s, &main).await;
        let invoke = |name, args| {
            let s = s.clone();
            let configuration = configuration.clone();
            async move {
                if internal {
                    call(&s, &configuration, name, args).await
                } else {
                    public_call(&s, &["read", "manage", "run"], name, args).await
                }
            }
        };
        let retention = invoke("get_conversation_retention", json!({})).await;
        assert_eq!(result(&retention)["inactivityDays"], 7);
        let retention = invoke(
            "set_conversation_retention",
            json!({ "inactivityDays": 14 }),
        )
        .await;
        assert_eq!(result(&retention)["inactivityDays"], 14);
        assert_eq!(
            s.conversation_retention().await.unwrap()["inactivityDays"],
            14
        );
        let invalid = invoke(
            "set_conversation_retention",
            json!({ "inactivityDays": -1 }),
        )
        .await;
        assert_eq!(invalid["result"]["isError"], true);
        let invalid = invoke(
            "set_conversation_retention",
            json!({ "inactivityDays": 36501 }),
        )
        .await;
        assert_eq!(invalid["result"]["isError"], true);
        let denied = public_call(
            &s,
            &["read"],
            "set_conversation_retention",
            json!({ "inactivityDays": 0 }),
        )
        .await;
        assert_eq!(denied["result"]["isError"], true);
        assert_eq!(
            s.conversation_retention().await.unwrap()["inactivityDays"],
            14
        );
        let created = invoke("create_chat", json!({})).await;
        let chat_id = result(&created)["id"].as_str().unwrap();
        assert_eq!(result(&created)["agentId"], MAIN_AGENT_ID);
        s.store
            .set(
                &format!("chat-error:{chat_id}"),
                json!("Preparation requires attention"),
                None,
            )
            .await
            .unwrap();
        let failed = invoke("get_chat", json!({ "chatId": chat_id })).await;
        assert_eq!(
            result(&failed)["chat"]["error"],
            "Preparation requires attention"
        );
        assert_eq!(
            s.chat_detail(chat_id).await.unwrap()["error"],
            "Preparation requires attention"
        );
        let pause = invoke(
            "set_chat_paused",
            json!({ "chatId": chat_id, "paused": true }),
        )
        .await;
        assert_eq!(result(&pause)["paused"], true);
        let message_id = "ABCDEFAB-CDEF-4ABC-8DEF-ABCDEFABCDE1";
        let attachment_id = "ABCDEFAB-CDEF-4ABC-8DEF-ABCDEFABCDE2";
        let uploaded = invoke(
            "upload_chat_attachment",
            json!({
                "chatId": chat_id, "attachmentId": attachment_id,
                "name": "uppercase.txt", "data": "QQ==",
            }),
        )
        .await;
        assert_eq!(result(&uploaded)["id"], attachment_id);
        // Both public MCP and the built-in tool retain the acceptance precondition;
        // neither may discard it and queue an ordinary request on a missing proposal.
        let stale = invoke(
            "send_chat_message",
            json!({
                "chatId": chat_id,
                "message": {
                    "id": uuid::Uuid::new_v4(), "text": "Implement the plan you proposed.",
                    "planApproval": {"sourceId":"missing:1", "revision":"stale"},
                },
            }),
        )
        .await;
        assert_eq!(stale["result"]["isError"], true, "{stale}");
        assert!(
            s.chat_detail(chat_id).await.unwrap()["messages"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        let args = json!({
            "chatId": chat_id,
            "message": { "id": message_id, "text": "First message", "attachmentIds": [attachment_id] },
        });
        let sent = invoke("send_chat_message", args.clone()).await;
        assert_eq!(result(&sent)["id"], message_id);
        assert_eq!(result(&sent)["attachments"][0]["id"], attachment_id);
        assert_eq!(result(&sent)["status"], "queued");
        let retry = invoke("send_chat_message", args).await;
        assert_eq!(result(&sent), result(&retry));
        let changed = invoke(
            "edit_chat_message",
            json!({
                "chatId": chat_id, "messageId": message_id, "message": { "text": "Edited" },
            }),
        )
        .await;
        assert_eq!(result(&changed)["text"], "Edited");
        let renamed = invoke(
            "rename_chat",
            json!({ "chatId": chat_id, "title": "  Gestion Cairn  ", "revision": s.store.conversation_boundary(chat_id).await.unwrap().unwrap()["revision"] }),
        )
        .await;
        assert_eq!(result(&renamed)["title"], "Gestion Cairn");
        assert_eq!(
            invoke("rename_chat", json!({ "chatId": chat_id, "title": "   " })).await["result"]["isError"],
            true
        );
        let listed = invoke("list_chats", json!({ "limit": 1 })).await;
        assert_eq!(result(&listed)["chats"][0]["id"], chat_id);
        let detail = invoke("get_chat", json!({ "chatId": chat_id })).await;
        assert_eq!(result(&detail)["messages"][0]["message"]["text"], "Edited");
        assert!(result(&detail)["chat"]["run"].is_null());
        let resumed = invoke(
            "set_chat_paused",
            json!({ "chatId": chat_id, "paused": false }),
        )
        .await;
        assert_eq!(result(&resumed)["paused"], false);
        let stopped = invoke("stop_chat", json!({ "chatId": chat_id })).await;
        assert_eq!(result(&stopped)["paused"], true);
        assert_eq!(
            invoke("delete_chat", json!({ "chatId": chat_id })).await["result"]["isError"],
            true
        );
        let removed = invoke(
            "delete_chat_message",
            json!({ "chatId": chat_id, "messageId": message_id }),
        )
        .await;
        assert_eq!(result(&removed)["deleted"], true);
        let deleted = invoke("delete_chat", json!({ "chatId": chat_id })).await;
        assert_eq!(result(&deleted)["lifecycle"], "trash");
        let listed = invoke("list_chats", json!({ "view": "trash" })).await;
        assert_eq!(result(&listed)["chats"][0]["id"], chat_id);
        let rejected = invoke(
            "send_chat_message",
            json!({
                "chatId": chat_id, "message": { "id": uuid::Uuid::new_v4(), "text": "Deleted" },
            }),
        )
        .await;
        assert_eq!(rejected["result"]["isError"], true);
    }
}

#[tokio::test]
async fn chat_tools_preserve_scopes_caller_defaults_and_revocation() {
    let root = TempDir::new().unwrap();
    let s = service(&root).await;
    let agent = s
        .agent_fixture(
            json!({
                "name": "Delegated manager", "access": { "installation": true, "projects": [] },
            }),
            None,
        )
        .await
        .unwrap();
    let (run, configuration) = active_run(&s, &agent).await;
    let created = call(&s, &configuration, "create_chat", json!({})).await;
    let chat_id = result(&created)["id"].as_str().unwrap();
    assert_eq!(result(&created)["agentId"], agent["id"]);
    for (name, args) in [
        ("create_chat", json!({})),
        (
            "rename_chat",
            json!({ "chatId": chat_id, "title": "Denied" }),
        ),
        (
            "send_chat_message",
            json!({ "chatId": chat_id, "message": { "id": uuid::Uuid::new_v4(), "text": "Denied" } }),
        ),
        ("delete_chat", json!({ "chatId": chat_id })),
    ] {
        let denied = public_call(&s, &["read"], name, args).await;
        assert_eq!(denied["result"]["isError"], true, "{denied}");
        assert!(denied["result"]["_meta"]["mcp/www_authenticate"].is_string());
    }
    let read = public_call(&s, &["read"], "get_chat", json!({ "chatId": chat_id })).await;
    assert_eq!(result(&read)["chat"]["id"], chat_id);
    s.store
        .change_run(
            run["id"].as_str().unwrap(),
            cairn_installation::runs::RunChanges {
                cancel_requested_at: Some(Some(1)),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let token = configuration["env"]["CAIRN_MCP_RUN_TOKEN"]
        .as_str()
        .unwrap();
    for name in [
        "list_chats",
        "create_chat",
        "get_chat",
        "list_chat_artifacts",
        "stop_chat",
    ] {
        assert_eq!(
            request(
                &s,
                token,
                "tools/call",
                json!({ "name": name, "arguments": {} })
            )
            .await
            .0,
            StatusCode::UNAUTHORIZED
        );
    }
}

#[tokio::test]
async fn chat_file_tools_read_private_artifacts_and_attachments_in_bounded_chunks() {
    use base64::{Engine, engine::general_purpose::STANDARD};
    let root = TempDir::new().unwrap();
    let s = service(&root).await;
    let main = enabled_main(&s).await;
    let (_, configuration) = active_run(&s, &main).await;
    let (target, _) = active_run(&s, &main).await;
    let chat_id = text(&target, "taskId");
    let artifact_id = uuid::Uuid::new_v4().to_string();
    let data = (0..220_000).map(|i| (i % 251) as u8).collect::<Vec<_>>();
    let digest = {
        use sha2::Digest;
        hex::encode(sha2::Sha256::digest(&data))
    };
    let record = json!({
        "id": artifact_id, "runId": target["id"], "key": "report", "version": 1,
        "title": "Private report", "name": "report.bin", "size": data.len(),
        "digest": digest, "visibility": "private", "createdAt": 1,
    });
    let artifact_record = common::deliverable(text(&target, "id"), &artifact_id, &record);
    s.store
        .transaction(move |db| db.insert_artifact(&artifact_record))
        .await
        .unwrap();
    tokio::fs::create_dir_all(s.config.data_dir.join("artifacts"))
        .await
        .unwrap();
    tokio::fs::write(
        s.config.data_dir.join("artifacts").join(&artifact_id),
        &data,
    )
    .await
    .unwrap();
    // Retention keeps originals and their authorization contracts on both MCP interfaces.
    s.store
        .change_run(
            text(&target, "id"),
            cairn_installation::runs::RunChanges {
                status: Some(RunStatus::Succeeded),
                finished_at: Some(Some(1)),
                recovery_pending: Some(false),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let mut expired = s
        .store
        .conversation_boundary(chat_id)
        .await
        .unwrap()
        .unwrap();
    expired["lastActivityAt"] = 1.into();
    s.store.save_conversation_boundary(expired).await.unwrap();
    s.cleanup_conversations().await.unwrap();
    assert_eq!(
        s.chat_detail(chat_id).await.unwrap()["environmentState"],
        "expired"
    );
    let detail = call(&s, &configuration, "get_chat", json!({ "chatId": chat_id })).await;
    assert_eq!(result(&detail)["chat"]["environmentState"], "expired");
    let detail = public_call(&s, &["read"], "get_chat", json!({ "chatId": chat_id })).await;
    assert_eq!(result(&detail)["chat"]["environmentState"], "expired");
    for cause in [
        "inactivity",
        "node-change",
        "node-revocation",
        "storage-migration",
    ] {
        s.store
            .update_run_boundary_fixture(
                text(&target, "id"),
                json!({ "environmentExpirationCause": cause }),
            )
            .await
            .unwrap();
        assert_eq!(
            s.chat_detail(chat_id).await.unwrap()["environmentExpirationCause"],
            cause
        );
        for public in [false, true] {
            let detail = if public {
                public_call(&s, &["read"], "get_chat", json!({ "chatId": chat_id })).await
            } else {
                call(&s, &configuration, "get_chat", json!({ "chatId": chat_id })).await
            };
            assert_eq!(result(&detail)["chat"]["environmentExpirationCause"], cause);
            let list = if public {
                public_call(&s, &["read"], "list_chats", json!({})).await
            } else {
                call(&s, &configuration, "list_chats", json!({})).await
            };
            let chat = result(&list)["chats"]
                .as_array()
                .unwrap()
                .iter()
                .find(|chat| chat["id"] == chat_id)
                .unwrap();
            assert_eq!(chat["environmentExpirationCause"], cause);
        }
    }
    let listed = call(
        &s,
        &configuration,
        "list_chat_artifacts",
        json!({ "chatId": chat_id }),
    )
    .await;
    assert_eq!(result(&listed)["artifacts"][0]["visibility"], "private");
    let public_file = public_call(
        &s,
        &["read"],
        "read_chat_artifact",
        json!({
            "chatId": chat_id, "artifactId": artifact_id,
        }),
    )
    .await;
    assert_eq!(result(&public_file)["totalBytes"], data.len());
    let denied_share = public_call(
        &s,
        &["read"],
        "set_chat_artifact_visibility",
        json!({
            "chatId": chat_id, "artifactId": artifact_id, "visibility": "public",
        }),
    )
    .await;
    assert_eq!(denied_share["result"]["isError"], true);
    let mut offset = 0;
    let mut received = Vec::new();
    loop {
        let chunk = call(
            &s,
            &configuration,
            "read_chat_artifact",
            json!({
                "chatId": chat_id, "artifactId": artifact_id, "offset": offset,
            }),
        )
        .await;
        let chunk = result(&chunk);
        let bytes = STANDARD.decode(chunk["data"].as_str().unwrap()).unwrap();
        assert!(bytes.len() <= 192 * 1024);
        received.extend(bytes);
        let Some(next) = chunk["nextOffset"].as_u64() else {
            break;
        };
        offset = next;
    }
    assert_eq!(received, data);
    let shared = call(
        &s,
        &configuration,
        "set_chat_artifact_visibility",
        json!({
            "chatId": chat_id, "artifactId": artifact_id, "visibility": "public",
        }),
    )
    .await;
    assert!(
        result(&shared)["publicUrl"]
            .as_str()
            .unwrap()
            .starts_with("http://127.0.0.1:")
    );
    let private = call(
        &s,
        &configuration,
        "set_chat_artifact_visibility",
        json!({
            "chatId": chat_id, "artifactId": artifact_id, "visibility": "private",
        }),
    )
    .await;
    assert!(result(&private)["publicUrl"].is_null());
    let other = s.chat_create(json!({})).await.unwrap();
    let denied = call(
        &s,
        &configuration,
        "read_chat_artifact",
        json!({
            "chatId": other["id"], "artifactId": artifact_id,
        }),
    )
    .await;
    assert_eq!(denied["result"]["isError"], true);
    let attachment_id = uuid::Uuid::new_v4().to_string();
    let upload = json!({
        "chatId": chat_id, "attachmentId": attachment_id, "name": "note.txt",
        "data": STANDARD.encode("Bonjour à tous"),
    });
    let uploaded = call(&s, &configuration, "upload_chat_attachment", upload.clone()).await;
    let retried = call(&s, &configuration, "upload_chat_attachment", upload).await;
    assert_eq!(result(&uploaded), result(&retried));
    let attachments = call(
        &s,
        &configuration,
        "list_chat_attachments",
        json!({ "chatId": chat_id }),
    )
    .await;
    assert_eq!(result(&attachments)["attachments"][0]["id"], attachment_id);
    let downloaded = call(
        &s,
        &configuration,
        "read_chat_attachment",
        json!({ "chatId": chat_id, "attachmentId": attachment_id }),
    )
    .await;
    assert_eq!(
        STANDARD
            .decode(result(&downloaded)["data"].as_str().unwrap())
            .unwrap(),
        "Bonjour à tous".as_bytes()
    );
    let stopped = call(
        &s,
        &configuration,
        "stop_chat",
        json!({ "chatId": chat_id }),
    )
    .await;
    assert_eq!(result(&stopped)["paused"], true);
    assert_eq!(
        s.store.run(target["id"].as_str().unwrap()).await.unwrap()["status"],
        RunStatus::Succeeded
    );
}

#[tokio::test]
async fn chat_questions_redact_private_answers_and_content_chunks_detect_changes() {
    let root = TempDir::new().unwrap();
    let s = service(&root).await;
    let main = enabled_main(&s).await;
    let (_, configuration) = active_run(&s, &main).await;
    let (target, _) = active_run(&s, &main).await;
    let chat_id = text(&target, "taskId").to_owned();
    let question_id = "a".repeat(64);
    s.question_receive(
        target["id"].as_str().unwrap(),
        json!({
            "id": question_id, "blocking": true,
            "fields": [{ "id": "secret", "title": "Private value", "secret": true }],
        }),
    )
    .await
    .unwrap();
    let questions = call(
        &s,
        &configuration,
        "list_chat_questions",
        json!({ "chatId": chat_id }),
    )
    .await;
    assert_eq!(result(&questions)["questions"][0]["status"], "pending");
    let answer = json!({
        "chatId": chat_id, "questionId": question_id,
        "answer": { "id": uuid::Uuid::new_v4().to_string().to_uppercase(), "answers": { "secret": { "selected": [], "comment": "fixture-private-answer" } } },
    });
    let answered = call(&s, &configuration, "answer_chat_question", answer.clone()).await;
    assert_eq!(result(&answered)["status"], "answering");
    let repeated = call(&s, &configuration, "answer_chat_question", answer).await;
    assert_eq!(result(&answered), result(&repeated));
    for tool in ["get_chat", "read_chat_content", "list_chat_questions"] {
        let response = call(&s, &configuration, tool, json!({ "chatId": chat_id })).await;
        result(&response);
        assert!(
            !response.to_string().contains("fixture-private-answer"),
            "{tool} exposed a private answer"
        );
    }
    let obsolete = "b".repeat(64);
    s.question_receive(target["id"].as_str().unwrap(), json!({
        "id": obsolete, "blocking": false, "fields": [{ "id": "note", "title": "Still useful?" }],
    })).await.unwrap();
    let withdrawn = call(
        &s,
        &configuration,
        "cancel_chat_question",
        json!({ "chatId": chat_id, "questionId": obsolete }),
    )
    .await;
    assert_eq!(result(&withdrawn)["status"], "cancelled");
    // Populate a long, entirely delivered history so pagination is limited by bytes,
    // independently of the undelivered-message queue limit.
    let chat_key = chat_id.clone();
    s.store
        .transaction(move |db| {
            for i in 0..8 {
                db.put_message(&json!({
                    "id": uuid::Uuid::new_v4(), "chatId": chat_key,
                    "text": "é".repeat(50_000), "status": "delivered", "createdAt": i,
                }))?;
            }
            Ok(())
        })
        .await
        .unwrap();
    let first = call(&s, &configuration, "get_chat", json!({ "chatId": chat_id })).await;
    assert_eq!(result(&first)["hasMore"], true);
    let mut offset = 0;
    let mut messages = Vec::new();
    loop {
        let response = call(
            &s,
            &configuration,
            "get_chat",
            json!({ "chatId": chat_id, "offset": offset }),
        )
        .await;
        messages.extend(result(&response)["messages"].as_array().unwrap().clone());
        let Some(next) = result(&response)["nextOffset"].as_u64() else {
            break;
        };
        offset = next;
    }
    assert_eq!(messages.len(), 9);
    assert_eq!(messages[8]["message"]["text"], "Private answer");
    let mut offset = 0;
    let mut checksum = Value::Null;
    let mut content = String::new();
    loop {
        let mut args = json!({ "chatId": chat_id, "offset": offset });
        if !checksum.is_null() {
            args["sha256"] = checksum.clone();
        }
        let response = call(&s, &configuration, "read_chat_content", args).await;
        let chunk = result(&response);
        checksum = chunk["sha256"].clone();
        content.push_str(chunk["data"].as_str().unwrap());
        let Some(next) = chunk["nextOffset"].as_u64() else {
            break;
        };
        offset = next;
    }
    let decoded: Value = serde_json::from_str(&content).unwrap();
    assert_eq!(decoded["messages"].as_array().unwrap().len(), 9);
    let renamed = call(
        &s,
        &configuration,
        "rename_chat",
        json!({ "chatId": chat_id, "title": "Changed snapshot", "revision": s.store.conversation_boundary(&chat_id).await.unwrap().unwrap()["revision"] }),
    )
    .await;
    result(&renamed);
    let stale = call(
        &s,
        &configuration,
        "read_chat_content",
        json!({ "chatId": chat_id, "offset": 1, "sha256": checksum }),
    )
    .await;
    assert_eq!(stale["result"]["isError"], true);
}

#[tokio::test]
async fn chat_recovery_tools_preserve_confirmation_and_retention_rules() {
    let root = TempDir::new().unwrap();
    let s = service(&root).await;
    let main = enabled_main(&s).await;
    let (_, configuration) = active_run(&s, &main).await;
    let (target, _) = active_run(&s, &main).await;
    let run_id = target["id"].as_str().unwrap();
    s.store
        .change_run(
            run_id,
            cairn_installation::runs::RunChanges {
                status: Some(RunStatus::Failed),
                boundary: Some(serde_json::json!({"restoredAt": 1})),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    s.store
        .save_checkpoint_boundary(run_id, json!({ "launched": true }))
        .await
        .unwrap();
    let chat_id = text(&target, "taskId").to_owned();
    let denied = call(
        &s,
        &configuration,
        "restart_chat_session",
        json!({ "chatId": chat_id }),
    )
    .await;
    assert_eq!(denied["result"]["isError"], true);
    let restarted = call(
        &s,
        &configuration,
        "restart_chat_session",
        json!({ "chatId": chat_id, "confirm": true }),
    )
    .await;
    assert_eq!(result(&restarted)["sessionRestartRequested"], true);
    assert_eq!(result(&restarted)["paused"], true);
    let replay = call(
        &s,
        &configuration,
        "resume_chat",
        json!({ "chatId": chat_id }),
    )
    .await;
    assert_eq!(replay["result"]["isError"], true);
    let deleted = call(
        &s,
        &configuration,
        "delete_chat",
        json!({ "chatId": chat_id }),
    )
    .await;
    assert_eq!(result(&deleted)["lifecycle"], "trash");
    let expired = call(
        &s,
        &configuration,
        "restore_chat",
        json!({ "chatId": chat_id }),
    )
    .await;
    assert_eq!(expired["result"]["isError"], true);
    let mut retained = s.conversation_boundary(&chat_id).await.unwrap();
    retained["purgeAt"] = (cairn_installation::config::now() + 60_000).into();
    s.store.save_conversation_boundary(retained).await.unwrap();
    let restored = call(
        &s,
        &configuration,
        "restore_chat",
        json!({ "chatId": chat_id }),
    )
    .await;
    assert_eq!(result(&restored)["lifecycle"], "active");
    assert_eq!(result(&restored)["paused"], true);
}

#[tokio::test]
async fn trashed_conversations_reject_timeline_content_on_both_mcp_surfaces() {
    let root = TempDir::new().unwrap();
    let s = service(&root).await;
    let main = enabled_main(&s).await;
    let (_, configuration) = active_run(&s, &main).await;
    let (target, _) = active_run(&s, &main).await;
    let run_id = target["id"].as_str().unwrap();
    s.store
        .event(
            run_id,
            &EventKind::EngineRetry,
            "retry",
            Some(json!({"type":"engine.retry", "attempt":1, "reason":"overloaded"})),
        )
        .await
        .unwrap();
    s.store
        .change_run(
            run_id,
            cairn_installation::runs::RunChanges {
                status: Some(RunStatus::Failed),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let chat_id = text(&target, "taskId").to_owned();

    let timeline = call(
        &s,
        &configuration,
        "read_run_timeline",
        json!({"runId": run_id}),
    )
    .await;
    let entry = result(&timeline)["entries"][0]["entry"].clone();
    let entry_id = entry["id"].as_str().unwrap().to_owned();
    let inputs = [
        json!({"runId":run_id, "entryId":entry_id}),
        json!({"runId":run_id, "entryId":entry_id, "timelineSnapshot":entry}),
    ];
    for input in &inputs {
        result(&call(&s, &configuration, "read_run_content", input.clone()).await);
        result(&public_call(&s, &["read"], "read_run_content", input.clone()).await);
    }

    let deleted = call(&s, &configuration, "delete_chat", json!({"chatId":chat_id})).await;
    assert_eq!(result(&deleted)["lifecycle"], "trash");
    for input in inputs {
        let installation = call(&s, &configuration, "read_run_content", input.clone()).await;
        let public = public_call(&s, &["read"], "read_run_content", input).await;
        assert_eq!(installation["result"]["isError"], true);
        assert_eq!(public["result"]["isError"], true);
    }
}

#[tokio::test]
async fn timeline_page_counts_the_complete_wrapper_at_the_byte_limit() {
    let root = TempDir::new().unwrap();
    let s = service(&root).await;
    let main = enabled_main(&s).await;
    let (run, configuration) = active_run(&s, &main).await;
    let run_id = run["id"].as_str().unwrap();
    let record =
        |text: String| json!({"item": {"id":"bounded-reply", "type":"agent_message", "text":text}});
    s.store
        .event(
            run_id,
            &EventKind::ItemUpdated,
            "x",
            Some(record("x".into())),
        )
        .await
        .unwrap();
    let first = call(
        &s,
        &configuration,
        "read_run_timeline",
        json!({"runId":run_id}),
    )
    .await;
    let entry = result(&first)["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|page| page["entry"]["content"]["itemId"] == "bounded-reply")
        .unwrap()["entry"]
        .clone();
    let entry_bytes = serde_json::to_vec(&entry).unwrap().len();
    let text = "x".repeat(256 * 1024 - 2 - entry_bytes + 1 - 10);
    s.store
        .event(
            run_id,
            &EventKind::ItemUpdated,
            &text,
            Some(record(text.clone())),
        )
        .await
        .unwrap();
    for public in [false, true] {
        let args = json!({"runId":run_id});
        let response = if public {
            public_call(&s, &["read"], "read_run_timeline", args).await
        } else {
            call(&s, &configuration, "read_run_timeline", args).await
        };
        let entries = result(&response)["entries"].as_array().unwrap();
        assert!(serde_json::to_vec(entries).unwrap().len() <= 256 * 1024);
        assert!(entries.iter().any(|page| page["kind"] == "truncated"));
    }
}
