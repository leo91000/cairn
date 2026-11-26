use super::*;

#[tokio::test]
async fn an_owner_approved_bot_can_submit_to_its_native_agent() {
    use cairn_installation::plugins::{capabilities, registry, work};

    let f = Fixture::new().await;
    f.service
        .plugins
        .use_worker(std::path::PathBuf::from(env!("CARGO_BIN_EXE_cairn")));
    let mut node = f.node.clone();
    node["nativeAccounts"] = json!([TRUSTED, "fixture-owner"]);
    f.service
        .store
        .put_node_fixture(node.clone())
        .await
        .unwrap();
    f.service.store.set("native-authority", json!({
        "observedAt": now(),
        "accounts": { "fixture-owner": { "email": "owner@example.test", "accessId": "fixture-owner" } }
    }), None).await.unwrap();
    let owner = RelayContext::new(&common::relay_fixture::context(&f.service).await);
    let input = json!({
        "name": "Native bot", "enabled": true, "agentId": f.agent["id"],
        "accessMode": "everyone", "settings": { "guildId": "100", "channelId": "200" },
        "token": "synthetic-discord-token"
    });
    let mut forged = input.clone();
    forged["nativeAuthority"] =
        json!({ "nativePrincipals": ["fixture-owner"], "nativeProvenanceUnknown": false });
    assert_eq!(
        common::save_plugin(&f.service, "discord", &forged, None)
            .await
            .unwrap_err()
            .status,
        400,
        "The typed command must not accept authority from JSON"
    );
    let denied = common::save_plugin(&f.service, "discord", &input, None)
        .await
        .unwrap_err();
    assert_eq!(
        denied.status, 403,
        "Unscoped management cannot mint a native bot grant"
    );
    let (status, saved) = f
        .call_as(
            &owner,
            "POST",
            "/api/plugins/discord/connections",
            input,
            false,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    let connection = f
        .service
        .store
        .plugin_instance(saved["id"].as_str().unwrap())
        .await
        .unwrap()
        .unwrap();
    let event = json!({
        "name": "message", "revision": connection.revision, "authorId": "external-person",
        "payload": { "id": "external-message", "author": { "id": "external-person", "name": "Person" } }
    });
    let context = capabilities::Context {
        management: None,
        connection: connection.clone(),
        manifest: registry::builtin("discord").unwrap().manifest,
        package_digest: None,
        event,
        link: None,
        draft: None,
    };
    let receipt = work::submit(
        &f.service,
        &context,
        &json!({
            "text": "Which projects can you access?", "thread": "500", "shared": true
        }),
    )
    .await
    .expect("The selected native agent must honor the owner's bot delegation");
    f.service.chat_tick(&HashSet::new()).await.unwrap();
    let chat = f
        .service
        .conversation_boundary(text(&receipt, "chatId"))
        .await
        .unwrap();
    let run = f.service.store.run(text(&chat, "runId")).await.unwrap();
    assert_eq!(run["nativePrincipals"], json!(["fixture-owner"]));
    assert_eq!(run["nativeProvenanceUnknown"], false);
    let check = run.clone();
    let target = node.clone();
    assert!(
        f.service
            .store
            .read(move |db| policy::allows(db, &check, &target))
            .await
            .unwrap()
    );

    // The current mission view must retain the plugin's revocation lineage through
    // SQL reload, replacement and the frozen launch snapshot, independently of its author.
    let mission_owner =
        auth::InstallationIdentity::trusted(auth::InstallationRole::Owner, "fixture-owner");
    let mission = f.service.task_as_fixture(json!({
        "name": "Retained bot commitment", "prompt": "Follow the approved delegation", "agentId": f.agent["id"],
        "cron": "0 9 * * *", "timezone": "UTC"
    }), None, Some(&mission_owner)).await.unwrap();
    let mut stored = f
        .service
        .store
        .mission(text(&mission, "id"))
        .await
        .unwrap()
        .unwrap();
    stored.native_plugin_grants =
        serde_json::from_value(run["nativePluginGrants"].clone()).unwrap();
    f.service
        .store
        .transaction(move |db| db.save_mission(&stored))
        .await
        .unwrap();
    let replaced = f
        .service
        .task_as_fixture(
            json!({ "prompt": "Retain the approved delegation after editing" }),
            Some(text(&mission, "id")),
            Some(&mission_owner),
        )
        .await
        .unwrap();
    assert_eq!(replaced["nativePluginGrants"], run["nativePluginGrants"]);
    let restarted = Service::new(f.service.config.clone()).await.unwrap();
    assert_eq!(
        serde_json::to_value(
            restarted
                .store
                .mission(text(&mission, "id"))
                .await
                .unwrap()
                .unwrap()
        )
        .unwrap(),
        replaced
    );
    let queued = f
        .service
        .enqueue(text(&mission, "id"), "schedule", None)
        .await
        .unwrap();
    assert_eq!(queued["nativePluginGrants"], run["nativePluginGrants"]);
    assert_eq!(queued["snapshot"]["task"], replaced);

    let original = serde_json::to_value(&connection).unwrap();
    for (label, field, value) in [
        ("disabled", "enabled", json!(false)),
        ("replaced", "revision", json!(id())),
        ("person removed", "accessMode", json!("allowlist")),
        ("owner changed", "ownerAccessId", json!("different-owner")),
        (
            "agent policy revoked",
            "grantedAccess",
            json!({ "backend": "firecracker" }),
        ),
    ] {
        let mut changed = original.clone();
        changed[field] = value;
        f.service.store.put_instance_fixture(changed).await.unwrap();
        let (check, target) = (run.clone(), node.clone());
        assert!(
            !f.service
                .store
                .read(move |db| policy::allows(db, &check, &target))
                .await
                .unwrap(),
            "{label}"
        );
        let (check, target) = (queued.clone(), node.clone());
        assert!(
            !f.service
                .store
                .read(move |db| policy::allows(db, &check, &target))
                .await
                .unwrap(),
            "Mission lineage: {label}"
        );
        f.service
            .store
            .put_instance_fixture(original.clone())
            .await
            .unwrap();
    }

    f.service.store.set("native-authority", json!({
        "observedAt": now(), "accounts": {
            "fixture-owner": { "email": "owner@example.test", "accessId": "replacement-access" }
        }
    }), None).await.unwrap();
    let (check, target) = (run.clone(), node.clone());
    assert!(
        !f.service
            .store
            .read(move |db| policy::allows(db, &check, &target))
            .await
            .unwrap(),
        "Fresh membership must match the owner access that approved the delegation"
    );

    common::enable_plugin(
        &f.service,
        "discord",
        &connection.id,
        &json!({ "enabled": false }),
    )
    .await
    .unwrap();
    let paused = f
        .service
        .task_as_fixture(
            json!({ "enabled": false }),
            Some(text(&mission, "id")),
            Some(&mission_owner),
        )
        .await
        .unwrap();
    assert_eq!(paused["nativePluginGrants"], replaced["nativePluginGrants"]);
    assert_eq!(
        f.service.store.run(text(&queued, "id")).await.unwrap()["snapshot"]["task"],
        replaced
    );
    let denied = f
        .service
        .task_as_fixture(
            json!({ "enabled": true }),
            Some(text(&mission, "id")),
            Some(&mission_owner),
        )
        .await
        .unwrap_err();
    assert_eq!(denied.status, 403);
    assert!(
        !f.service
            .store
            .read(move |db| policy::allows(db, &run, &node))
            .await
            .unwrap(),
        "Disabling the bot must revoke its native work, including already queued work"
    );
}

#[tokio::test]
async fn native_administrator_requires_owner_review_verified_generation_and_explicit_admission() {
    for backend in ["native", "auto"] {
        administrator_admission(backend).await;
    }
}

#[tokio::test]
async fn native_mode_preview_detects_concurrent_access_changes_and_agent_requests_need_owner() {
    let f = Fixture::new().await;
    let node_id = text(&f.node, "id");
    let path = format!("/api/nodes/{node_id}/native-mode");
    let mut node = f.node.clone();
    node["capabilities"]["nativeModeProtocol"] = 1.into();
    f.service
        .store
        .put_node_fixture(node.clone())
        .await
        .unwrap();
    let (_, preview) = f
        .owner_call("POST", &path, json!({ "mode": "administrator" }))
        .await;
    node["nativeAccounts"] = json!([TRUSTED, OTHER]);
    f.service.store.put_node_fixture(node).await.unwrap();
    let (status, _) = f
        .owner_call(
            "POST",
            &path,
            json!({
                "mode": "administrator", "previewId": preview["previewId"], "confirm": true,
            }),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    let preview = cairn_installation::nodes::native_mode::change(
        &f.service,
        node_id,
        json!({ "mode": "administrator" }),
        |_| Ok(()),
    )
    .await
    .unwrap();
    let proposal = cairn_installation::nodes::native_mode::change(
        &f.service,
        node_id,
        json!({
            "mode": "administrator", "previewId": preview["previewId"], "confirm": true,
        }),
        |_| Ok(()),
    )
    .await
    .unwrap();
    assert_eq!(proposal["status"], "awaiting-owner");
    assert!(f.service.node_boundary(node_id).await.unwrap()["nativeModeRequest"].is_null());
    // The owner reviews again through the same API and can then apply the request.
    let (_, preview) = f
        .owner_call("POST", &path, json!({ "mode": "administrator" }))
        .await;
    let (status, changed) = f
        .owner_call(
            "POST",
            &path,
            json!({
                "mode": "administrator", "previewId": preview["previewId"], "confirm": true,
            }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert!(changed["node"]["nativeModeProposal"].is_null());
}

#[tokio::test]
async fn converting_a_mission_retains_native_authority_and_rejects_unauthorized_callers() {
    let f = Fixture::new().await;
    let mut run = f.run();
    run["status"] = "succeeded".into();
    run["sessionId"] = "saved-session".into();
    run["projectId"] = Value::Null;
    run["snapshot"]["task"] =
        json!({ "name": "Native mission", "prompt": "Inspect the workspace" });
    let run_id = run["id"].as_str().unwrap().to_owned();
    let saved = run.clone();
    f.service
        .store
        .transaction(move |db| {
            db.add_run_fixture(&saved, None)?;
            db.save_checkpoint_boundary(saved["id"].as_str().unwrap(), &json!({ "prepared": {} }))
        })
        .await
        .unwrap();
    let run = f.service.store.run(&run_id).await.unwrap();
    let path = format!("/api/runs/{run_id}/chat");
    let (status, _) = f.call(&f.other, "POST", &path, json!({})).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(f.service.chat_list().await.unwrap().is_empty());
    assert_eq!(f.service.store.run(&run_id).await.unwrap(), run);
    let (status, chat) = f.call(&f.trusted, "POST", &path, json!({})).await;
    assert_eq!(status, StatusCode::OK, "{chat}");
    assert!(chat.get("nativePrincipals").is_none());
    let chat = f
        .service
        .conversation_boundary(text(&chat, "id"))
        .await
        .unwrap();
    assert_eq!(chat["nativePrincipals"], json!([TRUSTED]));
    assert_eq!(chat["nativeAuthorityNodes"], json!([f.node["id"]]));
    assert_eq!(chat["nativeProvenanceUnknown"], false);
}

#[tokio::test]
async fn public_mcp_preserves_the_trusted_native_caller_and_automatic_priority() {
    for backend in ["native", "auto"] {
        let mut f = Fixture::new().await;
        let vm = f.vm_node().await;
        let nodes = if backend == "auto" {
            json!([f.node["id"], vm["id"]])
        } else {
            json!([f.node["id"]])
        };
        f.agent = f
            .service
            .agent_fixture(
                json!({
                    "access": { "backend": backend, "nodes": nodes },
                    "nodePriorities": nodes,
                }),
                f.agent["id"].as_str(),
            )
            .await
            .unwrap();
        let response = public_mcp_call(
            &f,
            TRUSTED,
            &["manage"],
            "create_chat",
            json!({ "agentId": f.agent["id"], "projectId": null }),
        )
        .await;
        assert_ne!(response["result"]["isError"], true, "{backend}: {response}");
        let public_chat = &response["result"]["structuredContent"]["result"];
        assert!(public_chat.get("nativePrincipals").is_none());
        let stored = f
            .service
            .conversation_boundary(text(public_chat, "id"))
            .await
            .unwrap();
        let chat = &stored;
        assert_eq!(
            chat["nativePrincipals"],
            json!([TRUSTED]),
            "{backend}: {chat}"
        );
        assert_eq!(chat["nativeAuthorityNodes"], json!([f.node["id"]]));
        assert_eq!(chat["nativeProvenanceUnknown"], false);

        let response = public_mcp_call(
            &f, TRUSTED, &["run"], "send_chat_message",
            json!({ "chatId": chat["id"], "message": { "id": id(), "text": "Continue the authorized work" } }),
        ).await;
        assert_ne!(response["result"]["isError"], true, "{backend}: {response}");
        f.service.chat_tick(&HashSet::new()).await.unwrap();
        let chat = f
            .service
            .conversation_boundary(text(chat, "id"))
            .await
            .unwrap();
        let run = f.service.store.run(text(&chat, "runId")).await.unwrap();
        assert_eq!(run["nativePrincipals"], json!([TRUSTED]));
        assert_eq!(run["nativeProvenanceUnknown"], false);
        let placed = placement::reserve(&f.service, &run, &id()).await.unwrap();
        assert_eq!(placed["nodeId"], f.node["id"], "{backend}: {placed}");
    }
}

#[tokio::test]
async fn public_mcp_native_execution_keeps_caller_and_oauth_scope_restrictions() {
    let f = Fixture::new().await;
    for (account, scopes) in [(OTHER, vec!["manage"]), (TRUSTED, vec!["read"])] {
        let response = public_mcp_call(
            &f,
            account,
            &scopes,
            "create_chat",
            json!({ "agentId": f.agent["id"], "projectId": null }),
        )
        .await;
        assert_eq!(response["result"]["isError"], true, "{response}");
        assert!(f.service.chat_list().await.unwrap().is_empty());
    }
}

#[tokio::test]
async fn public_mcp_rechecks_the_native_caller_when_directing_existing_work() {
    let f = Fixture::new().await;
    let (status, chat) = f
        .call(
            &f.trusted,
            "POST",
            "/api/chats",
            json!({ "agentId": f.agent["id"] }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{chat}");
    let before = f
        .service
        .conversation_boundary(text(&chat, "id"))
        .await
        .unwrap();
    let args = json!({
        "chatId": chat["id"],
        "message": { "id": id(), "text": "Continue the authorized work" },
    });
    for (account, scopes) in [(OTHER, vec!["run"]), (TRUSTED, vec!["manage"])] {
        let response =
            public_mcp_call(&f, account, &scopes, "send_chat_message", args.clone()).await;
        assert_eq!(response["result"]["isError"], true, "{response}");
    }
    let mut node = f.node.clone();
    node["nativeAccounts"] = json!([]);
    f.service.store.put_node_fixture(node).await.unwrap();
    let response = public_mcp_call(&f, TRUSTED, &["run"], "send_chat_message", args).await;
    assert_eq!(response["result"]["isError"], true, "{response}");
    let saved = f
        .service
        .conversation_boundary(text(&chat, "id"))
        .await
        .unwrap();
    assert_eq!(
        saved, before,
        "Rejected calls must not change the conversation"
    );
}

#[tokio::test]
async fn automatic_placement_uses_agent_priorities_and_falls_back_without_converting_existing_work()
{
    use cairn_installation::nodes::LOCAL_NODE_ID;

    for scenario in [
        "preferred",
        "full",
        "untrusted",
        "offline",
        "existing-vm",
        "preferred-vm",
        "no-controller",
    ] {
        let mut f = Fixture::new().await;
        let vm = f.vm_node().await;
        let vm_id = if scenario == "no-controller" {
            LOCAL_NODE_ID
        } else {
            text(&vm, "id")
        };
        f.agent = f
            .service
            .agent_fixture(
                json!({
                    "access": { "backend": "auto", "nodes": [f.node["id"], vm_id] },
                    "nodePriorities": if scenario == "no-controller" { json!([vm_id, f.node["id"]]) } else { json!([f.node["id"], vm_id]) },
                }),
                f.agent["id"].as_str(),
            )
            .await
            .unwrap();
        let persisted = f
            .service
            .agent_snapshot(text(&f.agent, "id"))
            .await
            .unwrap();
        assert_eq!(persisted["nodePriorities"], f.agent["nodePriorities"]);

        let mut run = f.run();
        match scenario {
            "full" => {
                f.service
                    .store
                    .put_attempt_fixture(json!({
                        "id": id(), "runId": id(), "nodeId": f.node["id"], "role": "execution",
                        "released": false, "leaseExpiresAt": now() + 60_000,
                        "resources": { "cpu": 2, "memoryMiB": 4096, "diskMiB": 32768 },
                    }))
                    .await
                    .unwrap();
            }
            "offline" => {
                f.node["lastSeen"] = 0.into();
            }
            "untrusted" => {
                run["nativeProvenanceUnknown"] = true.into();
            }
            "existing-vm" => {
                run["snapshot"]["agent"]["access"]["backend"] = "firecracker".into();
            }
            "preferred-vm" => {
                run["preferredNodeId"] = vm_id.into();
            }
            _ => {}
        }
        f.service
            .store
            .put_node_fixture(f.node.clone())
            .await
            .unwrap();
        common::add_run(&f.service.store, &run).await;
        let attempt = id();
        let placement = placement::reserve(&f.service, &run, &attempt)
            .await
            .unwrap();
        let expected = if matches!(scenario, "preferred" | "no-controller") {
            text(&f.node, "id")
        } else {
            vm_id
        };
        assert_eq!(placement["nodeId"], expected, "{scenario}");
        let saved = f.service.store.run(text(&run, "id")).await.unwrap();
        let backend = if matches!(scenario, "preferred" | "no-controller") {
            "native"
        } else {
            "firecracker"
        };
        assert_eq!(
            saved["snapshot"]["agent"]["access"]["backend"], backend,
            "{scenario}"
        );
        if scenario == "existing-vm" {
            f.service
                .store
                .update_run_boundary_fixture(text(&saved, "id"), json!({ "nodeId": vm_id }))
                .await
                .unwrap();
        }
        placement::release(&f.service, &attempt).await.unwrap();

        if scenario == "preferred" {
            f.node["accepting"] = false.into();
            f.service
                .store
                .put_node_fixture(f.node.clone())
                .await
                .unwrap();
            assert!(placement::is_no_capacity(
                &placement::reserve(&f.service, &run, &id())
                    .await
                    .unwrap_err()
            ));
        }
    }
}

#[tokio::test]
async fn priorities_never_authorize_an_ungranted_node() {
    let f = Fixture::new().await;
    let error = f
        .service
        .agent_fixture(
            json!({
                "access": { "backend": "auto", "nodes": [f.node["id"]] },
                "nodePriorities": [cairn_installation::nodes::LOCAL_NODE_ID],
            }),
            f.agent["id"].as_str(),
        )
        .await
        .unwrap_err();
    assert_eq!(error.status, 400);
    assert!(error.message.contains("Only authorized nodes"));
    let saved = f
        .service
        .agent_snapshot(text(&f.agent, "id"))
        .await
        .unwrap();
    assert_eq!(saved["access"]["backend"], "native");
}

#[tokio::test]
async fn automatic_profiles_offer_optional_conversion_to_the_other_environment() {
    let mut f = Fixture::new().await;
    let vm = f.vm_node().await;
    f.node["nativeAccounts"] = json!([TRUSTED, OTHER]);
    f.service
        .store
        .put_node_fixture(f.node.clone())
        .await
        .unwrap();
    f.agent = f
        .service
        .agent_fixture(
            json!({ "access": { "backend": "auto", "nodes": [f.node["id"], vm["id"]] } }),
            f.agent["id"].as_str(),
        )
        .await
        .unwrap();
    let (_, run) = f.conversation().await;
    for from in ["firecracker", "native"] {
        let mut snapshot = run["snapshot"].clone();
        snapshot["agent"]["access"]["backend"] = from.into();
        f.service
            .store
            .change_run(
                text(&run, "id"),
                cairn_installation::runs::RunChanges {
                    snapshot: Some(serde_json::from_value(snapshot).unwrap()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let (status, preview) = f
            .owner_call(
                "GET",
                &format!(
                    "/api/nodes/placement/{}/backend-migration",
                    text(&run, "id")
                ),
                Value::Null,
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{preview}");
        assert_eq!(preview["fromBackend"], from);
        assert_eq!(preview["required"], false);
        assert_eq!(preview["optional"], true);
        let (to, node) = if from == "native" {
            ("firecracker", &vm["id"])
        } else {
            ("native", &f.node["id"])
        };
        assert_eq!(preview["toBackend"], to);
        assert!(preview["reason"].is_null(), "{preview}");
        assert!(
            preview["nodes"]
                .as_array()
                .unwrap()
                .iter()
                .any(|candidate| &candidate["id"] == node),
            "{preview}"
        );
    }
}

#[tokio::test]
async fn expired_conversations_explicitly_change_backend_without_recovering_deleted_disks() {
    for destination_backend in ["native", "firecracker"] {
        let f = Fixture::new().await;
        let vm = f.vm_node().await;
        let (chat, run) = f.conversation().await;
        let chat_id = text(&chat, "id").to_owned();
        let run_id = text(&run, "id").to_owned();
        let from = if destination_backend == "native" {
            "firecracker"
        } else {
            "native"
        };
        let mut snapshot = run["snapshot"].clone();
        snapshot["agent"]["access"]["backend"] = from.into();
        let (cid, rid) = (chat_id.clone(), run_id.clone());
        f.service
            .store
            .transaction(move |db| {
                for mut message in db.messages(&cid)? {
                    message["status"] = "delivered".into();
                    db.put_message(&message)?;
                }
                db.change_run(
                    &rid,
                    cairn_installation::runs::RunChanges {
                        status: Some(cairn_installation::run_status::RunStatus::Succeeded),
                        snapshot: Some(serde_json::from_value(snapshot).unwrap()),
                        recovery_pending: Some(false),
                        placement: Some(Box::new(cairn_installation::nodes::run_placement::PlacementChanges {node_id: Some(None), ..Default::default() })), boundary: Some(serde_json::json!({"environmentState": "expired","environmentExpiredAt": now(),"isolated": false})),
                        ..Default::default()
                    },
                )?;
                db.save_checkpoint_boundary(&rid, &json!({ "completed": true }))?;
                let mut chat = db.conversation_boundary(&cid)?.unwrap();
                chat["environmentState"] = "expired".into();
                chat["environmentExpiredAt"] = now().into();
                chat["paused"] = true.into();
                db.save_conversation_boundary(&chat)?;
                Ok(())
            })
            .await
            .unwrap();
        let destination = if destination_backend == "native" {
            &f.node
        } else {
            &vm
        };
        let mut preferred = destination.clone();
        preferred["id"] = id().into();
        preferred["nativeAccounts"] = json!([TRUSTED, OTHER]);
        f.service
            .store
            .put_node_fixture(preferred.clone())
            .await
            .unwrap();
        f.service.agent_fixture(json!({
            "access": { "backend": destination_backend, "nodes": [preferred["id"], destination["id"]] },
            "nodePriorities": [preferred["id"], destination["id"]],
        }), f.agent["id"].as_str()).await.unwrap();
        let path = format!("/api/nodes/placement/{run_id}/backend-migration");
        let input =
            json!({ "nodeId": destination["id"], "backend": destination_backend, "confirm": true });
        let (status, preview) = f.owner_call("GET", &path, Value::Null).await;
        assert_eq!(status, StatusCode::OK, "{preview}");
        assert_eq!(preview["environmentExpired"], true);
        assert_eq!(preview["required"], true);
        // Only the trusted caller has native authority; management cannot invent it.
        if destination_backend == "native" {
            assert!(
                !preview["nodes"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|node| node["id"] == destination["id"])
            );
            assert_eq!(
                f.owner_call("POST", &path, input.clone()).await.0,
                StatusCode::FORBIDDEN
            );
            let mut node = f.node.clone();
            node["nativeAccounts"] = json!([TRUSTED, OTHER]);
            f.service.store.put_node_fixture(node).await.unwrap();
        }
        let mut unconfirmed = input.clone();
        unconfirmed["confirm"] = false.into();
        assert_eq!(
            f.owner_call("POST", &path, unconfirmed).await.0,
            StatusCode::BAD_REQUEST
        );
        // Expiration does not bypass the existing restriction on other reduced access.
        let before = f.service.store.run(&run_id).await.unwrap();
        let mut broader = before["snapshot"].clone();
        broader["agent"]["access"]["installation"] = true.into();
        f.service
            .store
            .change_run(
                &run_id,
                cairn_installation::runs::RunChanges {
                    snapshot: Some(serde_json::from_value(broader).unwrap()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let (status, denied) = f.owner_call("POST", &path, input.clone()).await;
        assert_eq!(status, StatusCode::CONFLICT, "{denied}");
        assert!(text(&denied, "error").contains("Other agent access was reduced"));
        f.service
            .store
            .change_run(
                &run_id,
                cairn_installation::runs::RunChanges {
                    snapshot: Some(serde_json::from_value(before["snapshot"].clone()).unwrap()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let (status, result) = f.owner_call("POST", &path, input).await;
        assert_eq!(status, StatusCode::OK, "{result}");
        let migrated = f.service.store.run(&run_id).await.unwrap();
        assert_eq!(
            migrated["snapshot"]["agent"]["access"]["backend"],
            destination_backend
        );
        assert!(migrated["nodeId"].is_null());
        assert!(migrated["pinnedNodeId"].is_null());
        assert_eq!(migrated["targetNodeId"], destination["id"]);
        assert!(migrated["backup"].is_null());
        assert!(migrated["backendMigration"].is_null());
        assert!(migrated["sessionId"].is_null());
        let checkpoint = f
            .service
            .store
            .checkpoint_boundary(&run_id)
            .await
            .unwrap()
            .unwrap();
        assert!(checkpoint["prepared"].is_null());
        assert!(migrated["pinnedNodeId"].is_null());
        // No disk has been created, so the existing preference control can
        // change this choice without trying to transfer a deleted environment.
        let placement_path = format!("/api/nodes/placement/{run_id}");
        for selected in [&preferred, destination] {
            let (status, selection) = f
                .owner_call(
                    "PUT",
                    &placement_path,
                    json!({"selection":{
                        "pinnedNodeId": selected["id"],
                        "preferredNodeId": null,
                    }}),
                )
                .await;
            assert_eq!(status, StatusCode::OK, "{selection}");
            assert_eq!(selection["pinnedNodeId"], selected["id"]);
        }
        assert_eq!(
            f.service.conversation_boundary(&chat_id).await.unwrap()["paused"],
            true
        );
        assert_eq!(
            f.owner_call(
                "POST",
                &format!("/api/chats/{chat_id}/pause"),
                json!({ "paused": false })
            )
            .await
            .0,
            StatusCode::OK
        );
        let (status, message) = f
            .owner_call(
                "POST",
                &format!("/api/chats/{chat_id}/messages"),
                json!({ "id": id(), "text": "Continue in a fresh environment" }),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{message}");
        f.service.chat_tick(&HashSet::new()).await.unwrap();
        let continued = f.service.store.run(&run_id).await.unwrap();
        assert_eq!(continued["status"], "queued");
        assert_eq!(continued["environmentState"], "expired");
        assert_eq!(
            continued["snapshot"]["agent"]["access"]["backend"],
            destination_backend
        );
        assert_eq!(
            f.service.conversation_boundary(&chat_id).await.unwrap()["runId"],
            run_id
        );
        let admission = placement::reserve(&f.service, &continued, &id())
            .await
            .unwrap();
        assert_eq!(
            admission["nodeId"], destination["id"],
            "An explicitly confirmed destination must win over automatic priorities"
        );
    }
}

#[tokio::test]
async fn native_authority_outage_preserves_automatic_vm_admission_and_vm_continuations() {
    use std::os::unix::fs::PermissionsExt;
    use std::sync::atomic::{AtomicUsize, Ordering};

    for mixed in [false, true] {
        let mut f = Fixture::new().await;
        let vm = f.vm_node().await;
        let nodes = if mixed {
            json!([vm["id"], f.node["id"]])
        } else {
            json!([vm["id"]])
        };
        f.agent = f
            .service
            .agent_fixture(
                json!({ "access": { "backend": "auto", "nodes": nodes } }),
                f.agent["id"].as_str(),
            )
            .await
            .unwrap();
        let (listener, address) = common::bind().await;
        let native_requests = Arc::new(AtomicUsize::new(0));
        let observed_native = native_requests.clone();
        let membership_requests = Arc::new(AtomicUsize::new(0));
        let observed_membership = membership_requests.clone();
        let memberships = cairn_protocol::TaskAuthorPolicy {
            owner: cairn_protocol::TaskAuthorGrant {
                account_id: "fixture-owner".into(),
                access_id: "fixture-owner".into(),
            },
            members: [(TRUSTED, "one"), (OTHER, "two")]
                .into_iter()
                .map(|(account, access)| cairn_protocol::TaskAuthorGrant {
                    account_id: account.into(),
                    access_id: access.into(),
                })
                .collect(),
        };
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new()
                    .route(
                        "/api/relay/{installation}/task-authors",
                        axum::routing::get(move || {
                            observed_membership.fetch_add(1, Ordering::SeqCst);
                            let memberships = memberships.clone();
                            async move { axum::Json(memberships) }
                        }),
                    )
                    .fallback(move || {
                        observed_native.fetch_add(1, Ordering::SeqCst);
                        async { StatusCode::SERVICE_UNAVAILABLE }
                    }),
            )
            .await
            .unwrap();
        });
        // Directing writes still confirm current memberships; only native authority
        // is unavailable, independently of the account-access verification.
        members_use_every_agent(&f.service).await;
        let identity_dir = f.service.config.data_dir.join("installation-relay");
        std::fs::create_dir_all(&identity_dir).unwrap();
        let identity = identity_dir.join("identity.json");
        std::fs::write(&identity, json!({ "origin": format!("http://{address}"), "installationId": id(), "token": "synthetic-native-authority-test" }).to_string()).unwrap();
        std::fs::set_permissions(identity, std::fs::Permissions::from_mode(0o600)).unwrap();
        f.service.store.set("native-authority", json!({ "observedAt": now() - 30_000, "accounts": { TRUSTED: { "email": "trusted@example.test", "accessId": "one" } } }), None).await.unwrap();

        let (chat, run) = f.conversation().await;
        assert!(membership_requests.load(Ordering::SeqCst) > 0);
        assert_eq!(native_requests.load(Ordering::SeqCst) > 0, mixed);
        let placement = placement::reserve(&f.service, &run, &id()).await.unwrap();
        assert_eq!(placement["nodeId"], vm["id"]);
        let before = native_requests.load(Ordering::SeqCst);
        policy::refresh_chat(&f.service, text(&chat, "id"))
            .await
            .unwrap();
        assert_eq!(
            native_requests.load(Ordering::SeqCst),
            before,
            "A concrete VM never requires native authority"
        );
        let membership_before = membership_requests.load(Ordering::SeqCst);
        let (status, body) = f
            .call(
                &f.trusted,
                "POST",
                &format!("/api/chats/{}/messages", text(&chat, "id")),
                json!({ "id": id(), "text": "Continue in this VM" }),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(membership_requests.load(Ordering::SeqCst) > membership_before);
        assert_eq!(native_requests.load(Ordering::SeqCst), before);

        let mut saved = f.service.store.run(text(&run, "id")).await.unwrap();
        saved["snapshot"]["agent"]["access"]["backend"] = "native".into();
        f.service
            .store
            .change_run(
                text(&run, "id"),
                cairn_installation::runs::RunChanges {
                    snapshot: Some(serde_json::from_value(saved["snapshot"].clone()).unwrap()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert!(
            policy::refresh_chat(&f.service, text(&chat, "id"))
                .await
                .is_err(),
            "A concrete native environment still requires fresh authority"
        );
        assert!(native_requests.load(Ordering::SeqCst) > before);
        server.abort();
    }
}

#[tokio::test]
async fn automatic_vm_only_bot_delegation_needs_no_native_authority() {
    let mut f = Fixture::new().await;
    f.service
        .plugins
        .use_worker(std::path::PathBuf::from(env!("CARGO_BIN_EXE_cairn")));
    f.agent = f.service.agent_fixture(json!({ "access": { "backend": "auto", "nodes": [cairn_installation::nodes::LOCAL_NODE_ID] } }), f.agent["id"].as_str()).await.unwrap();
    f.service
        .store
        .set(
            "native-authority",
            json!({ "observedAt": 0, "accounts": {} }),
            None,
        )
        .await
        .unwrap();
    let owner = RelayContext::new(&common::relay_fixture::context(&f.service).await);
    let (status, saved) = f
        .call_as(
            &owner,
            "POST",
            "/api/plugins/discord/connections",
            json!({
                "name": "Automatic VM bot", "enabled": true, "agentId": f.agent["id"],
                "accessMode": "everyone", "settings": { "guildId": "100", "channelId": "200" },
                "token": "synthetic-discord-token"
            }),
            false,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    let connection = f
        .service
        .store
        .plugin_instance(text(&saved, "id"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(connection.granted_access.unwrap().backend, "auto");
    assert!(connection.native_authority.is_none());
}
