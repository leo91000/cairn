mod common;

use common::relay_fixture::router;

use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode, request::Builder},
    response::Response,
};
use cairn_installation::{
    attachments::MAX_FILE,
    auth::{InstallationIdentity, InstallationRole},
    config::{MAIN_AGENT_ID, id},
    service::Service,
};
use common::{Credentials, RelayContext, read_bytes, read_json, read_text, request, send};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

async fn app() -> (TempDir, Router, Arc<Service>) {
    let root = TempDir::new().unwrap();
    let config = common::config(root.path());
    let service = Service::new(config).await.unwrap();
    let app = router(service.clone()).await.unwrap();
    (root, app, service)
}

/// A request as the application sends it from its own origin.
fn json_request(method: &str, path: &str) -> Builder {
    request(method, path).header("content-type", "application/json")
}

fn setup_request() -> Builder {
    json_request("POST", "/api/setup")
}

fn setup_body() -> Body {
    let setup = json!({ "setupToken": "browser-test-setup", "password": "password-long-enough" });
    Body::from(setup.to_string())
}

async fn owner_context(service: &Service) -> RelayContext {
    RelayContext::new(&common::relay_fixture::context(service).await)
}

async fn health(app: &Router) -> Value {
    read_json(
        send(
            app,
            json_request("GET", "/health").body(Body::empty()).unwrap(),
        )
        .await,
    )
    .await
}

#[tokio::test]
async fn installation_http_accepts_only_trusted_context_and_checks_host_and_origin() {
    let (_root, _fixture, service) = app().await;
    let app = cairn_installation::http::router(service.clone())
        .await
        .unwrap();
    let legacy = RelayContext::new(&common::relay_fixture::context(&service).await);
    for path in ["/api/session", "/api/projects", "/api/chats"] {
        let response = send(
            &app,
            Credentials::Cookie(&legacy)
                .apply(json_request("GET", path))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(response.headers()["cache-control"], "no-store");
        assert_eq!(response.headers()["x-frame-options"], "DENY");
    }
    let response = send(&app, setup_request().body(setup_body()).unwrap()).await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert!(!response.headers().contains_key("set-cookie"));
    let foreign_host = Request::builder()
        .uri("/api/session")
        .header("host", "attacker.example")
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        send(&app, foreign_host).await.status(),
        StatusCode::FORBIDDEN
    );
    let foreign_origin = setup_request()
        .header("origin", "https://attacker.example")
        .body(setup_body())
        .unwrap();
    assert_eq!(
        send(&app, foreign_origin).await.status(),
        StatusCode::FORBIDDEN
    );
    let trusted = json_request("POST", "/api/chats")
        .extension(InstallationIdentity::trusted(
            InstallationRole::Owner,
            "official-account",
        ))
        .body(Body::from("{}"))
        .unwrap();
    assert_eq!(send(&app, trusted).await.status(), StatusCode::OK);
}

#[tokio::test]
async fn member_identity_can_list_conversations_but_cannot_manage_nodes() {
    let (_root, app, service) = app().await;
    let session = owner_context(&service).await;

    for (path, expected) in [
        ("/api/chats", StatusCode::OK),
        ("/api/nodes", StatusCode::FORBIDDEN),
    ] {
        let mut request = session
            .authorize(json_request("GET", path))
            .body(Body::empty())
            .unwrap();
        request
            .extensions_mut()
            .insert(InstallationIdentity::trusted(
                InstallationRole::Member,
                "member-account",
            ));

        assert_eq!(send(&app, request).await.status(), expected, "{path}");
    }
}

