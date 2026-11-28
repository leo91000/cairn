use super::*;

#[tokio::test]
async fn delegated_native_administration_requires_the_requesters_current_review() {
    let mut f = Fixture::with_workspace(true).await;
    f.agent["access"]["delegation"] = true.into();
    f.agent["access"]["nodes"] = Value::Null;
    f.service
        .store
        .put_agent_fixture(f.agent.clone())
        .await
        .unwrap();

    let mut administrator = f.node.clone();
    administrator["id"] = id().into();
    administrator["name"] = "Administrative host".into();
    f.service
        .store
        .put_node_fixture(administrator.clone())
        .await
        .unwrap();
    let target = f
        .service
        .agent_fixture(
            json!({
                "name": "Reviewed target",
                "access": { "backend": "native", "nodes": [administrator["id"]] },
            }),
            None,
        )
        .await
        .unwrap();
    let (_, parent) = f.conversation().await;
    let parent = f
        .service
        .store
        .change_run(
            text(&parent, "id"),
            cairn_installation::runs::RunChanges {
                status: Some(cairn_contracts::RunStatus::Running),
                placement: Some(Box::new(
                    cairn_installation::nodes::run_placement::PlacementChanges {
                        node_id: Some(
                            serde_json::from_value(serde_json::json!(f.node["id"])).unwrap(),
                        ),
                        ..Default::default()
                    },
                )),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let parent = f.service.store.run(&parent.id).await.unwrap();
    let configuration = f
        .service
        .mcps
        .run_configuration(&f.service, &parent)
        .await
        .unwrap();
    let token = text(&configuration["env"], "CAIRN_MCP_RUN_TOKEN");

    let generation = id();
    administrator["capabilities"]["nativeModeProtocol"] = 1.into();
    administrator["nativeModeRequest"] =
        json!({ "mode": "administrator", "generation": generation });
    administrator["nativeModeObserved"] = json!({
        "mode": "administrator", "generation": generation, "status": "applied",
    });
    administrator["nativeReviewedAgents"] = json!([target["id"]]);
    f.service
        .store
        .put_node_fixture(administrator.clone())
        .await
        .unwrap();
    let input = json!({ "task": "Review on the administrative host", "agentId": target["id"] });
    let error = cairn_installation::delegation::delegate(&f.service, token, &input)
        .await
        .unwrap_err();
    assert_eq!(error.status, StatusCode::FORBIDDEN);

    administrator["nativeReviewedAgents"] = json!([target["id"], f.agent["id"]]);
    f.service
        .store
        .put_node_fixture(administrator.clone())
        .await
        .unwrap();
    let task = cairn_installation::delegation::delegate(&f.service, token, &input)
        .await
        .unwrap();
    f.service.chat_tick(&HashSet::new()).await.unwrap();
    let child = f.service.chat_detail(text(&task, "chatId")).await.unwrap()["run"].clone();
    let check = |node: Value| {
        let child = child.clone();
        f.service
            .store
            .read(move |db| policy::allows(db, &child, &node))
    };
    assert!(check(administrator.clone()).await.unwrap());

    administrator["nativeReviewedAgents"] = json!([target["id"]]);
    f.service
        .store
        .put_node_fixture(administrator.clone())
        .await
        .unwrap();
    assert!(
        !check(administrator).await.unwrap(),
        "Removing the requester's review revokes its child's administrator admission"
    );
}

#[tokio::test]
async fn task_delegation_has_its_own_revocable_native_authority() {
    let mut f = Fixture::with_workspace(true).await;
    f.agent["access"]["delegation"] = true.into();
    f.service
        .store
        .put_agent_fixture(f.agent.clone())
        .await
        .unwrap();
    let (_, parent) = f.conversation().await;
    let parent = f
        .service
        .store
        .change_run(
            text(&parent, "id"),
            cairn_installation::runs::RunChanges {
                status: Some(cairn_contracts::RunStatus::Running),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let parent = f.service.store.run(&parent.id).await.unwrap();
    let configuration = f
        .service
        .mcps
        .run_configuration(&f.service, &parent)
        .await
        .unwrap();
    let task = cairn_installation::delegation::delegate(
        &f.service,
        text(&configuration["env"], "CAIRN_MCP_RUN_TOKEN"),
        &json!({ "task": "Review on the authorized native node" }),
    )
    .await
    .unwrap();
    let child_id = text(&task, "chatId");
    let child = f.service.conversation_boundary(child_id).await.unwrap();
    assert_eq!(child["nativeParentRunId"], parent["id"]);
    assert_eq!(child["nativeParentAccess"], "delegation");
    assert_eq!(child["nativeAuthorityNodes"], json!([f.node["id"]]));
    f.service.chat_tick(&HashSet::new()).await.unwrap();
    let child = f.service.chat_detail(child_id).await.unwrap()["run"].clone();
    assert_eq!(child["nativeParentAccess"], "delegation");

    let check = |run: Value| {
        let node = f.node.clone();
        f.service
            .store
            .read(move |db| policy::allows(db, &run, &node))
    };
    assert!(check(child.clone()).await.unwrap());
    let mut management_child = child.clone();
    management_child["nativeParentAccess"] = Value::Null;
    assert!(
        !check(management_child).await.unwrap(),
        "Management-created and historical edges still require installation access"
    );

    // A human follow-up preserves the capability that authorized the parent edge.
    policy::CALLER
        .scope(
            Some(TRUSTED.into()),
            f.service.chat_send(
                child_id,
                json!({ "id": id(), "text": "Also inspect the tests" }),
            ),
        )
        .await
        .unwrap();
    let continued = f.service.conversation_boundary(child_id).await.unwrap();
    assert_eq!(continued["nativeParentAccess"], "delegation");

    let mut revoked = f.agent.clone();
    revoked["access"]["delegation"] = false.into();
    revoked["access"]["installation"] = true.into();
    f.service.store.put_agent_fixture(revoked).await.unwrap();
    assert!(
        !check(child.clone()).await.unwrap(),
        "Installation access cannot replace revoked task delegation"
    );
    f.service
        .store
        .put_agent_fixture(f.agent.clone())
        .await
        .unwrap();
    assert!(check(child.clone()).await.unwrap());
    f.service
        .store
        .update_run_boundary_fixture(text(&parent, "id"), json!({ "nativeAuthorityNodes": [] }))
        .await
        .unwrap();
    assert!(
        !check(child).await.unwrap(),
        "The parent's node ceiling remains live"
    );
}

#[tokio::test]
async fn side_questions_check_native_access_without_changing_execution_provenance() {
    let f = Fixture::new().await;
    let mut agent = f.agent.clone();
    agent["provider"] = "claude".into();
    f.service.store.put_agent_fixture(agent).await.unwrap();
    let (chat, run) = f.conversation().await;
    let chat_id = chat["id"].as_str().unwrap();
    let run_id = run["id"].as_str().unwrap();
    f.service
        .store
        .change_run(
            run_id,
            cairn_installation::runs::RunChanges {
                status: Some(cairn_installation::run_status::RunStatus::Running),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    f.service
        .side_questions_available(run_id, true)
        .await
        .unwrap();
    let before_chat = f.service.conversation_boundary(chat_id).await.unwrap();
    let before_run = f.service.store.run(run_id).await.unwrap();
    let mut node = f.node.clone();
    node["nativeAccounts"] = json!([TRUSTED, OTHER]);
    f.service
        .store
        .put_node_fixture(node.clone())
        .await
        .unwrap();

    let question_id = id();
    let path = format!("/api/chats/{chat_id}/side-questions");
    let (status, response) = f
        .owner_call("POST", &path, json!({ "id": question_id, "text": "Why?" }))
        .await;
    assert_eq!(status, StatusCode::OK, "{response}");
    let (status, response) = f
        .owner_call("POST", &format!("{path}/{question_id}/cancel"), json!({}))
        .await;
    assert_eq!(status, StatusCode::OK, "{response}");
    let after_chat = f.service.conversation_boundary(chat_id).await.unwrap();
    let after_run = f.service.store.run(run_id).await.unwrap();
    for key in [
        "nativePrincipals",
        "nativeAuthorityNodes",
        "nativeParentRunId",
        "nativeParentAccess",
        "nativePluginGrants",
        "nativeProvenanceUnknown",
    ] {
        assert_eq!(after_chat[key], before_chat[key], "chat {key}");
        assert_eq!(after_run[key], before_run[key], "run {key}");
    }

    let director = f
        .service
        .agent_fixture(
            json!({ "name": "Side question caller", "access": { "installation": true } }),
            None,
        )
        .await
        .unwrap();
    let mut parent = f.run();
    parent["snapshot"]["agent"] = director;
    parent["status"] = "running".into();
    parent["nativePrincipals"] = json!([OTHER]);
    common::add_run(&f.service.store, &parent).await;
    let configuration = f
        .service
        .mcps
        .run_configuration(&f.service, &parent)
        .await
        .unwrap();
    let delegated_id = id();
    let asked = delegated_call(
        &f,
        &configuration,
        "ask_chat_side_question",
        json!({
            "chatId": chat_id, "question": { "id": delegated_id, "text": "Why this way?" },
        }),
    )
    .await;
    assert_ne!(asked["result"]["isError"], true, "{asked}");
    let cancelled = delegated_call(
        &f,
        &configuration,
        "cancel_chat_side_question",
        json!({
            "chatId": chat_id, "sideQuestionId": delegated_id,
        }),
    )
    .await;
    assert_ne!(cancelled["result"]["isError"], true, "{cancelled}");
    let delegated_chat = f.service.conversation_boundary(chat_id).await.unwrap();
    let delegated_run = f.service.store.run(run_id).await.unwrap();
    for key in [
        "nativePrincipals",
        "nativeAuthorityNodes",
        "nativeParentRunId",
        "nativeParentAccess",
        "nativePluginGrants",
        "nativeProvenanceUnknown",
    ] {
        assert_eq!(
            delegated_chat[key], before_chat[key],
            "delegated chat {key}"
        );
        assert_eq!(delegated_run[key], before_run[key], "delegated run {key}");
    }

    node["nativeAccounts"] = json!([TRUSTED]);
    f.service.store.put_node_fixture(node).await.unwrap();
    let (status, _) = f
        .owner_call("POST", &path, json!({ "id": id(), "text": "Denied" }))
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn rejected_native_mutations_do_not_become_historical_directors() {
    let f = Fixture::new().await;
    let (chat, run) = f.conversation().await;
    let (chat_id, run_id) = (chat["id"].as_str().unwrap(), run["id"].as_str().unwrap());
    let mut node = f.node.clone();
    node["nativeAccounts"] = json!([TRUSTED, OTHER]);
    f.service
        .store
        .put_node_fixture(node.clone())
        .await
        .unwrap();
    let mutations = [
        (
            "POST",
            format!("/api/chats/{chat_id}/coding-agent"),
            json!({ "requestId": id(), "runId": run_id, "turnId": "stale-turn", "provider": "claude" }),
            StatusCode::CONFLICT,
        ),
        (
            "POST",
            format!("/api/chats/{chat_id}/messages"),
            json!({ "id": "invalid", "text": "Rejected" }),
            StatusCode::BAD_REQUEST,
        ),
        (
            "POST",
            format!("/api/chats/{chat_id}/messages"),
            json!({ "id": id(), "text": "x".repeat(50_001) }),
            StatusCode::BAD_REQUEST,
        ),
        (
            "POST",
            format!("/api/chats/{chat_id}/unsupported"),
            json!({}),
            StatusCode::NOT_FOUND,
        ),
        (
            "POST",
            format!("/api/runs/{run_id}/resume"),
            json!({}),
            StatusCode::CONFLICT,
        ),
        (
            "POST",
            format!("/api/chats/{chat_id}/new-session"),
            json!({ "confirm": false }),
            StatusCode::CONFLICT,
        ),
        (
            "POST",
            format!("/api/chats/{chat_id}/restore"),
            json!({}),
            StatusCode::CONFLICT,
        ),
        (
            "POST",
            format!("/api/chats/{chat_id}/questions/{}/cancel", id()),
            json!({}),
            StatusCode::NOT_FOUND,
        ),
        (
            "PUT",
            format!("/api/nodes/placement/{run_id}"),
            json!({ "unsupported": true }),
            StatusCode::BAD_REQUEST,
        ),
        (
            "POST",
            format!("/api/nodes/placement/{run_id}/move"),
            json!({ "nodeId": node["id"], "cpu": 1 }),
            StatusCode::BAD_REQUEST,
        ),
    ];
    for (method, path, body, expected) in mutations {
        let (status, response) = f.owner_call(method, &path, body).await;
        assert_eq!(status, expected, "{method} {path}: {response}");
        let current_chat = f.service.conversation_boundary(chat_id).await.unwrap();
        let current_run = f.service.store.run(run_id).await.unwrap();
        for key in [
            "nativePrincipals",
            "nativeAuthorityNodes",
            "nativeParentRunId",
            "nativeProvenanceUnknown",
        ] {
            assert_eq!(current_chat[key], chat[key], "{path}: chat {key}");
            assert_eq!(current_run[key], run[key], "{path}: run {key}");
        }
    }

    // The MCP control path must also keep rejected resumes out of provenance.
    let director = f
        .service
        .agent_fixture(
            json!({ "name": "Other director", "access": { "installation": true } }),
            None,
        )
        .await
        .unwrap();
    let mut parent = f.run();
    parent["snapshot"]["agent"] = director;
    parent["status"] = "running".into();
    parent["nativePrincipals"] = json!([OTHER]);
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
        "resume_chat",
        json!({ "chatId": chat_id }),
    )
    .await;
    assert_eq!(response["result"]["isError"], true, "{response}");
    let current_chat = f.service.conversation_boundary(chat_id).await.unwrap();
    let current_run = f.service.store.run(run_id).await.unwrap();
    for key in [
        "nativePrincipals",
        "nativeAuthorityNodes",
        "nativeParentRunId",
        "nativeProvenanceUnknown",
    ] {
        assert_eq!(current_chat[key], chat[key], "MCP chat {key}");
        assert_eq!(current_run[key], run[key], "MCP run {key}");
    }

    // Revoking a rejected caller must leave the original person's work authorized.
    node["nativeAccounts"] = json!([TRUSTED]);
    f.service
        .store
        .put_node_fixture(node.clone())
        .await
        .unwrap();
    let record = current_run;
    let target = node.clone();
    assert!(
        f.service
            .store
            .read(move |db| policy::allows(db, &record, &target))
            .await
            .unwrap()
    );

    // A successful unpause records the accepted director in the same effect.
    node["nativeAccounts"] = json!([TRUSTED, OTHER]);
    f.service
        .store
        .put_node_fixture(node.clone())
        .await
        .unwrap();
    f.service.chat_pause(chat_id, true).await.unwrap();
    let (status, response) = f
        .owner_call(
            "POST",
            &format!("/api/chats/{chat_id}/pause"),
            json!({ "paused": false }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{response}");
    let directed = f.service.store.run(run_id).await.unwrap();
    assert_eq!(directed["nativePrincipals"], json!([TRUSTED, OTHER]));
    assert!(response.get("nativePrincipals").is_none());
    let saved = f.service.conversation_boundary(chat_id).await.unwrap();
    assert_eq!(saved["nativePrincipals"], directed["nativePrincipals"]);
    node["nativeAccounts"] = json!([TRUSTED]);
    f.service
        .store
        .put_node_fixture(node.clone())
        .await
        .unwrap();
    assert!(
        !f.service
            .store
            .read(move |db| policy::allows(db, &directed, &node))
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn native_stop_controls_remain_available_without_host_admission() {
    for action in ["pause", "cancel", "stop"] {
        let f = Fixture::new().await;
        let (mut chat, run) = f.conversation().await;
        let chat_id = chat["id"].as_str().unwrap().to_owned();
        let run_id = run["id"].as_str().unwrap().to_owned();
        if action == "stop" {
            f.service
                .store
                .change_run(
                    &run_id,
                    cairn_installation::runs::RunChanges {
                        status: Some(cairn_contracts::RunStatus::Succeeded),
                        ..Default::default()
                    },
                )
                .await
                .unwrap();
            chat["wakeup"] = serde_json::to_value(common::run_fixture::complete_wakeup_fixture(
                &json!({"id":"saved-wait","wakeAt":now()+60_000,"reason":"Saved continuation"}),
            ))
            .unwrap();
            f.service
                .store
                .save_conversation_boundary(chat)
                .await
                .unwrap();
        }
        let mut node = f.node.clone();
        node["nativeEnabled"] = false.into();
        node["nativeAccounts"] = json!([]);
        f.service.store.put_node_fixture(node).await.unwrap();
        // Execution-reducing controls must succeed when a refresh would fail.
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
        let path = if action == "cancel" {
            format!("/api/runs/{run_id}/cancel")
        } else {
            format!("/api/chats/{chat_id}/{action}")
        };
        let (status, response) = f
            .call(&f.other, "POST", &path, json!({ "paused": true }))
            .await;
        assert_eq!(status, StatusCode::OK, "{action}: {response}");
        let chat = f.service.conversation_boundary(&chat_id).await.unwrap();
        let run = f.service.store.run(&run_id).await.unwrap();
        assert_eq!(chat["nativePrincipals"], json!([TRUSTED]));
        assert_eq!(run["nativePrincipals"], json!([TRUSTED]));
        if action != "cancel" {
            assert_eq!(chat["paused"], true);
        }
        if action == "stop" {
            assert!(chat["wakeup"].is_null());
        }
        if action == "cancel" {
            assert_eq!(run["status"], "cancelled");
        }
        if action == "pause" {
            f.service
                .store
                .set(
                    "native-authority",
                    json!({ "observedAt": now(), "accounts": { OTHER: {} } }),
                    None,
                )
                .await
                .unwrap();
            let (status, response) = f
                .call(&f.other, "POST", &path, json!({ "paused": false }))
                .await;
            assert_eq!(
                status,
                StatusCode::FORBIDDEN,
                "unpause must still fail closed: {response}"
            );
            assert_eq!(
                f.service.conversation_boundary(&chat_id).await.unwrap()["paused"],
                true
            );
        }
    }
}

#[tokio::test]
async fn accepted_native_run_controls_retain_the_director_on_the_chat_and_run() {
    for action in ["resume", "placement"] {
        let f = Fixture::new().await;
        let (chat, run) = f.conversation().await;
        let (chat_id, run_id) = (chat["id"].as_str().unwrap(), run["id"].as_str().unwrap());
        assert_eq!(chat["runId"], run["id"]);
        let mut node = f.node.clone();
        node["nativeAccounts"] = json!([TRUSTED, OTHER]);
        f.service
            .store
            .put_node_fixture(node.clone())
            .await
            .unwrap();
        let (method, path, body) = if action == "resume" {
            f.service
                .store
                .change_run(
                    run_id,
                    cairn_installation::runs::RunChanges {
                        status: Some(cairn_contracts::RunStatus::Cancelled),
                        ..Default::default()
                    },
                )
                .await
                .unwrap();
            ("POST", format!("/api/runs/{run_id}/resume"), json!({}))
        } else {
            (
                "PUT",
                format!("/api/nodes/placement/{run_id}"),
                json!({ "selection":{ "pinnedNodeId": node["id"] } }),
            )
        };
        let (status, response) = f.owner_call(method, &path, body).await;
        assert_eq!(status, StatusCode::OK, "{action}: {response}");
        let chat = f.service.conversation_boundary(chat_id).await.unwrap();
        let run = f.service.store.run(run_id).await.unwrap();
        assert_eq!(
            chat["nativePrincipals"],
            json!([TRUSTED, OTHER]),
            "{action}"
        );
        assert_eq!(
            run["nativePrincipals"], chat["nativePrincipals"],
            "{action}"
        );
        node["nativeAccounts"] = json!([TRUSTED]);
        f.service
            .store
            .put_node_fixture(node.clone())
            .await
            .unwrap();
        assert!(
            !f.service
                .store
                .read(move |db| policy::allows(db, &run, &node))
                .await
                .unwrap(),
            "{action}"
        );
    }
}

#[tokio::test]
async fn selected_native_agent_requires_a_trusted_human_on_every_message() {
    let f = Fixture::new().await;
    let (status, _) = f
        .call(
            &f.other,
            "POST",
            "/api/chats",
            json!({ "agentId": f.agent["id"] }),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, chat) = f
        .call(
            &f.trusted,
            "POST",
            "/api/chats",
            json!({ "agentId": f.agent["id"] }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let path = format!("/api/chats/{}/messages", chat["id"].as_str().unwrap());
    let (status, _) = f
        .call(
            &f.other,
            "POST",
            &path,
            json!({ "text": "Run on the host", "nativePrincipals": [TRUSTED] }),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = f
        .call(
            &f.trusted,
            "POST",
            &path,
            json!({ "id": id(), "text": "Build the project" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    // Internal callers and plugins do not inherit a human's authority by choosing their agent.
    let rejected = f
        .service
        .chat_send(
            chat["id"].as_str().unwrap(),
            json!({ "id": id(), "text": "Unscoped native work" }),
        )
        .await
        .unwrap_err();
    assert_eq!(rejected.status, 403);
}

#[tokio::test]
async fn native_placement_works_without_kvm_or_fuse_and_never_falls_back_to_a_vm() {
    let f = Fixture::new().await;
    let run = f.run();
    common::add_run(&f.service.store, &run).await;
    let attempt = id();
    let placement = placement::reserve(&f.service, &run, &attempt)
        .await
        .unwrap();
    assert_eq!(placement["nodeId"], f.node["id"]);
    placement::release(&f.service, &attempt).await.unwrap();
    let mut node = f.node.clone();
    node["nativeEnabled"] = false.into();
    f.service.store.put_node_fixture(node).await.unwrap();
    assert!(placement::check(&f.service, &run).await.is_err());
}

#[tokio::test]
async fn moved_conversations_claim_reservations_for_their_execution_backend() {
    for (backend, changed_backend) in [("native", false), ("firecracker", false), ("native", true)]
    {
        let f = Fixture::new().await;
        let native = backend == "native";
        let mut node = f.node.clone();
        node["capabilities"]["backend"] = backend.into();
        node["capabilities"]["kvm"] = (!native).into();
        f.service
            .store
            .put_node_fixture(node.clone())
            .await
            .unwrap();
        let mut agent = f.agent.clone();
        agent["access"]["backend"] = backend.into();
        f.service
            .store
            .put_agent_fixture(agent.clone())
            .await
            .unwrap();

        let mut run = f.run();
        run["snapshot"]["agent"] = agent;
        run["placementTransition"] = true.into();
        run["targetNodeId"] = node["id"].clone();
        common::add_run(&f.service.store, &run).await;
        let run_id = run["id"].as_str().unwrap();
        let reservation = id();
        placement::reserve(&f.service, &run, &reservation)
            .await
            .unwrap();
        let held = f
            .service
            .store
            .attempt_fixture(&reservation)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(held["role"], "destination");

        // An active/automatic move persists this state after restoring files.
        let resumed = f
            .service
            .store
            .change_run(
                run_id,
                cairn_installation::runs::RunChanges {
                    recovery_pending: Some(true),
                    status: Some(cairn_contracts::RunStatus::Queued),
                    placement: Some(Box::new(
                        cairn_installation::nodes::run_placement::PlacementChanges {
                            target_node_id: Some(None),
                            move_reservation: Some(
                                serde_json::from_value(serde_json::json!(reservation)).unwrap(),
                            ),
                            node_id: Some(
                                serde_json::from_value(serde_json::json!(node["id"])).unwrap(),
                            ),
                            node_state: Some(
                                serde_json::from_value(serde_json::json!("resuming")).unwrap(),
                            ),
                            ..Default::default()
                        },
                    )),
                    boundary: Some(serde_json::json!({"placementTransition": false})),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        common::set_checkpoint(
            &f.service.store,
            run_id,
            json!({ "nodeId": node["id"], "runtimeId": "development" }),
        )
        .await;

        if changed_backend {
            node["capabilities"]["backend"] = "firecracker".into();
            node["capabilities"]["kvm"] = true.into();
        }
        f.service.store.put_node_fixture(node).await.unwrap();
        let attempt = id();
        let resumed = f.service.store.run(&resumed.id).await.unwrap();
        let claimed = placement::reserve(&f.service, &resumed, &attempt).await;
        let succeeds = !changed_backend;
        if succeeds {
            let claimed = claimed.unwrap();
            assert_eq!(claimed["nodeId"], f.node["id"]);
            assert_eq!(claimed["resources"], held["resources"]);
            assert_eq!(claimed["runtimeId"], held["runtimeId"]);
            let execution = f
                .service
                .store
                .attempt_fixture(&attempt)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(execution["role"], "execution");
            assert!(execution["leaseExpiresAt"].as_i64().unwrap() > now());
        } else {
            let error = claimed.unwrap_err();
            assert_eq!(error.status, 409);
            assert_eq!(error.message, "Destination execution backend changed.");
            assert!(
                f.service
                    .store
                    .attempt_fixture(&attempt)
                    .await
                    .unwrap()
                    .is_none()
            );
        }
        let previous = f
            .service
            .store
            .attempt_fixture(&reservation)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(previous["released"], succeeds);
        let persisted = f.service.store.run(run_id).await.unwrap();
        if succeeds {
            assert!(persisted["moveReservation"].is_null());
        } else {
            assert_eq!(persisted["moveReservation"], reservation);
        }
    }
}

#[tokio::test]
async fn retained_conversations_exhaust_the_shared_native_admission_budget() {
    let f = Fixture::new().await;
    let mut node = f.node.clone();
    node["usage"] = json!({ "diskMiB": 20 * 1024 });
    f.service
        .store
        .put_node_fixture(node.clone())
        .await
        .unwrap();
    let first = f.run();
    common::add_run(&f.service.store, &first).await;
    let attempt = id();
    placement::reserve(&f.service, &first, &attempt)
        .await
        .unwrap();
    placement::release(&f.service, &attempt).await.unwrap();
    // Heartbeats include all retained conversations, not just a running slot.
    node["usage"] = json!({ "diskMiB": 40 * 1024 });
    f.service.store.put_node_fixture(node).await.unwrap();
    let next = f.run();
    common::add_run(&f.service.store, &next).await;
    assert!(placement::reserve(&f.service, &next, &id()).await.is_err());
}

#[tokio::test]
async fn native_admission_retains_allowances_below_the_configured_budget_minimum() {
    for (budget, allowance) in [(256, 116), (264, 120)] {
        let f = Fixture::new().await;
        let mut node = f.node.clone();
        node["limits"]["diskMiB"] = budget.into();
        node["diskGrowthProtocol"] = 1.into();
        node["diskCapacity"] = json!({
            "protocol": 1,
            "capturedAt": now(),
            "usedBytes": 0,
            "freeBytes": 900 * 1024 * 1024 * 1024u64,
            "ownedBytes": {},
            "retainedBytes": {},
            "requests": [],
        });
        f.service.store.put_node_fixture(node).await.unwrap();
        let run = f.run();
        common::add_run(&f.service.store, &run).await;
        let attempt = id();

        let selected = placement::reserve(&f.service, &run, &attempt)
            .await
            .unwrap();
        assert_eq!(selected["resources"]["diskMiB"], allowance);
        assert_eq!(
            f.service
                .store
                .attempt(&attempt)
                .await
                .unwrap()
                .unwrap()
                .resources
                .disk_mi_b,
            allowance
        );
        placement::release(&f.service, &attempt).await.unwrap();
        let retained = f.service.store.attempt(&attempt).await.unwrap().unwrap();
        assert!(retained.released);
        assert_eq!(retained.resources.disk_mi_b, allowance);
    }
}

#[tokio::test]
async fn native_same_node_resume_reuses_retained_capacity_on_a_small_pool() {
    let f = Fixture::new().await;
    let mut node = f.node.clone();
    node["limits"]["diskMiB"] = 32768.into();
    node["diskGrowthProtocol"] = 1.into();
    node["diskCapacity"] = json!({ "protocol": 1, "capturedAt": now(), "usedBytes": 0,
        "freeBytes": 900 * 1024 * 1024 * 1024u64, "ownedBytes": {}, "retainedBytes": {}, "requests": [] });
    f.service
        .store
        .put_node_fixture(node.clone())
        .await
        .unwrap();
    let mut run = f.run();
    common::add_run(&f.service.store, &run).await;
    let first = id();
    let selected = placement::reserve(&f.service, &run, &first).await.unwrap();
    assert_eq!(selected["resources"]["diskMiB"], 31_000);
    common::set_checkpoint(
        &f.service.store,
        text(&run, "id"),
        json!({
            "nodeId": node["id"], "runnerId": first, "resources": selected["resources"],
        }),
    )
    .await;
    placement::materialize(&f.service, &first).await.unwrap();
    placement::release(&f.service, &first).await.unwrap();
    let volume_id = format!("{}:{}", text(&run, "id"), text(&node, "id"));
    let mut volume = f
        .service
        .store
        .get("node-volumes", &volume_id)
        .await
        .unwrap()
        .unwrap();
    volume["materialized"] = true.into();
    volume["storageMode"] = "on-demand".into();
    volume["diskMiB"] = 256.into();
    volume["activeDiskMiB"] = 256.into();
    f.service.store.put("node-volumes", volume).await.unwrap();
    node["diskCapacity"]["usedBytes"] = (256 * 1024 * 1024u64).into();
    node["diskCapacity"]["retainedBytes"] = json!({ text(&run, "id"): 256 * 1024 * 1024u64 });
    f.service.store.put_node_fixture(node).await.unwrap();
    run["status"] = "queued".into();
    run["finishedAt"] = Value::Null;
    run["recoveryPending"] = true.into();
    f.service
        .store
        .change_run_fixture(text(&run, "id"), run.clone())
        .await
        .unwrap();
    placement::check(&f.service, &run).await.unwrap();
    let selected = placement::reserve(&f.service, &run, &id()).await.unwrap();
    assert_eq!(selected["resources"]["diskMiB"], 31_000);
}

#[tokio::test]
async fn native_disk_growth_is_idempotent_and_competes_atomically_with_admission() {
    let f = Fixture::new().await;
    let gib = 1024 * 1024 * 1024u64;
    let mut node = f.node.clone();
    node["slots"] = 4.into();
    node["limits"]["diskMiB"] = (80 * 1024).into();
    node["capabilities"]["diskTotalMiB"] = (1024 * 1024).into();
    node["capabilities"]["diskMiB"] = (900 * 1024).into();
    f.service
        .store
        .put_node_fixture(node.clone())
        .await
        .unwrap();
    let token = auth::token();
    f.service
        .store
        .set(
            &format!("node-token:{}", auth::digest(&token)),
            node["id"].clone(),
            None,
        )
        .await
        .unwrap();
    let mut attempts = Vec::new();
    let mut runs = Vec::new();
    for _ in 0..2 {
        let mut run = f.run();
        run["status"] = "running".into();
        run["resources"] = json!({ "cpu": 2, "memoryMiB": 4096, "diskMiB": 32768 });
        common::add_run(&f.service.store, &run).await;
        let attempt = id();
        placement::reserve(&f.service, &run, &attempt)
            .await
            .unwrap();
        common::set_checkpoint(
            &f.service.store,
            text(&run, "id"),
            json!({ "nodeId": node["id"], "runnerId": attempt }),
        )
        .await;
        attempts.push(attempt);
        runs.push(run);
    }
    let body = json!({
        "backend": "native", "nodeProtocol": 2, "executionReady": true, "dataRoot": "/fixture",
        "runtimeId": "development",
        "capabilities": node["capabilities"], "pool": { "capacity": 4 },
        "sharedResources": true, "budget": { "slots": 4, "limits": node["limits"] },
        "nativeMode": { "mode": "protected", "generation": null, "status": "applied" },
        "usage": { "memoryMiB": 0, "diskMiB": 16384, "diskBytes": 16 * gib },
        "diskGrowth": { "protocol": 1, "capturedAt": now(), "usedBytes": 16 * gib, "freeBytes": 900 * gib,
            "ownedBytes": { &attempts[0]: 8 * gib, &attempts[1]: 8 * gib },
            "requests": [
                { "attemptId": attempts[0], "currentMiB": 32768 },
                { "attemptId": attempts[1], "currentMiB": 32768 },
            ] },
    });
    for _ in 0..2 {
        let request = Request::builder()
            .method("POST")
            .uri("/internal/nodes/heartbeat")
            .header("host", common::HOST)
            .header("authorization", format!("Bearer {token}"))
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();
        let response = common::send(&f.router, request).await;
        assert_eq!(response.status(), StatusCode::OK);
        let reply = read_json(response).await;
        assert_eq!(
            reply["diskGrowth"]["grants"][0]["allowanceMiB"], 40960,
            "{reply}"
        );
        assert_eq!(
            reply["diskGrowth"]["grants"][1]["allowanceMiB"], 32768,
            "{reply}"
        );
    }
    assert_eq!(
        f.service
            .get_node(text(&node, "id"))
            .await
            .unwrap()
            .usage
            .unwrap()
            .disk_bytes,
        Some(16 * gib),
        "Retain the exact disk bytes emitted by native Controller::usage"
    );
    assert_eq!(
        f.service.store.run(text(&runs[0], "id")).await.unwrap()["resources"]["diskMiB"],
        40960
    );
    let incoming = f.run();
    common::add_run(&f.service.store, &incoming).await;
    assert!(
        placement::reserve(&f.service, &incoming, &id())
            .await
            .is_err()
    );
    placement::release(&f.service, &attempts[0]).await.unwrap();
    placement::release(&f.service, &attempts[0]).await.unwrap();
    assert!(
        placement::reserve(&f.service, &incoming, &id())
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn native_disk_growth_stops_at_the_task_limit_and_preserves_older_allowances() {
    let gib = 1024 * 1024 * 1024u64;
    for (limit_gib, initial_gib, pool_gib) in [
        (150u64, 144u64, 158u64),
        (150, 150, 1024),
        (150, 504, 1024),
        (75, 72, 80),
        (300, 296, 1024),
        (200, 144, 1024),
    ] {
        let f = Fixture::new().await;
        let owner_token = common::relay_fixture::grant(&f.service, &["read", "manage"]).await;
        let read = common::request("GET", "/api/settings/task-storage")
            .header("x-test-relay-token", &owner_token)
            .body(Body::empty())
            .unwrap();
        let response = common::send(&f.router, read).await;
        assert_eq!(response.status(), StatusCode::OK);
        let current = read_json(response).await;
        let save = common::request("PUT", "/api/settings/task-storage")
            .header("x-test-relay-token", &owner_token)
            .body(Body::from(
                json!({
                    "maxDiskGiB": limit_gib, "revision": current["revision"],
                })
                .to_string(),
            ))
            .unwrap();
        assert_eq!(common::send(&f.router, save).await.status(), StatusCode::OK);
        let mut node = f.node.clone();
        node["slots"] = 4.into();
        // Only the final 6 GiB increment fits this pool, rather than a full 8 GiB.
        node["limits"]["diskMiB"] = (pool_gib * 1024).into();
        node["capabilities"]["diskTotalMiB"] = (2048 * 1024).into();
        node["capabilities"]["diskMiB"] = (1800 * 1024).into();
        f.service
            .store
            .put_node_fixture(node.clone())
            .await
            .unwrap();
        let token = auth::token();
        f.service
            .store
            .set(
                &format!("node-token:{}", auth::digest(&token)),
                node["id"].clone(),
                None,
            )
            .await
            .unwrap();

        let mut run = f.run();
        run["status"] = "running".into();
        run["resources"] = json!({
            "cpu": 2, "memoryMiB": 4096, "diskMiB": initial_gib * 1024,
        });
        common::add_run(&f.service.store, &run).await;
        let attempt = id();
        placement::reserve(&f.service, &run, &attempt)
            .await
            .unwrap();
        common::set_checkpoint(
            &f.service.store,
            text(&run, "id"),
            json!({
                "nodeId": node["id"], "runnerId": attempt,
            }),
        )
        .await;

        let expected_mib = initial_gib.max((initial_gib + 8).min(limit_gib)) * 1024;
        let mut active_limit_gib = limit_gib;
        // Replaying a lost response must not grant a second increment.
        for (index, current_mib) in [initial_gib * 1024, initial_gib * 1024, expected_mib]
            .into_iter()
            .enumerate()
        {
            if limit_gib == 200 && index == 1 {
                // The first grant was persisted, but its response was lost. The
                // owner lowers the cap before the controller retries at 144 GiB.
                let current = f
                    .service
                    .store
                    .kv("task-storage-limits")
                    .await
                    .unwrap()
                    .unwrap();
                let save = common::request("PUT", "/api/settings/task-storage")
                    .header("x-test-relay-token", &owner_token)
                    .body(Body::from(
                        json!({
                            "maxDiskGiB": 150, "revision": current["revision"],
                        })
                        .to_string(),
                    ))
                    .unwrap();
                assert_eq!(common::send(&f.router, save).await.status(), StatusCode::OK);
                active_limit_gib = 150;
            }
            let body = json!({
                "backend": "native", "nodeProtocol": 2, "executionReady": true,
                "dataRoot": "/fixture", "runtimeId": "development",
                "capabilities": node["capabilities"], "pool": { "capacity": 4 },
                "sharedResources": true, "budget": { "slots": 4, "limits": node["limits"] },
                "nativeMode": { "mode": "protected", "generation": null, "status": "applied" },
                "usage": { "diskMiB": 0, "diskBytes": 0 },
                "diskGrowth": {
                    "protocol": 1, "capturedAt": now(), "usedBytes": 0,
                    "freeBytes": 1800 * gib, "ownedBytes": {},
                    "requests": [{ "attemptId": attempt, "currentMiB": current_mib }],
                },
            });
            let request = Request::builder()
                .method("POST")
                .uri("/internal/nodes/heartbeat")
                .header("host", common::HOST)
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap();
            let response = common::send(&f.router, request).await;
            assert_eq!(response.status(), StatusCode::OK);
            let reply = read_json(response).await;
            let grant = &reply["diskGrowth"]["grants"][0];
            assert_eq!(
                reply["diskGrowth"]["maxTaskDiskMiB"],
                active_limit_gib * 1024
            );
            assert_eq!(grant["allowanceMiB"], expected_mib, "{reply}");
            assert_eq!(
                grant["previousAllowanceMiB"],
                if index == 0 {
                    initial_gib * 1024
                } else {
                    expected_mib
                }
            );
            assert_eq!(
                grant["reason"],
                if current_mib < expected_mib {
                    "granted"
                } else {
                    "task-limit"
                }
            );
            assert_eq!(
                f.service.store.run(text(&run, "id")).await.unwrap()["resources"]["diskMiB"],
                expected_mib,
            );
            assert_eq!(
                f.service
                    .store
                    .kv(&format!("native-capacity-pressure:{}", text(&node, "id")))
                    .await
                    .unwrap(),
                None,
                "Reaching the task limit must not evict other conversations' caches",
            );
        }
    }
}

#[tokio::test]
async fn storage_pressure_reserves_waiting_admission_before_another_growth_increment() {
    let f = Fixture::new().await;
    let gib = 1024 * 1024 * 1024u64;
    let mut node = f.node.clone();
    node["slots"] = 4.into();
    node["limits"]["diskMiB"] = (80 * 1024).into();
    node["capabilities"]["diskTotalMiB"] = (1024 * 1024).into();
    node["capabilities"]["diskMiB"] = (900 * 1024).into();
    f.service
        .store
        .put_node_fixture(node.clone())
        .await
        .unwrap();
    let token = auth::token();
    f.service
        .store
        .set(
            &format!("node-token:{}", auth::digest(&token)),
            node["id"].clone(),
            None,
        )
        .await
        .unwrap();

    let mut running = f.run();
    running["status"] = "running".into();
    running["isolated"] = true.into();
    common::add_run(&f.service.store, &running).await;
    let attempt = id();
    placement::reserve(&f.service, &running, &attempt)
        .await
        .unwrap();
    common::set_checkpoint(
        &f.service.store,
        text(&running, "id"),
        json!({
            "nodeId": node["id"], "runnerId": attempt
        }),
    )
    .await;
    let mut waiting = f.run();
    waiting["isolated"] = true.into();
    common::add_run(&f.service.store, &waiting).await;

    for (current, expected, retrying) in [
        (32768, 40960, false),
        (40960, 40960, false),
        (40960, 49152, true),
    ] {
        if retrying {
            common::set_checkpoint(
                &f.service.store,
                text(&waiting, "id"),
                json!({
                    "nodeId": node["id"], "capacityRetryAt": now() + 30_000
                }),
            )
            .await;
        }
        let body = json!({
            "backend": "native", "nodeProtocol": 2, "executionReady": true,
            "dataRoot": "/fixture", "runtimeId": "development",
            "capabilities": node["capabilities"], "pool": { "capacity": 4 },
            "sharedResources": true, "budget": { "slots": 4, "limits": node["limits"] },
            "nativeMode": { "mode": "protected", "generation": null, "status": "applied" },
            "usage": { "diskMiB": 8192 },
            "diskGrowth": {
                "protocol": 1, "capturedAt": now(), "usedBytes": 8 * gib,
                "freeBytes": 900 * gib, "ownedBytes": { &attempt: 8 * gib },
                "requests": [{ "attemptId": attempt, "currentMiB": current }]
            }
        });
        let request = Request::builder()
            .method("POST")
            .uri("/internal/nodes/heartbeat")
            .header("host", common::HOST)
            .header("authorization", format!("Bearer {token}"))
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();
        let response = common::send(&f.router, request).await;
        assert_eq!(response.status(), StatusCode::OK);
        let reply = read_json(response).await;
        assert_eq!(
            reply["diskGrowth"]["grants"][0]["allowanceMiB"], expected,
            "Growth must leave room for the queued conversation: {reply}"
        );
        if current == 40960 && !retrying {
            assert!(placement::check(&f.service, &waiting).await.is_ok());
        }
    }
}

#[tokio::test]
async fn wildcard_node_access_still_requires_trusted_contributors() {
    let f = Fixture::new().await;
    let mut agent = f.agent.clone();
    agent["access"]["nodes"] = Value::Null;
    let agent = f
        .service
        .agent_fixture(agent, f.agent["id"].as_str())
        .await
        .unwrap();
    assert!(agent["access"]["nodes"].is_null());
    let mut run = f.run();
    let trusted = run.clone();
    let node = f.node.clone();
    assert!(
        f.service
            .store
            .read(move |db| policy::allows(db, &trusted, &node))
            .await
            .unwrap()
    );
    run["nativePrincipals"] = json!([TRUSTED, OTHER]);
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
async fn native_nodes_admit_six_attempts_only_after_the_configured_budget_is_applied() {
    let f = Fixture::new().await;
    let (status, configured) = f
        .owner_call(
            "PUT",
            &format!("/api/nodes/{}", text(&f.node, "id")),
            json!({
                "name": f.node["name"], "tags": [], "accepting": true,
                "limits": f.node["limits"], "slots": 6,
            }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{configured}");
    let token = auth::token();
    f.service
        .store
        .set(
            &format!("node-token:{}", auth::digest(&token)),
            f.node["id"].clone(),
            None,
        )
        .await
        .unwrap();
    let send = |slots| {
        let body = json!({
            "runtimeId": "development",
            "capabilities": f.node["capabilities"], "dataRoot": "/private/native-node/data",
            "backend": "native", "nodeProtocol": 2, "executionReady": true,
            "nativeMode": f.node["nativeModeObserved"],
            "budget": { "slots": slots, "limits": f.node["limits"] },
            "pool": { "capacity": slots },
        });
        common::send(
            &f.router,
            Request::builder()
                .method("POST")
                .uri("/internal/nodes/heartbeat")
                .header("host", common::HOST)
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
    };
    assert_eq!(send(1).await.status(), StatusCode::OK);
    let run = f.run();
    common::add_run(&f.service.store, &run).await;
    assert!(placement::reserve(&f.service, &run, &id()).await.is_err());
    assert_eq!(send(6).await.status(), StatusCode::OK);
    let mut attempts = Vec::new();
    for _ in 0..6 {
        let run = f.run();
        common::add_run(&f.service.store, &run).await;
        let attempt = id();
        placement::reserve(&f.service, &run, &attempt)
            .await
            .unwrap();
        attempts.push(attempt);
    }
    let overflow = f.run();
    common::add_run(&f.service.store, &overflow).await;
    assert!(
        placement::reserve(&f.service, &overflow, &id())
            .await
            .is_err()
    );
    placement::release(&f.service, &attempts[0]).await.unwrap();
    placement::reserve(&f.service, &overflow, &id())
        .await
        .unwrap();
}

#[tokio::test]
async fn native_mode_default_protected_failure_is_observable_without_a_saved_request() {
    let f = Fixture::new().await;
    let token = auth::token();
    f.service
        .store
        .set(
            &format!("node-token:{}", auth::digest(&token)),
            f.node["id"].clone(),
            None,
        )
        .await
        .unwrap();
    let mut capabilities = f.node["capabilities"].clone();
    capabilities["nativeModeProtocol"] = 1.into();
    for state in ["failed", "pending", "applied"] {
        let body = json!({
            "runtimeId": "development", "capabilities": capabilities,
            "dataRoot": "/private/native-node/data", "backend": "native", "nodeProtocol": 2,
            "executionReady": true, "budget": { "slots": 1, "limits": f.node["limits"] },
            "pool": { "capacity": 1 },
            "nativeMode": { "mode": "protected", "generation": null, "status": state },
        });
        let request = Request::builder()
            .method("POST")
            .uri("/internal/nodes/heartbeat")
            .header("host", common::HOST)
            .header("authorization", format!("Bearer {token}"))
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();
        let response = common::send(&f.router, request).await;
        assert_eq!(response.status(), StatusCode::OK);
        let node = f.service.node_boundary(text(&f.node, "id")).await.unwrap();
        assert!(
            node["nativeModeRequest"].is_null(),
            "No default migration creates an elevation request"
        );
        assert_eq!(node["nativeModeStatus"], state);
        assert_eq!(node["executionReady"], state == "applied");
        let (status, listed) = f.owner_call("GET", "/api/nodes", Value::Null).await;
        assert_eq!(status, StatusCode::OK);
        let public = listed
            .as_array()
            .unwrap()
            .iter()
            .find(|node| node["id"] == f.node["id"])
            .unwrap();
        assert_eq!(public["nativeModeStatus"], state);
    }
}

#[tokio::test]
async fn revoking_a_native_person_stops_lease_renewals_and_workspace_access() {
    let f = Fixture::new().await;
    let mut run = f.run();
    run["status"] = "running".into();
    common::add_run(&f.service.store, &run).await;
    let attempt = id();
    common::set_checkpoint(
        &f.service.store,
        run["id"].as_str().unwrap(),
        json!({ "runnerId": attempt, "nodeId": f.node["id"] }),
    )
    .await;
    f.service
        .store
        .put_attempt_fixture(json!({
            "id": attempt,
            "runId": run["id"],
            "nodeId": f.node["id"],
            "released": false,
            "leaseExpiresAt": now() + 60000,
        }))
        .await
        .unwrap();
    let token = auth::token();
    f.service
        .store
        .set(
            &format!("node-token:{}", auth::digest(&token)),
            f.node["id"].clone(),
            None,
        )
        .await
        .unwrap();
    let heartbeat = json!({
        "runtimeId": "development",
        "capabilities": f.node["capabilities"],
        "dataRoot": "/private/native-node/data",
        "backend": "native",
        "nodeProtocol": 2,
        "executionReady": true,
        "nativeMode": f.node["nativeModeObserved"],
        "budget": { "slots": 1, "limits": f.node["limits"] },
        "pool": { "capacity": 1 },
    });
    let send = |path: &str, body: Value| {
        let request = Request::builder()
            .method("POST")
            .uri(path)
            .header("host", common::HOST)
            .header("authorization", format!("Bearer {token}"))
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();
        common::send(&f.router, request)
    };
    let plans = f.service.config.data_dir.join("runner-plans");
    std::fs::create_dir_all(&plans).unwrap();
    std::fs::write(
        plans.join(format!("{attempt}.json")),
        json!({ "runId": run["id"], "backend": "native" }).to_string(),
    )
    .unwrap();
    let workspace = format!("/internal/node-workspace/{attempt}/plan");
    let heartbeat_path = "/internal/nodes/heartbeat";

    // During a Beacon outage, the cached proof can renew existing work up to
    // its deadline. Removing the local grant still blocks renewal and access.
    common::relay_fixture::native_authority_unavailable(&f.service).await;
    f.service
        .store
        .set(
            "native-authority",
            json!({
                "observedAt": now() - 30_000,
                "accounts": { TRUSTED: {"email":"trusted@example.test","accessId":"one"} }
            }),
            None,
        )
        .await
        .unwrap();
    let first = read_json(send(heartbeat_path, heartbeat.clone()).await).await;
    assert_eq!(first["leases"].as_array().unwrap().len(), 1);
    let remaining = first["leases"][0]["remainingMs"].as_u64().unwrap();
    assert!(
        remaining > 0 && remaining <= 30_000,
        "grant must retain the proof deadline: {first}"
    );
    assert_eq!(send(&workspace, json!({})).await.status(), StatusCode::OK);

    let mut node = f
        .service
        .node_boundary(f.node["id"].as_str().unwrap())
        .await
        .unwrap();
    assert_eq!(node["executionReady"], true);
    node["nativeAccounts"] = json!([]);
    f.service.store.put_node_fixture(node).await.unwrap();
    let renewed = read_json(send(heartbeat_path, heartbeat).await).await;
    assert!(renewed["leases"].as_array().unwrap().is_empty());
    assert_eq!(
        send(&workspace, json!({})).await.status(),
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn expired_authority_and_removed_members_fail_closed() {
    let f = Fixture::new().await;
    f.service
        .store
        .set(
            "native-authority",
            json!({ "observedAt": now() - 60001, "accounts": { TRUSTED: {} } }),
            None,
        )
        .await
        .unwrap();
    let (run, node) = (f.run(), f.node.clone());
    assert!(
        !f.service
            .store
            .read(move |db| policy::allows(db, &run, &node))
            .await
            .unwrap()
    );
    f.service
        .store
        .set(
            "native-authority",
            json!({ "observedAt": now(), "accounts": {} }),
            None,
        )
        .await
        .unwrap();
    let (run, node) = (f.run(), f.node.clone());
    assert!(
        !f.service
            .store
            .read(move |db| policy::allows(db, &run, &node))
            .await
            .unwrap()
    );
}
