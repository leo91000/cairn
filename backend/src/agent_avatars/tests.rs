use super::*;
use crate::config::Config;
use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
    routing::post,
};
use tempfile::TempDir;
use tokio::sync::{mpsc, oneshot};
use tower::ServiceExt;

type Reply = oneshot::Sender<(StatusCode, Json<Value>)>;

/// Synthetic Codex credentials whose account ID routes the fixture to `endpoint`.
fn tokens(endpoint: &str) -> Value {
    json!({
        "tokens": {
            "access_token": "synthetic",
            "refresh_token": "synthetic-refresh",
            "account_id": endpoint,
        },
    })
}

/// A queued conversation, which background work yields to.
fn foreground_run() -> Value {
    json!({ "id": "foreground", "taskId": "chat", "status": "queued", "createdAt": 1 })
}

struct Fixture {
    _root: TempDir,
    s: Arc<Service>,
    app: Router,
    cookie: String,
    csrf: String,
    requests: mpsc::UnboundedReceiver<(Value, Reply)>,
    provider: tokio::task::JoinHandle<()>,
    endpoint: String,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.s.shutdown.cancel();
        self.provider.abort();
    }
}

impl Fixture {
    async fn new(configured: bool) -> Self {
        let root = TempDir::new().unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/images", listener.local_addr().unwrap());
        let (tx, requests) = mpsc::unbounded_channel();
        let mock = Router::new().route(
            "/images",
            post(move |Json(input): Json<Value>| {
                let tx = tx.clone();
                async move {
                    let (reply, response) = oneshot::channel();
                    tx.send((input, reply)).unwrap();
                    response.await.unwrap()
                }
            }),
        );
        let provider = tokio::spawn(async move { axum::serve(listener, mock).await.unwrap() });
        let codex = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../tests/fixtures/avatar-codex.mjs");
        let config: Config = serde_json::from_value(json!({
            "dataDir": root.path().join("data"),
            "home": root.path().join("home"),
            "workspaceRoots": [root.path()],
            "publicUrl": "http://localhost:4310",
            "host": "127.0.0.1",
            "port": 0,
            "setupToken": "test",
            "codexBin": codex,
            "ghBin": "gh",
            "concurrency": 1,
            "logger": false,
            "workerEnabled": false,
            "runnerUrl": "",
        }))
        .unwrap();
        let s = Service::new(config).await.unwrap();
        s.accounts.initialize(&s).await.unwrap();
        if configured {
            let account = s
                .accounts
                .create(&s, Provider::Codex, "Portrait fixture")
                .await
                .unwrap();
            s.vault
                .set(
                    &format!("codex-account:{}", text(&account, "id")),
                    &tokens(&endpoint),
                )
                .await
                .unwrap();
            s.accounts.refresh(&s, text(&account, "id")).await.unwrap();
        }
        let app = crate::http::router(s.clone()).await.unwrap();
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/setup")
                    .header("host", "localhost:4310")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({ "setupToken": "test", "password": "portrait-test-password" })
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
        let csrf = body(response).await["csrf"].as_str().unwrap().to_owned();
        Self {
            _root: root,
            s,
            app,
            cookie,
            csrf,
            requests,
            provider,
            endpoint,
        }
    }

    async fn request(&self, method: &str, path: &str, bytes: Vec<u8>) -> Response {
        self.app
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(path)
                    .header("host", "localhost:4310")
                    .header("cookie", &self.cookie)
                    .header("x-csrf-token", &self.csrf)
                    .header("content-type", "application/json")
                    .body(Body::from(bytes))
                    .unwrap(),
            )
            .await
            .unwrap()
    }

    async fn json(&self, method: &str, path: &str, input: Value) -> Value {
        let response = self
            .request(method, path, input.to_string().into_bytes())
            .await;
        assert_eq!(response.status(), 200);
        body(response).await
    }

    async fn create(&self) -> Value {
        let agent = json!({
            "name": "Release engineer",
            "description": "Ship tested software",
            "instructions": "PRIVATE INSTRUCTIONS NEVER SENT",
        });
        self.json("POST", "/api/agents", agent).await
    }

    async fn wait(&self, id: &str, status: &str) -> Value {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let agent = self.s.get("agents", id).await.unwrap();
                if agent["avatar"]["status"] == status {
                    return agent;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap()
    }

    async fn next(&mut self) -> (Value, Reply) {
        tokio::time::timeout(Duration::from_secs(5), self.requests.recv())
            .await
            .unwrap()
            .unwrap()
    }
}

