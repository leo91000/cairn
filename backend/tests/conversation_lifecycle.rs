use axum::{
    Router,
    body::{Body, to_bytes},
    http::Request,
};
use leo_agent_manager::{config::Config, http::router, service::Service};
use serde_json::{Value, json};
use std::sync::Arc;
use tempfile::TempDir;
use tower::ServiceExt;

struct App {
    _root: TempDir,
    router: Router,
    service: Arc<Service>,
    cookie: String,
    csrf: String,
}
impl App {
    async fn new() -> Self {
        let root = TempDir::new().unwrap();
        let service = Service::new(Config {
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
        })
        .await
        .unwrap();
        let router = router(service.clone()).await.unwrap();
        let session = service.auth.session().await.unwrap();
        Self {
            _root: root,
            router,
            service,
            cookie: format!("leo_session={}", session["value"].as_str().unwrap()),
            csrf: session["csrf"].as_str().unwrap().into(),
        }
    }
    async fn request(&self, method: &str, path: &str, body: Value) -> (u16, Value) {
        let response = self
            .router
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(path)
                    .header("host", "localhost:4310")
                    .header("content-type", "application/json")
                    .header("cookie", &self.cookie)
                    .header("x-csrf-token", &self.csrf)
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status().as_u16();
        let bytes = to_bytes(response.into_body(), 1_000_000).await.unwrap();
        (status, serde_json::from_slice(&bytes).unwrap())
    }
}

#[tokio::test]
async fn deleting_a_conversation_moves_it_to_trash_and_recovery_preserves_it() {
    let app = App::new().await;
    let (status, chat) = app.request("POST", "/api/chats", json!({})).await;
    assert_eq!(status, 200);
    let path = format!("/api/chats/{}", chat["id"].as_str().unwrap());
    let (status, deleted) = app.request("DELETE", &path, json!({})).await;
    assert_eq!(status, 200, "{deleted}");
    assert_eq!(deleted["lifecycle"], "trash");
    assert_eq!(
        deleted["purgeAt"].as_i64().unwrap() - deleted["trashedAt"].as_i64().unwrap(),
        30 * 86_400_000
    );
    let (_, active) = app.request("GET", "/api/chats", Value::Null).await;
    assert!(active.as_array().unwrap().is_empty());
    let (_, trash) = app
        .request("GET", "/api/chats?view=trash", Value::Null)
        .await;
    assert_eq!(trash[0]["id"], chat["id"]);
    let (status, restored) = app
        .request("POST", &format!("{path}/restore"), json!({}))
        .await;
    assert_eq!(status, 200, "{restored}");
    assert_eq!(restored["lifecycle"], "active");
    let (_, active) = app.request("GET", "/api/chats", Value::Null).await;
    assert_eq!(active[0]["id"], chat["id"]);
}

#[tokio::test]
async fn trash_requires_confirmation_for_pending_work_and_never_replays_cancelled_messages() {
    let app = App::new().await;
    let (_, chat) = app.request("POST", "/api/chats", json!({})).await;
    let path = format!("/api/chats/{}", chat["id"].as_str().unwrap());
    let message = json!({"id":leo_agent_manager::config::id(),"text":"Keep these instructions, do not execute them after recovery"});
    assert_eq!(
        app.request("POST", &format!("{path}/messages"), message)
            .await
            .0,
        200
    );
    assert_eq!(app.request("DELETE", &path, json!({})).await.0, 409);
    assert_eq!(
        app.request("DELETE", &path, json!({"confirm":true}))
            .await
            .0,
        200
    );
    assert_eq!(
        app.request(
            "POST",
            &format!("{path}/messages"),
            json!({"id":leo_agent_manager::config::id(),"text":"Must not dispatch"})
        )
        .await
        .0,
        409
    );
    assert_eq!(
        app.request("POST", &format!("{path}/pause"), json!({"paused":false}))
            .await
            .0,
        409
    );
    let (_, hidden) = app.request("GET", &path, Value::Null).await;
    assert!(hidden["messages"].as_array().unwrap().is_empty());
    assert_eq!(
        app.request("POST", &format!("{path}/restore"), json!({}))
            .await
            .0,
        200
    );
    let (_, restored) = app.request("GET", &path, Value::Null).await;
    assert_eq!(restored["messages"][0]["status"], "cancelled");
    assert_eq!(
        restored["messages"][0]["text"],
        "Keep these instructions, do not execute them after recovery"
    );
    app.service.chat_tick(&Default::default()).await.unwrap();
    let (_, after) = app.request("GET", &path, Value::Null).await;
    assert!(after["run"].is_null());
    assert_eq!(after["messages"][0]["status"], "cancelled");
}

