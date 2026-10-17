mod common;

use axum::{http::StatusCode, response::IntoResponse};
use cairn_installation::{
    auth::{InstallationIdentity, InstallationRole},
    plugins::{panels, registry},
    service::Service,
    validation::text,
};
use serde_json::{Value, json};
use std::{path::PathBuf, sync::Arc};
use tempfile::TempDir;

fn request(method: &str, path: &str, body: Value) -> axum::extract::Request {
    let mut request = common::request(method, path)
        .header("content-type", "application/json")
        .body(axum::Json(body).into_response().into_body())
        .unwrap();
    request
        .extensions_mut()
        .insert(InstallationIdentity::trusted(
            InstallationRole::Owner,
            "fixture-owner",
        ));
    request
}

async fn fixture(
    complete: bool,
    derived_required: bool,
) -> (TempDir, Arc<Service>, axum::Router, Value, Value) {
    let root = TempDir::new().unwrap();
    let s = Service::new(common::config(root.path())).await.unwrap();
    common::relay_fixture::claimed(&s).await.unwrap();
    s.plugins
        .use_worker(PathBuf::from(env!("CARGO_BIN_EXE_cairn")));
    let package = registry::install(&s, &json!({
        "manifest": {
            "id": "setup-check", "name": "Setup check", "version": "1", "apiVersion": 2,
            "capabilities": ["resources", "http"], "hosts": ["api.example.test"],
            "credential": {"label": "App key", "header": "x-app-key"},
            "instanceForm": true, "instanceView": true,
            "settings": [
                {"key": "label", "title": "Label", "type": "string", "required": true},
                {"key": "rootSecret", "title": "Root secret", "type": "secret", "required": true},
                {"key": "callbackId", "title": "Generated ID", "type": "string", "required": derived_required},
                {"key": "callbackUrl", "title": "Callback", "type": "url"},
            ],
            "resources": [{"name": "workspace", "title": "Workspaces", "singular": "Workspace", "settings": [
                {"key": "externalId", "title": "Workspace ID", "type": "string", "required": true},
                {"key": "token", "title": "Workspace token", "type": "secret", "required": true},
            ]}],
        },
        "source": r#"return {
            form = function(event)
                assert(event.submit and event.instance ~= nil and event.setup ~= nil)
                assert(event.values.rootSecret == nil and event.credential)
                local response = cairn.http.request({url = 'https://api.example.test/validate', auth = true})
                assert(response.json.ok)
                return {values = {callbackId = event.instance, callbackUrl = event.setup.urls.oauthCallback}}
            end,
            action = function(event)
                local app = cairn.http.request({url = 'https://api.example.test/app', auth = true})
                local root = cairn.http.request({url = 'https://api.example.test/root', auth = {secret = 'rootSecret'}})
                local workspace = cairn.http.request({url = 'https://api.example.test/workspace', auth = {resource = event.payload.resource, secret = 'token'}})
                return {notice = app.json.ok and root.json.ok and workspace.json.ok and 'All credentials retained' or 'Wrong credential'}
            end,
        }"#,
    })).await.unwrap();
    registry::approve(&s, "setup-check", "1", text(&package, "digest"))
        .await
        .unwrap();
    let app = cairn_installation::http::router(s.clone()).await.unwrap();
    let response = common::send(
        &app,
        request(
            "POST",
            "/api/plugins/setup-check/setup-drafts",
            json!({
                "name": "App setup", "settings": {"label": "Saved label"},
                "token": "app-key-fixture", "secrets": {"rootSecret": "root-secret-fixture"},
            }),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let draft = common::read_json(response).await;
    let collection = format!(
        "/api/plugins/setup-check/setup-drafts/{}/resources/workspace",
        text(&draft, "id")
    );
    let response = common::send(&app, request("POST", &collection, json!({
        "setupRevision": draft["revision"], "name": "Workspace",
        "settings": if complete {json!({"externalId": "T111"})} else {json!({})},
        "secrets": if complete {json!({"token": "workspace-token-fixture"})} else {json!({})},
    }))).await;
    assert_eq!(response.status(), StatusCode::OK);
    let resource = common::read_json(response).await;
    (root, s, app, draft, resource)
}

fn provider() -> axum::Router {
    axum::Router::new().route(
        "/{kind}",
        axum::routing::get(
            |axum::extract::Path(kind): axum::extract::Path<String>,
             headers: axum::http::HeaderMap| async move {
                let expected = match kind.as_str() {
                    "validate" | "app" => ("x-app-key", "app-key-fixture"),
                    "root" => ("authorization", "root-secret-fixture"),
                    "workspace" => ("authorization", "workspace-token-fixture"),
                    _ => panic!("unexpected provider request"),
                };
                assert_eq!(headers[expected.0], expected.1);
                axum::Json(json!({"ok": true}))
            },
        ),
    )
}

#[tokio::test]
async fn incomplete_resources_roll_back_activation_and_the_final_form_can_supply_required_values() {
    let (_root, s, app, draft, resource) = fixture(false, true).await;
    let (origin, server) = common::serve_locally(provider()).await;
    s.plugins.route_http_to(Some(origin));
    let root = format!(
        "/api/plugins/setup-check/setup-drafts/{}",
        text(&draft, "id")
    );
    let activation = format!("{root}/activate");
    let input = json!({"revision": draft["revision"], "enabled": true});
    let response = common::send(&app, request("POST", &activation, input.clone())).await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        common::send(&app, request("GET", &root, Value::Null))
            .await
            .status(),
        StatusCode::OK
    );
    assert!(
        s.store
            .get("plugin-connections", text(&draft, "id"))
            .await
            .unwrap()
            .is_none()
    );
    let path = format!("{root}/resources/workspace/{}", text(&resource, "id"));
    let response = common::send(
        &app,
        request(
            "PUT",
            &path,
            json!({
                "setupRevision": draft["revision"], "revision": resource["revision"],
                "name": "Workspace", "settings": {"externalId": "T111"},
                "secrets": {"token": "workspace-token-fixture"},
            }),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let response = common::send(&app, request("POST", &activation, input)).await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "{}",
        common::read_text(response).await
    );
    let active = common::read_json(response).await;
    assert_eq!(active["id"], draft["id"]);
    assert_eq!(active["settings"]["callbackId"], draft["id"]);
    assert!(
        text(&active["settings"], "callbackUrl")
            .ends_with(&format!("/plugin-oauth/{}/callback", text(&draft, "id")))
    );
    let result = panels::action(
        &s,
        "setup-check",
        text(&draft, "id"),
        &json!({"action": "check", "payload": {"resource": resource["id"]}}),
    )
    .await
    .unwrap();
    assert_eq!(result["notice"], "All credentials retained");
    server.abort();
}

#[tokio::test]
async fn held_activation_cannot_publish_changed_revoked_or_already_consumed_setup() {
    for change in [
        "edit-root",
        "delete-root",
        "edit-resource",
        "delete-resource",
        "expire",
        "approval",
        "owner",
        "activate",
    ] {
        let (_root, s, app, draft, resource) = fixture(true, false).await;
        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let hold = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let (read_entered, read_release) = (entered.clone(), release.clone());
        let provider = axum::Router::new().route(
            "/validate",
            axum::routing::get(move |headers: axum::http::HeaderMap| {
                let (entered, release, hold) =
                    (read_entered.clone(), read_release.clone(), hold.clone());
                async move {
                    assert_eq!(headers["x-app-key"], "app-key-fixture");
                    if hold.swap(false, std::sync::atomic::Ordering::SeqCst) {
                        entered.notify_one();
                        release.notified().await;
                    }
                    axum::Json(json!({"ok": true}))
                }
            }),
        );
        let (origin, server) = common::serve_locally(provider).await;
        s.plugins.route_http_to(Some(origin));
        let root_path = format!(
            "/api/plugins/setup-check/setup-drafts/{}",
            text(&draft, "id")
        );
        let activation = format!("{root_path}/activate");
        let input = json!({"revision": draft["revision"], "enabled": true});
        let pending_app = app.clone();
        let initial = request("POST", &activation, input.clone());
        let pending = tokio::spawn(async move { common::send(&pending_app, initial).await });
        tokio::time::timeout(std::time::Duration::from_secs(3), entered.notified())
            .await
            .unwrap();
        let item = format!("{root_path}/resources/workspace/{}", text(&resource, "id"));
        let mut owner_server = None;
        match change {
            "edit-root" => {
                assert_eq!(common::send(&app, request("PATCH", &root_path, json!({
                    "revision": draft["revision"], "name": "Renamed setup", "settings": {"label": "Saved label"},
                }))).await.status(), StatusCode::OK);
            }
            "delete-root" => {
                assert_eq!(
                    common::send(
                        &app,
                        request("DELETE", &root_path, json!({"revision": draft["revision"]}))
                    )
                    .await
                    .status(),
                    StatusCode::OK
                );
            }
            "edit-resource" | "delete-resource" => {
                let response = common::send(
                    &app,
                    request(
                        if change == "edit-resource" {
                            "PUT"
                        } else {
                            "DELETE"
                        },
                        &item,
                        json!({
                            "setupRevision": draft["revision"], "revision": resource["revision"],
                            "name": "Changed workspace", "settings": {"externalId": "T111"},
                        }),
                    ),
                )
                .await;
                assert_eq!(response.status(), StatusCode::OK);
            }
            "expire" => {
                let key = format!("plugin-setup:{}", text(&draft, "id"));
                let mut saved = s.store.kv(&key).await.unwrap().unwrap();
                saved["expiresAt"] = json!(cairn_installation::config::now() - 1);
                s.store.set(&key, saved, None).await.unwrap();
            }
            "approval" => {
                let package = registry::active(&s, "setup-check").await.unwrap();
                registry::approve(&s, "setup-check", "1", &package.digest)
                    .await
                    .unwrap();
            }
            "owner" => {
                let authority = axum::Router::new().route("/api/relay/{id}/task-authors", axum::routing::get(|| async {
                    axum::Json(json!({"owner": {"account_id": "fixture-owner", "access_id": "replaced-access-fixture"}, "members": []}))
                }));
                let (origin, server) = common::serve_locally(authority).await;
                let file = s.config.data_dir.join("installation-relay/identity.json");
                let mut identity: Value =
                    serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
                identity["origin"] = json!(origin);
                std::fs::write(&file, serde_json::to_vec(&identity).unwrap()).unwrap();
                owner_server = Some(server);
            }
            "activate" => {
                let response = common::send(&app, request("POST", &activation, input)).await;
                assert_eq!(
                    response.status(),
                    StatusCode::OK,
                    "{}",
                    common::read_text(response).await
                );
            }
            _ => unreachable!(),
        }
        release.notify_one();
        let response = pending.await.unwrap();
        assert!(
            matches!(
                response.status(),
                StatusCode::CONFLICT | StatusCode::NOT_FOUND | StatusCode::FORBIDDEN
            ),
            "{change}: {}",
            common::read_text(response).await
        );
        let live = s
            .store
            .get("plugin-connections", text(&draft, "id"))
            .await
            .unwrap();
        assert_eq!(
            live.is_some(),
            change == "activate",
            "{change} published a partial or revoked app"
        );
        if let Some(server) = owner_server {
            server.abort();
        }
        server.abort();
    }
}

#[tokio::test]
async fn promoted_credentials_survive_the_prepared_setup_deadline() {
    let (_root, s, app, draft, resource) = fixture(true, false).await;
    let deadline = cairn_installation::config::now() + 2000;
    let key = format!("plugin-setup:{}", text(&draft, "id"));
    let mut value = s.store.kv(&key).await.unwrap().unwrap();
    value["expiresAt"] = json!(deadline);
    value["createdAt"] = json!(deadline - 86_400_000);
    s.store.set(&key, value, Some(deadline)).await.unwrap();
    for key in [
        format!("mcp-secret:plugin-credential:{}", text(&draft, "id")),
        format!("mcp-secret:plugin-secrets:{}", text(&draft, "id")),
        format!(
            "plugin-setup-resource:{}:{}",
            text(&draft, "id"),
            text(&resource, "id")
        ),
        format!(
            "mcp-secret:plugin-resource-secrets:{}",
            text(&resource, "id")
        ),
    ] {
        let value = s.store.kv(&key).await.unwrap().unwrap();
        s.store.set(&key, value, Some(deadline)).await.unwrap();
    }
    let (origin, server) = common::serve_locally(provider()).await;
    s.plugins.route_http_to(Some(origin));
    let root_path = format!(
        "/api/plugins/setup-check/setup-drafts/{}",
        text(&draft, "id")
    );
    let response = common::send(
        &app,
        request(
            "POST",
            &format!("{root_path}/activate"),
            json!({"revision": draft["revision"], "enabled": true}),
        ),
    )
    .await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "{}",
        common::read_text(response).await
    );
    let remaining = (deadline - cairn_installation::config::now()).max(0) as u64;
    tokio::time::sleep(std::time::Duration::from_millis(remaining + 30)).await;
    let result = panels::action(
        &s,
        "setup-check",
        text(&draft, "id"),
        &json!({"action": "check", "payload": {"resource": resource["id"]}}),
    )
    .await
    .unwrap();
    assert_eq!(result["notice"], "All credentials retained");
    server.abort();
}