#[tokio::test]
async fn member_identity_cannot_manage_installation_resources() {
    let (_root, _fixture, service) = app().await;
    let app = cairn_installation::http::router(service).await.unwrap();

    let agent = MAIN_AGENT_ID;
    let management = [
        ("GET", "/api/accounts".to_owned()),
        ("GET", "/api/onepassword".to_owned()),
        ("GET", "/api/mcps".to_owned()),
        ("GET", "/api/connections/login".to_owned()),
        ("GET", "/api/settings".to_owned()),
        ("GET", "/api/audit".to_owned()),
        ("GET", "/api/nodes/settings".to_owned()),
        ("HEAD", "/api/nodes".to_owned()),
        ("GET", "/api/agent-avatars".to_owned()),
        ("POST", "/api/agents".to_owned()),
        ("PUT", format!("/api/agents/{agent}")),
        ("DELETE", format!("/api/agents/{agent}")),
        ("GET", format!("/api/agents/{agent}/github-token")),
        ("PUT", format!("/api/agents/{agent}/github-token")),
        ("POST", format!("/api/agents/{agent}/avatar/generate")),
        ("PUT", format!("/api/agents/{agent}/avatar")),
    ];

    for (method, path) in management {
        let mut request = json_request(method, &path).body(Body::empty()).unwrap();
        request
            .extensions_mut()
            .insert(InstallationIdentity::trusted(
                InstallationRole::Member,
                "member-account",
            ));

        assert_eq!(
            send(&app, request).await.status(),
            StatusCode::FORBIDDEN,
            "{method} {path}",
        );
    }

    // Official MCP grants have no installation-local management routes.
    for role in [InstallationRole::Owner, InstallationRole::Member] {
        for (method, path) in [("GET", "/api/tokens"), ("POST", "/api/oauth/consent")] {
            let request = json_request(method, path)
                .extension(InstallationIdentity::trusted(role, "official-account"))
                .body(Body::from("{}"))
                .unwrap();
            assert_eq!(
                send(&app, request).await.status(),
                StatusCode::NOT_FOUND,
                "{method} {path}"
            );
        }
    }

    // Choosing an agent for a conversation is available to every member.
    for method in ["GET", "HEAD"] {
        let mut request = json_request(method, "/api/agents")
            .body(Body::empty())
            .unwrap();
        request
            .extensions_mut()
            .insert(InstallationIdentity::trusted(
                InstallationRole::Member,
                "member-account",
            ));
        assert_eq!(send(&app, request).await.status(), StatusCode::OK);
    }
}

#[tokio::test]
async fn member_identity_can_create_and_stream_conversations_without_a_local_session() {
    let (_root, app, _service) = app().await;
    let mut create = json_request("POST", "/api/chats")
        .body(Body::from("{}"))
        .unwrap();
    create
        .extensions_mut()
        .insert(InstallationIdentity::trusted(
            InstallationRole::Member,
            "member-account",
        ));
    let response = send(&app, create).await;
    assert_eq!(response.status(), StatusCode::OK);
    let chat = read_json(response).await;

    for path in [
        "/api/chats/stream".to_owned(),
        format!("/api/chats/{}/stream", chat["id"].as_str().unwrap()),
    ] {
        let mut request = json_request("GET", &path).body(Body::empty()).unwrap();
        request
            .extensions_mut()
            .insert(InstallationIdentity::trusted(
                InstallationRole::Member,
                "member-account",
            ));
        let response = send(&app, request).await;
        assert_eq!(response.status(), StatusCode::OK, "{path}");
        assert_eq!(response.headers()["content-type"], "text/event-stream");

        let mut body = response.into_body();
        let frame = tokio::time::timeout(Duration::from_secs(2), body.frame())
            .await
            .expect("the member should receive the initial batch")
            .expect("the member stream should remain open")
            .unwrap();
        let batch = String::from_utf8(frame.into_data().unwrap().to_vec()).unwrap();
        assert!(batch.contains("event: batch"), "{path}: {batch}");
    }
}

#[tokio::test]
async fn removed_login_cannot_be_enabled_by_forged_forwarded_ips() {
    let (_root, _fixture, service) = app().await;
    let app = cairn_installation::http::router(service).await.unwrap();
    for n in 0..35 {
        let login = json_request("POST", "/api/login")
            .header("x-forwarded-for", format!("192.0.2.{n}"))
            .body(Body::from("{\"password\":\"password-long-enough\"}"))
            .unwrap();
        assert_eq!(send(&app, login).await.status(), StatusCode::UNAUTHORIZED);
    }
}

#[tokio::test]
async fn health_and_deployment_lease_report_live_worker_ownership() {
    let (_root, app, service) = app().await;
    let saved = json!({
        "id": "saved-run",
        "taskId": "task",
        "projectId": "project",
        "status": "running",
        "createdAt": 1,
    });
    common::add_run(&service.store, &saved).await;
    assert_eq!(
        health(&app).await["activeRuns"],
        0,
        "Persisted runs are not live processes when the worker is disabled"
    );
    service
        .worker
        .active
        .lock()
        .await
        .insert("preparing-run".into(), CancellationToken::new());
    assert_eq!(
        health(&app).await["activeRuns"],
        1,
        "Preparing attempts already belong to the live worker"
    );
    let result = service
        .worker
        .deployment_lease(&service, "owner".into(), false)
        .await
        .unwrap();
    assert_eq!(result, json!({ "paused": true, "activeRuns": 1 }));
    assert_eq!(
        service.store.kv("deployment-lease").await.unwrap(),
        Some(json!("owner"))
    );
    assert_eq!(
        service
            .worker
            .deployment_lease(&service, "another-owner".into(), true)
            .await
            .unwrap_err()
            .status,
        409
    );
    service
        .worker
        .deployment_lease(&service, "owner".into(), true)
        .await
        .unwrap();
    assert!(
        service
            .store
            .kv("deployment-lease")
            .await
            .unwrap()
            .is_none()
    );
}

