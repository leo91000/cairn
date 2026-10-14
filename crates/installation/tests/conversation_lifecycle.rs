mod common;

use common::relay_fixture::router;

use axum::{Router, body::Body, http::StatusCode};
use cairn_installation::{config::id, run_status::RunStatus, service::Service};
use common::RelayContext;
use serde_json::{Value, json};
use std::{collections::HashSet, sync::Arc};
use tempfile::TempDir;

const CHATS: &str = "/api/chats";

struct App {
    _root: TempDir,
    router: Router,
    service: Arc<Service>,
    session: RelayContext,
}

impl App {
    async fn new() -> Self {
        let root = TempDir::new().unwrap();
        let service = Service::new(common::config(root.path())).await.unwrap();
        let router = router(service.clone()).await.unwrap();
        let session = RelayContext::new(&common::relay_fixture::context(&service).await);
        Self {
            _root: root,
            router,
            service,
            session,
        }
    }

    /// Calls the API as the signed-in owner.
    async fn request(&self, method: &str, path: &str, body: Value) -> (StatusCode, Value) {
        let request = self
            .session
            .authorize(common::request(method, path))
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();
        let response = common::send(&self.router, request).await;
        (response.status(), common::read_json(response).await)
    }

    async fn get(&self, path: &str) -> (StatusCode, Value) {
        self.request("GET", path, Value::Null).await
    }

    async fn create_chat(&self) -> Value {
        self.request("POST", CHATS, json!({})).await.1
    }

    /// Lets the chat scheduler run once, with no conversation active.
    async fn tick(&self) {
        self.service.chat_tick(&HashSet::new()).await.unwrap();
    }

    /// Inserts a finished run, with its events, directly in the database.
    async fn add_finished_run(&self, run: &str, status: RunStatus, extra: Value, context: &str) {
        let (run, context) = (run.to_owned(), context.to_owned());
        let mut record = json!({ "id": run, "status": status });
        for (key, value) in extra.as_object().unwrap() {
            record[key] = value.clone();
        }
        self.service
            .store
            .transaction(move |db| {
                db.0.execute(
                    "INSERT INTO runs(id,task_id,project_id,status,created_at,data) VALUES(?1,?1,'',?2,0,?3)",
                    rusqlite::params![run, status.as_str(), record.to_string()],
                )?;
                db.event(&run, "chat.user", &context, None)?;
                Ok(())
            })
            .await
            .unwrap();
    }
}

fn chat_path(chat: &Value) -> String {
    format!("{CHATS}/{}", chat["id"].as_str().unwrap())
}

