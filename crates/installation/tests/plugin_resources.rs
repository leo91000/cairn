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
