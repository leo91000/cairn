use super::*;

#[tokio::test]
async fn task_storage_limits_have_http_and_both_mcp_parity_without_agent_increases() {
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
    let owner = common::RelayContext::new(&common::relay_fixture::context(&s).await);
    let member = common::RelayContext::new(
        &common::relay_fixture::member_context(&s, "task-limit-member").await,
    );
    for method in ["GET", "PUT"] {
        let denied = member
            .authorize(common::request(method, "/api/settings/task-storage"))
            .body(Body::from(
                json!({ "maxDiskGiB": 200, "revision": "default" }).to_string(),
            ))
            .unwrap();
        assert_eq!(
            common::send(&app, denied).await.status(),
            StatusCode::FORBIDDEN
        );
    }

    let read = owner
        .authorize(common::request("GET", "/api/settings/task-storage"))
        .body(Body::empty())
        .unwrap();
    let response = common::send(&app, read).await;
    assert_eq!(response.status(), StatusCode::OK);
    let original = common::read_json(response).await;
    assert_eq!(original["maxDiskGiB"], 150);
    let command = json!({ "maxDiskGiB": 200, "revision": original["revision"] });
    let write = owner
        .authorize(common::request("PUT", "/api/settings/task-storage"))
        .body(Body::from(command.to_string()))
        .unwrap();
    let response = common::send(&app, write).await;
    assert_eq!(response.status(), StatusCode::OK);
    let mut current = common::read_json(response).await;
    assert_eq!(current["maxDiskGiB"], 200);
    let stale = owner
        .authorize(common::request("PUT", "/api/settings/task-storage"))
        .body(Body::from(command.to_string()))
        .unwrap();
    assert_eq!(
        common::send(&app, stale).await.status(),
        StatusCode::CONFLICT
    );

    for (endpoint, token, maximum) in [
        ("/mcp", owner_token.as_str(), 240),
        ("/mcp-installation", agent_token, 150),
    ] {
        let (_, view) =
            node_management_call(&app, endpoint, token, "get_task_storage_limits", json!({})).await;
        assert_eq!(result(&view), &current);
        let (_, saved) = node_management_call(
            &app,
            endpoint,
            token,
            "save_task_storage_limits",
            json!({
                "maxDiskGiB": maximum, "revision": current["revision"],
            }),
        )
        .await;
        current = result(&saved).clone();
        assert_eq!(current["maxDiskGiB"], maximum);
    }

    for (endpoint, token) in [
        ("/mcp-installation", agent_token),
        ("/mcp", read_token.as_str()),
    ] {
        let (_, denied) = node_management_call(
            &app,
            endpoint,
            token,
            "save_task_storage_limits",
            json!({
                "maxDiskGiB": 151, "revision": current["revision"],
            }),
        )
        .await;
        assert_eq!(denied["result"]["isError"], true);
    }
    let (_, unchanged) = node_management_call(
        &app,
        "/mcp",
        &owner_token,
        "get_task_storage_limits",
        json!({}),
    )
    .await;
    assert_eq!(result(&unchanged), &current);

    s.agent_fixture(
        json!({ "access": { "installation": false } }),
        Some(MAIN_AGENT_ID),
    )
    .await
    .unwrap();
    let (status, _) = node_management_call(
        &app,
        "/mcp-installation",
        agent_token,
        "get_task_storage_limits",
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn native_configuration_has_http_and_both_mcp_parity_with_live_scopes() {
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
    let target = json!({ "provider": "codex", "agentId": null });
    for (endpoint, token) in [
        ("/mcp", owner_token.as_str()),
        ("/mcp-installation", agent_token),
    ] {
        let (_, current) = node_management_call(
            &app,
            endpoint,
            token,
            "get_coding_agent_configuration",
            target.clone(),
        )
        .await;
        let revision = result(&current)["revision"].clone();
        let command = json!({ "provider": "codex", "agentId": null, "revision": revision, "document": "model_context_window = 1000000\n", "changes": [], "reset": false });
        let (_, preview) = node_management_call(
            &app,
            endpoint,
            token,
            "preview_coding_agent_configuration",
            command.clone(),
        )
        .await;
        assert!(
            result(&preview)["document"]
                .as_str()
                .unwrap()
                .contains("1000000")
        );
        let (_, saved) = node_management_call(
            &app,
            endpoint,
            token,
            "save_coding_agent_configuration",
            command,
        )
        .await;
        assert!(
            result(&saved)["document"]
                .as_str()
                .unwrap()
                .contains("1000000")
        );
        let request = common::request("POST", "/api/settings/coding-agents/read")
            .header("x-test-relay-token", &owner_token)
            .body(Body::from(target.to_string()))
            .unwrap();
        let response = common::send(&app, request).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(common::read_json(response).await, *result(&saved));
    }
    let (_, current) = node_management_call(
        &app,
        "/mcp",
        &owner_token,
        "get_coding_agent_configuration",
        target,
    )
    .await;
    let revision = result(&current)["revision"].clone();
    let command = json!({ "provider": "codex", "agentId": null, "revision": revision, "document": "model_context_window = 500000\n", "changes": [], "reset": false });
    let (_, refused) = node_management_call(
        &app,
        "/mcp",
        &read_token,
        "save_coding_agent_configuration",
        command,
    )
    .await;
    assert_eq!(refused["result"]["isError"], true);
    let (_, unchanged) = node_management_call(
        &app,
        "/mcp-installation",
        agent_token,
        "get_coding_agent_configuration",
        json!({ "provider": "codex", "agentId": null }),
    )
    .await;
    assert_eq!(result(&unchanged)["revision"], revision);

    for (provider, document) in [
        ("codex", "notify = ['blocked-callback']\n"),
        ("codex", "default_permissions = ':read-only'\n"),
        ("codex", "[permissions.readonly]\n"),
        (
            "codex",
            "[agents.reviewer]\nconfig_file = 'private-role-file.toml'\n",
        ),
        (
            "codex",
            "[hooks.state.callback]\ntrusted_hash = 'test-only'\n",
        ),
        ("codex", "[features]\nhooks = true\n"),
        ("codex", "[features]\ncodex_hooks = true\n"),
        (
            "codex",
            "[features]\ndefault_mode_request_user_input = true\n",
        ),
        (
            "claude",
            r#"{"hooks":{"SessionStart":[{"hooks":[{"type":"command","command":"blocked-callback"}]}]}}"#,
        ),
        (
            "claude",
            r#"{"env":{"CLAUDE_CODE_DISABLE_BACKGROUND_TASKS":"0"}}"#,
        ),
        ("opencode", r#"{"{env:RUNTIME_CONFIG_KEY}":{"rogue":{}}}"#),
        ("opencode", r#"{"permissions":{}}"#),
        ("opencode", r#"{"tools":{"bash":false}}"#),
        ("opencode", r#"{"agents":{"build":{"permissions":{}}}}"#),
        ("opencode", r#"{"agent":{"build":{"permissions":{}}}}"#),
        ("opencode", r#"{"mode":{"build":{"permissions":{}}}}"#),
        (
            "opencode",
            r#"{"lsp":{"custom":{"command":["blocked-callback"]}}}"#,
        ),
        (
            "opencode",
            r#"{"formatter":{"custom":{"environment":{"NODE_OPTIONS":"blocked"}}}}"#,
        ),
        (
            "opencode",
            r#"{"agent":{"build":{"permission":{"*":"allow"}}}}"#,
        ),
        ("opencode", r#"{"agent":{"build":{"tools":{"*":true}}}}"#),
        (
            "opencode",
            r#"{"mode":{"build":{"permission":{"*":"allow"}}}}"#,
        ),
        ("opencode", r#"{"mode":{"build":{"tools":{"*":true}}}}"#),
    ] {
        let target = json!({ "provider": provider, "agentId": null });
        let (_, current) = node_management_call(
            &app,
            "/mcp",
            &owner_token,
            "get_coding_agent_configuration",
            target.clone(),
        )
        .await;
        let revision = result(&current)["revision"].clone();
        let command = json!({ "provider": provider, "agentId": null, "revision": revision, "document": document, "changes": [], "reset": false });
        for (endpoint, token) in [
            ("/mcp", owner_token.as_str()),
            ("/mcp-installation", agent_token),
        ] {
            let (_, refused) = node_management_call(
                &app,
                endpoint,
                token,
                "save_coding_agent_configuration",
                command.clone(),
            )
            .await;
            assert_eq!(refused["result"]["isError"], true, "{provider}: {endpoint}");
        }
        let request = common::request("PUT", "/api/settings/coding-agents")
            .header("x-test-relay-token", &owner_token)
            .body(Body::from(command.to_string()))
            .unwrap();
        assert_eq!(
            common::send(&app, request).await.status(),
            StatusCode::BAD_REQUEST,
            "{provider}"
        );
        let (_, unchanged) = node_management_call(
            &app,
            "/mcp",
            &owner_token,
            "get_coding_agent_configuration",
            target,
        )
        .await;
        assert_eq!(result(&unchanged)["revision"], revision);
    }
}

#[tokio::test]
async fn pending_recovery_can_be_inspected_and_cancelled_through_both_tool_interfaces() {
    let root = TempDir::new().unwrap();
    let s = service(&root).await;
    let main = enabled_main(&s).await;
    let (_, configuration) = active_run(&s, &main).await;
    let agent_token = configuration["env"]["CAIRN_MCP_RUN_TOKEN"]
        .as_str()
        .unwrap();
    let owner_token = common::relay_fixture::grant(&s, &["read", "run", "manage"]).await;
    let app = common::relay_fixture::router(s.clone()).await.unwrap();
    for (endpoint, token) in [
        ("/mcp", owner_token.as_str()),
        ("/mcp-installation", agent_token),
    ] {
        for chat_retry in [true, false] {
            let chat = s.chat_create(json!({})).await.unwrap();
            s.chat_send(
                chat["id"].as_str().unwrap(),
                json!({ "text": "Keep this request", "id": uuid::Uuid::new_v4().to_string() }),
            )
            .await
            .unwrap();
            s.chat_tick(&std::collections::HashSet::default())
                .await
                .unwrap();
            let chat = s.chat_detail(chat["id"].as_str().unwrap()).await.unwrap();
            let run_id = chat["runId"].as_str().unwrap();
            let retry = json!({
                "attempt": 2,
                "limit": 5,
                "nextAttemptAt": cairn_installation::config::now() + 60_000,
                "startedAt": cairn_installation::config::now(),
                "deadlineAt": cairn_installation::config::now() + 600_000,
                "cause": { "kind": "timeout", "message": "Temporary timeout", "startup": true },
            });
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
            let (_, history) =
                node_management_call(&app, endpoint, token, "get_run", json!({ "runId": run_id }))
                    .await;
            assert_eq!(result(&history)["run"]["run"]["retry"], retry);
            let (name, args) = if chat_retry {
                ("stop_chat", json!({ "chatId": chat["id"] }))
            } else {
                ("cancel_run", json!({ "runId": run_id }))
            };
            let (status, response) = node_management_call(&app, endpoint, token, name, args).await;
            assert_eq!(status, StatusCode::OK, "{response}");
            result(&response);
            let stopped = s.store.run(run_id).await.unwrap();
            assert_eq!(stopped["status"], RunStatus::Cancelled);
            assert!(stopped["retry"].is_null());
            let retained = s.chat_detail(chat["id"].as_str().unwrap()).await.unwrap();
            assert_eq!(retained["messages"].as_array().unwrap().len(), 1);
            if chat_retry {
                assert_eq!(retained["paused"], true);
            }
        }
    }
}

#[tokio::test]
async fn node_switch_controls_match_public_mcp_and_installation_tools_and_keep_their_grants() {
    let root = TempDir::new().unwrap();
    let s = service(&root).await;
    let main = enabled_main(&s).await;
    let (run, configuration) = active_run(&s, &main).await;
    let agent_token = configuration["env"]["CAIRN_MCP_RUN_TOKEN"]
        .as_str()
        .unwrap();
    let owner_token = common::relay_fixture::grant(&s, &["read", "manage"]).await;
    let app = common::relay_fixture::router(s.clone()).await.unwrap();
    let trusted = uuid::Uuid::new_v4().to_string();
    for (endpoint, token) in [
        ("/mcp", owner_token.as_str()),
        ("/mcp-installation", agent_token),
    ] {
        s.store
            .set(
                "native-authority",
                json!({
                    "observedAt": cairn_installation::config::now(),
                    "accounts": { (trusted.clone()): { "email": "owner@example.test" } },
                }),
                None,
            )
            .await
            .unwrap();
        let (_, callers) =
            node_management_call(&app, endpoint, token, "list_native_callers", json!({})).await;
        assert_eq!(result(&callers)[0]["id"], trusted);
        let mut identities = Vec::new();
        for backend in ["firecracker", "native"] {
            let (_, response) = node_management_call(
                &app,
                endpoint,
                token,
                "create_node_enrollment",
                json!({
                    "name": "Desk", "backend": backend,
                }),
            )
            .await;
            identities.push(enroll_management_node(&app, result(&response)).await);
        }
        let previous = identities[0]["nodeId"].as_str().unwrap();
        let next = identities[1]["nodeId"].as_str().unwrap();
        let (_, updated) = node_management_call(
            &app,
            endpoint,
            token,
            "update_node",
            json!({
                "nodeId": next,
                "node": {
                    "revision": s.get_node(next).await.unwrap().revision,
                    "name": "Trusted desktop", "tags": ["gpu"],
                    "limits": { "cpu": 2, "memoryMiB": 2048, "diskMiB": 8192 },
                    "slots": 6, "accepting": true,
                    "nativeEnabled": true, "nativeAccounts": [trusted],
                },
            }),
        )
        .await;
        assert_eq!(result(&updated)["nativeEnabled"], true);
        assert_eq!(result(&updated)["capabilities"]["backend"], "native");
        let mut capable = s.node_boundary(next).await.unwrap();
        capable["capabilities"]["nativeModeProtocol"] = 1.into();
        capable["exclusiveReservations"] = true.into();
        s.store.put_node_fixture(capable).await.unwrap();
        let (_, preview) = node_management_call(
            &app,
            endpoint,
            token,
            "change_native_node_mode",
            json!({
                "nodeId": next, "change": { "mode": "administrator" },
            }),
        )
        .await;
        let preview = result(&preview);
        assert!(
            preview["preview"]["warning"]
                .as_str()
                .unwrap()
                .contains("real host")
        );
        let (_, requested) = node_management_call(&app, endpoint, token, "change_native_node_mode", json!({
            "nodeId": next, "change": { "mode": "administrator", "previewId": preview["previewId"], "confirm": true },
        })).await;
        if endpoint == "/mcp-installation" {
            assert_eq!(result(&requested)["kind"], "awaitingOwner", "{requested}");
            assert!(s.node_boundary(next).await.unwrap()["nativeModeRequest"].is_null());
        } else {
            assert_eq!(
                result(&requested)["node"]["nativeModeStatus"],
                "pending",
                "{requested}"
            );
            assert_eq!(
                result(&requested)["node"]["nativeModeRequest"]["mode"],
                "administrator"
            );
        }

        let (_, listed) =
            node_management_call(&app, endpoint, token, "list_nodes", json!({})).await;
        let node = result(&listed)
            .as_array()
            .unwrap()
            .iter()
            .find(|node| node["id"] == next)
            .unwrap();
        assert_eq!(node["nativeAccounts"], json!([trusted]));
        assert_eq!(node["slots"], 6);
        assert_eq!(node["exclusiveReservations"], true);
        s.store
            .update_run_boundary_fixture(
                run["id"].as_str().unwrap(),
                json!({ "pinnedNodeId": previous }),
            )
            .await
            .unwrap();
        let (_, refused) = node_management_call(
            &app,
            endpoint,
            token,
            "revoke_node",
            json!({ "nodeId": previous }),
        )
        .await;
        assert_eq!(refused["result"]["isError"], true, "{refused}");
        assert!(
            refused.to_string().contains("discardWorkingFiles: true"),
            "{refused}"
        );
        let (_, retired) = node_management_call(
            &app,
            endpoint,
            token,
            "revoke_node",
            json!({
                "nodeId": previous, "releasePlacements": true, "discardWorkingFiles": true,
            }),
        )
        .await;
        assert_eq!(result(&retired)["node"]["revoked"], true);
        assert_eq!(result(&retired)["releasedPlacements"], 1);
        assert!(s.store.run(run["id"].as_str().unwrap()).await.unwrap()["pinnedNodeId"].is_null());
        // Ordinary revocation preserves an explicit preference, as the owner UI does.
        s.store
            .update_run_boundary_fixture(
                run["id"].as_str().unwrap(),
                json!({ "preferredNodeId": next }),
            )
            .await
            .unwrap();
        let (_, retired) = node_management_call(
            &app,
            endpoint,
            token,
            "revoke_node",
            json!({ "nodeId": next, "discardWorkingFiles": true }),
        )
        .await;
        assert_eq!(result(&retired)["releasedPlacements"], 0);
        assert_eq!(
            s.store.run(run["id"].as_str().unwrap()).await.unwrap()["preferredNodeId"],
            next
        );
    }

    let read_token = common::relay_fixture::grant(&s, &["read"]).await;
    let (_, denied) = node_management_call(
        &app,
        "/mcp",
        &read_token,
        "create_node_enrollment",
        json!({ "name": "Denied" }),
    )
    .await;
    assert_eq!(denied["result"]["isError"], true, "{denied}");
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
        "create_node_enrollment",
        json!({ "name": "Denied" }),
    )
    .await;
    assert!(matches!(
        status,
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN
    ));
}

#[tokio::test]
async fn node_storage_controls_share_probes_persistence_and_permissions_across_both_tools() {
    use std::sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    };

    let root = TempDir::new().unwrap();
    std::fs::create_dir(root.path().join("home")).unwrap();
    let probes = Arc::new(Mutex::new(Vec::<Value>::new()));
    let fail_probe = Arc::new(AtomicBool::new(false));
    let hold_probe = Arc::new(AtomicBool::new(false));
    let probed = Arc::new(tokio::sync::Notify::new());
    let release_probe = Arc::new(tokio::sync::Notify::new());
    let applied = Arc::new(Mutex::new(
        std::collections::BTreeMap::<String, Value>::new(),
    ));
    let holding = hold_probe.clone();
    let entered = probed.clone();
    let released = release_probe.clone();
    let configured = applied.clone();
    let recorded = probes.clone();
    let failure = fail_probe.clone();
    let controller = axum::Router::new()
        .route(
            "/internal/execution/{node}/storage-policy/probe",
            axum::routing::post(move |axum::Json(policy): axum::Json<Value>| {
                let recorded = recorded.clone();
                let failure = failure.clone();
                let holding = holding.clone();
                let entered = entered.clone();
                let released = released.clone();
                async move {
                    recorded.lock().unwrap().push(policy);
                    if failure.load(Ordering::SeqCst) {
                        return StatusCode::CONFLICT;
                    }
                    if holding.swap(false, Ordering::SeqCst) {
                        entered.notify_one();
                        released.notified().await;
                    }
                    StatusCode::OK
                }
            }),
        )
        .route(
            "/internal/execution/{node}/storage-policy",
            axum::routing::post(
                move |axum::extract::Path(node): axum::extract::Path<String>,
                      axum::Json(configuration): axum::Json<Value>| {
                    let configured = configured.clone();
                    async move {
                        assert!(configuration["generation"].as_u64().unwrap() > 0);
                        configured
                            .lock()
                            .unwrap()
                            .insert(node, configuration["policy"].clone());
                        StatusCode::OK
                    }
                },
            ),
        );
    let (url, server) = common::serve_locally(controller).await;
    let mut config = common::config(root.path());
    config.public_url = url;
    let s = Service::new(config).await.unwrap();
    common::relay_fixture::claimed(&s).await.unwrap();
    // Synthetic successful AWS CLI responses; no real object store or credentials.
    std::fs::write(
        s.config.data_dir.join("storage-s3.json"),
        json!({ "bucket": "cairn-storage-test", "awsBinary": "/bin/true" }).to_string(),
    )
    .unwrap();
    let main = enabled_main(&s).await;
    let (_, configuration) = active_run(&s, &main).await;
    let agent_token = configuration["env"]["CAIRN_MCP_RUN_TOKEN"]
        .as_str()
        .unwrap();
    let owner_token = common::relay_fixture::grant(&s, &["read", "manage"]).await;
    let app = common::relay_fixture::router(s.clone()).await.unwrap();
    let policy = json!({
        "memoryCacheMiB": 512,
        "reserveMiB": 2048,
        "reservePercent": 7,
    });
    let mut last_node = String::new();
    for (endpoint, token) in [
        ("/mcp", owner_token.as_str()),
        ("/mcp-installation", agent_token),
    ] {
        let (_, invitation) = node_management_call(
            &app,
            endpoint,
            token,
            "create_node_enrollment",
            json!({ "name": "Storage host", "backend": "native" }),
        )
        .await;
        let identity = enroll_management_node(&app, result(&invitation)).await;
        let node = identity["nodeId"].as_str().unwrap();
        let (_, updated) = node_management_call(
            &app,
            endpoint,
            token,
            "update_node_storage",
            json!({ "nodeId": node, "revision": s.get_node(node).await.unwrap().revision, "storage": policy }),
        )
        .await;
        assert_eq!(result(&updated)["storage"], policy);
        for removed in ["cacheMiB", "backupSeconds", "maxDirtySeconds"] {
            let mut obsolete = policy.clone();
            obsolete[removed] = 60.into();
            let revision = s.get_node(node).await.unwrap().revision;
            let (_, refused) = node_management_call(
                &app,
                endpoint,
                token,
                "update_node_storage",
                json!({
                    "nodeId": node,
                    "revision": revision,
                    "storage": obsolete,
                }),
            )
            .await;
            assert_eq!(refused["result"]["isError"], true, "{removed}: {refused}");
            let stored = s.get_node(node).await.unwrap().storage;
            assert_eq!(serde_json::to_value(stored).unwrap(), policy);
        }
        assert_eq!(probes.lock().unwrap().last(), Some(&policy));
        let (_, listed) =
            node_management_call(&app, endpoint, token, "list_nodes", json!({})).await;
        let saved = result(&listed)
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["id"] == node)
            .unwrap();
        assert_eq!(saved["storage"], policy);

        assert_eq!(applied.lock().unwrap().get(node), Some(&policy));

        // A draft that loses its revision during the side-effect-free probe
        // must not reach the mutating controller route.
        hold_probe.store(true, Ordering::SeqCst);
        let pending_app = app.clone();
        let pending_token = token.to_owned();
        let pending_node = node.to_owned();
        let mut draft = policy.clone();
        draft["memoryCacheMiB"] = 1024.into();
        let revision = s.get_node(node).await.unwrap().revision;
        let pending = tokio::spawn(async move {
            node_management_call(
                &pending_app,
                endpoint,
                &pending_token,
                "update_node_storage",
                json!({ "nodeId": pending_node, "revision": revision, "storage": draft }),
            )
            .await
        });
        probed.notified().await;
        let current = s.get_node(node).await.unwrap();
        let (_, changed) = node_management_call(
            &app,
            endpoint,
            token,
            "update_node",
            json!({ "nodeId": node, "node": {
                "revision": current.revision, "name": "Changed during probe",
                "tags": current.tags, "limits": current.limits,
                "slots": current.slots, "accepting": current.accepting
            } }),
        )
        .await;
        assert_eq!(result(&changed)["name"], "Changed during probe");
        release_probe.notify_one();
        let (_, rejected) = pending.await.unwrap();
        assert_eq!(rejected["result"]["isError"], true, "{rejected}");
        assert_eq!(
            s.get_node(node).await.unwrap().storage,
            serde_json::from_value(policy.clone()).unwrap()
        );
        assert_eq!(applied.lock().unwrap().get(node), Some(&policy));

        fail_probe.store(true, Ordering::SeqCst);
        let mut rejected = policy.clone();
        rejected["memoryCacheMiB"] = 1024.into();
        let (_, response) = node_management_call(
            &app,
            endpoint,
            token,
            "update_node_storage",
            json!({ "nodeId": node, "revision": s.get_node(node).await.unwrap().revision, "storage": rejected }),
        )
        .await;
        assert_eq!(response["result"]["isError"], true, "{response}");
        assert_eq!(s.node_boundary(node).await.unwrap()["storage"], policy);
        fail_probe.store(false, Ordering::SeqCst);
        last_node = node.to_owned();
    }
    hold_probe.store(true, Ordering::SeqCst);
    let pending_app = app.clone();
    let pending_token = agent_token.to_owned();
    let pending_node = last_node.clone();
    let revision = s.get_node(&last_node).await.unwrap().revision;
    let mut draft = policy.clone();
    draft["memoryCacheMiB"] = 1024.into();
    let pending = tokio::spawn(async move {
        node_management_call(
            &pending_app,
            "/mcp-installation",
            &pending_token,
            "update_node_storage",
            json!({ "nodeId": pending_node, "revision": revision, "storage": draft }),
        )
        .await
    });
    probed.notified().await;
    s.agent_fixture(
        json!({ "name": "Main agent", "access": { "installation": false } }),
        Some(MAIN_AGENT_ID),
    )
    .await
    .unwrap();
    release_probe.notify_one();
    let (_, denied) = pending.await.unwrap();
    assert_eq!(denied["result"]["isError"], true, "{denied}");
    assert_eq!(applied.lock().unwrap().get(&last_node), Some(&policy));

    let before = probes.lock().unwrap().len();
    let read_token = common::relay_fixture::grant(&s, &["read"]).await;
    let (_, denied) = node_management_call(
        &app,
        "/mcp",
        &read_token,
        "update_node_storage",
        json!({ "nodeId": last_node, "storage": policy }),
    )
    .await;
    assert_eq!(denied["result"]["isError"], true, "{denied}");
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
        "update_node_storage",
        json!({ "nodeId": last_node, "storage": policy }),
    )
    .await;
    assert!(matches!(
        status,
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN
    ));
    assert_eq!(probes.lock().unwrap().len(), before);
    server.abort();
}

#[tokio::test]
async fn agent_catalog_accepts_current_providers_and_reasoning_through_native_mcp() {
    let root = TempDir::new().unwrap();
    let s = service(&root).await;
    let main = enabled_main(&s).await;
    let (_, configuration) = active_run(&s, &main).await;
    let token = configuration["env"]["CAIRN_MCP_RUN_TOKEN"]
        .as_str()
        .unwrap();
    let (_, catalog) = request(&s, token, "tools/list", json!({})).await;
    let tools = catalog["result"]["tools"].as_array().unwrap();
    assert_eq!(
        tools
            .iter()
            .map(|tool| tool["name"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["search", "execute"]
    );
    let catalog = call(
        &s,
        &configuration,
        "search",
        json!({ "names": ["save_agent", "replace_agent"], "detail": "schema" }),
    )
    .await;
    let operations = result(&catalog)["operations"].as_array().unwrap();
    assert_eq!(operations.len(), 2);
    for name in ["save_agent", "replace_agent"] {
        let tool = operations.iter().find(|tool| tool["name"] == name).unwrap();
        let schema = &tool["inputSchema"]["definitions"]["AgentSettings"];
        assert_eq!(
            schema["properties"]["provider"]["pattern"],
            json!("^(codex|claude|opencode|antigravity)$")
        );
        assert!(schema["properties"]["reasoning"].get("enum").is_none());
    }

    let saved = call(
        &s,
        &configuration,
        "save_agent",
        json!({ "name": "Default engine" }),
    )
    .await;
    assert_eq!(result(&saved)["provider"], "codex");
    assert_eq!(result(&saved)["reasoning"], "high");

    for provider in ["codex", "claude", "opencode"] {
        let saved = call(
            &s,
            &configuration,
            "save_agent",
            json!({
                "name": "Selected engine",
                "provider": provider,
                "model": if provider == "opencode" { "opencode/test-model" } else { "" },
                "reasoning": if provider == "claude" { "max" } else { "future_effort-2" },
            }),
        )
        .await;
        let agent = result(&saved);
        assert_eq!(agent["provider"], provider);
        assert_eq!(
            agent["reasoning"],
            if provider == "claude" {
                "max"
            } else {
                "future_effort-2"
            }
        );
        assert_eq!(
            s.agent_snapshot(agent["id"].as_str().unwrap())
                .await
                .unwrap()["provider"],
            provider
        );
    }

    for invalid in [
        json!({ "provider": "unknown" }),
        json!({ "provider": null }),
        json!({ "reasoning": "INVALID" }),
        json!({ "reasoning": "a".repeat(41) }),
    ] {
        let mut args = json!({ "name": "Invalid profile" });
        args.as_object_mut()
            .unwrap()
            .extend(invalid.as_object().unwrap().clone());
        let rejected = call(&s, &configuration, "save_agent", args).await;
        assert_eq!(rejected["result"]["isError"], true, "{rejected}");
    }
}

#[tokio::test]
async fn agent_replacements_keep_explicit_settings_and_accept_new_engine_settings() {
    let root = TempDir::new().unwrap();
    let s = service(&root).await;
    let main = enabled_main(&s).await;
    let (_, configuration) = active_run(&s, &main).await;
    let saved = s
        .agent_fixture(
            json!({
                "name": "Restricted Claude",
                "provider": "claude",
                "reasoning": "max",
                "model": "claude-opus-4-6",
                "instructions": "Preserve these settings",
                "timeoutMinutes": 37,
                "access": { "nodes": [], "installation": true },
            }),
            None,
        )
        .await
        .unwrap();
    let original = &saved;
    let updated = call(
        &s,
        &configuration,
        "replace_agent",
        json!({
            "id": original["id"],
            "agent": { "description": "New description" },
        }),
    )
    .await;
    let agent = result(&updated);
    for field in [
        "provider",
        "reasoning",
        "model",
        "instructions",
        "timeoutMinutes",
        "access",
    ] {
        assert_eq!(agent[field], original[field], "{field}");
    }

    let updated = call(
        &s,
        &configuration,
        "replace_agent",
        json!({
            "id": original["id"],
            "agent": {
                "provider": "opencode",
                "model": "opencode/test-model",
                "reasoning": "future_effort-2",
                "access": {},
            },
        }),
    )
    .await;
    let agent = result(&updated);
    assert_eq!(agent["provider"], "opencode");
    assert_eq!(agent["reasoning"], "future_effort-2");
    assert_eq!(agent["access"]["nodes"], json!([]));
    assert_eq!(agent["access"]["installation"], true);

    let updated = call(
        &s,
        &configuration,
        "replace_agent",
        json!({
            "id": original["id"],
            "agent": { "reasoning": "" },
        }),
    )
    .await;
    assert_eq!(result(&updated)["reasoning"], "");
    assert_eq!(result(&updated)["provider"], "opencode");

    for patch in [
        json!({ "provider": "unknown" }),
        json!({ "provider": null }),
        json!({ "reasoning": "INVALID" }),
    ] {
        let before = s
            .agent_snapshot(original["id"].as_str().unwrap())
            .await
            .unwrap();
        let rejected = call(
            &s,
            &configuration,
            "replace_agent",
            json!({
                "id": original["id"],
                "agent": patch,
            }),
        )
        .await;
        assert_eq!(rejected["result"]["isError"], true, "{rejected}");
        assert_eq!(
            s.agent_snapshot(original["id"].as_str().unwrap())
                .await
                .unwrap(),
            before
        );
    }
}

#[tokio::test]
async fn model_speeds_and_agent_speeds_match_public_mcp_and_installation_tools() {
    let root = TempDir::new().unwrap();
    std::fs::create_dir(root.path().join("home")).unwrap();
    let config = cairn_installation::config::Config {
        codex_bin: common::fixture("codex.mjs"),
        ..common::config(root.path())
    };
    let s = Service::new(config).await.unwrap();
    common::relay_fixture::claimed(&s).await.unwrap();
    let main = enabled_main(&s).await;
    let (_, configuration) = active_run(&s, &main).await;

    let internal = call(&s, &configuration, "list_models", json!({})).await;
    let public = public_call(&s, &["read"], "list_models", json!({ "provider": "codex" })).await;
    assert_eq!(result(&internal), result(&public));
    let fast = result(&public)["models"]
        .as_array()
        .unwrap()
        .iter()
        .find(|model| model["model"] == "fixture-fast")
        .unwrap()
        .clone();
    assert_eq!(fast["serviceTiers"][0]["id"], "priority");
    assert_eq!(fast["serviceTiers"][0]["name"], "Fast");
    let denied = public_call(&s, &["run"], "list_models", json!({})).await;
    assert_eq!(denied["result"]["isError"], true, "{denied}");

    let saved = call(
        &s,
        &configuration,
        "save_agent",
        json!({ "name": "Fast reviewer", "speed": "priority" }),
    )
    .await;
    let agent = result(&saved).clone();
    assert_eq!(agent["speed"], "priority");
    let kept = public_call(
        &s,
        &["manage"],
        "replace_agent",
        json!({ "id": agent["id"], "agent": { "description": "Keeps its speed" } }),
    )
    .await;
    assert_eq!(result(&kept)["speed"], "priority");
    let standard = call(
        &s,
        &configuration,
        "replace_agent",
        json!({ "id": agent["id"], "agent": { "speed": "standard" } }),
    )
    .await;
    assert_eq!(result(&standard)["speed"], "standard");

    // Speed belongs to Codex: other coding agents reject it, and switching drops it.
    let rejected = public_call(
        &s,
        &["manage"],
        "save_agent",
        json!({ "name": "Claude", "provider": "claude", "speed": "priority" }),
    )
    .await;
    assert_eq!(rejected["result"]["isError"], true, "{rejected}");
    let rejected = call(
        &s,
        &configuration,
        "replace_agent",
        json!({ "id": agent["id"], "agent": { "provider": "claude", "speed": "priority" } }),
    )
    .await;
    assert_eq!(rejected["result"]["isError"], true, "{rejected}");
    let switched = call(
        &s,
        &configuration,
        "replace_agent",
        json!({ "id": agent["id"], "agent": { "provider": "claude", "reasoning": "max", "speed": "" } }),
    )
    .await;
    assert_eq!(result(&switched)["provider"], "claude");
    assert_eq!(result(&switched)["speed"], "");

    // A queued message keeps its speed through edits that do not send one, on both tools.
    let chat = s.chat_create(json!({})).await.unwrap();
    let chat_id = chat["id"].as_str().unwrap();
    s.chat_pause(chat_id, true).await.unwrap();
    let message_id = uuid::Uuid::new_v4().to_string();
    let sent = public_call(
        &s,
        &["run"],
        "send_chat_message",
        json!({
            "chatId": chat_id,
            "message": { "id": message_id, "text": "Next", "provider": "codex", "speed": "priority" },
        }),
    )
    .await;
    assert_eq!(result(&sent)["speed"], "priority", "{sent}");
    let edited = public_call(
        &s,
        &["run"],
        "edit_chat_message",
        json!({ "chatId": chat_id, "messageId": message_id, "message": { "text": "Reworded" } }),
    )
    .await;
    assert_eq!(result(&edited)["speed"], "priority", "{edited}");
    let edited = call(
        &s,
        &configuration,
        "edit_chat_message",
        json!({ "chatId": chat_id, "messageId": message_id, "message": { "text": "Again" } }),
    )
    .await;
    assert_eq!(result(&edited)["speed"], "priority", "{edited}");
    let standard = call(
        &s,
        &configuration,
        "edit_chat_message",
        json!({ "chatId": chat_id, "messageId": message_id, "message": { "text": "Again", "speed": "standard" } }),
    )
    .await;
    assert_eq!(result(&standard)["speed"], "standard", "{standard}");
}

#[tokio::test]
async fn installation_access_is_opt_in_and_complete_profile_replacements_preserve_it() {
    let root = TempDir::new().unwrap();
    let s = service(&root).await;
    let main = s.agent_snapshot(MAIN_AGENT_ID).await.unwrap();
    let (_, disabled) = active_run(&s, &main).await;
    assert!(disabled["codexConfig"].get("cairn_installation").is_none());
    assert!(
        disabled["claudeMcps"]["mcpServers"]
            .get("cairn_installation")
            .is_none()
    );
    assert_eq!(
        request(&s, "invalid", "tools/list", json!({})).await.0,
        StatusCode::UNAUTHORIZED
    );

    let enabled = enabled_main(&s).await;
    let (_, configuration) = active_run(&s, &enabled).await;
    assert_eq!(
        configuration["codexConfig"]["cairn_installation"]["url"],
        "http://localhost:4310/mcp-installation"
    );
    assert!(configuration.get("claudeMcps").is_none());
    let claude = s
        .agent_fixture(
            json!({
                "name": "Claude", "provider": "claude",
                "access": { "installation": true },
            }),
            None,
        )
        .await
        .unwrap();
    let (_, claude_configuration) = active_run(&s, &claude).await;
    assert!(claude_configuration.get("codexConfig").is_none());
    assert_eq!(
        claude_configuration["claudeMcps"]["mcpServers"]["cairn_installation"]["url"],
        "http://localhost:4310/mcp-installation"
    );
    let token = configuration["env"]["CAIRN_MCP_RUN_TOKEN"]
        .as_str()
        .unwrap();
    assert!(
        configuration["redactions"]
            .as_array()
            .unwrap()
            .contains(&json!(token))
    );
    let saved = s
        .agent_fixture(
            json!({ "name": "Older editor", "access": { "nodes": null } }),
            Some(MAIN_AGENT_ID),
        )
        .await
        .unwrap();
    assert_eq!(saved["access"]["installation"], true);
    assert_eq!(
        request(&s, token, "tools/list", json!({})).await.0,
        StatusCode::OK
    );
}

#[tokio::test]
async fn installation_tools_schedule_inspect_update_run_and_cancel_missions() {
    let root = TempDir::new().unwrap();
    let s = service(&root).await;
    let main = enabled_main(&s).await;
    let (run, configuration) = active_run(&s, &main).await;
    let token = configuration["env"]["CAIRN_MCP_RUN_TOKEN"]
        .as_str()
        .unwrap();
    let (_, catalog) = request(&s, token, "tools/list", json!({})).await;
    let native = catalog["result"]["tools"].as_array().unwrap();
    assert_eq!(
        native
            .iter()
            .map(|tool| tool["name"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["search", "execute"]
    );
    assert!(native.iter().all(|tool| tool.get("_meta").is_none()));
    assert!(serde_json::to_vec(native).unwrap().len() < 6000);
    let mut tools = Vec::new();
    let mut offset = 0;
    loop {
        let page = call(
            &s,
            &configuration,
            "search",
            json!({ "detail": "schema", "limit": 20, "offset": offset }),
        )
        .await;
        tools.extend(result(&page)["operations"].as_array().unwrap().clone());
        let Some(next) = result(&page)["nextOffset"].as_u64() else {
            break;
        };
        offset = next;
    }
    for name in [
        "run_hook_rule",
        "preview_schedule",
        "snooze_chat",
        "get_run",
        "delete_agent",
    ] {
        assert!(tools.iter().any(|tool| tool["name"] == name), "{name}");
    }
    for removed in [
        "list_tasks",
        "get_task",
        "create_task",
        "update_task",
        "replace_task",
        "set_task_state",
        "delete_task",
        "list_task_activity",
        "run_task",
        "retry_run",
        "convert_run_to_chat",
        "set_outcome_aside",
    ] {
        assert!(
            !tools.iter().any(|tool| tool["name"] == removed),
            "{removed}"
        );
    }
    let public: Value =
        serde_json::from_str(include_str!("../../schemas/mcp-operations.json")).unwrap();
    let mut public_names: std::collections::BTreeSet<_> = public
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    let installed_names: std::collections::BTreeSet<_> = tools
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    public_names.extend([
        "list_mcps",
        "create_mcp",
        "update_mcp",
        "test_mcp",
        "disconnect_mcp",
        "delete_mcp",
        "begin_mcp_connection",
        "mcp_connection_status",
        "cancel_mcp_connection",
    ]);
    public_names.extend([
        "cancel_run",
        "cleanup_run",
        "list_runs",
        "get_run",
        "read_run_history",
        "read_run_timeline",
        "read_run_content",
    ]);
    public_names.extend([
        "get_conversation_retention",
        "set_conversation_retention",
        "get_chat_coding_agent_options",
        "switch_chat_coding_agent",
        "list_chats",
        "get_chat",
        "create_chat",
        "send_chat_message",
        "edit_chat_message",
        "delete_chat_message",
        "rename_chat",
        "set_chat_paused",
        "stop_chat",
        "compact_chat",
        "delete_chat",
        "resume_chat",
        "restart_chat_session",
        "restore_chat",
        "read_chat_content",
    ]);
    public_names.extend(["get_chat_share", "set_chat_share", "revoke_chat_share"]);
    public_names.insert("upload_chat_attachment");
    public_names.insert(cairn_contracts::RESUME_RUN_ID);
    public_names.extend([
        "list_run_artifacts",
        "read_run_artifact",
        "set_run_artifact_visibility",
    ]);
    public_names.insert("manage_plugin");
    public_names.extend([
        "list_plugins",
        "list_plugin_resources",
        "resolve_plugin_resource",
        "install_plugin_package",
        "approve_plugin_package",
        "activate_plugin_package",
        "delete_plugin_package",
        "list_plugin_programs",
        "prepare_plugin_program",
        "get_plugin_operation",
        "get_plugin_resource_operation",
        "cancel_plugin_operation",
    ]);
    public_names.extend([
        "get_conversation_placement",
        "configure_conversation_placement",
        "move_to_node",
        "get_node_move_operation",
        "cancel_node_move_operation",
        "release_node_reservation",
        "list_nodes",
        "list_native_callers",
        "create_node_enrollment",
        "update_node",
        "change_native_node_mode",
        "update_node_storage",
        "revoke_node",
        "set_agent_avatar",
        "list_agents",
        "get_agent",
        "get_agent_plugin_grants",
        "replace_agent_plugin_grants",
        "save_agent",
        "replace_agent",
        "preview_agent",
        "merge_agents",
        "delete_agent",
        "get_agent_avatar_configuration",
        "list_agent_accounts",
        "generate_agent_avatar",
        "preview_schedule",
        "list_projects",
        "get_project",
        "save_project",
        "replace_project",
        "delete_project",
    ]);
    public_names.extend([
        "list_chat_questions",
        "answer_chat_question",
        "cancel_chat_question",
        "list_chat_side_questions",
        "ask_chat_side_question",
        "cancel_chat_side_question",
        "list_models",
        "list_coding_agent_accounts",
        "get_coding_agent_account_sign_in",
        "list_coding_agent_account_providers",
        "replace_coding_agent_account",
        "order_coding_agent_accounts",
        "refresh_coding_agent_accounts",
        "connect_coding_agent_account",
        "reconnect_coding_agent_account",
        "replace_coding_agent_account_key",
        "submit_coding_agent_account_code",
        "cancel_coding_agent_account_sign_in",
        "delete_coding_agent_account",
    ]);
    public_names.extend([
        "mark_read",
        "snooze_chat",
        "get_installation_settings",
        "get_coding_agent_configuration",
        "preview_coding_agent_configuration",
        "save_coding_agent_configuration",
        "get_installation_overview",
        "list_audit",
        "get_node_runtime_settings",
        "save_node_runtime_settings",
        "get_task_storage_limits",
        "save_task_storage_limits",
        "get_chat_plan_settings",
        "save_chat_plan_settings",
        "import_project",
        "list_source_projects",
        "list_skills",
        "read_skill",
        "save_skill",
        "edit_skill_files",
        "stage_skill_upload",
        "import_skill",
        "delete_skill",
        "export_skill_archive",
        "get_skill_source_status",
        "update_skill_source",
        "forget_skill_source",
        "list_hook_rules",
        "get_hook_rule",
        "list_hook_rule_versions",
        "list_hook_rule_triggerings",
        "get_hook_rule_triggering",
        "get_hook_rule_catalog",
        "validate_hook_rule",
        "create_hook_rule",
        "replace_hook_rule",
        "set_hook_rule_enabled",
        "set_hook_rule_emergency_pause",
        "lift_hook_rule_budget",
        "run_hook_rule",
        "duplicate_hook_rule",
        "list_hook_rule_templates",
        "install_hook_rule_template",
        "delete_hook_rule",
    ]);
    assert_eq!(installed_names, public_names);
    assert!(tools.iter().all(|tool| tool.get("_meta").is_none()));
    let deletion = tools
        .iter()
        .find(|tool| tool["name"] == "delete_agent")
        .unwrap();
    assert_eq!(deletion["annotations"]["destructiveHint"], true);
    assert_eq!(deletion["annotations"]["readOnlyHint"], false);

    let (target, _) = active_run(&s, &main).await;
    let queued_id = target["id"].clone();
    s.store
        .change_run(
            queued_id.as_str().unwrap(),
            cairn_installation::runs::RunChanges {
                status: Some(RunStatus::Queued),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let cancelled = call(
        &s,
        &configuration,
        "cancel_run",
        json!({ "runId": queued_id }),
    )
    .await;
    assert_eq!(result(&cancelled)["cancelled"], true);
    assert_eq!(
        s.store.run(queued_id.as_str().unwrap()).await.unwrap()["status"],
        RunStatus::Cancelled
    );
    // The engine's own retries and usage limits read as the app shows them.
    let engine =
        json!({ "type": "engine.retry", "attempt": 2, "maxRetries": 10, "reason": "overloaded" });
    s.store
        .event(
            run["id"].as_str().unwrap(),
            &EventKind::EngineRetry,
            "retry",
            Some(engine.clone()),
        )
        .await
        .unwrap();
    let history = call(&s, &configuration, "get_run", json!({ "runId": run["id"] })).await;
    assert_eq!(result(&history)["run"]["run"]["id"], run["id"]);
    let events = result(&history)["events"].as_array().unwrap().clone();
    let window = call(
        &s,
        &configuration,
        "read_run_history",
        json!({
            "runId": run["id"], "history": result(&history)["history"],
        }),
    )
    .await;
    assert_ne!(window["result"]["isError"], true);
    assert!(
        result(&window)["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|record| record["event"]["type"] == "engine.retry")
    );

    assert!(
        events
            .iter()
            .any(|record| record["event"]["type"] == "engine.retry"
                && record["event"]["payload"] == engine)
    );
    // The timeline gives agents the same interpreted card the apps present.
    let timeline = call(
        &s,
        &configuration,
        "read_run_timeline",
        json!({ "runId": run["id"] }),
    )
    .await;
    let cards = result(&timeline)["entries"].as_array().unwrap().clone();
    let card = cards
        .iter()
        .find(|page| page["entry"]["content"]["notice"]["code"] == "engineRetry")
        .unwrap();
    assert_eq!(card["kind"], "complete");
    assert_eq!(
        card["entry"]["content"]["notice"]["engineRetry"],
        json!({"attempt": 2, "maxRetries": 10, "reason": "overloaded", "httpStatus": null, "delayMs": null, "noResponse": false})
    );
    assert_eq!(card["entry"]["content"]["notice"]["prominent"], true);
    let chunk = call(
        &s,
        &configuration,
        "read_run_content",
        json!({ "runId": run["id"] }),
    )
    .await;
    assert_eq!(result(&chunk)["runId"], run["id"]);
}

#[tokio::test]
async fn delete_agent_removes_an_unused_profile_and_preserves_completed_history() {
    let root = TempDir::new().unwrap();
    let s = service(&root).await;
    let main = enabled_main(&s).await;
    let (_, configuration) = active_run(&s, &main).await;
    let agent = s
        .agent_fixture(json!({ "name": "Retired maintainer" }), None)
        .await
        .unwrap();
    let id = agent["id"].as_str().unwrap();
    let (run, _) = active_run(&s, &agent).await;
    s.store
        .change_run(
            run["id"].as_str().unwrap(),
            cairn_installation::runs::RunChanges {
                status: Some(RunStatus::Succeeded),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    s.store
        .transaction({
            let id = id.to_owned();
            move |db| {
                db.set(
                    &format!("agent-avatar:{id}"),
                    &json!({ "status": "ready" }),
                    None,
                )?;
                // A plugin instance (such as a 1Password service account) granting it.
                let mut instance = cairn_installation::plugins::connections::Connection::draft(
                    "00000000-0000-4000-8000-000000000099",
                    "onepassword",
                    json!({}),
                    Vec::new(),
                );
                instance.name = "Fixture".into();
                instance.revision = "r1".into();
                {
                    db.save_plugin_version(&instance.plugin_id, "fixture", &"0".repeat(64))?;
                    db.save_plugin_instance(&instance)
                }?;
                for agent_id in [id.as_str(), MAIN_AGENT_ID] {
                    let mut agent = db.agent(agent_id)?.unwrap();
                    agent
                        .settings
                        .plugins
                        .push("00000000-0000-4000-8000-000000000099".into());
                    db.save_agent(&agent)?;
                }
                Ok(())
            }
        })
        .await
        .unwrap();

    let deleted = call(&s, &configuration, "delete_agent", json!({ "id": id })).await;
    assert_eq!(result(&deleted)["deleted"], true);
    assert!(s.store.agent_snapshot(id).await.unwrap().is_none());
    assert!(
        s.store
            .kv(&format!("agent-avatar:{id}"))
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        s.store
            .instance_fixture("00000000-0000-4000-8000-000000000099")
            .await
            .unwrap()
            .unwrap()["agentIds"],
        json!([MAIN_AGENT_ID])
    );
    assert_eq!(
        s.store.run(run["id"].as_str().unwrap()).await.unwrap()["snapshot"]["agent"]["id"],
        id
    );
    let missing = call(&s, &configuration, "delete_agent", json!({ "id": id })).await;
    assert_eq!(missing["result"]["isError"], true);
}

#[tokio::test]
async fn delete_agent_rejects_main_task_references_and_active_work() {
    let root = TempDir::new().unwrap();
    let s = service(&root).await;
    let main = enabled_main(&s).await;
    let (_, configuration) = active_run(&s, &main).await;
    let main_delete = call(
        &s,
        &configuration,
        "delete_agent",
        json!({ "id": MAIN_AGENT_ID }),
    )
    .await;
    assert_eq!(main_delete["result"]["isError"], true);
    assert_eq!(
        main_delete["result"]["content"][0]["text"],
        "The main agent cannot be removed."
    );

    let agent = s
        .agent_fixture(json!({ "name": "Assigned maintainer" }), None)
        .await
        .unwrap();
    let id = agent["id"].as_str().unwrap();

    let (_run, _) = active_run(&s, &agent).await;
    let busy = call(&s, &configuration, "delete_agent", json!({ "id": id })).await;
    assert_eq!(busy["result"]["isError"], true);
    assert_eq!(
        busy["result"]["content"][0]["text"],
        "This item has active work. Cancel or wait for the run first."
    );
    assert!(s.store.agent_snapshot(id).await.unwrap().is_some());
}

#[tokio::test]
async fn delete_agent_cannot_use_a_revoked_installation_grant() {
    let root = TempDir::new().unwrap();
    let s = service(&root).await;
    let main = enabled_main(&s).await;
    let (run, configuration) = active_run(&s, &main).await;
    let agent = s
        .agent_fixture(json!({ "name": "Keep this profile" }), None)
        .await
        .unwrap();
    s.mcps
        .revoke_run(&s, run["id"].as_str().unwrap())
        .await
        .unwrap();
    let token = configuration["env"]["CAIRN_MCP_RUN_TOKEN"]
        .as_str()
        .unwrap();
    let (status, _) = request(
        &s,
        token,
        "tools/call",
        json!({
            "name": "delete_agent",
            "arguments": { "agentId": agent["id"] },
        }),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(
        s.store
            .agent_snapshot(agent["id"].as_str().unwrap())
            .await
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn merge_agents_cannot_use_a_revoked_installation_grant() {
    let root = TempDir::new().unwrap();
    let s = service(&root).await;
    let main = enabled_main(&s).await;
    let (run, configuration) = active_run(&s, &main).await;
    let agent = s
        .agent_fixture(json!({ "name": "Keep this profile" }), None)
        .await
        .unwrap();
    s.mcps
        .revoke_run(&s, run["id"].as_str().unwrap())
        .await
        .unwrap();
    let token = configuration["env"]["CAIRN_MCP_RUN_TOKEN"]
        .as_str()
        .unwrap();
    let (status, _) = request(
        &s,
        token,
        "tools/call",
        common::mcp_arguments(
            "merge_agents",
            &json!({
                "sourceId": agent["id"], "targetId": MAIN_AGENT_ID, "confirm": true,
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(
        s.store
            .agent_snapshot(agent["id"].as_str().unwrap())
            .await
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn installation_access_manages_resources_and_other_agents_work() {
    let root = TempDir::new().unwrap();
    let s = service(&root).await;
    let main = s.agent_snapshot(MAIN_AGENT_ID).await.unwrap();
    let (foreign, _) = active_run(&s, &main).await;
    let manager = s
        .agent_fixture(
            json!({
                "name": "Manager",
                "access": {
                    "installation": true,
                    "projects": [],
                    "skills": [],
                    "mcps": [],
                    "sandbox": "read-only"
                }
            }),
            None,
        )
        .await
        .unwrap();
    let (_, configuration) = active_run(&s, &manager).await;

    let saved = call(
        &s,
        &configuration,
        "save_agent",
        json!({ "name": "Created through MCP" }),
    )
    .await;
    let agent = result(&saved);
    let updated = call(
        &s,
        &configuration,
        "replace_agent",
        json!({
            "id": agent["id"],
            "agent": { "description": "Updated through MCP" }
        }),
    )
    .await;
    assert_eq!(result(&updated)["description"], "Updated through MCP");
    let agents = call(&s, &configuration, "list_agents", json!({})).await;
    assert!(
        result(&agents)["agents"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["id"] == MAIN_AGENT_ID)
    );

    let project_path = root.path().join("managed-project");
    std::fs::create_dir(&project_path).unwrap();
    let project = call(
        &s,
        &configuration,
        "save_project",
        json!({
            "settings": { "name": "Managed project", "path": project_path, "sourceMode": "local" }
        }),
    )
    .await;
    let project_id = result(&project)["id"].clone();
    let projects = call(&s, &configuration, "list_projects", json!({})).await;
    assert!(
        result(&projects)["projects"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["id"] == project_id)
    );

    let connection = call(
        &s,
        &configuration,
        "create_mcp",
        json!({
            "name": "Managed connection",
            "url": "https://example.com/mcp",
            "enabled": false
        }),
    )
    .await;
    let connection_id = result(&connection)["id"].clone();
    let updated = call(
        &s,
        &configuration,
        "update_mcp",
        json!({
            "id": connection_id,
            "revision": result(&connection)["revision"],
            "connection": {
                "name": "Renamed connection",
                "url": "https://example.com/mcp",
                "enabled": false,
                "enabledTools": null,
                "allowPrivateNetwork": false,
                "transport": "http",
                "command": "",
                "args": [],
                "auth": "none",
                "clientId": "",
                "scopes": ""
            }
        }),
    )
    .await;
    assert_eq!(result(&updated)["name"], "Renamed connection");
    let connections = call(&s, &configuration, "list_mcps", json!({})).await;
    assert!(
        result(&connections)["mcps"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["id"] == connection_id)
    );
    let disconnected = call(
        &s,
        &configuration,
        "disconnect_mcp",
        json!({ "id": connection_id }),
    )
    .await;
    assert_eq!(result(&disconnected)["completed"], true);
    let deleted = call(
        &s,
        &configuration,
        "delete_mcp",
        json!({ "id": connection_id }),
    )
    .await;
    assert_eq!(result(&deleted)["completed"], true);
    let connections = call(&s, &configuration, "list_mcps", json!({})).await;
    assert!(result(&connections)["mcps"].as_array().unwrap().is_empty());

    let runs = call(&s, &configuration, "list_runs", json!({})).await;
    assert!(
        result(&runs)
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["id"] == foreign["id"])
    );
    for name in ["get_run", "read_run_content"] {
        let read = call(&s, &configuration, name, json!({ "runId": foreign["id"] })).await;
        result(&read);
    }
    let cancelled = call(
        &s,
        &configuration,
        "cancel_run",
        json!({ "runId": foreign["id"] }),
    )
    .await;
    assert_eq!(result(&cancelled)["cancelled"], true);
}

#[tokio::test]
async fn installation_access_can_save_global_and_project_skills() {
    let root = TempDir::new().unwrap();
    let s = service(&root).await;
    let manager = s
        .agent_fixture(
            json!({
                "name": "Skill author",
                "access": {
                    "installation": true,
                    "projects": [],
                    "skills": [],
                    }
            }),
            None,
        )
        .await
        .unwrap();
    let (_, configuration) = active_run(&s, &manager).await;
    let project_path = root.path().join("project");
    std::fs::create_dir(&project_path).unwrap();
    let project = s
        .project_fixture(
            json!({
                "name": "Skill project",
                "path": project_path,
                "sourceMode": "local"
            }),
            None,
        )
        .await
        .unwrap();
    let project_id = project["id"].clone();

    let content = "---\nname: independent-review\ndescription: Review until clean.\n---\n\nReview, fix, and repeat.\n";
    s.store
        .write(|db| {
            db.save_conversation_boundary(&json!({ "id": "another-device" }))?;
            db.put_message(&json!({
                "id": "skill-usage",
                "chatId": "another-device",
                "text": "$independent-review $independent-review",
                "status": "delivered",
                "createdAt": 1,
            }))?;
            Ok(())
        })
        .await
        .unwrap();
    for scope in [None, Some(project_id.clone())] {
        let mut args = json!({ "name": "independent-review", "content": content });
        let mut list = json!({});
        if let Some(project_id) = scope {
            args["scope"] = project_id.clone();
            list["scope"] = project_id;
        }
        let saved = call(&s, &configuration, "save_skill", args).await;
        result(&saved);
        let skills = call(&s, &configuration, "list_skills", list).await;
        assert!(
            result(&skills)
                .as_array()
                .unwrap()
                .iter()
                .any(|skill| skill["name"] == "independent-review"
                    && skill["content"] == content
                    && skill["usageCount"] == 1)
        );
    }

    let invalid = call(
        &s,
        &configuration,
        "save_skill",
        json!({
            "name": "invalid",
            "content": "Missing required frontmatter"
        }),
    )
    .await;
    assert_eq!(invalid["result"]["isError"], true);
    let unknown = call(&s, &configuration, "unknown_tool", json!({})).await;
    assert_eq!(unknown["result"]["isError"], true);
    assert_eq!(unknown["result"]["content"][0]["text"], "Unknown operation");
}

#[tokio::test]
async fn management_discovery_and_legacy_teardown_are_fenced_by_revocation() {
    use axum::{Json, Router, http::StatusCode, response::IntoResponse, routing::post};
    use std::{
        sync::atomic::{AtomicBool, AtomicUsize, Ordering},
        time::Duration,
    };
    use tokio::sync::Notify;

    for legacy in [false, true] {
        let root = TempDir::new().unwrap();
        let s = service(&root).await;
        let main = enabled_main(&s).await;
        let (run, configuration) = active_run(&s, &main).await;
        let started = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let blocked = Arc::new(AtomicBool::new(false));
        let deletions = Arc::new(AtomicUsize::new(0));
        let app = Router::new().route(
            "/mcp",
            post({
                let started = started.clone();
                let release = release.clone();
                let blocked = blocked.clone();
                move |Json(request): Json<Value>| {
                    let started = started.clone();
                    let release = release.clone();
                    let blocked = blocked.clone();
                    async move {
                        if legacy && request["method"] == "server/discover" {
                            return StatusCode::BAD_REQUEST.into_response();
                        }
                        if request["method"] == "notifications/initialized" {
                            return StatusCode::ACCEPTED.into_response();
                        }
                        let result = match text(&request, "method") {
                            "server/discover" => json!({
                                "supportedVersions": ["2026-07-28"],
                                "capabilities": {"tools": {}}
                            }),
                            "initialize" => json!({
                                "protocolVersion": "2025-11-25",
                                "capabilities": {"tools": {}}
                            }),
                            method => {
                                assert_eq!(method, "tools/list");
                                if blocked.load(Ordering::SeqCst) {
                                    started.notify_one();
                                    release.notified().await;
                                }
                                json!({"tools": []})
                            }
                        };
                        let mut response = Json(json!({
                            "jsonrpc": "2.0",
                            "id": request["id"],
                            "result": result
                        }))
                        .into_response();
                        if legacy {
                            response
                                .headers_mut()
                                .insert("mcp-session-id", "fixture-session".parse().unwrap());
                        }
                        response
                    }
                }
            })
            .delete({
                let deletions = deletions.clone();
                move || {
                    let deletions = deletions.clone();
                    async move {
                        deletions.fetch_add(1, Ordering::SeqCst);
                        StatusCode::NO_CONTENT
                    }
                }
            }),
        );
        let (origin, server) = common::serve_locally(app).await;
        let connection = s
            .mcp_fixture(
                json!({
                    "name":"Delayed discovery",
                    "url":format!("{origin}/mcp"),
                    "allowPrivateNetwork":true,
                    "auth":"bearer",
                    "token":"synthetic-teardown-token"
                }),
                None,
            )
            .await
            .unwrap();
        let id = connection["id"].as_str().unwrap();
        // Authorized legacy sessions still send DELETE, and modern ones remain stateless.
        s.mcps.test(&s, id).await.unwrap();
        assert_eq!(deletions.swap(0, Ordering::SeqCst), usize::from(legacy));
        let before = s.mcps.list(&s).await.unwrap();
        blocked.store(true, Ordering::SeqCst);
        let pending = tokio::spawn({
            let s = s.clone();
            let configuration = configuration.clone();
            async move {
                call(
                    &s,
                    &configuration,
                    "test_mcp",
                    json!({"id":connection["id"]}),
                )
                .await
            }
        });
        tokio::time::timeout(Duration::from_secs(10), started.notified())
            .await
            .unwrap();
        s.mcps
            .revoke_run(&s, run["id"].as_str().unwrap())
            .await
            .unwrap();
        release.notify_one();
        let response = tokio::time::timeout(Duration::from_secs(10), pending)
            .await
            .unwrap()
            .unwrap();
        server.abort();
        assert_eq!(response["result"]["isError"], true, "{response}");
        assert_eq!(
            deletions.load(Ordering::SeqCst),
            0,
            "A revoked grant must not send a remote teardown request"
        );
        assert_eq!(s.mcps.list(&s).await.unwrap(), before);
    }
}

#[tokio::test]
async fn management_oauth_refresh_cannot_save_credentials_after_revocation() {
    use axum::{Json, Router, routing::post};
    use std::time::Duration;
    use tokio::sync::Notify;

    let root = TempDir::new().unwrap();
    let s = service(&root).await;
    let main = enabled_main(&s).await;
    let (run, configuration) = active_run(&s, &main).await;
    let started = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let app = Router::new().route(
        "/token",
        post({
            let started = started.clone();
            let release = release.clone();
            move || {
                let started = started.clone();
                let release = release.clone();
                async move {
                    started.notify_one();
                    release.notified().await;
                    Json(json!({
                        "access_token": "fixture-refreshed",
                        "token_type": "Bearer",
                        "expires_in": 3600
                    }))
                }
            }
        }),
    );
    let (origin, server) = common::serve_locally(app).await;
    let connection = s
        .mcp_fixture(
            json!({
                "name": "Delayed OAuth refresh",
                "url": format!("{origin}/mcp"),
                "allowPrivateNetwork": true,
                "auth": "oauth",
                "clientId": "fixture-client"
            }),
            None,
        )
        .await
        .unwrap();
    let id = connection["id"].as_str().unwrap().to_owned();
    let credentials = json!({
        "tokens": {
            "access_token": "fixture-expired",
            "refresh_token": "fixture-refresh",
            "token_type": "Bearer"
        },
        "tokenExpiresAt": 1,
        "discovery": {
            "authorizationServerUrl": origin,
            "resourceMetadataUrl": format!("{origin}/resource"),
            "resourceMetadata": { "resource": format!("{origin}/mcp") },
            "authorizationServerMetadata": { "token_endpoint": format!("{origin}/token") }
        }
    });
    let replacement: cairn_installation::mcps::record::McpSecrets =
        serde_json::from_value(credentials.clone()).unwrap();
    s.mcps
        .update_secrets(&s, &id, |secrets| *secrets = replacement)
        .await
        .unwrap();
    let before = s.mcps.list(&s).await.unwrap();
    let pending = tokio::spawn({
        let s = s.clone();
        let configuration = configuration.clone();
        async move {
            call(
                &s,
                &configuration,
                "test_mcp",
                json!({ "id": connection["id"] }),
            )
            .await
        }
    });
    tokio::time::timeout(Duration::from_secs(10), started.notified())
        .await
        .unwrap();
    s.mcps
        .revoke_run(&s, run["id"].as_str().unwrap())
        .await
        .unwrap();
    release.notify_one();
    let response = tokio::time::timeout(Duration::from_secs(10), pending)
        .await
        .unwrap()
        .unwrap();
    server.abort();
    assert_eq!(response["result"]["isError"], true, "{response}");
    // Assert equality without printing even fixture credentials on failure.
    assert!(
        serde_json::to_value(s.mcps.server_secrets(&s, &id).await.unwrap()).unwrap() == credentials
    );
    assert_eq!(s.mcps.list(&s).await.unwrap(), before);
}

#[tokio::test]
async fn run_grants_enforce_revocation_completion_and_chat_turn_binding() {
    let root = TempDir::new().unwrap();
    let s = service(&root).await;
    let main = enabled_main(&s).await;
    let (run, configuration) = active_run(&s, &main).await;
    let token = configuration["env"]["CAIRN_MCP_RUN_TOKEN"]
        .as_str()
        .unwrap();
    let id = run["id"].as_str().unwrap();
    s.store
        .update_run_boundary_fixture(id, json!({ "chatExecution": { "messageId": "new-turn" } }))
        .await
        .unwrap();
    assert_eq!(
        request(&s, token, "tools/list", json!({})).await.0,
        StatusCode::UNAUTHORIZED
    );
    s.store
        .update_run_boundary_fixture(id, json!({ "chatExecution": null }))
        .await
        .unwrap();
    s.agent_fixture(
        json!({ "name": "Main", "access": { "installation": false } }),
        Some(MAIN_AGENT_ID),
    )
    .await
    .unwrap();
    assert_eq!(
        request(&s, token, "tools/list", json!({})).await.0,
        StatusCode::FORBIDDEN
    );
    let (status, _) = request(
        &s,
        token,
        "tools/call",
        json!({
            "name": "save_skill",
            "arguments": {
                "name": "revoked",
                "content": "---\nname: revoked\ndescription: Must not be saved.\n---\n"
            }
        }),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(!root.path().join("home/.agents/skills/revoked").exists());
    enabled_main(&s).await;
    s.store
        .change_run(
            id,
            cairn_installation::runs::RunChanges {
                status: Some(RunStatus::Succeeded),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(
        request(&s, token, "tools/list", json!({})).await.0,
        StatusCode::UNAUTHORIZED
    );
    s.store
        .change_run(
            id,
            cairn_installation::runs::RunChanges {
                status: Some(RunStatus::Running),
                cancel_requested_at: Some(Some(1)),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(
        request(&s, token, "tools/list", json!({})).await.0,
        StatusCode::UNAUTHORIZED
    );
    s.store
        .change_run(
            id,
            cairn_installation::runs::RunChanges {
                cancel_requested_at: Some(None),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    s.mcps.revoke_run(&s, id).await.unwrap();
    assert_eq!(
        request(&s, token, "tools/list", json!({})).await.0,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn workspace_grants_do_not_acquire_installation_access_and_vm_channels_bind_the_run() {
    let root = TempDir::new().unwrap();
    let s = service(&root).await;
    let main = enabled_main(&s).await;
    let (run, configuration) = active_run(&s, &main).await;
    let token = configuration["env"]["CAIRN_MCP_RUN_TOKEN"]
        .as_str()
        .unwrap();
    let key = hex_digest(token);
    let mut grant = s.store.mcp_run_grant(&key).await.unwrap().unwrap();
    grant.installation = false;
    s.store
        .transaction(move |db| db.save_mcp_run_grant(&key, &grant, None))
        .await
        .unwrap();
    assert_eq!(
        request(&s, token, "tools/list", json!({})).await.0,
        StatusCode::UNAUTHORIZED
    );

    let configuration = s.mcps.run_configuration(&s, &run).await.unwrap();
    let token = configuration["env"]["CAIRN_MCP_RUN_TOKEN"]
        .as_str()
        .unwrap();
    let home = root.path().join("channel");
    let _channel = channel::serve(&s, &home, run["id"].as_str().unwrap())
        .await
        .unwrap();
    let http = reqwest::Client::builder()
        .unix_socket(home.join("cairn-mcp.sock"))
        .build()
        .unwrap();
    let body = json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {} });
    let send = |token: &str| {
        http.post("http://localhost/mcp-installation")
            .bearer_auth(token)
            .json(&body)
    };
    let response = send(token).send().await.unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    assert!(response.json::<Value>().await.unwrap()["result"]["tools"].is_array());
    let (_, other) = active_run(&s, &main).await;
    let other_token = other["env"]["CAIRN_MCP_RUN_TOKEN"].as_str().unwrap();
    assert_eq!(
        send(other_token).send().await.unwrap().status(),
        reqwest::StatusCode::UNAUTHORIZED
    );
}
