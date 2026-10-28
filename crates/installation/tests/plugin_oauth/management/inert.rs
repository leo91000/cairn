use super::*;

#[tokio::test]
async fn owner_http_inert_resource_consent_is_the_lifecycle_reference() {
    inert_lifecycle("http").await;
}

#[tokio::test]
async fn public_mcp_can_manage_inert_resource_consent() {
    inert_lifecycle("public").await;
}

#[tokio::test]
async fn an_unchanged_inert_resource_consent_exchange_can_finish() {
    revoke_during_exchange(false).await;
}

#[tokio::test]
async fn public_mcp_revoke_fences_an_inert_resource_consent_exchange() {
    revoke_during_exchange(true).await;
}

async fn revoke_during_exchange(revoke: bool) {
    let (_root, service, draft, resource, path, app) = draft_fixture(false).await;
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
                    "access_token": "inert-delayed-private-access",
                    "refresh_token": "inert-delayed-private-refresh",
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
        "draftId": draft["id"],
        "collection": "workspace",
        "resourceId": resource["id"],
        "provider": "bot",
        "action": "start",
        "data": {
            "setupRevision": draft["revision"],
            "scopes": ["messages:read"],
        },
    });
    let response =
        configured_action(&service, &app, &path, &Value::Null, "http", args.clone()).await;
    let started = observed("http", response);
    let callback = format!(
        "/api/plugin-oauth/{}/callback?state={}&code=inert-delayed-fixture",
        text(&draft, "id"),
        text(&started, "state")
    );
    let response = common::send(&app, request("GET", &callback, Value::Null, None)).await;
    assert_eq!(response.status(), StatusCode::OK);
    let routed = app.clone();
    let finish = format!("{path}/finish");
    let pending = tokio::spawn(async move {
        common::send(
            &routed,
            request(
                "POST",
                &finish,
                json!({"state": started["state"]}),
                Some("fixture-owner"),
            ),
        )
        .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(5), entered.notified())
        .await
        .expect("The owner exchange must reach the actual token endpoint");
    if revoke {
        let mut args = args;
        args["action"] = json!("revoke");
        args["data"] = json!({"setupRevision": draft["revision"]});
        let response = public_call(&service, &["manage"], "manage_plugin_oauth", args).await;
        assert_eq!(result(&response)["status"], "disconnected");
    }
    release.notify_one();
    let response = pending.await.unwrap();
    if revoke {
        assert_ne!(response.status(), StatusCode::OK);
    } else {
        assert_eq!(response.status(), StatusCode::OK);
    }
    assert_eq!(
        http_status(&app, &path).await["status"],
        if revoke { "disconnected" } else { "authorized" }
    );
    assert_eq!(exchanges.load(Ordering::SeqCst), 1);
    server.abort();
}

