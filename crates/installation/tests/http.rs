use common::AgentFixture;

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
use std::{collections::HashMap, sync::Arc, time::Duration};
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
            "beacon-account",
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
        ("GET", "/api/plugins".to_owned()),
        ("GET", "/api/mcps".to_owned()),
        ("GET", "/api/plugins/github/projects".to_owned()),
        ("POST", "/api/plugins/github/device-flow".to_owned()),
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
        ("POST", "/api/skills/uploads".to_owned()),
        ("POST", "/api/skills/global/import".to_owned()),
        ("POST", "/api/skills/global/review/bundle".to_owned()),
        ("GET", "/api/skills/global/review/source".to_owned()),
        ("POST", "/api/skills/global/review/source/update".to_owned()),
        ("DELETE", "/api/skills/global/review/source".to_owned()),
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

    // Beacon MCP grants have no installation-local management routes.
    for role in [InstallationRole::Owner, InstallationRole::Member] {
        for (method, path) in [("GET", "/api/tokens"), ("POST", "/api/oauth/consent")] {
            let request = json_request(method, path)
                .extension(InstallationIdentity::trusted(role, "beacon-account"))
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
    let tools = health(&app).await["tools"].clone();
    for tool in ["codex", "gh", "claude", "opencode"] {
        assert!(
            tools.as_object().unwrap().contains_key(tool),
            "Health reports the image's {tool} version"
        );
    }
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
    // An abandoned updater must not hold the worker forever or prevent the next
    // updater from acquiring ownership after its lease expires.
    service
        .store
        .set(
            "deployment-lease",
            json!("abandoned-owner"),
            Some(cairn_installation::config::now() - 1),
        )
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
    service
        .worker
        .deployment_lease(&service, "next-owner".into(), false)
        .await
        .unwrap();
    assert_eq!(
        service.store.kv("deployment-lease").await.unwrap(),
        Some(json!("next-owner"))
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
    let stream_url = format!("/api/chats/{chat_id}/attachments/{attachment_id}/stream");
    assert_eq!(
        attachment(&app, anonymous, "GET", &stream_url, vec![])
            .await
            .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        attachment(&app, owner, "GET", &format!("{other_url}/stream"), vec![])
            .await
            .status(),
        StatusCode::NOT_FOUND
    );
    let response = attachment(&app, owner, "GET", &stream_url, vec![]).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["content-type"], "image/png");
    assert_eq!(read_bytes(response).await.as_ref(), png);
    assert_hostile_uploads_are_neutralized(&app, &session, chat_id).await;
}

/// A blank PDF with `pages` pages and a valid cross-reference table.
fn blank_pdf(pages: usize) -> Vec<u8> {
    let kids: Vec<String> = (0..pages).map(|page| format!("{} 0 R", page + 3)).collect();
    let mut objects = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
        format!(
            "<< /Type /Pages /Kids [{}] /Count {pages} >>",
            kids.join(" ")
        ),
    ];
    objects.extend(
        (0..pages).map(|_| "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] >>".to_owned()),
    );
    let mut pdf = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    for (index, object) in objects.iter().enumerate() {
        offsets.push(pdf.len());
        pdf.extend(format!("{} 0 obj\n{object}\nendobj\n", index + 1).bytes());
    }
    let table = pdf.len();
    pdf.extend(format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).bytes());
    for offset in offsets {
        pdf.extend(format!("{offset:010} 00000 n \n").bytes());
    }
    pdf.extend(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{table}\n%%EOF\n",
            objects.len() + 1
        )
        .bytes(),
    );
    pdf
}

fn installed(tool: &str) -> bool {
    std::process::Command::new(tool)
        .arg("-v")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok()
}

