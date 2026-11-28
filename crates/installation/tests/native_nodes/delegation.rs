use super::*;

#[tokio::test]
async fn a_vm_coordinator_can_delegate_only_its_traced_native_authority() {
    let f = Fixture::new().await;
    let (parent, response) = delegate(&f, true).await;
    assert_ne!(response["result"]["isError"], true, "{response}");
    let public_child = &response["result"]["structuredContent"]["result"];
    assert!(public_child.get("nativeParentRunId").is_none());
    let child = f
        .service
        .conversation_boundary(text(public_child, "id"))
        .await
        .unwrap();
    assert_eq!(child["nativeParentRunId"], parent["id"], "{response}");
    assert_eq!(child["nativeAuthorityNodes"], json!([f.node["id"]]));
    let mut run = f.run();
    run["nativeParentRunId"] = parent["id"].clone();
    let node = f.node.clone();
    let valid = run.clone();
    assert!(
        f.service
            .store
            .read(move |db| policy::allows(db, &valid, &node))
            .await
            .unwrap()
    );
    f.service
        .store
        .change_run(
            parent["id"].as_str().unwrap(),
            cairn_installation::runs::RunChanges {
                cancel_requested_at: Some(Some(now())),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let node = f.node.clone();
    assert!(
        !f.service
            .store
            .read(move |db| policy::allows(db, &run, &node))
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn unattributed_vm_work_cannot_acquire_native_access_through_delegation() {
    let f = Fixture::new().await;
    let (_, response) = delegate(&f, false).await;
    assert_eq!(response["result"]["isError"], true, "{response}");
}

#[tokio::test]
async fn parent_node_grant_reductions_revoke_existing_native_delegation() {
    let f = Fixture::new().await;
    let vm_nodes = [id(), id()];
    for node_id in &vm_nodes {
        let mut node = f.node.clone();
        node["id"] = node_id.clone().into();
        node["capabilities"]["backend"] = "firecracker".into();
        node["capabilities"]["kvm"] = true.into();
        f.service.store.put_node_fixture(node).await.unwrap();
    }
    let agent = f
        .service
        .agent_fixture(
            json!({
                "name": "Bounded coordinator", "access": { "installation": true, "nodes": vm_nodes }
            }),
            None,
        )
        .await
        .unwrap();
    let mut parent = f.run();
    parent["snapshot"]["agent"] = agent.clone();
    parent["status"] = "running".into();
    add_conversation_run_fixture(&f.service.store, &mut parent).await;
    let configuration = f
        .service
        .mcps
        .run_configuration(&f.service, &parent)
        .await
        .unwrap();
    let response = delegated_call(
        &f,
        &configuration,
        "create_chat",
        json!({ "agentId": f.agent["id"] }),
    )
    .await;
    assert_ne!(response["result"]["isError"], true, "{response}");
    let chat_id = response["result"]["structuredContent"]["result"]["id"]
        .as_str()
        .unwrap();
    let response = delegated_call(
        &f,
        &configuration,
        "send_chat_message",
        json!({
            "chatId": chat_id, "message": { "id": id(), "text": "Run native work" }
        }),
    )
    .await;
    assert_ne!(response["result"]["isError"], true, "{response}");
    f.service.chat_tick(&HashSet::new()).await.unwrap();
    let chat = f.service.conversation_boundary(chat_id).await.unwrap();
    let child = f
        .service
        .store
        .run(chat["runId"].as_str().unwrap())
        .await
        .unwrap();
    assert_eq!(child["nativeParentRunId"], parent["id"]);

    for (nodes, retains_grant) in [
        (json!(vm_nodes), true),
        (json!([vm_nodes[0], vm_nodes[1], f.node["id"]]), true),
        (json!([vm_nodes[0]]), false),
        (json!([vm_nodes[0], f.node["id"]]), false),
        (json!([]), false),
    ] {
        let mut current = agent.clone();
        current["access"]["nodes"] = nodes.clone();
        f.service.store.put_agent_fixture(current).await.unwrap();
        let (run, node) = (child.clone(), f.node.clone());
        let existing_allowed = f
            .service
            .store
            .read(move |db| policy::allows(db, &run, &node))
            .await
            .unwrap();
        assert_eq!(existing_allowed, retains_grant, "parent nodes {nodes}");
    }
}

#[tokio::test]
async fn a_vm_coordinator_directed_by_unrelated_parents_loses_native_delegation() {
    let f = Fixture::new().await;
    let (parent_a, _) = delegate(&f, true).await;
    let parent_b_agent = f
        .service
        .agent_fixture(
            json!({
                "name": "Other VM director", "access": { "installation": true }
            }),
            None,
        )
        .await
        .unwrap();
    let mut parent_b = parent_a.clone();
    parent_b["id"] = id().into();
    parent_b["taskId"] = id().into();
    parent_b["snapshot"]["agent"] = parent_b_agent.clone();
    add_conversation_run_fixture(&f.service.store, &mut parent_b).await;
    let coordinator = f
        .service
        .agent_fixture(
            json!({
                "name": "Shared VM coordinator", "access": { "installation": true }
            }),
            None,
        )
        .await
        .unwrap();
    let a = f
        .service
        .mcps
        .run_configuration(&f.service, &parent_a)
        .await
        .unwrap();
    let b = f
        .service
        .mcps
        .run_configuration(&f.service, &parent_b)
        .await
        .unwrap();
    let created = delegated_call(
        &f,
        &a,
        "create_chat",
        json!({ "agentId": coordinator["id"] }),
    )
    .await;
    assert_ne!(created["result"]["isError"], true, "{created}");
    let chat_id = created["result"]["structuredContent"]["result"]["id"].clone();
    let sent = delegated_call(
        &f,
        &a,
        "send_chat_message",
        json!({
            "chatId": chat_id, "message": { "id": id(), "text": "Coordinate trusted work" }
        }),
    )
    .await;
    assert_ne!(sent["result"]["isError"], true, "{sent}");
    f.service.chat_tick(&HashSet::new()).await.unwrap();
    let chat = f
        .service
        .conversation_boundary(chat_id.as_str().unwrap())
        .await
        .unwrap();
    let coordinator_run = f
        .service
        .store
        .run(chat["runId"].as_str().unwrap())
        .await
        .unwrap();
    let coordinator_run = f
        .service
        .store
        .change_run(
            coordinator_run["id"].as_str().unwrap(),
            cairn_installation::runs::RunChanges {
                status: Some(cairn_contracts::RunStatus::Running),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let coordinator_run = f.service.store.run(&coordinator_run.id).await.unwrap();
    let c = f
        .service
        .mcps
        .run_configuration(&f.service, &coordinator_run)
        .await
        .unwrap();
    let child = delegated_call(&f, &c, "create_chat", json!({ "agentId": f.agent["id"] })).await;
    assert_ne!(child["result"]["isError"], true, "{child}");
    let child_id = child["result"]["structuredContent"]["result"]["id"].clone();
    let sent = delegated_call(
        &f,
        &c,
        "send_chat_message",
        json!({
            "chatId": child_id, "message": { "id": id(), "text": "Use the native node" }
        }),
    )
    .await;
    assert_ne!(sent["result"]["isError"], true, "{sent}");
    f.service.chat_tick(&HashSet::new()).await.unwrap();
    let child_chat = f
        .service
        .conversation_boundary(child_id.as_str().unwrap())
        .await
        .unwrap();
    let existing_native = f
        .service
        .store
        .run(child_chat["runId"].as_str().unwrap())
        .await
        .unwrap();
    assert_eq!(existing_native["nativeParentRunId"], coordinator_run["id"]);
    let valid = existing_native.clone();
    let node = f.node.clone();
    assert!(
        f.service
            .store
            .read(move |db| policy::allows(db, &valid, &node))
            .await
            .unwrap()
    );

    let directed = delegated_call(
        &f,
        &b,
        "send_chat_message",
        json!({
            "chatId": chat_id, "message": { "id": id(), "text": "Also act on my instructions" }
        }),
    )
    .await;
    assert_ne!(directed["result"]["isError"], true, "{directed}");
    let updated = f
        .service
        .store
        .run(chat["runId"].as_str().unwrap())
        .await
        .unwrap();
    assert_eq!(updated["nativeProvenanceUnknown"], true);

    for reduction in [
        json!({ "cancelRequestedAt": now() }),
        json!({ "cancelRequestedAt": null, "nativeAuthorityNodes": [] }),
    ] {
        f.service
            .store
            .change_run_fixture(parent_b["id"].as_str().unwrap(), reduction)
            .await
            .unwrap();
        let run = existing_native.clone();
        let node = f.node.clone();
        assert!(
            !f.service
                .store
                .read(move |db| policy::allows(db, &run, &node))
                .await
                .unwrap()
        );
        let denied =
            delegated_call(&f, &c, "create_chat", json!({ "agentId": f.agent["id"] })).await;
        assert_eq!(denied["result"]["isError"], true, "{denied}");
    }
    f.service
        .agent_fixture(
            json!({
                "name": "Other VM director", "access": { "installation": false }
            }),
            parent_b_agent["id"].as_str(),
        )
        .await
        .unwrap();
    let node = f.node.clone();
    assert!(
        !f.service
            .store
            .read(move |db| policy::allows(db, &existing_native, &node))
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn changing_the_parent_permission_or_node_ceiling_revokes_descendants() {
    let f = Fixture::new().await;
    let (parent, _) = delegate(&f, true).await;
    let mut child = f.run();
    child["nativeParentRunId"] = parent["id"].clone();
    f.service
        .store
        .update_run_boundary_fixture(
            parent["id"].as_str().unwrap(),
            json!({ "nativeAuthorityNodes": [] }),
        )
        .await
        .unwrap();
    let (run, node) = (child.clone(), f.node.clone());
    assert!(
        !f.service
            .store
            .read(move |db| policy::allows(db, &run, &node))
            .await
            .unwrap()
    );
    f.service
        .store
        .update_run_boundary_fixture(
            parent["id"].as_str().unwrap(),
            json!({ "nativeAuthorityNodes": [f.node["id"]] }),
        )
        .await
        .unwrap();
    let mut agent = parent["snapshot"]["agent"].clone();
    agent["access"]["installation"] = false.into();
    f.service.store.put_agent_fixture(agent).await.unwrap();
    let node = f.node.clone();
    assert!(
        !f.service
            .store
            .read(move |db| policy::allows(db, &child, &node))
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn scheduled_native_conversations_recheck_the_rule_creators_node_access() {
    let f = Fixture::new().await;
    let date = now() + 3_600_000;
    let definition = json!({
        "name": "Scheduled native work",
        "description": "Verify the creator retains native access",
        "executionAgentId": f.agent["id"],
        "trigger": { "type": "clock", "at": date, "timezone": "UTC" },
        "limits": cairn_contracts::HookRuleLimits::default(),
        "steps": [{
            "type": "action",
            "id": "start_agent",
            "action": {
                "type": "startAgent",
                "agentId": f.agent["id"],
                "prompt": { "type": "literal", "value": "Run on the native node." },
            },
        }],
    });
    let (status, rule) = f
        .call(
            &f.trusted,
            "POST",
            "/api/hook-rules",
            json!({ "definition": definition }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{rule}");
    assert_eq!(rule["nativeAuthority"], true, "{rule}");
    let run_now = format!("/api/hook-rules/{}/run", text(&rule, "id"));
    let (status, queued) = f.call(&f.trusted, "POST", &run_now, Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{queued}");
    cairn_installation::rules::engine::tick(&f.service)
        .await
        .unwrap();
    f.service.chat_tick(&HashSet::new()).await.unwrap();
    let chats = f.service.chat_list().await.unwrap();
    assert_eq!(chats.len(), 1, "{chats:?}");
    assert_eq!(chats[0]["originRule"]["schedule"], true);
    let run = f.service.store.run(text(&chats[0], "runId")).await.unwrap();
    assert_eq!(run["nativePrincipals"], json!([TRUSTED]));
    placement::check(&f.service, &run).await.unwrap();

    let mut node = f.node.clone();
    node["nativeAccounts"] = json!([]);
    f.service.store.put_node_fixture(node).await.unwrap();
    assert!(placement::check(&f.service, &run).await.is_err());
    let (status, queued) = f.call(&f.trusted, "POST", &run_now, Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{queued}");
    cairn_installation::rules::engine::tick(&f.service)
        .await
        .unwrap();
    let chats = f.service.chat_list().await.unwrap();
    assert_eq!(chats.len(), 1, "{chats:?}");
}

#[tokio::test]
async fn converting_wildcard_access_never_reviews_host_root_without_explicit_owner_confirmation() {
    let f = Fixture::new().await;
    let node_id = text(&f.node, "id");
    let mut node = f.node.clone();
    node["nativeModeRequest"] = json!({ "mode": "administrator", "generation": id() });
    node["nativeReviewedAgents"] = json!([]);
    f.service.store.put_node_fixture(node).await.unwrap();
    let agent = f
        .service
        .agent_fixture(
            json!({ "name": "Future wildcard", "access": { "backend": "native", "nodes": null } }),
            None,
        )
        .await
        .unwrap();
    let path = format!("/api/agents/{}", text(&agent, "id"));
    let edit =
        json!({ "name": "Future wildcard", "access": { "backend": "native", "nodes": [node_id] } });
    let (status, _) = f.owner_call("PUT", &path, edit.clone()).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(
        f.service.node_boundary(node_id).await.unwrap()["nativeReviewedAgents"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(
        f.service.agent_snapshot(text(&agent, "id")).await.unwrap()["access"]["nodes"].is_null()
    );
    let mut confirmed = edit;
    confirmed["nativeNodeReviews"] = json!([node_id]);
    let (status, result) = f.owner_call("PUT", &path, confirmed).await;
    assert_eq!(status, StatusCode::OK, "{result}");
    assert!(
        f.service.node_boundary(node_id).await.unwrap()["nativeReviewedAgents"]
            .as_array()
            .unwrap()
            .contains(&agent["id"])
    );
    assert!(
        result["nativeNodeReviews"].is_null(),
        "The acknowledgement is not a new profile permission"
    );
}

#[tokio::test]
async fn native_mode_preview_includes_unplaced_automatic_work_and_rejects_concurrent_arrivals() {
    for backend in ["native", "auto"] {
        unplaced_administrator_preview(backend).await;
    }
}