async fn body(response: Response) -> Value {
    serde_json::from_slice(&to_bytes(response.into_body(), MAX_RESPONSE).await.unwrap()).unwrap()
}

fn png(color: [u8; 3]) -> Vec<u8> {
    let mut bytes = Cursor::new(Vec::new());
    image::RgbImage::from_pixel(32, 48, image::Rgb(color))
        .write_to(&mut bytes, image::ImageFormat::Png)
        .unwrap();
    bytes.into_inner()
}

fn generated(bytes: Vec<u8>) -> (StatusCode, Json<Value>) {
    (
        StatusCode::OK,
        Json(json!({
            "type": "imageGeneration",
            "id": "image-1",
            "status": "completed",
            "result": STANDARD.encode(bytes),
        })),
    )
}

#[tokio::test]
async fn creation_is_nonblocking_and_portrait_survives_edits_with_private_authenticated_delivery() {
    let mut f = Fixture::new(true).await;
    let agent = f.create().await;
    let id = text(&agent, "id");
    assert_eq!(agent["avatar"]["status"], "generating");
    let (request, reply) = f.next().await;
    assert_eq!(request["threadId"], "portrait-thread");
    assert!(text(&request["input"][0], "text").contains("Ship tested software"));
    assert!(!text(&request["input"][0], "text").contains("PRIVATE INSTRUCTIONS"));
    assert_eq!(
        f.request("POST", &format!("/api/agents/{id}/avatar/generate"), vec![])
            .await
            .status(),
        409
    );
    f.json(
        "PUT",
        &format!("/api/agents/{id}"),
        json!({ "name": "Renamed agent" }),
    )
    .await;
    reply.send(generated(png([50, 100, 180]))).unwrap();
    let ready = f.wait(id, "ready").await;
    assert_eq!(ready["name"], "Renamed agent");
    for account in f.s.accounts.list(&f.s).await.unwrap() {
        assert!(f.s.accounts.active(text(&account, "id")).await.is_empty());
    }
    let edited = f
        .json(
            "PUT",
            &format!("/api/agents/{id}"),
            json!({ "name": "New role", "avatar": { "url": "https://evil.invalid" } }),
        )
        .await;
    assert_eq!(edited["avatar"], ready["avatar"]);
    assert!(f.requests.try_recv().is_err());
    let url = text(&ready["avatar"], "url");
    let response = f.request("GET", url, vec![]).await;
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["cache-control"], "no-store");
    assert_eq!(response.headers()["content-type"], "image/png");
    let bytes = to_bytes(response.into_body(), MAX_UPLOAD).await.unwrap();
    let image = image::load_from_memory(&bytes).unwrap();
    assert_eq!((image.width(), image.height()), (256, 256));
    let response = f
        .app
        .clone()
        .oneshot(
            Request::builder()
                .uri(url)
                .header("host", "localhost:4310")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), 401);
    let response = f
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(url)
                .header("host", "localhost:4310")
                .header("cookie", &f.cookie)
                .body(Body::from(png([0, 0, 0])))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), 403);
}

