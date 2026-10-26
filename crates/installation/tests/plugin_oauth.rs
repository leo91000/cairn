mod common;
#[path = "plugin_oauth/integration.rs"]
mod integration;
#[path = "plugin_oauth/management.rs"]
mod management;
#[path = "plugin_oauth/receipts.rs"]
mod receipts;

use axum::{http::StatusCode, response::IntoResponse};
use cairn_installation::{
    auth::{InstallationIdentity, InstallationRole},
    config::MAIN_AGENT_ID,
    plugins::{
        agents, capabilities, connections, grants, panels, registry, resources, scope::Scope,
    },
    service::Service,
    validation::text,
};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};
use tempfile::TempDir;

fn request(method: &str, path: &str, body: Value, account: Option<&str>) -> axum::extract::Request {
    let mut request = common::request(method, path)
        .header("content-type", "application/json")
        .body(axum::Json(body).into_response().into_body())
        .unwrap();
    if let Some(account) = account {
        request
            .extensions_mut()
            .insert(InstallationIdentity::trusted(
                InstallationRole::Owner,
                account,
            ));
    }
    request
}

async fn fixture(pkce: bool) -> (TempDir, Arc<Service>, String, String, String, axum::Router) {
    configured_fixture(pkce, false).await
}

async fn configured_fixture(
    pkce: bool,
    personal: bool,
) -> (TempDir, Arc<Service>, String, String, String, axum::Router) {
    configured_form_fixture(pkce, personal, false).await
}

async fn configured_form_fixture(
    pkce: bool,
    personal: bool,
    resource_forms: bool,
) -> (TempDir, Arc<Service>, String, String, String, axum::Router) {
    let root = TempDir::new().unwrap();
    std::fs::create_dir_all(root.path().join("home")).unwrap();
    let mut config = common::config(root.path());
    config.runner_url = "http://runner:4311".into();
    let s = Service::new(config).await.unwrap();
    common::relay_fixture::claimed(&s).await.unwrap();
    s.plugins
        .use_worker(PathBuf::from(env!("CARGO_BIN_EXE_cairn")));
    let mut manifest = json!({
        "id": "example", "name": "Example", "version": "1", "apiVersion": 2,
        "capabilities": ["resources", "oauth", "http"], "hosts": ["api.example.test"], "instanceView": true,
        "agentAccess": {},
        "operations": [{
            "name": "read", "collection": "workspace", "profile": "read",
            "requests": [{"method": "GET", "host": "api.example.test", "path": "/messages"}]
        }],
        "tools": [{
            "name": "example_read", "description": "Read messages", "operation": "read",
            "inputSchema": {
                "type": "object", "required": ["resource"],
                "properties": {"resource": {"type": "string"}}
            }
        }],
        "settings": [
            {"key": "clientId", "title": "Client ID", "type": "string"},
            {"key": "clientSecret", "title": "Client secret", "type": "secret"}
        ],
        "resources": [{"name": "workspace", "title": "Workspaces", "singular": "Workspace", "settings": [
            {"key": "externalId", "title": "Workspace ID", "type": "string", "pattern": "^T[0-9]+$"}
        ]}],
        "oauth": [{
            "name": "bot", "title": "Connect workspace", "collection": "workspace",
            "authorizationUrl": "https://api.example.test/authorize", "tokenUrl": "https://api.example.test/token",
            "clientId": "clientId", "clientSecret": "clientSecret", "scopes": ["messages:read", "messages:write"],
            "token": {
                "accessToken": "/access_token", "refreshToken": "/refresh_token",
                "expiresIn": "/expires_in", "scope": "/scope"
            },
            "metadata": {"externalId": "/team/id"}
        }]
    });
    if pkce {
        manifest["oauth"][0]
            .as_object_mut()
            .unwrap()
            .remove("clientSecret");
        manifest["oauth"][0]["pkce"] = json!(true);
        manifest["oauth"][0]["token"] = json!({
            "accessToken": "/authed_user/access_token", "refreshToken": "/authed_user/refresh_token",
            "expiresIn": "/authed_user/expires_in", "scope": "/authed_user/scope"
        });
        manifest["oauth"][0]["refreshTokenResponse"] = json!({
            "accessToken": "/access_token", "refreshToken": "/refresh_token", "expiresIn": "/expires_in", "scope": "/scope"
        });
    }
    if personal {
        manifest["oauth"][0]["personal"] = json!({"subject": "/authed_user/id"});
        manifest["commands"] = json!([{
            "name": "read", "operation": "read", "arguments": [{"name": "resource", "flag": "--resource", "required": true}]
        }]);
    }
    if resource_forms {
        manifest["resources"].as_array_mut().unwrap().push(json!({
            "name": "channel", "title": "Channels", "singular": "Channel", "parent": "workspace", "form": true,
            "settings": [
                {"key": "verified", "title": "Verified status", "type": "string", "hidden": true},
                {"key": "channel", "title": "Channel", "type": "select", "dynamic": true, "remote": true},
            ]
        }));
    }
    let source = r#"local function read(resource)
        return cairn.http.request({url = 'https://api.example.test/messages',
            auth = {resource = resource, oauth = 'bot'}})
    end
    return {action = function(event)
        local response = read(event.payload.resource)
        return {notice = response.json.ok and 'Authorized' or 'Denied'}
    end, tool = function(event) return read(event.input.resource).json end,
    command = function(event) return read(event.arguments.resource).json end}"#;
    let source = if resource_forms {
        let form = r#"form = function(event)
            local response = cairn.http.request({url = 'https://api.example.test/messages', auth = {configuration = 'parent', oauth = 'bot'}})
            local label = response.json.ok and 'Authorized' or 'Denied'
            if event.lookup then
                return {options = {channel = {{value = 'C1', label = label}}}, pages = {channel = {nextCursor = cairn.json.null}}}
            end
            return {values = {verified = label}}
        end}"#;
        format!("{}, {form}", source.strip_suffix('}').unwrap())
    } else {
        source.to_owned()
    };
    let package = registry::install(&s, &json!({"manifest": manifest, "source": source}))
        .await
        .unwrap();
    registry::approve(&s, "example", "1", text(&package, "digest"))
        .await
        .unwrap();
    let instance = connections::save(
        &s,
        "example",
        &json!({
            "name": "Example app", "enabled": true, "settings": {"clientId": "client-fixture"},
            "secrets": {"clientSecret": "client-secret-fixture"}
        }),
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
        &json!({"name": "Engineering", "settings": if personal {json!({"externalId": "T111"})} else {json!({})}}),
        None,
    )
    .await
    .unwrap();
    let resource_id = text(&resource, "id");
    let path = format!(
        "/api/plugins/example/connections/{instance}/resources/workspace/{resource_id}/oauth/bot"
    );
    let app = cairn_installation::http::router(s.clone()).await.unwrap();
    (root, s, instance.into(), resource_id.into(), path, app)
}

async fn begin(app: &axum::Router, path: &str) -> Value {
    let response = common::send(
        app,
        request(
            "POST",
            path,
            json!({"scopes": ["messages:read"]}),
            Some("fixture-owner"),
        ),
    )
    .await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "{}",
        common::read_text(response).await
    );
    common::read_json(response).await
}

#[tokio::test]
async fn owner_oauth_captures_tokens_without_exposing_them_and_binds_the_resource_identity() {
    let (_root, s, instance, resource_id, path, app) = fixture(false).await;
    let instance = instance.as_str();
    let resource_id = resource_id.as_str();
    let started = begin(&app, &path).await;
    let authorization = url::Url::parse(text(&started, "url")).unwrap();
    let query: HashMap<_, _> = authorization.query_pairs().into_owned().collect();
    assert_eq!(query["client_id"], "client-fixture");
    assert_eq!(query["scope"], "messages:read");
    assert_eq!(query["state"], started["state"]);
    assert!(!started.to_string().contains("client-secret-fixture"));
    let redirect_uri = query["redirect_uri"].clone();
    let provider = axum::Router::new().route("/token", axum::routing::post(move |
        headers: axum::http::HeaderMap, axum::Form(form): axum::Form<HashMap<String, String>>
    | {
        let redirect_uri = redirect_uri.clone();
        async move {
            assert!(headers["authorization"].to_str().unwrap().starts_with("Basic "));
            assert_eq!(form["code"], "one-use-code-fixture");
            assert_eq!(form["redirect_uri"], redirect_uri);
            axum::Json(json!({"access_token": "oauth-bot-fixture", "refresh_token": "oauth-refresh-fixture",
                "expires_in": 43200, "scope": "messages:read", "team": {"id": "T111"},
                "authed_user": {"access_token": "must-never-escape-personal-fixture"}}))
        }
    })).route("/messages", axum::routing::get(|headers: axum::http::HeaderMap| async move {
        assert_eq!(headers["authorization"], "Bearer oauth-bot-fixture");
        axum::Json(json!({"ok": true}))
    }));
    let (origin, server) = common::serve_locally(provider).await;
    s.plugins.route_http_to(Some(origin));
    let callback = format!(
        "/api/plugin-oauth/{instance}/callback?state={}&code=one-use-code-fixture",
        text(&started, "state")
    );
    let response = common::send(&app, request("GET", &callback, Value::Null, None)).await;
    assert_eq!(response.status(), StatusCode::OK);
    let completion_page = common::read_text(response).await;
    assert!(!completion_page.contains("one-use-code-fixture"));
    let finish = format!("{path}/finish");
    let state = json!({"state": started["state"]});
    let denied = common::send(
        &app,
        request("POST", &finish, state.clone(), Some("other-owner")),
    )
    .await;
    assert_eq!(denied.status(), StatusCode::FORBIDDEN);
    let finished = common::send(
        &app,
        request("POST", &finish, state.clone(), Some("fixture-owner")),
    )
    .await;
    assert_eq!(
        finished.status(),
        StatusCode::OK,
        "{}",
        common::read_text(finished).await
    );
    let finished = common::read_json(finished).await;
    assert_eq!(finished["status"], "authorized");
    assert_eq!(finished["scopes"], json!(["messages:read"]));
    assert!(!finished.to_string().contains("fixture"));
    assert_eq!(
        resources::get(&s, "example", instance, "workspace", resource_id)
            .await
            .unwrap()
            .settings["externalId"],
        "T111"
    );
    let replay = common::send(&app, request("POST", &finish, state, Some("fixture-owner"))).await;
    assert_eq!(replay.status(), StatusCode::CONFLICT);
    let result = panels::action(
        &s,
        "example",
        instance,
        &json!({"action": "read", "payload": {"resource": resource_id}}),
    )
    .await
    .unwrap();
    assert_eq!(result["notice"], "Authorized");
    let current = resources::get(&s, "example", instance, "workspace", resource_id)
        .await
        .unwrap();
    resources::save(&s, "example", instance, "workspace", &json!({
        "name": "Another workspace", "revision": current.revision, "settings": {"externalId": "T222"}
    }), Some(resource_id)).await.unwrap();
    let status = common::send(
        &app,
        request("GET", &path, Value::Null, Some("fixture-owner")),
    )
    .await;
    assert_eq!(common::read_json(status).await["status"], "reconnect");
    assert!(
        panels::action(
            &s,
            "example",
            instance,
            &json!({"action": "read", "payload": {"resource": resource_id}})
        )
        .await
        .is_err()
    );
    let revoked = common::send(
        &app,
        request("DELETE", &path, Value::Null, Some("fixture-owner")),
    )
    .await;
    assert_eq!(revoked.status(), StatusCode::OK);
    assert!(
        panels::action(
            &s,
            "example",
            instance,
            &json!({"action": "read", "payload": {"resource": resource_id}})
        )
        .await
        .is_err()
    );
    server.abort();
}

#[tokio::test]
async fn owner_resource_forms_share_one_parent_refresh_for_active_and_inert_configuration() {
    for inert in [false, true] {
        resource_form_parent_refresh(inert, false).await;
    }
}

#[tokio::test]
async fn owner_resource_forms_never_retry_an_uncertain_parent_refresh() {
    for inert in [false, true] {
        resource_form_parent_refresh(inert, true).await;
    }
}

async fn resource_form_parent_refresh(inert: bool, uncertain: bool) {
    let (_root, s, active, active_resource, active_path, app) =
        configured_form_fixture(false, false, true).await;
    let (instance, resource, oauth, setup_revision) = if inert {
        let (draft, resource, path) = create_setup(&app).await;
        (
            text(&draft, "id").to_owned(),
            text(&resource, "id").to_owned(),
            path,
            Some(draft["revision"].clone()),
        )
    } else {
        connections::set_enabled(&s, "example", &active, &json!({"enabled": false}))
            .await
            .unwrap();
        (active, active_resource, active_path, None)
    };
    let refreshes = Arc::new(AtomicUsize::new(0));
    let count = refreshes.clone();
    let provider = axum::Router::new()
            .route("/token", axum::routing::post(move |axum::Form(fields): axum::Form<HashMap<String, String>>| {
                let count = count.clone();
                async move {
                    let tokens = if fields["grant_type"] == "refresh_token" {
                        assert_eq!(count.fetch_add(1, Ordering::SeqCst), 0, "Parent refresh was consumed twice");
                        assert_eq!(fields["refresh_token"], "parent-refresh-fixture");
                        if uncertain {return Err(StatusCode::BAD_GATEWAY);}
                        json!({
                            "access_token": "parent-renewed-fixture",
                            "refresh_token": "parent-next-fixture",
                            "expires_in": 43200,
                            "scope": "messages:read",
                        })
                    } else {
                        json!({
                            "access_token": "parent-old-fixture",
                            "refresh_token": "parent-refresh-fixture",
                            "expires_in": 1,
                            "scope": "messages:read",
                            "team": {"id": "T111"},
                        })
                    };
                    Ok(axum::Json(tokens))
                }
            }))
            .route("/messages", axum::routing::get(|headers: axum::http::HeaderMap| async move {
                axum::Json(json!({
                    "ok": headers.get("authorization").is_some_and(|value| value == "Bearer parent-renewed-fixture"),
                }))
            }));
    let (origin, server) = common::serve_locally(provider).await;
    s.plugins.route_http_to(Some(origin));
    let mut body = json!({"scopes": ["messages:read"]});
    if let Some(revision) = &setup_revision {
        body["setupRevision"] = revision.clone();
    }
    let response = common::send(&app, request("POST", &oauth, body, Some("fixture-owner"))).await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "{}",
        common::read_text(response).await
    );
    let started = common::read_json(response).await;
    let callback = format!(
        "/api/plugin-oauth/{instance}/callback?state={}&code=fixture",
        text(&started, "state")
    );
    assert_eq!(
        common::send(&app, request("GET", &callback, Value::Null, None))
            .await
            .status(),
        StatusCode::OK
    );
    let mut body = json!({"state": started["state"]});
    if let Some(revision) = &setup_revision {
        body["setupRevision"] = revision.clone();
    }
    let response = common::send(
        &app,
        request(
            "POST",
            &format!("{oauth}/finish"),
            body,
            Some("fixture-owner"),
        ),
    )
    .await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "{}",
        common::read_text(response).await
    );

    let item = oauth.strip_suffix("/oauth/bot").unwrap();
    let parent = common::read_json(
        common::send(
            &app,
            request("GET", item, Value::Null, Some("fixture-owner")),
        )
        .await,
    )
    .await;
    let mut body = json!({"name": "Paused parent", "settings": parent["settings"], "revision": parent["revision"], "enabled": false});
    if let Some(revision) = &setup_revision {
        body["setupRevision"] = revision.clone();
    }
    let paused = common::send(&app, request("PUT", item, body, Some("fixture-owner"))).await;
    assert_eq!(
        paused.status(),
        StatusCode::OK,
        "{}",
        common::read_text(paused).await
    );
    let collection = item.rsplit_once("/workspace/").unwrap().0;
    let form = format!("{collection}/channel/form");
    let mut body = json!({"parentId": resource, "settings": {}});
    if let Some(revision) = &setup_revision {
        body["setupRevision"] = revision.clone();
    }
    let mut lookup = body.clone();
    lookup["lookup"] = json!({"field": "channel", "query": ""});
    let (first, second) = tokio::join!(
        common::send(
            &app,
            request("POST", &form, lookup.clone(), Some("fixture-owner"))
        ),
        common::send(
            &app,
            request("POST", &form, body.clone(), Some("fixture-owner"))
        )
    );
    for response in [first, second] {
        if uncertain {
            assert!(matches!(
                response.status(),
                StatusCode::BAD_GATEWAY | StatusCode::FORBIDDEN
            ));
        } else {
            assert_eq!(
                response.status(),
                StatusCode::OK,
                "{}",
                common::read_text(response).await
            );
            let result = common::read_json(response).await;
            if result.get("lookup").is_some() {
                assert_eq!(result["lookup"], lookup["lookup"]);
                assert_eq!(result["options"]["channel"][0]["label"], "Authorized");
                assert_eq!(result["pages"]["channel"]["nextCursor"], Value::Null);
            } else {
                assert_eq!(result["values"]["verified"], "Authorized");
            }
        }
    }
    if uncertain {
        let refused = common::send(
            &app,
            request("POST", &form, body.clone(), Some("fixture-owner")),
        )
        .await;
        assert_eq!(refused.status(), StatusCode::FORBIDDEN);
        assert!(
            common::read_text(refused)
                .await
                .contains("renewal was interrupted")
        );
    }
    assert_eq!(refreshes.load(Ordering::SeqCst), 1);
    server.abort();
}

