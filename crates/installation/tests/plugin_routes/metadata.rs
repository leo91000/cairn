use super::*;

use super::management;
use std::sync::atomic::Ordering;

#[tokio::test]
async fn plugin_metadata_keeps_the_routed_turns_original_participant_authority() {
    Box::pin(after_revocation(
        Surface::PluginMetadata,
        false,
        Change::Person,
    ))
    .await;
}

#[tokio::test]
async fn plugin_metadata_rechecks_its_original_requester_before_returning_a_read() {
    Box::pin(after_revocation(
        Surface::PluginMetadata,
        true,
        Change::Person,
    ))
    .await;
}

#[tokio::test]
async fn general_installation_tools_refuse_the_revoked_external_requester() {
    Box::pin(after_revocation(
        Surface::Installation,
        false,
        Change::Person,
    ))
    .await;
}

#[tokio::test]
async fn general_installation_reads_recheck_source_before_returning_the_result() {
    Box::pin(after_revocation(
        Surface::Installation,
        true,
        Change::Person,
    ))
    .await;
}

#[tokio::test]
async fn typed_project_reads_recheck_source_before_returning_the_result() {
    Box::pin(after_revocation(
        Surface::TypedProject,
        true,
        Change::Person,
    ))
    .await;
}

#[tokio::test]
async fn source_package_replacement_during_management_authority_io_blocks_effects_and_results() {
    for surface in [Surface::TypedProjectWrite, Surface::TypedProject] {
        Box::pin(after_revocation(surface, true, Change::Package)).await;
    }
}

#[derive(Clone, Copy)]
enum Change {
    Person,
    Package,
}

#[derive(Clone, Copy)]
enum Surface {
    PluginMetadata,
    Installation,
    TypedProject,
    TypedProjectWrite,
}

async fn refused(s: &Arc<Service>, configuration: &Value, name: &str, args: Value) -> bool {
    let bearer = text(&configuration["env"], "CAIRN_MCP_RUN_TOKEN");
    let (status, response) =
        management::request(s, bearer, "tools/call", common::mcp_arguments(name, &args)).await;
    status == StatusCode::FORBIDDEN || response["result"]["isError"] == true
}

