mod common;

use axum::{body::Body, http::StatusCode};
use cairn_installation::{
    auth::{InstallationIdentity, InstallationRole},
    config::MAIN_AGENT_ID,
    plugins::{agents, connections, registry, resources},
    service::Service,
    validation::text,
};
use serde_json::json;
use std::path::PathBuf;
use tempfile::TempDir;

#[tokio::test]
async fn resource_profiles_and_scope_limits_apply_to_tools_commands_and_the_actual_http_request() {
    let root = TempDir::new().unwrap();
    std::fs::create_dir_all(root.path().join("home")).unwrap();
    let mut config = common::config(root.path());
    config.runner_url = "http://runner:4311".into();
    let s = Service::new(config).await.unwrap();
    common::relay_fixture::claimed(&s).await.unwrap();
    s.plugins
        .use_worker(PathBuf::from(env!("CARGO_BIN_EXE_cairn")));
    let package = registry::install(&s, &json!({
        "manifest": serde_json::from_str::<serde_json::Value>(include_str!("../../../tests/fixtures/plugin-resource-access/manifest.json")).unwrap(),
        "source": include_str!("../../../tests/fixtures/plugin-resource-access/main.luau"),
    })).await.unwrap();
    registry::approve(&s, "example", "1", text(&package, "digest"))
        .await
        .unwrap();
    let instance = connections::save(
        &s,
        "example",
        &json!({"name": "Example", "enabled": true}),
        None,
    )
    .await
    .unwrap();
    let instance = text(&instance, "id");
    let resource = resources::save(
        &s,
        "example",
        instance,
        "workspace",
        &json!({
        "name": "Engineering", "settings": {"externalId": "T111"}, "secrets": {"token": "grant-fixture-token"}
        }),
        None,
    )
    .await
    .unwrap();
    let resource = text(&resource, "id");
    let task = s
        .task(
            json!({"name": "Probe", "prompt": "Probe", "agentId": MAIN_AGENT_ID}),
            None,
        )
        .await
        .unwrap();
    let run = s.enqueue(text(&task, "id"), "manual", None).await.unwrap();
    let run = s
        .store
        .patch_run(
            text(&run, "id"),
            json!({"status": "running", "isolated": true}),
        )
        .await
        .unwrap();
    let configuration = s.mcps.run_configuration(&s, &run).await.unwrap();
    let bearer = text(&configuration["env"], "CAIRN_MCP_RUN_TOKEN");
    assert!(agents::tools(&s, bearer).await.unwrap().is_empty());
    let app = cairn_installation::http::router(s.clone()).await.unwrap();
    let path = format!(
        "/api/plugins/example/connections/{instance}/resources/workspace/{resource}/grants"
    );
    let mut request = common::request("POST", &path)
        .header("content-type", "application/json")
        .body(Body::from(
            json!({"agentId": MAIN_AGENT_ID, "profile": "read", "scopes": {"channel": ["C1"]}})
                .to_string(),
        ))
        .unwrap();
    request
        .extensions_mut()
        .insert(InstallationIdentity::trusted(
            InstallationRole::Owner,
            "fixture-owner",
        ));
    let response = common::send(&app, request).await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "{}",
        common::read_text(response).await
    );
    let grant = common::read_json(response).await;
    let provider = axum::Router::new().route(
        "/messages",
        axum::routing::get(|headers: axum::http::HeaderMap| async move {
            assert_eq!(headers["authorization"], "Bearer grant-fixture-token");
            axum::Json(json!({"ok": true}))
        }),
    );
    let provider = provider.route(
        "/read-messages",
        axum::routing::post(
            |headers: axum::http::HeaderMap,
             axum::Form(form): axum::Form<std::collections::HashMap<String, String>>| async move {
                assert_eq!(headers["authorization"], "Bearer grant-fixture-token");
                assert_eq!(form.get("channel").map(String::as_str), Some("C1"));
                assert_eq!(form.get("team_id").map(String::as_str), Some("T111"));
                axum::Json(json!({"ok": true}))
            },
        ),
    );
    let (origin, server) = common::serve_locally(provider).await;
    s.plugins.route_http_to(Some(origin));
    let tools = agents::tools(&s, bearer).await.unwrap();
    assert_eq!(
        tools
            .iter()
            .map(|tool| text(tool, "name"))
            .collect::<Vec<_>>(),
        vec!["example_read"]
    );
    let input = json!({"resource": resource, "channel": "C1"});
    let mut form = input.clone();
    form["form"] = json!(true);
    assert_eq!(
        agents::call_tool(&s, bearer, "example_read", &form)
            .await
            .unwrap()
            .unwrap(),
        json!({"ok": true})
    );
    assert_eq!(
        agents::call_tool(&s, bearer, "example_read", &input)
            .await
            .unwrap()
            .unwrap(),
        json!({"ok": true})
    );
    for (tool, extra) in [
        ("example_write", json!({})),
        ("example_read", json!({"channel": "C2"})),
        ("example_read", json!({"method": "POST"})),
        ("example_read", json!({"override": "C2"})),
        ("example_read", json!({"raw": "channel=C2"})),
        ("example_read", json!({"form": true, "override": "C2"})),
        ("example_read", json!({"teamOverride": "T222"})),
    ] {
        let mut value = input.clone();
        value
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        assert_eq!(
            agents::call_tool(&s, bearer, tool, &value)
                .await
                .unwrap_err()
                .status,
            403
        );
    }
    let command = json!({"plugin": "example", "command": "read", "args": ["--resource", resource, "--channel", "C1"]});
    assert_eq!(
        s.plugins.command(&s, bearer, &command).await.unwrap()["value"],
        json!({"ok": true})
    );
    let mut revoke = common::request("DELETE", &format!("{path}/{}", text(&grant, "id")))
        .body(Body::empty())
        .unwrap();
    revoke
        .extensions_mut()
        .insert(InstallationIdentity::trusted(
            InstallationRole::Owner,
            "fixture-owner",
        ));
    assert_eq!(common::send(&app, revoke).await.status(), StatusCode::OK);
    assert!(agents::tools(&s, bearer).await.unwrap().is_empty());
    assert!(
        agents::call_tool(&s, bearer, "example_read", &input)
            .await
            .unwrap()
            .is_none()
    );
    let retired = s
        .agent(json!({"name": "Unused resource agent"}), None)
        .await
        .unwrap();
    let mut request = common::request("POST", &path)
        .header("content-type", "application/json")
        .body(Body::from(
            json!({"agentId": retired["id"], "profile": "read", "scopes": {"channel": ["C1"]}})
                .to_string(),
        ))
        .unwrap();
    request
        .extensions_mut()
        .insert(InstallationIdentity::trusted(
            InstallationRole::Owner,
            "fixture-owner",
        ));
    assert_eq!(common::send(&app, request).await.status(), StatusCode::OK);
    let mut remove = common::request("DELETE", &format!("/api/agents/{}", text(&retired, "id")))
        .body(Body::empty())
        .unwrap();
    remove
        .extensions_mut()
        .insert(InstallationIdentity::trusted(
            InstallationRole::Owner,
            "fixture-owner",
        ));
    assert_eq!(common::send(&app, remove).await.status(), StatusCode::OK);
    let mut request = common::request("GET", &path).body(Body::empty()).unwrap();
    request
        .extensions_mut()
        .insert(InstallationIdentity::trusted(
            InstallationRole::Owner,
            "fixture-owner",
        ));
    assert_eq!(
        common::read_json(common::send(&app, request).await).await["items"],
        json!([])
    );
    server.abort();
}