/// Sends `bytes` to an attachment endpoint.
async fn attachment(
    app: &Router,
    credentials: Credentials<'_>,
    method: &str,
    url: &str,
    bytes: Vec<u8>,
) -> Response {
    let request = credentials.apply(request(method, url));
    send(app, request.body(Body::from(bytes)).unwrap()).await
}

/// A hostile name cannot escape the chat directory, and active content is only
/// ever served as a sandboxed download.
async fn assert_hostile_uploads_are_neutralized(
    app: &Router,
    session: &RelayContext,
    chat_id: &str,
) {
    let owner = Credentials::Owner(session);
    let evil = format!(
        "/api/chats/{chat_id}/attachments/{}?name=..%2F..%2Ffile.svg",
        id()
    );
    let svg = b"<svg onload='alert(1)'/>".to_vec();
    let response = attachment(app, owner, "PUT", &evil, svg).await;
    assert_eq!(response.status(), StatusCode::OK);
    let data = read_json(response).await;
    assert!(!data["name"].as_str().unwrap().contains('/'));
    let response = attachment(app, owner, "GET", &evil, vec![]).await;
    let headers = response.headers();
    assert_eq!(headers["content-type"], "application/octet-stream");
    assert!(
        headers["content-disposition"]
            .to_str()
            .unwrap()
            .starts_with("attachment;")
    );
    assert!(
        headers["content-security-policy"]
            .to_str()
            .unwrap()
            .contains("sandbox")
    );
}

#[tokio::test]
async fn attachments_are_private_scoped_bounded_and_durable() {
    let (_root, app, service) = app().await;
    let session = RelayContext::new(&common::relay_fixture::context(&service).await);
    let (anonymous, cookie, owner) = (
        Credentials::Anonymous,
        Credentials::Cookie(&session),
        Credentials::Owner(&session),
    );
    let chat = service.chat_create(json!({})).await.unwrap();
    let other = service.chat_create(json!({})).await.unwrap();
    let attachment_id = id();
    let chat_id = chat["id"].as_str().unwrap();
    let other_id = other["id"].as_str().unwrap();
    let url = format!("/api/chats/{chat_id}/attachments/{attachment_id}?name=design.png");
    let png = b"\x89PNG\r\n\x1a\nfixture".to_vec();
    let put = async |credentials: Credentials<'_>, bytes: Vec<u8>| {
        attachment(&app, credentials, "PUT", &url, bytes)
            .await
            .status()
    };
    assert_eq!(put(anonymous, png.clone()).await, StatusCode::UNAUTHORIZED);
    assert_eq!(put(cookie, png.clone()).await, StatusCode::UNAUTHORIZED);
    assert_eq!(
        put(owner, vec![0; MAX_FILE + 1]).await,
        StatusCode::PAYLOAD_TOO_LARGE
    );
    let response = attachment(&app, owner, "PUT", &url, png.clone()).await;
    assert_eq!(response.status(), StatusCode::OK);
    let uploaded = read_json(response).await;
    assert_eq!(uploaded["kind"], "image");
    assert_eq!(put(owner, png.clone()).await, StatusCode::OK);
    assert_eq!(
        put(owner, b"different".to_vec()).await,
        StatusCode::CONFLICT
    );
    assert_eq!(
        attachment(&app, anonymous, "GET", &url, vec![])
            .await
            .status(),
        StatusCode::UNAUTHORIZED
    );
    let other_url = format!("/api/chats/{other_id}/attachments/{attachment_id}");
    assert_eq!(
        attachment(&app, owner, "GET", &other_url, vec![])
            .await
            .status(),
        StatusCode::NOT_FOUND
    );
    for (chat, message) in [
        (
            other_id,
            json!({ "id": id(), "attachmentIds": [attachment_id] }),
        ),
        (
            chat_id,
            json!({ "id": id(), "attachmentIds": [attachment_id, attachment_id] }),
        ),
        (chat_id, json!({ "id": id(), "text": "" })),
    ] {
        assert!(service.chat_send(chat, message).await.is_err());
    }
    let message_id = id();
    let message = service
        .chat_send(
            chat_id,
            json!({ "id": message_id, "attachmentIds": [attachment_id] }),
        )
        .await
        .unwrap();
    assert_eq!(message["attachments"][0], uploaded);
    assert_eq!(
        service.chat_detail(chat_id).await.unwrap()["title"],
        "design.png"
    );
    let changed = json!({ "id": message_id, "text": "changed", "attachmentIds": [] });
    assert!(service.chat_send(chat_id, changed).await.is_err());
    let reopened = Service::new(service.config.clone()).await.unwrap();
    assert_eq!(
        reopened.chat_detail(chat_id).await.unwrap()["messages"][0]["attachments"][0],
        uploaded
    );
    let response = attachment(&app, owner, "GET", &url, vec![]).await;
    assert_eq!(response.headers()["cache-control"], "no-store");
    assert_eq!(response.headers()["content-type"], "image/png");
    assert_eq!(read_bytes(response).await.as_ref(), png);
    assert_hostile_uploads_are_neutralized(&app, &session, chat_id).await;
}