#[tokio::test]
async fn owner_resource_forms_cannot_borrow_personal_oauth() {
    let (_root, s, instance, resource, _oauth, app) =
        configured_form_fixture(false, true, true).await;
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = calls.clone();
    let provider = axum::Router::new().route(
        "/messages",
        axum::routing::get(move || {
            seen.fetch_add(1, Ordering::SeqCst);
            async { axum::Json(json!({"ok": true})) }
        }),
    );
    let (origin, server) = common::serve_locally(provider).await;
    s.plugins.route_http_to(Some(origin));
    let path = format!("/api/plugins/example/connections/{instance}/resources/channel/form");
    for lookup in [Value::Null, json!({"field": "channel", "query": ""})] {
        let response = common::send(
            &app,
            request(
                "POST",
                &path,
                json!({"parentId": resource, "lookup": lookup}),
                Some("fixture-owner"),
            ),
        )
        .await;
        assert_eq!(
            response.status(),
            StatusCode::FORBIDDEN,
            "{}",
            common::read_text(response).await
        );
        assert!(common::read_text(response).await.contains("personal OAuth"));
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    server.abort();
}

#[tokio::test]
async fn confidential_exchange_and_shared_refresh_form_encode_client_credentials() {
    confidential_refresh(true).await;
}

#[tokio::test]
async fn confirmed_nonrotating_refresh_preserves_its_credential_for_the_next_expiry() {
    confidential_refresh(false).await;
}

async fn confidential_refresh(rotates: bool) {
    let (_root, s, instance, resource, path, app) = fixture(false).await;
    connections::save(
        &s,
        "example",
        &json!({
            "name": "Example app", "enabled": true,
            "settings": {"clientId": "app:desktop"},
            "secrets": {"clientSecret": "s+%word: /?="}
        }),
        Some(&instance),
    )
    .await
    .unwrap();
    let started = begin(&app, &path).await;
    let exchanges = Arc::new(AtomicUsize::new(0));
    let exchange_count = exchanges.clone();
    let provider = axum::Router::new().route("/token", axum::routing::post(move |headers: axum::http::HeaderMap, axum::Form(form): axum::Form<HashMap<String, String>>| {
        let count = exchange_count.clone();
        async move {
            // RFC 6749 section 2.3.1: encode each credential before Basic.
            let credentials = base64::Engine::encode(
                &base64::engine::general_purpose::STANDARD,
                "app%3Adesktop:s%2B%25word%3A+%2F%3F%3D",
            );
            assert_eq!(headers["authorization"], format!("Basic {credentials}"));
            assert!(!form.contains_key("client_id"));
            assert!(!form.contains_key("client_secret"));
            let call = count.fetch_add(1, Ordering::SeqCst);
            if call == 0 {
                assert_eq!(form["grant_type"], "authorization_code");
                axum::Json(json!({"access_token": "expiring-fixture", "refresh_token": "refresh-0-fixture", "expires_in": 1,
                    "scope": "messages:read", "team": {"id": "T111"}}))
            } else {
                assert!(call <= if rotates { 1 } else { 2 }, "unexpected refresh replay");
                assert_eq!(form["grant_type"], "refresh_token");
                assert_eq!(form["refresh_token"], "refresh-0-fixture");
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                let mut response = json!({
                    "access_token": "renewed-fixture", "scope": "messages:read",
                    "expires_in": if rotates { 43200 } else { 1 },
                });
                if rotates {
                    response["refresh_token"] = json!("refresh-1-fixture");
                }
                axum::Json(response)
            }
        }
    })).route("/messages", axum::routing::get(|headers: axum::http::HeaderMap| async move {
        assert_eq!(headers["authorization"], "Bearer renewed-fixture");
        axum::Json(json!({"ok": true}))
    }));
    let (origin, server) = common::serve_locally(provider).await;
    s.plugins.route_http_to(Some(origin));
    let callback = format!(
        "/api/plugin-oauth/{instance}/callback?state={}&code=fixture",
        text(&started, "state")
    );
    assert_eq!(
        common::send(&app, request("GET", &callback, Value::Null, None))
            .await
            .status(),
        StatusCode::OK
    );
    let response = common::send(
        &app,
        request(
            "POST",
            &format!("{path}/finish"),
            json!({"state": started["state"]}),
            Some("fixture-owner"),
        ),
    )
    .await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "{}",
        common::read_text(response).await
    );
    let input = json!({"action": "read", "payload": {"resource": resource}});
    if rotates {
        let (first, second) = tokio::join!(
            panels::action(&s, "example", &instance, &input),
            panels::action(&s, "example", &instance, &input)
        );
        assert_eq!(first.unwrap()["notice"], "Authorized");
        assert_eq!(second.unwrap()["notice"], "Authorized");
        assert_eq!(exchanges.load(Ordering::SeqCst), 2);
    } else {
        // Each access token expires immediately. A second actual action must
        // reuse the confirmed nonrotating credential, not ask for consent again.
        for _ in 0..2 {
            let answer = panels::action(&s, "example", &instance, &input)
                .await
                .unwrap();
            assert_eq!(answer["notice"], "Authorized");
        }
        assert_eq!(exchanges.load(Ordering::SeqCst), 3);
    }
    server.abort();
}

#[tokio::test]
async fn disconnect_during_code_exchange_prevents_authorization_from_being_restored() {
    for personal in [false, true] {
        let (_root, s, instance, _resource, path, app) = configured_fixture(false, personal).await;
        let started = begin(&app, &path).await;
        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let entering = entered.clone();
        let releasing = release.clone();
        let provider = axum::Router::new().route("/token", axum::routing::post(move || {
        let entered = entering.clone(); let release = releasing.clone();
        async move {
            entered.notify_one(); release.notified().await;
            axum::Json(json!({"access_token": "revoked-fixture", "scope": "messages:read", "team": {"id": "T111"}, "authed_user": {"id": "U111"}}))
        }
    }));
        let (origin, server) = common::serve_locally(provider).await;
        s.plugins.route_http_to(Some(origin));
        let callback = format!(
            "/api/plugin-oauth/{instance}/callback?state={}&code=fixture",
            text(&started, "state")
        );
        assert_eq!(
            common::send(&app, request("GET", &callback, Value::Null, None))
                .await
                .status(),
            StatusCode::OK
        );
        let pending_app = app.clone();
        let finish = format!("{path}/finish");
        let pending = tokio::spawn(async move {
            common::send(
                &pending_app,
                request(
                    "POST",
                    &finish,
                    json!({"state": started["state"]}),
                    Some("fixture-owner"),
                ),
            )
            .await
        });
        tokio::time::timeout(std::time::Duration::from_secs(3), entered.notified())
            .await
            .unwrap();
        assert_eq!(
            common::send(
                &app,
                request("DELETE", &path, Value::Null, Some("fixture-owner"))
            )
            .await
            .status(),
            StatusCode::OK
        );
        release.notify_one();
        assert_eq!(pending.await.unwrap().status(), StatusCode::CONFLICT);
        let response = common::send(
            &app,
            request("GET", &path, Value::Null, Some("fixture-owner")),
        )
        .await;
        assert_eq!(common::read_json(response).await["status"], "disconnected");
        server.abort();
    }
}

#[tokio::test]
async fn pkce_nested_tokens_and_cumulative_scopes_keep_requested_and_granted_rights_distinct() {
    use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
    use sha2::{Digest, Sha256};
    let (_root, s, instance, resource, path, app) = fixture(true).await;
    let started = begin(&app, &path).await;
    let url = url::Url::parse(text(&started, "url")).unwrap();
    let query: HashMap<_, _> = url.query_pairs().into_owned().collect();
    assert_eq!(query["code_challenge_method"], "S256");
    let challenge = query["code_challenge"].clone();
    let provider = axum::Router::new().route("/token", axum::routing::post(move |headers: axum::http::HeaderMap, axum::Form(form): axum::Form<HashMap<String, String>>| {
        let challenge = challenge.clone();
        async move {
            assert!(!headers.contains_key("authorization"));
            assert!(!form.contains_key("client_secret"));
            assert_eq!(form["client_id"], "client-fixture");
            assert_eq!(URL_SAFE_NO_PAD.encode(Sha256::digest(form["code_verifier"].as_bytes())), challenge);
            axum::Json(json!({"ok": true, "team": {"id": "T111"}, "access_token": "unrequested-root-fixture",
                "authed_user": {"access_token": "nested-fixture", "refresh_token": "nested-refresh-fixture", "expires_in": 43200,
                    "scope": "messages:read,messages:write"}}))
        }
    })).route("/messages", axum::routing::get(|headers: axum::http::HeaderMap| async move {
        assert_eq!(headers["authorization"], "Bearer nested-fixture");
        axum::Json(json!({"ok": true}))
    }));
    let (origin, server) = common::serve_locally(provider).await;
    s.plugins.route_http_to(Some(origin));
    let callback = format!(
        "/api/plugin-oauth/{instance}/callback?state={}&code=fixture",
        text(&started, "state")
    );
    assert_eq!(
        common::send(&app, request("GET", &callback, Value::Null, None))
            .await
            .status(),
        StatusCode::OK
    );
    let response = common::send(
        &app,
        request(
            "POST",
            &format!("{path}/finish"),
            json!({"state": started["state"]}),
            Some("fixture-owner"),
        ),
    )
    .await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "{}",
        common::read_text(response).await
    );
    let value = common::read_json(response).await;
    assert_eq!(value["requestedScopes"], json!(["messages:read"]));
    assert_eq!(value["scopes"], json!(["messages:read", "messages:write"]));
    let result = panels::action(
        &s,
        "example",
        &instance,
        &json!({"action": "read", "payload": {"resource": resource}}),
    )
    .await
    .unwrap();
    assert_eq!(result["notice"], "Authorized");
    server.abort();
}

#[tokio::test]
async fn callbacks_are_single_use_and_never_confer_account_or_exchange_authority() {
    let (_root, _s, instance, _resource, path, app) = fixture(false).await;
    let started = begin(&app, &path).await;
    let finish = format!("{path}/finish");
    let state = json!({"state": started["state"]});
    let waiting = common::send(
        &app,
        request("POST", &finish, state.clone(), Some("fixture-owner")),
    )
    .await;
    assert_eq!(common::read_json(waiting).await["status"], "pending");
    let callback = format!("/api/plugin-oauth/{instance}/callback");
    let nonce = text(&started, "state");
    for query in [
        format!("state={nonce}&code=a&code=b"),
        format!("state={nonce}&code=a&error=denied"),
        format!("state={nonce}"),
    ] {
        assert_eq!(
            common::send(
                &app,
                request("GET", &format!("{callback}?{query}"), Value::Null, None)
            )
            .await
            .status(),
            StatusCode::BAD_REQUEST
        );
    }
    let wrong_instance = format!(
        "/api/plugin-oauth/{}/callback?state={nonce}&code=a",
        uuid::Uuid::new_v4()
    );
    assert_eq!(
        common::send(&app, request("GET", &wrong_instance, Value::Null, None))
            .await
            .status(),
        StatusCode::CONFLICT
    );
    let mut escaped = request("GET", "/api/settings", Value::Null, Some("fixture-owner"));
    escaped
        .extensions_mut()
        .get_mut::<InstallationIdentity>()
        .unwrap()
        .public_oauth = Some(callback.clone());
    assert_eq!(
        common::send(&app, escaped).await.status(),
        StatusCode::FORBIDDEN
    );
    let denied = format!("{callback}?state={nonce}&error=denied");
    assert_eq!(
        common::send(&app, request("GET", &denied, Value::Null, None))
            .await
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        common::send(&app, request("GET", &denied, Value::Null, None))
            .await
            .status(),
        StatusCode::CONFLICT
    );
    let finished = common::send(&app, request("POST", &finish, state, Some("fixture-owner"))).await;
    assert_eq!(common::read_json(finished).await["status"], "denied");
}

#[tokio::test]
async fn an_uncertain_refresh_result_is_not_retried_with_the_consumed_token() {
    let (_root, s, instance, resource, path, app) = fixture(false).await;
    let started = begin(&app, &path).await;
    let exchanges = Arc::new(AtomicUsize::new(0));
    let calls = exchanges.clone();
    let provider = axum::Router::new().route("/token", axum::routing::post(move || {
        let calls = calls.clone();
        async move {
            let call = calls.fetch_add(1, Ordering::SeqCst);
            if call == 0 {
                (StatusCode::OK, axum::Json(json!({"access_token": "expiring-fixture", "refresh_token": "one-use-refresh-fixture",
                    "expires_in": 1, "scope": "messages:read", "team": {"id": "T111"}})))
            } else {
                (StatusCode::BAD_GATEWAY, axum::Json(json!({"error": "must-not-appear-in-error-fixture"})))
            }
        }
    }));
    let (origin, server) = common::serve_locally(provider).await;
    s.plugins.route_http_to(Some(origin));
    let callback = format!(
        "/api/plugin-oauth/{instance}/callback?state={}&code=fixture",
        text(&started, "state")
    );
    assert_eq!(
        common::send(&app, request("GET", &callback, Value::Null, None))
            .await
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        common::send(
            &app,
            request(
                "POST",
                &format!("{path}/finish"),
                json!({"state": started["state"]}),
                Some("fixture-owner")
            )
        )
        .await
        .status(),
        StatusCode::OK
    );
    let input = json!({"action": "read", "payload": {"resource": resource}});
    for _ in 0..2 {
        let error = panels::action(&s, "example", &instance, &input)
            .await
            .unwrap_err();
        assert!(!error.message.contains("fixture"));
    }
    assert_eq!(exchanges.load(Ordering::SeqCst), 2);
    let response = common::send(
        &app,
        request("GET", &path, Value::Null, Some("fixture-owner")),
    )
    .await;
    assert_eq!(common::read_json(response).await["status"], "reconnect");
    server.abort();
}

