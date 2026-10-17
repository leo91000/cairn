mod common;

use axum::{http::StatusCode, response::IntoResponse};
use cairn_installation::{
    auth::{InstallationIdentity, InstallationRole},
    config::MAIN_AGENT_ID,
    plugins::{agents, connections, panels, registry, resources},
    service::Service,
    validation::text,
};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
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
    let source = r#"local function read(resource)
        return cairn.http.request({url = 'https://api.example.test/messages',
            auth = {resource = resource, oauth = 'bot'}})
    end
    return {action = function(event)
        local response = read(event.payload.resource)
        return {notice = response.json.ok and 'Authorized' or 'Denied'}
    end, tool = function(event) return read(event.input.resource).json end}"#;
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
        &json!({"name": "Engineering"}),
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
async fn concurrent_calls_share_one_refresh_and_reuse_only_the_rotated_token() {
    let (_root, s, instance, resource, path, app) = fixture(false).await;
    let started = begin(&app, &path).await;
    let exchanges = Arc::new(AtomicUsize::new(0));
    let exchange_count = exchanges.clone();
    let provider = axum::Router::new().route("/token", axum::routing::post(move |axum::Form(form): axum::Form<HashMap<String, String>>| {
        let count = exchange_count.clone();
        async move {
            let call = count.fetch_add(1, Ordering::SeqCst);
            if call == 0 {
                assert_eq!(form["grant_type"], "authorization_code");
                axum::Json(json!({"access_token": "expiring-fixture", "refresh_token": "refresh-0-fixture", "expires_in": 1,
                    "scope": "messages:read", "team": {"id": "T111"}}))
            } else {
                assert_eq!(call, 1, "redeemed a refresh token more than once");
                assert_eq!(form["grant_type"], "refresh_token");
                assert_eq!(form["refresh_token"], "refresh-0-fixture");
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                axum::Json(json!({"access_token": "renewed-fixture", "refresh_token": "refresh-1-fixture", "expires_in": 43200, "scope": "messages:read"}))
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
    let (first, second) = tokio::join!(
        panels::action(&s, "example", &instance, &input),
        panels::action(&s, "example", &instance, &input)
    );
    assert_eq!(first.unwrap()["notice"], "Authorized");
    assert_eq!(second.unwrap()["notice"], "Authorized");
    assert_eq!(exchanges.load(Ordering::SeqCst), 2);
    server.abort();
}

#[tokio::test]
async fn disconnect_during_code_exchange_prevents_authorization_from_being_restored() {
    let (_root, s, instance, _resource, path, app) = fixture(false).await;
    let started = begin(&app, &path).await;
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let entering = entered.clone();
    let releasing = release.clone();
    let provider = axum::Router::new().route("/token", axum::routing::post(move || {
        let entered = entering.clone(); let release = releasing.clone();
        async move {
            entered.notify_one(); release.notified().await;
            axum::Json(json!({"access_token": "revoked-fixture", "scope": "messages:read", "team": {"id": "T111"}}))
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
async fn revoking_an_agent_during_oauth_refresh_prevents_the_following_resource_request() {
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
    release.notify_one();
    assert!(pending.await.unwrap().is_err());
    assert_eq!(
        reads.load(Ordering::SeqCst),
        0,
        "sent a new resource request after the agent's grant was revoked"
    );
    server.abort();
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
