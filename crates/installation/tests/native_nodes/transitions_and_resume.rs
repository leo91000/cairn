use super::*;

#[tokio::test]
async fn native_mode_confirmation_covers_incoming_moves_and_reservations() {
    let f = Fixture::new().await;
    let node_id = text(&f.node, "id");
    let mut node = f.node.clone();
    node["capabilities"]["nativeModeProtocol"] = 1.into();
    f.service.store.put_node_fixture(node).await.unwrap();
    let source = id();
    let mut incoming = Vec::new();
    for selection in ["move", "reservation", "later"] {
        let mut run = f.run();
        run["nodeId"] = source.clone().into();
        if selection == "move" {
            run["moveRequest"] = json!({ "nodeId": node_id, "reservation": id() });
        }
        common::add_run(&f.service.store, &run).await;
        if selection == "reservation" {
            f.service
                .store
                .put_attempt_fixture(json!({
                    "id": id(), "runId": run["id"], "nodeId": node_id, "released": false,
                }))
                .await
                .unwrap();
        }
        incoming.push(run);
    }
    let path = format!("/api/nodes/{node_id}/native-mode");
    let (status, preview) = f
        .owner_call("POST", &path, json!({ "mode": "administrator" }))
        .await;
    assert_eq!(status, StatusCode::OK, "{preview}");
    let listed = preview["preview"]["conversations"].as_array().unwrap();
    assert_eq!(listed.len(), 2);
    for run in &incoming[..2] {
        assert!(listed.iter().any(|listed| listed["id"] == run["id"]));
    }
    f.service
        .store
        .update_run_boundary_fixture(
            text(&incoming[2], "id"),
            json!({
                "moveRequest": { "nodeId": node_id, "reservation": id() },
            }),
        )
        .await
        .unwrap();
    let (status, _) = f
        .owner_call(
            "POST",
            &path,
            json!({
                "mode": "administrator", "previewId": preview["previewId"], "confirm": true,
            }),
        )
        .await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "A newly incoming conversation requires review"
    );
    let (_, preview) = f
        .owner_call("POST", &path, json!({ "mode": "administrator" }))
        .await;
    assert_eq!(
        preview["preview"]["conversations"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
    // A new attempt for an already displayed conversation also invalidates the
    // interruption snapshot, even though the conversation count stays the same.
    f.service
        .store
        .put_attempt_fixture(json!({
            "id": id(), "runId": incoming[0]["id"], "nodeId": node_id, "released": false,
        }))
        .await
        .unwrap();
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
    assert!(f.service.node_boundary(node_id).await.unwrap()["nativeModeRequest"].is_null());
}

#[tokio::test]
async fn native_mode_transition_and_unavailability_keep_the_environment_on_its_source() {
    use cairn_installation::nodes::moves;

    let f = Fixture::new().await;
    let source = text(&f.node, "id");
    let destination = id();
    let mut other = f.node.clone();
    other["id"] = destination.clone().into();
    f.service.store.put_node_fixture(other).await.unwrap();
    let mut agent = f.agent.clone();
    agent["access"]["nodes"] = json!([source, destination]);
    f.service
        .store
        .put_agent_fixture(agent.clone())
        .await
        .unwrap();
    let mut run = f.run();
    let run_id = text(&run, "id").to_owned();
    run["nodeId"] = source.into();
    run["snapshot"]["agent"] = agent;
    run["nativeAuthorityNodes"] = json!([source, destination]);
    run["resources"] = json!({ "cpu": 1, "memoryMiB": 128, "diskMiB": 128 });
    common::add_run(&f.service.store, &run).await;
    common::set_checkpoint(
        &f.service.store,
        &run_id,
        json!({
            "nodeId": source, "runtimeId": "development", "completed": true,
        }),
    )
    .await;
    let mut node = f.node.clone();
    let generation = id();
    node["capabilities"]["nativeModeProtocol"] = 1.into();
    node["nativeModeRequest"] = json!({ "mode": "administrator", "generation": generation });
    node["nativeReviewedAgents"] = json!([f.agent["id"]]);
    node["executionReady"] = false.into();
    for state in ["pending", "failed"] {
        node["nativeModeObserved"] = json!({
            "mode": "protected", "generation": null, "status": state,
        });
        f.service
            .store
            .put_node_fixture(node.clone())
            .await
            .unwrap();
        assert!(!moves::node_ready(&f.service, &run).await.unwrap());
        assert!(f.service.store.run(&run_id).await.unwrap()["moveRequest"].is_null());
        assert!(f.service.store.attempts_fixture().await.unwrap().is_empty());
        assert_eq!(
            f.service
                .store
                .checkpoint_boundary(&run_id)
                .await
                .unwrap()
                .unwrap()["nodeId"],
            source
        );
    }
    node["nativeModeObserved"] = json!({
        "mode": "administrator", "generation": generation, "status": "applied",
    });
    node["executionReady"] = true.into();
    f.service
        .store
        .put_node_fixture(node.clone())
        .await
        .unwrap();
    assert!(moves::node_ready(&f.service, &run).await.unwrap());
    // Genuine unavailability after confirmation waits for the node: no relocation.
    node["executionReady"] = false.into();
    f.service.store.put_node_fixture(node).await.unwrap();
    assert!(!moves::node_ready(&f.service, &run).await.unwrap());
    let waiting = f.service.store.run(&run_id).await.unwrap();
    assert!(waiting["moveRequest"].is_null());
    assert_eq!(waiting["nodeState"], "waiting-for-node");
    assert!(f.service.store.attempts_fixture().await.unwrap().is_empty());
    assert_eq!(
        f.service
            .store
            .checkpoint_boundary(&run_id)
            .await
            .unwrap()
            .unwrap()["nodeId"],
        source
    );
}

#[tokio::test]
async fn deleting_an_agent_revokes_admin_review_before_reusing_its_creation_id() {
    let f = Fixture::new().await;
    let node_id = text(&f.node, "id");
    let mut node = f.node.clone();
    let generation = id();
    node["nativeModeRequest"] = json!({ "mode": "administrator", "generation": generation });
    node["nativeModeObserved"] =
        json!({ "mode": "administrator", "generation": generation, "status": "applied" });
    node["capabilities"]["nativeModeProtocol"] = 1.into();
    node["nativeReviewedAgents"] = json!([f.agent["id"]]);
    let original = f.run();
    let before = node.clone();
    assert!(
        f.service
            .store
            .read(move |db| policy::allows(db, &original, &before))
            .await
            .unwrap()
    );
    f.service.store.put_node_fixture(node).await.unwrap();
    let path = format!("/api/agents/{}", text(&f.agent, "id"));
    let (status, result) = f.owner_call("DELETE", &path, json!({})).await;
    assert_eq!(status, StatusCode::OK, "{result}");
    assert!(
        f.service.node_boundary(node_id).await.unwrap()["nativeReviewedAgents"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let (status, recreated) = f.owner_call("POST", "/api/agents", json!({
        "creationId": f.agent["id"], "name": "Replayed creation", "access": { "backend": "native", "nodes": null }
    })).await;
    assert_eq!(status, StatusCode::OK, "{recreated}");
    assert_eq!(recreated["id"], f.agent["id"]);
    let node = f.service.node_boundary(node_id).await.unwrap();
    assert!(!cairn_installation::nodes::native_mode::reviewed(
        &node, &recreated
    ));
    let (status, _) = f.owner_call("PUT", &path, json!({ "name": "Explicit replay", "access": { "backend": "native", "nodes": [node_id] } })).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn run_resume_selection_authorizes_before_catalog_discovery_over_http_and_public_mcp() {
    let f = Fixture::with_codex_fixture().await;
    let (chat, queued_run) = f.conversation().await;
    let (chat_id, run_id) = (
        text(&chat, "id").to_owned(),
        text(&queued_run, "id").to_owned(),
    );
    let turn_id = text(&queued_run["chatExecution"], "messageId").to_owned();
    let discovery_log = f.service.config.home.join(".codex/fixture-lifecycle.jsonl");
    std::fs::create_dir_all(discovery_log.parent().unwrap()).unwrap();
    std::fs::write(&discovery_log, "").unwrap();
    assert!(f.service.store.kv("codex-models:").await.unwrap().is_none());
    f.service
        .store
        .change_run(
            &run_id,
            cairn_installation::runs::RunChanges {
                status: Some(cairn_contracts::RunStatus::Cancelled),
                resume_available: Some(true),
                session_id: Some(Some("retained-session".into())),
                workspace: Some(Some("/saved/native/workspace".into())),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    common::set_checkpoint(
        &f.service.store,
        &run_id,
        json!({
            "launched": true,
            "prepared": { "cwd": "/saved/native/workspace" },
            "generation": "retained-generation",
        }),
    )
    .await;
    let before = f.service.store.run(&run_id).await.unwrap();
    let path = format!("/api/runs/{run_id}/resume");
    let body = json!({
        "turnId": turn_id,
        "selection": {
            "model": "fixture-fast",
            "reasoning": "high",
            "speed": "priority",
        },
    });

    let missing_turn = f
        .call(
            &f.trusted,
            "POST",
            &path,
            json!({ "selection": body["selection"] }),
        )
        .await;
    assert_eq!(
        missing_turn.0,
        StatusCode::BAD_REQUEST,
        "{}",
        missing_turn.1
    );
    assert_eq!(f.service.store.run(&run_id).await.unwrap(), before);
    assert!(std::fs::read_to_string(&discovery_log).unwrap().is_empty());
    assert!(f.service.store.kv("codex-models:").await.unwrap().is_none());

    let (status, denied) = f.call(&f.other, "POST", &path, body.clone()).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{denied}");
    assert_eq!(f.service.store.run(&run_id).await.unwrap(), before);
    assert!(std::fs::read_to_string(&discovery_log).unwrap().is_empty());
    assert!(f.service.store.kv("codex-models:").await.unwrap().is_none());

    let denied_public = public_mcp_call(
        &f,
        OTHER,
        &["run"],
        "resume_run",
        json!({
            "runId": run_id,
            "turnId": turn_id,
            "selection": body["selection"],
        }),
    )
    .await;
    assert_eq!(denied_public["result"]["isError"], true, "{denied_public}");
    assert_eq!(f.service.store.run(&run_id).await.unwrap(), before);
    assert!(std::fs::read_to_string(&discovery_log).unwrap().is_empty());
    assert!(f.service.store.kv("codex-models:").await.unwrap().is_none());

    let (status, resumed) = f.call(&f.trusted, "POST", &path, body).await;
    assert_eq!(status, StatusCode::OK, "{resumed}");
    let after = f.service.store.run(&run_id).await.unwrap();
    assert_eq!(after["status"], "queued");
    assert_eq!(after["snapshot"]["agent"]["model"], "fixture-fast");
    assert_eq!(after["snapshot"]["agent"]["reasoning"], "high");
    assert_eq!(after["snapshot"]["agent"]["speed"], "priority");
    assert_eq!(after["id"], before["id"]);
    assert_eq!(after["chatExecution"]["messageId"], turn_id);
    assert!(
        std::fs::read_to_string(&discovery_log)
            .unwrap()
            .contains("model/list")
    );
    assert!(f.service.conversation_boundary(&chat_id).await.is_ok());
}

#[tokio::test]
async fn public_mcp_resumes_native_missions_with_verified_owner_authority() {
    let f = Fixture::new().await;
    let run = saved_native_mission(&f).await;
    let run_id = text(&run, "id");
    let args = json!({ "runId": run_id });
    for (account, scopes) in [(OTHER, vec!["run"]), (TRUSTED, vec!["read"])] {
        let response = public_mcp_call(&f, account, &scopes, "resume_run", args.clone()).await;
        assert_eq!(response["result"]["isError"], true, "{response}");
        assert_eq!(f.service.store.run(run_id).await.unwrap(), run);
    }
    let response = public_mcp_call(&f, TRUSTED, &["run"], "resume_run", args).await;
    assert_ne!(response["result"]["isError"], true, "{response}");
    let resumed = f.service.store.run(run_id).await.unwrap();
    assert_eq!(resumed["nativePrincipals"], json!([TRUSTED]));
    assert_eq!(resumed["nativeProvenanceUnknown"], false);
    assert_eq!(resumed["sessionId"], "retained-session");
    f.service.worker.cancel(&f.service, run_id).await.unwrap();
}

#[tokio::test]
async fn resume_run_preserves_native_human_and_delegated_provenance() {
    let f = Fixture::new().await;
    let run = saved_native_mission(&f).await;
    let run_id = text(&run, "id");
    let path = format!("/api/runs/{run_id}/resume");
    let (status, _) = f.call(&f.other, "POST", &path, json!({})).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(f.service.store.run(run_id).await.unwrap(), run);
    let public = common::relay_fixture::grant(&f.service, &["run"]).await;
    let request = common::request("POST", "/mcp")
        .header("authorization", format!("Bearer {public}"))
        .body(Body::from(
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": common::mcp_arguments("resume_run", &json!({ "runId": run_id })),
            })
            .to_string(),
        ))
        .unwrap();
    let denied = read_json(common::send(&f.router, request).await).await;
    assert_eq!(denied["result"]["isError"], true, "{denied}");
    let (_, unknown) = resume_coordinator(&f, false).await;
    let denied = delegated_call(&f, &unknown, "resume_run", json!({ "runId": run_id })).await;
    assert_eq!(denied["result"]["isError"], true, "{denied}");
    assert_eq!(f.service.store.run(run_id).await.unwrap(), run);
    let (caller, traced) = resume_coordinator(&f, true).await;
    let resumed = delegated_call(&f, &traced, "resume_run", json!({ "runId": run_id })).await;
    assert_ne!(resumed["result"]["isError"], true, "{resumed}");
    let after = f.service.store.run(run_id).await.unwrap();
    assert_eq!(after["nativeParentRunId"], caller["id"]);
    assert_eq!(after["nativePrincipals"], json!([TRUSTED]));
    assert_eq!(after["sessionId"], "retained-session");
    f.service.worker.cancel(&f.service, run_id).await.unwrap();
    f.service
        .store
        .change_run(
            run_id,
            cairn_installation::runs::RunChanges {
                status: Some(cairn_contracts::RunStatus::Cancelled),
                recovery_pending: Some(false),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let (status, _) = f.call(&f.trusted, "POST", &path, json!({})).await;
    assert_eq!(status, StatusCode::OK);
    let after = f.service.store.run(run_id).await.unwrap();
    assert_eq!(after["nativePrincipals"], json!([TRUSTED]));
    assert_eq!(after["nativeParentRunId"], caller["id"]);
}

#[tokio::test]
async fn resume_run_rechecks_grant_and_cancellation_after_native_preparation() {
    for revoke in [true, false] {
        let f = Fixture::new().await;
        let target = saved_native_mission(&f).await;
        let (caller, configuration) = resume_coordinator(&f, true).await;
        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let server = Router::new().route("/api/relay/00000000-0000-4000-8000-000000000055/native-authority", axum::routing::get({
            let entered = entered.clone();
            let release = release.clone();
            move || {
                let entered = entered.clone();
                let release = release.clone();
                async move {
                    entered.notify_one();
                    release.notified().await;
                    axum::Json(json!({ TRUSTED: { "email": "trusted@example.test", "accessId": "one" } }))
                }
            }
        }));
        let (origin, authority) = common::serve_locally(server).await;
        let identity_path = f
            .service
            .config
            .data_dir
            .join("installation-relay/identity.json");
        let mut identity: Value =
            serde_json::from_slice(&std::fs::read(&identity_path).unwrap()).unwrap();
        identity["origin"] = origin.into();
        std::fs::write(&identity_path, serde_json::to_vec(&identity).unwrap()).unwrap();
        f.service.store.delete("native-authority").await.unwrap();
        let attempt = delegated_call(
            &f,
            &configuration,
            "resume_run",
            json!({ "runId": target["id"] }),
        );
        tokio::pin!(attempt);
        tokio::select! {
            () = entered.notified() => {},

            response = &mut attempt => panic!("Resume did not enter preparation: {response}"),

            () = tokio::time::sleep(std::time::Duration::from_secs(5)) => panic!("Native preparation did not start"),
        }
        if revoke {
            f.service
                .mcps
                .revoke_run(&f.service, text(&caller, "id"))
                .await
                .unwrap();
        } else {
            f.service
                .store
                .change_run(
                    text(&caller, "id"),
                    cairn_installation::runs::RunChanges {
                        cancel_requested_at: Some(Some(now())),
                        ..Default::default()
                    },
                )
                .await
                .unwrap();
        }
        release.notify_one();
        let response = attempt.await;
        authority.abort();
        assert_eq!(response["result"]["isError"], true, "{response}");
        assert_eq!(
            f.service.store.run(text(&target, "id")).await.unwrap(),
            target
        );
    }
}

#[tokio::test]
async fn enrolled_native_backend_refreshes_authority_when_health_telemetry_is_missing() {
    use axum::routing::get;
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new().await;
    let app = Router::new().route(
        "/api/relay/{id}/native-authority",
        get(|| async {
            axum::Json(json!({TRUSTED: {"email":"trusted@example.test","accessId":"one"}}))
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let directory = f.service.config.data_dir.join("installation-relay");
    std::fs::create_dir_all(&directory).unwrap();
    let identity = directory.join("identity.json");
    std::fs::write(&identity,json!({"origin":format!("http://{address}"),"installationId":id(),"token":"synthetic-authority-token"}).to_string()).unwrap();
    std::fs::set_permissions(identity, std::fs::Permissions::from_mode(0o600)).unwrap();
    f.service
        .store
        .set(
            "native-authority",
            json!({"observedAt":now()-61_000,"accounts":{}}),
            None,
        )
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
    let mut run = f.run();
    run["status"] = "running".into();
    common::add_run(&f.service.store, &run).await;
    let attempt = id();
    common::set_checkpoint(
        &f.service.store,
        text(&run, "id"),
        json!({"runnerId":attempt,"nodeId":f.node["id"]}),
    )
    .await;
    f.service
        .store
        .put_attempt_fixture(
            json!({"id":attempt,"runId":run["id"],"nodeId":f.node["id"],"released":false}),
        )
        .await
        .unwrap();
    let started = now();
    let request = Request::builder()
        .method("POST")
        .uri("/internal/nodes/heartbeat")
        .header("host", common::HOST)
        .header("authorization", format!("Bearer {token}"))
        .header("content-type", "application/json")
        .body(Body::from(
            json!({"runtimeId":"development","nodeProtocol":2,"executionReady":false}).to_string(),
        ))
        .unwrap();
    let response = common::send(&f.router, request).await;
    assert_eq!(response.status(), StatusCode::OK);
    let reply = read_json(response).await;
    assert_eq!(
        reply["leases"].as_array().unwrap().len(),
        1,
        "missing telemetry must retain currently authorized native work: {reply}"
    );
    let observed = f.service.node_boundary(text(&f.node, "id")).await.unwrap();
    assert_eq!(
        observed["executionReady"], false,
        "unknown telemetry still prevents new admission"
    );
    let proof = f
        .service
        .store
        .kv("native-authority")
        .await
        .unwrap()
        .unwrap();
    assert!(proof["observedAt"].as_i64().unwrap() >= started);
    assert!(proof["accounts"].get(TRUSTED).is_some());
    let mut changed = observed;
    changed["nativeModeRequest"] = json!({"mode":"protected","generation":id()});
    f.service.store.put_node_fixture(changed).await.unwrap();
    let request = Request::builder()
        .method("POST")
        .uri("/internal/nodes/heartbeat")
        .header("host", common::HOST)
        .header("authorization", format!("Bearer {token}"))
        .header("content-type", "application/json")
        .body(Body::from(
            json!({"runtimeId":"development","nodeProtocol":2,"executionReady":false}).to_string(),
        ))
        .unwrap();
    let reply = read_json(common::send(&f.router, request).await).await;
    assert!(
        reply["leases"].as_array().unwrap().is_empty(),
        "a changed desired generation must still stop old grants"
    );
    server.abort();
}

#[tokio::test]
async fn installation_can_direct_its_own_native_run_without_widening_authority_or_parent_cycle() {
    let mut f = Fixture::new().await;
    f.agent["access"]["installation"] = true.into();
    f.service
        .store
        .put_agent_fixture(f.agent.clone())
        .await
        .unwrap();
    let mut run = f.run();
    run["status"] = "running".into();
    run["requiredTags"] = json!([]);
    common::add_run(&f.service.store, &run).await;
    let configuration = f
        .service
        .mcps
        .run_configuration(&f.service, &run)
        .await
        .unwrap();
    // A new node becomes trusted after this run received its original ceiling.
    // An agent's own directing action cannot supply new human authorization.
    let mut additional = f.node.clone();
    additional["id"] = id().into();
    f.service
        .store
        .put_node_fixture(additional.clone())
        .await
        .unwrap();
    f.agent["access"]["nodes"] = json!([f.node["id"], additional["id"]]);
    f.service
        .store
        .put_agent_fixture(f.agent.clone())
        .await
        .unwrap();
    let saved = delegated_call(
        &f,
        &configuration,
        "configure_conversation_placement",
        json!({
            "runId":run["id"], "selection":{"pinnedNodeId":null,"preferredNodeId":f.node["id"]}
        }),
    )
    .await;
    assert_ne!(saved["result"]["isError"], true, "{saved}");
    let current = f.service.store.run(text(&run, "id")).await.unwrap();
    assert!(current["nativeParentRunId"].is_null(), "{current}");
    assert_eq!(current["nativeAuthorityNodes"], json!([f.node["id"]]));
    let checked = current.clone();
    let node = f.node.clone();
    assert!(
        f.service
            .store
            .read(move |db| policy::allows(db, &checked, &node))
            .await
            .unwrap()
    );
    let checked = current.clone();
    let outside_ceiling = additional.clone();
    assert!(
        !f.service
            .store
            .read(move |db| policy::allows(db, &checked, &outside_ceiling))
            .await
            .unwrap()
    );
    let denied = delegated_move_result(
        &f,
        &configuration,
        json!({ "runId": run["id"], "nodeId": additional["id"], "discardWorkingFiles": true }),
    )
    .await;
    assert_eq!(
        denied.state,
        cairn_contracts::NodeMoveOperationState::Failed
    );
    assert_eq!(denied.error_status, Some(403), "{denied:?}");
    assert!(f.service.store.run(text(&run, "id")).await.unwrap()["moveRequest"].is_null());
    let moved = delegated_call(
        &f,
        &configuration,
        "move_to_node",
        json!({
            "runId": run["id"], "nodeId": f.node["id"], "discardWorkingFiles": true
        }),
    )
    .await;
    assert_ne!(moved["result"]["isError"], true, "{moved}");
    // A self move revokes the old turn's token: observe its committed effect rather
    // than polling with that obsolete token.
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let state = f.service.store.run(text(&run, "id")).await.unwrap();
            if state["environmentExpirationCause"] == "node-change" {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let current = f.service.store.run(text(&run, "id")).await.unwrap();
    assert!(current["nativeParentRunId"].is_null());
    assert_eq!(current["nativeAuthorityNodes"], json!([f.node["id"]]));
    assert!(current["moveRequest"].is_null());
    assert_eq!(current["targetNodeId"], f.node["id"]);
    assert_eq!(current["status"], "queued");
    let node = f.node.clone();
    assert!(
        f.service
            .store
            .read(move |db| policy::allows(db, &current, &node))
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn both_backend_change_routes_require_file_loss_confirmation_and_expire_without_conversion() {
    for destination_backend in ["native", "firecracker"] {
        for direct_move in [false, true] {
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
            let (chat, run) = f.conversation().await;
            let run_id = text(&run, "id").to_owned();
            let from = if destination_backend == "native" {
                "firecracker"
            } else {
                "native"
            };
            let (source, destination) = if from == "native" {
                (&f.node, &vm)
            } else {
                (&vm, &f.node)
            };
            let mut snapshot = run["snapshot"].clone();
            snapshot["agent"]["access"]["backend"] = from.into();
            f.service
                .store
                .change_run(
                    &run_id,
                    cairn_installation::runs::RunChanges {
                        status: Some(cairn_installation::run_status::RunStatus::Succeeded),
                        snapshot: Some(serde_json::from_value(snapshot).unwrap()),
                        session_id: Some(Some("old-engine".into())),
                        placement: Some(Box::new(
                            cairn_installation::nodes::run_placement::PlacementChanges {
                                node_id: Some(source["id"].as_str().map(str::to_owned)),
                                ..Default::default()
                            },
                        )),
                        ..Default::default()
                    },
                )
                .await
                .unwrap();
            common::set_checkpoint(&f.service.store, &run_id, json!({ "nodeId": source["id"], "runnerId": id(), "completed": true, "prepared": { "backend": from, "isolated": true } })).await;
            let path = format!(
                "/api/nodes/placement/{run_id}/{}",
                if direct_move {
                    "move"
                } else {
                    "backend-migration"
                }
            );
            let mut input = json!({ "nodeId": destination["id"] });
            if !direct_move {
                input["backend"] = destination_backend.into();
                input["confirm"] = true.into();
            }
            let (status, refused) = f.owner_call("POST", &path, input.clone()).await;
            assert_eq!(status, StatusCode::CONFLICT, "{refused}");
            assert!(
                text(&refused, "error")
                    .contains(cairn_installation::environment_expiration::WARNING),
                "{refused}"
            );
            assert_eq!(
                f.service.store.run(&run_id).await.unwrap()["sessionId"],
                "old-engine"
            );
            input["discardWorkingFiles"] = true.into();
            let (status, changed) = f.owner_call("POST", &path, input).await;
            assert_eq!(status, StatusCode::OK, "{changed}");
            let expired = f.service.store.run(&run_id).await.unwrap();
            assert_eq!(expired["environmentState"], "expired");
            assert_eq!(expired["environmentExpirationCause"], "node-change");
            assert_eq!(expired["targetNodeId"], destination["id"]);
            assert_eq!(expired["executionBackend"], destination_backend);
            assert!(expired["sessionId"].is_null());
            assert!(expired["backendMigration"].is_null());
            let checkpoint = f
                .service
                .store
                .checkpoint_boundary(&run_id)
                .await
                .unwrap()
                .unwrap();
            assert!(checkpoint["prepared"].is_null());
            assert!(checkpoint["runnerId"].is_null());
            let fresh = json!({ "completed": false, "launched": false, "turnPending": true, "remainingMs": 123000 });
            common::set_checkpoint(&f.service.store, &run_id, fresh.clone()).await;
            f.service
                .store
                .change_run(
                    &run_id,
                    cairn_installation::runs::RunChanges {
                        status: Some(cairn_contracts::RunStatus::Queued),
                        ..Default::default()
                    },
                )
                .await
                .unwrap();
            let stale = f.service.store.run(&run_id).await.unwrap();
            let (status, replaced) = f
                .owner_call(
                    "POST",
                    &format!("/api/nodes/placement/{run_id}/backend-migration"),
                    json!({ "nodeId": source["id"], "backend": from, "confirm": true }),
                )
                .await;
            assert_eq!(status, StatusCode::OK, "{replaced}");
            assert_eq!(
                f.service
                    .store
                    .checkpoint_boundary(&run_id)
                    .await
                    .unwrap()
                    .unwrap(),
                fresh
            );
            let current = f.service.store.run(&run_id).await.unwrap();
            assert_eq!(current["status"], "queued");
            assert_eq!(current["targetNodeId"], source["id"]);
            assert_eq!(current["snapshot"]["agent"]["access"]["backend"], from);
            assert_ne!(
                current["environmentAdmissionRevision"],
                stale["environmentAdmissionRevision"]
            );
            f.service.retire_environments().await.unwrap();
            let error = placement::reserve(&f.service, &stale, &id())
                .await
                .unwrap_err();
            assert!(placement::is_no_capacity(&error), "{error:?}");
            assert!(
                error.message.contains("changed during preparation"),
                "{error:?}"
            );
            assert_eq!(
                f.service
                    .conversation_boundary(text(&chat, "id"))
                    .await
                    .unwrap()["runId"],
                run_id
            );
        }
    }
}

#[tokio::test]
async fn expired_backend_override_replaces_a_pending_destination_without_pinning_or_losing_the_wait()
 {
    for pending in [false, true] {
        let mut f = Fixture::new().await;
        let mut vm = f.vm_node().await;
        vm["tags"] = json!(["vm-only"]);
        f.service.store.put_node_fixture(vm.clone()).await.unwrap();
        f.node["nativeAccounts"] = json!([TRUSTED, OTHER]);
        f.service
            .store
            .put_node_fixture(f.node.clone())
            .await
            .unwrap();
        let mut replacement = f.node.clone();
        replacement["id"] = id().into();
        f.service
            .store
            .put_node_fixture(replacement.clone())
            .await
            .unwrap();
        f.agent = f.service.agent_fixture(json!({ "access": { "backend": "auto", "nodes": [f.node["id"], vm["id"], replacement["id"]] } }), f.agent["id"].as_str()).await.unwrap();
        let (chat, run) = f.conversation().await;
        let (cid, rid) = (text(&chat, "id").to_owned(), text(&run, "id").to_owned());
        let wake = json!({ "id": id(), "messageId": run["chatExecution"]["messageId"], "seconds": 1, "createdAt": now() - 2000, "wakeAt": now() - 1000, "reason": "Continue after the wait" });
        let (chat_id, run_id, source, wakeup) =
            (cid.clone(), rid.clone(), f.node["id"].clone(), wake.clone());
        f.service.store.transaction(move |db| {
            let mut chat = db.conversation_boundary(&chat_id)?.unwrap();
            chat["wakeup"] = wakeup;
            db.save_conversation_boundary(&chat)?;
            db.change_run(&run_id, cairn_installation::runs::RunChanges {
                status: Some(if pending { cairn_contracts::RunStatus::Running } else { cairn_contracts::RunStatus::Succeeded }),
                placement: Some(Box::new(cairn_installation::nodes::run_placement::PlacementChanges { node_id: Some(source.as_str().map(str::to_owned)), ..Default::default() })),
                ..Default::default()
            })?;
            db.save_checkpoint_boundary(&run_id, &json!({ "nodeId": source, "completed": true, "remainingMs": 123000, "prepared": { "backend": "native", "isolated": true } }))
        }).await.unwrap();
        if pending {
            f.service
                .worker
                .active
                .lock()
                .await
                .insert(rid.clone(), tokio_util::sync::CancellationToken::new());
        }
        let (status, moved) = f
            .owner_call(
                "POST",
                &format!("/api/nodes/placement/{rid}/move"),
                json!({ "nodeId": vm["id"], "discardWorkingFiles": true, "requiredTags": ["vm-only"] }),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{moved}");
        let (status, replaced) = f
            .owner_call(
                "POST",
                &format!("/api/nodes/placement/{rid}/backend-migration"),
                json!({ "nodeId": replacement["id"], "backend": "native", "confirm": true, "requiredTags": [] }),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{replaced}");
        let changed = f.service.store.run(&rid).await.unwrap();
        assert_eq!(changed["targetNodeId"], replacement["id"]);
        assert!(changed["nodeId"].is_null());
        assert!(changed["pinnedNodeId"].is_null());
        assert_eq!(changed["requiredTags"], json!([]));
        if pending {
            f.service
                .store
                .change_run(
                    &rid,
                    cairn_installation::runs::RunChanges {
                        status: Some(cairn_contracts::RunStatus::Cancelled),
                        ..Default::default()
                    },
                )
                .await
                .unwrap();
            f.service.worker.active.lock().await.remove(&rid);
            f.service.detach_stopped_environments().await.unwrap();
        }
        let chat = f.service.conversation_boundary(&cid).await.unwrap();
        assert_eq!(chat["wakeup"], wake);
        assert_ne!(chat["sessionRestartRequested"], true);
        f.service.chat_tick(&HashSet::new()).await.unwrap();
        let continued = f.service.store.run(&rid).await.unwrap();
        assert_eq!(continued["status"], "queued");
        assert_eq!(continued["targetNodeId"], replacement["id"]);
        assert!(continued["chatExecution"]["context"].is_string());
        f.service.retire_environments().await.unwrap();
        let attempt = id();
        let placed = placement::reserve(&f.service, &continued, &attempt)
            .await
            .unwrap();
        assert_eq!(placed["nodeId"], replacement["id"]);
        let reserved = f.service.store.run(&rid).await.unwrap();
        assert_eq!(reserved["targetNodeId"], replacement["id"]);
        assert_eq!(reserved["environmentState"], "expired");
        assert!(reserved["pinnedNodeId"].is_null());
        placement::materialize(&f.service, &attempt).await.unwrap();
        let (run_id, attempt_id) = (rid.clone(), attempt.clone());
        f.service
            .store
            .transaction(move |db| {
                let current = db.run(&run_id)?.unwrap();
                let held = db.attempt(&attempt_id)?.unwrap();
                let mut checkpoint = db
                    .checkpoint_boundary(&run_id)?
                    .unwrap_or_else(|| json!({}));
                checkpoint["runnerId"] = attempt_id.clone().into();
                checkpoint["nodeId"] = held.node_id.into();
                db.save_checkpoint_boundary(&run_id, &checkpoint)?;
                db.change_run(
                    &run_id,
                    cairn_installation::runs::RunChanges {
                        status: Some(cairn_contracts::RunStatus::Running),
                        cancel_requested_at: Some(None),
                        ..Default::default()
                    },
                )?;
                placement::publish_admission(
                    db,
                    &run_id,
                    &attempt_id,
                    &current["environmentAdmissionRevision"],
                )
            })
            .await
            .unwrap();
        let admitted = f.service.store.run(&rid).await.unwrap();
        assert!(admitted["targetNodeId"].is_null());
        assert!(admitted["pinnedNodeId"].is_null());
        assert!(admitted["environmentState"].is_null());
        placement::release(&f.service, &attempt).await.unwrap();
    }
}