#[tokio::test]
async fn upload_wins_over_generation_and_invalid_images_preserve_the_previous_portrait() {
    let mut f = Fixture::new(true).await;
    let idle_refs = Arc::strong_count(&f.s);
    let agent = f.create().await;
    let id = text(&agent, "id");
    let (_, reply) = f.next().await;
    let path = format!("/api/agents/{id}/avatar");
    let uploaded = f.request("PUT", &path, png([255, 0, 0])).await;
    assert_eq!(uploaded.status(), 200);
    let uploaded = body(uploaded).await;
    reply.send(generated(png([0, 0, 255]))).unwrap();
    // Wait for both the provider request and its conditional completion to finish.
    tokio::time::timeout(Duration::from_secs(5), async {
        while Arc::strong_count(&f.s) > idle_refs {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        f.s.get("agents", id).await.unwrap()["avatar"],
        uploaded["avatar"]
    );
    for bytes in [
        b"<svg onload='bad()'/>".to_vec(),
        b"\x89PNG\r\n\x1a\ncorrupt".to_vec(),
    ] {
        assert_eq!(f.request("PUT", &path, bytes).await.status(), 400);
    }
    let mut oversized = Cursor::new(Vec::new());
    image::RgbImage::new(1, 4097)
        .write_to(&mut oversized, image::ImageFormat::Png)
        .unwrap();
    assert_eq!(
        f.request("PUT", &path, oversized.into_inner())
            .await
            .status(),
        400
    );
    assert_eq!(
        f.request("PUT", &path, vec![0; MAX_UPLOAD + 1])
            .await
            .status(),
        413
    );
    let bytes = to_bytes(
        f.request("GET", &path, vec![]).await.into_body(),
        MAX_UPLOAD,
    )
    .await
    .unwrap();
    assert_eq!(
        image::load_from_memory(&bytes)
            .unwrap()
            .to_rgb8()
            .get_pixel(0, 0)
            .0,
        [255, 0, 0]
    );
}

#[tokio::test]
async fn regeneration_failure_keeps_the_image_and_does_not_expose_provider_errors() {
    let mut f = Fixture::new(true).await;
    let agent = f.create().await;
    let id = text(&agent, "id");
    f.next().await.1.send(generated(png([0, 120, 0]))).unwrap();
    let ready = f.wait(id, "ready").await;
    let pending = f
        .json(
            "POST",
            &format!("/api/agents/{id}/avatar/generate"),
            Value::Null,
        )
        .await;
    assert_eq!(pending["avatar"]["url"], ready["avatar"]["url"]);
    f.next()
        .await
        .1
        .send((
            StatusCode::TOO_MANY_REQUESTS,
            Json(json!({ "error": "SECRET PROVIDER RESPONSE" })),
        ))
        .unwrap();
    let failed = f.wait(id, "failed").await;
    assert_eq!(failed["avatar"]["url"], ready["avatar"]["url"]);
    assert!(!failed.to_string().contains("SECRET PROVIDER RESPONSE"));
    assert!(f.requests.try_recv().is_err());
    f.json(
        "POST",
        &format!("/api/agents/{id}/avatar/generate"),
        Value::Null,
    )
    .await;
    let (_, reply) = f.next().await;
    f.json("DELETE", &format!("/api/agents/{id}"), Value::Null)
        .await;
    reply.send(generated(png([0, 0, 0]))).unwrap();
    assert!(f.s.store.get("agents", id).await.unwrap().is_none());
    assert!(
        f.s.store
            .kv(&format!("agent-avatar:{id}"))
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn uploads_work_without_provider_configuration_and_restart_never_rebills() {
    let f = Fixture::new(false).await;
    assert_eq!(
        f.json("GET", "/api/agent-avatars", Value::Null).await["configured"],
        false
    );
    let agent = f.create().await;
    let id = text(&agent, "id");
    assert!(agent["avatar"].is_null());
    assert_eq!(
        f.request("POST", &format!("/api/agents/{id}/avatar/generate"), vec![])
            .await
            .status(),
        503
    );
    let path = format!("/api/agents/{id}/avatar");
    let uploaded = body(f.request("PUT", &path, png([120, 80, 40])).await).await;
    let mut interrupted = uploaded.clone();
    interrupted["avatar"]["status"] = "generating".into();
    f.s.store.put("agents", interrupted).await.unwrap();
    let restarted = Service::new(f.s.config.clone()).await.unwrap();
    let recovered = restarted.get("agents", id).await.unwrap();
    assert_eq!(recovered["avatar"]["status"], "failed");
    assert_eq!(recovered["avatar"]["url"], uploaded["avatar"]["url"]);
    assert!(
        restarted
            .store
            .kv(&format!("agent-avatar:{id}"))
            .await
            .unwrap()
            .is_some()
    );
    restarted.shutdown.cancel();
}

#[tokio::test]
async fn missing_or_invalid_images_fail_without_retry_and_release_the_account() {
    let mut f = Fixture::new(true).await;
    for item in [
        json!({ "type": "agentMessage", "text": "Here is an SVG instead" }),
        json!({ "type": "imageGeneration", "status": "completed", "result": "not base64" }),
        json!({
            "type": "imageGeneration",
            "status": "completed",
            "result": STANDARD.encode(b"not an image"),
        }),
        json!({
            "type": "imageGeneration",
            "status": "failed",
            "result": "",
            "failure": { "type": "usageLimitExceeded" },
        }),
    ] {
        let agent = f.create().await;
        f.next().await.1.send((StatusCode::OK, Json(item))).unwrap();
        let failed = f.wait(text(&agent, "id"), "failed").await;
        assert!(failed["avatar"]["url"].is_null());
        assert!(f.requests.try_recv().is_err());
        for account in f.s.accounts.list(&f.s).await.unwrap() {
            assert!(f.s.accounts.active(text(&account, "id")).await.is_empty());
        }
    }
}

#[tokio::test]
async fn close_drains_running_and_queued_portraits_and_rejects_new_jobs() {
    let mut f = Fixture::new(true).await;
    let first = f.create().await;
    let (first_request, first_reply) = f.next().await;
    let second = f.create().await;
    let (second_request, second_reply) = f.next().await;
    let queued = f.create().await;
    tokio::time::timeout(Duration::from_secs(5), f.s.avatars.close())
        .await
        .unwrap();
    // close itself must wait for metadata, process exit and lease release; do not poll.
    for agent in [&first, &second, &queued] {
        let failed = f.s.get("agents", text(agent, "id")).await.unwrap();
        assert_eq!(failed["avatar"]["status"], "failed");
        assert_eq!(failed["avatar"]["error"], INTERRUPTED);
    }
    for request in [&first_request, &second_request] {
        let pid = request["fixturePid"].as_i64().unwrap() as i32;
        assert_eq!(
            unsafe { libc::kill(pid, 0) },
            -1,
            "Codex process is still alive after close"
        );
    }
    for account in f.s.store.list(crate::accounts::KIND).await.unwrap() {
        assert!(f.s.accounts.active(text(&account, "id")).await.is_empty());
    }
    assert!(
        tokio::fs::read_dir(f.s.config.data_dir.join("runs"))
            .await
            .unwrap()
            .next_entry()
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        f.request(
            "POST",
            &format!("/api/agents/{}/avatar/generate", text(&first, "id")),
            vec![]
        )
        .await
        .status(),
        503
    );
    let _ = first_reply.send(generated(png([0, 0, 0])));
    let _ = second_reply.send(generated(png([0, 0, 0])));
    assert!(f.requests.try_recv().is_err());
}

#[tokio::test]
async fn occupied_account_capacity_does_not_start_a_second_codex_session() {
    let mut f = Fixture::new(true).await;
    let mut account =
        f.s.store
            .list(crate::accounts::KIND)
            .await
            .unwrap()
            .remove(0);
    account["maxConcurrentRuns"] = 1.into();
    f.s.store
        .put(crate::accounts::KIND, account.clone())
        .await
        .unwrap();
    let foreground =
        f.s.accounts
            .acquire(&f.s, "foreground", Provider::Codex, "")
            .await
            .unwrap()
            .unwrap();
    let agent = f.create().await;
    let failed = f.wait(text(&agent, "id"), "failed").await;
    assert!(text(&failed["avatar"], "error").contains("No Codex account is available"));
    assert!(f.requests.try_recv().is_err());
    assert_eq!(f.s.accounts.active(text(&account, "id")).await.len(), 1);
    f.s.accounts.release(&foreground).await.unwrap();
}

#[tokio::test]
async fn portrait_selection_respects_the_explicit_models_quota() {
    let mut f = Fixture::new(true).await;
    let mut limited =
        f.s.store
            .list(crate::accounts::KIND)
            .await
            .unwrap()
            .remove(0);
    limited["usage"]["windows"]
        .as_array_mut()
        .unwrap()
        .push(json!({
            "id": "astra:weekly",
            "usedPercent": 100,
            "models": ["gpt-6-astra"],
            "reached": true,
        }));
    f.s.store
        .put(crate::accounts::KIND, limited.clone())
        .await
        .unwrap();
    let available =
        f.s.accounts
            .create(&f.s, Provider::Codex, "Available")
            .await
            .unwrap();
    f.s.vault
        .set(
            &format!("codex-account:{}", text(&available, "id")),
            &tokens(&format!("{}?available", f.endpoint)),
        )
        .await
        .unwrap();
    f.s.accounts
        .refresh(&f.s, text(&available, "id"))
        .await
        .unwrap();
    let agent = f.create().await;
    let (request, reply) = f.next().await;
    assert_eq!(request["model"], "gpt-6-astra");
    assert!(f.s.accounts.active(text(&limited, "id")).await.is_empty());
    let leases = f.s.accounts.active(text(&available, "id")).await;
    assert_eq!(leases.len(), 1);
    assert_eq!(leases[0].model, "gpt-6-astra");
    reply.send(generated(png([10, 20, 30]))).unwrap();
    f.wait(text(&agent, "id"), "ready").await;
}

#[tokio::test]
async fn queued_conversation_interrupts_portrait_and_gets_the_single_account_slot() {
    let mut f = Fixture::new(true).await;
    let mut account =
        f.s.store
            .list(crate::accounts::KIND)
            .await
            .unwrap()
            .remove(0);
    account["maxConcurrentRuns"] = 1.into();
    f.s.store
        .put(crate::accounts::KIND, account.clone())
        .await
        .unwrap();
    let agent = f.create().await;
    let (_, reply) = f.next().await;
    f.s.store
        .transaction(|db| {
            db.add_run(&foreground_run(), None)?;
            Ok(())
        })
        .await
        .unwrap();
    let failed = f.wait(text(&agent, "id"), "failed").await;
    assert!(text(&failed["avatar"], "error").contains("yielded to a conversation"));
    let foreground =
        f.s.accounts
            .acquire(&f.s, "foreground", Provider::Codex, "gpt-6-astra")
            .await
            .unwrap()
            .unwrap();
    assert_eq!(foreground.account_id, account["id"].as_str().unwrap());
    f.s.accounts.release(&foreground).await.unwrap();
    let _ = reply.send(generated(png([0, 0, 0])));
    assert!(f.requests.try_recv().is_err());
}

#[tokio::test]
async fn foreground_queue_and_deployment_prevent_starting_a_portrait() {
    for deployment in [false, true] {
        let mut f = Fixture::new(true).await;
        if deployment {
            f.s.store
                .set("deployment-lease", "deploy".into(), None)
                .await
                .unwrap();
        } else {
            f.s.store
                .transaction(|db| {
                    db.add_run(&foreground_run(), None)?;
                    Ok(())
                })
                .await
                .unwrap();
        }
        let agent = f.create().await;
        let failed = f.wait(text(&agent, "id"), "failed").await;
        assert!(text(&failed["avatar"], "error").contains("yielded"));
        assert!(f.requests.try_recv().is_err());
        for account in f.s.store.list(crate::accounts::KIND).await.unwrap() {
            assert!(f.s.accounts.active(text(&account, "id")).await.is_empty());
        }
    }
}