#[tokio::test]
async fn attachment_previews_and_summaries_are_rendered_on_demand() {
    let (_root, app, service) = app().await;
    let session = RelayContext::new(&common::relay_fixture::context(&service).await);
    let owner = Credentials::Owner(&session);
    let chat = service.chat_create(json!({})).await.unwrap();
    let chat_id = chat["id"].as_str().unwrap();
    let png = b"\x89\x50\x4e\x47\x0d\x0a\x1a\x0a\x00\x00\x00\x0d\x49\x48\x44\x52\x00\x00\x00\x04\x00\x00\x00\x02\x08\x06\x00\x00\x00\x7f\xa8\x7d\x63\x00\x00\x00\x11\x49\x44\x41\x54\x78\x9c\x63\x10\x31\x8b\xfa\x8f\x8c\x19\xd0\x05\x00\xd3\x26\x0d\x19\xad\xbe\x6f\x4f\x00\x00\x00\x00\x49\x45\x4e\x44\xae\x42\x60\x82";
    let files = [
        ("export.csv", b"id,name\r\n1,Acme\r\n2,Globex".to_vec()),
        ("brief.pdf", blank_pdf(3)),
        ("archive.zip", b"PK\x03\x04\0\0binary".to_vec()),
        ("logo.png", png.to_vec()),
    ];
    let mut urls = HashMap::new();
    for (name, bytes) in files {
        let url = format!("/api/chats/{chat_id}/attachments/{}", id());
        let upload = format!("{url}?name={name}");
        let response = attachment(&app, owner, "PUT", &upload, bytes).await;
        assert_eq!(response.status(), StatusCode::OK);
        urls.insert(name, url);
    }
    let get = async |name: &str, flag: &str| {
        attachment(
            &app,
            owner,
            "GET",
            &format!("{}?{flag}", urls[name]),
            vec![],
        )
        .await
    };

    let summary = read_json(get("export.csv", "summary").await).await;
    assert_eq!(
        summary,
        json!({ "lines": 3, "excerpt": "id,name\n1,Acme\n2,Globex" })
    );
    assert_eq!(
        read_json(get("archive.zip", "summary").await).await,
        json!({})
    );
    for name in ["export.csv", "archive.zip"] {
        assert_eq!(get(name, "preview").await.status(), StatusCode::NOT_FOUND);
    }

    // Rendering relies on the optional poppler and ffmpeg tools of the server image.
    let pages = read_json(get("brief.pdf", "summary").await).await;
    assert_eq!(
        pages,
        if installed("pdfinfo") {
            json!({ "pages": 3 })
        } else {
            json!({})
        }
    );
    for (name, tool) in [("brief.pdf", "pdftoppm"), ("logo.png", "ffmpeg")] {
        let response = get(name, "preview").await;
        if !installed(tool) {
            assert_eq!(response.status(), StatusCode::NOT_FOUND);
            continue;
        }
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()["content-type"], "image/jpeg");
        assert!(read_bytes(response).await.starts_with(&[0xff, 0xd8, 0xff]));
    }

    let original = get("logo.png", "name=logo.png").await;
    assert_eq!(read_bytes(original).await.as_ref(), png);
}