async fn inert_lifecycle(source: &str) {
    let (_root, service, draft, resource, path, app) = draft_fixture(false).await;
    let exchanges = Arc::new(AtomicUsize::new(0));
    let server = token_endpoint(&service, exchanges.clone()).await;
    let args = json!({
        "pluginId": "example",
        "draftId": draft["id"],
        "collection": "workspace",
        "resourceId": resource["id"],
        "provider": "bot",
        "action": "start",
        "data": {
            "setupRevision": draft["revision"],
            "scopes": ["messages:read"],
        },
    });
    let response =
        configured_action(&service, &app, &path, &Value::Null, source, args.clone()).await;
    let started = observed(source, response);
    assert!(!started.to_string().contains("client-secret-fixture"));
    let mut finish = args.clone();
    finish["action"] = json!("finish");
    finish["data"] = json!({"state": started["state"]});
    let pending =
        configured_action(&service, &app, &path, &Value::Null, source, finish.clone()).await;
    assert_eq!(observed(source, pending)["status"], "pending");
    assert_eq!(exchanges.load(Ordering::SeqCst), 0);
    let callback = format!(
        "/api/plugin-oauth/{}/callback?state={}&code=inert-management-fixture",
        text(&draft, "id"),
        text(&started, "state")
    );
    let response = common::send(&app, request("GET", &callback, Value::Null, None)).await;
    assert_eq!(response.status(), StatusCode::OK);
    let finished = configured_action(&service, &app, &path, &Value::Null, source, finish).await;
    let finished = observed(source, finished);
    assert_eq!(finished["status"], "authorized");
    assert_eq!(finished["scopes"], json!(["messages:read"]));
    assert!(!finished.to_string().contains("inert-managed-private"));
    assert_eq!(http_status(&app, &path).await, finished);

    let mut status = args.clone();
    status["action"] = json!("status");
    status.as_object_mut().unwrap().remove("data");
    let status = configured_action(&service, &app, &path, &Value::Null, source, status).await;
    assert_eq!(observed(source, status), finished);
    let response = common::send(
        &app,
        request(
            "GET",
            &format!("/api/plugins/example/connections/{}", text(&draft, "id")),
            Value::Null,
            Some("fixture-owner"),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let mut revoke = args;
    revoke["action"] = json!("revoke");
    revoke["data"] = json!({"setupRevision": draft["revision"]});
    let revoked = configured_action(&service, &app, &path, &Value::Null, source, revoke).await;
    assert_eq!(observed(source, revoked), json!({"status": "disconnected"}));
    assert_eq!(http_status(&app, &path).await["status"], "disconnected");
    assert_eq!(exchanges.load(Ordering::SeqCst), 1);
    server.abort();
}

#[tokio::test]
async fn native_inert_resource_consent_keeps_the_private_owner_boundary() {
    let (_root, service, draft, resource, path, app) = draft_fixture(false).await;
    let main = enabled_main(&service).await;
    let (_, configuration) = active_run(&service, &main).await;
    let exchanges = Arc::new(AtomicUsize::new(0));
    let server = token_endpoint(&service, exchanges.clone()).await;
    let mut args = json!({
        "pluginId": "example",
        "draftId": draft["id"],
        "collection": "workspace",
        "resourceId": resource["id"],
        "provider": "bot",
        "action": "start",
        "data": {
            "setupRevision": draft["revision"],
            "scopes": ["messages:read"],
        },
    });
    let denied = call(
        &service,
        &configuration,
        "manage_plugin_oauth",
        args.clone(),
    )
    .await;
    private_consent_refused(&denied);
    let response =
        configured_action(&service, &app, &path, &Value::Null, "http", args.clone()).await;
    let started = observed("http", response);
    let callback = format!(
        "/api/plugin-oauth/{}/callback?state={}&code=inert-owner-fixture",
        text(&draft, "id"),
        text(&started, "state")
    );
    let response = common::send(&app, request("GET", &callback, Value::Null, None)).await;
    assert_eq!(response.status(), StatusCode::OK);
    args["action"] = json!("finish");
    args["data"] = json!({"state": started["state"]});
    let denied = call(
        &service,
        &configuration,
        "manage_plugin_oauth",
        args.clone(),
    )
    .await;
    private_consent_refused(&denied);
    let response =
        configured_action(&service, &app, &path, &Value::Null, "http", args.clone()).await;
    assert_eq!(observed("http", response)["status"], "authorized");
    args["action"] = json!("revoke");
    args["data"] = json!({"setupRevision": draft["revision"]});
    let denied = call(
        &service,
        &configuration,
        "manage_plugin_oauth",
        args.clone(),
    )
    .await;
    assert_eq!(denied["result"]["isError"], true);
    assert_eq!(http_status(&app, &path).await["status"], "authorized");
    args["action"] = json!("status");
    args.as_object_mut().unwrap().remove("data");
    let status = call(&service, &configuration, "manage_plugin_oauth", args).await;
    assert_eq!(result(&status)["status"], "authorized");
    assert!(!status.to_string().contains("inert-managed-private"));
    assert!(!status.to_string().contains("client-secret-fixture"));
    assert_eq!(exchanges.load(Ordering::SeqCst), 1);
    server.abort();
}

fn observed(source: &str, response: (StatusCode, Value)) -> Value {
    assert_eq!(response.0, StatusCode::OK);
    if source == "http" {
        response.1
    } else {
        result(&response.1).clone()
    }
}

fn private_consent_refused(response: &Value) {
    assert_eq!(response["result"]["isError"], true);
    assert!(
        response
            .to_string()
            .contains("Private setup consent belongs to the owner")
    );
}

async fn token_endpoint(
    service: &Service,
    exchanges: Arc<AtomicUsize>,
) -> tokio::task::JoinHandle<()> {
    let provider = axum::Router::new().route(
        "/token",
        axum::routing::post(move || {
            let exchanges = exchanges.clone();
            async move {
                exchanges.fetch_add(1, Ordering::SeqCst);
                axum::Json(json!({
                    "access_token": "inert-managed-private-access",
                    "refresh_token": "inert-managed-private-refresh",
                    "scope": "messages:read",
                    "expires_in": 3600,
                    "team": {"id": "T111"},
                }))
            }
        }),
    );
    let (origin, server) = common::serve_locally(provider).await;
    service.plugins.route_http_to(Some(origin));
    server
}
