use axum::{
    body::{Body, to_bytes},
    http::Request,
};
use leo_agent_manager::{config::Config, http::router, service::Service};
use serde_json::{Value, json};
use tempfile::TempDir;
use tower::ServiceExt;

async fn app() -> (TempDir, axum::Router, std::sync::Arc<Service>) {
    let root = TempDir::new().unwrap();
    let config = Config {
        data_dir: root.path().join("data"),
        home: root.path().join("home"),
        workspace_roots: vec![root.path().to_owned()],
        public_url: "http://localhost:4310".into(),
        host: "127.0.0.1".into(),
        port: 0,
        setup_token: "test-setup".into(),
        codex_bin: "codex".into(),
        claude_bin: "claude".into(),
        gh_bin: "gh".into(),
        concurrency: 1,
        logger: false,
        worker_enabled: false,
        runner_url: String::new(),
    };
    let service = Service::new(config).await.unwrap();
    let app = router(service.clone()).await.unwrap();
    (root, app, service)
}

fn request(method: &str, path: &str, body: Value) -> axum::http::request::Builder {
    let _ = body;
    Request::builder()
        .method(method)
        .uri(path)
        .header("host", "localhost:4310")
        .header("content-type", "application/json")
}

#[tokio::test]
async fn http_authentication_csrf_host_origin_and_cookie_contracts() {
    let (_root, app, _service) = app().await;
    let response = app
        .clone()
        .oneshot(
            request("GET", "/api/projects", Value::Null)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), 401);
    assert_eq!(response.headers()["cache-control"], "no-store");
    assert_eq!(response.headers()["x-frame-options"], "DENY");
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/session")
                .header("host", "attacker.example")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), 403);
    let response = app
        .clone()
        .oneshot(
            request("POST", "/api/setup", Value::Null)
                .header("origin", "https://attacker.example")
                .body(Body::from(
                    json!({
                        "setupToken": "test-setup",
                        "password": "password-long-enough",
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), 403);
    let response = app
        .clone()
        .oneshot(
            request("POST", "/api/setup", Value::Null)
                .body(Body::from(
                    json!({
                        "setupToken": "test-setup",
                        "password": "password-long-enough",
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let cookie = response.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .to_owned();
    assert!(cookie.contains("HttpOnly"));
    assert!(cookie.contains("SameSite=Lax"));
    let cookie = cookie.split(';').next().unwrap();
    let body: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 10000).await.unwrap()).unwrap();
    let csrf = body["csrf"].as_str().unwrap();
    let response = app
        .clone()
        .oneshot(
            request("POST", "/api/chats", Value::Null)
                .header("cookie", cookie)
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), 403);
    let response = app
        .clone()
        .oneshot(
            request("POST", "/api/chats", Value::Null)
                .header("cookie", cookie)
                .header("x-csrf-token", csrf)
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let response = app
        .clone()
        .oneshot(
            request("GET", "/api/chats", Value::Null)
                .header("cookie", cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let chats: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 10000).await.unwrap()).unwrap();
    assert_eq!(chats.as_array().unwrap().len(), 1);
    let response = app
        .clone()
        .oneshot(
            request("POST", "/api/logout", Value::Null)
                .header("cookie", cookie)
                .header("x-csrf-token", csrf)
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let response = app
        .oneshot(
            request("GET", "/api/chats", Value::Null)
                .header("cookie", cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), 401);
}

#[tokio::test]
async fn login_limits_ignore_forged_forwarded_ips() {
    let (_root, app, _service) = app().await;
    for n in 0..11 {
        let response = app
            .clone()
            .oneshot(
                request("POST", "/api/login", Value::Null)
                    .header("x-forwarded-for", format!("192.0.2.{n}"))
                    .body(Body::from("{\"password\":\"test\"}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), if n < 10 { 401 } else { 429 });
    }
}

#[tokio::test]
async fn health_and_deployment_lease_report_live_worker_ownership() {
    let (_root, app, service) = app().await;
    service
        .store
        .write(|db| {
            db.add_run(
                &json!({
                    "id": "saved-run",
                    "taskId": "task",
                    "projectId": "project",
                    "status": "running",
                    "createdAt": 1,
                }),
                None,
            )
        })
        .await
        .unwrap();
    let response = app
        .clone()
        .oneshot(
            request("GET", "/health", Value::Null)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let health: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 10000).await.unwrap()).unwrap();
    assert_eq!(
        health["activeRuns"], 0,
        "Persisted runs are not live processes when the worker is disabled"
    );
    service.worker.active.lock().await.insert(
        "preparing-run".into(),
        tokio_util::sync::CancellationToken::new(),
    );
    let response = app
        .oneshot(
            request("GET", "/health", Value::Null)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let health: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 10000).await.unwrap()).unwrap();
    assert_eq!(
        health["activeRuns"], 1,
        "Preparing attempts already belong to the live worker"
    );
    let result = service
        .worker
        .deployment_lease(&service, "owner".into(), false)
        .await
        .unwrap();
    assert_eq!(
        result,
        json!({
            "paused": true,
            "activeRuns": 1
        })
    );
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

#[tokio::test]
async fn attachments_are_private_scoped_bounded_and_durable() {
    let (_root, app, service) = app().await;
    let session = service.auth.session().await.unwrap();
    let chat = service.chat_create(json!({})).await.unwrap();
    let other = service.chat_create(json!({})).await.unwrap();
    let id = leo_agent_manager::config::id();
    let chat_id = chat["id"].as_str().unwrap();
    let url = format!("/api/chats/{chat_id}/attachments/{id}?name=design.png");
    async fn call(
        app: &axum::Router,
        session: &Value,
        method: &str,
        url: &str,
        bytes: Vec<u8>,
        csrf: bool,
    ) -> axum::response::Response {
        let mut req = Request::builder()
            .method(method)
            .uri(url)
            .header("host", "localhost:4310");
        if !session.is_null() {
            req = req.header(
                "cookie",
                format!("leo_session={}", session["value"].as_str().unwrap()),
            );
        }
        if csrf {
            req = req.header("x-csrf-token", session["csrf"].as_str().unwrap());
        }
        app.clone()
            .oneshot(req.body(Body::from(bytes)).unwrap())
            .await
            .unwrap()
    }
    let png = b"\x89PNG\r\n\x1a\nfixture".to_vec();
    assert_eq!(
        call(&app, &Value::Null, "PUT", &url, png.clone(), false)
            .await
            .status(),
        401
    );
    assert_eq!(
        call(&app, &session, "PUT", &url, png.clone(), false)
            .await
            .status(),
        403
    );
    assert_eq!(
        call(
            &app,
            &session,
            "PUT",
            &url,
            vec![0; leo_agent_manager::attachments::MAX_FILE + 1],
            true
        )
        .await
        .status(),
        413
    );
    let response = call(&app, &session, "PUT", &url, png.clone(), true).await;
    assert_eq!(response.status(), 200);
    let attachment: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 10000).await.unwrap()).unwrap();
    assert_eq!(attachment["kind"], "image");
    assert_eq!(
        call(&app, &session, "PUT", &url, png.clone(), true)
            .await
            .status(),
        200
    );
    assert_eq!(
        call(&app, &session, "PUT", &url, b"different".to_vec(), true)
            .await
            .status(),
        409
    );
    assert_eq!(
        call(&app, &Value::Null, "GET", &url, vec![], false)
            .await
            .status(),
        401
    );
    let other_url = format!(
        "/api/chats/{}/attachments/{id}",
        other["id"].as_str().unwrap()
    );
    assert_eq!(
        call(&app, &session, "GET", &other_url, vec![], false)
            .await
            .status(),
        404
    );
    assert!(
        service
            .chat_send(
                other["id"].as_str().unwrap(),
                json!({
                    "id": leo_agent_manager::config::id(),
                    "attachmentIds": [id]
                })
            )
            .await
            .is_err()
    );
    assert!(
        service
            .chat_send(
                chat_id,
                json!({
                    "id": leo_agent_manager::config::id(),
                    "attachmentIds": [id, id]
                })
            )
            .await
            .is_err()
    );
    assert!(
        service
            .chat_send(
                chat_id,
                json!({
                    "id": leo_agent_manager::config::id(),
                    "text": ""
                })
            )
            .await
            .is_err()
    );
    let message_id = leo_agent_manager::config::id();
    let message = service
        .chat_send(
            chat_id,
            json!({
                "id": message_id,
                "attachmentIds": [id],
            }),
        )
        .await
        .unwrap();
    assert_eq!(message["attachments"][0], attachment);
    assert_eq!(
        service.chat_detail(chat_id).await.unwrap()["title"],
        "design.png"
    );
    assert!(
        service
            .chat_send(
                chat_id,
                json!({
                    "id": message_id,
                    "text": "changed",
                    "attachmentIds": []
                })
            )
            .await
            .is_err()
    );
    let reopened = Service::new(service.config.clone()).await.unwrap();
    assert_eq!(
        reopened.chat_detail(chat_id).await.unwrap()["messages"][0]["attachments"][0],
        attachment
    );
    let response = call(&app, &session, "GET", &url, vec![], false).await;
    assert_eq!(response.headers()["cache-control"], "no-store");
    assert_eq!(response.headers()["content-type"], "image/png");
    assert_eq!(
        to_bytes(response.into_body(), 10000)
            .await
            .unwrap()
            .as_ref(),
        png
    );
    let evil = format!(
        "/api/chats/{chat_id}/attachments/{}?name=..%2F..%2Ffile.svg",
        leo_agent_manager::config::id()
    );
    let response = call(
        &app,
        &session,
        "PUT",
        &evil,
        b"<svg onload='alert(1)'/>".to_vec(),
        true,
    )
    .await;
    assert_eq!(response.status(), 200);
    let data: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 10000).await.unwrap()).unwrap();
    assert!(!data["name"].as_str().unwrap().contains('/'));
    let response = call(&app, &session, "GET", &evil, vec![], false).await;
    assert_eq!(
        response.headers()["content-type"],
        "application/octet-stream"
    );
    assert!(
        response.headers()["content-disposition"]
            .to_str()
            .unwrap()
            .starts_with("attachment;")
    );
    assert!(
        response.headers()["content-security-policy"]
            .to_str()
            .unwrap()
            .contains("sandbox")
    );
}

#[tokio::test]
async fn native_mcp_callback_requires_the_initiating_session_and_csrf_to_finish() {
    use leo_agent_manager::{auth::hex_digest, config::now};
    let (_root, app, service) = app().await;
    let session = service.auth.session().await.unwrap();
    let other = service.auth.session().await.unwrap();
    let id = "00000000-0000-4000-8000-000000000099";
    let nonce = "native-callback-test-nonce";
    let key = format!("mcp-oauth:{}", hex_digest(nonce));
    let expires = now() + 600000;
    service
        .store
        .set(
            &key,
            json!({
                "native": true,
                "connectionId": id,
                "session": hex_digest(session["csrf"].as_str().unwrap()),
                "nonce": nonce,
                "expiresAt": expires,
            }),
            Some(expires),
        )
        .await
        .unwrap();
    let response = app
        .clone()
        .oneshot(
            request(
                "GET",
                &format!("/oauth/mcp/callback?state={nonce}&code=private-code"),
                Value::Null,
            )
            .body(Body::empty())
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["referrer-policy"], "no-referrer");
    assert_eq!(response.headers()["cache-control"], "no-store");
    let html = String::from_utf8(
        to_bytes(response.into_body(), 10000)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    assert!(html.contains("Revenez dans Leo"));
    assert!(!html.contains("private-code"));
    assert!(!html.contains(nonce));
    let pending = service.store.kv(&key).await.unwrap().unwrap();
    assert_eq!(pending["callback"]["code"], "private-code");
    assert_eq!(pending["expiresAt"], expires);
    for (credentials, csrf, status) in [
        (Value::Null, false, 401),
        (session.clone(), false, 403),
        (other.clone(), true, 200),
    ] {
        let mut req = request("POST", &format!("/api/mcps/{id}/callback"), Value::Null);
        if let Some(cookie) = credentials["value"].as_str() {
            req = req.header("cookie", format!("leo_session={cookie}"));
        }
        if csrf {
            req = req.header("x-csrf-token", credentials["csrf"].as_str().unwrap());
        }
        let response = app
            .clone()
            .oneshot(req.body(Body::from("{}")).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), status);
        if status == 200 {
            let body: Value =
                serde_json::from_slice(&to_bytes(response.into_body(), 10000).await.unwrap())
                    .unwrap();
            assert_eq!(
                body,
                json!({
                    "pending": false,
                    "result": "expired"
                })
            );
        }
        assert!(service.store.kv(&key).await.unwrap().is_some());
    }
    service
        .auth
        .logout(session["value"].as_str().unwrap())
        .await
        .unwrap();
    let response = app
        .oneshot(
            request("POST", &format!("/api/mcps/{id}/callback"), Value::Null)
                .header(
                    "cookie",
                    format!("leo_session={}", session["value"].as_str().unwrap()),
                )
                .header("x-csrf-token", session["csrf"].as_str().unwrap())
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), 401);
    // Expiration is enforced by the store, including capture attempts from the browser.
    service
        .store
        .set(&key, pending, Some(now() - 1))
        .await
        .unwrap();
    assert!(
        !service
            .mcps
            .capture_native_callback(
                &service,
                &std::collections::HashMap::from([
                    ("state".into(), nonce.into()),
                    ("code".into(), "replacement".into())
                ])
            )
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn onepassword_management_requires_owner_session_and_csrf() {
    let (_root, app, _) = app().await;
    for (method, path) in [("GET", "/api/onepassword"), ("POST", "/api/onepassword")] {
        let response = app
            .clone()
            .oneshot(
                request(method, path, Value::Null)
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), 401);
    }
    let response = app
        .clone()
        .oneshot(
            request("POST", "/api/setup", Value::Null)
                .body(Body::from(
                    json!({
                        "setupToken": "test-setup",
                        "password": "password-long-enough",
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let cookie = response.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let body: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 10000).await.unwrap()).unwrap();
    let input = json!({
        "name": "Fixture",
        "token": "ops_http_fixture",
        "enabled": true,
        "agentIds": [],
    });
    let response = app
        .clone()
        .oneshot(
            request("POST", "/api/onepassword", Value::Null)
                .header("cookie", &cookie)
                .body(Body::from(input.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), 403);
    let response = app
        .clone()
        .oneshot(
            request("POST", "/api/onepassword", Value::Null)
                .header("cookie", &cookie)
                .header("x-csrf-token", body["csrf"].as_str().unwrap())
                .body(Body::from(input.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let bytes = to_bytes(response.into_body(), 10000).await.unwrap();
    assert!(!String::from_utf8_lossy(&bytes).contains("ops_http_fixture"));
    let response = app
        .oneshot(
            request("GET", "/api/onepassword", Value::Null)
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let bytes = to_bytes(response.into_body(), 10000).await.unwrap();
    assert!(!String::from_utf8_lossy(&bytes).contains("ops_http_fixture"));
}

#[tokio::test]
async fn github_projects_require_owner_session_and_csrf() {
    let (_root, app, _service) = app().await;
    for (method, path) in [
        ("GET", "/api/github/repositories"),
        ("POST", "/api/projects/github"),
    ] {
        let response = app
            .clone()
            .oneshot(
                request(method, path, Value::Null)
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), 401);
    }
    let response = app
        .clone()
        .oneshot(
            request("POST", "/api/setup", Value::Null)
                .body(Body::from(
                    json!({
                        "setupToken": "test-setup",
                        "password": "password-long-enough",
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let cookie = response.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let response = app
        .oneshot(
            request("POST", "/api/projects/github", Value::Null)
                .header("cookie", cookie)
                .body(Body::from("{\"repository\":\"fixture/repo\"}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), 403);
}

#[tokio::test]
async fn agents_default_to_unlimited_and_migrate_only_the_main_agents_old_default() {
    use leo_agent_manager::config::MAIN_AGENT_ID;
    let (_root, _app, s) = app().await;
    let main = s.get("agents", MAIN_AGENT_ID).await.unwrap();
    assert_eq!(main["timeoutMinutes"], 0);
    assert_eq!(
        s.agent(
            json!({
                "name": "New agent"
            }),
            None
        )
        .await
        .unwrap()["timeoutMinutes"],
        0
    );
    for minutes in [0, 1, 720] {
        assert_eq!(
            s.agent(
                json!({
                    "name": "Configured",
                    "timeoutMinutes": minutes
                }),
                None
            )
            .await
            .unwrap()["timeoutMinutes"],
            minutes
        );
    }
    for minutes in [-1, 721] {
        assert!(
            s.agent(
                json!({
                    "name": "Invalid",
                    "timeoutMinutes": minutes
                }),
                None
            )
            .await
            .is_err()
        );
    }
    for (previous, expected) in [(90, 90), (120, 0)] {
        s.agent(
            json!({
                "name": "Main agent",
                "timeoutMinutes": previous,
            }),
            Some(MAIN_AGENT_ID),
        )
        .await
        .unwrap();
        s.store
            .write(|db| db.delete("migration:main-agent-unlimited"))
            .await
            .unwrap();
        let restarted = Service::new(s.config.clone()).await.unwrap();
        assert_eq!(
            restarted.get("agents", MAIN_AGENT_ID).await.unwrap()["timeoutMinutes"],
            expected
        );
    }
    s.agent(
        json!({
            "name": "Main agent",
            "timeoutMinutes": 120,
        }),
        Some(MAIN_AGENT_ID),
    )
    .await
    .unwrap();
    let restarted = Service::new(s.config.clone()).await.unwrap();
    assert_eq!(
        restarted.get("agents", MAIN_AGENT_ID).await.unwrap()["timeoutMinutes"],
        120,
        "An explicit choice after migration must survive restart"
    );
}
