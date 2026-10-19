mod common;

use axum::{body::Body, http::StatusCode};
use cairn_installation::{
    auth::{InstallationIdentity, InstallationRole},
    plugins::{connections, panels, registry, resources},
    service::Service,
    validation::text,
};
use serde_json::{Value, json};
use std::{path::PathBuf, sync::Arc};
use tempfile::TempDir;

async fn fixture(source: &str, network: bool) -> (TempDir, Arc<Service>, Value) {
    let root = TempDir::new().unwrap();
    let s = Service::new(common::config(root.path())).await.unwrap();
    common::relay_fixture::claimed(&s).await.unwrap();
    s.plugins
        .use_worker(PathBuf::from(env!("CARGO_BIN_EXE_cairn")));
    let package = registry::install(&s, &json!({
        "manifest": {
            "id": "example", "name": "Example", "version": "1", "apiVersion": 2,
            "capabilities": if network { vec!["resources", "http"] } else { vec!["resources"] },
            "hosts": if network { vec!["api.example.test"] } else { vec![] },
            "instanceView": network,
            "resources": [{"name": "workspace", "title": "Workspaces", "singular": "Workspace", "settings": [
                {"key": "externalId", "title": "Workspace ID", "type": "string", "required": true},
                {"key": "token", "title": "Token", "type": "secret", "required": true}
            ]}]
        }, "source": source
    })).await.unwrap();
    registry::approve(&s, "example", "1", text(&package, "digest"))
        .await
        .unwrap();
    let instance = connections::save(
        &s,
        "example",
        &json!({"name": "Example app", "enabled": true}),
        None,
    )
    .await
    .unwrap();
    (root, s, instance)
}

#[tokio::test]
async fn owner_configures_resource_fields_and_secret_names_while_members_cannot_manage_them() {
    let (_root, s, instance) = fixture("return {}", false).await;
    let path = format!(
        "/api/plugins/example/connections/{}/resources/workspace",
        text(&instance, "id")
    );
    let app = cairn_installation::http::router(s).await.unwrap();
    let request = |method, body: Value, role| {
        let mut request = common::request(method, &path)
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();
        request
            .extensions_mut()
            .insert(InstallationIdentity::trusted(role, "fixture-account"));
        request
    };
    let input = json!({"name": "Engineering", "settings": {"externalId": "T111"}, "secrets": {"token": "resource-fixture-token"}});
    let member = common::send(
        &app,
        request("POST", input.clone(), InstallationRole::Member),
    )
    .await;
    assert_eq!(member.status(), StatusCode::FORBIDDEN);
    let created = common::send(&app, request("POST", input, InstallationRole::Owner)).await;
    assert_eq!(
        created.status(),
        StatusCode::OK,
        "{}",
        common::read_text(created).await
    );
    let created = common::read_json(created).await;
    assert_eq!(created["settings"], json!({"externalId": "T111"}));
    assert_eq!(created["secretKeys"], json!(["token"]));
    assert!(!created.to_string().contains("resource-fixture-token"));
    let listed = common::send(&app, request("GET", json!({}), InstallationRole::Owner)).await;
    assert_eq!(common::read_json(listed).await["items"], json!([created]));

    let item = format!("{path}/{}", text(&created, "id"));
    let mut update = request(
        "PUT",
        json!({
            "name": "Engineering renamed", "settings": {"externalId": "T111"},
            "secrets": {"token": ""}, "revision": created["revision"],
        }),
        InstallationRole::Owner,
    );
    *update.uri_mut() = item.parse().unwrap();
    let updated = common::send(&app, update).await;
    assert_eq!(
        updated.status(),
        StatusCode::OK,
        "{}",
        common::read_text(updated).await
    );
    let updated = common::read_json(updated).await;
    assert_eq!(updated["name"], "Engineering renamed");
    assert_eq!(updated["secretKeys"], json!(["token"]));
    assert_ne!(updated["revision"], created["revision"]);
    for (method, expected) in [
        ("GET", StatusCode::OK),
        ("DELETE", StatusCode::OK),
        ("GET", StatusCode::NOT_FOUND),
    ] {
        let mut request = request(method, json!({}), InstallationRole::Owner);
        *request.uri_mut() = item.parse().unwrap();
        let response = common::send(&app, request).await;
        assert_eq!(response.status(), expected);
        if method == "GET" && expected == StatusCode::OK {
            assert_eq!(common::read_json(response).await, updated);
        }
    }
}