#[tokio::test]
async fn deleting_a_conversation_has_no_recovery_window() {
    let app = App::new().await;
    let (status, chat) = app.request("POST", CHATS, json!({})).await;
    assert_eq!(status, StatusCode::OK);
    let path = chat_path(&chat);
    let (status, deleted) = app.request("DELETE", &path, json!({})).await;
    assert_eq!(status, StatusCode::OK, "{deleted}");
    assert_eq!(deleted["lifecycle"], "trash");
    assert_eq!(
        deleted["purgeAt"].as_i64().unwrap() - deleted["trashedAt"].as_i64().unwrap(),
        0
    );
    let (_, active) = app.get(CHATS).await;
    assert!(active.as_array().unwrap().is_empty());
    let (_, trash) = app.get(&format!("{CHATS}?view=trash")).await;
    assert_eq!(trash[0]["id"], chat["id"]);
    let (status, restored) = app
        .request("POST", &format!("{path}/restore"), json!({}))
        .await;
    assert_eq!(status, StatusCode::GONE, "{restored}");
    app.service.cleanup_conversations().await.unwrap();
    assert_eq!(app.get(&path).await.0, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn deletion_requires_confirmation_for_pending_work_and_never_replays_cancelled_messages() {
    let app = App::new().await;
    let path = chat_path(&app.create_chat().await);
    let messages = format!("{path}/messages");
    let instructions = "Do not execute these instructions after deletion";
    let status_of =
        async |method: &str, path: &str, body: Value| app.request(method, path, body).await.0;
    assert_eq!(
        status_of(
            "POST",
            &messages,
            json!({ "id": id(), "text": instructions })
        )
        .await,
        StatusCode::OK
    );
    assert_eq!(
        status_of("DELETE", &path, json!({})).await,
        StatusCode::CONFLICT
    );
    assert_eq!(
        status_of("DELETE", &path, json!({ "confirm": true })).await,
        StatusCode::OK
    );
    assert_eq!(
        status_of(
            "POST",
            &messages,
            json!({ "id": id(), "text": "Must not dispatch" })
        )
        .await,
        StatusCode::CONFLICT
    );
    assert_eq!(
        status_of("POST", &format!("{path}/pause"), json!({ "paused": false })).await,
        StatusCode::CONFLICT
    );
    let (_, hidden) = app.get(&path).await;
    assert!(hidden["messages"].as_array().unwrap().is_empty());
    assert_eq!(
        status_of("POST", &format!("{path}/restore"), json!({})).await,
        StatusCode::GONE
    );
    app.tick().await;
    let (_, after) = app.get(&path).await;
    assert!(after["run"].is_null());
    app.service.cleanup_conversations().await.unwrap();
    assert_eq!(app.get(&path).await.0, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn trash_denies_direct_run_history_and_artifact_access() {
    let app = App::new().await;
    let mut chat = app.create_chat().await;
    let run = id();
    app.add_finished_run(&run, RunStatus::Succeeded, json!({}), "Private transcript")
        .await;
    chat["runId"] = run.clone().into();
    app.service.store.put("chats", chat.clone()).await.unwrap();
    let path = chat_path(&chat);
    assert_eq!(
        app.request("DELETE", &path, json!({})).await.0,
        StatusCode::OK
    );
    for suffix in ["", "/events", "/history", "/artifacts"] {
        let (status, _) = app.get(&format!("/api/runs/{run}{suffix}")).await;
        assert_eq!(
            status,
            StatusCode::CONFLICT,
            "run{suffix} must be inaccessible in trash"
        );
    }
}

#[tokio::test]
async fn cleanup_racing_cancellation_still_returns_successful_deletion() {
    let app = Arc::new(App::new().await);
    let mut chat = app.create_chat().await;
    let run = id();
    app.add_finished_run(&run, RunStatus::Succeeded, json!({}), "Finished work")
        .await;
    chat["runId"] = run.into();
    app.service.store.put("chats", chat.clone()).await.unwrap();
    let path = chat_path(&chat);
    let active = app.service.worker.active.lock().await;
    let deleting = app.clone();
    let deleting_path = path.clone();
    let response = tokio::spawn(async move {
        deleting
            .request("DELETE", &deleting_path, json!({ "confirm": true }))
            .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let current = app
                .service
                .store
                .get("chats", chat["id"].as_str().unwrap())
                .await
                .unwrap()
                .unwrap();
            if current["lifecycle"] == "trash" {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let cleaning = app.clone();
    let cleanup =
        tokio::spawn(async move { cleaning.service.cleanup_conversations().await.unwrap() });
    drop(active);
    let (status, deleted) = response.await.unwrap();
    assert_eq!(status, StatusCode::OK, "{deleted}");
    cleanup.await.unwrap();
    assert_eq!(app.get(&path).await.0, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn deletion_waits_for_running_agents_before_removing_their_files() {
    let app = App::new().await;
    let mut chat = app.create_chat().await;
    let run = id();
    app.add_finished_run(&run, RunStatus::Running, json!({}), "Work in progress")
        .await;
    chat["runId"] = run.clone().into();
    app.service.store.put("chats", chat.clone()).await.unwrap();
    let cancellation = tokio_util::sync::CancellationToken::new();
    app.service
        .worker
        .active
        .lock()
        .await
        .insert(run.clone(), cancellation.clone());
    let path = chat_path(&chat);
    assert_eq!(
        app.request("DELETE", &path, json!({ "confirm": true }))
            .await
            .0,
        StatusCode::OK
    );
    assert!(cancellation.is_cancelled());
    app.service.cleanup_conversations().await.unwrap();
    assert_eq!(app.get(&path).await.0, StatusCode::OK);
    let (_, retained) = app.get(&format!("{CHATS}?view=trash")).await;
    assert_eq!(retained[0]["id"], chat["id"]);
    assert_eq!(
        app.request("POST", &format!("{path}/restore"), json!({}))
            .await
            .0,
        StatusCode::GONE
    );
    app.service
        .store
        .patch_run(&run, json!({ "status": RunStatus::Succeeded }))
        .await
        .unwrap();
    app.service.cleanup_conversations().await.unwrap();
    assert_eq!(
        app.get(&path).await.0,
        StatusCode::OK,
        "Worker finalization must finish before cleanup"
    );
    app.service
        .worker
        .active
        .lock()
        .await
        .remove(chat["runId"].as_str().unwrap());
    app.service.cleanup_conversations().await.unwrap();
    assert_eq!(app.get(&path).await.0, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn incompatible_restored_session_requires_consent_and_never_restarts_on_its_own() {
    let app = App::new().await;
    let mut chat = app.create_chat().await;
    let run = id();
    chat["runId"] = run.clone().into();
    chat["restoredAt"] = 10.into();
    chat["paused"] = true.into();
    app.service.store.put("chats", chat.clone()).await.unwrap();
    app.add_finished_run(
        &run,
        RunStatus::Failed,
        json!({ "sessionId": "old-native" }),
        "Context to preserve",
    )
    .await;
    common::set_checkpoint(
        &app.service.store,
        &run,
        json!({ "prepared": { "workspace": "preserved" }, "launched": true }),
    )
    .await;
    let path = format!("{}/new-session", chat_path(&chat));
    assert_eq!(
        app.request("POST", &path, json!({})).await.0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        app.request("POST", &path, json!({ "confirm": true }))
            .await
            .0,
        StatusCode::OK
    );
    let pause = format!("{}/pause", chat_path(&chat));
    assert_eq!(
        app.request("POST", &pause, json!({ "paused": false }))
            .await
            .0,
        StatusCode::OK
    );
    app.tick().await;
    let (_, run) = app.get(&format!("/api/runs/{run}")).await;
    assert_ne!(run["status"], RunStatus::Queued);
    assert_ne!(run["status"], RunStatus::Running);
    assert!(run["sessionId"].is_null());
}

#[tokio::test]
async fn inactivity_never_archives_even_with_an_old_enabled_policy() {
    let app = App::new().await;
    let mut chat = app.create_chat().await;
    chat["updatedAt"] = 1.into();
    let path = chat_path(&chat);
    app.service.store.put("chats", chat).await.unwrap();
    let policy = json!({ "enabled": true, "inactivityDays": 1, "coldAfterDays": 1 });
    app.service
        .store
        .set("conversation-retention", policy, None)
        .await
        .unwrap();
    app.service.cleanup_conversations().await.unwrap();
    let (_, chat) = app.get(&path).await;
    assert_eq!(chat["lifecycle"], "active");
    assert!(chat["archiveKey"].is_null());
    for method in ["GET", "PUT"] {
        assert_eq!(
            app.request(
                method,
                "/api/conversation-retention",
                json!({ "enabled": true })
            )
            .await
            .0,
            StatusCode::NOT_FOUND
        );
    }
    assert_eq!(
        app.get(&format!("{CHATS}?view=archives")).await.0,
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn immediate_deletion_removes_files_and_records_without_archival() {
    let app = App::new().await;
    let chat = app.create_chat().await;
    let directory = app
        .service
        .config
        .data_dir
        .join("chat-attachments")
        .join(chat["id"].as_str().unwrap());
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(directory.join("private.txt"), "private content").unwrap();
    let path = chat_path(&chat);
    app.request("DELETE", &path, json!({})).await;
    assert_eq!(
        app.request("POST", &format!("{path}/restore"), json!({}))
            .await
            .0,
        StatusCode::GONE
    );
    app.service.cleanup_conversations().await.unwrap();
    assert_eq!(app.get(&path).await.0, StatusCode::NOT_FOUND);
    assert!(!directory.exists());
}

#[tokio::test]
async fn readable_slugs_resolve_live_requests_and_keep_uuid_links_and_lifecycle() {
    let app = App::new().await;
    let chat = app.create_chat().await;
    let slug = chat["slug"].as_str().unwrap();
    let path = format!("{CHATS}/{slug}");
    assert_ne!(chat["id"], chat["slug"]);
    assert_eq!(slug.split('-').count(), 3);
    assert_eq!(app.get(&path).await.1["id"], chat["id"]);
    assert_eq!(app.get(&chat_path(&chat)).await.1["slug"], slug);

    let response = common::send(
        &app.router,
        app.session
            .authorize(common::request("GET", &format!("{path}/stream?after=0")))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    drop(response);

    let (_, message) = app
        .request(
            "POST",
            &format!("{path}/messages"),
            json!({
                "id": id(),
                "text": "A readable conversation link",
            }),
        )
        .await;
    assert_eq!(message["chatId"], chat["id"]);
    assert_eq!(app.get(&path).await.1["slug"], slug);
    assert_eq!(
        app.get("/api/chats/quiet-cedar-00000000").await.0,
        StatusCode::NOT_FOUND
    );

    assert_eq!(
        app.request("DELETE", &path, json!({ "confirm": true }))
            .await
            .1["lifecycle"],
        "trash"
    );
    assert_eq!(
        app.request("POST", &format!("{path}/restore"), json!({}))
            .await
            .0,
        StatusCode::GONE
    );
    assert_eq!(app.get(&path).await.1["slug"], slug);
}

#[tokio::test]
async fn previously_trashed_conversations_keep_their_recovery_deadline() {
    let app = App::new().await;
    let mut chat = app.create_chat().await;
    let path = chat_path(&chat);
    chat["lifecycle"] = "trash".into();
    chat["trashedAt"] = cairn_installation::config::now().into();
    chat["purgeAt"] = (cairn_installation::config::now() + 30 * 86_400_000).into();
    app.service.store.put("chats", chat).await.unwrap();
    app.service.cleanup_conversations().await.unwrap();
    let (status, restored) = app
        .request("POST", &format!("{path}/restore"), json!({}))
        .await;
    assert_eq!(status, StatusCode::OK, "{restored}");
    assert_eq!(restored["lifecycle"], "active");
}

#[tokio::test]
async fn legacy_conversations_get_stable_slugs_without_changing_activity_or_uuid() {
    use cairn_installation::store::Store;
    let root = TempDir::new().unwrap();
    let store = Store::open(root.path()).unwrap();
    let chat_id = id();
    let legacy = json!({ "id": chat_id, "title": "Legacy conversation", "updatedAt": 123 });
    let record = legacy.clone();
    store
        .write(move |db| {
            db.0.execute(
                "INSERT INTO records VALUES(?1,'chats',?2,123)",
                rusqlite::params![record["id"].as_str(), record.to_string()],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    drop(store);

    let store = Store::open(root.path()).unwrap();
    let migrated = store.get("chats", &chat_id).await.unwrap().unwrap();
    assert_eq!(migrated["id"], legacy["id"]);
    assert_eq!(migrated["updatedAt"], 123);
    let timestamp = store
        .read(|db| {
            Ok(db.0.query_row(
                "SELECT updated_at FROM records WHERE kind='chats'",
                [],
                |row| row.get::<_, i64>(0),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(timestamp, 123);
    let mut edited = migrated.clone();
    edited["slug"] = "changed-slug".into();
    edited["title"] = "Renamed conversation".into();
    let saved = store.put("chats", edited).await.unwrap();
    assert_eq!(saved["slug"], migrated["slug"]);
    drop(store);

    let reopened = Store::open(root.path()).unwrap();
    assert_eq!(
        reopened.get("chats", &chat_id).await.unwrap().unwrap()["slug"],
        migrated["slug"]
    );
}

#[tokio::test]
async fn chat_aliases_keep_json_body_limits_before_lookup_and_live_subscription() {
    let app = App::new().await;
    let chat = app.create_chat().await;
    let known = chat["slug"].as_str().unwrap();
    let body = json!({ "id": id(), "text": "x".repeat(1_000_000) });
    for key in [known, "missing", "quiet-cedar-00000000"] {
        for suffix in ["messages", "stream"] {
            let (status, _) = app
                .request("POST", &format!("{CHATS}/{key}/{suffix}"), body.clone())
                .await;
            assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE, "{key}/{suffix}");
        }
    }
    assert_eq!(
        app.get(&format!("{CHATS}/quiet-cedar-00000000")).await.0,
        StatusCode::NOT_FOUND
    );
}
