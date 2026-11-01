use super::*;

struct Actor {
    chat: Value,
    run: Value,
    configuration: Value,
}

async fn fixture(backend: &str) -> Fixture {
    let mut f = Fixture::with_workspace(true).await;
    f.agent["access"]["installation"] = true.into();
    f.agent["access"]["delegation"] = true.into();
    f.agent["access"]["backend"] = backend.into();
    if backend == "firecracker" {
        let vm = f.vm_node().await;
        f.agent["access"]["nodes"] = json!([vm["id"]]);
    }
    f.service
        .store
        .put_agent_fixture(f.agent.clone())
        .await
        .unwrap();
    f
}

async fn running(f: &Fixture, chat: Value) -> Actor {
    let run = f
        .service
        .store
        .change_run(
            text(&chat, "runId"),
            cairn_installation::runs::RunChanges {
                status: Some(cairn_contracts::RunStatus::Running),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let run = f.service.store.run(&run.id).await.unwrap();
    let configuration = f
        .service
        .mcps
        .run_configuration(&f.service, &run)
        .await
        .unwrap();
    Actor {
        chat,
        run,
        configuration,
    }
}

async fn root(f: &Fixture) -> Actor {
    let (chat, _) = f.conversation().await;
    running(f, chat).await
}

async fn delegated(f: &Fixture, parent: &Actor) -> Actor {
    let task = cairn_installation::delegation::delegate(
        &f.service,
        text(&parent.configuration["env"], "CAIRN_MCP_RUN_TOKEN"),
        &json!({ "task": "Review the requester's implementation" }),
    )
    .await
    .unwrap();
    f.service.chat_tick(&HashSet::new()).await.unwrap();
    let chat = f
        .service
        .conversation_boundary(text(&task, "chatId"))
        .await
        .unwrap();
    running(f, chat).await
}

async fn managed(f: &Fixture, parent: &Actor) -> Actor {
    managed_as(f, parent, text(&f.agent, "id")).await
}

async fn managed_as(f: &Fixture, parent: &Actor, agent: &str) -> Actor {
    let created = delegated_call(
        f,
        &parent.configuration,
        "create_chat",
        json!({ "agentId": agent }),
    )
    .await;
    assert_ne!(created["result"]["isError"], true, "{created}");
    let chat_id = created["result"]["structuredContent"]["result"]["id"]
        .as_str()
        .unwrap();
    let response = delegated_call(
        f,
        &parent.configuration,
        "send_chat_message",
        json!({
            "chatId": chat_id,
            "message": { "id": id(), "text": "Inspect the implementation" },
        }),
    )
    .await;
    assert_ne!(response["result"]["isError"], true, "{response}");
    f.service.chat_tick(&HashSet::new()).await.unwrap();
    let chat = f.service.conversation_boundary(chat_id).await.unwrap();
    running(f, chat).await
}

async fn send(f: &Fixture, caller: &Actor, destination: &Actor, message: &str) -> Value {
    delegated_call(
        f,
        &caller.configuration,
        "send_chat_message",
        json!({
            "chatId": destination.chat["id"],
            "message": { "id": message, "text": "Direct this conversation", "mode": "queue" },
        }),
    )
    .await
}

async fn allowed(f: &Fixture, actor: &Actor) -> bool {
    let run = f.service.store.run(text(&actor.run, "id")).await.unwrap();
    let node = f.node.clone();
    f.service
        .store
        .read(move |db| policy::allows(db, &run, &node))
        .await
        .unwrap()
}

async fn messages(f: &Fixture, actor: &Actor) -> Vec<String> {
    let chat_id = text(&actor.chat, "id").to_owned();
    f.service
        .store
        .read(move |db| {
            Ok(db
                .conversation_messages(&chat_id)?
                .into_iter()
                .map(|message| message.content.id)
                .collect())
        })
        .await
        .unwrap()
}

#[tokio::test]
async fn delegated_native_child_cannot_make_its_requester_depend_on_it() {
    let f = fixture("native").await;
    let parent = root(&f).await;
    let child = delegated(&f, &parent).await;
    assert!(parent.run["nativeParentRunId"].is_null());
    assert_eq!(child.run["nativeParentRunId"], parent.run["id"]);
    assert_eq!(child.run["nativeParentAccess"], "delegation");
    assert!(allowed(&f, &parent).await);
    assert!(allowed(&f, &child).await);

    let message = id();
    let response = send(&f, &child, &parent, &message).await;
    let saved_parent = f.service.store.run(text(&parent.run, "id")).await.unwrap();
    assert!(
        saved_parent["nativeParentRunId"].is_null(),
        "A child's directing call must not commit a native parent cycle"
    );
    assert_eq!(response["result"]["isError"], true, "{response}");
    let saved_chat = f
        .service
        .conversation_boundary(text(&parent.chat, "id"))
        .await
        .unwrap();
    assert!(saved_chat["nativeParentRunId"].is_null());
    assert!(!messages(&f, &parent).await.contains(&message));
    assert!(allowed(&f, &parent).await);
    assert!(allowed(&f, &child).await);
}

#[tokio::test]
async fn a_self_directing_call_cannot_persist_a_self_edge_on_either_backend() {
    for backend in ["native", "firecracker"] {
        let f = fixture(backend).await;
        let actor = root(&f).await;
        let message = id();
        let before = messages(&f, &actor).await;
        let response = send(&f, &actor, &actor, &message).await;
        assert_eq!(response["result"]["isError"], true, "{backend}: {response}");
        let saved = f.service.store.run(text(&actor.run, "id")).await.unwrap();
        assert!(saved["nativeParentRunId"].is_null());
        assert_eq!(messages(&f, &actor).await, before);
    }
}

#[tokio::test]
async fn a_longer_management_chain_cannot_be_closed_into_a_cycle() {
    let f = fixture("native").await;
    let parent = root(&f).await;
    let child = delegated(&f, &parent).await;
    let grandchild = managed(&f, &child).await;
    assert_eq!(grandchild.run["nativeParentRunId"], child.run["id"]);
    assert_eq!(grandchild.run["nativeParentAccess"], "installation");
    assert!(allowed(&f, &grandchild).await);
    let before = messages(&f, &parent).await;
    let response = send(&f, &grandchild, &parent, &id()).await;
    assert_eq!(response["result"]["isError"], true, "{response}");
    assert_eq!(messages(&f, &parent).await, before);
    for actor in [&parent, &child, &grandchild] {
        assert!(allowed(&f, actor).await);
    }
}

#[tokio::test]
async fn a_vm_descendant_cannot_poison_its_root_native_provenance() {
    let f = fixture("firecracker").await;
    let parent = root(&f).await;
    let child = managed(&f, &parent).await;
    assert_eq!(child.run["nativeParentRunId"], parent.run["id"]);
    let before_parent = f.service.store.run(text(&parent.run, "id")).await.unwrap();
    let before_chat = f
        .service
        .conversation_boundary(text(&parent.chat, "id"))
        .await
        .unwrap();
    let before_messages = messages(&f, &parent).await;
    let response = send(&f, &child, &parent, &id()).await;
    assert_eq!(response["result"]["isError"], true, "{response}");
    assert!(
        response["result"]
            .to_string()
            .contains("cyclic native authority lineage")
    );
    assert_eq!(
        f.service.store.run(text(&parent.run, "id")).await.unwrap(),
        before_parent
    );
    assert_eq!(
        f.service
            .conversation_boundary(text(&parent.chat, "id"))
            .await
            .unwrap(),
        before_chat
    );
    assert_eq!(messages(&f, &parent).await, before_messages);
}

#[tokio::test]
async fn migrating_an_expired_vm_requester_cannot_adopt_its_native_child() {
    let mut f = fixture("auto").await;
    let vm = f.vm_node().await;
    f.agent["access"]["nodes"] = json!([vm["id"], f.node["id"]]);
    f.service
        .store
        .put_agent_fixture(f.agent.clone())
        .await
        .unwrap();
    f.node["nativeAccounts"] = json!([TRUSTED, OTHER]);
    f.service
        .store
        .put_node_fixture(f.node.clone())
        .await
        .unwrap();
    let parent = root(&f).await;
    let mut snapshot = parent.run["snapshot"].clone();
    snapshot["agent"]["access"]["backend"] = "firecracker".into();
    f.service
        .store
        .change_run(
            text(&parent.run, "id"),
            cairn_installation::runs::RunChanges {
                snapshot: Some(serde_json::from_value(snapshot).unwrap()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let parent = running(&f, parent.chat).await;
    let native_agent = f
        .service
        .agent_fixture(
            json!({
                "name": "Native migration reviewer",
                "access": { "backend": "native", "nodes": [f.node["id"]], "installation": true },
            }),
            None,
        )
        .await
        .unwrap();
    let child = managed_as(&f, &parent, text(&native_agent, "id")).await;
    assert_eq!(child.run["nativeParentRunId"], parent.run["id"]);
    assert!(allowed(&f, &child).await);

    // Model the normal terminal expiration state without creating/converting a
    // disk. The public migration call below must reject before any conversion.
    let (chat_id, run_id) = (
        text(&parent.chat, "id").to_owned(),
        text(&parent.run, "id").to_owned(),
    );
    f.service
        .store
        .transaction(move |db| {
            for mut message in db.messages(&chat_id)? {
                message["status"] = "delivered".into();
                db.put_message(&message)?;
            }
            db.change_run(
                &run_id,
                cairn_installation::runs::RunChanges {
                    status: Some(cairn_contracts::RunStatus::Succeeded),
                    recovery_pending: Some(false),
                    resume_available: Some(false),
                    boundary: Some(json!({
                        "environmentState": "expired",
                        "environmentExpiredAt": now(),
                        "nodeId": null,
                        "isolated": false,
                    })),
                    ..Default::default()
                },
            )?;
            db.save_checkpoint_boundary(&run_id, &json!({ "completed": true }))?;
            db.revoke_mcp_run(&run_id)?;
            let mut chat = db.conversation_boundary(&chat_id)?.unwrap();
            chat["environmentState"] = "expired".into();
            chat["environmentExpiredAt"] = now().into();
            chat["paused"] = true.into();
            db.save_conversation_boundary(&chat)?;
            Ok(())
        })
        .await
        .unwrap();
    assert!(allowed(&f, &child).await);
    let before_run = f.service.store.run(text(&parent.run, "id")).await.unwrap();
    let before_chat = f
        .service
        .conversation_boundary(text(&parent.chat, "id"))
        .await
        .unwrap();
    let before_checkpoint = f
        .service
        .store
        .checkpoint_boundary(text(&parent.run, "id"))
        .await
        .unwrap();
    let before_messages = messages(&f, &parent).await;
    let response = delegated_call(
        &f,
        &child.configuration,
        "migrate_chat_backend",
        json!({
            "chatId": parent.chat["id"],
            "nodeId": f.node["id"],
            "backend": "native",
            "confirm": true,
        }),
    )
    .await;
    assert_eq!(response["result"]["isError"], true, "{response}");
    assert!(
        response["result"]
            .to_string()
            .contains("cyclic native authority lineage")
    );
    assert_eq!(
        f.service.store.run(text(&parent.run, "id")).await.unwrap(),
        before_run
    );
    assert_eq!(
        f.service
            .conversation_boundary(text(&parent.chat, "id"))
            .await
            .unwrap(),
        before_chat
    );
    assert_eq!(
        f.service
            .store
            .checkpoint_boundary(text(&parent.run, "id"))
            .await
            .unwrap(),
        before_checkpoint
    );
    assert_eq!(messages(&f, &parent).await, before_messages);
    assert!(allowed(&f, &child).await);

    // A verified installation owner can still explicitly adopt this expired
    // environment; that operation never acquires the child's reverse edge.
    let (status, result) = f
        .owner_call(
            "POST",
            &format!(
                "/api/nodes/placement/{}/backend-migration",
                text(&parent.run, "id")
            ),
            json!({ "nodeId": f.node["id"], "backend": "native", "confirm": true }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{result}");
    let adopted = f.service.store.run(text(&parent.run, "id")).await.unwrap();
    assert!(adopted["nativeParentRunId"].is_null());
    assert_eq!(adopted["nativePrincipals"], json!([OTHER]));
    assert_eq!(adopted["snapshot"]["agent"]["access"]["backend"], "native");
    assert!(allowed(&f, &child).await);
}

#[tokio::test]
async fn concurrent_opposing_directions_commit_only_one_acyclic_effect() {
    let f = fixture("native").await;
    let a = root(&f).await;
    let b = root(&f).await;
    let a_message = id();
    let b_message = id();
    let (a_response, b_response) =
        tokio::join!(send(&f, &a, &b, &a_message), send(&f, &b, &a, &b_message),);
    let a_ok = a_response["result"]["isError"] != true;
    let b_ok = b_response["result"]["isError"] != true;
    assert_ne!(a_ok, b_ok, "Exactly one opposing direction must commit");
    assert_eq!(messages(&f, &b).await.contains(&a_message), a_ok);
    assert_eq!(messages(&f, &a).await.contains(&b_message), b_ok);
    let saved_a = f.service.store.run(text(&a.run, "id")).await.unwrap();
    let saved_b = f.service.store.run(text(&b.run, "id")).await.unwrap();
    assert_eq!(saved_a["nativeParentRunId"].is_null(), a_ok);
    assert_eq!(saved_b["nativeParentRunId"].is_null(), b_ok);
    assert!(allowed(&f, &a).await);
    assert!(allowed(&f, &b).await);
}

#[tokio::test]
async fn directing_authority_keeps_the_sixteen_scope_depth_bound_on_both_backends() {
    for backend in ["native", "firecracker"] {
        let f = fixture(backend).await;
        let accepted = root(&f).await;
        let refused = root(&f).await;
        let mut previous = Value::Null;
        let mut last = Value::Null;
        // The destination plus fifteen stored ancestors is sixteen scopes.
        for _ in 0..15 {
            let mut run = f.run();
            run["status"] = "running".into();
            run["nativeParentRunId"] = previous;
            run["nativeParentAccess"] = "installation".into();
            common::add_run(&f.service.store, &run).await;
            previous = run["id"].clone();
            last = run;
        }
        let configuration = f
            .service
            .mcps
            .run_configuration(&f.service, &last)
            .await
            .unwrap();
        let caller = Actor {
            chat: Value::Null,
            run: last,
            configuration,
        };
        let response = send(&f, &caller, &accepted, &id()).await;
        assert_ne!(response["result"]["isError"], true, "{backend}: {response}");
        let saved = f
            .service
            .store
            .run(text(&accepted.run, "id"))
            .await
            .unwrap();
        assert_eq!(saved["nativeParentRunId"], caller.run["id"]);
        if backend == "native" {
            assert!(allowed(&f, &accepted).await);
        }

        let mut extra = f.run();
        extra["status"] = "running".into();
        extra["nativeParentRunId"] = caller.run["id"].clone();
        extra["nativeParentAccess"] = "installation".into();
        common::add_run(&f.service.store, &extra).await;
        let configuration = f
            .service
            .mcps
            .run_configuration(&f.service, &extra)
            .await
            .unwrap();
        let over_limit = Actor {
            chat: Value::Null,
            run: extra,
            configuration,
        };
        let before = f.service.store.run(text(&refused.run, "id")).await.unwrap();
        let before_messages = messages(&f, &refused).await;
        let response = send(&f, &over_limit, &refused, &id()).await;
        assert_eq!(response["result"]["isError"], true, "{backend}: {response}");
        assert_eq!(
            f.service.store.run(text(&refused.run, "id")).await.unwrap(),
            before
        );
        assert_eq!(messages(&f, &refused).await, before_messages);
    }
}

#[tokio::test]
async fn a_rejected_message_rolls_back_an_otherwise_valid_prospective_parent() {
    let f = fixture("native").await;
    let caller = root(&f).await;
    let destination = root(&f).await;
    let before = messages(&f, &destination).await;
    let response = send(&f, &caller, &destination, &before[0]).await;
    assert_eq!(response["result"]["isError"], true, "{response}");
    assert_eq!(messages(&f, &destination).await, before);
    let saved = f
        .service
        .store
        .run(text(&destination.run, "id"))
        .await
        .unwrap();
    let chat = f
        .service
        .conversation_boundary(text(&destination.chat, "id"))
        .await
        .unwrap();
    assert!(saved["nativeParentRunId"].is_null());
    assert!(chat["nativeParentRunId"].is_null());
    assert!(allowed(&f, &caller).await);
    assert!(allowed(&f, &destination).await);
}

#[tokio::test]
async fn a_child_cannot_create_a_cycle_when_resuming_its_mission_origin() {
    let f = fixture("native").await;
    let mission = saved_native_mission(&f).await;
    let current = f
        .service
        .store
        .change_run(
            text(&mission, "id"),
            cairn_installation::runs::RunChanges {
                status: Some(cairn_contracts::RunStatus::Running),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let run = f.service.store.run(&current.id).await.unwrap();
    let configuration = f
        .service
        .mcps
        .run_configuration(&f.service, &run)
        .await
        .unwrap();
    let origin = Actor {
        chat: Value::Null,
        run,
        configuration,
    };
    let child = managed(&f, &origin).await;
    f.service
        .store
        .change_run(
            text(&mission, "id"),
            cairn_installation::runs::RunChanges {
                status: Some(cairn_contracts::RunStatus::Failed),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let before = f.service.store.run(text(&mission, "id")).await.unwrap();
    assert!(allowed(&f, &child).await);
    let response = delegated_call(
        &f,
        &child.configuration,
        "resume_run",
        json!({ "runId": mission["id"] }),
    )
    .await;
    assert_eq!(response["result"]["isError"], true, "{response}");
    assert!(
        response["result"]
            .to_string()
            .contains("cyclic native authority lineage"),
        "The native grant check must reject this resume before its effect: {response}"
    );
    assert_eq!(
        f.service.store.run(text(&mission, "id")).await.unwrap(),
        before
    );
    assert!(allowed(&f, &child).await);
}

#[tokio::test]
async fn equivalent_parent_directions_and_task_reports_preserve_revocation() {
    let f = fixture("native").await;
    let parent = root(&f).await;
    let child = delegated(&f, &parent).await;
    for _ in 0..2 {
        let response = send(&f, &parent, &child, &id()).await;
        assert_ne!(response["result"]["isError"], true, "{response}");
    }
    let saved = f.service.store.run(text(&child.run, "id")).await.unwrap();
    assert_eq!(saved["nativeParentRunId"], parent.run["id"]);
    assert_eq!(saved["nativeParentAccess"], "delegation");
    assert!(allowed(&f, &child).await);
    f.service
        .store
        .change_run(
            text(&child.run, "id"),
            cairn_installation::runs::RunChanges {
                summary: Some("Independent review report".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let report = cairn_installation::delegation::status(
        &f.service,
        text(&parent.configuration["env"], "CAIRN_MCP_RUN_TOKEN"),
        &json!({ "chatId": child.chat["id"] }),
    )
    .await
    .unwrap();
    assert_eq!(report["tasks"][0]["report"], "Independent review report");
    let saved_parent = f.service.store.run(text(&parent.run, "id")).await.unwrap();
    assert!(saved_parent["nativeParentRunId"].is_null());

    let mut revoked = f.agent.clone();
    revoked["access"]["delegation"] = false.into();
    f.service.store.put_agent_fixture(revoked).await.unwrap();
    assert!(!allowed(&f, &child).await);
    f.service
        .store
        .put_agent_fixture(f.agent.clone())
        .await
        .unwrap();
    assert!(allowed(&f, &child).await);
    f.service
        .store
        .change_run(
            text(&parent.run, "id"),
            cairn_installation::runs::RunChanges {
                cancel_requested_at: Some(Some(now())),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert!(!allowed(&f, &child).await);
}