#[tokio::test]
async fn package_reads_resource_metadata_and_host_injects_only_its_enabled_resource_secret() {
    let source = r#"return {action = function(event)
        local values = cairn.resources.list({collection = 'workspace'})
        local value = cairn.resources.get({collection = 'workspace', resource = values.items[1].id})
        assert(value.settings.externalId == 'T111' and value.settings.token == nil)
        local response = cairn.http.request({url = 'https://api.example.test/check',
            auth = {resource = value.id, secret = 'token', prefix = 'Bearer '}})
        return {notice = response.json.authorized and 'Connected' or 'Wrong credential'}
    end}"#;
    let (_root, s, instance) = fixture(source, true).await;
    let instance = text(&instance, "id");
    let input = json!({"name": "Engineering", "settings": {"externalId": "T111"}, "secrets": {"token": "resource-fixture-token"}});
    let resource = resources::save(&s, "example", instance, "workspace", &input, None)
        .await
        .unwrap();
    let app = axum::Router::new().route("/check", axum::routing::get(|headers: axum::http::HeaderMap| async move {
        axum::Json(json!({"authorized": headers.get("authorization").is_some_and(|value| value == "Bearer resource-fixture-token")}))
    }));
    let (origin, server) = common::serve_locally(app).await;
    s.plugins.route_http_to(Some(origin));
    let action = json!({"action": "check"});
    let result = panels::action(&s, "example", instance, &action)
        .await
        .unwrap();
    assert_eq!(result["notice"], "Connected");
    let mut input = input;
    input["enabled"] = json!(false);
    input["revision"] = resource["revision"].clone();
    resources::save(
        &s,
        "example",
        instance,
        "workspace",
        &input,
        Some(text(&resource, "id")),
    )
    .await
    .unwrap();
    assert_eq!(
        panels::action(&s, "example", instance, &action)
            .await
            .unwrap_err()
            .status,
        403
    );
    server.abort();
}

#[tokio::test]
async fn revoking_a_resource_during_http_io_prevents_its_result_from_reaching_the_package() {
    let source = r#"return {action = function(event)
        local response = cairn.http.request({url = 'https://api.example.test/check',
            auth = {resource = event.payload.resource, secret = 'token', prefix = 'Bearer '}})
        return {notice = response.json.message}
    end}"#;
    let (_root, s, instance) = fixture(source, true).await;
    let instance = text(&instance, "id").to_owned();
    let input = json!({"name": "Engineering", "settings": {"externalId": "T111"}, "secrets": {"token": "resource-fixture-token"}});
    let resource = resources::save(&s, "example", &instance, "workspace", &input, None)
        .await
        .unwrap();
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let entered_handler = entered.clone();
    let release_handler = release.clone();
    let app = axum::Router::new().route(
        "/check",
        axum::routing::get(move || {
            let entered = entered_handler.clone();
            let release = release_handler.clone();
            async move {
                entered.notify_one();
                release.notified().await;
                axum::Json(json!({"message": "must not reach the package"}))
            }
        }),
    );
    let (origin, server) = common::serve_locally(app).await;
    s.plugins.route_http_to(Some(origin));
    let action_service = s.clone();
    let action_instance = instance.clone();
    let action = json!({"action": "check", "payload": {"resource": resource["id"]}});
    let pending = tokio::spawn(async move {
        panels::action(&action_service, "example", &action_instance, &action).await
    });
    tokio::time::timeout(std::time::Duration::from_secs(3), entered.notified())
        .await
        .unwrap();
    let mut input = input;
    input["revision"] = resource["revision"].clone();
    input["enabled"] = json!(false);
    resources::save(
        &s,
        "example",
        &instance,
        "workspace",
        &input,
        Some(text(&resource, "id")),
    )
    .await
    .unwrap();
    release.notify_one();
    assert_eq!(pending.await.unwrap().unwrap_err().status, 403);
    server.abort();
}