#[tokio::test]
async fn a_pending_profile_grant_cannot_cross_a_package_approval_change() {
    use serde_json::Value;
    use std::sync::Arc;

    let root = TempDir::new().unwrap();
    std::fs::create_dir_all(root.path().join("home")).unwrap();
    let s = Service::new(common::config(root.path())).await.unwrap();
    common::relay_fixture::claimed(&s).await.unwrap();
    s.plugins
        .use_worker(PathBuf::from(env!("CARGO_BIN_EXE_cairn")));
    let mut manifest: Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/plugin-resource-access/manifest.json"
    ))
    .unwrap();
    let source = include_str!("../../../tests/fixtures/plugin-resource-access/main.luau");
    let package = registry::install(&s, &json!({"manifest": manifest, "source": source}))
        .await
        .unwrap();
    registry::approve(&s, "example", "1", text(&package, "digest"))
        .await
        .unwrap();
    let instance = connections::save(
        &s,
        "example",
        &json!({"name": "Example", "enabled": true}),
        None,
    )
    .await
    .unwrap();
    let instance = text(&instance, "id").to_owned();
    let resource = resources::save(
        &s,
        "example",
        &instance,
        "workspace",
        &json!({"name": "Engineering", "settings": {"externalId": "T111"}, "secrets": {"token": "fixture-only"}}),
        None,
    )
    .await
    .unwrap();
    let resource = text(&resource, "id").to_owned();
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let (entering, releasing) = (entered.clone(), release.clone());
    let authority = axum::Router::new().route(
        "/api/relay/00000000-0000-4000-8000-000000000055/task-authors",
        axum::routing::get(move || {
            let (entered, release) = (entering.clone(), releasing.clone());
            async move {
                entered.notify_one();
                release.notified().await;
                axum::Json(json!({
                    "owner": {"account_id": "fixture-owner", "access_id": "fixture-owner"},
                    "members": [],
                }))
            }
        }),
    );
    let (origin, server) = common::serve_locally(authority).await;
    let identity_path = s.config.data_dir.join("installation-relay/identity.json");
    let previous_identity = std::fs::read(&identity_path).unwrap();
    let mut identity: Value = serde_json::from_slice(&previous_identity).unwrap();
    identity["origin"] = json!(origin);
    std::fs::write(&identity_path, serde_json::to_vec(&identity).unwrap()).unwrap();
    let app = cairn_installation::http::router(s.clone()).await.unwrap();
    let path = format!(
        "/api/plugins/example/connections/{instance}/resources/workspace/{resource}/grants"
    );
    let mut request = common::request("POST", &path)
        .header("content-type", "application/json")
        .body(Body::from(
            json!({"agentId": MAIN_AGENT_ID, "profile": "read"}).to_string(),
        ))
        .unwrap();
    request
        .extensions_mut()
        .insert(InstallationIdentity::trusted(
            InstallationRole::Owner,
            "fixture-owner",
        ));
    let pending_app = app.clone();
    let mut pending = tokio::spawn(async move { common::send(&pending_app, request).await });
    tokio::select! {
        () = entered.notified() => {},

        result = &mut pending => panic!("Grant ended before authority lookup: {result:?}"),

        () = tokio::time::sleep(std::time::Duration::from_secs(5)) => {
            panic!("Grant did not reach the authority lookup")
        },
    }
    // The held request has already selected its authority. Package approval uses
    // the original responsive fixture, independently of that pending response.
    std::fs::write(&identity_path, previous_identity).unwrap();
    manifest["version"] = json!("2");
    let mut extra = manifest["operations"][0].clone();
    extra["name"] = json!("export");
    manifest["operations"].as_array_mut().unwrap().push(extra);
    let package = registry::install(&s, &json!({"manifest": manifest, "source": source}))
        .await
        .unwrap();
    registry::approve(&s, "example", "2", text(&package, "digest"))
        .await
        .unwrap();
    release.notify_one();
    let response = pending.await.unwrap();
    server.abort();
    assert_eq!(
        response.status(),
        StatusCode::CONFLICT,
        "The uncommitted profile crossed package approval: {}",
        common::read_text(response).await,
    );
    assert!(
        s.store
            .list("plugin-resource-grants")
            .await
            .unwrap()
            .is_empty()
    );
    let mut request = common::request("POST", &path)
        .header("content-type", "application/json")
        .body(Body::from(
            json!({"agentId": MAIN_AGENT_ID, "profile": "read"}).to_string(),
        ))
        .unwrap();
    request
        .extensions_mut()
        .insert(InstallationIdentity::trusted(
            InstallationRole::Owner,
            "fixture-owner",
        ));
    let response = common::send(&app, request).await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "{}",
        common::read_text(response).await
    );
    assert_eq!(common::read_json(response).await["profile"], "read");
    assert_eq!(
        s.store.list("plugin-resource-grants").await.unwrap().len(),
        1
    );
}