async fn after_revocation(surface: Surface, during_read: bool, change: Change) {
    let (_root, s, instance, resource, app) = fixture().await;
    let agent = s
        .agent(
            json!({
                "name": "Metadata manager", "access": {"projects": [], "installation": true},
            }),
            None,
        )
        .await
        .unwrap();
    let route = create_route(
        &app,
        &instance,
        &resource,
        json!({
            "name": "Support", "enabled": true, "match": {"channel": "C1"},
            "agentId": agent["id"], "accessMode": "requests", "userIds": ["alice"],
        }),
    )
    .await;
    let inbound = json!({
        "id": "metadata-author", "resource": resource, "author": {"id": "alice"},
        "context": {"channel": "C1", "thread": "metadata-thread"},
        "body": {"text": "Inspect the installation's plugin configuration"},
    });
    assert_eq!(
        deliver(&app, &instance, inbound.clone()).await.1["status"],
        "accepted"
    );
    let connection = connections::get(&s, "router", &instance).await.unwrap();
    let package = registry::active(&s, "router").await.unwrap();
    let mut session = s
        .plugins
        .host
        .start(
            &PathBuf::from(env!("CARGO_BIN_EXE_cairn")),
            &package.manifest,
            &package.source,
        )
        .await
        .unwrap();
    runtime::pump(&s, &connection, &mut session).await.unwrap();
    session.stop().await;
    s.chat_tick(&std::collections::HashSet::new())
        .await
        .unwrap();
    let chat = s.chat_list().await.unwrap()[0].clone();
    let detail = s.chat_detail(text(&chat, "id")).await.unwrap();
    // Use the native lifecycle fixture without launching a VM/provider process.
    let run = s
        .store
        .patch_run(
            text(&detail["run"], "id"),
            json!({
                "status": "running", "isolated": true,
            }),
        )
        .await
        .unwrap();
    let configuration = s.mcps.run_configuration(&s, &run).await.unwrap();
    let reads = match surface {
        Surface::TypedProjectWrite => vec![(
            "save_project",
            json!({ "settings": {
                "name": "Original source management", "path": _root.path().join("home"),
            }}),
        )],
        Surface::TypedProject => vec![("list_projects", json!({}))],
        Surface::PluginMetadata => vec![
            ("list_plugin_packages", json!({})),
            ("get_plugin_package", json!({"pluginId": "router"})),
            ("list_plugin_instance_access", json!({})),
            ("list_plugin_agent_grant_counts", json!({})),
            (
                "list_plugin_resource_choices",
                json!({
                    "pluginId": "router", "instanceId": instance, "collection": "workspace",
                }),
            ),
        ],
        Surface::Installation => vec![
            ("list_agents", json!({})),
            ("list_chats", json!({})),
            ("get_chat", json!({ "chatId": chat["id"] })),
            ("list_tasks", json!({})),
            (
                "create_task",
                json!({
                    "name": "Authorized management task", "prompt": "Inspect the installation",
                    "agentId": agent["id"], "worktree": false,
                }),
            ),
            ("create_chat", json!({ "agentId": agent["id"] })),
        ],
    };
    let operation = match surface {
        Surface::PluginMetadata => "list_plugin_instance_access",
        Surface::Installation => "list_agents",
        Surface::TypedProject => "list_projects",
        Surface::TypedProjectWrite => "save_project",
    };
    let gate = if during_read {
        Some(common::relay_fixture::owner_lookups(&s).await)
    } else {
        None
    };
    let authentication_lookups = if let Some(gate) = &gate {
        let bearer = text(&configuration["env"], "CAIRN_MCP_RUN_TOKEN");
        let (status, reply) = management::request(&s, bearer, "ping", json!({})).await;
        assert_eq!(status, StatusCode::OK, "{reply}");
        gate.calls.swap(0, Ordering::SeqCst)
    } else {
        0
    };
    let mut operation_lookups = 0;
    for (name, args) in &reads {
        if let Some(gate) = &gate {
            gate.calls.store(0, Ordering::SeqCst);
        }
        let positive = management::call(&s, &configuration, name, args.clone()).await;
        management::result(&positive);
        if *name == operation
            && let Some(gate) = &gate
        {
            operation_lookups = gate.calls.load(Ordering::SeqCst);
        }
    }
    let projects_before_pending = s.store.projects().await.unwrap();
    let mut pending = None;
    if let Some(gate) = &gate {
        // Calibrate the final operation lookup after a successful request. The
        // ping baseline includes HTTP authentication, so this gate must reach
        // the operation's own authority check rather than initial admission.
        assert!(operation_lookups > authentication_lookups);
        gate.calls.store(0, Ordering::SeqCst);
        gate.hold_at.store(operation_lookups, Ordering::SeqCst);
        let args = reads
            .iter()
            .find(|(name, _)| *name == operation)
            .unwrap()
            .1
            .clone();
        let (s, configuration) = (s.clone(), configuration.clone());
        let mut reading = tokio::spawn(async move {
            let bearer = text(&configuration["env"], "CAIRN_MCP_RUN_TOKEN");
            management::request(
                &s,
                bearer,
                "tools/call",
                common::mcp_arguments(operation, &args),
            )
            .await
        });
        tokio::select! {
            () = gate.entered.notified() => {},
            result = &mut reading => panic!("Metadata returned without its post-read source check: {result:?}"),
            () = tokio::time::sleep(std::time::Duration::from_secs(10)) => panic!("The source check was not reached"),
        }
        pending = Some(reading);
    }
    let people = format!(
        "/api/plugins/router/connections/{instance}/resources/workspace/{resource}/routes/{}/people/alice",
        text(&route, "id"),
    );
    let removed = if matches!(change, Change::Package) {
        let mut manifest = serde_json::to_value(&package.manifest).unwrap();
        manifest["version"] = json!("2");
        let replacement = registry::install(&s, &json!({
            "manifest": manifest, "source": format!("-- new source approval\n{}", package.source.main),
        })).await.unwrap();
        registry::approve(&s, "router", "2", text(&replacement, "digest"))
            .await
            .unwrap();
        Value::Null
    } else {
        let response = common::send(
            &app,
            owner_request(
                "POST",
                &people,
                json!({
                    "action": "remove", "revision": route["revision"],
                }),
            ),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        common::read_json(response).await
    };
    if let Some(pending) = pending {
        gate.as_ref().unwrap().release.notify_one();
        let (status, result) = tokio::time::timeout(std::time::Duration::from_secs(10), pending)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(status, StatusCode::OK, "{result}");
        assert_eq!(
            result["result"]["isError"], true,
            "A late source revocation returned metadata"
        );
        if matches!(surface, Surface::TypedProjectWrite) {
            assert_eq!(
                s.store.projects().await.unwrap(),
                projects_before_pending,
                "The rejected source left a project effect behind"
            );
        }
    }
    let control = refused(&s, &configuration, "manage_plugin_resource", json!({
        "action": "list", "pluginId": "router", "instanceId": instance, "collection": "workspace",
    })).await;
    assert!(control, "The source revocation must be effective");
    let mut denied = Vec::new();
    for (name, args) in &reads {
        denied.push((*name, refused(&s, &configuration, name, args.clone()).await));
        let owner = management::public_call(&s, &["read", "manage"], name, args.clone()).await;
        management::result(&owner);
    }
    assert!(
        denied.iter().all(|(_, denied)| *denied),
        "Management escaped its original requester: {denied:?}"
    );

    if matches!(change, Change::Package) {
        return;
    }

    let mut fresh = inbound;
    fresh["id"] = json!("request-new-metadata-grant");
    assert_eq!(
        deliver(&app, &instance, fresh).await.1["status"],
        "requested"
    );
    let response = common::send(
        &app,
        owner_request(
            "POST",
            &people,
            json!({
                "action": "approve", "revision": removed["revision"],
            }),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    for (name, args) in &reads {
        assert!(
            refused(&s, &configuration, name, args.clone()).await,
            "Reapproval revived the old turn: {name}"
        );
    }
}