#[tokio::test]
async fn setup_resources_are_owner_only_partial_and_inert_until_activation() {
    let (_root, s, _) = fixture("return {}", false).await;
    let app = cairn_installation::http::router(s.clone()).await.unwrap();
    let request = |method: &str, path: &str, body: Value, role| {
        let mut request = common::request(method, path)
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();
        request
            .extensions_mut()
            .insert(InstallationIdentity::trusted(role, "fixture-owner"));
        request
    };
    let collection = "/api/plugins/example/setup-drafts";
    let response = common::send(
        &app,
        request(
            "POST",
            collection,
            json!({
                "name": "Prepare app",
                "settings": {},
            }),
            InstallationRole::Owner,
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let draft = common::read_json(response).await;
    let path = format!("{collection}/{}/resources/workspace", text(&draft, "id"));
    let input = json!({
        "setupRevision": draft["revision"],
        "name": "Pending workspace",
        "settings": {},
    });
    let member = common::send(
        &app,
        request("POST", &path, input.clone(), InstallationRole::Member),
    )
    .await;
    assert_eq!(member.status(), StatusCode::FORBIDDEN);
    let response = common::send(&app, request("POST", &path, input, InstallationRole::Owner)).await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "{}",
        common::read_text(response).await
    );
    let resource = common::read_json(response).await;
    assert_eq!(resource["settings"], json!({}));
    assert_eq!(resource["secretKeys"], json!([]));
    let item = format!("{path}/{}", text(&resource, "id"));
    let response = common::send(
        &app,
        request("GET", &path, Value::Null, InstallationRole::Owner),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        common::read_json(response).await["items"],
        json!([resource])
    );
    let live = format!(
        "/api/plugins/example/connections/{}/resources/workspace",
        text(&draft, "id")
    );
    for method in ["GET", "POST"] {
        let response = common::send(
            &app,
            request(
                method,
                &live,
                json!({
                    "name": "Must not become live",
                    "settings": {"externalId": "T111"},
                    "secrets": {"token": "fixture-resource-token"},
                }),
                InstallationRole::Owner,
            ),
        )
        .await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }
    let update = json!({
        "setupRevision": draft["revision"],
        "revision": resource["revision"],
        "name": "Connected workspace",
        "settings": {"externalId": "T111"},
        "secrets": {"token": "fixture-resource-token"},
    });
    let response = common::send(
        &app,
        request("PUT", &item, update.clone(), InstallationRole::Owner),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let updated = common::read_json(response).await;
    assert_eq!(updated["id"], resource["id"]);
    assert_eq!(updated["secretKeys"], json!(["token"]));
    assert!(!updated.to_string().contains("fixture-resource-token"));
    let stale = common::send(&app, request("PUT", &item, update, InstallationRole::Owner)).await;
    assert_eq!(stale.status(), StatusCode::CONFLICT);
    let root_path = format!("{collection}/{}", text(&draft, "id"));
    let deleted = common::send(
        &app,
        request(
            "DELETE",
            &root_path,
            json!({
                "revision": draft["revision"],
            }),
            InstallationRole::Owner,
        ),
    )
    .await;
    assert_eq!(deleted.status(), StatusCode::OK);
    let missing = common::send(
        &app,
        request("GET", &item, Value::Null, InstallationRole::Owner),
    )
    .await;
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
    assert!(s.chat_list().await.unwrap().is_empty());
}

#[tokio::test]
async fn setup_resource_parent_trees_never_cross_into_other_drafts_or_live_instances() {
    let (_root, s, instance) = fixture("return {}", false).await;
    let package = registry::active(&s, "example").await.unwrap();
    let mut manifest = serde_json::to_value(&package.manifest).unwrap();
    manifest["version"] = json!("2");
    manifest["resources"].as_array_mut().unwrap().push(json!({
        "name": "channel",
        "title": "Channels",
        "singular": "Channel",
        "parent": "workspace",
        "settings": [],
    }));
    let installed = registry::install(&s, &json!({"manifest": manifest, "source": "return {}"}))
        .await
        .unwrap();
    registry::approve(&s, "example", "2", text(&installed, "digest"))
        .await
        .unwrap();
    let live_parent = resources::save(
        &s,
        "example",
        text(&instance, "id"),
        "workspace",
        &json!({
            "name": "Live workspace",
            "settings": {"externalId": "T-live"},
            "secrets": {"token": "live-token-fixture"},
        }),
        None,
    )
    .await
    .unwrap();
    let app = cairn_installation::http::router(s.clone()).await.unwrap();
    let request = |method: &str, path: &str, value: Value| {
        let mut request = common::request(method, path)
            .header("content-type", "application/json")
            .body(Body::from(value.to_string()))
            .unwrap();
        request
            .extensions_mut()
            .insert(InstallationIdentity::trusted(
                InstallationRole::Owner,
                "fixture-owner",
            ));
        request
    };
    let collection = "/api/plugins/example/setup-drafts";
    let mut drafts = Vec::new();
    let mut parents = Vec::new();
    for name in ["First", "Second"] {
        let response = common::send(&app, request("POST", collection, json!({"name": name}))).await;
        assert_eq!(response.status(), StatusCode::OK);
        let draft = common::read_json(response).await;
        let path = format!("{collection}/{}/resources/workspace", text(&draft, "id"));
        let response = common::send(
            &app,
            request(
                "POST",
                &path,
                json!({
                    "setupRevision": draft["revision"],
                    "name": name,
                }),
            ),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        parents.push(common::read_json(response).await);
        drafts.push(draft);
    }
    let channels = format!("{collection}/{}/resources/channel", text(&drafts[0], "id"));
    for parent in [&parents[1], &live_parent] {
        let response = common::send(
            &app,
            request(
                "POST",
                &channels,
                json!({
                    "setupRevision": drafts[0]["revision"],
                    "name": "Foreign parent",
                    "parentId": parent["id"],
                }),
            ),
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
    for revision in [Value::Null, json!("stale")] {
        let response = common::send(
            &app,
            request(
                "POST",
                &channels,
                json!({
                    "setupRevision": revision,
                    "name": "Stale setup",
                    "parentId": parents[0]["id"],
                }),
            ),
        )
        .await;
        assert_eq!(response.status(), StatusCode::CONFLICT);
    }
    let response = common::send(
        &app,
        request(
            "POST",
            &channels,
            json!({
                "setupRevision": drafts[0]["revision"],
                "name": "Correct parent",
                "parentId": parents[0]["id"],
            }),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let child = common::read_json(response).await;
    assert_eq!(child["parentId"], parents[0]["id"]);
    let parent_item = format!(
        "{collection}/{}/resources/workspace/{}",
        text(&drafts[0], "id"),
        text(&parents[0], "id")
    );
    let deleted = common::send(
        &app,
        request(
            "DELETE",
            &parent_item,
            json!({
                "setupRevision": drafts[0]["revision"],
                "revision": parents[0]["revision"],
            }),
        ),
    )
    .await;
    assert_eq!(deleted.status(), StatusCode::OK);
    let response = common::send(&app, request("GET", &channels, Value::Null)).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(common::read_json(response).await["items"], json!([]));
    let other_item = format!(
        "{collection}/{}/resources/workspace/{}",
        text(&drafts[1], "id"),
        text(&parents[1], "id")
    );
    assert_eq!(
        common::send(&app, request("GET", &other_item, Value::Null))
            .await
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        resources::get(
            &s,
            "example",
            text(&instance, "id"),
            "workspace",
            text(&live_parent, "id")
        )
        .await
        .unwrap()
        .settings["externalId"],
        "T-live"
    );
}

#[tokio::test]
async fn large_resources_remain_listable_through_the_real_luau_worker() {
    let root = TempDir::new().unwrap();
    let s = Service::new(common::config(root.path())).await.unwrap();
    common::relay_fixture::claimed(&s).await.unwrap();
    s.plugins
        .use_worker(PathBuf::from(env!("CARGO_BIN_EXE_cairn")));
    let fields = (0..6)
        .map(|n| {
            json!({
                "key": format!("field{n}"), "title": format!("Field {n}"),
                "type": "string", "maxLength": 10_000,
            })
        })
        .collect::<Vec<_>>();
    let source = r#"return {action = function(event)
        local cursor = nil
        local seen = {}
        local total = 0
        local pages = 0
        repeat
            local page = cairn.resources.list({collection = 'workspace', cursor = cursor})
            pages += 1
            assert(#page.items > 0)
            for _, value in page.items do
                assert(seen[value.id] == nil)
                seen[value.id] = true
                assert(#value.settings.field0 == 10000)
                assert(value.settings.token == nil)
                total += 1
            end
            local nextCursor = page.nextCursor
            assert(nextCursor == nil or nextCursor ~= cursor)
            cursor = nextCursor
        until cursor == nil
        assert(total == 18 and pages > 1)
        return {notice = 'Listed all 18 resources'}
    end}"#;
    let manifest = json!({
        "id": "resource-pages", "name": "Resource pages", "version": "1", "apiVersion": 2,
        "capabilities": ["resources"], "instanceView": true,
        "resources": [{"name": "workspace", "title": "Workspaces", "singular": "Workspace", "settings": fields}],
    });
    assert!(manifest.to_string().len() < 2000);
    let installed = registry::install(&s, &json!({"manifest": manifest, "source": source}))
        .await
        .unwrap();
    registry::approve(&s, "resource-pages", "1", text(&installed, "digest"))
        .await
        .unwrap();
    let instance = connections::save(
        &s,
        "resource-pages",
        &json!({"name": "Large resources", "enabled": true}),
        None,
    )
    .await
    .unwrap();
    let instance = text(&instance, "id");
    let settings = (0..6)
        .map(|n| (format!("field{n}"), json!("x".repeat(10_000))))
        .collect::<serde_json::Map<_, _>>();
    assert!(serde_json::to_vec(&settings).unwrap().len() < 65_536);
    let mut expected = std::collections::BTreeSet::new();
    for n in 0..18 {
        let input = json!({"name": format!("Workspace {n}"), "settings": settings});
        assert!(input.to_string().len() < 150_000);
        let resource = resources::save(&s, "resource-pages", instance, "workspace", &input, None)
            .await
            .unwrap();
        expected.insert(text(&resource, "id").to_owned());
    }
    let first = resources::list(&s, "resource-pages", instance, "workspace", None)
        .await
        .unwrap();
    eprintln!(
        "First resource page's worker reply: {} bytes",
        serde_json::to_vec(&cairn_plugin_api::Frame::Reply { value: first })
            .unwrap()
            .len()
    );
    let result = panels::action(
        &s,
        "resource-pages",
        instance,
        &json!({"action": "enumerate"}),
    )
    .await;
    assert!(
        result.is_ok(),
        "Real Luau enumeration of accepted resources failed: {result:?}"
    );
    assert_eq!(result.unwrap()["notice"], "Listed all 18 resources");
    let app = cairn_installation::http::router(s.clone()).await.unwrap();
    let path = format!("/api/plugins/resource-pages/connections/{instance}/resources/workspace");
    let mut cursor: Option<String> = None;
    let mut seen = std::collections::BTreeSet::new();
    let mut pages = 0;
    loop {
        let url = cursor
            .as_ref()
            .map_or_else(|| path.clone(), |cursor| format!("{path}?cursor={cursor}"));
        let mut request = common::request("GET", &url).body(Body::empty()).unwrap();
        request
            .extensions_mut()
            .insert(InstallationIdentity::trusted(
                InstallationRole::Owner,
                "fixture-owner",
            ));
        let response = common::send(&app, request).await;
        assert_eq!(response.status(), StatusCode::OK);
        let page = common::read_json(response).await;
        let bytes = serde_json::to_vec(&cairn_plugin_api::Frame::Reply {
            value: page.clone(),
        })
        .unwrap()
        .len();
        assert!(
            bytes < cairn_plugin_api::MAX_FRAME_BYTES,
            "Resource page exceeds worker frame: {bytes} bytes"
        );
        pages += 1;
        for value in page["items"].as_array().unwrap() {
            assert!(seen.insert(text(value, "id").to_owned()));
        }
        let next = page["nextCursor"].as_str().map(str::to_owned);
        assert!(next.is_none() || next != cursor);
        cursor = next;
        if cursor.is_none() {
            break;
        }
        assert!(pages < 18);
    }
    assert_eq!(seen, expected);
    assert!(pages > 1);
}
