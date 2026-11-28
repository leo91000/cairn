use super::*;

#[tokio::test]
async fn code_mode_discovers_schemas_and_composes_chat_and_artifact_reads() {
    let root = TempDir::new().unwrap();
    let s = service(&root).await;
    let main = enabled_main(&s).await;
    let (_, configuration) = active_run(&s, &main).await;
    let summary = call(
        &s,
        &configuration,
        "search",
        json!({ "query": "artifact", "limit": 20 }),
    )
    .await;
    let operations = result(&summary)["operations"].as_array().unwrap();
    assert!(
        operations
            .iter()
            .any(|op| op["name"] == "read_chat_artifact")
    );
    assert!(operations.iter().all(|op| op.get("inputSchema").is_none()));
    let schema = call(
        &s,
        &configuration,
        "search",
        json!({
            "names": ["create_chat"], "detail": "schema",
        }),
    )
    .await;
    assert_eq!(
        result(&schema)["operations"][0]["inputSchema"]["properties"]["agentId"]["default"],
        MAIN_AGENT_ID
    );
    let response = call(&s, &configuration, "execute", json!({
        "input": { "text": "A message containing quotes: \" and ` ${input} ", "id": uuid::Uuid::new_v4() },
        "code": r#"
            const chat = await cairn.create_chat({});
            await cairn.set_chat_paused({chatId: chat.id, paused: true});
            await cairn.send_chat_message({chatId: chat.id, message: input});
            const detail = await cairn.get_chat({chatId: chat.id});
            const files = await cairn.list_chat_artifacts({chatId: chat.id});
            return {id: chat.id, text: detail.messages[0].message.text, files: files.artifacts.length};
        "#,
    })).await;
    assert_eq!(
        result(&response)["text"],
        "A message containing quotes: \" and ` ${input}"
    );
    assert_eq!(result(&response)["files"], 0);
    assert!(result(&response)["id"].is_string());
}

#[tokio::test]
async fn plan_turns_only_read_the_installation() {
    let root = TempDir::new().unwrap();
    let s = service(&root).await;
    let main = enabled_main(&s).await;
    let (run, _) = active_run(&s, &main).await;
    let ordinary_configuration = s.mcps.run_configuration(&s, &run).await.unwrap();
    let ordinary = call(
        &s,
        &ordinary_configuration,
        "search",
        json!({
            "names": [
                "list_source_projects",
                "get_skill_source_status",
                "preview_agent",
                "get_plugin_operation",
            ],
        }),
    )
    .await;
    let ordinary_names = result(&ordinary)["operations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|operation| operation["name"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        ordinary_names
            .iter()
            .copied()
            .collect::<std::collections::BTreeSet<_>>(),
        [
            "list_source_projects",
            "get_skill_source_status",
            "preview_agent",
            "get_plugin_operation",
        ]
        .into_iter()
        .collect::<std::collections::BTreeSet<_>>()
    );
    // Search filters membership but retains catalogue order, not input order.
    assert_eq!(
        ordinary_names,
        [
            "preview_agent",
            "get_plugin_operation",
            "list_source_projects",
            "get_skill_source_status",
        ]
    );
    let planning = json!({ "messageId": "plan-message", "text": "Plan", "plan": true });
    let run = s
        .store
        .change_run_fixture(
            run["id"].as_str().unwrap(),
            json!({ "chatExecution": planning }),
        )
        .await
        .unwrap();
    let configuration = s.mcps.run_configuration(&s, &run).await.unwrap();
    let names = [
        "list_agents",
        "get_chat",
        "create_chat",
        "send_chat_message",
        "create_hook_rule",
        "update_agent",
        // These operations are read-only in effect, but require the `manage`
        // scope and must not enter the installation surface during Plan.
        "list_source_projects",
        "get_skill_source_status",
        "preview_agent",
        "get_plugin_operation",
    ];
    let summary = call(&s, &configuration, "search", json!({ "names": names })).await;
    let operations = result(&summary)["operations"].as_array().unwrap();
    assert!(operations.iter().any(|op| op["name"] == "list_agents"));
    assert!(operations.iter().any(|op| op["name"] == "get_chat"));
    for changing in [
        "create_chat",
        "send_chat_message",
        "create_hook_rule",
        "update_agent",
        "list_source_projects",
        "get_skill_source_status",
        "preview_agent",
        "get_plugin_operation",
    ] {
        assert!(
            !operations.iter().any(|op| op["name"] == changing),
            "{changing}"
        );
    }

    let read = call(&s, &configuration, "list_agents", json!({})).await;
    assert!(
        result(&read)["agents"].is_array() || result(&read).is_array(),
        "{read}"
    );
    let denied = call(&s, &configuration, "create_chat", json!({})).await;
    assert_eq!(denied["result"]["isError"], true, "{denied}");
    assert!(denied.to_string().contains("plan turn"), "{denied}");

    for manage_scoped in [
        "list_source_projects",
        "get_skill_source_status",
        "preview_agent",
        "get_plugin_operation",
    ] {
        let denied = call(
            &s,
            &configuration,
            "execute",
            json!({
                // `call` remains available even when an operation was removed
                // from the discovered names, so this exercises dispatch too.
                "code": format!("await cairn.call({manage_scoped:?}, {{}});"),
            }),
        )
        .await;
        assert_eq!(
            denied["result"]["isError"], true,
            "{manage_scoped}: {denied}"
        );
        assert!(
            denied.to_string().contains("read-scoped"),
            "{manage_scoped}: {denied}"
        );
    }

    let chats = s.chat_list().await.unwrap();
    assert_eq!(chats.len(), 1);
    assert_eq!(chats[0]["runId"], run["id"]);
}

#[tokio::test]
async fn code_mode_checks_each_operations_scopes_and_revocation_without_rolling_back_prior_effects()
{
    let root = TempDir::new().unwrap();
    let s = service(&root).await;
    let main = enabled_main(&s).await;
    let (_, configuration) = active_run(&s, &main).await;
    let denied = public_call(
        &s,
        &["read", "manage"],
        "execute",
        json!({
            "code": r#"
            const chat = await cairn.create_chat({});
            await cairn.send_chat_message({chatId: chat.id, message: input});
            return chat;
        "#,
            "input": { "id": uuid::Uuid::new_v4(), "text": "Must not execute" },
        }),
    )
    .await;
    assert_eq!(denied["result"]["isError"], true);
    assert!(
        denied["result"]["_meta"]["mcp/www_authenticate"]
            .as_str()
            .unwrap()
            .contains("run")
    );
    let listed = public_call(&s, &["read"], "list_chats", json!({})).await;
    let chat_id = &result(&listed)["chats"][0]["id"];
    let detail = public_call(&s, &["read"], "get_chat", json!({ "chatId": chat_id })).await;
    assert_eq!(result(&detail)["messages"], json!([]));

    let mut access = main["access"].clone();
    access["installation"] = false.into();
    let revoked = call(
        &s,
        &configuration,
        "execute",
        json!({
            "code": r#"
            await cairn.replace_agent(input);
            return await cairn.list_chats({});
        "#,
            "input": replacement(&s, json!({ "id": MAIN_AGENT_ID, "agent": { "access": access } })).await,
        }),
    )
    .await;
    assert_eq!(revoked["result"]["isError"], true);
    let agents = public_call(&s, &["read"], "list_agents", json!({})).await;
    let main = result(&agents)["agents"]
        .as_array()
        .unwrap()
        .iter()
        .find(|agent| agent["id"] == MAIN_AGENT_ID)
        .unwrap();
    assert_eq!(main["access"]["installation"], false);
}