#[tokio::test]
async fn trash_denies_direct_run_history_and_artifact_access() {
    let app = App::new().await;
    let (_, mut chat) = app.request("POST", "/api/chats", json!({})).await;
    let run = leo_agent_manager::config::id();
    let owned = run.clone();
    app.service.store.transaction(move |db| {
        db.0.execute("INSERT INTO runs(id,task_id,project_id,status,created_at,data) VALUES(?1,?1,'','succeeded',0,?2)", rusqlite::params![owned,json!({"id":owned,"status":"succeeded"}).to_string()])?;
        db.event(&owned, "chat.user", "Private transcript", None)?;
        Ok(())
    }).await.unwrap();
    chat["runId"] = run.clone().into();
    app.service.store.put("chats", chat.clone()).await.unwrap();
    let path = format!("/api/chats/{}", chat["id"].as_str().unwrap());
    assert_eq!(app.request("DELETE", &path, json!({})).await.0, 200);
    for suffix in ["", "/events", "/history", "/artifacts"] {
        let (status, _) = app
            .request("GET", &format!("/api/runs/{run}{suffix}"), Value::Null)
            .await;
        assert_eq!(status, 409, "run{suffix} must be inaccessible in trash");
    }
}

#[tokio::test]
async fn incompatible_restored_session_requires_consent_and_never_restarts_on_its_own() {
    let app = App::new().await;
    let (_, mut chat) = app.request("POST", "/api/chats", json!({})).await;
    let run = leo_agent_manager::config::id();
    chat["runId"] = run.clone().into();
    chat["restoredAt"] = 10.into();
    chat["paused"] = true.into();
    app.service.store.put("chats", chat.clone()).await.unwrap();
    let owned = run.clone();
    app.service.store.transaction(move |db| {
        db.0.execute("INSERT INTO runs(id,task_id,project_id,status,created_at,data) VALUES(?1,?1,'','failed',0,?2)",rusqlite::params![owned,json!({"id":owned,"status":"failed","sessionId":"old-native"}).to_string()])?;
        db.set(&format!("run-checkpoint:{owned}"),&json!({"prepared":{"workspace":"preserved"},"launched":true}),None)?;
        db.event(&owned,"chat.user","Context to preserve",None)?;
        Ok(())
    }).await.unwrap();
    let path = format!("/api/chats/{}/new-session", chat["id"].as_str().unwrap());
    assert_eq!(app.request("POST", &path, json!({})).await.0, 409);
    assert_eq!(
        app.request("POST", &path, json!({"confirm":true})).await.0,
        200
    );
    let pause = format!("/api/chats/{}/pause", chat["id"].as_str().unwrap());
    assert_eq!(
        app.request("POST", &pause, json!({"paused":false})).await.0,
        200
    );
    app.service.chat_tick(&Default::default()).await.unwrap();
    let (_, run) = app
        .request("GET", &format!("/api/runs/{run}"), Value::Null)
        .await;
    assert_ne!(run["status"], "queued");
    assert_ne!(run["status"], "running");
    assert!(run["sessionId"].is_null());
}

#[tokio::test]
async fn inactivity_never_archives_even_with_an_old_enabled_policy() {
    let app = App::new().await;
    let (_, mut chat) = app.request("POST", "/api/chats", json!({})).await;
    chat["updatedAt"] = 1.into();
    let cid = chat["id"].as_str().unwrap().to_owned();
    app.service.store.put("chats", chat).await.unwrap();
    app.service
        .store
        .set(
            "conversation-retention",
            json!({"enabled":true,"inactivityDays":1,"coldAfterDays":1}),
            None,
        )
        .await
        .unwrap();
    app.service.cleanup_conversations().await.unwrap();
    let (_, chat) = app
        .request("GET", &format!("/api/chats/{cid}"), Value::Null)
        .await;
    assert_eq!(chat["lifecycle"], "active");
    assert!(chat["archiveKey"].is_null());
    for method in ["GET", "PUT"] {
        assert_eq!(
            app.request(
                method,
                "/api/conversation-retention",
                json!({"enabled":true})
            )
            .await
            .0,
            404
        );
    }
    assert_eq!(
        app.request("GET", "/api/chats?view=archives", Value::Null)
            .await
            .0,
        400
    );
}

#[tokio::test]
async fn expired_trash_removes_files_and_records_without_archival() {
    let app = App::new().await;
    let (_, chat) = app.request("POST", "/api/chats", json!({})).await;
    let cid = chat["id"].as_str().unwrap();
    let directory = app
        .service
        .config
        .data_dir
        .join("chat-attachments")
        .join(cid);
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(directory.join("private.txt"), "private content").unwrap();
    let path = format!("/api/chats/{cid}");
    let (_, mut deleted) = app.request("DELETE", &path, json!({})).await;
    deleted["purgeAt"] = 1.into();
    app.service.store.put("chats", deleted).await.unwrap();
    assert_eq!(
        app.request("POST", &format!("{path}/restore"), json!({}))
            .await
            .0,
        410
    );
    app.service.cleanup_conversations().await.unwrap();
    assert_eq!(app.request("GET", &path, Value::Null).await.0, 404);
    assert!(!directory.exists());
}