#[tokio::test]
async fn revoking_access_during_oauth_refresh_prevents_the_following_resource_request() {
    for revoked in ["grant", "run"] {
        let (_root, s, instance, resource, path, app) = fixture(false).await;
        let started = begin(&app, &path).await;
        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let entering = entered.clone();
        let releasing = release.clone();
        let provider = axum::Router::new().route("/token", axum::routing::post(move |axum::Form(form): axum::Form<HashMap<String, String>>| {
        let entered = entering.clone(); let release = releasing.clone();
        async move {
            if form["grant_type"] == "refresh_token" {
                entered.notify_one(); release.notified().await;
                axum::Json(json!({"access_token": "new-fixture", "refresh_token": "next-fixture", "expires_in": 43200, "scope": "messages:read"}))
            } else {
                axum::Json(json!({"access_token": "old-fixture", "refresh_token": "one-use-fixture", "expires_in": 1, "scope": "messages:read", "team": {"id": "T111"}}))
            }
        }
    }));
        let reads = Arc::new(AtomicUsize::new(0));
        let provider_reads = reads.clone();
        let provider = provider.route(
            "/messages",
            axum::routing::get(move || {
                provider_reads.fetch_add(1, Ordering::SeqCst);
                async { axum::Json(json!({"ok": true})) }
            }),
        );
        let (origin, server) = common::serve_locally(provider).await;
        s.plugins.route_http_to(Some(origin));
        let callback = format!(
            "/api/plugin-oauth/{instance}/callback?state={}&code=fixture",
            text(&started, "state")
        );
        assert_eq!(
            common::send(&app, request("GET", &callback, Value::Null, None))
                .await
                .status(),
            StatusCode::OK
        );
        assert_eq!(
            common::send(
                &app,
                request(
                    "POST",
                    &format!("{path}/finish"),
                    json!({"state": started["state"]}),
                    Some("fixture-owner")
                )
            )
            .await
            .status(),
            StatusCode::OK
        );
        let grants = format!(
            "/api/plugins/example/connections/{instance}/resources/workspace/{resource}/grants"
        );
        let grant = common::send(
            &app,
            request(
                "POST",
                &grants,
                json!({"agentId": MAIN_AGENT_ID, "profile": "read"}),
                Some("fixture-owner"),
            ),
        )
        .await;
        assert_eq!(grant.status(), StatusCode::OK);
        let grant = common::read_json(grant).await;
        let task = s
        .task(
            json!({"name": "Read messages", "prompt": "Read messages", "agentId": MAIN_AGENT_ID}),
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
        let config = s.mcps.run_configuration(&s, &run).await.unwrap();
        let bearer = text(&config["env"], "CAIRN_MCP_RUN_TOKEN").to_owned();
        let calling = s.clone();
        let mut pending = tokio::spawn(async move {
            agents::call_tool(
                &calling,
                &bearer,
                "example_read",
                &json!({"resource": resource}),
            )
            .await
        });
        tokio::select! {
            () = entered.notified() => {},
            result = &mut pending => panic!("The call ended before reaching the token service: {result:?}"),
            () = tokio::time::sleep(std::time::Duration::from_secs(3)) => panic!("Token service was not called"),
        }
        if revoked == "grant" {
            let revoke = format!("{grants}/{}", text(&grant, "id"));
            assert_eq!(
                common::send(
                    &app,
                    request("DELETE", &revoke, Value::Null, Some("fixture-owner"))
                )
                .await
                .status(),
                StatusCode::OK
            );
        } else {
            s.mcps.revoke_run(&s, text(&run, "id")).await.unwrap();
        }
        release.notify_one();
        let result = pending.await.unwrap();
        assert_eq!(
            reads.load(Ordering::SeqCst),
            0,
            "sent a new resource request after {revoked} access was revoked"
        );
        assert!(result.is_err());
        server.abort();
    }
}

#[tokio::test]
async fn missing_or_invalid_provider_identity_cannot_create_a_resource_authorization() {
    let (_root, s, instance, _resource, path, app) = fixture(false).await;
    let provider = axum::Router::new().route("/token", axum::routing::post(|axum::Form(form): axum::Form<HashMap<String, String>>| async move {
        axum::Json(json!({"access_token": "invalid-identity-fixture", "scope": "messages:read",
            "team": if form["code"] == "invalid" { json!({"id": "not-a-workspace"}) } else { Value::Null }}))
    }));
    let (origin, server) = common::serve_locally(provider).await;
    s.plugins.route_http_to(Some(origin));
    for code in ["invalid", "missing"] {
        let started = begin(&app, &path).await;
        let callback = format!(
            "/api/plugin-oauth/{instance}/callback?state={}&code={code}",
            text(&started, "state")
        );
        assert_eq!(
            common::send(&app, request("GET", &callback, Value::Null, None))
                .await
                .status(),
            StatusCode::OK
        );
        let result = common::send(
            &app,
            request(
                "POST",
                &format!("{path}/finish"),
                json!({"state": started["state"]}),
                Some("fixture-owner"),
            ),
        )
        .await;
        assert_eq!(
            result.status(),
            StatusCode::BAD_GATEWAY,
            "accepted {code} identity"
        );
        let status = common::send(
            &app,
            request("GET", &path, Value::Null, Some("fixture-owner")),
        )
        .await;
        assert_eq!(common::read_json(status).await["status"], "disconnected");
    }
    server.abort();
}

fn member_request(method: &str, path: &str, body: Value, account: &str) -> axum::extract::Request {
    let mut request = request(method, path, body, None);
    request
        .extensions_mut()
        .insert(InstallationIdentity::trusted(
            InstallationRole::Member,
            account,
        ));
    request
}

#[tokio::test]
async fn authorization_capacity_preserves_connections_and_allows_idempotent_disconnect() {
    let (_root, s, instance, resource, _path, app) = configured_fixture(false, true).await;
    let mut app = app;
    let original = registry::active(&s, "example").await.unwrap();
    let mut manifest = serde_json::to_value(&original.manifest).unwrap();
    manifest["version"] = json!("2");
    let provider = manifest["oauth"][0].clone();
    let names: Vec<_> = (0..8).map(|index| format!("personal-{index}")).collect();
    manifest["oauth"] = json!(
        names
            .iter()
            .map(|name| {
                let mut declared = provider.clone();
                declared["name"] = json!(name);
                declared
            })
            .collect::<Vec<_>>()
    );
    let package = registry::install(
        &s,
        &json!({
            "manifest": manifest,
            "source": original.source.main,
        }),
    )
    .await
    .unwrap();
    registry::approve(&s, "example", "2", text(&package, "digest"))
        .await
        .unwrap();
    let upstream = axum::Router::new().route(
        "/token",
        axum::routing::post(|| async {
            axum::Json(json!({
                "access_token": "capacity-credential-fixture",
                "scope": "messages:read",
                "team": { "id": "T111" },
                "authed_user": { "id": "U111" },
            }))
        }),
    );
    let (origin, server) = common::serve_locally(upstream).await;
    s.plugins.route_http_to(Some(origin));
    let prefix = format!("/api/plugins/example/connections/{instance}/resources/workspace");
    let mut connected = Vec::new();
    // Each personal authorization retains a credential and its revocation state.
    // Exercise the production capacity through owner HTTP, without reading storage.
    for index in 0..128 {
        // Recreate the HTTP server between batches. OAuth capacity must survive
        // restarts independently of the server's per-minute request limit.
        if index > 0 && index % 16 == 0 {
            app = cairn_installation::http::router(s.clone()).await.unwrap();
        }
        let resource = if index == 0 {
            resource.clone()
        } else {
            let created = common::send(
                &app,
                request(
                    "POST",
                    &prefix,
                    json!({
                        "name": format!("Workspace {index}"),
                        "settings": { "externalId": "T111" },
                    }),
                    Some("fixture-owner"),
                ),
            )
            .await;
            assert_eq!(created.status(), StatusCode::OK);
            text(&common::read_json(created).await, "id").to_owned()
        };
        for name in &names {
            let path = format!("{prefix}/{resource}/oauth/{name}");
            let started = begin(&app, &path).await;
            let callback = format!(
                "/api/plugin-oauth/{instance}/callback?state={}&code=fixture",
                text(&started, "state")
            );
            assert_eq!(
                common::send(&app, request("GET", &callback, Value::Null, None))
                    .await
                    .status(),
                StatusCode::OK
            );
            let finished = common::send(
                &app,
                request(
                    "POST",
                    &format!("{path}/finish"),
                    json!({
                        "state": started["state"],
                    }),
                    Some("fixture-owner"),
                ),
            )
            .await;
            assert_eq!(
                finished.status(),
                StatusCode::OK,
                "{}",
                common::read_text(finished).await
            );
            connected.push(path);
        }
    }
    let unused = common::send(
        &app,
        request(
            "POST",
            &prefix,
            json!({
                "name": "Unused workspace",
                "settings": { "externalId": "T111" },
            }),
            Some("fixture-owner"),
        ),
    )
    .await;
    assert_eq!(unused.status(), StatusCode::OK);
    let unused = format!(
        "{prefix}/{}/oauth/personal-0",
        text(&common::read_json(unused).await, "id")
    );
    let full = common::send(
        &app,
        request(
            "POST",
            &unused,
            json!({
                "scopes": ["messages:read"],
            }),
            Some("fixture-owner"),
        ),
    )
    .await;
    assert_eq!(full.status(), StatusCode::TOO_MANY_REQUESTS);
    let unchanged = common::read_json(
        common::send(
            &app,
            request("GET", &connected[0], Value::Null, Some("fixture-owner")),
        )
        .await,
    )
    .await;
    assert_eq!(unchanged["status"], "authorized");

    let disconnected = common::send(
        &app,
        request("DELETE", &unused, Value::Null, Some("fixture-owner")),
    )
    .await;
    assert_eq!(
        disconnected.status(),
        StatusCode::OK,
        "an already disconnected person could not disconnect at capacity"
    );
    for path in connected.iter().take(2) {
        let disconnected = common::send(
            &app,
            request("DELETE", path, Value::Null, Some("fixture-owner")),
        )
        .await;
        assert_eq!(disconnected.status(), StatusCode::OK);
        let status = common::read_json(
            common::send(
                &app,
                request("GET", path, Value::Null, Some("fixture-owner")),
            )
            .await,
        )
        .await;
        assert_eq!(status["status"], "disconnected");
    }
    let started = begin(&app, &unused).await;
    let callback = format!(
        "/api/plugin-oauth/{instance}/callback?state={}&code=fixture",
        text(&started, "state")
    );
    assert_eq!(
        common::send(&app, request("GET", &callback, Value::Null, None))
            .await
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        common::send(
            &app,
            request(
                "POST",
                &format!("{unused}/finish"),
                json!({
                    "state": started["state"],
                }),
                Some("fixture-owner")
            )
        )
        .await
        .status(),
        StatusCode::OK
    );
    server.abort();
}

#[tokio::test]
async fn pending_consent_is_bounded_and_a_completed_exchange_releases_its_slot() {
    let (_root, s, instance, _resource, path, app) = configured_fixture(false, true).await;
    let mut first = Value::Null;
    for index in 0..128 {
        let started = begin(&app, &path).await;
        if index == 0 {
            first = started;
        }
    }
    let full = common::send(
        &app,
        request(
            "POST",
            &path,
            json!({"scopes": ["messages:read"]}),
            Some("fixture-owner"),
        ),
    )
    .await;
    assert_eq!(
        full.status(),
        StatusCode::TOO_MANY_REQUESTS,
        "pending consent has no capacity bound"
    );
    let provider = axum::Router::new().route(
        "/token",
        axum::routing::post(|| async {
            axum::Json(
                json!({"access_token": "bounded-consent-fixture", "scope": "messages:read",
            "team": {"id": "T111"}, "authed_user": {"id": "U111"}}),
            )
        }),
    );
    let (origin, server) = common::serve_locally(provider).await;
    s.plugins.route_http_to(Some(origin));
    let callback = format!(
        "/api/plugin-oauth/{instance}/callback?state={}&code=fixture",
        text(&first, "state")
    );
    assert_eq!(
        common::send(&app, request("GET", &callback, Value::Null, None))
            .await
            .status(),
        StatusCode::OK
    );
    let completed = common::send(
        &app,
        request(
            "POST",
            &format!("{path}/finish"),
            json!({"state": first["state"]}),
            Some("fixture-owner"),
        ),
    )
    .await;
    assert_eq!(completed.status(), StatusCode::OK);
    begin(&app, &path).await;
    let status = common::read_json(
        common::send(
            &app,
            request("GET", &path, Value::Null, Some("fixture-owner")),
        )
        .await,
    )
    .await;
    assert_eq!(status["status"], "authorized");
    server.abort();
}

#[tokio::test]
async fn personal_oauth_isolated_by_verified_account_and_provider_subject() {
    let (_root, s, instance, resource, path, app) = configured_fixture(false, true).await;
    let policy = Arc::new(std::sync::Mutex::new(json!({
        "owner": {"account_id": "fixture-owner", "access_id": "fixture-owner"},
        "members": [
            {"account_id": "alice", "access_id": "alice-membership-1"},
            {"account_id": "bob", "access_id": "bob-membership-1"},
            {"account_id": "charlie", "access_id": "charlie-membership-1"}
        ]
    })));
    let policy_ref = policy.clone();
    let authority = axum::Router::new().route(
        "/api/relay/00000000-0000-4000-8000-000000000055/task-authors",
        axum::routing::get(move || {
            let policy = policy_ref.lock().unwrap().clone();
            async move { axum::Json(policy) }
        }),
    );
    let (authority_origin, authority_server) = common::serve_locally(authority).await;
    let identity_path = s.config.data_dir.join("installation-relay/identity.json");
    let mut identity: Value =
        serde_json::from_slice(&std::fs::read(&identity_path).unwrap()).unwrap();
    identity["origin"] = json!(authority_origin);
    std::fs::write(&identity_path, serde_json::to_vec(&identity).unwrap()).unwrap();

    let discovery = "/api/plugins/personal-authorizations";
    let discovered =
        common::send(&app, member_request("GET", discovery, Value::Null, "alice")).await;
    assert_eq!(
        discovered.status(),
        StatusCode::OK,
        "{}",
        common::read_text(discovered).await
    );
    let discovered = common::read_json(discovered).await;
    assert_eq!(discovered["items"].as_array().unwrap().len(), 1);
    assert_eq!(discovered["items"][0]["resourceName"], "Engineering");
    assert_eq!(
        discovered["items"][0]["authorization"]["status"],
        "disconnected"
    );
    for private in [
        "client-secret-fixture",
        "client-fixture",
        "externalId",
        "settings",
        "owner_access",
        "alice-membership-1",
    ] {
        assert!(
            !discovered.to_string().contains(private),
            "discovery exposed {private}"
        );
    }
    assert_eq!(
        common::send(
            &app,
            member_request("GET", "/api/plugins", Value::Null, "alice")
        )
        .await
        .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        common::send(&app, member_request("POST", discovery, json!({}), "alice"))
            .await
            .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        common::send(
            &app,
            member_request("GET", discovery, Value::Null, "stranger")
        )
        .await
        .status(),
        StatusCode::FORBIDDEN
    );

    let provider = axum::Router::new().route(
        "/token",
        axum::routing::post(
            |axum::Form(form): axum::Form<HashMap<String, String>>| async move {
                let (subject, token) = match form["code"].as_str() {
                    "alice-code" | "charlie-as-alice" => ("U111", "personal-alice-fixture"),
                    "bob-code" => ("U222", "personal-bob-fixture"),
                    code => panic!("Unexpected fixture code: {code}"),
                };
                axum::Json(json!({"access_token": token, "scope": "messages:read",
                "team": {"id": "T111"}, "authed_user": {"id": subject}}))
            },
        ),
    );
    let hold_read = Arc::new(AtomicBool::new(false));
    let read_entered = Arc::new(tokio::sync::Notify::new());
    let release_read = Arc::new(tokio::sync::Notify::new());
    let held = hold_read.clone();
    let entered = read_entered.clone();
    let release = release_read.clone();
    let provider = provider.route(
        "/messages",
        axum::routing::get(move |headers: axum::http::HeaderMap| {
            let held = held.clone();
            let entered = entered.clone();
            let release = release.clone();
            async move {
                if held.swap(false, Ordering::SeqCst) {
                    entered.notify_one();
                    release.notified().await;
                }
                let subject = match headers["authorization"].to_str().unwrap() {
                    "Bearer personal-alice-fixture" => "U111",
                    "Bearer personal-bob-fixture" => "U222",
                    _ => panic!("unexpected personal authorization"),
                };
                axum::Json(json!({"subject": subject}))
            }
        }),
    );
    let (origin, server) = common::serve_locally(provider).await;
    s.plugins.route_http_to(Some(origin));
    for (account, code, subject) in [("alice", "alice-code", "U111"), ("bob", "bob-code", "U222")] {
        let started = common::send(
            &app,
            member_request("POST", &path, json!({"scopes": ["messages:read"]}), account),
        )
        .await;
        assert_eq!(
            started.status(),
            StatusCode::OK,
            "{}",
            common::read_text(started).await
        );
        let started = common::read_json(started).await;
        let state = json!({"state": started["state"]});
        let finish = format!("{path}/finish");
        let thief = if account == "alice" { "bob" } else { "alice" };
        assert_eq!(
            common::send(&app, member_request("POST", &finish, state.clone(), thief))
                .await
                .status(),
            StatusCode::FORBIDDEN
        );
        let callback = format!(
            "/api/plugin-oauth/{instance}/callback?state={}&code={code}",
            text(&started, "state")
        );
        assert_eq!(
            common::send(&app, request("GET", &callback, Value::Null, None))
                .await
                .status(),
            StatusCode::OK
        );
        let finished = common::send(&app, member_request("POST", &finish, state, account)).await;
        assert_eq!(
            finished.status(),
            StatusCode::OK,
            "{}",
            common::read_text(finished).await
        );
        let finished = common::read_json(finished).await;
        assert_eq!(finished["status"], "authorized");
        assert_eq!(finished["subject"], subject);
        assert!(!finished.to_string().contains("personal-"));
    }
    for (account, subject) in [("alice", "U111"), ("bob", "U222")] {
        let status = common::read_json(
            common::send(&app, member_request("GET", &path, Value::Null, account)).await,
        )
        .await;
        assert_eq!(status["subject"], subject);
        let own = common::read_json(
            common::send(&app, member_request("GET", discovery, Value::Null, account)).await,
        )
        .await;
        assert_eq!(own["items"][0]["authorization"]["subject"], subject);
        let other = if account == "alice" { "U222" } else { "U111" };
        assert!(!own.to_string().contains(other));
        assert!(!own.to_string().contains("personal-"));
    }
    let duplicate = common::send(
        &app,
        member_request(
            "POST",
            &path,
            json!({"scopes": ["messages:read"]}),
            "charlie",
        ),
    )
    .await;
    assert_eq!(duplicate.status(), StatusCode::OK);
    let duplicate = common::read_json(duplicate).await;
    let callback = format!(
        "/api/plugin-oauth/{instance}/callback?state={}&code=charlie-as-alice",
        text(&duplicate, "state")
    );
    assert_eq!(
        common::send(&app, request("GET", &callback, Value::Null, None))
            .await
            .status(),
        StatusCode::OK
    );
    let refused = common::send(
        &app,
        member_request(
            "POST",
            &format!("{path}/finish"),
            json!({"state": duplicate["state"]}),
            "charlie",
        ),
    )
    .await;
    assert_eq!(
        refused.status(),
        StatusCode::CONFLICT,
        "two active accounts acquired the same external identity"
    );
    let owner_status = common::read_json(
        common::send(
            &app,
            request("GET", &path, Value::Null, Some("fixture-owner")),
        )
        .await,
    )
    .await;
    assert_eq!(owner_status["status"], "disconnected");
    let grants = format!(
        "/api/plugins/example/connections/{instance}/resources/workspace/{resource}/grants"
    );
    let granted = common::send(
        &app,
        request(
            "POST",
            &grants,
            json!({"agentId": MAIN_AGENT_ID, "profile": "read"}),
            Some("fixture-owner"),
        ),
    )
    .await;
    assert_eq!(granted.status(), StatusCode::OK);
    for (account, subject) in [("alice", "U111"), ("bob", "U222")] {
        let chat = s
            .chat_create(json!({"agentId": MAIN_AGENT_ID}))
            .await
            .unwrap();
        let message_path = format!("/api/chats/{}/messages", text(&chat, "id"));
        let message = json!({"id": cairn_installation::config::id(), "text": "Read with my own authorization"});
        assert_eq!(
            common::send(
                &app,
                member_request("POST", &message_path, message.clone(), account)
            )
            .await
            .status(),
            StatusCode::OK
        );
        s.chat_tick(&HashSet::new()).await.unwrap();
        let detail = s.chat_detail(text(&chat, "id")).await.unwrap();
        let run = s
            .store
            .patch_run(
                text(&detail["run"], "id"),
                json!({"status": "running", "isolated": true}),
            )
            .await
            .unwrap();
        let configuration = s.mcps.run_configuration(&s, &run).await.unwrap();
        let bearer = text(&configuration["env"], "CAIRN_MCP_RUN_TOKEN");
        let result = agents::call_tool(&s, bearer, "example_read", &json!({"resource": resource}))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(result["subject"], subject);
        let other = if account == "alice" { "bob" } else { "alice" };
        for stopped in ["cancelled", "interrupted"] {
            s.store
                .patch_run(
                    text(&run, "id"),
                    json!({
                        "status": stopped, "recoveryPending": false,
                    }),
                )
                .await
                .unwrap();
            let steering = cairn_installation::config::id();
            let response = common::send(&app, member_request("POST", &message_path,
                json!({"id": steering, "text": "Use my instruction on the resumed turn", "mode": "steer"}), other)).await;
            if response.status() == StatusCode::OK {
                // A native resume retains this turn's original requester. Publish
                // the queued steering through the actual native chat tick.
                s.store
                    .patch_run(text(&run, "id"), json!({"status": "running"}))
                    .await
                    .unwrap();
                s.chat_tick(&HashSet::new()).await.unwrap();
                let input = s
                    .config
                    .data_dir
                    .join("runs")
                    .join(text(&run, "id"))
                    .join("chat-input/messages.json");
                let published: Value =
                    serde_json::from_slice(&std::fs::read(input).unwrap()).unwrap();
                let retained =
                    agents::call_tool(&s, bearer, "example_read", &json!({"resource": resource}))
                        .await
                        .unwrap()
                        .unwrap();
                assert_eq!(retained["subject"], subject);
                assert!(
                    published
                        .as_array()
                        .unwrap()
                        .iter()
                        .all(|m| m["id"] != steering),
                    "Another author's steering entered a resumed turn that retained {account}'s OAuth: {published}"
                );
            }
            assert_eq!(
                response.status(),
                StatusCode::CONFLICT,
                "A stopped execution accepted another requester's steering"
            );
            s.store
                .patch_run(text(&run, "id"), json!({"status": "running"}))
                .await
                .unwrap();
        }
        // A persisted input from a formerly accepted stopped-turn admission
        // must also be refused at consumption, independently of the new gate.
        let stale = cairn_installation::config::id();
        let queued = common::send(
            &app,
            member_request(
                "POST",
                &message_path,
                json!({"id": stale, "text": "Earlier foreign steering"}),
                other,
            ),
        )
        .await;
        assert_eq!(queued.status(), StatusCode::OK);
        let stale_chat = text(&chat, "id").to_owned();
        let stale_id = stale.clone();
        s.store
            .transaction(move |db| {
                let mut pending = db
                    .messages(&stale_chat)?
                    .into_iter()
                    .find(|m| m["id"] == stale_id)
                    .unwrap();
                pending["mode"] = json!("steer");
                db.put_message(&pending)?;
                Ok(())
            })
            .await
            .unwrap();
        s.chat_tick(&HashSet::new()).await.unwrap();
        let input = s
            .config
            .data_dir
            .join("runs")
            .join(text(&run, "id"))
            .join("chat-input/messages.json");
        let published: Value = serde_json::from_slice(&std::fs::read(input).unwrap()).unwrap();
        assert!(
            published
                .as_array()
                .unwrap()
                .iter()
                .all(|m| m["id"] != stale),
            "A queued input inherited another requester's execution authority"
        );
        assert!(
            s.chat_detail(text(&chat, "id")).await.unwrap()["messages"]
                .as_array()
                .unwrap()
                .iter()
                .any(|m| m["id"] == stale && m["status"] == "cancelled")
        );
        let retry = common::send(
            &app,
            member_request("POST", &message_path, message.clone(), other),
        )
        .await;
        assert_eq!(
            retry.status(),
            StatusCode::CONFLICT,
            "reusing a message ID transferred its requester"
        );
        let steer = common::send(&app, member_request("POST", &message_path,
            json!({"id": cairn_installation::config::id(), "text": "act as me", "mode": "steer"}), other)).await;
        assert_eq!(
            steer.status(),
            StatusCode::CONFLICT,
            "another participant inherited the running turn's personal identity"
        );
        let question = "a".repeat(64);
        s.question_receive(
            text(&run, "id"),
            json!({
                "id": question, "blocking": false,
                "fields": [{"id": "choice", "title": "Continue?", "options": []}]
            }),
        )
        .await
        .unwrap();
        let answer_path = format!(
            "/api/chats/{}/questions/{question}/answer",
            text(&chat, "id")
        );
        let answer =
            json!({"id": cairn_installation::config::id(), "answers": {"choice": ["Yes"]}});
        let foreign_answer = common::send(
            &app,
            member_request("POST", &answer_path, answer.clone(), other),
        )
        .await;
        assert_eq!(
            foreign_answer.status(),
            StatusCode::CONFLICT,
            "a question answer bypassed cross-author steering checks"
        );
        assert_eq!(
            common::send(
                &app,
                member_request("POST", &answer_path, answer.clone(), account)
            )
            .await
            .status(),
            StatusCode::OK
        );
        s.chat_edit(text(&chat, "id"), text(&answer, "id"), None)
            .await
            .unwrap();
        if account == "alice" {
            hold_read.store(true, Ordering::SeqCst);
            let pending_service = s.clone();
            let pending_bearer = bearer.to_owned();
            let pending_input = json!({"resource": resource});
            let pending = tokio::spawn(async move {
                agents::call_tool(
                    &pending_service,
                    &pending_bearer,
                    "example_read",
                    &pending_input,
                )
                .await
            });
            tokio::time::timeout(std::time::Duration::from_secs(3), read_entered.notified())
                .await
                .unwrap();
            let controlled = common::send(
                &app,
                request("POST", &answer_path, answer.clone(), Some("fixture-owner")),
            )
            .await;
            assert_eq!(
                controlled.status(),
                StatusCode::OK,
                "the owner lost conversation control"
            );
            release_read.notify_one();
            assert!(
                pending.await.unwrap().is_err(),
                "a result from the suspended personal authority escaped to the plugin"
            );
            assert!(
                agents::call_tool(&s, bearer, "example_read", &json!({"resource": resource}))
                    .await
                    .is_err(),
                "owner control inherited the original requester's personal authorization"
            );
            let next_question = "c".repeat(64);
            s.question_receive(
                text(&run, "id"),
                json!({
                    "id": next_question,
                    "blocking": false,
                    "fields": [{"id": "choice", "title": "Continue again?", "options": []}],
                }),
            )
            .await
            .unwrap();
            let next_path = format!(
                "/api/chats/{}/questions/{next_question}/answer",
                text(&chat, "id")
            );
            let next_answer = json!({
                "id": cairn_installation::config::id(),
                "answers": {"choice": ["Continue"]},
            });
            assert_eq!(
                common::send(
                    &app,
                    member_request("POST", &next_path, next_answer.clone(), other)
                )
                .await
                .status(),
                StatusCode::CONFLICT
            );
            let continued = common::send(
                &app,
                member_request("POST", &next_path, next_answer.clone(), account),
            )
            .await;
            assert_eq!(
                continued.status(),
                StatusCode::OK,
                "The original author lost question control after owner intervention"
            );
            let anonymous = s
                .chat_send(
                    text(&chat, "id"),
                    json!({
                        "id": cairn_installation::config::id(),
                        "text": "Unattributed control",
                        "mode": "steer",
                    }),
                )
                .await;
            assert!(
                anonymous.is_err(),
                "Suspending OAuth authorized an unrelated anonymous input"
            );
            s.chat_tick(&HashSet::new()).await.unwrap();
            let file = s
                .config
                .data_dir
                .join("runs")
                .join(text(&run, "id"))
                .join("chat-input/messages.json");
            let published: Value = serde_json::from_slice(&std::fs::read(file).unwrap()).unwrap();
            for id in [text(&answer, "id"), text(&next_answer, "id")] {
                assert!(
                    published.as_array().unwrap().iter().any(|m| m["id"] == id),
                    "An authorized owner/original-author answer was not delivered"
                );
            }
            assert!(
                agents::call_tool(&s, bearer, "example_read", &json!({"resource": resource}))
                    .await
                    .is_err(),
                "Original-author control restored suspended personal OAuth"
            );
            // Settle Alice's turn through ordinary delivery acknowledgements,
            // then let Bob submit his own turn in the same conversation/run.
            for delivered in [
                text(&message, "id"),
                text(&answer, "id"),
                text(&next_answer, "id"),
            ] {
                s.chat_acknowledge(text(&run, "id"), delivered)
                    .await
                    .unwrap();
            }
            s.store
                .set(
                    &format!("run-checkpoint:{}", text(&run, "id")),
                    json!({"completed": true, "launched": true}),
                    None,
                )
                .await
                .unwrap();
            s.store
                .patch_run(text(&run, "id"), json!({"status": "succeeded"}))
                .await
                .unwrap();
            let bob_message = cairn_installation::config::id();
            assert_eq!(
                common::send(
                    &app,
                    member_request(
                        "POST",
                        &message_path,
                        json!({
                            "id": bob_message,
                            "text": "A separate request with Bob's authorization",
                        }),
                        "bob",
                    ),
                )
                .await
                .status(),
                StatusCode::OK
            );
            s.chat_tick(&HashSet::new()).await.unwrap();
            let detail = s.chat_detail(text(&chat, "id")).await.unwrap();
            assert_eq!(detail["run"]["id"], run["id"]);
            assert_eq!(detail["run"]["chatExecution"]["messageId"], bob_message);
            let bob_run = s
                .store
                .patch_run(
                    text(&run, "id"),
                    json!({"status": "running", "isolated": true}),
                )
                .await
                .unwrap();
            let configuration = s.mcps.run_configuration(&s, &bob_run).await.unwrap();
            let bob_bearer = text(&configuration["env"], "CAIRN_MCP_RUN_TOKEN");
            let input = json!({"resource": resource});
            assert_eq!(
                agents::call_tool(&s, bob_bearer, "example_read", &input)
                    .await
                    .unwrap()
                    .unwrap()["subject"],
                "U222"
            );
            let retried = common::send(
                &app,
                request("POST", &answer_path, answer.clone(), Some("fixture-owner")),
            )
            .await;
            assert_eq!(retried.status(), StatusCode::OK);
            assert_eq!(
                agents::call_tool(&s, bob_bearer, "example_read", &input)
                    .await
                    .expect("Retrying an old owner answer suspended the later requester's OAuth")
                    .unwrap()["subject"],
                "U222"
            );
            let foreign_retry = common::send(
                &app,
                member_request("POST", &answer_path, answer.clone(), "bob"),
            )
            .await;
            assert_eq!(foreign_retry.status(), StatusCode::CONFLICT);
            // Providers can reuse question IDs in a later turn. The old answer
            // message is not a new intervention in that repeated question.
            s.question_receive(
                text(&run, "id"),
                json!({
                    "id": question,
                    "blocking": false,
                    "fields": [{"id": "choice", "title": "Continue?", "options": []}],
                }),
            )
            .await
            .unwrap();
            let reused = common::send(
                &app,
                request("POST", &answer_path, answer.clone(), Some("fixture-owner")),
            )
            .await;
            assert_eq!(reused.status(), StatusCode::CONFLICT);
            assert_eq!(
                agents::call_tool(&s, bob_bearer, "example_read", &input)
                    .await
                    .unwrap()
                    .unwrap()["subject"],
                "U222"
            );
        }
        if account == "bob" {
            let own_steer = common::send(&app, member_request("POST", &message_path,
                json!({"id": cairn_installation::config::id(), "text": "continue", "mode": "steer"}), account)).await;
            assert_eq!(own_steer.status(), StatusCode::OK);
        }
    }
    let action = panels::action(
        &s,
        "example",
        &instance,
        &json!({"action": "read", "payload": {"resource": resource}}),
    )
    .await;
    assert!(
        action.is_err(),
        "an owner panel inherited a personal authorization"
    );
    let revoked = common::send(&app, member_request("DELETE", &path, Value::Null, "alice")).await;
    assert_eq!(revoked.status(), StatusCode::OK);
    let bob = common::read_json(
        common::send(&app, member_request("GET", &path, Value::Null, "bob")).await,
    )
    .await;
    assert_eq!(bob["status"], "authorized");
    assert_eq!(bob["subject"], "U222");
    policy.lock().unwrap()["members"] = json!([]);
    assert_eq!(
        common::send(&app, member_request("GET", discovery, Value::Null, "bob"))
            .await
            .status(),
        StatusCode::FORBIDDEN
    );
    let removed = common::send(&app, member_request("GET", &path, Value::Null, "bob")).await;
    assert_eq!(removed.status(), StatusCode::FORBIDDEN);
    policy.lock().unwrap()["members"] =
        json!([{ "account_id": "bob", "access_id": "bob-membership-2" }]);
    let rejoined = common::read_json(
        common::send(&app, member_request("GET", &path, Value::Null, "bob")).await,
    )
    .await;
    assert_eq!(
        rejoined["status"], "reconnect",
        "rejoining restored the revoked membership's token"
    );
    policy.lock().unwrap()["members"]
        .as_array_mut()
        .unwrap()
        .push(json!({"account_id": "charlie", "access_id": "charlie-membership-1"}));
    let connection = connections::get(&s, "example", &instance).await.unwrap();
    connections::save(
        &s,
        "example",
        &json!({"name": connection.name, "enabled": false, "settings": connection.settings}),
        Some(&instance),
    )
    .await
    .unwrap();
    let paused = common::read_json(
        common::send(&app, member_request("GET", discovery, Value::Null, "bob")).await,
    )
    .await;
    assert_eq!(
        paused["items"].as_array().unwrap().len(),
        1,
        "pausing hid the person's existing authorization"
    );
    assert_eq!(paused["items"][0]["enabled"], false);
    let unavailable = common::read_json(
        common::send(
            &app,
            member_request("GET", discovery, Value::Null, "charlie"),
        )
        .await,
    )
    .await;
    assert!(unavailable["items"].as_array().unwrap().is_empty());
    assert_eq!(
        common::send(&app, member_request("DELETE", &path, Value::Null, "bob"))
            .await
            .status(),
        StatusCode::OK
    );
    let disconnected = common::read_json(
        common::send(&app, member_request("GET", discovery, Value::Null, "bob")).await,
    )
    .await;
    assert!(disconnected["items"].as_array().unwrap().is_empty());
    server.abort();
    authority_server.abort();
}

#[tokio::test]
async fn disconnect_during_the_final_authority_check_prevents_a_personal_request() {
    let (_root, s, instance, resource, path, app) = configured_fixture(false, true).await;
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_ref = calls.clone();
    let provider = axum::Router::new()
        .route(
            "/token",
            axum::routing::post(|| async {
                axum::Json(
                    json!({"access_token": "personal-owner-fixture", "scope": "messages:read",
                "team": {"id": "T111"}, "authed_user": {"id": "U111"}}),
                )
            }),
        )
        .route(
            "/messages",
            axum::routing::get(move |headers: axum::http::HeaderMap| {
                let calls = calls_ref.clone();
                async move {
                    assert_eq!(headers["authorization"], "Bearer personal-owner-fixture");
                    calls.fetch_add(1, Ordering::SeqCst);
                    axum::Json(json!({"actor": "U111"}))
                }
            }),
        );
    let (origin, server) = common::serve_locally(provider).await;
    s.plugins.route_http_to(Some(origin));
    let started = begin(&app, &path).await;
    let callback = format!(
        "/api/plugin-oauth/{instance}/callback?state={}&code=owner-code",
        text(&started, "state")
    );
    assert_eq!(
        common::send(&app, request("GET", &callback, Value::Null, None))
            .await
            .status(),
        StatusCode::OK
    );
    let finished = common::send(
        &app,
        request(
            "POST",
            &format!("{path}/finish"),
            json!({"state": started["state"]}),
            Some("fixture-owner"),
        ),
    )
    .await;
    assert_eq!(finished.status(), StatusCode::OK);
    let grants = format!(
        "/api/plugins/example/connections/{instance}/resources/workspace/{resource}/grants"
    );
    let granted = common::send(
        &app,
        request(
            "POST",
            &grants,
            json!({"agentId": MAIN_AGENT_ID, "profile": "read"}),
            Some("fixture-owner"),
        ),
    )
    .await;
    assert_eq!(granted.status(), StatusCode::OK);
    let chat = s
        .chat_create(json!({"agentId": MAIN_AGENT_ID}))
        .await
        .unwrap();
    let sent = common::send(
        &app,
        request(
            "POST",
            &format!("/api/chats/{}/messages", text(&chat, "id")),
            json!({"id": cairn_installation::config::id(), "text": "Read messages"}),
            Some("fixture-owner"),
        ),
    )
    .await;
    assert_eq!(sent.status(), StatusCode::OK);
    s.chat_tick(&HashSet::new()).await.unwrap();
    let detail = s.chat_detail(text(&chat, "id")).await.unwrap();
    let run = s
        .store
        .patch_run(
            text(&detail["run"], "id"),
            json!({"status": "running", "isolated": true}),
        )
        .await
        .unwrap();
    let configuration = s.mcps.run_configuration(&s, &run).await.unwrap();
    let bearer = text(&configuration["env"], "CAIRN_MCP_RUN_TOKEN");
    let package = registry::active(&s, "example").await.unwrap();
    let Scope::ResourceAgent(authorization) = grants::authorize(
        &s,
        &run,
        &package,
        "read",
        &json!({"resource": resource}),
        bearer,
    )
    .await
    .unwrap() else {
        panic!("Expected a native resource authorization")
    };
    let authorization: Arc<grants::Authorization> = Arc::from(authorization);
    let context = capabilities::Context {
        management: None,
        local_authority: None,
        connection: authorization.connection.clone(),
        manifest: package.manifest,
        event: Value::Null,
        link: None,
        draft: None,
        resource_grant: Some(authorization.clone()),
    };
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let (calling, entering, releasing) = (s.clone(), entered.clone(), release.clone());
    let mut pending = tokio::spawn(async move {
        capabilities::effect(&calling, &context, "http.request",
            json!({"url": "https://api.example.test/messages", "auth": {"resource": resource, "oauth": "bot"}}),
            || async {
                // A final caller check may await Beacon after OAuth was loaded.
                entering.notify_one();
                releasing.notified().await;
                grants::recheck(&calling, &authorization).await
            }).await
    });
    tokio::select! {
        () = entered.notified() => {},
        result = &mut pending => panic!("Ended before final authority check: {result:?}"),
        () = tokio::time::sleep(std::time::Duration::from_secs(3)) => panic!("Final authority check was not reached"),
    }
    let disconnected = common::send(
        &app,
        request("DELETE", &path, Value::Null, Some("fixture-owner")),
    )
    .await;
    assert_eq!(disconnected.status(), StatusCode::OK);
    release.notify_one();
    let result = pending.await.unwrap();
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "A disconnected personal credential reached the provider"
    );
    assert!(result.is_err());
    server.abort();
}