#[tokio::test]
async fn onepassword_management_requires_trusted_owner_context() {
    let (_root, app, service) = app().await;
    for method in ["GET", "POST"] {
        let request = json_request(method, "/api/onepassword")
            .body(Body::from("{}"))
            .unwrap();
        assert_eq!(send(&app, request).await.status(), StatusCode::UNAUTHORIZED);
    }
    let session = owner_context(&service).await;
    let input = json!({
        "name": "Fixture",
        "token": "ops_http_fixture",
        "enabled": true,
        "agentIds": [],
    });
    let save = |credentials: Credentials| {
        credentials
            .apply(json_request("POST", "/api/onepassword"))
            .body(Body::from(input.to_string()))
            .unwrap()
    };
    assert_eq!(
        send(&app, save(Credentials::Cookie(&session)))
            .await
            .status(),
        StatusCode::UNAUTHORIZED
    );
    let response = send(&app, save(Credentials::Owner(&session))).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(!read_text(response).await.contains("ops_http_fixture"));
    let listed = Credentials::Owner(&session)
        .apply(json_request("GET", "/api/onepassword"))
        .body(Body::empty())
        .unwrap();
    let response = send(&app, listed).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(!read_text(response).await.contains("ops_http_fixture"));
}

#[tokio::test]
async fn github_projects_require_trusted_owner_context() {
    let (_root, app, service) = app().await;
    for (method, path) in [
        ("GET", "/api/github/repositories"),
        ("POST", "/api/projects/github"),
    ] {
        let request = json_request(method, path).body(Body::from("{}")).unwrap();
        assert_eq!(send(&app, request).await.status(), StatusCode::UNAUTHORIZED);
    }
    let session = owner_context(&service).await;
    let import = Credentials::Cookie(&session)
        .apply(json_request("POST", "/api/projects/github"))
        .body(Body::from(r#"{"repository":"fixture/repo"}"#))
        .unwrap();
    assert_eq!(send(&app, import).await.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn agents_default_to_unlimited_and_migrate_only_the_main_agents_old_default() {
    let (_root, _app, s) = app().await;
    let timeout_of_main = async |service: &Service| {
        service.get("agents", MAIN_AGENT_ID).await.unwrap()["timeoutMinutes"].clone()
    };
    assert_eq!(timeout_of_main(&s).await, 0);
    assert_eq!(
        s.agent(json!({ "name": "New agent" }), None).await.unwrap()["timeoutMinutes"],
        0
    );
    for minutes in [0, 1, 720] {
        let configured = json!({ "name": "Configured", "timeoutMinutes": minutes });
        assert_eq!(
            s.agent(configured, None).await.unwrap()["timeoutMinutes"],
            minutes
        );
    }
    for minutes in [-1, 721] {
        let invalid = json!({ "name": "Invalid", "timeoutMinutes": minutes });
        assert!(s.agent(invalid, None).await.is_err());
    }
    let main_agent = |minutes: i64| json!({ "name": "Main agent", "timeoutMinutes": minutes });
    for (previous, expected) in [(90, 90), (120, 0)] {
        s.agent(main_agent(previous), Some(MAIN_AGENT_ID))
            .await
            .unwrap();
        s.store
            .write(|db| db.delete("migration:main-agent-unlimited"))
            .await
            .unwrap();
        let restarted = Service::new(s.config.clone()).await.unwrap();
        assert_eq!(timeout_of_main(&restarted).await, expected);
    }
    s.agent(main_agent(120), Some(MAIN_AGENT_ID)).await.unwrap();
    let restarted = Service::new(s.config.clone()).await.unwrap();
    assert_eq!(
        timeout_of_main(&restarted).await,
        120,
        "An explicit choice after migration must survive restart"
    );
}