#[tokio::test]
async fn plugin_secrets_require_trusted_owner_context() {
    let (_root, app, service) = app().await;
    for (method, path) in [
        ("GET", "/api/plugins"),
        ("POST", "/api/plugins/onepassword/connections"),
    ] {
        let request = json_request(method, path).body(Body::from("{}")).unwrap();
        assert_eq!(send(&app, request).await.status(), StatusCode::UNAUTHORIZED);
    }
    let session = owner_context(&service).await;
    let input = json!({
        "name": "Fixture",
        "secrets": { "token": "ops_http_fixture" },
        "enabled": true,
        "agentIds": [],
    });
    let save = |credentials: Credentials| {
        credentials
            .apply(json_request("POST", "/api/plugins/onepassword/connections"))
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
        .apply(json_request("GET", "/api/plugins"))
        .body(Body::empty())
        .unwrap();
    let response = send(&app, listed).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(!read_text(response).await.contains("ops_http_fixture"));
}

#[tokio::test]
async fn plugin_project_imports_require_trusted_owner_context() {
    let (_root, app, service) = app().await;
    for (method, path) in [
        ("GET", "/api/plugins/github/projects"),
        ("POST", "/api/projects/import"),
    ] {
        let request = json_request(method, path).body(Body::from("{}")).unwrap();
        assert_eq!(send(&app, request).await.status(), StatusCode::UNAUTHORIZED);
    }
    let session = owner_context(&service).await;
    let import = Credentials::Cookie(&session)
        .apply(json_request("POST", "/api/projects/import"))
        .body(Body::from(r#"{"pluginId":"github","id":"fixture/repo"}"#))
        .unwrap();
    assert_eq!(send(&app, import).await.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn agent_bootstrap_defaults_to_unlimited_and_preserves_configured_limits() {
    let (_root, _app, s) = app().await;
    let timeout_of_main = async |service: &Service| {
        service.agent_snapshot(MAIN_AGENT_ID).await.unwrap()["timeoutMinutes"].clone()
    };
    assert_eq!(timeout_of_main(&s).await, 0);
    assert_eq!(
        s.agent_fixture(json!({ "name": "New agent" }), None)
            .await
            .unwrap()["timeoutMinutes"],
        0
    );
    for minutes in [0, 1, 720] {
        let configured = json!({ "name": "Configured", "timeoutMinutes": minutes });
        assert_eq!(
            s.agent_fixture(configured, None).await.unwrap()["timeoutMinutes"],
            minutes
        );
    }
    for minutes in [-1, 721] {
        let invalid = json!({ "name": "Invalid", "timeoutMinutes": minutes });
        assert!(s.agent_fixture(invalid, None).await.is_err());
    }
    let main_agent = |minutes: i64| json!({ "name": "Main agent", "timeoutMinutes": minutes });
    for (previous, expected) in [(90, 90), (120, 120)] {
        s.agent_fixture(main_agent(previous), Some(MAIN_AGENT_ID))
            .await
            .unwrap();
        let restarted = Service::new(s.config.clone()).await.unwrap();
        assert_eq!(timeout_of_main(&restarted).await, expected);
    }
    s.agent_fixture(main_agent(120), Some(MAIN_AGENT_ID))
        .await
        .unwrap();
    let restarted = Service::new(s.config.clone()).await.unwrap();
    assert_eq!(
        timeout_of_main(&restarted).await,
        120,
        "An explicit limit must survive restart"
    );
}

#[tokio::test]
async fn history_reads_do_not_block_sending_a_message() {
    let (_root, _fixture, service) = app().await;
    let chat = service.chat_create(json!({})).await.unwrap();
    let app = cairn_installation::http::router(service).await.unwrap();
    let identity = InstallationIdentity::trusted(InstallationRole::Owner, "reading-account");

    // Reads made by navigation, pagination and background refresh share this account.
    for _ in 0..300 {
        let response = send(
            &app,
            json_request("GET", "/api/chats")
                .extension(identity.clone())
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
    }

    let response = send(
        &app,
        json_request(
            "POST",
            &format!("/api/chats/{}/messages", chat["id"].as_str().unwrap()),
        )
        .extension(identity)
        .body(Body::from(
            json!({ "id": id(), "text": "Continue" }).to_string(),
        ))
        .unwrap(),
    )
    .await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "reading must not exhaust the message submission budget"
    );
}

#[tokio::test]
async fn account_reads_share_a_bounded_budget_across_paths_and_methods() {
    let (_root, _fixture, service) = app().await;
    let app = cairn_installation::http::router(service).await.unwrap();
    let identity = InstallationIdentity::trusted(InstallationRole::Member, "reading-account");

    for _ in 0..300 {
        let response = send(
            &app,
            json_request("GET", "/api/chats")
                .extension(identity.clone())
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
    }

    let response = send(
        &app,
        json_request("HEAD", "/api/agents")
            .extension(identity)
            .header("x-forwarded-for", "192.0.2.123")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(response.headers()["cache-control"], "no-store");

    let response = send(
        &app,
        json_request("GET", "/api/chats")
            .extension(InstallationIdentity::trusted(
                InstallationRole::Member,
                "other-account",
            ))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn exhausting_writes_does_not_block_reading_the_conversation() {
    let (_root, _fixture, service) = app().await;
    let chat = service.chat_create(json!({})).await.unwrap();
    let app = cairn_installation::http::router(service).await.unwrap();
    let identity = InstallationIdentity::trusted(InstallationRole::Owner, "writing-account");

    for _ in 0..300 {
        let response = send(
            &app,
            json_request("POST", "/api/chats")
                .extension(identity.clone())
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
    }

    let response = send(
        &app,
        json_request("POST", "/api/chats")
            .extension(identity.clone())
            .body(Body::from("{}"))
            .unwrap(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);

    let response = send(
        &app,
        json_request(
            "GET",
            &format!("/api/chats/{}/stream", chat["id"].as_str().unwrap()),
        )
        .extension(identity)
        .body(Body::empty())
        .unwrap(),
    )
    .await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "opening the conversation must not consume the exhausted write budget"
    );
}

#[tokio::test]
async fn mixed_peer_limits_report_the_first_refusal_once_per_window() {
    use std::io::Write;
    use std::sync::Mutex;
    use tracing::instrument::WithSubscriber;

    #[derive(Clone)]
    struct Logs(Arc<Mutex<Vec<u8>>>);

    impl Write for Logs {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    // tracing-core 0.1.36's Rebuilder::JustOne uses the registering thread's dispatcher.
    // Keep a second dispatcher alive so an uncaptured request cannot cache Interest::never
    // for the warning while our scoped subscriber is registered on another context.
    let _callsite_registration =
        tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default());

    // Generic peer traffic and gateway traffic retain their existing shared bucket.
    // Its selected maximum depends on the path (300 or 600).
    for gateway_first in [true, false] {
        let (_root, _fixture, service) = app().await;
        let app = cairn_installation::http::router(service.clone())
            .await
            .unwrap();
        let gateway = format!("/mcp-gateway/{}", id());
        let bytes = Arc::new(Mutex::new(Vec::new()));
        let captured = Logs(bytes.clone());
        let subscriber = tracing_subscriber::fmt()
            .with_ansi(false)
            .without_time()
            .with_writer(move || captured.clone())
            .finish();
        let subscriber = tracing::Dispatch::new(subscriber);

        // Another request can first register the warning outside our capture scope.
        // Use independent rate-limit state to force that ordering before this test's refusals.
        let uncaptured = cairn_installation::http::router(service).await.unwrap();
        for _ in 0..300 {
            assert_eq!(
                send(
                    &uncaptured,
                    json_request("GET", "/health").body(Body::empty()).unwrap(),
                )
                .await
                .status(),
                StatusCode::OK,
            );
        }
        assert_eq!(
            send(
                &uncaptured,
                json_request("GET", "/health").body(Body::empty()).unwrap(),
            )
            .await
            .status(),
            StatusCode::TOO_MANY_REQUESTS,
        );

        async {
            let first = if gateway_first { &gateway } else { "/health" };
            for _ in 0..301 {
                let response = send(
                    &app,
                    json_request("GET", first).body(Body::empty()).unwrap(),
                )
                .await;
                if gateway_first {
                    assert_ne!(response.status(), StatusCode::TOO_MANY_REQUESTS);
                }
            }

            // At count 302, /health refuses even if count 301 used the higher limit.
            assert_eq!(
                send(
                    &app,
                    json_request("GET", "/health").body(Body::empty()).unwrap()
                )
                .await
                .status(),
                StatusCode::TOO_MANY_REQUESTS,
            );
            assert_eq!(
                String::from_utf8(bytes.lock().unwrap().clone())
                    .unwrap()
                    .matches("Installation HTTP rate limit exceeded")
                    .count(),
                1,
                "the first refusal must be reported immediately"
            );
            for _ in 0..300 {
                let response = send(
                    &app,
                    json_request("GET", &gateway).body(Body::empty()).unwrap(),
                )
                .await;
                drop(response);
            }
        }
        .with_subscriber(subscriber)
        .await;

        let logs = String::from_utf8(bytes.lock().unwrap().clone()).unwrap();
        assert_eq!(
            logs.matches("Installation HTTP rate limit exceeded")
                .count(),
            1
        );
    }
}
