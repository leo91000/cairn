//! The owner UI and both management tools share resource authorization lifecycle.
use super::*;

#[path = "../common/management.rs"]
mod access;

use access::{active_run, call, enabled_main, public_call, result};

#[path = "management/inert.rs"]
mod inert;

async fn connect(app: &axum::Router, path: &str, instance: &str) {
    let started = begin(app, path).await;
    let callback = format!(
        "/api/plugin-oauth/{instance}/callback?state={}&code=management-consent",
        text(&started, "state")
    );
    let response = common::send(app, request("GET", &callback, Value::Null, None)).await;
    assert_eq!(response.status(), StatusCode::OK);
    let response = common::send(
        app,
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
    assert_eq!(common::read_json(response).await["status"], "authorized");
}

async fn http_status(app: &axum::Router, path: &str) -> Value {
    let response = common::send(
        app,
        request("GET", path, Value::Null, Some("fixture-owner")),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    common::read_json(response).await
}

#[tokio::test]
async fn owner_http_resource_consent_is_the_lifecycle_reference() {
    resource_consent_lifecycle("http").await;
}

#[tokio::test]
async fn public_mcp_can_complete_the_resource_consent_lifecycle() {
    resource_consent_lifecycle("public").await;
}

#[tokio::test]
async fn installation_tool_can_complete_the_resource_consent_lifecycle() {
    resource_consent_lifecycle("installation").await;
}

#[tokio::test]
async fn an_unchanged_native_grant_can_finish_a_resource_consent_exchange() {
    resource_consent_during_revocation(false).await;
}

#[tokio::test]
async fn a_resource_consent_exchange_cannot_publish_after_its_native_grant_is_revoked() {
    resource_consent_during_revocation(true).await;
}

async fn resource_consent_during_revocation(revoke: bool) {
    let (_root, service, instance, resource, path, app) = fixture(false).await;
    let main = enabled_main(&service).await;
    connections::save(
        &service,
        "example",
        &json!({
            "name": "Example app",
            "enabled": true,
            "settings": {"clientId": "client-fixture"},
            "agentIds": [MAIN_AGENT_ID],
        }),
        Some(&instance),
    )
    .await
    .unwrap();
    let (run, configuration) = active_run(&service, &main).await;
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let exchanges = Arc::new(AtomicUsize::new(0));
    let (entering, releasing, calls) = (entered.clone(), release.clone(), exchanges.clone());
    let provider = axum::Router::new().route(
        "/token",
        axum::routing::post(move || {
            let (entered, release, calls) = (entering.clone(), releasing.clone(), calls.clone());
            async move {
                calls.fetch_add(1, Ordering::SeqCst);
                entered.notify_one();
                release.notified().await;
                axum::Json(json!({
                    "access_token": "fixture-delayed-private-access",
                    "refresh_token": "fixture-delayed-private-refresh",
                    "scope": "messages:read",
                    "expires_in": 3600,
                    "team": {"id": "T111"},
                }))
            }
        }),
    );
    let (origin, server) = common::serve_locally(provider).await;
    service.plugins.route_http_to(Some(origin));
    let started = begin(&app, &path).await;
    let callback = format!(
        "/api/plugin-oauth/{instance}/callback?state={}&code=delayed-fixture",
        text(&started, "state")
    );
    let response = common::send(&app, request("GET", &callback, Value::Null, None)).await;
    assert_eq!(response.status(), StatusCode::OK);
    let args = json!({
        "pluginId": "example",
        "instanceId": instance,
        "collection": "workspace",
        "resourceId": resource,
        "provider": "bot",
        "action": "finish",
        "data": {"state": started["state"]},
    });
    let routed = service.clone();
    let pending =
        tokio::spawn(
            async move { call(&routed, &configuration, "manage_plugin_oauth", args).await },
        );
    tokio::time::timeout(std::time::Duration::from_secs(5), entered.notified())
        .await
        .expect("The native management call must reach the actual token endpoint");
    if revoke {
        service
            .mcps
            .revoke_run(&service, text(&run, "id"))
            .await
            .unwrap();
    }
    release.notify_one();
    let response = pending.await.unwrap();
    if revoke {
        assert_eq!(response["result"]["isError"], true);
    } else {
        assert_eq!(result(&response)["status"], "authorized");
    }
    assert!(!response.to_string().contains("fixture-delayed-private"));
    assert_eq!(
        http_status(&app, &path).await["status"],
        if revoke { "disconnected" } else { "authorized" }
    );
    // The exchange consumed its code before provider I/O; refusal must not make
    // that uncertain code reusable by the owner or a subsequent management run.
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
    assert_ne!(response.status(), StatusCode::OK);
    assert_eq!(exchanges.load(Ordering::SeqCst), 1);
    server.abort();
}

async fn resource_consent_lifecycle(source: &str) {
    let (_root, service, instance, resource, path, app) = fixture(false).await;
    let main = enabled_main(&service).await;
    connections::save(
        &service,
        "example",
        &json!({
            "name": "Example app",
            "enabled": true,
            "settings": {"clientId": "client-fixture"},
            "agentIds": [MAIN_AGENT_ID],
        }),
        Some(&instance),
    )
    .await
    .unwrap();
    let (_, configuration) = active_run(&service, &main).await;
    let exchanges = Arc::new(AtomicUsize::new(0));
    let calls = exchanges.clone();
    let provider = axum::Router::new().route(
        "/token",
        axum::routing::post(move || {
            let calls = calls.clone();
            async move {
                calls.fetch_add(1, Ordering::SeqCst);
                axum::Json(json!({
                    "access_token": "fixture-managed-consent-private-access",
                    "refresh_token": "fixture-managed-consent-private-refresh",
                    "scope": "messages:read",
                    "expires_in": 3600,
                    "team": {"id": "T111"},
                }))
            }
        }),
    );
    let (origin, server) = common::serve_locally(provider).await;
    service.plugins.route_http_to(Some(origin));
    let args = json!({
        "pluginId": "example",
        "instanceId": instance,
        "collection": "workspace",
        "resourceId": resource,
        "provider": "bot",
        "action": "start",
        "data": {"scopes": ["messages:read"]},
    });
    let response =
        configured_action(&service, &app, &path, &configuration, source, args.clone()).await;
    assert_eq!(response.0, StatusCode::OK);
    let started = if source == "http" {
        &response.1
    } else {
        result(&response.1)
    };
    let url = url::Url::parse(text(started, "url")).unwrap();
    assert_eq!(
        url.origin().ascii_serialization(),
        "https://api.example.test"
    );
    assert_eq!(url.path(), "/authorize");
    assert!(
        url.query_pairs()
            .any(|(key, value)| key == "scope" && value == "messages:read")
    );
    let state = text(started, "state");
    let mut finish = args;
    finish["action"] = json!("finish");
    finish["data"] = json!({"state": state});
    let response = configured_action(
        &service,
        &app,
        &path,
        &configuration,
        source,
        finish.clone(),
    )
    .await;
    assert_eq!(response.0, StatusCode::OK);
    let pending = if source == "http" {
        &response.1
    } else {
        result(&response.1)
    };
    assert_eq!(pending["status"], "pending");
    assert_eq!(exchanges.load(Ordering::SeqCst), 0);

    let callback =
        format!("/api/plugin-oauth/{instance}/callback?state={state}&code=managed-fixture");
    let response = common::send(&app, request("GET", &callback, Value::Null, None)).await;
    assert_eq!(response.status(), StatusCode::OK);
    let response = configured_action(
        &service,
        &app,
        &path,
        &configuration,
        source,
        finish.clone(),
    )
    .await;
    assert_eq!(response.0, StatusCode::OK);
    let finished = if source == "http" {
        &response.1
    } else {
        result(&response.1)
    };
    assert_eq!(finished["status"], "authorized");
    assert_eq!(finished["scopes"], json!(["messages:read"]));
    assert!(
        !finished
            .to_string()
            .contains("fixture-managed-consent-private")
    );
    assert!(!finished.to_string().contains("client-secret-fixture"));
    assert_eq!(http_status(&app, &path).await, *finished);
    let repeated = configured_action(&service, &app, &path, &configuration, source, finish).await;
    if source == "http" {
        assert_ne!(repeated.0, StatusCode::OK);
    } else {
        assert_eq!(repeated.1["result"]["isError"], true);
    }
    assert_eq!(exchanges.load(Ordering::SeqCst), 1);
    server.abort();
}

#[tokio::test]
async fn resource_authorization_status_and_revoke_match_owner_http_and_both_tools() {
    let (_root, s, instance, resource, path, app) = fixture(false).await;
    let main = enabled_main(&s).await;
    // Only the owner grants this private instance. Installation management cannot
    // manufacture the grant required to disconnect its saved authorization.
    connections::save(
        &s,
        "example",
        &json!({
            "name": "Example app", "enabled": true, "settings": {"clientId": "client-fixture"},
            "agentIds": [MAIN_AGENT_ID],
        }),
        Some(&instance),
    )
    .await
    .unwrap();
    let (_, configuration) = active_run(&s, &main).await;
    let provider = axum::Router::new().route("/token", axum::routing::post(|| async {
        axum::Json(json!({
            "access_token": "management-private-access", "refresh_token": "management-private-refresh",
            "scope": "messages:read", "expires_in": 3600, "team": {"id": "T111"},
        }))
    }));
    let (origin, server) = common::serve_locally(provider).await;
    s.plugins.route_http_to(Some(origin));
    for source in ["http", "public", "installation"] {
        connect(&app, &path, &instance).await;
        let expected = http_status(&app, &path).await;
        assert_eq!(expected["status"], "authorized");
        assert_eq!(expected["scopes"], json!(["messages:read"]));
        let args = json!({
            "pluginId": "example", "instanceId": instance, "collection": "workspace",
            "resourceId": resource, "provider": "bot", "action": "status",
        });
        let status = match source {
            "http" => expected.clone(),
            "public" => {
                result(&public_call(&s, &["manage"], "manage_plugin_oauth", args.clone()).await)
                    .clone()
            }
            _ => {
                result(&call(&s, &configuration, "manage_plugin_oauth", args.clone()).await).clone()
            }
        };
        assert_eq!(
            status, expected,
            "{source} returned a different authorization status"
        );
        let encoded = status.to_string();
        assert!(!encoded.contains("management-private"));
        assert!(!encoded.contains("client-secret-fixture"));
        let mut revoke = args;
        revoke["action"] = json!("revoke");
        let revoked = match source {
            "http" => {
                let response = common::send(
                    &app,
                    request("DELETE", &path, Value::Null, Some("fixture-owner")),
                )
                .await;
                assert_eq!(response.status(), StatusCode::OK);
                common::read_json(response).await
            }
            "public" => {
                result(&public_call(&s, &["manage"], "manage_plugin_oauth", revoke).await).clone()
            }
            _ => result(&call(&s, &configuration, "manage_plugin_oauth", revoke).await).clone(),
        };
        assert_eq!(revoked, json!({"status": "disconnected"}));
        assert_eq!(http_status(&app, &path).await, revoked);
    }
    server.abort();
}

#[tokio::test]
async fn authorization_management_cannot_select_a_missing_or_foreign_resource() {
    let root = TempDir::new().unwrap();
    let s = access::service(&root).await;
    s.plugins
        .use_worker(std::path::PathBuf::from(env!("CARGO_BIN_EXE_cairn")));
    let main = enabled_main(&s).await;
    let (_, configuration) = active_run(&s, &main).await;
    let package = registry::install(&s, &json!({
        "manifest": {
            "id": "managed-oauth", "name": "Managed OAuth", "version": "1", "apiVersion": 2,
            "capabilities": ["resources", "oauth"], "hosts": ["api.example.test"],
            "settings": [{"key": "clientId", "title": "Client", "type": "string"}],
            "resources": [{"name": "workspace", "title": "Workspaces", "singular": "Workspace"}],
            "oauth": [{"name": "bot", "title": "Connect", "collection": "workspace",
                "authorizationUrl": "https://api.example.test/authorize", "tokenUrl": "https://api.example.test/token",
                "clientId": "clientId", "pkce": true, "scopes": ["messages:read"],
                "token": {"accessToken": "/access_token"}}],
        }, "source": "return {}",
    })).await.unwrap();
    registry::approve(&s, "managed-oauth", "1", text(&package, "digest"))
        .await
        .unwrap();
    let instance = connections::save(
        &s,
        "managed-oauth",
        &json!({"name": "Selected", "enabled": true, "settings": {"clientId": "client-fixture"}}),
        None,
    )
    .await
    .unwrap();
    let peer = connections::save(
        &s,
        "managed-oauth",
        &json!({"name": "Other", "enabled": true, "settings": {"clientId": "client-fixture"}}),
        None,
    )
    .await
    .unwrap();
    let foreign = resources::save(
        &s,
        "managed-oauth",
        text(&peer, "id"),
        "workspace",
        &json!({"name": "Foreign", "settings": {}}),
        None,
    )
    .await
    .unwrap();
    for resource in [
        text(&foreign, "id").to_owned(),
        cairn_installation::config::id(),
    ] {
        for source in ["public", "installation"] {
            let args = json!({
                "pluginId": "managed-oauth",
                "instanceId": instance["id"],
                "collection": "workspace",
                "resourceId": resource,
                "provider": "bot",
                "action": "status",
            });
            let response = if source == "public" {
                public_call(&s, &["manage"], "manage_plugin_oauth", args).await
            } else {
                call(&s, &configuration, "manage_plugin_oauth", args).await
            };
            assert_eq!(response["result"]["isError"], true, "{response}");
            assert!(
                response.to_string().contains("Resource not found"),
                "{response}"
            );
        }
    }
    assert_eq!(
        resources::get(
            &s,
            "managed-oauth",
            text(&peer, "id"),
            "workspace",
            text(&foreign, "id")
        )
        .await
        .unwrap()
        .revision,
        foreign["revision"]
    );
}

async fn provider(s: &Service) -> tokio::task::JoinHandle<()> {
    let provider = axum::Router::new().route(
        "/token",
        axum::routing::post(|| async {
            axum::Json(json!({
                "access_token": "management-private-access",
                "refresh_token": "management-private-refresh",
                "scope": "messages:read",
                "expires_in": 3600,
                "team": {"id": "T111"},
            }))
        }),
    );
    let (origin, server) = common::serve_locally(provider).await;
    s.plugins.route_http_to(Some(origin));
    server
}

#[tokio::test]
async fn installation_access_and_public_read_scope_cannot_disconnect_a_private_authorization() {
    let (_root, s, instance, resource, path, app) = fixture(false).await;
    let main = enabled_main(&s).await;
    let (_, configuration) = active_run(&s, &main).await;
    let server = provider(&s).await;
    connect(&app, &path, &instance).await;
    let before = http_status(&app, &path).await;
    let args = json!({
        "pluginId": "example",
        "instanceId": instance,
        "collection": "workspace",
        "resourceId": resource,
        "provider": "bot",
        "action": "status",
    });
    // Metadata is visible without reusing the private credential.
    assert_eq!(
        result(&call(&s, &configuration, "manage_plugin_oauth", args.clone()).await),
        &before
    );
    let denied = public_call(&s, &["read"], "manage_plugin_oauth", args.clone()).await;
    assert_eq!(denied["result"]["isError"], true, "{denied}");
    let mut revoke = args;
    revoke["action"] = json!("revoke");
    let denied = call(&s, &configuration, "manage_plugin_oauth", revoke).await;
    assert_eq!(denied["result"]["isError"], true, "{denied}");
    assert!(
        denied.to_string().contains("does not grant access"),
        "{denied}"
    );
    assert_eq!(http_status(&app, &path).await, before);
    server.abort();
}

#[tokio::test]
async fn management_cannot_borrow_the_owners_identity_for_personal_authorization() {
    let (_root, s, instance, resource, path, app) = configured_fixture(false, true).await;
    let main = enabled_main(&s).await;
    let (_, configuration) = active_run(&s, &main).await;
    assert_eq!(http_status(&app, &path).await["status"], "disconnected");
    for source in ["public", "installation"] {
        let args = json!({
            "pluginId": "example",
            "instanceId": instance,
            "collection": "workspace",
            "resourceId": resource,
            "provider": "bot",
            "action": "status",
        });
        let response = if source == "public" {
            public_call(&s, &["manage"], "manage_plugin_oauth", args).await
        } else {
            call(&s, &configuration, "manage_plugin_oauth", args).await
        };
        assert_eq!(response["result"]["isError"], true, "{response}");
        assert!(
            response
                .to_string()
                .contains("Personal authorization belongs to its requester"),
            "{response}"
        );
    }
}

#[tokio::test]
async fn queued_disconnect_cannot_outlive_its_original_native_grant() {
    let (_root, s, instance, resource, path, app) = fixture(false).await;
    let main = enabled_main(&s).await;
    connections::save(&s, "example", &json!({"name": "Example app", "enabled": true, "settings": {"clientId": "client-fixture"}, "agentIds": [MAIN_AGENT_ID]}), Some(&instance)).await.unwrap();
    let (_, configuration) = active_run(&s, &main).await;
    let bearer = text(&configuration["env"], "CAIRN_MCP_RUN_TOKEN").to_owned();
    let server = provider(&s).await;
    connect(&app, &path, &instance).await;
    let before = http_status(&app, &path).await;
    let (release, blocked) = std::sync::mpsc::channel();
    let (entered, ready) = tokio::sync::oneshot::channel();
    let service = s.clone();
    let key = format!(
        "mcp-grant:{}",
        cairn_installation::auth::hex_digest(&bearer)
    );
    let writer = tokio::spawn(async move {
        service
            .store
            .transaction(move |db| {
                entered.send(()).unwrap();
                blocked
                    .recv_timeout(std::time::Duration::from_secs(10))
                    .unwrap();
                db.delete(&key)
            })
            .await
            .unwrap();
    });
    ready.await.unwrap();
    let service = s.clone();
    let args = json!({
        "pluginId": "example",
        "instanceId": instance,
        "collection": "workspace",
        "resourceId": resource,
        "provider": "bot",
        "action": "revoke",
    });
    let tool = tokio::spawn(async move {
        access::request(
            &service,
            &bearer,
            "tools/call",
            common::mcp_arguments("manage_plugin_oauth", &args),
        )
        .await
    });
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    let pending = !tool.is_finished();
    release.send(()).unwrap();
    writer.await.unwrap();
    let (status, response) = tool.await.unwrap();
    assert!(pending, "The disconnect did not wait on the held writer");
    assert!(
        status == StatusCode::UNAUTHORIZED || response["result"]["isError"] == true,
        "{status}: {response}"
    );
    assert_eq!(http_status(&app, &path).await, before);
    server.abort();
}

async fn configured_action(
    s: &Arc<Service>,
    app: &axum::Router,
    path: &str,
    configuration: &Value,
    source: &str,
    args: Value,
) -> (StatusCode, Value) {
    if source == "http" {
        let (method, path) = match text(&args, "action") {
            "status" => ("GET", path.to_owned()),
            "revoke" => ("DELETE", path.to_owned()),
            "start" => ("POST", path.to_owned()),
            "finish" => ("POST", format!("{path}/finish")),
            action => panic!("Unknown fixture authorization action: {action}"),
        };
        let body = args.get("data").cloned().unwrap_or(Value::Null);
        let response = common::send(app, request(method, &path, body, Some("fixture-owner"))).await;
        return (response.status(), common::read_json(response).await);
    }
    let reply = if source == "public" {
        public_call(s, &["manage"], "manage_plugin_oauth", args).await
    } else {
        call(s, configuration, "manage_plugin_oauth", args).await
    };
    (StatusCode::OK, reply)
}

async fn configured_files_after_last_lookup(action: &'static str) {
    for source in ["http", "public", "installation"] {
        let (_root, s, instance, resource, path, app) = fixture(false).await;
        let main = enabled_main(&s).await;
        connections::save(
            &s,
            "example",
            &json!({
                "name": "Example app", "enabled": true, "settings": {"clientId": "client-fixture"},
                "agentIds": [MAIN_AGENT_ID],
            }),
            Some(&instance),
        )
        .await
        .unwrap();
        let (_, configuration) = active_run(&s, &main).await;
        let provider = axum::Router::new().route("/token", axum::routing::post(|| async {
            axum::Json(json!({"access_token": "files-fixture-access", "refresh_token": "files-fixture-refresh",
                "scope": "messages:read", "expires_in": 3600, "team": {"id": "T111"}}))
        }));
        let (origin, provider) = common::serve_locally(provider).await;
        s.plugins.route_http_to(Some(origin));
        connect(&app, &path, &instance).await;
        let gate = common::relay_fixture::owner_lookups(&s).await;
        let args = json!({
            "pluginId": "example", "instanceId": instance, "collection": "workspace",
            "resourceId": resource, "provider": "bot", "action": action,
        });
        let baseline =
            configured_action(&s, &app, &path, &configuration, source, args.clone()).await;
        assert_eq!(baseline.0, StatusCode::OK, "{baseline:?}");
        let baseline_value = if source == "http" {
            &baseline.1
        } else {
            result(&baseline.1)
        };
        assert_eq!(
            baseline_value["status"],
            if action == "status" {
                "authorized"
            } else {
                "disconnected"
            }
        );
        let lookups = gate.calls.swap(0, Ordering::SeqCst);
        assert!(lookups > 0);
        if action == "revoke" {
            connect(&app, &path, &instance).await;
            gate.calls.store(0, Ordering::SeqCst);
        }
        gate.hold_at.store(lookups, Ordering::SeqCst);
        let (pending_s, pending_app, pending_path, pending_configuration) =
            (s.clone(), app.clone(), path.clone(), configuration.clone());
        let pending = tokio::spawn(async move {
            configured_action(
                &pending_s,
                &pending_app,
                &pending_path,
                &pending_configuration,
                source,
                args,
            )
            .await
        });
        tokio::time::timeout(std::time::Duration::from_secs(10), gate.entered.notified())
            .await
            .unwrap();
        let file = s.config.data_dir.join("plugins/example/1/main.luau");
        let original = std::fs::read(&file).unwrap();
        let mut changed = original.clone();
        changed.extend_from_slice(b"\n-- edited during final owner lookup\n");
        std::fs::write(&file, changed).unwrap();
        gate.release.notify_one();
        let reply = pending.await.unwrap();
        std::fs::write(&file, original).unwrap();
        let after = http_status(&app, &path).await;
        provider.abort();
        s.shutdown.cancel();
        assert_eq!(
            after["status"], "authorized",
            "{source}/{action}: a stale configuration modified saved OAuth: {reply:?}"
        );
        let refused = if source == "http" {
            reply.0 == StatusCode::FORBIDDEN
        } else {
            reply.1["result"]["isError"] == true || reply.1.get("error").is_some()
        };
        assert!(
            refused,
            "{source}/{action}: changed installed files were accepted after the last owner lookup: {reply:?}"
        );
    }
}

#[tokio::test]
async fn authorization_status_rechecks_files_after_the_last_owner_lookup() {
    configured_files_after_last_lookup("status").await;
}

#[tokio::test]
async fn authorization_revoke_rechecks_files_after_the_last_owner_lookup() {
    configured_files_after_last_lookup("revoke").await;
}