#[tokio::test]
async fn code_mode_truncates_one_completed_result_without_repeating_its_effect() {
    let root = TempDir::new().unwrap();
    let s = service(&root).await;
    let main = enabled_main(&s).await;
    let (_, configuration) = active_run(&s, &main).await;
    let before = call(&s, &configuration, "list_chats", json!({})).await;
    let before = result(&before)["chats"].as_array().unwrap().len();

    let response = call(
        &s,
        &configuration,
        "execute",
        json!({
            "code": "await cairn.create_chat({}); return 'x'.repeat(1100000);",
        }),
    )
    .await;
    assert_eq!(response["id"], 1);
    let after = call(&s, &configuration, "list_chats", json!({})).await;
    assert_eq!(
        result(&after)["chats"].as_array().unwrap().len(),
        before + 1,
        "The oversized-result operation must have executed exactly once",
    );
    if response["result"]["isError"] == true {
        assert_eq!(
            response["result"]["content"][0]["text"],
            "Code result exceeds 1 MiB. Filter results or read bounded pages.",
            "Only the known result-budget failure qualifies the reference RED",
        );
    }
    assert_ne!(
        response["result"]["isError"], true,
        "A completed operation must not become a tool error because its result is large",
    );
    assert!(serde_json::to_vec(&response["result"]).unwrap().len() <= 1024 * 1024);
    assert_eq!(response["result"]["structuredContent"]["truncated"], true);
    assert_eq!(response["result"]["structuredContent"]["retained"], false);
    assert!(
        response["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("truncated")
    );

    let next = call(
        &s,
        &configuration,
        "execute",
        json!({ "code": "return 42;" }),
    )
    .await;
    assert_eq!(result(&next), 42);
}

#[tokio::test]
async fn code_mode_bounds_the_complete_encoded_response_and_preserves_null_results() {
    let root = TempDir::new().unwrap();
    let s = service(&root).await;
    let main = enabled_main(&s).await;
    let (_, configuration) = active_run(&s, &main).await;
    let token = configuration["env"]["CAIRN_MCP_RUN_TOKEN"]
        .as_str()
        .unwrap();
    let app = cairn_installation::http::router(s.clone()).await.unwrap();
    let (_, catalog) = request(&s, token, "tools/list", json!({})).await;
    let schema = catalog["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == "execute")
        .unwrap()["outputSchema"]
        .clone();

    // The first two values fit the worker's single-value budget but exceed the
    // final MCP budget after duplicated representations and JSON escaping.
    for code in [
        "return '\"'.repeat(400000);",
        "return '🦀'.repeat(200000);",
        "return '\\u0000'.repeat(200000);",
        "return 'x'.repeat(9 * 1024 * 1024);",
    ] {
        let body = json!({
            "jsonrpc": "2.0", "id": "encoded-fixture", "method": "tools/call",
            "params": { "name": "execute", "arguments": { "code": code } },
        });
        let request = common::request("POST", "/mcp-installation")
            .header("authorization", format!("Bearer {token}"))
            .header("mcp-protocol-version", "2026-07-28")
            .header("mcp-method", "tools/call")
            .body(Body::from(body.to_string()))
            .unwrap();
        let response = common::send(&app, request).await;
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024 + 1024)
            .await
            .unwrap();
        let response: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(response["id"], "encoded-fixture");
        let result = &response["result"];
        assert_eq!(result["resultType"], "complete");
        assert_ne!(result["isError"], true, "{code}");
        assert!(serde_json::to_vec(result).unwrap().len() <= 1024 * 1024);
        assert_eq!(result["structuredContent"]["truncated"], true);
        assert_eq!(result["structuredContent"]["retained"], false);
        assert!(jsonschema::is_valid(&schema, &result["structuredContent"]));
    }

    for code in ["return null;", "return undefined;", "return false;"] {
        let response = call(&s, &configuration, "execute", json!({ "code": code })).await;
        let expected = if code == "return false;" {
            json!(false)
        } else {
            Value::Null
        };
        assert_eq!(result(&response), &expected);
    }
}

#[tokio::test]
async fn code_mode_isolates_scripts_bounds_resources_and_remains_usable_after_failure() {
    let root = TempDir::new().unwrap();
    let s = service(&root).await;
    let main = enabled_main(&s).await;
    let (_, configuration) = active_run(&s, &main).await;
    let isolated = call(&s, &configuration, "execute", json!({
        "code": "globalThis.persisted = 1; return { frozen: Object.isFrozen(cairn), unavailable: [typeof process, typeof require, typeof fetch, typeof __cairnHost, typeof __cairnInput] };",
    })).await;
    assert_eq!(result(&isolated)["frozen"], true);
    assert_eq!(
        result(&isolated)["unavailable"],
        json!([
            "undefined",
            "undefined",
            "undefined",
            "undefined",
            "undefined"
        ])
    );
    let fresh = call(
        &s,
        &configuration,
        "execute",
        json!({ "code": "return typeof persisted;" }),
    )
    .await;
    assert_eq!(result(&fresh), "undefined");
    for code in [
        "return (", // Syntax errors cannot reach an operation.
        "return await import('node:fs');",
        "return () => 42;",
        "return 'x'.repeat(80 * 1024 * 1024);", // Heap budget.
        "for (;;) {}",                          // Process CPU budget.
        "for (let i = 0; i < 65; i++) await cairn.list_chats({}); return true;",
    ] {
        let failed = call(&s, &configuration, "execute", json!({ "code": code })).await;
        assert_eq!(failed["result"]["isError"], true, "{code}: {failed}");
    }
    let invalid = call(
        &s,
        &configuration,
        "execute",
        json!({
            "code": "return await cairn.create_chat({agentId: 'invalid'});",
        }),
    )
    .await;
    assert_eq!(invalid["result"]["isError"], true);
    let valid = call(
        &s,
        &configuration,
        "execute",
        json!({ "code": "return input.value + 1;", "input": { "value": 41 } }),
    )
    .await;
    assert_eq!(result(&valid), 42);
}

#[tokio::test]
async fn installation_access_cannot_grant_secrets_or_exclusive_reservations() {
    let root = TempDir::new().unwrap();
    let s = service(&root).await;
    let account = common::save_plugin(
        &s,
        "onepassword",
        &json!({ "name": "Production", "enabled": true, "secrets": { "token": "ops_fixture" } }),
        None,
    )
    .await
    .unwrap();
    let account = account["id"].clone();
    let manager = s
        .agent_fixture(
            json!({
                "name": "Manager",
                "access": { "installation": true },
                "plugins": [account]
            }),
            None,
        )
        .await
        .unwrap();
    let (_, configuration) = active_run(&s, &manager).await;

    // Neither an update nor a new agent can change the owner's live grants.
    call(
        &s,
        &configuration,
        "replace_agent",
        json!({
            "id": manager["id"],
            "agent": { "description": "Edited", "plugins": [], "exclusiveNodes": [] }
        }),
    )
    .await;
    let stored = s
        .agent_snapshot(manager["id"].as_str().unwrap())
        .await
        .unwrap();
    assert_eq!(stored["plugins"], json!([account]));

    let created = call(
        &s,
        &configuration,
        "save_agent",
        json!({ "name": "Escalation", "plugins": [account] }),
    )
    .await;
    // The published contract has no live grants: the request is refused or ignored.
    let agents = s.store.agent_snapshots().await.unwrap();
    for agent in agents.iter().filter(|agent| agent["id"] != manager["id"]) {
        assert_eq!(agent["plugins"], json!([]), "{created}");
    }
    let instance = s
        .store
        .instance_fixture(account.as_str().unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(instance["agentIds"], json!([manager["id"]]));
}

#[tokio::test]
async fn backend_migration_tools_match_and_recheck_management_grants() {
    let root = TempDir::new().unwrap();
    let s = service(&root).await;
    let main = enabled_main(&s).await;
    let (_, configuration) = active_run(&s, &main).await;
    let agent_token = configuration["env"]["CAIRN_MCP_RUN_TOKEN"]
        .as_str()
        .unwrap();
    let owner_token = common::relay_fixture::grant(&s, &["read", "manage"]).await;
    let app = common::relay_fixture::router(s.clone()).await.unwrap();
    let chat = cairn_installation::config::id();
    let run = cairn_installation::config::id();
    let node = cairn_installation::config::id();
    s.store
        .put_node_fixture(json!({
            "id": node,
            "runtimeId": "development",
            "capabilities": { "backend": "native" },
            "nativeEnabled": true,
            "nativeAccounts": [],
        }))
        .await
        .unwrap();
    s.store
        .set(
            "native-authority",
            json!({ "observedAt": cairn_installation::config::now(), "accounts": {} }),
            None,
        )
        .await
        .unwrap();
    // Neither caller has a traced native host grant. Management access alone
    // must not allow the migration to manufacture that authority.
    let profile = s.agent_fixture(json!({ "name": "Native destination", "access": { "backend": "native", "nodes": [node] } }), None).await.unwrap();
    let mut previous = profile.clone();
    previous["access"]["backend"] = "firecracker".into();
    common::add_run(
        &s.store,
        &json!({
            "id": run,
            "taskId": "fixture",
            "status": "succeeded",
            "createdAt": 0,
            "snapshot": { "agent": previous },
        }),
    )
    .await;
    s.store
        .save_conversation_boundary(json!({ "id": chat, "runId": run, "agentId": profile["id"] }))
        .await
        .unwrap();
    common::set_checkpoint(
        &s.store,
        &run,
        json!({ "prepared": { "projectRoot": "/data/work" } }),
    )
    .await;
    for expired in [false, true] {
        let mut current = s.store.conversation_boundary(&chat).await.unwrap().unwrap();
        current["environmentState"] = if expired {
            json!("expired")
        } else {
            Value::Null
        };
        s.store.save_conversation_boundary(current).await.unwrap();
        s.store
            .update_run_boundary_fixture(
                &run,
                json!({ "environmentState": if expired { json!("expired") } else { Value::Null } }),
            )
            .await
            .unwrap();
        if expired {
            common::set_checkpoint(&s.store, &run, json!({ "completed": true })).await;
        }
        for (endpoint, token) in [
            ("/mcp", owner_token.as_str()),
            ("/mcp-installation", agent_token),
        ] {
            let (_, preview) = node_management_call(
                &app,
                endpoint,
                token,
                "get_chat_backend_migration",
                json!({ "chatId": chat }),
            )
            .await;
            assert_eq!(result(&preview)["required"], true);
            assert_eq!(result(&preview)["environmentExpired"], expired);
            if expired {
                assert!(result(&preview)["warning"].is_null());
            } else {
                assert_eq!(
                    result(&preview)["warning"],
                    cairn_installation::environment_expiration::WARNING
                );
            }
            assert!(result(&preview)["nodes"].as_array().unwrap().is_empty());
            let (_, denied) = node_management_call(
                &app,
                endpoint,
                token,
                "migrate_chat_backend",
                json!({ "chatId": chat, "nodeId": node, "backend": "native", "confirm": true }),
            )
            .await;
            assert_eq!(denied["result"]["isError"], true, "{denied}");
            assert!(s.store.run(&run).await.unwrap()["backendMigration"].is_null());
        }
    }
    let read = common::relay_fixture::grant(&s, &["read"]).await;
    let (_, denied) = node_management_call(
        &app,
        "/mcp",
        &read,
        "migrate_chat_backend",
        json!({ "chatId": chat, "nodeId": node, "backend": "native", "confirm": true }),
    )
    .await;
    assert_eq!(denied["result"]["isError"], true);
    assert!(denied["result"]["_meta"]["mcp/www_authenticate"].is_string());
    s.agent_fixture(
        json!({ "name": "Main agent", "access": { "installation": false } }),
        Some(MAIN_AGENT_ID),
    )
    .await
    .unwrap();
    let (status, _) = node_management_call(
        &app,
        "/mcp-installation",
        agent_token,
        "get_chat_backend_migration",
        json!({ "chatId": chat }),
    )
    .await;
    assert!(matches!(
        status,
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN
    ));
}

#[tokio::test]
async fn scheduled_conversation_wakeups_are_visible_and_cancellable_through_both_tools() {
    let root = TempDir::new().unwrap();
    let s = service(&root).await;
    let main = enabled_main(&s).await;
    let (_, configuration) = active_run(&s, &main).await;
    let agent_token = text(&configuration["env"], "CAIRN_MCP_RUN_TOKEN");
    let owner_token = common::relay_fixture::grant(&s, &["read", "manage", "run"]).await;
    let app = common::relay_fixture::router(s.clone()).await.unwrap();
    for (endpoint, token) in [
        ("/mcp", owner_token.as_str()),
        ("/mcp-installation", agent_token),
    ] {
        let (run, _) = active_run(&s, &main).await;
        let run_id = text(&run, "id");
        let chat_id = text(&run, "taskId");
        let message = cairn_installation::config::id();
        s.store
            .update_run_boundary(
                run_id,
                json!({ "chatExecution": {
                    "messageId": message, "text": "Check CI", "attachments": [],
                }}),
            )
            .await
            .unwrap();
        let run = s.store.run(run_id).await.unwrap();
        let target_configuration = s.mcps.run_configuration(&s, &run).await.unwrap();
        let target_token = text(&target_configuration["env"], "CAIRN_MCP_RUN_TOKEN");
        let reason = "Check for the CI result";
        let wait = cairn_installation::chats::wait_for::schedule(
            &s,
            target_token,
            &json!({
                "conditions": [{ "type": "delay", "seconds": 60 }],
                "reason": reason,
            }),
        )
        .await
        .unwrap();
        assert_eq!(wait["deferred"], true);
        let wakeup = &wait["wakeup"];
        assert_eq!(wakeup["messageId"], message);
        s.store
            .change_run(
                run_id,
                cairn_installation::runs::RunChanges {
                    status: Some(RunStatus::Succeeded),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(&s.chat_detail(chat_id).await.unwrap()["wakeup"], wakeup);

        let args = json!({ "chatId": chat_id });
        let (status, fetched) =
            node_management_call(&app, endpoint, token, "get_chat", args.clone()).await;
        assert_eq!(status, StatusCode::OK, "{fetched}");
        assert_eq!(result(&fetched)["chat"]["runId"], run_id);
        assert_eq!(result(&fetched)["chat"]["status"], "succeeded");
        assert!(result(&fetched)["chat"]["wakeup"].is_null());
        let (_, history) =
            node_management_call(&app, endpoint, token, "get_run", json!({ "runId": run_id }))
                .await;
        assert!(
            result(&history)["events"]
                .as_array()
                .unwrap()
                .iter()
                .map(|entry| &entry["event"])
                .any(|event| {
                    event["type"] == "chat.wait"
                        && event["text"] == reason
                        && event["payload"]["id"] == wakeup["id"]
                }),
            "{history}"
        );
        let (_, listed) =
            node_management_call(&app, endpoint, token, "list_chats", json!({})).await;
        assert!(
            result(&listed)["chats"]
                .as_array()
                .unwrap()
                .iter()
                .any(|item| item["id"] == chat_id && item["runId"] == run_id)
        );
        let (_, stopped) = node_management_call(&app, endpoint, token, "stop_chat", args).await;
        assert_eq!(result(&stopped)["paused"], true);
        assert!(result(&stopped)["wakeup"].is_null());
        let final_chat = s.chat_detail(chat_id).await.unwrap();
        assert_eq!(final_chat["run"]["status"], RunStatus::Succeeded);
        assert!(final_chat["wakeup"].is_null());
    }
}

#[tokio::test]
async fn neither_agent_interface_can_review_an_administrator_node() {
    let root = TempDir::new().unwrap();
    let s = service(&root).await;
    let main = enabled_main(&s).await;
    let (_, configuration) = active_run(&s, &main).await;
    let agent_token = configuration["env"]["CAIRN_MCP_RUN_TOKEN"]
        .as_str()
        .unwrap();
    let owner_token = common::relay_fixture::grant(&s, &["read", "manage"]).await;
    let app = common::relay_fixture::router(s.clone()).await.unwrap();
    let agent = s
        .agent_fixture(
            json!({ "name": "Wildcard native", "access": { "backend": "native", "nodes": null } }),
            None,
        )
        .await
        .unwrap();
    let mut nodes = Vec::new();
    for _ in 0..2 {
        let node = cairn_installation::config::id();
        s.store.put_node_fixture(json!({
            "id": node, "capabilities": { "backend": "native", "nativeModeProtocol": 1 },
            "nativeModeRequest": { "mode": "administrator", "generation": cairn_installation::config::id() },
            "nativeReviewedAgents": [],
        })).await.unwrap();
        nodes.push(node);
    }
    let args = replacement(
        &s,
        json!({ "id": agent["id"], "agent": { "nativeNodeReviews": [nodes[0]] } }),
    )
    .await;
    let (_, denied) = node_management_call(
        &app,
        "/mcp-installation",
        agent_token,
        "replace_agent",
        args.clone(),
    )
    .await;
    assert_eq!(denied["result"]["isError"], true, "{denied}");
    assert!(
        s.node_boundary(&nodes[0]).await.unwrap()["nativeReviewedAgents"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let (_, response) =
        node_management_call(&app, "/mcp", &owner_token, "replace_agent", args).await;
    assert_eq!(response["result"]["isError"], true, "{response}");
    assert!(
        s.node_boundary(&nodes[0]).await.unwrap()["nativeReviewedAgents"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn chat_compaction_matches_public_mcp_and_installation_tools() {
    for internal in [false, true] {
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
                    status: Some(cairn_installation::run_status::RunStatus::Succeeded),
                    session_id: Some(Some("native-session".into())),
                    summary: Some("Last reply".into()),
                    finished_at: Some(Some(500)),
                    boundary: Some(json!({
                        "contextUsage": {
                            "contextTokens": 150_000,
                            "contextWindow": 200_000,
                            "cachedTokens": 120_000,
                            "estimated": false,
                        },
                    })),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        s.store
            .save_checkpoint_boundary(run_id, json!({ "launched": true, "completed": true }))
            .await
            .unwrap();
        let chat_id = text(&target, "taskId").to_owned();
        let invoke = |name, args| {
            let s = s.clone();
            let configuration = configuration.clone();
            async move {
                if internal {
                    call(&s, &configuration, name, args).await
                } else {
                    public_call(&s, &["read", "run"], name, args).await
                }
            }
        };

        let metadata = invoke("get_run", json!({ "runId": run_id })).await;
        assert_eq!(
            result(&metadata)["run"]["run"]["contextUsage"]["contextTokens"],
            150_000
        );

        let read = invoke("get_chat", json!({ "chatId": chat_id })).await;
        let summary = &result(&read)["chat"];
        assert_eq!(summary["contextUsage"]["contextTokens"], 150_000);
        assert_eq!(summary["compaction"]["available"], true);
        assert_eq!(summary["compaction"]["supported"], true);

        // Compaction starts model work: reading alone is not enough.
        let denied = public_call(&s, &["read"], "compact_chat", json!({ "chatId": chat_id })).await;
        assert_eq!(denied["result"]["isError"], true);
        assert_eq!(s.store.run(run_id).await.unwrap()["status"], "succeeded");

        // The turn's worker still owns the run while it saves its checkpoint: compaction
        // waits for it rather than resetting that checkpoint underneath it.
        s.worker.active.lock().await.insert(
            run_id.to_owned(),
            tokio_util::sync::CancellationToken::new(),
        );
        let busy = invoke("compact_chat", json!({ "chatId": chat_id })).await;
        assert_eq!(busy["result"]["isError"], true, "{busy}");
        assert!(
            busy.to_string().contains("Wait for the current turn"),
            "{busy}"
        );
        let untouched = s.store.run(run_id).await.unwrap();
        assert_eq!(untouched["status"], "succeeded");
        assert!(untouched["chatExecution"]["operation"].is_null());
        s.worker.active.lock().await.remove(run_id);

        let compacted = invoke("compact_chat", json!({ "chatId": chat_id })).await;
        assert_eq!(result(&compacted)["compaction"]["running"], true);
        let saved = s
            .store
            .conversation_boundary(&chat_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(result(&compacted)["revision"], saved["revision"]);
        let renamed = public_call(
            &s,
            &["manage"],
            "rename_chat",
            json!({
                "chatId": chat_id,
                "title": "Renamed after compaction",
                "revision": result(&compacted)["revision"],
            }),
        )
        .await;
        assert_ne!(renamed["result"]["isError"], true, "{renamed}");
        let run = s.store.run(run_id).await.unwrap();
        assert_eq!(run["status"], "queued");
        assert_eq!(run["chatExecution"]["operation"], "compact");
        assert_eq!(run["summary"], "Last reply");

        // A retry keeps the same compaction; nothing else can start meanwhile.
        let retried = invoke("compact_chat", json!({ "chatId": chat_id })).await;
        assert_eq!(result(&retried)["compaction"]["running"], true);
        assert_eq!(
            s.store.run(run_id).await.unwrap()["chatExecution"],
            run["chatExecution"]
        );
    }
}

#[tokio::test]
async fn resume_run_is_one_capability_on_http_public_and_installation() {
    let root = TempDir::new().unwrap();
    let s = service(&root).await;
    let main = enabled_main(&s).await;
    let (_, configuration) = active_run(&s, &main).await;
    let public = common::relay_fixture::grant(&s, &["run"]).await;
    let read = common::relay_fixture::grant(&s, &["read"]).await;
    let app = common::relay_fixture::router(s.clone()).await.unwrap();
    let owner = common::RelayContext::new(&common::relay_fixture::context(&s).await);
    let member =
        common::RelayContext::new(&common::relay_fixture::member_context(&s, "other-member").await);
    common::relay_fixture::allow_member(&s, "other-member", MAIN_AGENT_ID).await;
    let agent_token = text(&configuration["env"], "CAIRN_MCP_RUN_TOKEN");
    for surface in ["owner", "member", "public", "installation"] {
        let before = saved_run(&s, &main).await;
        let id = text(&before, "id");
        let (_, denied) =
            node_management_call(&app, "/mcp", &read, "resume_run", json!({ "runId": id })).await;
        assert_eq!(denied["result"]["isError"], true);
        assert!(denied["result"]["_meta"]["mcp/www_authenticate"].is_string());
        assert_eq!(s.store.run(id).await.unwrap(), before);
        let output = if matches!(surface, "owner" | "member") {
            let context = if surface == "owner" { &owner } else { &member };
            let request = context
                .authorize(common::request("POST", &format!("/api/runs/{id}/resume")))
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap();
            let response = common::send(&app, request).await;
            assert_eq!(response.status(), StatusCode::OK);
            common::read_json(response).await
        } else {
            let (endpoint, token) = if surface == "public" {
                ("/mcp", public.as_str())
            } else {
                ("/mcp-installation", agent_token)
            };
            let (_, response) =
                node_management_call(&app, endpoint, token, "resume_run", json!({ "runId": id }))
                    .await;
            result(&response).clone()
        };
        assert_eq!(output, json!({ "runId": id }));
        let after = s.store.run(id).await.unwrap();
        for field in ["id", "taskId", "workspace", "sessionId", "snapshot"] {
            assert_eq!(after[field], before[field], "{surface}: {field}");
        }
        assert_eq!(after["status"], "queued");
        assert!(after["retry"].is_null());
        let checkpoint = s.store.checkpoint_boundary(id).await.unwrap().unwrap();
        assert_eq!(
            checkpoint["prepared"],
            json!({ "cwd": "/saved/workspace", "marker": "retained" })
        );
        assert_eq!(checkpoint["generation"], "retained-generation");
        assert_ne!(checkpoint["remainingMs"], 0);
        let repeated = call(&s, &configuration, "resume_run", json!({ "runId": id })).await;
        assert_eq!(repeated["result"]["isError"], true);
        s.worker.cancel(&s, id).await.unwrap();
        assert!(s.store.run(id).await.unwrap()["cancelRequestedAt"].is_number());
    }
    let missing = cairn_installation::config::id();
    let request = owner
        .authorize(common::request(
            "POST",
            &format!("/api/runs/{missing}/resume"),
        ))
        .header("content-type", "application/json")
        .body(Body::from("{}"))
        .unwrap();
    let response = common::send(&app, request).await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let error: cairn_contracts::ResumeRunError =
        serde_json::from_value(common::read_json(response).await).unwrap();
    assert_eq!(error.error, "Run not found");
    for run_id in ["not-a-uuid".to_owned(), cairn_installation::config::id()] {
        let response = call(&s, &configuration, "resume_run", json!({ "runId": run_id })).await;
        assert_eq!(response["result"]["isError"], true);
    }
    let page = call(
        &s,
        &configuration,
        "search",
        json!({ "query": "resume_run", "detail": "schema" }),
    )
    .await;
    let operation = result(&page)["operations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == "resume_run")
        .unwrap();
    assert_eq!(
        operation["inputSchema"]["properties"]["runId"]["format"],
        "uuid"
    );
    assert_eq!(
        operation["outputSchema"]["properties"]["runId"]["format"],
        "uuid"
    );
}

#[tokio::test]
async fn installation_resume_selection_authorizes_target_before_catalog_discovery() {
    let root = TempDir::new().unwrap();
    std::fs::create_dir(root.path().join("home")).unwrap();
    let config = cairn_installation::config::Config {
        codex_bin: common::fixture("codex.mjs"),
        ..common::config(root.path())
    };
    let s = Service::new(config).await.unwrap();
    common::relay_fixture::claimed(&s).await.unwrap();
    let app = common::relay_fixture::router(s.clone()).await.unwrap();

    let caller_agent = enabled_main(&s).await;
    let (_, caller_configuration) = active_run(&s, &caller_agent).await;
    let caller_token = text(&caller_configuration["env"], "CAIRN_MCP_RUN_TOKEN");

    let target_agent = s
        .agent_fixture(
            json!({
                "name": "Native resume target",
                "provider": "codex",
                "access": {
                    "backend": "auto",
                    "nodes": [cairn_installation::nodes::LOCAL_NODE_ID],
                },
            }),
            None,
        )
        .await
        .unwrap();
    let chat = s
        .chat_create(json!({ "agentId": target_agent["id"] }))
        .await
        .unwrap();
    let chat_id = text(&chat, "id").to_owned();
    let turn_id = uuid::Uuid::new_v4().to_string();
    s.chat_send(
        &chat_id,
        json!({
            "id": turn_id,
            "text": "Keep the existing native session",
            "provider": "codex",
            "model": "fixture-fast",
            "reasoning": "high",
            "speed": "priority",
        }),
    )
    .await
    .unwrap();
    s.chat_tick(&std::collections::HashSet::new())
        .await
        .unwrap();
    let conversation = s.chat_detail(&chat_id).await.unwrap();
    let target_id = text(&conversation, "runId").to_owned();
    let queued = s.store.run(&target_id).await.unwrap();
    assert_eq!(queued["trigger"], "chat");
    assert_eq!(queued["chatExecution"]["messageId"], turn_id);
    assert_eq!(
        queued["snapshot"]["agent"]["access"]["nodes"],
        json!([cairn_installation::nodes::LOCAL_NODE_ID]),
        "the saved target must retain its authorized execution node",
    );

    let mut snapshot = queued["snapshot"].clone();
    snapshot["agent"]["access"]["backend"] = "native".into();
    snapshot["agent"]["access"]["nodes"] = json!([]);
    s.store
        .change_run(
            &target_id,
            cairn_installation::runs::RunChanges {
                snapshot: Some(serde_json::from_value(snapshot).unwrap()),
                status: Some(RunStatus::Cancelled),
                resume_available: Some(true),
                session_id: Some(Some("retained-native-session".into())),
                workspace: Some(Some("/saved/native/workspace".into())),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    common::set_checkpoint(
        &s.store,
        &target_id,
        json!({
            "launched": true,
            "prepared": { "cwd": "/saved/native/workspace" },
            "generation": "retained-generation",
        }),
    )
    .await;
    let before = s.store.run(&target_id).await.unwrap();

    // A cache miss must stay untouched when the valid caller token cannot direct this target.
    s.store.delete("codex-models:").await.unwrap();
    let discovery_log = s.config.home.join(".codex/fixture-lifecycle.jsonl");
    std::fs::create_dir_all(discovery_log.parent().unwrap()).unwrap();
    std::fs::write(&discovery_log, "").unwrap();
    assert!(s.store.kv("codex-models:").await.unwrap().is_none());

    let (status, denied) = node_management_call(
        &app,
        "/mcp-installation",
        caller_token,
        "resume_run",
        json!({
            "runId": target_id,
            "turnId": turn_id,
            "selection": {
                "model": "fixture-fast",
                "reasoning": "high",
                "speed": "priority",
            },
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{denied}");
    assert_eq!(denied["result"]["isError"], true, "{denied}");
    let after = s.store.run(&target_id).await.unwrap();
    assert_eq!(after["snapshot"], before["snapshot"]);
    assert_eq!(after["status"], before["status"]);
    assert_eq!(after, before);
    assert!(std::fs::read_to_string(discovery_log).unwrap().is_empty());
    assert!(s.store.kv("codex-models:").await.unwrap().is_none());
}

#[tokio::test]
async fn concurrent_resume_run_publishes_once_and_a_revoked_grant_cannot_resume() {
    let root = TempDir::new().unwrap();
    let s = service(&root).await;
    let main = enabled_main(&s).await;
    let (_, configuration) = active_run(&s, &main).await;
    let target = saved_run(&s, &main).await;
    let id = text(&target, "id");
    let args = json!({ "runId": id });
    let (one, two) = tokio::join!(
        call(&s, &configuration, "resume_run", args.clone()),
        call(&s, &configuration, "resume_run", args),
    );
    let successes = [&one, &two]
        .iter()
        .filter(|response| response["result"]["isError"] != true)
        .count();
    assert_eq!(successes, 1, "{one} {two}");
    let run_id = id.to_owned();
    let events = s
        .store
        .read(move |db| db.events_fixture(&run_id, 0, 100))
        .await
        .unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|event| event["text"] == "Resume requested")
            .count(),
        1
    );
    s.worker.cancel(&s, id).await.unwrap();
    s.agent_fixture(
        json!({ "name": "Main agent", "access": { "installation": false } }),
        Some(MAIN_AGENT_ID),
    )
    .await
    .unwrap();
    let token = text(&configuration["env"], "CAIRN_MCP_RUN_TOKEN");
    let (status, _) = request(
        &s,
        token,
        "tools/call",
        common::mcp_arguments("resume_run", &json!({ "runId": id })),
    )
    .await;
    assert!(matches!(
        status,
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN
    ));
    assert!(s.store.run(id).await.unwrap()["cancelRequestedAt"].is_number());
}

#[tokio::test]
async fn beacon_account_methods_are_absent_from_both_tools() {
    let root = TempDir::new().unwrap();
    let s = service(&root).await;
    let main = enabled_main(&s).await;
    let (_, configuration) = active_run(&s, &main).await;
    let token = text(&configuration["env"], "CAIRN_MCP_RUN_TOKEN");
    let public = common::relay_fixture::grant(&s, &["read", "write", "run", "manage"]).await;
    let app = common::relay_fixture::router(s.clone()).await.unwrap();
    for (endpoint, bearer) in [("/mcp", public.as_str()), ("/mcp-installation", token)] {
        let (_, discovery) = node_management_call(
            &app,
            endpoint,
            bearer,
            "search",
            json!({ "names": ["list_account_methods"], "detail": "schema" }),
        )
        .await;
        let operations = result(&discovery)["operations"].as_array().unwrap();
        assert!(operations.is_empty(), "{discovery}");

        let (_, denied) =
            node_management_call(&app, endpoint, bearer, "list_account_methods", json!({})).await;
        assert_eq!(denied["result"]["isError"], true, "{denied}");
        assert!(denied.to_string().contains("Unknown operation"), "{denied}");
    }
}

#[tokio::test]
async fn typed_project_lifecycle_has_the_same_contract_on_both_tools() {
    let root = TempDir::new().unwrap();
    let s = service(&root).await;
    let main = enabled_main(&s).await;
    let (_, configuration) = active_run(&s, &main).await;
    let agent_token = configuration["env"]["CAIRN_MCP_RUN_TOKEN"]
        .as_str()
        .unwrap();
    let owner_token = common::relay_fixture::grant(&s, &["read", "manage"]).await;
    let read_token = common::relay_fixture::grant(&s, &["read"]).await;
    let app = common::relay_fixture::router(s.clone()).await.unwrap();
    for (endpoint, token) in [
        ("/mcp", owner_token.as_str()),
        ("/mcp-installation", agent_token),
    ] {
        let input = json!({
            "creationId": cairn_installation::config::id(),
            "settings": { "name": "Typed project", "path": root.path() },
        });
        let (_, created) =
            node_management_call(&app, endpoint, token, "save_project", input.clone()).await;
        let first = result(&created).clone();
        let project_id = first["id"].clone();
        let (_, read) = node_management_call(
            &app,
            endpoint,
            token,
            "get_project",
            json!({ "projectId": project_id }),
        )
        .await;
        assert_eq!(result(&read), &first);
        let replacement = json!({
            "projectId": project_id,
            "revision": first["revision"],
            "settings": { "name": "Replaced", "path": root.path(), "sourceMode": "local" },
        });
        let (_, replaced) = node_management_call(
            &app,
            endpoint,
            token,
            "replace_project",
            replacement.clone(),
        )
        .await;
        assert_eq!(result(&replaced)["name"], "Replaced");
        let (_, stale) =
            node_management_call(&app, endpoint, token, "replace_project", replacement).await;
        assert_eq!(stale["result"]["isError"], true, "{stale}");
        let (_, replay) = node_management_call(&app, endpoint, token, "save_project", input).await;
        assert_eq!(result(&replay), result(&replaced));
        let (_, listing) =
            node_management_call(&app, endpoint, token, "list_projects", json!({})).await;
        assert!(
            result(&listing)["projects"]
                .as_array()
                .unwrap()
                .iter()
                .any(|p| p == result(&replaced))
        );
        let (_, denied) = node_management_call(
            &app,
            "/mcp",
            &read_token,
            "delete_project",
            json!({ "projectId": project_id }),
        )
        .await;
        assert_eq!(denied["result"]["isError"], true);
        let (_, deleted) = node_management_call(
            &app,
            endpoint,
            token,
            "delete_project",
            json!({ "projectId": project_id }),
        )
        .await;
        assert_eq!(result(&deleted)["deleted"], true);
        let (_, missing) = node_management_call(
            &app,
            endpoint,
            token,
            "get_project",
            json!({ "projectId": project_id }),
        )
        .await;
        assert_eq!(missing["result"]["isError"], true);
    }
}

#[tokio::test]
async fn typed_agent_profiles_preview_and_avatars_match_on_both_tools() {
    use base64::{Engine, engine::general_purpose::STANDARD};
    let root = TempDir::new().unwrap();
    let s = service(&root).await;
    let main = enabled_main(&s).await;
    let (_, configuration) = active_run(&s, &main).await;
    let token = common::relay_fixture::grant(&s, &["read", "manage"]).await;
    let app = common::relay_fixture::router(s.clone()).await.unwrap();
    let connection = s.mcp_fixture( json!({
        "name": "Selected tools", "transport": "http", "url": "http://127.0.0.1:9/mcp", "allowPrivateNetwork": true,
    }), None).await.unwrap();
    let connection_id = connection["id"].as_str().unwrap();
    for installation in [false, true] {
        let invoke = |name, args| {
            let configuration = &configuration;
            let s = &s;
            let token = &token;
            let app = &app;
            async move {
                if installation {
                    call(s, configuration, name, args).await
                } else {
                    let (status, response) =
                        node_management_call(app, "/mcp", token, name, args).await;
                    assert_eq!(status, StatusCode::OK);
                    response
                }
            }
        };
        let malformed: Value = serde_json::from_str(include_str!(
            "../../../../tests/fixtures/agent-contracts.json"
        ))
        .unwrap();
        for command in malformed["invalidCreates"]
            .as_array()
            .unwrap()
            .iter()
            .skip(7)
        {
            let rejected = invoke("save_agent", command.clone()).await;
            assert_eq!(rejected["result"]["isError"], true, "{rejected}");
        }
        let command = create_profile(json!({ "name": "Typed profile" }));
        let created = invoke("save_agent", command).await;
        let created: cairn_contracts::Agent =
            serde_json::from_value(result(&created).clone()).unwrap();
        let settings = created.settings.clone();
        let replacing = cairn_contracts::ReplaceAgentInput {
            agent_id: created.id,
            revision: created.revision.clone(),
            settings: settings.clone(),
            review: cairn_contracts::AgentReview::default(),
        };
        // Creation starts avatar work independently. Settle it before observing preview writes.
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let profile = s
                    .store
                    .agent(&created.id.to_string())
                    .await
                    .unwrap()
                    .unwrap();
                if profile
                    .avatar
                    .as_ref()
                    .is_none_or(|avatar| avatar.status != "generating")
                {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        let before = s.store.revision();
        let preview = invoke("preview_agent", serde_json::to_value(&replacing).unwrap()).await;
        let preview: cairn_contracts::AgentPreview =
            serde_json::from_value(result(&preview).clone()).unwrap();
        assert_eq!(preview.runs.queued + preview.runs.running, 0);
        assert!(preview.delegations.is_empty());
        assert_eq!(
            s.store.revision(),
            before,
            "Preview must not write or notify"
        );
        let replaced = invoke("replace_agent", serde_json::to_value(&replacing).unwrap()).await;
        let replaced: cairn_contracts::Agent =
            serde_json::from_value(result(&replaced).clone()).unwrap();
        assert_ne!(created.revision, replaced.revision);
        let stale = invoke("replace_agent", serde_json::to_value(&replacing).unwrap()).await;
        assert_eq!(stale["result"]["isError"], true);
        let mut expanded = replacing.clone();
        expanded.revision = replaced.revision.clone();
        expanded.settings.access.projects = None;
        let denied = invoke("replace_agent", serde_json::to_value(expanded).unwrap()).await;
        assert_eq!(
            denied["result"]["isError"], true,
            "An agent cannot expand permissions"
        );
        assert_eq!(
            s.store
                .agent(&created.id.to_string())
                .await
                .unwrap()
                .unwrap(),
            replaced
        );
        let configured = invoke("get_agent_avatar_configuration", json!({})).await;
        assert_eq!(result(&configured), &json!({ "configured": false }));
        let unavailable = invoke("generate_agent_avatar", json!({ "agentId": created.id })).await;
        assert_eq!(unavailable["result"]["isError"], true);
        let mut image =
            include_bytes!("../../../../tests/fixtures/artifacts/thumbnail.png").to_vec();
        image.resize(200_000, 0); // A valid image beyond the ordinary small-command body limit.
        let uploaded = invoke(
            "set_agent_avatar",
            json!({ "agentId": created.id, "data": STANDARD.encode(image) }),
        )
        .await;
        let uploaded: cairn_contracts::Agent =
            serde_json::from_value(result(&uploaded).clone()).unwrap();
        assert_eq!(uploaded.avatar.as_ref().unwrap().status, "ready");
        assert_eq!(
            uploaded.revision, replaced.revision,
            "Portrait completion preserves the settings revision"
        );
        let read = invoke("get_agent", json!({ "agentId": created.id })).await;
        assert_eq!(result(&read), &serde_json::to_value(&uploaded).unwrap());
        // The owner grants a restricted connection. Agents may remove it, but cannot
        // turn its selected tools into unrestricted tools while retaining the connection.
        let granted = s
            .agent_fixture(
                json!({ "access": {
            "mcps": [connection_id], "mcpTools": { connection_id: ["read"] },
        } }),
                Some(&created.id.to_string()),
            )
            .await
            .unwrap();
        let granted: cairn_contracts::Agent = serde_json::from_value(granted).unwrap();
        let mut reduced = cairn_contracts::ReplaceAgentInput {
            agent_id: granted.id,
            revision: granted.revision.clone(),
            settings: granted.settings,
            review: cairn_contracts::AgentReview::default(),
        };
        reduced.settings.access.mcp_tools.clear();
        let expanded = invoke("replace_agent", serde_json::to_value(&reduced).unwrap()).await;
        assert_eq!(expanded["result"]["isError"], true, "{expanded}");
        assert_eq!(
            s.store
                .agent(&created.id.to_string())
                .await
                .unwrap()
                .unwrap()
                .revision,
            reduced.revision
        );
        reduced.settings.access.mcps = Some(Vec::new());
        let removed = invoke("replace_agent", serde_json::to_value(reduced).unwrap()).await;
        assert_eq!(result(&removed)["access"]["mcps"], json!([]));
        assert_eq!(result(&removed)["access"]["mcpTools"], json!({}));

        let deleted = invoke("delete_agent", json!({ "agentId": created.id })).await;
        assert_eq!(result(&deleted), &json!({ "deleted": true }));
        assert!(
            s.store
                .agent(&created.id.to_string())
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            s.store
                .kv(&format!("agent-avatar:{}", created.id))
                .await
                .unwrap()
                .is_none()
        );
    }
}

#[tokio::test]
async fn automatic_agent_priorities_round_trip_through_both_management_tools() {
    let root = TempDir::new().unwrap();
    let s = service(&root).await;
    let main = enabled_main(&s).await;
    let (_, configuration) = active_run(&s, &main).await;
    let nodes = [
        cairn_installation::config::id(),
        cairn_installation::config::id(),
    ];
    for id in &nodes {
        s.store
            .put_node_fixture(
                json!({ "id": id, "name": id, "capabilities": { "backend": "firecracker" } }),
            )
            .await
            .unwrap();
    }
    let target = s.agent_fixture(json!({
        "name": "Automatic placement", "access": { "backend": "auto", "nodes": nodes }, "nodePriorities": nodes,
    }), None).await.unwrap();
    for internal in [false, true] {
        let mut profile = s.store.agent(text(&target, "id")).await.unwrap().unwrap();
        profile.settings.node_priorities.reverse();
        let input = serde_json::to_value(cairn_contracts::ReplaceAgentInput {
            agent_id: profile.id,
            revision: profile.revision,
            settings: profile.settings.clone(),
            review: cairn_contracts::AgentReview::default(),
        })
        .unwrap();
        let response = if internal {
            call(&s, &configuration, "replace_agent", input).await
        } else {
            public_call(&s, &["manage"], "replace_agent", input).await
        };
        assert_ne!(response["result"]["isError"], true, "{response}");
        let saved: cairn_contracts::Agent =
            serde_json::from_value(result(&response).clone()).unwrap();
        assert_eq!(saved.settings, profile.settings);
        let read = if internal {
            call(
                &s,
                &configuration,
                "get_agent",
                json!({ "agentId": saved.id }),
            )
            .await
        } else {
            public_call(&s, &["read"], "get_agent", json!({ "agentId": saved.id })).await
        };
        let read: cairn_contracts::Agent = serde_json::from_value(result(&read).clone()).unwrap();
        assert_eq!(read.settings.access.backend, "auto");
        assert_eq!(
            read.settings.node_priorities,
            saved.settings.node_priorities
        );
        assert_eq!(read.settings.access.nodes, Some(nodes.to_vec()));
    }
}

#[tokio::test]
async fn typed_mcp_commands_share_revisions_secret_redaction_and_human_oauth_boundaries() {
    let root = TempDir::new().unwrap();
    let s = service(&root).await;
    let main = enabled_main(&s).await;
    let (_, configuration) = active_run(&s, &main).await;
    let agent_token = text(&configuration["env"], "CAIRN_MCP_RUN_TOKEN");
    let public = common::relay_fixture::grant(&s, &["read", "manage"]).await;
    let read_only = common::relay_fixture::grant(&s, &["read"]).await;
    let app = common::relay_fixture::router(s.clone()).await.unwrap();
    for (endpoint, token) in [
        ("/mcp", public.as_str()),
        ("/mcp-installation", agent_token),
    ] {
        let input = json!({
            "name":"Typed MCP",
            "url":"https://example.test/mcp",
            "auth":"oauth",
            "clientSecret":"synthetic-secret",
            "enabled":false,
            "enabledTools":[],
        });
        let (_, created) =
            node_management_call(&app, endpoint, token, "create_mcp", input.clone()).await;
        let view = result(&created).clone();
        assert_eq!(view["hasClientSecret"], true, "{created}");
        assert_eq!(view["enabledTools"], json!([]));
        for key in [
            "clientSecret",
            "token",
            "env",
            "tokens",
            "verifier",
            "discovery",
        ] {
            assert!(view.get(key).is_none());
        }
        let id = view["id"].as_str().unwrap();
        let before = s.mcps.server_secrets(&s, id).await.unwrap();
        let (_, begin) = node_management_call(
            &app,
            endpoint,
            token,
            "begin_mcp_connection",
            json!({"id":id,"native":true}),
        )
        .await;
        assert!(
            result(&begin)["url"].as_str().unwrap().ends_with("/mcps"),
            "{begin}"
        );
        let (_, status) = node_management_call(
            &app,
            endpoint,
            token,
            "mcp_connection_status",
            json!({"id":id}),
        )
        .await;
        assert_eq!(result(&status)["pending"], false);
        assert_eq!(
            s.mcps.server_secrets(&s, id).await.unwrap(),
            before,
            "An agent cannot initiate a human session or mutate OAuth credentials"
        );
        let settings = serde_json::to_value(
            serde_json::from_value::<cairn_contracts::McpInput>(input).unwrap(),
        )
        .unwrap();
        let command = json!({"id":id,"revision":view["revision"],"connection":settings});
        for omitted in ["enabled", "enabledTools"] {
            let mut incomplete = command.clone();
            incomplete["connection"]
                .as_object_mut()
                .unwrap()
                .remove(omitted);
            let (_, denied) =
                node_management_call(&app, endpoint, token, "update_mcp", incomplete).await;
            assert_eq!(denied["result"]["isError"], true, "{denied}");
            let unchanged = s.mcps.server(&s, id).await.unwrap();
            assert_eq!(unchanged.revision, 1);
            assert!(!unchanged.enabled);
            assert_eq!(unchanged.enabled_tools, Some(vec![]));
        }
        let (_, saved) =
            node_management_call(&app, endpoint, token, "update_mcp", command.clone()).await;
        assert_eq!(result(&saved)["revision"], 2);
        let (_, stale) = node_management_call(&app, endpoint, token, "update_mcp", command).await;
        assert_eq!(stale["result"]["isError"], true, "{stale}");
        assert!(stale.to_string().contains("changed elsewhere"));
        let (_, cancel) = node_management_call(
            &app,
            endpoint,
            token,
            "cancel_mcp_connection",
            json!({"id":id}),
        )
        .await;
        assert_eq!(result(&cancel)["completed"], true);
        assert_eq!(s.mcps.server_secrets(&s, id).await.unwrap(), before);
        let (_, deleted) =
            node_management_call(&app, endpoint, token, "delete_mcp", json!({"id":id})).await;
        assert_eq!(result(&deleted)["completed"], true);
        assert!(s.store.mcp_server(id).await.unwrap().is_none());
    }
    let (_, denied) = node_management_call(
        &app,
        "/mcp",
        &read_only,
        "create_mcp",
        json!({"name":"denied","url":"https://example.test"}),
    )
    .await;
    assert_eq!(denied["result"]["isError"], true);
    assert!(s.mcps.list(&s).await.unwrap().is_empty());
}
