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
    common::add_run(&f.service.store, &parent).await;
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
    common::add_run(&f.service.store, &parent_b).await;
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
async fn editing_a_vm_task_cannot_replace_its_authors_with_a_native_trusted_editor() {
    let f = Fixture::new().await;
    let _authority = mission_authority(&f).await;
    let author = auth::InstallationIdentity::trusted(auth::InstallationRole::Member, OTHER);
    let editor = auth::InstallationIdentity::trusted(auth::InstallationRole::Owner, TRUSTED);
    let vm = f
        .service
        .agent_fixture(json!({ "name": "VM task agent" }), None)
        .await
        .unwrap();
    members_use_agents(&f.service, &[(OTHER, "two")]).await;
    let task = f
        .service
        .task_as_fixture(
            json!({
                "name": "Retained commitment",
                "prompt": "Original brief",
                "agentId": vm["id"],
            }),
            None,
            Some(&author),
        )
        .await
        .unwrap();
    assert_eq!(task["authorId"], OTHER);
    let mut edited = task.clone();
    edited["agentId"] = f.agent["id"].clone();
    let error = f
        .service
        .task_as_fixture(edited, task["id"].as_str(), Some(&editor))
        .await
        .unwrap_err();
    assert_eq!(error.status, 403);
    assert_eq!(
        f.service
            .mission_snapshot_fixture(task["id"].as_str().unwrap())
            .await
            .unwrap()["agentId"],
        vm["id"]
    );
}

#[tokio::test]
async fn native_tasks_retain_previous_directing_editors_and_current_author_rights() {
    let f = Fixture::new().await;
    trust_owner(&f).await;
    let _authority = mission_authority(&f).await;
    let author =
        auth::InstallationIdentity::trusted(auth::InstallationRole::Member, "fixture-owner");
    let editor = auth::InstallationIdentity::trusted(auth::InstallationRole::Owner, TRUSTED);
    members_use_agents(&f.service, &[("fixture-owner", "fixture-owner")]).await;
    let task = f
        .service
        .task_as_fixture(
            json!({
                "name": "Native commitment",
                "prompt": "Original brief",
                "agentId": f.agent["id"],
            }),
            None,
            Some(&author),
        )
        .await
        .unwrap();
    let mut edited = task.clone();
    edited["prompt"] = "Updated brief".into();
    let saved = f
        .service
        .task_as_fixture(edited, task["id"].as_str(), Some(&editor))
        .await
        .unwrap();
    assert_eq!(saved["authorId"], "fixture-owner");
    assert!(
        saved["nativePrincipals"]
            .as_array()
            .unwrap()
            .contains(&json!(TRUSTED))
    );
    assert!(
        saved["nativePrincipals"]
            .as_array()
            .unwrap()
            .contains(&json!("fixture-owner"))
    );
    f.service
        .store
        .put_node_fixture(f.node.clone())
        .await
        .unwrap();
    let error = f
        .service
        .task_as_fixture(saved, task["id"].as_str(), Some(&editor))
        .await
        .unwrap_err();
    assert_eq!(error.status, 403);
}

#[tokio::test]
async fn revoked_native_permissions_still_allow_disabling_but_never_reenabling_a_task() {
    let f = Fixture::new().await;
    trust_owner(&f).await;
    let owner = RelayContext::new(&common::relay_fixture::context(&f.service).await);
    let (status, task) = f
        .call(
            &owner,
            "POST",
            "/api/tasks",
            json!({ "settings": {
                "name": "Native schedule", "prompt": "Build", "agentId": f.agent["id"],
                "cron": "0 * * * *", "timezone": "UTC"
            } }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{task}");
    f.service
        .store
        .put_node_fixture(f.node.clone())
        .await
        .unwrap();
    // Disabling a task must succeed while Beacon authority is unavailable.
    common::relay_fixture::native_authority_unavailable(&f.service).await;
    f.service
        .store
        .set(
            "native-authority",
            json!({ "observedAt": 0, "accounts": {} }),
            None,
        )
        .await
        .unwrap();
    let path = format!("/api/tasks/{}/state", task["id"].as_str().unwrap());
    let disabled = json!({ "taskId": task["id"], "revision": task["revision"], "enabled": false, "archived": false });
    let (status, saved) = f.call(&owner, "POST", &path, disabled).await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert_eq!(saved["enabled"], false);
    assert_eq!(saved["nativePrincipals"], task["nativePrincipals"]);
    assert_eq!(saved["nativeAuthorityNodes"], task["nativeAuthorityNodes"]);
    f.service
        .store
        .set(
            "native-authority",
            json!({
                "observedAt": now(),
                "accounts": { TRUSTED: {} },
            }),
            None,
        )
        .await
        .unwrap();
    let enabled = json!({ "taskId": saved["id"], "revision": saved["revision"], "enabled": true, "archived": false });
    let (status, _) = f.call(&owner, "POST", &path, enabled).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn a_native_task_without_recorded_provenance_says_how_to_schedule_it_again() {
    let f = Fixture::new().await;
    trust_owner(&f).await;
    // A complete current mission whose agent subsequently moves to native.
    let vm = f
        .service
        .agent_fixture(json!({ "name": "VM before native" }), None)
        .await
        .unwrap();
    let mut task = f
        .service
        .task_fixture(
            json!({
                "name": "Existing commitment", "prompt": "Original brief", "agentId": vm["id"],
                "cron": "0 9 * * *", "timezone": "Europe/Paris"
            }),
            None,
        )
        .await
        .unwrap();
    task["agentId"] = f.agent["id"].clone();
    let author =
        auth::InstallationIdentity::trusted(auth::InstallationRole::Owner, "fixture-owner");
    f.service
        .store
        .put_mission_fixture(task.clone())
        .await
        .unwrap();

    let error = f
        .service
        .task_as_fixture(task.clone(), task["id"].as_str(), Some(&author))
        .await
        .unwrap_err();
    assert_eq!(error.status, 403, "{}", error.message);
    assert!(
        error.message.contains("no recorded native authority"),
        "{}",
        error.message
    );

    let mut duplicate = task.clone();
    duplicate.as_object_mut().unwrap().remove("id");
    let saved = f
        .service
        .task_as_fixture(duplicate, None, Some(&author))
        .await
        .unwrap();
    assert_eq!(saved["nativeProvenanceUnknown"], false);
}

#[tokio::test]
async fn scheduled_native_placement_rechecks_the_retained_author_even_on_older_provenance() {
    let f = Fixture::new().await;
    let mut run = f.run();
    run["trigger"] = "schedule".into();
    run["snapshot"]["task"] = json!({ "authorId": OTHER });
    common::add_run(&f.service.store, &run).await;
    assert!(placement::check(&f.service, &run).await.is_err());
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