#[tokio::test]
async fn agent_pages_discover_and_revoke_scoped_rules_without_granting_the_instance() {
    let root = TempDir::new().unwrap();
    let s = Service::new(common::config(root.path())).await.unwrap();
    common::relay_fixture::claimed(&s).await.unwrap();
    s.plugins
        .use_worker(PathBuf::from(env!("CARGO_BIN_EXE_cairn")));
    let manifest = serde_json::from_str::<serde_json::Value>(include_str!(
        "../../../tests/fixtures/plugin-resource-access/manifest.json"
    ))
    .unwrap();
    let package = registry::install(&s, &json!({ "manifest": manifest, "source": "return {}" }))
        .await
        .unwrap();
    registry::approve(&s, "example", "1", text(&package, "digest"))
        .await
        .unwrap();
    let instance = connections::save(
        &s,
        "example",
        &json!({ "name": "App", "enabled": true }),
        None,
    )
    .await
    .unwrap();
    let resource = resources::save(
        &s,
        "example",
        text(&instance, "id"),
        "workspace",
        &json!({
            "name": "Engineering", "settings": { "externalId": "T111" },
            "secrets": { "token": "private-resource-fixture-token" },
        }),
        None,
    )
    .await
    .unwrap();
    let other = s.agent(json!({ "name": "Other" }), None).await.unwrap();
    let app = cairn_installation::http::router(s.clone()).await.unwrap();
    let request = |method: &str, path: &str, value: serde_json::Value, role| {
        let mut req = common::request(method, path)
            .header("content-type", "application/json")
            .body(Body::from(value.to_string()))
            .unwrap();
        req.extensions_mut()
            .insert(InstallationIdentity::trusted(role, "fixture-owner"));
        req
    };
    let resource_grants = format!(
        "/api/plugins/example/connections/{}/resources/workspace/{}/grants",
        text(&instance, "id"),
        text(&resource, "id")
    );
    let mut created = Vec::new();
    for index in 0..34 {
        let agent = if index == 33 {
            text(&other, "id")
        } else {
            MAIN_AGENT_ID
        };
        let response = common::send(
            &app,
            request(
                "POST",
                &resource_grants,
                json!({
                    "agentId": agent, "profile": "read", "scopes": { "channel": ["C1"] },
                }),
                InstallationRole::Owner,
            ),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        created.push(common::read_json(response).await);
    }
    let directory = format!("/api/agents/{MAIN_AGENT_ID}/plugin-resource-grants");
    for method in ["GET", "DELETE"] {
        let path = if method == "GET" {
            directory.clone()
        } else {
            format!("{directory}/{}", text(&created[0], "id"))
        };
        let response = common::send(
            &app,
            request(method, &path, json!(null), InstallationRole::Member),
        )
        .await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }
    let response = common::send(
        &app,
        request("GET", &directory, json!(null), InstallationRole::Owner),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let first = common::read_json(response).await;
    assert_eq!(first["items"].as_array().unwrap().len(), 32);
    for item in first["items"].as_array().unwrap() {
        assert_eq!(item["agentId"], MAIN_AGENT_ID);
        assert_eq!(item["resourceName"], "Engineering");
        assert_eq!(item["collection"], "workspace");
        assert_eq!(item["scopes"], json!({ "channel": ["C1"] }));
    }
    assert!(!first.to_string().contains("private-resource-fixture-token"));
    let response = common::send(
        &app,
        request(
            "GET",
            &format!("{directory}?cursor={}", text(&first, "nextCursor")),
            json!(null),
            InstallationRole::Owner,
        ),
    )
    .await;
    let last = common::read_json(response).await;
    assert_eq!(last["items"].as_array().unwrap().len(), 1);
    assert!(last["nextCursor"].is_null());
    let response = common::send(
        &app,
        request("GET", "/api/plugins", json!(null), InstallationRole::Owner),
    )
    .await;
    let catalog = common::read_json(response).await;
    assert_eq!(catalog["resourceGrantCounts"][MAIN_AGENT_ID], 33);
    assert_eq!(catalog["resourceGrantCounts"][text(&other, "id")], 1);
    assert_eq!(
        s.store.get("agents", MAIN_AGENT_ID).await.unwrap().unwrap()["plugins"],
        json!([])
    );
    assert_eq!(
        s.store
            .get(connections::KIND, text(&instance, "id"))
            .await
            .unwrap()
            .unwrap()["agentIds"],
        json!([])
    );
    let wrong_agent = format!(
        "/api/agents/{}/plugin-resource-grants/{}",
        text(&other, "id"),
        text(&created[0], "id")
    );
    let response = common::send(
        &app,
        request("DELETE", &wrong_agent, json!(null), InstallationRole::Owner),
    )
    .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    // An invalidated package does not strand its saved permissions on the agent page.
    std::fs::write(
        s.config.data_dir.join("plugins/example/1/main.luau"),
        "return { changed = true }",
    )
    .unwrap();
    let response = common::send(
        &app,
        request(
            "DELETE",
            &format!("{directory}/{}", text(&created[0], "id")),
            json!(null),
            InstallationRole::Owner,
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        s.store.list("plugin-resource-grants").await.unwrap().len(),
        33
    );
}

#[tokio::test]
async fn large_saved_rules_keep_byte_bounded_pages_without_skipping_matching_ids() {
    let root = TempDir::new().unwrap();
    let s = Service::new(common::config(root.path())).await.unwrap();
    let other = s.agent(json!({ "name": "Other" }), None).await.unwrap();
    let other = text(&other, "id").to_owned();
    let instance = cairn_installation::config::id();
    let resource = cairn_installation::config::id();
    let expected = s
        .store
        .transaction(move |db| {
            db.put(
                connections::KIND,
                &json!({ "id": instance, "name": "Unavailable app", "pluginId": "missing" }),
            )?;
            db.put(
                resources::KIND,
                &json!({ "id": resource, "name": "Saved workspace", "collection": "workspace" }),
            )?;
            let scopes = (0..8)
                .map(|scope| {
                    let values = (0..100)
                        .map(|value| format!("{value:03}{}", "s".repeat(176)))
                        .collect::<Vec<_>>();
                    (format!("scope{scope}"), values)
                })
                .collect::<std::collections::BTreeMap<_, _>>();
            let mut expected = Vec::new();
            for index in 0..64 {
                let id = cairn_installation::config::id();
                let agent = if index % 8 == 0 {
                    &other
                } else {
                    MAIN_AGENT_ID
                };
                db.put(
                    "plugin-resource-grants",
                    &json!({
                        "id": id,
                        "connectionId": instance,
                        "resourceId": resource,
                        "agentId": agent,
                        "profile": "read",
                        "operations": [],
                        "deniedOperations": [],
                        "scopes": scopes,
                        "ownerAccessId": "old-owner",
                        "revision": cairn_installation::config::id(),
                        "updatedAt": 1,
                    }),
                )?;
                if agent == MAIN_AGENT_ID {
                    expected.push(id);
                }
            }
            expected.sort();
            Ok(expected)
        })
        .await
        .unwrap();
    let app = cairn_installation::http::router(s.clone()).await.unwrap();
    let mut cursor = String::new();
    let mut found = Vec::new();
    let mut pages = 0;
    loop {
        let suffix = if cursor.is_empty() {
            String::new()
        } else {
            format!("?cursor={cursor}")
        };
        let path = format!("/api/agents/{MAIN_AGENT_ID}/plugin-resource-grants{suffix}");
        let mut request = common::request("GET", &path).body(Body::empty()).unwrap();
        request
            .extensions_mut()
            .insert(InstallationIdentity::trusted(
                InstallationRole::Owner,
                "fixture-owner",
            ));
        let response = common::send(&app, request).await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), cairn_protocol::MAX_BODY)
            .await
            .unwrap();
        assert!(body.len() <= cairn_protocol::MAX_BODY / 2);
        let page: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let items = page["items"].as_array().unwrap();
        assert!(!items.is_empty());
        assert!(
            items.len() < 32,
            "Large scope rules must hit the byte bound before the record bound"
        );
        found.extend(items.iter().map(|item| text(item, "id").to_owned()));
        pages += 1;
        if page["nextCursor"].is_null() {
            break;
        }
        let next = text(&page, "nextCursor");
        assert!(next > cursor.as_str());
        cursor = next.to_owned();
        assert!(pages < 4);
    }
    assert_eq!(found, expected);
    assert!(pages > 1);
}
