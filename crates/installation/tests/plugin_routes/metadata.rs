use super::*;

#[allow(dead_code)]
#[path = "../common/management.rs"]
mod management;

#[tokio::test]
async fn plugin_metadata_keeps_the_routed_turns_original_participant_authority() {
    Box::pin(metadata_after_revocation(false)).await;
}

#[tokio::test]
async fn plugin_metadata_rechecks_its_original_requester_before_returning_a_read() {
    Box::pin(metadata_after_revocation(true)).await;
}

async fn metadata_after_revocation(during_read: bool) {
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
    let reads = [
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
    ];
    for (name, args) in &reads {
        let positive = management::call(&s, &configuration, name, args.clone()).await;
        management::result(&positive);
    }
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let mut authority_server = None;
    let mut pending = None;
    if during_read {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let (entering, releasing) = (entered.clone(), release.clone());
        let calls = Arc::new(AtomicUsize::new(0));
        let authority = axum::Router::new().route(
            "/api/relay/00000000-0000-4000-8000-000000000055/task-authors",
            axum::routing::get(move || {
                let (entered, release, calls) = (entering.clone(), releasing.clone(), calls.clone());
                async move {
                    if calls.fetch_add(1, Ordering::SeqCst) == 1 {
                        entered.notify_one();
                        release.notified().await;
                    }
                    axum::Json(json!({"owner": {"account_id": "fixture-owner", "access_id": "fixture-owner"}, "members": []}))
                }
            }),
        );
        let (origin, server) = common::serve_locally(authority).await;
        authority_server = Some(server);
        let path = s.config.data_dir.join("installation-relay/identity.json");
        let mut identity: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        identity["origin"] = json!(origin);
        std::fs::write(path, serde_json::to_vec(&identity).unwrap()).unwrap();
        let (s, configuration) = (s.clone(), configuration.clone());
        let mut reading = tokio::spawn(async move {
            management::call(&s, &configuration, "list_plugin_instance_access", json!({})).await
        });
        tokio::select! {
            () = entered.notified() => {},
            result = &mut reading => panic!("Metadata returned without its post-read source check: {result:?}"),
            () = tokio::time::sleep(std::time::Duration::from_secs(10)) => panic!("The source check was not reached"),
        }
        pending = Some(reading);
    }
    let people = format!(
        "/api/plugins/router/connections/{instance}/resources/workspace/{resource}/routes/{}/people/alice",
        text(&route, "id"),
    );
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
    let removed = common::read_json(response).await;
    if let Some(pending) = pending {
        release.notify_one();
        let result = tokio::time::timeout(std::time::Duration::from_secs(10), pending)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            result["result"]["isError"], true,
            "A late source revocation returned metadata"
        );
    }
    let control = management::call(&s, &configuration, "manage_plugin_resource", json!({
        "action": "list", "pluginId": "router", "instanceId": instance, "collection": "workspace",
    })).await;
    assert_eq!(
        control["result"]["isError"], true,
        "The source revocation must be effective"
    );
    let mut denied = Vec::new();
    for (name, args) in &reads {
        let response = management::call(&s, &configuration, name, args.clone()).await;
        denied.push((*name, response["result"]["isError"] == true));
        let owner = management::public_call(&s, &["read"], name, args.clone()).await;
        management::result(&owner);
    }
    assert!(
        denied.iter().all(|(_, denied)| *denied),
        "Metadata escaped its original requester: {denied:?}"
    );

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
        let response = management::call(&s, &configuration, name, args.clone()).await;
        assert_eq!(
            response["result"]["isError"], true,
            "Reapproval revived the old turn: {name}"
        );
    }
    if let Some(server) = authority_server {
        server.abort();
    }
}