#[tokio::test]
async fn personal_tools_use_only_the_authenticated_author_of_the_current_chat_turn() {
    let (_root, s, instance, resource, path, app) = configured_fixture(false, true).await;
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_ref = calls.clone();
    let provider = axum::Router::new()
        .route(
            "/token",
            axum::routing::post(|| async {
                axum::Json(
                    json!({"access_token": "personal-owner-fixture", "scope": "messages:read",
                "team": {"id": "T111"}, "authed_user": {"id": "U111"}}),
                )
            }),
        )
        .route(
            "/messages",
            axum::routing::get(move |headers: axum::http::HeaderMap| {
                let calls = calls_ref.clone();
                async move {
                    assert_eq!(headers["authorization"], "Bearer personal-owner-fixture");
                    calls.fetch_add(1, Ordering::SeqCst);
                    axum::Json(json!({"actor": "U111"}))
                }
            }),
        );
    let (origin, server) = common::serve_locally(provider).await;
    s.plugins.route_http_to(Some(origin));
    let started = begin(&app, &path).await;
    let callback = format!(
        "/api/plugin-oauth/{instance}/callback?state={}&code=owner-code",
        text(&started, "state")
    );
    assert_eq!(
        common::send(&app, request("GET", &callback, Value::Null, None))
            .await
            .status(),
        StatusCode::OK
    );
    let finished = common::send(
        &app,
        request(
            "POST",
            &format!("{path}/finish"),
            json!({"state": started["state"]}),
            Some("fixture-owner"),
        ),
    )
    .await;
    assert_eq!(finished.status(), StatusCode::OK);
    let grants = format!(
        "/api/plugins/example/connections/{instance}/resources/workspace/{resource}/grants"
    );
    let granted = common::send(
        &app,
        request(
            "POST",
            &grants,
            json!({"agentId": MAIN_AGENT_ID, "profile": "read"}),
            Some("fixture-owner"),
        ),
    )
    .await;
    assert_eq!(granted.status(), StatusCode::OK);
    let input = json!({"resource": resource});
    // In-process submissions have no authenticated human, even when a claimed
    // installation has an owner and that owner authorized this personal provider.
    let untrusted = s
        .chat_create(json!({"agentId": MAIN_AGENT_ID}))
        .await
        .unwrap();
    s.chat_send(
        text(&untrusted, "id"),
        json!({"id": cairn_installation::config::id(), "text": "unattributed"}),
    )
    .await
    .unwrap();
    s.chat_tick(&HashSet::new()).await.unwrap();
    let detail = s.chat_detail(text(&untrusted, "id")).await.unwrap();
    let run = s
        .store
        .patch_run(
            text(&detail["run"], "id"),
            json!({"status": "running", "isolated": true}),
        )
        .await
        .unwrap();
    let configuration = s.mcps.run_configuration(&s, &run).await.unwrap();
    let bearer = text(&configuration["env"], "CAIRN_MCP_RUN_TOKEN");
    assert!(
        agents::call_tool(&s, bearer, "example_read", &input)
            .await
            .is_err()
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);

    let chat = s
        .chat_create(json!({"agentId": MAIN_AGENT_ID}))
        .await
        .unwrap();
    let message_path = format!("/api/chats/{}/messages", text(&chat, "id"));
    let sent = common::send(&app, request("POST", &message_path,
        json!({"id": cairn_installation::config::id(), "text": "attributed", "requester": {"account": "another-account"}}), Some("fixture-owner"))).await;
    assert_eq!(
        sent.status(),
        StatusCode::OK,
        "{}",
        common::read_text(sent).await
    );
    s.chat_tick(&HashSet::new()).await.unwrap();
    let detail = s.chat_detail(text(&chat, "id")).await.unwrap();
    let run = s
        .store
        .patch_run(
            text(&detail["run"], "id"),
            json!({"status": "running", "isolated": true}),
        )
        .await
        .unwrap();
    let configuration = s.mcps.run_configuration(&s, &run).await.unwrap();
    let bearer = text(&configuration["env"], "CAIRN_MCP_RUN_TOKEN");
    let result = agents::call_tool(&s, bearer, "example_read", &input)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(result, json!({"actor": "U111"}));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let command = s
        .plugins
        .command(
            &s,
            bearer,
            &json!({
                "plugin": "example", "command": "read", "args": ["--resource", resource]
            }),
        )
        .await
        .unwrap();
    assert_eq!(command["value"], json!({"actor": "U111"}));
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    // A changed turn invalidates the old run token and the requester's authority.
    s.store
        .patch_run(
            text(&run, "id"),
            json!({"chatExecution": {"messageId": "another-turn"}}),
        )
        .await
        .unwrap();
    assert!(
        agents::call_tool(&s, bearer, "example_read", &input)
            .await
            .is_err()
    );
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    server.abort();
}

