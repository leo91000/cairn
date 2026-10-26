//! The owner UI and both management tools share resource authorization lifecycle.
use super::*;

#[path = "../common/management.rs"]
mod access;

use access::{active_run, call, enabled_main, public_call, result};

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
        let method = if text(&args, "action") == "status" {
            "GET"
        } else {
            "DELETE"
        };
        let response = common::send(
            app,
            request(method, path, Value::Null, Some("fixture-owner")),
        )
        .await;
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