async fn draft_fixture(
    personal: bool,
) -> (TempDir, Arc<Service>, Value, Value, String, axum::Router) {
    let (root, s, _live, _resource, _path, app) = configured_fixture(false, personal).await;
    let (draft, resource, oauth) = create_setup(&app).await;
    (root, s, draft, resource, oauth, app)
}

async fn create_setup(app: &axum::Router) -> (Value, Value, String) {
    let collection = "/api/plugins/example/setup-drafts";
    let response = common::send(
        app,
        request(
            "POST",
            collection,
            json!({
                "name": "Prepare app",
                "settings": {"clientId": "client-fixture"},
                "secrets": {"clientSecret": "client-secret-fixture"},
            }),
            Some("fixture-owner"),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let draft = common::read_json(response).await;
    let resources = format!("{collection}/{}/resources/workspace", text(&draft, "id"));
    let response = common::send(
        app,
        request(
            "POST",
            &resources,
            json!({
                "setupRevision": draft["revision"],
                "name": "Workspace",
            }),
            Some("fixture-owner"),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let resource = common::read_json(response).await;
    let item = format!("{resources}/{}", text(&resource, "id"));
    let oauth = format!("{item}/oauth/bot");
    (draft, resource, oauth)
}

#[tokio::test]
async fn owner_can_connect_an_inert_draft_resource_without_publishing_its_tokens_or_authority() {
    let (_root, s, draft, resource, oauth, app) = draft_fixture(false).await;
    let item = oauth.strip_suffix("/oauth/bot").unwrap();
    let response = common::send(
        &app,
        request(
            "POST",
            &oauth,
            json!({
                "setupRevision": draft["revision"],
                "scopes": ["messages:read"],
            }),
            Some("fixture-owner"),
        ),
    )
    .await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "{}",
        common::read_text(response).await
    );
    let started = common::read_json(response).await;
    assert!(started["expiresAt"].as_i64().unwrap() <= draft["expiresAt"].as_i64().unwrap());
    let provider = axum::Router::new().route(
        "/token",
        axum::routing::post(
            |headers: axum::http::HeaderMap,
             axum::Form(form): axum::Form<HashMap<String, String>>| async move {
                assert!(
                    headers["authorization"]
                        .to_str()
                        .unwrap()
                        .starts_with("Basic ")
                );
                assert_eq!(form["code"], "draft-code-fixture");
                axum::Json(json!({
                    "access_token": "draft-bot-token-fixture",
                    "refresh_token": "draft-refresh-fixture",
                    "scope": "messages:read",
                    "team": {"id": "T111"},
                }))
            },
        ),
    );
    let (origin, server) = common::serve_locally(provider).await;
    s.plugins.route_http_to(Some(origin));
    let callback = format!(
        "/api/plugin-oauth/{}/callback?state={}&code=draft-code-fixture",
        text(&draft, "id"),
        text(&started, "state")
    );
    assert_eq!(
        common::send(&app, request("GET", &callback, Value::Null, None))
            .await
            .status(),
        StatusCode::OK
    );
    let finish = format!("{oauth}/finish");
    let live_finish = format!(
        "/api/plugins/example/connections/{}/resources/workspace/{}/oauth/bot/finish",
        text(&draft, "id"),
        text(&resource, "id")
    );
    let wrong_namespace = common::send(
        &app,
        request(
            "POST",
            &live_finish,
            json!({"state": started["state"]}),
            Some("fixture-owner"),
        ),
    )
    .await;
    assert_eq!(
        wrong_namespace.status(),
        StatusCode::FORBIDDEN,
        "the active OAuth endpoint accepted setup consent"
    );
    let response = common::send(
        &app,
        request(
            "POST",
            &finish,
            json!({
                "state": started["state"],
            }),
            Some("fixture-owner"),
        ),
    )
    .await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "{}",
        common::read_text(response).await
    );
    let finished = common::read_json(response).await;
    assert_eq!(finished["status"], "authorized");
    assert!(!finished.to_string().contains("fixture"));
    let response = common::send(
        &app,
        request("GET", &oauth, Value::Null, Some("fixture-owner")),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(common::read_json(response).await["status"], "authorized");
    let response = common::send(
        &app,
        request("GET", item, Value::Null, Some("fixture-owner")),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let resource = common::read_json(response).await;
    assert_eq!(resource["settings"]["externalId"], "T111");
    assert!(!resource.to_string().contains("fixture"));
    let live = format!(
        "/api/plugins/example/connections/{}/resources/workspace/{}",
        text(&draft, "id"),
        text(&resource, "id")
    );
    assert_eq!(
        common::send(
            &app,
            request("GET", &live, Value::Null, Some("fixture-owner"))
        )
        .await
        .status(),
        StatusCode::NOT_FOUND
    );
    assert!(
        panels::action(
            &s,
            "example",
            text(&draft, "id"),
            &json!({
                "action": "read", "payload": {"resource": resource["id"]},
            })
        )
        .await
        .is_err(),
        "an inert draft acquired a live owner's OAuth capability"
    );
    let replay = common::send(
        &app,
        request(
            "POST",
            &finish,
            json!({"state": started["state"]}),
            Some("fixture-owner"),
        ),
    )
    .await;
    assert_eq!(replay.status(), StatusCode::CONFLICT);
    server.abort();
}

#[tokio::test]
async fn changing_or_discarding_setup_during_exchange_prevents_late_oauth_authority() {
    for change in [
        "disconnect",
        "edit-resource",
        "edit-draft",
        "delete-resource",
        "delete-draft",
    ] {
        let (_root, s, draft, resource, oauth, app) = draft_fixture(false).await;
        let response = common::send(
            &app,
            request(
                "POST",
                &oauth,
                json!({
                    "setupRevision": draft["revision"],
                    "scopes": ["messages:read"],
                }),
                Some("fixture-owner"),
            ),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let started = common::read_json(response).await;
        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let (entering, releasing) = (entered.clone(), release.clone());
        let provider = axum::Router::new().route(
            "/token",
            axum::routing::post(move || {
                let (entered, release) = (entering.clone(), releasing.clone());
                async move {
                    entered.notify_one();
                    release.notified().await;
                    axum::Json(json!({
                        "access_token": "must-not-escape-draft-fixture",
                        "scope": "messages:read",
                        "team": {"id": "T111"},
                    }))
                }
            }),
        );
        let (origin, server) = common::serve_locally(provider).await;
        s.plugins.route_http_to(Some(origin));
        let callback = format!(
            "/api/plugin-oauth/{}/callback?state={}&code=fixture",
            text(&draft, "id"),
            text(&started, "state")
        );
        assert_eq!(
            common::send(&app, request("GET", &callback, Value::Null, None))
                .await
                .status(),
            StatusCode::OK
        );
        let finish = format!("{oauth}/finish");
        let pending_app = app.clone();
        let pending = tokio::spawn(async move {
            common::send(
                &pending_app,
                request(
                    "POST",
                    &finish,
                    json!({"state": started["state"]}),
                    Some("fixture-owner"),
                ),
            )
            .await
        });
        tokio::time::timeout(std::time::Duration::from_secs(3), entered.notified())
            .await
            .unwrap();
        let root_path = format!("/api/plugins/example/setup-drafts/{}", text(&draft, "id"));
        let item = oauth.strip_suffix("/oauth/bot").unwrap();
        let changed = match change {
            "disconnect" => request(
                "DELETE",
                &oauth,
                json!({"setupRevision": draft["revision"]}),
                Some("fixture-owner"),
            ),
            "edit-resource" => request(
                "PUT",
                item,
                json!({
                    "setupRevision": draft["revision"], "revision": resource["revision"],
                    "name": "Renamed while connecting", "settings": {},
                }),
                Some("fixture-owner"),
            ),
            "edit-draft" => request(
                "PATCH",
                &root_path,
                json!({
                    "revision": draft["revision"], "name": "Renamed app",
                    "settings": {"clientId": "client-fixture"},
                }),
                Some("fixture-owner"),
            ),
            "delete-resource" => request(
                "DELETE",
                item,
                json!({
                    "setupRevision": draft["revision"], "revision": resource["revision"],
                }),
                Some("fixture-owner"),
            ),
            "delete-draft" => request(
                "DELETE",
                &root_path,
                json!({"revision": draft["revision"]}),
                Some("fixture-owner"),
            ),
            _ => unreachable!(),
        };
        assert_eq!(common::send(&app, changed).await.status(), StatusCode::OK);
        release.notify_one();
        let response = pending.await.unwrap();
        assert!(
            matches!(
                response.status(),
                StatusCode::FORBIDDEN | StatusCode::NOT_FOUND | StatusCode::CONFLICT
            ),
            "{change} allowed a revoked exchange to finish: {}",
            common::read_text(response).await
        );
        server.abort();
    }
}

#[tokio::test]
async fn setup_oauth_requires_its_owner_and_refuses_personal_providers() {
    let (_root, _s, draft, _resource, oauth, app) = draft_fixture(false).await;
    let input = json!({"setupRevision": draft["revision"], "scopes": ["messages:read"]});
    let mut member = request("POST", &oauth, input.clone(), Some("fixture-owner"));
    member
        .extensions_mut()
        .insert(InstallationIdentity::trusted(
            InstallationRole::Member,
            "fixture-owner",
        ));
    assert_eq!(
        common::send(&app, member).await.status(),
        StatusCode::FORBIDDEN
    );
    let stranger = common::send(
        &app,
        request("POST", &oauth, input.clone(), Some("other-owner")),
    )
    .await;
    assert!(matches!(
        stranger.status(),
        StatusCode::FORBIDDEN | StatusCode::NOT_FOUND
    ));
    let mut stale = input;
    stale["setupRevision"] = json!("stale");
    assert_eq!(
        common::send(&app, request("POST", &oauth, stale, Some("fixture-owner")))
            .await
            .status(),
        StatusCode::CONFLICT
    );
    let (_root, _s, draft, _resource, oauth, app) = draft_fixture(true).await;
    let response = common::send(
        &app,
        request(
            "POST",
            &oauth,
            json!({
                "setupRevision": draft["revision"], "scopes": ["messages:read"],
            }),
            Some("fixture-owner"),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

async fn prepare_draft_expiry(s: &Service, draft: &Value, resource: &Value, deadline: i64) {
    let key = format!("plugin-setup:{}", text(draft, "id"));
    let mut saved = s.store.kv(&key).await.unwrap().unwrap();
    saved["expiresAt"] = json!(deadline);
    saved["createdAt"] = json!(deadline - 86_400_000);
    s.store.set(&key, saved, Some(deadline)).await.unwrap();
    for key in [
        format!("mcp-secret:plugin-secrets:{}", text(draft, "id")),
        format!(
            "plugin-setup-resource:{}:{}",
            text(draft, "id"),
            text(resource, "id")
        ),
        format!(
            "mcp-secret:plugin-resource-secrets:{}",
            text(resource, "id")
        ),
    ] {
        let value = s.store.kv(&key).await.unwrap().unwrap();
        s.store.set(&key, value, Some(deadline)).await.unwrap();
    }
}

#[tokio::test]
async fn setup_consent_and_authorization_expire_with_their_prepared_draft() {
    for phase in ["pending", "authorized"] {
        let (_root, s, draft, resource, oauth, app) = draft_fixture(false).await;
        // Prepare a coherent persisted draft near the end of its 24-hour lifetime.
        // HTTP still performs the real consent, callback, exchange and expiry checks.
        let deadline = cairn_installation::config::now() + 5000;
        prepare_draft_expiry(&s, &draft, &resource, deadline).await;
        let response = common::send(
            &app,
            request(
                "POST",
                &oauth,
                json!({
                    "setupRevision": draft["revision"], "scopes": ["messages:read"],
                }),
                Some("fixture-owner"),
            ),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let started = common::read_json(response).await;
        assert_eq!(
            started["expiresAt"], deadline,
            "setup consent outlived its draft"
        );
        let provider = axum::Router::new().route("/token", axum::routing::post(|| async {
            axum::Json(json!({"access_token": "expiring-draft-fixture", "scope": "messages:read", "team": {"id": "T111"}}))
        }));
        let (origin, server) = common::serve_locally(provider).await;
        s.plugins.route_http_to(Some(origin));
        let callback = format!(
            "/api/plugin-oauth/{}/callback?state={}&code=fixture",
            text(&draft, "id"),
            text(&started, "state")
        );
        if phase == "authorized" {
            assert_eq!(
                common::send(&app, request("GET", &callback, Value::Null, None))
                    .await
                    .status(),
                StatusCode::OK
            );
            let response = common::send(
                &app,
                request(
                    "POST",
                    &format!("{oauth}/finish"),
                    json!({"state": started["state"]}),
                    Some("fixture-owner"),
                ),
            )
            .await;
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(common::read_json(response).await["status"], "authorized");
        }
        // Observe encrypted persisted state as well as endpoint availability: an
        // expired root must not merely hide a permanently retained credential.
        let encrypted = s.store.keys("mcp-secret:plugin-oauth").await.unwrap();
        assert_eq!(
            encrypted.len(),
            1,
            "expected one pending or final credential"
        );
        assert!(
            !encrypted[0]
                .1
                .to_string()
                .contains("expiring-draft-fixture")
        );
        let remaining = (deadline - cairn_installation::config::now()).max(0) as u64;
        tokio::time::sleep(std::time::Duration::from_millis(remaining + 30)).await;
        assert!(s.store.kv(&encrypted[0].0).await.unwrap().is_none());
        assert_eq!(
            common::send(
                &app,
                request("GET", &oauth, Value::Null, Some("fixture-owner"))
            )
            .await
            .status(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            common::send(&app, request("GET", &callback, Value::Null, None))
                .await
                .status(),
            StatusCode::CONFLICT
        );
        server.abort();
    }
}

#[tokio::test]
async fn replacing_the_approval_invalidates_old_setup_consent_even_when_the_same_files_return() {
    for change in ["reapprove", "roundtrip"] {
        for phase in ["before-callback", "before-finish"] {
            let (_root, s, draft, _resource, oauth, app) = draft_fixture(false).await;
            let response = common::send(
                &app,
                request(
                    "POST",
                    &oauth,
                    json!({
                        "setupRevision": draft["revision"], "scopes": ["messages:read"],
                    }),
                    Some("fixture-owner"),
                ),
            )
            .await;
            assert_eq!(response.status(), StatusCode::OK);
            let started = common::read_json(response).await;
            let calls = Arc::new(AtomicUsize::new(0));
            let observed = calls.clone();
            let provider = axum::Router::new().route("/token", axum::routing::post(move || {
                let observed = observed.clone();
                async move {
                    observed.fetch_add(1, Ordering::SeqCst);
                    axum::Json(json!({"access_token": "must-not-be-issued-fixture", "scope": "messages:read", "team": {"id": "T111"}}))
                }
            }));
            let (origin, server) = common::serve_locally(provider).await;
            s.plugins.route_http_to(Some(origin));
            let callback = format!(
                "/api/plugin-oauth/{}/callback?state={}&code=fixture",
                text(&draft, "id"),
                text(&started, "state")
            );
            if phase == "before-finish" {
                assert_eq!(
                    common::send(&app, request("GET", &callback, Value::Null, None))
                        .await
                        .status(),
                    StatusCode::OK
                );
            }
            let package = registry::active(&s, "example").await.unwrap();
            if change == "roundtrip" {
                let mut manifest = serde_json::to_value(&package.manifest).unwrap();
                manifest["version"] = json!("2");
                let second = registry::install(
                    &s,
                    &json!({"manifest": manifest, "source": package.source.main}),
                )
                .await
                .unwrap();
                registry::approve(&s, "example", "2", text(&second, "digest"))
                    .await
                    .unwrap();
                registry::activate(&s, "example", "1").await.unwrap();
            } else {
                registry::approve(&s, "example", "1", &package.digest)
                    .await
                    .unwrap();
            }
            if phase == "before-callback" {
                assert_eq!(
                    common::send(&app, request("GET", &callback, Value::Null, None))
                        .await
                        .status(),
                    StatusCode::CONFLICT,
                    "{change} restored old callback authority"
                );
            }
            let response = common::send(
                &app,
                request(
                    "POST",
                    &format!("{oauth}/finish"),
                    json!({"state": started["state"]}),
                    Some("fixture-owner"),
                ),
            )
            .await;
            assert_eq!(
                response.status(),
                StatusCode::CONFLICT,
                "{change}/{phase} restored consent: {}",
                common::read_text(response).await
            );
            assert_eq!(
                calls.load(Ordering::SeqCst),
                0,
                "invalid consent reached the token provider"
            );
            server.abort();
        }
    }
}

#[tokio::test]
async fn owner_activation_promotes_setup_identity_resources_and_oauth_in_one_commit() {
    let (_root, s, draft, resource, oauth, app) = draft_fixture(false).await;
    let deadline = cairn_installation::config::now() + 5000;
    prepare_draft_expiry(&s, &draft, &resource, deadline).await;
    let instance = text(&draft, "id");
    let root_path = format!("/api/plugins/example/setup-drafts/{instance}");
    let activation = format!("{root_path}/activate");
    let provider = axum::Router::new()
        .route("/token", axum::routing::post(|| async {
            axum::Json(json!({"access_token": "promoted-bot-fixture", "scope": "messages:read", "team": {"id": "T111"}}))
        }))
        .route("/messages", axum::routing::get(|headers: axum::http::HeaderMap| async move {
            assert_eq!(headers["authorization"], "Bearer promoted-bot-fixture");
            axum::Json(json!({"ok": true}))
        }));
    let (origin, server) = common::serve_locally(provider).await;
    s.plugins.route_http_to(Some(origin));
    let response = common::send(
        &app,
        request(
            "POST",
            &oauth,
            json!({
                "setupRevision": draft["revision"], "scopes": ["messages:read"]
            }),
            Some("fixture-owner"),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let started = common::read_json(response).await;
    let callback = format!(
        "/api/plugin-oauth/{instance}/callback?state={}&code=fixture",
        text(&started, "state")
    );
    assert_eq!(
        common::send(&app, request("GET", &callback, Value::Null, None))
            .await
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        common::send(
            &app,
            request(
                "POST",
                &format!("{oauth}/finish"),
                json!({"state": started["state"]}),
                Some("fixture-owner")
            )
        )
        .await
        .status(),
        StatusCode::OK
    );
    // A second unfinished consent must be discarded rather than promoted.
    let response = common::send(
        &app,
        request(
            "POST",
            &oauth,
            json!({
                "setupRevision": draft["revision"], "scopes": ["messages:read"],
            }),
            Some("fixture-owner"),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let unfinished = common::read_json(response).await;
    let response = common::send(
        &app,
        request(
            "POST",
            &activation,
            json!({"revision": draft["revision"], "enabled": true}),
            Some("fixture-owner"),
        ),
    )
    .await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "{}",
        common::read_text(response).await
    );
    let active = common::read_json(response).await;
    assert_eq!(active["id"], draft["id"]);
    assert_eq!(active["name"], draft["name"]);
    assert_eq!(active["enabled"], true);
    assert_eq!(active["agentIds"], json!([]));
    assert!(!active.to_string().contains("secret-fixture"));
    let live_resource = format!(
        "/api/plugins/example/connections/{instance}/resources/workspace/{}",
        text(&resource, "id")
    );
    let response = common::send(
        &app,
        request("GET", &live_resource, Value::Null, Some("fixture-owner")),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let promoted = common::read_json(response).await;
    assert_eq!(promoted["id"], resource["id"]);
    assert_eq!(promoted["settings"]["externalId"], "T111");
    let remaining = (deadline - cairn_installation::config::now()).max(0) as u64;
    tokio::time::sleep(std::time::Duration::from_millis(remaining + 30)).await;
    let discarded_callback = format!(
        "/api/plugin-oauth/{instance}/callback?state={}&code=fixture",
        text(&unfinished, "state")
    );
    assert_eq!(
        common::send(&app, request("GET", &discarded_callback, Value::Null, None))
            .await
            .status(),
        StatusCode::CONFLICT
    );
    let response = common::send(
        &app,
        request(
            "GET",
            &format!("{live_resource}/oauth/bot"),
            Value::Null,
            Some("fixture-owner"),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(common::read_json(response).await["status"], "authorized");
    assert_eq!(
        panels::action(
            &s,
            "example",
            instance,
            &json!({"action": "read", "payload": {"resource": resource["id"]}})
        )
        .await
        .unwrap()["notice"],
        "Authorized"
    );
    assert_eq!(
        common::send(
            &app,
            request("GET", &root_path, Value::Null, Some("fixture-owner"))
        )
        .await
        .status(),
        StatusCode::NOT_FOUND
    );
    let again = common::send(
        &app,
        request(
            "POST",
            &activation,
            json!({"revision": draft["revision"], "enabled": true}),
            Some("fixture-owner"),
        ),
    )
    .await;
    assert!(matches!(
        again.status(),
        StatusCode::NOT_FOUND | StatusCode::CONFLICT
    ));
    server.abort();
}

#[tokio::test]
async fn activation_during_exchange_cannot_import_late_setup_tokens() {
    let (_root, s, draft, resource, oauth, app) = draft_fixture(false).await;
    let response = common::send(
        &app,
        request(
            "POST",
            &oauth,
            json!({
                "setupRevision": draft["revision"], "scopes": ["messages:read"],
            }),
            Some("fixture-owner"),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let started = common::read_json(response).await;
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let (read_entered, read_release) = (entered.clone(), release.clone());
    let provider = axum::Router::new().route("/token", axum::routing::post(move || {
        let (entered, release) = (read_entered.clone(), read_release.clone());
        async move {
            entered.notify_one();
            release.notified().await;
            axum::Json(json!({"access_token": "late-setup-fixture", "scope": "messages:read", "team": {"id": "T111"}}))
        }
    }));
    let (origin, server) = common::serve_locally(provider).await;
    s.plugins.route_http_to(Some(origin));
    let callback = format!(
        "/api/plugin-oauth/{}/callback?state={}&code=fixture",
        text(&draft, "id"),
        text(&started, "state")
    );
    assert_eq!(
        common::send(&app, request("GET", &callback, Value::Null, None))
            .await
            .status(),
        StatusCode::OK
    );
    let pending_app = app.clone();
    let pending_request = request(
        "POST",
        &format!("{oauth}/finish"),
        json!({"state": started["state"]}),
        Some("fixture-owner"),
    );
    let pending = tokio::spawn(async move { common::send(&pending_app, pending_request).await });
    tokio::time::timeout(std::time::Duration::from_secs(3), entered.notified())
        .await
        .unwrap();
    let activation = format!(
        "/api/plugins/example/setup-drafts/{}/activate",
        text(&draft, "id")
    );
    let response = common::send(
        &app,
        request(
            "POST",
            &activation,
            json!({"revision": draft["revision"], "enabled": true}),
            Some("fixture-owner"),
        ),
    )
    .await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "{}",
        common::read_text(response).await
    );
    release.notify_one();
    let response = pending.await.unwrap();
    assert!(matches!(
        response.status(),
        StatusCode::NOT_FOUND | StatusCode::CONFLICT | StatusCode::FORBIDDEN
    ));
    let status = format!(
        "/api/plugins/example/connections/{}/resources/workspace/{}/oauth/bot",
        text(&draft, "id"),
        text(&resource, "id")
    );
    let response = common::send(
        &app,
        request("GET", &status, Value::Null, Some("fixture-owner")),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(common::read_json(response).await["status"], "disconnected");
    server.abort();
}

#[tokio::test]
async fn final_form_cannot_promote_oauth_from_a_different_client_app() {
    let (_root, s, _live, _resource, _path, app) = fixture(false).await;
    let package = registry::active(&s, "example").await.unwrap();
    let mut manifest = serde_json::to_value(&package.manifest).unwrap();
    manifest["version"] = json!("2");
    manifest["instanceForm"] = json!(true);
    let source = package.source.main.replacen(
        "return {action = function(event)",
        "return {form = function(event) return {values = {clientId = 'derived-client-fixture'}} end, action = function(event)",
        1,
    );
    let package = registry::install(&s, &json!({"manifest": manifest, "source": source}))
        .await
        .unwrap();
    registry::approve(&s, "example", "2", text(&package, "digest"))
        .await
        .unwrap();
    let (draft, resource, oauth) = create_setup(&app).await;
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    let provider = axum::Router::new().route("/token", axum::routing::post(move |headers: axum::http::HeaderMap| {
        let observed = observed.clone();
        async move {
            let call = observed.fetch_add(1, Ordering::SeqCst);
            let client = if call == 0 {"client-fixture"} else {"derived-client-fixture"};
            let basic = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, format!("{client}:client-secret-fixture"));
            assert_eq!(headers["authorization"], format!("Basic {basic}"));
            axum::Json(json!({"access_token": "app-bound-access-fixture", "refresh_token": "app-bound-refresh-fixture", "scope": "messages:read", "team": {"id": "T111"}}))
        }
    }));
    let (origin, server) = common::serve_locally(provider).await;
    s.plugins.route_http_to(Some(origin));
    let consent = |revision: Value| {
        request(
            "POST",
            &oauth,
            json!({"setupRevision": revision, "scopes": ["messages:read"]}),
            Some("fixture-owner"),
        )
    };
    let authorize = |state: &Value| {
        request(
            "POST",
            &format!("{oauth}/finish"),
            json!({"state": state}),
            Some("fixture-owner"),
        )
    };
    let response = common::send(&app, consent(draft["revision"].clone())).await;
    assert_eq!(response.status(), StatusCode::OK);
    let started = common::read_json(response).await;
    let callback = format!(
        "/api/plugin-oauth/{}/callback?state={}&code=fixture",
        text(&draft, "id"),
        text(&started, "state")
    );
    assert_eq!(
        common::send(&app, request("GET", &callback, Value::Null, None))
            .await
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        common::send(&app, authorize(&started["state"]))
            .await
            .status(),
        StatusCode::OK
    );
    let root = format!("/api/plugins/example/setup-drafts/{}", text(&draft, "id"));
    let activation = format!("{root}/activate");
    let response = common::send(
        &app,
        request(
            "POST",
            &activation,
            json!({"revision": draft["revision"], "enabled": true}),
            Some("fixture-owner"),
        ),
    )
    .await;
    assert_eq!(
        response.status(),
        StatusCode::CONFLICT,
        "old client authorization was promoted: {}",
        common::read_text(response).await
    );
    assert!(
        s.store
            .get("plugin-connections", text(&draft, "id"))
            .await
            .unwrap()
            .is_none()
    );
    let response = common::send(
        &app,
        request("GET", &oauth, Value::Null, Some("fixture-owner")),
    )
    .await;
    assert_eq!(
        common::read_json(response).await["status"],
        "authorized",
        "failed activation discarded the usable draft consent"
    );
    let response = common::send(&app, request("PATCH", &root, json!({
        "revision": draft["revision"], "name": draft["name"], "settings": {"clientId": "derived-client-fixture"},
    }), Some("fixture-owner"))).await;
    assert_eq!(response.status(), StatusCode::OK);
    let updated = common::read_json(response).await;
    let response = common::send(&app, consent(updated["revision"].clone())).await;
    assert_eq!(response.status(), StatusCode::OK);
    let restarted = common::read_json(response).await;
    let callback = format!(
        "/api/plugin-oauth/{}/callback?state={}&code=fixture",
        text(&draft, "id"),
        text(&restarted, "state")
    );
    assert_eq!(
        common::send(&app, request("GET", &callback, Value::Null, None))
            .await
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        common::send(&app, authorize(&restarted["state"]))
            .await
            .status(),
        StatusCode::OK
    );
    let response = common::send(
        &app,
        request(
            "POST",
            &activation,
            json!({"revision": updated["revision"], "enabled": true}),
            Some("fixture-owner"),
        ),
    )
    .await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "{}",
        common::read_text(response).await
    );
    let live = common::read_json(response).await;
    assert_eq!(live["settings"]["clientId"], "derived-client-fixture");
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    let status = format!(
        "/api/plugins/example/connections/{}/resources/workspace/{}/oauth/bot",
        text(&draft, "id"),
        text(&resource, "id")
    );
    let response = common::send(
        &app,
        request("GET", &status, Value::Null, Some("fixture-owner")),
    )
    .await;
    assert_eq!(common::read_json(response).await["status"], "authorized");
    server.abort();
}

#[tokio::test]
async fn personal_authorization_discovery_pages_only_personal_providers() {
    let (_root, s, instance, _resource, _path, app) = configured_fixture(false, true).await;
    let original = registry::active(&s, "example").await.unwrap();
    let mut manifest = serde_json::to_value(&original.manifest).unwrap();
    manifest["version"] = json!("2");
    let mut owner_provider = manifest["oauth"][0].clone();
    owner_provider["name"] = json!("owner");
    owner_provider["title"] = json!("Owner-only connection");
    owner_provider.as_object_mut().unwrap().remove("personal");
    manifest["oauth"]
        .as_array_mut()
        .unwrap()
        .push(owner_provider);
    let installed = registry::install(
        &s,
        &json!({"manifest": manifest, "source": original.source.main}),
    )
    .await
    .unwrap();
    registry::approve(&s, "example", "2", text(&installed, "digest"))
        .await
        .unwrap();
    for i in 0..100 {
        resources::save(
            &s,
            "example",
            &instance,
            "workspace",
            &json!({"name": format!("Workspace {i}"), "settings": {"externalId": "T111"}}),
            None,
        )
        .await
        .unwrap();
    }
    let path = "/api/plugins/personal-authorizations";
    let first = common::send(
        &app,
        request("GET", path, Value::Null, Some("fixture-owner")),
    )
    .await;
    assert_eq!(
        first.status(),
        StatusCode::OK,
        "{}",
        common::read_text(first).await
    );
    let first = common::read_json(first).await;
    assert_eq!(first["items"].as_array().unwrap().len(), 100);
    assert!(!first.to_string().contains("Owner-only connection"));
    let cursor = text(&first, "nextCursor");
    assert!(!cursor.is_empty());
    let second = common::read_json(
        common::send(
            &app,
            request(
                "GET",
                &format!("{path}?cursor={cursor}"),
                Value::Null,
                Some("fixture-owner"),
            ),
        )
        .await,
    )
    .await;
    assert_eq!(second["items"].as_array().unwrap().len(), 1);
    assert!(second["nextCursor"].is_null());
    let mut ids = first["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| text(item, "id"))
        .collect::<HashSet<_>>();
    assert!(ids.insert(text(&second["items"][0], "id")));
    assert_eq!(ids.len(), 101);
    assert_eq!(
        common::send(
            &app,
            request(
                "GET",
                &format!("{path}?cursor=bad"),
                Value::Null,
                Some("fixture-owner")
            )
        )
        .await
        .status(),
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn personal_discovery_rechecks_membership_owner_and_package_after_loading_metadata() {
    for changed in ["membership", "owner", "approval", "files"] {
        let (_root, s, _instance, _resource, _path, app) = configured_fixture(false, true).await;
        let reads = Arc::new(AtomicUsize::new(0));
        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let reads_ref = reads.clone();
        let entered_ref = entered.clone();
        let release_ref = release.clone();
        let authority = axum::Router::new().route(
            "/api/relay/00000000-0000-4000-8000-000000000055/task-authors",
            axum::routing::get(move || {
                let read = reads_ref.fetch_add(1, Ordering::SeqCst);
                let entered = entered_ref.clone();
                let release = release_ref.clone();
                async move {
                    if read == 1 {
                        entered.notify_one();
                        release.notified().await;
                    }
                    axum::Json(json!({
                        "owner": {
                            "account_id": "fixture-owner",
                            "access_id": if read > 0 && changed == "owner" {"new-owner-access"} else {"fixture-owner"}
                        },
                        "members": [{
                            "account_id": "alice",
                            "access_id": if read > 0 && changed == "membership" {"alice-rejoined"} else {"alice-original"}
                        }]
                    }))
                }
            }),
        );
        let (origin, authority_server) = common::serve_locally(authority).await;
        let identity_path = s.config.data_dir.join("installation-relay/identity.json");
        let mut identity: Value =
            serde_json::from_slice(&std::fs::read(&identity_path).unwrap()).unwrap();
        identity["origin"] = json!(origin);
        std::fs::write(identity_path, serde_json::to_vec(&identity).unwrap()).unwrap();
        let app_ref = app.clone();
        let pending = tokio::spawn(async move {
            common::send(
                &app_ref,
                member_request(
                    "GET",
                    "/api/plugins/personal-authorizations",
                    Value::Null,
                    "alice",
                ),
            )
            .await
        });
        tokio::time::timeout(std::time::Duration::from_secs(5), entered.notified())
            .await
            .unwrap();
        if changed == "approval" {
            let original = registry::active(&s, "example").await.unwrap();
            let mut manifest = serde_json::to_value(&original.manifest).unwrap();
            manifest["version"] = json!("2");
            let installed = registry::install(
                &s,
                &json!({"manifest": manifest, "source": original.source.main}),
            )
            .await
            .unwrap();
            let approved = common::send(
                &app,
                request(
                    "POST",
                    "/api/plugins/packages/example/approve",
                    json!({"version": "2", "digest": installed["digest"]}),
                    Some("fixture-owner"),
                ),
            )
            .await;
            assert_eq!(approved.status(), StatusCode::OK);
        }
        if changed == "files" {
            let source = s.config.data_dir.join("plugins/example/1/main.luau");
            let content = std::fs::read_to_string(&source).unwrap();
            std::fs::write(source, format!("{content}\n-- late discovery edit\n")).unwrap();
        }
        release.notify_one();
        let response = pending.await.unwrap();
        let expected = if changed == "approval" {
            StatusCode::CONFLICT
        } else {
            StatusCode::FORBIDDEN
        };
        assert_eq!(
            response.status(),
            expected,
            "stale {changed} discovery: {}",
            common::read_text(response).await
        );
        authority_server.abort();
    }
}

#[tokio::test]
async fn disconnect_during_refresh_authority_lookup_never_sends_the_retired_token() {
    for disconnect in [false, true] {
        let (_root, s, instance, resource, path, app) = configured_fixture(false, true).await;
        let refreshes = Arc::new(AtomicUsize::new(0));
        let reads = Arc::new(AtomicUsize::new(0));
        let renewing = refreshes.clone();
        let reading = reads.clone();
        let provider = axum::Router::new()
            .route(
                "/token",
                axum::routing::post(
                    move |axum::Form(form): axum::Form<HashMap<String, String>>| {
                        let refreshes = renewing.clone();
                        async move {
                            if form["grant_type"] == "refresh_token" {
                                assert_eq!(form["refresh_token"], "retired-refresh-fixture");
                                refreshes.fetch_add(1, Ordering::SeqCst);
                                axum::Json(json!({
                                    "access_token": "renewed-access-fixture",
                                    "refresh_token": "next-refresh-fixture",
                                    "expires_in": 43200,
                                    "scope": "messages:read",
                                    "authed_user": {"id": "U111"}
                                }))
                            } else {
                                axum::Json(json!({
                                    "access_token": "expiring-access-fixture",
                                    "refresh_token": "retired-refresh-fixture",
                                    "expires_in": 1,
                                    "scope": "messages:read",
                                    "team": {"id": "T111"},
                                    "authed_user": {"id": "U111"}
                                }))
                            }
                        }
                    },
                ),
            )
            .route(
                "/messages",
                axum::routing::get(move |headers: axum::http::HeaderMap| {
                    let reads = reading.clone();
                    async move {
                        assert_eq!(headers["authorization"], "Bearer renewed-access-fixture");
                        reads.fetch_add(1, Ordering::SeqCst);
                        axum::Json(json!({"actor": "U111"}))
                    }
                }),
            );
        let (origin, server) = common::serve_locally(provider).await;
        s.plugins.route_http_to(Some(origin));
        let started = begin(&app, &path).await;
        let callback = format!(
            "/api/plugin-oauth/{instance}/callback?state={}&code=fixture",
            text(&started, "state")
        );
        assert_eq!(
            common::send(&app, request("GET", &callback, Value::Null, None))
                .await
                .status(),
            StatusCode::OK
        );
        assert_eq!(
            common::send(
                &app,
                request(
                    "POST",
                    &format!("{path}/finish"),
                    json!({"state": started["state"]}),
                    Some("fixture-owner")
                )
            )
            .await
            .status(),
            StatusCode::OK
        );
        let grants = format!(
            "/api/plugins/example/connections/{instance}/resources/workspace/{resource}/grants"
        );
        assert_eq!(
            common::send(
                &app,
                request(
                    "POST",
                    &grants,
                    json!({"agentId": MAIN_AGENT_ID, "profile": "read"}),
                    Some("fixture-owner")
                )
            )
            .await
            .status(),
            StatusCode::OK
        );
        let chat = s
            .chat_create(json!({"agentId": MAIN_AGENT_ID}))
            .await
            .unwrap();
        assert_eq!(
            common::send(
                &app,
                request(
                    "POST",
                    &format!("/api/chats/{}/messages", text(&chat, "id")),
                    json!({"id": cairn_installation::config::id(), "text": "Read as me"}),
                    Some("fixture-owner")
                )
            )
            .await
            .status(),
            StatusCode::OK
        );
        s.chat_tick(&HashSet::new()).await.unwrap();
        let detail = s.chat_detail(text(&chat, "id")).await.unwrap();
        let run = s
            .store
            .patch_run(
                text(&detail["run"], "id"),
                json!({"status": "running", "isolated": true}),
            )
            .await
            .unwrap();
        let configuration = s.mcps.run_configuration(&s, &run).await.unwrap();
        let bearer = text(&configuration["env"], "CAIRN_MCP_RUN_TOKEN").to_owned();

        // Hold a real authority lookup only after the refresh claim is durable.
        // Other requests, including the owner's disconnect, remain independent.
        let slot = format!(
            "plugin-oauth:{resource}:bot:personal:{}",
            cairn_installation::auth::hex_digest("fixture-owner")
        );
        let held = Arc::new(AtomicBool::new(false));
        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let (service, holding, entering, releasing) =
            (s.clone(), held.clone(), entered.clone(), release.clone());
        let authority = axum::Router::new().route(
            "/api/relay/00000000-0000-4000-8000-000000000055/task-authors",
            axum::routing::get(move || {
                let (service, slot, holding, entered, release) = (
                    service.clone(),
                    slot.clone(),
                    holding.clone(),
                    entering.clone(),
                    releasing.clone(),
                );
                async move {
                    let refreshing = service
                        .vault
                        .get(&slot)
                        .await
                        .unwrap()
                        .is_some_and(|value| value["refreshing"] == true);
                    if refreshing && !holding.swap(true, Ordering::SeqCst) {
                        entered.notify_one();
                        release.notified().await;
                    }
                    axum::Json(json!({
                        "owner": {"account_id": "fixture-owner", "access_id": "fixture-owner"},
                        "members": []
                    }))
                }
            }),
        );
        let (origin, authority_server) = common::serve_locally(authority).await;
        let identity_path = s.config.data_dir.join("installation-relay/identity.json");
        let mut identity: Value =
            serde_json::from_slice(&std::fs::read(&identity_path).unwrap()).unwrap();
        identity["origin"] = json!(origin);
        std::fs::write(identity_path, serde_json::to_vec(&identity).unwrap()).unwrap();
        let calling = s.clone();
        let mut pending = tokio::spawn(async move {
            agents::call_tool(
                &calling,
                &bearer,
                "example_read",
                &json!({"resource": resource}),
            )
            .await
        });
        tokio::select! {
            () = entered.notified() => {},
            result = &mut pending => panic!("Ended before the claimed refresh lookup: {result:?}"),
            () = tokio::time::sleep(std::time::Duration::from_secs(5)) => {
                panic!("The refresh authority lookup was not reached")
            },
        }
        if disconnect {
            assert_eq!(
                common::send(
                    &app,
                    request("DELETE", &path, Value::Null, Some("fixture-owner"))
                )
                .await
                .status(),
                StatusCode::OK
            );
        }
        release.notify_one();
        let result = pending.await.unwrap();
        if disconnect {
            assert!(result.is_err(), "A disconnected personal request succeeded");
            assert_eq!(
                refreshes.load(Ordering::SeqCst),
                0,
                "A disconnected personal refresh token reached the provider"
            );
            assert_eq!(reads.load(Ordering::SeqCst), 0);
        } else {
            assert_eq!(result.unwrap().unwrap()["actor"], "U111");
            assert_eq!(
                refreshes.load(Ordering::SeqCst),
                1,
                "The positive control did not exercise renewal"
            );
            assert_eq!(reads.load(Ordering::SeqCst), 1);
        }
        server.abort();
        authority_server.abort();
    }
}

#[tokio::test]
async fn mcp_editing_a_queued_message_does_not_inherit_its_authors_personal_oauth() {
    for edited in [false, true] {
        let (_root, s, instance, resource, path, app) = configured_fixture(false, true).await;
        let calls = Arc::new(AtomicUsize::new(0));
        let reading = calls.clone();
        let provider = axum::Router::new()
            .route(
                "/token",
                axum::routing::post(|| async {
                    axum::Json(json!({
                        "access_token": "personal-mcp-edit-fixture",
                        "scope": "messages:read",
                        "team": {"id": "T111"},
                        "authed_user": {"id": "U111"}
                    }))
                }),
            )
            .route(
                "/messages",
                axum::routing::get(move |headers: axum::http::HeaderMap| {
                    let calls = reading.clone();
                    async move {
                        assert_eq!(headers["authorization"], "Bearer personal-mcp-edit-fixture");
                        calls.fetch_add(1, Ordering::SeqCst);
                        axum::Json(json!({"actor": "U111"}))
                    }
                }),
            );
        let (origin, server) = common::serve_locally(provider).await;
        s.plugins.route_http_to(Some(origin));
        let started = begin(&app, &path).await;
        let callback = format!(
            "/api/plugin-oauth/{instance}/callback?state={}&code=fixture",
            text(&started, "state")
        );
        assert_eq!(
            common::send(&app, request("GET", &callback, Value::Null, None))
                .await
                .status(),
            StatusCode::OK
        );
        assert_eq!(
            common::send(
                &app,
                request(
                    "POST",
                    &format!("{path}/finish"),
                    json!({"state": started["state"]}),
                    Some("fixture-owner")
                )
            )
            .await
            .status(),
            StatusCode::OK
        );
        let grants = format!(
            "/api/plugins/example/connections/{instance}/resources/workspace/{resource}/grants"
        );
        assert_eq!(
            common::send(
                &app,
                request(
                    "POST",
                    &grants,
                    json!({"agentId": MAIN_AGENT_ID, "profile": "read"}),
                    Some("fixture-owner")
                )
            )
            .await
            .status(),
            StatusCode::OK
        );
        let chat = s
            .chat_create(json!({"agentId": MAIN_AGENT_ID}))
            .await
            .unwrap();
        let message = cairn_installation::config::id();
        assert_eq!(
            common::send(
                &app,
                request(
                    "POST",
                    &format!("/api/chats/{}/messages", text(&chat, "id")),
                    json!({"id": message, "text": "The person's original request"}),
                    Some("fixture-owner")
                )
            )
            .await
            .status(),
            StatusCode::OK
        );
        if edited {
            let response = mcp_management_call(
                &s,
                "edit_chat_message",
                json!({
                    "chatId": chat["id"],
                    "messageId": message,
                    "message": {"text": "Changed through owner-authorized MCP"}
                }),
            )
            .await;
            assert_ne!(response["result"]["isError"], true, "{response}");
            assert_eq!(
                response["result"]["structuredContent"]["result"]["text"],
                "Changed through owner-authorized MCP"
            );
        }
        s.chat_tick(&HashSet::new()).await.unwrap();
        let detail = s.chat_detail(text(&chat, "id")).await.unwrap();
        let run = s
            .store
            .patch_run(
                text(&detail["run"], "id"),
                json!({"status": "running", "isolated": true}),
            )
            .await
            .unwrap();
        let config = s.mcps.run_configuration(&s, &run).await.unwrap();
        let result = agents::call_tool(
            &s,
            text(&config["env"], "CAIRN_MCP_RUN_TOKEN"),
            "example_read",
            &json!({"resource": resource}),
        )
        .await;
        server.abort();
        if edited {
            assert!(
                result.is_err(),
                "The MCP-edited request retained its original author's OAuth"
            );
            assert_eq!(calls.load(Ordering::SeqCst), 0);
        } else {
            assert_eq!(result.unwrap().unwrap()["actor"], "U111");
            assert_eq!(
                calls.load(Ordering::SeqCst),
                1,
                "The positive control did not read as the author"
            );
            let steering = cairn_installation::config::id();
            let denied = mcp_management_call(
                &s,
                "send_chat_message",
                json!({
                    "chatId": chat["id"],
                    "message": {
                        "id": steering,
                        "text": "Unattributed MCP steering",
                        "mode": "steer"
                    }
                }),
            )
            .await;
            assert_eq!(denied["result"]["isError"], true, "{denied}");
            assert!(
                s.chat_detail(text(&chat, "id")).await.unwrap()["messages"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .all(|message| message["id"] != steering)
            );
        }
    }
}

async fn mcp_management_call(s: &Arc<Service>, name: &str, args: Value) -> Value {
    let token = common::relay_fixture::grant(s, &["read", "manage", "run"]).await;
    let endpoint = common::relay_fixture::router(s.clone()).await.unwrap();
    let rpc = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": common::mcp_arguments(name, &args)
    });
    let request = common::request("POST", "/mcp")
        .header("authorization", format!("Bearer {token}"))
        .body(axum::body::Body::from(rpc.to_string()))
        .unwrap();
    let response = common::send(&endpoint, request).await;
    assert_eq!(response.status(), StatusCode::OK);
    common::read_json(response).await
}

#[tokio::test]
async fn authenticated_chat_mutations_survive_personal_authority_unavailability() {
    let (_root, s, instance, resource, _path, app) = configured_fixture(false, true).await;
    let grant_path = format!(
        "/api/plugins/example/connections/{instance}/resources/workspace/{resource}/grants"
    );
    assert_eq!(
        common::send(
            &app,
            request(
                "POST",
                &grant_path,
                json!({"agentId": MAIN_AGENT_ID, "profile": "read"}),
                Some("fixture-owner")
            )
        )
        .await
        .status(),
        StatusCode::OK
    );
    let original = s
        .chat_create(json!({"agentId": MAIN_AGENT_ID}))
        .await
        .unwrap();
    let original_id = cairn_installation::config::id();
    let original_path = format!("/api/chats/{}/messages", text(&original, "id"));
    assert_eq!(
        common::send(
            &app,
            request(
                "POST",
                &original_path,
                json!({"id": original_id, "text": "Before outage"}),
                Some("fixture-owner")
            )
        )
        .await
        .status(),
        StatusCode::OK
    );
    let authority = axum::Router::new().route(
        "/api/relay/00000000-0000-4000-8000-000000000055/task-authors",
        axum::routing::get(|| async { StatusCode::SERVICE_UNAVAILABLE }),
    );
    let (origin, server) = common::serve_locally(authority).await;
    let identity_path = s.config.data_dir.join("installation-relay/identity.json");
    let original_identity = std::fs::read(&identity_path).unwrap();
    let mut identity: Value = serde_json::from_slice(&original_identity).unwrap();
    identity["origin"] = json!(origin);
    std::fs::write(&identity_path, serde_json::to_vec(&identity).unwrap()).unwrap();
    let chat = s
        .chat_create(json!({"agentId": MAIN_AGENT_ID}))
        .await
        .unwrap();
    let message = cairn_installation::config::id();
    let path = format!("/api/chats/{}/messages", text(&chat, "id"));
    // The authenticated HTTP context is exactly what an unexpired direct lease
    // supplies. This tests the native dispatch seam, not a WebRTC handshake.
    let sent = common::send(
        &app,
        request(
            "POST",
            &path,
            json!({"id": message, "text": "Ordinary request during outage"}),
            Some("fixture-owner"),
        ),
    )
    .await;
    assert_eq!(
        sent.status(),
        StatusCode::OK,
        "{}",
        common::read_text(sent).await
    );
    let edited = common::send(
        &app,
        request(
            "PUT",
            &format!("{path}/{message}"),
            json!({"text": "Edited while authority unavailable"}),
            Some("fixture-owner"),
        ),
    )
    .await;
    assert_eq!(
        edited.status(),
        StatusCode::OK,
        "{}",
        common::read_text(edited).await
    );
    let key = format!("plugin-message-requester:{}:{message}", text(&chat, "id"));
    let principal = s.store.read(move |db| db.kv(&key)).await.unwrap().unwrap();
    assert_eq!(principal["account"], "fixture-owner");
    assert!(
        principal["accessId"].is_null(),
        "An unavailable proof must not acquire personal credentials"
    );
    s.chat_tick(&HashSet::new()).await.unwrap();
    let ordinary = s.chat_detail(text(&chat, "id")).await.unwrap();
    let ordinary_run = s
        .store
        .patch_run(
            text(&ordinary["run"], "id"),
            json!({"status": "running", "isolated": true}),
        )
        .await
        .unwrap();
    let foreign = common::send(
        &app,
        member_request(
            "POST",
            &path,
            json!({
                "id": cairn_installation::config::id(),
                "text": "Different unverified account", "mode": "steer",
            }),
            "another-member",
        ),
    )
    .await;
    assert_eq!(foreign.status(), StatusCode::CONFLICT);
    let same = common::send(
        &app,
        request(
            "POST",
            &path,
            json!({
                "id": cairn_installation::config::id(),
                "text": "Same authenticated account", "mode": "steer",
            }),
            Some("fixture-owner"),
        ),
    )
    .await;
    assert_eq!(same.status(), StatusCode::OK);
    let detail = s.chat_detail(text(&original, "id")).await.unwrap();
    let run = s
        .store
        .patch_run(
            text(&detail["run"], "id"),
            json!({"status": "running", "isolated": true}),
        )
        .await
        .unwrap();
    let question = "b".repeat(64);
    s.question_receive(
        text(&run, "id"),
        json!({
            "id": question, "blocking": false,
            "fields": [{"id": "choice", "title": "Continue?", "options": []}],
        }),
    )
    .await
    .unwrap();
    let answer = common::send(
        &app,
        request(
            "POST",
            &format!(
                "/api/chats/{}/questions/{question}/answer",
                text(&original, "id")
            ),
            json!({"id": cairn_installation::config::id(), "answers": {"choice": ["Yes"]}}),
            Some("fixture-owner"),
        ),
    )
    .await;
    assert_eq!(
        answer.status(),
        StatusCode::OK,
        "{}",
        common::read_text(answer).await
    );
    let suspended = format!(
        "plugin-requester-suspended:{}:{}:{original_id}",
        text(&original, "id"),
        text(&run, "id")
    );
    assert_eq!(
        s.store.read(move |db| db.kv(&suspended)).await.unwrap(),
        Some(json!(true))
    );
    std::fs::write(&identity_path, original_identity).unwrap();
    let configuration = s.mcps.run_configuration(&s, &ordinary_run).await.unwrap();
    let bearer = text(&configuration["env"], "CAIRN_MCP_RUN_TOKEN");
    let error = agents::call_tool(&s, bearer, "example_read", &json!({"resource": resource}))
        .await
        .expect_err("The original input gained personal proof after Beacon recovered");
    assert!(
        error
            .message
            .contains("Personal authority was unavailable when this turn was admitted"),
        "{error:?}"
    );
    let recovered_id = cairn_installation::config::id();
    let recovered = common::send(
        &app,
        request(
            "POST",
            &path,
            json!({
                "id": recovered_id,
                "text": "Continue without upgrading this turn's permissions",
                "mode": "steer",
            }),
            Some("fixture-owner"),
        ),
    )
    .await;
    assert_eq!(
        recovered.status(),
        StatusCode::OK,
        "Restored proof prevented the same account from controlling its ordinary turn"
    );
    s.chat_tick(&HashSet::new()).await.unwrap();
    let file = s
        .config
        .data_dir
        .join("runs")
        .join(text(&ordinary_run, "id"))
        .join("chat-input/messages.json");
    let published: Value = serde_json::from_slice(&std::fs::read(file).unwrap()).unwrap();
    assert!(
        published
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["id"] == recovered_id)
    );
    let still_unproved =
        agents::call_tool(&s, bearer, "example_read", &json!({"resource": resource}))
            .await
            .expect_err("Steering upgraded the original turn's personal proof");
    assert!(
        still_unproved
            .message
            .contains("Personal authority was unavailable when this turn was admitted")
    );
    server.abort();
}

#[tokio::test]
async fn oauth_completion_rechecks_the_final_owner_and_personal_membership_proof() {
    for kind in [
        "owner",
        "setup",
        "personal-owner",
        "member-removed",
        "member-reinvited",
        "owner-files",
        "setup-files",
        "personal-files",
    ] {
        for changed in [false, true] {
            let personal = !kind.starts_with("owner") && !kind.starts_with("setup");
            let (root, s, instance, _resource, path, app) =
                configured_fixture(false, personal).await;
            let (_root, instance, path, setup_revision) = if kind.starts_with("setup") {
                let (draft, _resource, path) = create_setup(&app).await;
                (
                    root,
                    text(&draft, "id").to_owned(),
                    path,
                    Some(draft["revision"].clone()),
                )
            } else {
                (root, instance, path, None)
            };
            let returned = Arc::new(AtomicBool::new(false));
            let checks = Arc::new(AtomicUsize::new(0));
            // Initiator validation uses one owner lookup or an owner+member pair.
            // The next reply is the final policy snapshot used by the commit.
            let final_check = if personal { 3 } else { 2 };
            let (responded, policy_checks) = (returned.clone(), checks.clone());
            let source = s.config.data_dir.join("plugins/example/1/main.luau");
            let original_source = std::fs::read_to_string(&source).unwrap();
            let edited_source = source.clone();
            let authority = axum::Router::new().route(
                "/api/relay/00000000-0000-4000-8000-000000000055/task-authors",
                axum::routing::get(move || {
                    let post_provider = responded.load(Ordering::SeqCst);
                    let check = if post_provider {
                        policy_checks.fetch_add(1, Ordering::SeqCst) + 1
                    } else {
                        0
                    };
                    let retired = changed && check >= final_check && !kind.ends_with("files");
                    if changed && check == final_check && kind.ends_with("files") {
                        let content = std::fs::read_to_string(&edited_source).unwrap();
                        std::fs::write(
                            &edited_source,
                            format!("{content}\n-- late package edit\n"),
                        )
                        .unwrap();
                    }
                    let owner_access = if retired && !kind.starts_with("member-") {
                        "replacement-owner-access"
                    } else {
                        "fixture-owner"
                    };
                    let member_access = if retired && kind == "member-reinvited" {
                        "alice-reinvited"
                    } else {
                        "alice-original"
                    };
                    let members = if retired && kind == "member-removed" {
                        json!([])
                    } else {
                        json!([{"account_id": "alice", "access_id": member_access}])
                    };

                    async move {
                        axum::Json(json!({
                            "owner": {
                                "account_id": "fixture-owner",
                                "access_id": owner_access,
                            },
                            "members": members,
                        }))
                    }
                }),
            );
            let (origin, authority_server) = common::serve_locally(authority).await;
            let identity_path = s.config.data_dir.join("installation-relay/identity.json");
            let mut identity: Value =
                serde_json::from_slice(&std::fs::read(&identity_path).unwrap()).unwrap();
            identity["origin"] = json!(origin);
            std::fs::write(&identity_path, serde_json::to_vec(&identity).unwrap()).unwrap();
            let account = if personal { "alice" } else { "fixture-owner" };
            let own_request = |method: &str, path: &str, body: Value| {
                if personal {
                    member_request(method, path, body, account)
                } else {
                    request(method, path, body, Some(account))
                }
            };
            let mut input = json!({"scopes": ["messages:read"]});
            if let Some(revision) = setup_revision {
                input["setupRevision"] = revision;
            }
            let response = common::send(&app, own_request("POST", &path, input)).await;
            assert_eq!(
                response.status(),
                StatusCode::OK,
                "{}",
                common::read_text(response).await
            );
            let started = common::read_json(response).await;
            let item = path.strip_suffix("/oauth/bot").unwrap();
            let response = common::send(
                &app,
                request("GET", item, Value::Null, Some("fixture-owner")),
            )
            .await;
            assert_eq!(response.status(), StatusCode::OK);
            let before = common::read_json(response).await;
            let response_ready = returned.clone();
            let provider = axum::Router::new().route(
                "/token",
                axum::routing::post(move || {
                    response_ready.store(true, Ordering::SeqCst);
                    async {
                        axum::Json(json!({
                            "access_token": "final-consent-token-fixture",
                            "scope": "messages:read",
                            "team": {"id": "T111"},
                            "authed_user": {"id": "U111"},
                        }))
                    }
                }),
            );
            let (origin, server) = common::serve_locally(provider).await;
            s.plugins.route_http_to(Some(origin));
            let callback = format!(
                "/api/plugin-oauth/{instance}/callback?state={}&code=final-proof",
                text(&started, "state")
            );
            assert_eq!(
                common::send(&app, request("GET", &callback, Value::Null, None))
                    .await
                    .status(),
                StatusCode::OK
            );
            let response = common::send(
                &app,
                own_request(
                    "POST",
                    &format!("{path}/finish"),
                    json!({"state": started["state"]}),
                ),
            )
            .await;
            assert_eq!(
                checks.load(Ordering::SeqCst),
                final_check,
                "{kind} did not exercise its final policy lookup"
            );
            assert_eq!(
                response.status(),
                if changed && kind.ends_with("files") {
                    StatusCode::FORBIDDEN
                } else if changed {
                    StatusCode::CONFLICT
                } else {
                    StatusCode::OK
                },
                "Late {kind} revocation (changed={changed}) was ignored: {}",
                common::read_text(response).await
            );
            if !changed {
                assert_eq!(common::read_json(response).await["status"], "authorized");
            }
            // Restore authority for reads: a refused completion must remain absent
            // even if the same account becomes available again immediately.
            returned.store(false, Ordering::SeqCst);
            std::fs::write(&source, original_source).unwrap();
            if changed {
                let response = common::send(&app, own_request("GET", &path, Value::Null)).await;
                assert_eq!(
                    response.status(),
                    StatusCode::OK,
                    "{}",
                    common::read_text(response).await
                );
                assert_eq!(common::read_json(response).await["status"], "disconnected");
                let response = common::send(
                    &app,
                    request("GET", item, Value::Null, Some("fixture-owner")),
                )
                .await;
                assert_eq!(response.status(), StatusCode::OK);
                let after = common::read_json(response).await;
                assert_eq!(
                    after["revision"], before["revision"],
                    "{kind} changed resource revision after refusal"
                );
                assert_eq!(
                    after["settings"], before["settings"],
                    "{kind} changed resource metadata after refusal"
                );
                assert!(
                    s.store
                        .keys("plugin-oauth-subject:")
                        .await
                        .unwrap()
                        .is_empty()
                );
            }
            server.abort();
            authority_server.abort();
        }
    }
}

#[tokio::test]
async fn resource_changes_during_post_response_oauth_verification_prevent_result_delivery() {
    for change in ["none", "pause", "edit"] {
        let (_root, s, instance, resource, path, app) = fixture(false).await;
        let responded = Arc::new(AtomicBool::new(false));
        let responding = responded.clone();
        let provider = axum::Router::new()
            .route(
                "/token",
                axum::routing::post(|| async {
                    axum::Json(json!({
                        "access_token": "post-response-resource-fixture",
                        "scope": "messages:read",
                        "team": {"id": "T111"},
                    }))
                }),
            )
            .route(
                "/messages",
                axum::routing::get(move |headers: axum::http::HeaderMap| {
                    let responded = responding.clone();
                    async move {
                        assert_eq!(
                            headers["authorization"],
                            "Bearer post-response-resource-fixture"
                        );
                        responded.store(true, Ordering::SeqCst);
                        axum::Json(json!({"ok": true}))
                    }
                }),
            );
        let (origin, provider_server) = common::serve_locally(provider).await;
        s.plugins.route_http_to(Some(origin));
        let started = begin(&app, &path).await;
        let callback = format!(
            "/api/plugin-oauth/{instance}/callback?state={}&code=post-response-code",
            text(&started, "state")
        );
        assert_eq!(
            common::send(&app, request("GET", &callback, Value::Null, None))
                .await
                .status(),
            StatusCode::OK
        );
        let response = common::send(
            &app,
            request(
                "POST",
                &format!("{path}/finish"),
                json!({"state": started["state"]}),
                Some("fixture-owner"),
            ),
        )
        .await;
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "{}",
            common::read_text(response).await
        );

        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let first = Arc::new(AtomicBool::new(true));
        let (entering, releasing) = (entered.clone(), release.clone());
        let authority = axum::Router::new().route(
            "/api/relay/00000000-0000-4000-8000-000000000055/task-authors",
            axum::routing::get(move || {
                let hold = responded.load(Ordering::SeqCst) && first.swap(false, Ordering::SeqCst);
                let (entered, release) = (entering.clone(), releasing.clone());
                async move {
                    if hold {
                        entered.notify_one();
                        release.notified().await;
                    }
                    axum::Json(json!({
                        "owner": {"account_id": "fixture-owner", "access_id": "fixture-owner"},
                        "members": [],
                    }))
                }
            }),
        );
        let (origin, authority_server) = common::serve_locally(authority).await;
        let identity_path = s.config.data_dir.join("installation-relay/identity.json");
        let mut identity: Value =
            serde_json::from_slice(&std::fs::read(&identity_path).unwrap()).unwrap();
        identity["origin"] = json!(origin);
        std::fs::write(identity_path, serde_json::to_vec(&identity).unwrap()).unwrap();
        let (calling, called_instance, called_resource) =
            (s.clone(), instance.clone(), resource.clone());
        let mut pending = tokio::spawn(async move {
            panels::action(
                &calling,
                "example",
                &called_instance,
                &json!({
                    "action": "read", "payload": {"resource": called_resource},
                }),
            )
            .await
        });
        tokio::select! {
            () = entered.notified() => {},
            result = &mut pending => panic!("Ended before the post-response authority lookup: {result:?}"),
            () = tokio::time::sleep(std::time::Duration::from_secs(10)) => panic!("Post-response lookup was not reached"),
        }
        if change != "none" {
            let current = resources::get(&s, "example", &instance, "workspace", &resource)
                .await
                .unwrap();
            resources::save(
                &s,
                "example",
                &instance,
                "workspace",
                &json!({
                    "name": if change == "edit" { "Changed workspace" } else { &current.name },
                    "enabled": change != "pause",
                    "revision": current.revision,
                    "settings": current.settings,
                }),
                Some(&resource),
            )
            .await
            .unwrap();
        }
        release.notify_one();
        let result = pending.await.unwrap();
        if change == "none" {
            assert_eq!(result.unwrap()["notice"], "Authorized");
        } else {
            assert_eq!(result.unwrap_err().status, 403, "{change}");
        }
        provider_server.abort();
        authority_server.abort();
    }
}
