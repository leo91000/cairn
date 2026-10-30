use crate::common::run_fixture::{DbRunFixture, RunFixture};
use cairn_contracts::EventKind;
use common::account_fixture::AccountFixtures;
use common::node_fixture::*;

mod common;

use common::relay_fixture::router;

use axum::{Router, body::Body, http::StatusCode};
use cairn_installation::{config::id, run_status::RunStatus, service::Service, validation::text};
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
        Self::with_public_url(None).await
    }

    async fn with_public_url(public_url: Option<String>) -> Self {
        let root = TempDir::new().unwrap();
        let mut config = common::config(root.path());
        if let Some(public_url) = public_url {
            config.public_url = public_url;
        }
        let service = Service::new(config).await.unwrap();
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
        let (status, view) = self.request("POST", CHATS, json!({})).await;
        assert_eq!(status, StatusCode::OK, "{view}");
        // Fixture mutations retain the stored authority snapshot. The generated
        // public view deliberately does not expose internal directing grants.
        self.service
            .store
            .conversation_boundary(text(&view, "id"))
            .await
            .unwrap()
            .unwrap()
    }

    /// Lets the chat scheduler run once, with no conversation active.
    async fn tick(&self) {
        self.service.chat_tick(&HashSet::new()).await.unwrap();
    }

    /// Inserts a finished run, with its events, directly in the database.
    async fn add_finished_run(&self, run: &str, status: RunStatus, extra: Value, context: &str) {
        let (run, context) = (run.to_owned(), context.to_owned());
        let mut record = json!({
            "id": run,
            "taskId": run,
            "createdAt": 0,
            "status": status,
            "nativeProvenanceUnknown": true,
            "nativePrincipals": [],
            "nativeAuthorityNodes": [],
        });
        for (key, value) in extra.as_object().unwrap() {
            record[key] = value.clone();
        }
        self.service
            .store
            .transaction(move |db| {
                db.add_run_fixture(&record, None)?;
                db.event(&run, &EventKind::ChatUser, &context, None)?;
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
    app.service
        .store
        .save_conversation_boundary(chat.clone())
        .await
        .unwrap();
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
    app.service
        .store
        .save_conversation_boundary(chat.clone())
        .await
        .unwrap();
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
                .conversation_boundary(chat["id"].as_str().unwrap())
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
    app.service
        .store
        .save_conversation_boundary(chat.clone())
        .await
        .unwrap();
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
        .change_run(
            &run,
            cairn_installation::runs::RunChanges {
                status: Some(RunStatus::Succeeded),
                ..Default::default()
            },
        )
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
    chat["paused"] = true.into();
    app.add_finished_run(
        &run,
        RunStatus::Failed,
        json!({ "sessionId": "old-native", "restoredAt": 10 }),
        "Context to preserve",
    )
    .await;
    app.service
        .store
        .save_conversation_boundary(chat.clone())
        .await
        .unwrap();
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
async fn fresh_session_availability_uses_restored_runs_and_keeps_its_lifetime() {
    for historical_chat in [false, true] {
        let app = App::new().await;
        let mut chat = app.create_chat().await;
        let run = id();
        chat["runId"] = run.clone().into();
        app.add_finished_run(&run, RunStatus::Failed, json!({}), "History")
            .await;
        app.service
            .store
            .save_conversation_boundary(chat.clone())
            .await
            .unwrap();
        if historical_chat {
            app.service
                .store
                .update_run_boundary(&run, json!({ "restoredAt": 10 }))
                .await
                .unwrap();
        }
        let path = chat_path(&chat);
        assert_eq!(
            app.get(&path).await.1["canRequestNewSession"],
            historical_chat
        );
        if !historical_chat {
            assert_eq!(
                app.request(
                    "POST",
                    &format!("{path}/new-session"),
                    json!({ "confirm": true })
                )
                .await
                .0,
                StatusCode::CONFLICT
            );
            app.service
                .store
                .update_run_boundary(&run, json!({ "restoredAt": 10 }))
                .await
                .unwrap();
        }
        for (status, available) in [
            ("failed", true),
            ("succeeded", false),
            ("running", false),
            ("interrupted", true),
            ("failed", true),
        ] {
            app.service
                .store
                .change_run(
                    &run,
                    cairn_installation::runs::RunChanges {
                        status: Some(RunStatus::parse(status).unwrap()),
                        ..Default::default()
                    },
                )
                .await
                .unwrap();
            assert_eq!(app.get(&path).await.1["canRequestNewSession"], available);
            assert_eq!(app.get(CHATS).await.1[0]["canRequestNewSession"], available);
        }
        // No checkpoint access is needed to record the durable request.
        assert_eq!(
            app.request(
                "POST",
                &format!("{path}/new-session"),
                json!({ "confirm": true })
            )
            .await
            .0,
            StatusCode::OK
        );
        assert_eq!(app.get(&path).await.1["canRequestNewSession"], false);
        // A lost HTTP response can cause the same confirmation to be retried.
        let (status, pending) = app
            .request(
                "POST",
                &format!("{path}/new-session"),
                json!({ "confirm": true }),
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(pending["sessionRestartRequested"], true);
        assert_eq!(pending["paused"], true);
        let mut chat = app
            .service
            .store
            .conversation_boundary(text(&chat, "id"))
            .await
            .unwrap()
            .unwrap();
        chat["lifecycle"] = "trash".into();
        app.service
            .store
            .save_conversation_boundary(chat)
            .await
            .unwrap();
        assert_eq!(app.get(&path).await.1["canRequestNewSession"], false);
    }
}

#[tokio::test]
async fn inactivity_never_archives_even_with_an_old_enabled_policy() {
    let app = App::new().await;
    let mut chat = app.create_chat().await;
    chat["updatedAt"] = (cairn_installation::config::now() - 2 * 86_400_000).into();
    let path = chat_path(&chat);
    app.service
        .store
        .save_conversation_boundary(chat)
        .await
        .unwrap();
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
async fn permanent_revocation_does_not_block_requested_conversation_erasure() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    for (revoked, recorded_volume) in [
        (Some(true), true),
        (Some(true), false),
        (Some(false), true),
        (None, true),
    ] {
        let requests = Arc::new(AtomicUsize::new(0));
        let observed = requests.clone();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let public_url = format!("http://{}", listener.local_addr().unwrap());
        let backend = Router::new().fallback(move || {
            let observed = observed.clone();
            async move {
                observed.fetch_add(1, Ordering::SeqCst);
                StatusCode::SERVICE_UNAVAILABLE
            }
        });
        let server = tokio::spawn(async move { axum::serve(listener, backend).await.unwrap() });
        let app = App::with_public_url(Some(public_url)).await;
        let mut chat = app.create_chat().await;
        let run = id();
        let node = id();
        let grant = id();
        app.add_finished_run(
            &run,
            RunStatus::Succeeded,
            json!({ "isolated": true }),
            "retired work",
        )
        .await;
        chat["runId"] = run.clone().into();
        app.service
            .store
            .save_conversation_boundary(chat.clone())
            .await
            .unwrap();
        if let Some(revoked) = revoked {
            app.service
                .store
                .put_node_fixture(json!({ "id": node, "revoked": revoked, "local": false }))
                .await
                .unwrap();
        }
        app.service
            .store
            .save_checkpoint_boundary(&run, json!({ "nodeId": node }))
            .await
            .unwrap();
        app.service
            .store
            .put(
                "node-disk-grants",
                json!({ "id": grant, "runId": run, "nodeId": node }),
            )
            .await
            .unwrap();
        if recorded_volume {
            app.service
                .store
                .put(
                    "node-volumes",
                    json!({ "id": format!("{run}:{node}"), "runId": run, "nodeId": node }),
                )
                .await
                .unwrap();
        }
        let publication = id();
        let kept_publication = id();
        let unique = id();
        let shared = id();
        let (owned, kept, exclusive, reused, owner) = (
            publication.clone(),
            kept_publication.clone(),
            unique.clone(),
            shared.clone(),
            run.clone(),
        );
        app.service.store.transaction(move |db| {
            db.0.execute("INSERT INTO shared_publications VALUES(?1,?2)", rusqlite::params![owned, owner])?;
            db.0.execute("INSERT INTO shared_publications VALUES(?1,'other-retained-run')", [&kept])?;
            for (object, hash) in [(&exclusive, "exclusive"), (&reused, "shared")] {
                db.0.execute("INSERT INTO shared_objects(id,destination,hash,size,location,state) VALUES(?1,'fixture',?2,4096,'{}','ready')", rusqlite::params![object, hash])?;
                db.0.execute("INSERT INTO shared_references VALUES(?1,?2)", rusqlite::params![owned, object])?;
            }
            db.0.execute("INSERT INTO shared_references VALUES(?1,?2)", rusqlite::params![kept, reused])?;
            Ok(())
        }).await.unwrap();
        let directory = app.service.config.data_dir.join("runs").join(&run);
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(
            directory.join("private.txt"),
            "deleted conversation contents",
        )
        .unwrap();
        let path = chat_path(&chat);
        assert_eq!(
            app.request("DELETE", &path, json!({})).await.0,
            StatusCode::OK
        );
        app.service.cleanup_conversations().await.unwrap();

        if revoked == Some(true) {
            assert_eq!(app.get(&path).await.0, StatusCode::NOT_FOUND);
            assert!(!directory.exists());
            assert!(
                app.service
                    .store
                    .get("node-disk-grants", &grant)
                    .await
                    .unwrap()
                    .is_none()
            );
            assert!(
                app.service
                    .store
                    .get("node-volumes", &format!("{run}:{node}"))
                    .await
                    .unwrap()
                    .is_none()
            );
            assert_eq!(requests.load(Ordering::SeqCst), 0);
        } else {
            assert_eq!(app.get(&path).await.0, StatusCode::OK);
            assert!(directory.join("private.txt").exists());
            assert!(
                app.service
                    .store
                    .get("node-disk-grants", &grant)
                    .await
                    .unwrap()
                    .is_some()
            );
            assert!(
                app.service
                    .store
                    .get("node-volumes", &format!("{run}:{node}"))
                    .await
                    .unwrap()
                    .is_some()
            );
            assert!(requests.load(Ordering::SeqCst) > 0);
        }
        let (exclusive, reused) = (unique.clone(), shared.clone());
        let (unique_unused, shared_unused) = app
            .service
            .store
            .read(move |db| {
                let unused = |object: &str| {
                    db.0.query_row(
                        "SELECT unused_at IS NOT NULL FROM shared_objects WHERE id=?1",
                        [object],
                        |row| row.get::<_, bool>(0),
                    )
                };
                Ok((unused(&exclusive)?, unused(&reused)?))
            })
            .await
            .unwrap();
        assert_eq!(unique_unused, revoked == Some(true));
        assert!(!shared_unused, "Another conversation still owns this block");
        server.abort();
    }
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
    app.service
        .store
        .save_conversation_boundary(chat)
        .await
        .unwrap();
    app.service.cleanup_conversations().await.unwrap();
    let (status, restored) = app
        .request("POST", &format!("{path}/restore"), json!({}))
        .await;
    assert_eq!(status, StatusCode::OK, "{restored}");
    assert_eq!(restored["lifecycle"], "active");
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

const RETENTION: &str = "/api/settings/conversations";
const DAY: i64 = 86_400_000;

async fn old_chat(app: &App, days: i64) -> Value {
    let mut chat = app.create_chat().await;
    let activity = cairn_installation::config::now() - days * DAY;
    chat["createdAt"] = activity.into();
    chat["updatedAt"] = activity.into();
    chat["lastActivityAt"] = activity.into();
    app.service
        .store
        .save_conversation_boundary(chat)
        .await
        .unwrap()
}

#[tokio::test]
async fn empty_and_converted_chats_start_their_inactivity_clock_at_creation() {
    for converted in [false, true] {
        let app = App::new().await;
        let mut chat = if converted {
            let run = id();
            let agent = app
                .service
                .store
                .agent_snapshot(cairn_installation::config::MAIN_AGENT_ID)
                .await
                .unwrap()
                .unwrap();
            app.add_finished_run(
                &run,
                RunStatus::Succeeded,
                json!({
                    "snapshot": {
                        "agent": agent,
                        "task": { "name": "Completed mission", "prompt": "Inspect the workspace" },
                    },
                    "sessionId": "mission-session",
                    "finishedAt": cairn_installation::config::now() - 8 * DAY,
                }),
                "Completed mission history",
            )
            .await;
            app.service
                .store
                .save_checkpoint_boundary(&run, json!({ "prepared": {}, "completed": true }))
                .await
                .unwrap();
            let (status, chat) = app
                .request("POST", &format!("/api/runs/{run}/chat"), json!({}))
                .await;
            assert_eq!(status, StatusCode::OK, "{chat}");
            chat
        } else {
            app.create_chat().await
        };
        let activity = chat["lastActivityAt"].as_i64().unwrap();
        assert_eq!(chat["lastActivityAt"], chat["createdAt"]);
        assert_eq!(chat["lastActivityAt"], chat["updatedAt"]);
        app.service.cleanup_conversations().await.unwrap();
        assert_eq!(app.get(&chat_path(&chat)).await.0, StatusCode::OK);

        let expired = activity - 8 * DAY;
        for field in ["createdAt", "updatedAt", "lastActivityAt"] {
            chat[field] = expired.into();
        }
        app.service
            .store
            .save_conversation_boundary(chat.clone())
            .await
            .unwrap();
        app.service.cleanup_conversations().await.unwrap();
        assert_eq!(
            app.get(&chat_path(&chat)).await.1["environmentState"],
            "expired"
        );
    }
}

#[tokio::test]
async fn retention_defaults_to_seven_days_and_validates_persisted_owner_settings() {
    let app = App::new().await;
    assert_eq!(app.get(RETENTION).await.1, json!({ "inactivityDays": 7 }));
    for invalid in [
        json!({}),
        json!({ "inactivityDays": -1 }),
        json!({ "inactivityDays": 1.5 }),
        json!({ "inactivityDays": "7" }),
        json!({ "inactivityDays": 36501 }),
        json!({ "inactivityDays": null }),
    ] {
        assert_eq!(
            app.request("PUT", RETENTION, invalid).await.0,
            StatusCode::BAD_REQUEST
        );
    }
    let policy = json!({ "inactivityDays": 14 });
    assert_eq!(
        app.request("PUT", RETENTION, policy.clone()).await.1,
        policy
    );
    assert_eq!(app.get(RETENTION).await.1, policy);
    let root = app.service.config.data_dir.clone();
    let reopened = cairn_installation::store::Store::open(&root).unwrap();
    assert_eq!(
        serde_json::to_value(
            reopened
                .read(|db| db.conversation_retention())
                .await
                .unwrap()
        )
        .unwrap(),
        policy
    );
}

#[tokio::test]
async fn expired_remote_conversation_continues_with_history_without_fencing_a_retired_node() {
    let app = App::new().await;
    std::fs::create_dir_all(&app.service.config.home).unwrap();
    let chat = app.create_chat().await;
    let path = chat_path(&chat);
    let first = id();
    app.request(
        "POST",
        &format!("{path}/messages"),
        json!({ "id": first, "text": "Preserve this decision" }),
    )
    .await;
    app.tick().await;
    let detail = app.get(&path).await.1;
    let run = text(&detail["run"], "id").to_owned();
    assert!(!run.is_empty(), "{detail}");
    assert!(
        app.service.config.runner_url.is_empty(),
        "Control-only manager fixture"
    );
    let node = id();
    app.service
        .store
        .put_node_fixture(json!({ "id": node, "revoked": true, "local": false }))
        .await
        .unwrap();
    let chat_id = text(&chat, "id").to_owned();
    let run_id = run.clone();
    app.service
        .store
        .transaction(move |db| {
            for mut message in db.messages(&chat_id)? {
                message["status"] = "delivered".into();
                db.put_message(&message).expect("fixture delivered message");
            }
            db.event(
                &run_id,
                &EventKind::ChatUser,
                "Preserve this decision",
                None,
            )
            .expect("fixture history");
            db.change_run(
                &run_id,
                cairn_installation::runs::RunChanges {
                    status: Some(RunStatus::Succeeded),
                    finished_at: Some(Some(cairn_installation::config::now() - 8 * DAY)),
                    recovery_pending: Some(false),
                    session_id: Some(Some("old-native-session".into())),
                    boundary: Some(json!({ "isolated": true })),
                    ..Default::default()
                },
            )
            .expect("fixture finished run");
            db.save_checkpoint_boundary(
                &run_id,
                &json!({
                        "prepared": { "workspace": "old-workspace" },
                        "launched": true,
                        "completed": true,
                        "workspaceInitialized": true,
                        "runnerId": id(),
                "nodeId": node,
                    }),
            )
            .expect("fixture checkpoint");
            let mut chat = db.conversation_boundary(&chat_id)?.unwrap();
            chat["lastActivityAt"] = (cairn_installation::config::now() - 8 * DAY).into();
            db.save_conversation_boundary(&chat)?;
            Ok(())
        })
        .await
        .unwrap();
    app.service.cleanup_conversations().await.unwrap();
    assert_eq!(app.get(&path).await.1["environmentState"], "expired");
    assert_eq!(
        app.request(
            "POST",
            &format!("{path}/messages"),
            json!({ "id": id(), "text": "Continue" })
        )
        .await
        .0,
        StatusCode::OK
    );
    app.tick().await;
    let detail = app.get(&path).await.1;
    assert!(detail["environmentState"].is_null(), "{detail}");
    assert_eq!(detail["run"]["id"], run);
    assert_eq!(detail["run"]["status"], "queued");
    assert_eq!(detail["run"]["isolated"], false);
    cairn_installation::recovery::fence(&app.service, &detail["run"])
        .await
        .unwrap();
    assert!(detail["run"]["sessionId"].is_null());
    assert!(text(&detail["run"]["chatExecution"], "context").contains("Preserve this decision"));
    assert!(detail["run"]["environmentExpiredAt"].is_number());
    let checkpoint = app
        .service
        .store
        .checkpoint_boundary(&run)
        .await
        .unwrap()
        .unwrap();
    assert!(checkpoint["prepared"].is_null());
    assert!(checkpoint["runnerId"].is_null());
    assert_ne!(checkpoint["workspaceInitialized"], true);
    assert_eq!(checkpoint["launched"], false);
    assert_eq!(checkpoint["completed"], false);
}

#[tokio::test]
async fn interrupted_environment_cleanup_blocks_execution_and_retries_after_policy_is_disabled() {
    let app = App::new().await;
    let mut chat = old_chat(&app, 8).await;
    let run = id();
    app.add_finished_run(
        &run,
        RunStatus::Succeeded,
        json!({
            "workspaces": [{ "kind": "worktree", "path": app._root.path().to_str().unwrap() }],
        }),
        "Kept history",
    )
    .await;
    chat["runId"] = run.clone().into();
    app.service
        .store
        .save_conversation_boundary(chat.clone())
        .await
        .unwrap();
    app.service.cleanup_conversations().await.unwrap();
    let detail = app.get(&chat_path(&chat)).await.1;
    assert_eq!(detail["environmentState"], "cleaning");
    assert!(detail["environmentError"].is_string());
    assert_eq!(
        app.request(
            "POST",
            &format!("{}/messages", chat_path(&chat)),
            json!({ "id": id(), "text": "Wait for erasure" })
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    app.request("PUT", RETENTION, json!({ "inactivityDays": 0 }))
        .await;
    app.service
        .store
        .update_run_boundary(&run, json!({ "workspaces": [] }))
        .await
        .unwrap();
    let mut current = app
        .service
        .store
        .conversation_boundary(text(&chat, "id"))
        .await
        .unwrap()
        .unwrap();
    current["environmentRetryAfter"] = 0.into();
    app.service
        .store
        .save_conversation_boundary(current)
        .await
        .unwrap();
    app.service.cleanup_conversations().await.unwrap();
    let detail = app.get(&chat_path(&chat)).await.1;
    assert_eq!(detail["environmentState"], "expired");
    assert!(detail["environmentError"].is_null());
}

#[tokio::test]
async fn seven_day_expiration_preserves_history_uploads_artifacts_and_public_access() {
    let app = App::new().await;
    let mut expired = old_chat(&app, 8).await;
    let recent = old_chat(&app, 6).await;
    let run = id();
    app.add_finished_run(
        &run,
        RunStatus::Succeeded,
        json!({ "finishedAt": cairn_installation::config::now() - 8 * DAY }),
        "Old history",
    )
    .await;
    expired["runId"] = run.clone().into();
    app.service
        .store
        .save_conversation_boundary(expired.clone())
        .await
        .unwrap();
    let attachments = app
        .service
        .config
        .data_dir
        .join("chat-attachments")
        .join(expired["id"].as_str().unwrap());
    std::fs::create_dir_all(&attachments).unwrap();
    std::fs::write(attachments.join("file.txt"), "old work").unwrap();
    let artifact = id();
    let artifact_path = app
        .service
        .config
        .data_dir
        .join("artifacts")
        .join(&artifact);
    std::fs::create_dir_all(artifact_path.parent().unwrap()).unwrap();
    std::fs::write(&artifact_path, "shared work").unwrap();
    app.service
        .store
        .set(
            &format!("artifact:{run}:{artifact}"),
            json!({ "id": artifact, "publicToken": "expired-share", "visibility": "public" }),
            None,
        )
        .await
        .unwrap();
    app.service
        .store
        .set(
            "artifact-share:expired-share",
            json!({ "runId": run }),
            None,
        )
        .await
        .unwrap();
    let workspace = app.service.config.data_dir.join("runs").join(&run);
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::write(workspace.join("unpublished.txt"), "unpublished work").unwrap();
    assert_eq!(app.get(&chat_path(&expired)).await.0, StatusCode::OK);
    app.service.cleanup_conversations().await.unwrap();
    assert_eq!(
        app.get(&chat_path(&expired)).await.1["environmentState"],
        "expired"
    );
    assert_eq!(app.get(&chat_path(&recent)).await.0, StatusCode::OK);
    assert!(!workspace.exists());
    assert!(attachments.exists());
    assert!(artifact_path.exists());
    assert!(
        app.service
            .store
            .kv("artifact-share:expired-share")
            .await
            .unwrap()
            .is_some()
    );
    let run_id = run.clone();
    assert!(
        app.service
            .store
            .read(move |db| db.run(&run_id))
            .await
            .unwrap()
            .is_some()
    );

    let checkpoint = app
        .service
        .store
        .checkpoint_boundary(&run)
        .await
        .unwrap()
        .unwrap();
    assert!(checkpoint["prepared"].is_null());
    let retained = app.service.store.run(&run).await.unwrap();
    assert!(retained["sessionId"].is_null());
    assert_eq!(retained["resumeAvailable"], false);
    assert_eq!(
        app.request("POST", &format!("/api/runs/{run}/resume"), json!({}))
            .await
            .0,
        StatusCode::CONFLICT
    );
    // Another sweep cannot erase retained files or re-expire an already empty environment.
    app.service.cleanup_conversations().await.unwrap();
    assert!(artifact_path.exists());
    // Explicit deletion still erases everything, including previously retained artifacts.
    app.request("DELETE", &chat_path(&expired), json!({})).await;
    app.service.cleanup_conversations().await.unwrap();
    assert!(!artifact_path.exists());
    assert!(!attachments.exists());
    assert_eq!(app.get(&chat_path(&expired)).await.0, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn disabling_and_extending_retention_preserves_old_chats_but_manual_deletion_still_works() {
    let app = App::new().await;
    let chat = old_chat(&app, 8).await;
    for days in [0, 14] {
        app.request("PUT", RETENTION, json!({ "inactivityDays": days }))
            .await;
        app.service.cleanup_conversations().await.unwrap();
        assert_eq!(app.get(&chat_path(&chat)).await.0, StatusCode::OK);
    }
    app.request("PUT", RETENTION, json!({ "inactivityDays": 0 }))
        .await;
    app.request("DELETE", &chat_path(&chat), json!({})).await;
    app.service.cleanup_conversations().await.unwrap();
    assert_eq!(app.get(&chat_path(&chat)).await.0, StatusCode::NOT_FOUND);
    let chat = old_chat(&app, 8).await;
    app.request("PUT", RETENTION, json!({ "inactivityDays": 3 }))
        .await;
    app.service.cleanup_conversations().await.unwrap();
    assert_eq!(
        app.get(&chat_path(&chat)).await.1["environmentState"],
        "expired"
    );
}

#[tokio::test]
async fn retention_protects_pending_work_recent_completion_and_worker_finalization() {
    let app = App::new().await;
    for protection in [
        "queued",
        "running",
        "finished",
        "wakeup",
        "message",
        "question",
        "finalizing",
    ] {
        let mut chat = old_chat(&app, 8).await;
        let run = id();
        let status = match protection {
            "queued" => RunStatus::Queued,
            "running" => RunStatus::Running,
            _ => RunStatus::Succeeded,
        };
        let finished = if protection == "finished" {
            cairn_installation::config::now()
        } else {
            1
        };
        app.add_finished_run(
            &run,
            status,
            json!({ "finishedAt": finished }),
            "Protected work",
        )
        .await;
        chat["runId"] = run.clone().into();
        if protection == "wakeup" {
            chat["wakeup"] =
                serde_json::to_value(common::run_fixture::complete_wakeup_fixture(&json!({
                    "id": "retention-wakeup", "createdAt": cairn_installation::config::now(),
                    "wakeAt": cairn_installation::config::now() + 3600000,
                })))
                .unwrap();
        }
        if protection == "finalizing" {
            app.service
                .worker
                .active
                .lock()
                .await
                .insert(run.clone(), tokio_util::sync::CancellationToken::new());
        }
        let chat_id = chat["id"].as_str().unwrap().to_owned();
        if protection == "message" {
            app.service
                .store
                .write({
                    let chat_id = chat_id.clone();
                    move |db| {
                        db.put_message(&json!({
                            "id": id(),
                            "chatId": chat_id,
                            "text": "Pending work",
                            "status": "queued",
                            "createdAt": 1
                        }))?;
                        Ok(())
                    }
                })
                .await
                .unwrap();
        }
        if protection == "question" {
            let question = serde_json::from_value(json!({
                "id": "question", "chatId": chat_id, "runId": run,
                "source": "cairn", "blocking": false, "status": "pending",
                "fields": [{"id":"scope","title":"Pending question"}],
            }))
            .unwrap();
            app.service
                .store
                .transaction(move |db| db.save_question(&question))
                .await
                .unwrap();
        }
        app.service
            .store
            .save_conversation_boundary(chat.clone())
            .await
            .unwrap();
        app.service.cleanup_conversations().await.unwrap();
        assert_eq!(
            app.get(&chat_path(&chat)).await.1["lifecycle"],
            "active",
            "{protection}"
        );
        assert!(
            app.get(&chat_path(&chat)).await.1["environmentState"].is_null(),
            "{protection}"
        );
    }
}

#[tokio::test]
async fn expiration_uses_recorded_activity_and_bounds_each_pass() {
    let app = App::new().await;
    let mut recent = old_chat(&app, 10).await;
    recent["lastActivityAt"] = cairn_installation::config::now().into();
    app.service
        .store
        .save_conversation_boundary(recent.clone())
        .await
        .unwrap();
    let expired = old_chat(&app, 8).await;
    let second = old_chat(&app, 8).await;
    app.service.cleanup_conversations().await.unwrap();
    assert_eq!(
        app.service
            .store
            .conversations_boundary()
            .await
            .unwrap()
            .iter()
            .filter(|c| c["environmentState"] == "expired")
            .count(),
        1
    );
    app.service.cleanup_conversations().await.unwrap();
    assert_eq!(
        app.get(&chat_path(&expired)).await.1["environmentState"],
        "expired"
    );
    assert_eq!(
        app.get(&chat_path(&second)).await.1["environmentState"],
        "expired"
    );
    assert_eq!(app.get(&chat_path(&recent)).await.0, StatusCode::OK);
}

#[tokio::test]
async fn only_the_installation_owner_can_read_or_change_retention() {
    let app = App::new().await;
    let member = common::RelayContext::new(
        &common::relay_fixture::member_context(&app.service, "member-retention").await,
    );
    for method in ["GET", "PUT"] {
        let request = member
            .authorize(common::request(method, RETENTION))
            .header("content-type", "application/json")
            .body(Body::from(json!({ "inactivityDays": 0 }).to_string()))
            .unwrap();
        let response = common::send(&app.router, request).await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }
    assert_eq!(app.get(RETENTION).await.1["inactivityDays"], 7);
}

#[tokio::test]
async fn expiration_rechecks_policy_and_activity_after_waiting_for_worker_lock() {
    for disable in [true, false] {
        let app = Arc::new(App::new().await);
        let mut chat = old_chat(&app, 8).await;
        let active = app.service.worker.active.lock().await;
        let cleaning = app.clone();
        let cleanup =
            tokio::spawn(async move { cleaning.service.cleanup_conversations().await.unwrap() });
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if app.service.conversation_storage_lock.try_lock().is_err() {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        if disable {
            app.request("PUT", RETENTION, json!({ "inactivityDays": 0 }))
                .await;
        } else {
            chat["lastActivityAt"] = cairn_installation::config::now().into();
            app.service
                .store
                .save_conversation_boundary(chat.clone())
                .await
                .unwrap();
        }
        drop(active);
        cleanup.await.unwrap();
        assert_eq!(app.get(&chat_path(&chat)).await.1["lifecycle"], "active");
    }
}

/// Local SQLite overhead, independent of remote node/S3 latency. See the retention doc.
#[tokio::test]
#[ignore = "explicit local retention storage measurements"]
async fn measure_retention_storage() {
    use std::time::Instant;
    fn median(mut samples: Vec<f64>) -> f64 {
        samples.sort_by(f64::total_cmp);
        samples[samples.len() / 2]
    }
    fn cpu_seconds() -> f64 {
        let mut usage = std::mem::MaybeUninit::<libc::rusage>::zeroed();
        assert_eq!(
            unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) },
            0
        );
        let usage = unsafe { usage.assume_init() };
        (usage.ru_utime.tv_sec + usage.ru_stime.tv_sec) as f64
            + (usage.ru_utime.tv_usec + usage.ru_stime.tv_usec) as f64 / 1_000_000.0
    }
    let app = App::new().await;
    for _ in 0..100 {
        old_chat(&app, 8).await;
    }
    let mut chat = app
        .service
        .store
        .conversations_boundary()
        .await
        .unwrap()
        .remove(0);
    let id = chat["id"].as_str().unwrap().to_owned();
    let encoded = serde_json::to_vec(&chat).unwrap().len();
    for enabled in [false, true] {
        app.service
            .save_conversation_retention(json!({ "inactivityDays": if enabled { 7 } else { 0 } }))
            .await
            .unwrap();
        let before_cpu = cpu_seconds();
        let mut warm = Vec::new();
        let mut cold = Vec::new();
        let mut writes = Vec::new();
        let mut cleanup = Vec::new();
        for _ in 0..50 {
            let start = Instant::now();
            app.service.store.conversation_boundary(&id).await.unwrap();
            warm.push(start.elapsed().as_secs_f64() * 1000.0);
            // Fresh SQLite connections, with the host OS page cache still warm.
            let reopened =
                cairn_installation::store::Store::open(&app.service.config.data_dir).unwrap();
            let start = Instant::now();
            reopened.conversation_boundary(&id).await.unwrap();
            cold.push(start.elapsed().as_secs_f64() * 1000.0);
            drop(reopened);
            let start = Instant::now();
            chat = app
                .service
                .store
                .save_conversation_boundary(chat.clone())
                .await
                .unwrap();
            writes.push(start.elapsed().as_secs_f64() * 1000.0);
        }
        for _ in 0..20 {
            let start = Instant::now();
            app.service.cleanup_conversations().await.unwrap();
            cleanup.push(start.elapsed().as_secs_f64() * 1000.0);
        }
        println!(
            "{}",
            json!({
                "enabled": enabled,
                "chatEncodedBytes": encoded,
                "warmReadMedianMs": median(warm),
                "coldConnectionReadMedianMs": median(cold),
                "writeMedianMs": median(writes),
                "cleanupMedianMs": median(cleanup),
                "cpuSeconds": cpu_seconds() - before_cpu,
                "remainingChats": app.service.store.conversations_boundary().await.unwrap().len()
            })
        );
    }
}

#[tokio::test]
async fn http_edits_change_plan_explicitly_and_preserve_it_when_omitted() {
    for (initial, requested, expected) in [
        (false, Some(true), true),
        (true, Some(false), false),
        (true, None, true),
    ] {
        let app = App::new().await;
        common::managed_codex_home(&app.service.config.home);
        let chat = app.create_chat().await;
        let path = chat_path(&chat);
        let messages = format!("{path}/messages");
        let (status, body) = app
            .request("POST", &format!("{path}/pause"), json!({ "paused": true }))
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let message = id();
        let (status, body) = app
            .request(
                "POST",
                &messages,
                json!({ "id": message, "text": "Original request", "plan": initial }),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let mut edit = json!({ "text": "Edited request" });
        if let Some(plan) = requested {
            edit["plan"] = plan.into();
        }
        let (status, body) = app
            .request("PUT", &format!("{messages}/{message}"), edit)
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let (_, detail) = app.get(&path).await;
        let saved = detail["messages"]
            .as_array()
            .unwrap()
            .iter()
            .find(|saved| saved["id"] == message.as_str())
            .unwrap();
        assert_eq!(saved["plan"] == true, expected, "{saved}");
        let (status, body) = app
            .request("POST", &format!("{path}/pause"), json!({ "paused": false }))
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        app.tick().await;
        let (_, detail) = app.get(&path).await;
        assert_eq!(
            detail["run"]["chatExecution"]["messageId"],
            message.as_str()
        );
        assert_eq!(
            detail["run"]["chatExecution"]["plan"] == true,
            expected,
            "{detail}"
        );
    }
}

#[tokio::test]
async fn a_plan_request_admitted_while_idle_never_joins_a_turn_that_can_change_files() {
    let app = App::new().await;
    common::managed_codex_home(&app.service.config.home);
    let chat = app.create_chat().await;
    let path = chat_path(&chat);
    let messages = format!("{path}/messages");
    let inbox = |run: &Value| {
        let file = app
            .service
            .config
            .data_dir
            .join("runs")
            .join(run["id"].as_str().unwrap())
            .join("chat-input/messages.json");
        serde_json::from_slice::<Value>(&std::fs::read(file).unwrap()).unwrap()
    };

    // With the queue paused no turn runs, so a plan request asking to steer is admitted.
    let (status, _) = app
        .request("POST", &format!("{path}/pause"), json!({ "paused": true }))
        .await;
    assert_eq!(status, StatusCode::OK);
    let ordinary = id();
    let plan = id();
    for message in [
        json!({ "id": ordinary, "text": "Refactor the queue" }),
        json!({ "id": plan, "text": "Plan the follow-up", "mode": "steer", "plan": true }),
    ] {
        let (status, body) = app.request("POST", &messages, message).await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }
    app.request("POST", &format!("{path}/pause"), json!({ "paused": false }))
        .await;

    // The ordinary message ahead starts a turn that can change files.
    app.tick().await;
    let run = app
        .service
        .chat_detail(chat["id"].as_str().unwrap())
        .await
        .unwrap()["run"]
        .clone();
    assert_eq!(run["chatExecution"]["messageId"], ordinary.as_str());
    assert_ne!(run["chatExecution"]["plan"], true);
    let run_id = run["id"].as_str().unwrap().to_owned();
    app.service
        .store
        .change_run_fixture(&run_id, json!({ "status": "running" }))
        .await
        .unwrap();
    app.service
        .chat_acknowledge(&run_id, &ordinary)
        .await
        .unwrap();
    app.service
        .store
        .save_checkpoint_boundary(&run_id, json!({ "completed": true, "launched": true }))
        .await
        .unwrap();

    // Steering never hands it the plan request, which stays queued.
    app.tick().await;
    assert_eq!(inbox(&run), json!([]));
    let (_, detail) = app.get(&path).await;
    let pending = detail["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|message| message["id"] == plan.as_str())
        .unwrap()
        .clone();
    assert_eq!(pending["status"], "queued");

    // After the turn boundary it runs as its own plan turn.
    app.service
        .store
        .change_run_fixture(
            &run_id,
            json!({ "status": "succeeded", "sessionId": "saved-session", "workspace": "/saved/workspace" }),
        )
        .await
        .unwrap();
    app.tick().await;
    let next = app
        .service
        .chat_detail(chat["id"].as_str().unwrap())
        .await
        .unwrap()["run"]
        .clone();
    assert_eq!(next["chatExecution"]["messageId"], plan.as_str());
    assert_eq!(next["chatExecution"]["plan"], true);
}

/// Admission can reach quota before there is a checkpoint or an engine session.
async fn quota_switch_fixture(app: &App) -> (String, Value) {
    std::fs::create_dir_all(&app.service.config.home).unwrap();
    common::relay_fixture::claimed(&app.service).await.unwrap();
    let chat = app.create_chat().await;
    let path = chat_path(&chat);
    let message = json!({"id":uuid::Uuid::new_v4(),"text":"Keep my pending request"});
    assert_eq!(
        app.request("POST", &format!("{path}/messages"), message)
            .await
            .0,
        StatusCode::OK
    );
    app.tick().await;
    let detail = app.get(&path).await.1;
    let run = detail["run"].clone();
    assert!(run["id"].is_string(), "{detail}");
    app.service
        .store
        .change_run(
            text(&run, "id"),
            cairn_installation::runs::RunChanges {
                account_wait_reason: Some(Some(
                    "Waiting for a Codex account with available usage.".into(),
                )),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let mut account = serde_json::to_value(
        app.service
            .accounts
            .create(
                &app.service,
                cairn_installation::provider::Provider::Claude,
                "Alternative",
            )
            .await
            .unwrap(),
    )
    .unwrap();
    account["state"] = "ready".into();
    app.service
        .store
        .save_account_fixture(account)
        .await
        .unwrap();
    let input = json!({"requestId":uuid::Uuid::new_v4(),"runId":run["id"],"turnId":run["chatExecution"]["messageId"],"provider":"claude"});
    (path, input)
}

#[tokio::test]
async fn quota_switch_is_revision_checked_idempotent_and_cancellable() {
    for failed in [false, true] {
        let app = App::new().await;
        let (path, input) = quota_switch_fixture(&app).await;
        if failed {
            app.service
                .store
                .change_run(
                    text(&input, "runId"),
                    cairn_installation::runs::RunChanges {
                        status: Some(RunStatus::Failed),
                        error: Some(Some("Claude AI usage limit reached".into())),
                        account_wait_reason: Some(None),
                        ..Default::default()
                    },
                )
                .await
                .unwrap();
        }
        let endpoint = format!("{path}/coding-agent");
        let (status, options) = app.get(&endpoint).await;
        assert_eq!(status, StatusCode::OK, "{options}");
        assert_eq!(
            options["choices"],
            json!([{"provider":"claude","label":"Claude Code"}])
        );
        assert_eq!(options["turnId"], input["turnId"]);
        let mut stale = input.clone();
        stale["turnId"] = "different-turn".into();
        assert_eq!(
            app.request("POST", &endpoint, stale).await.0,
            StatusCode::CONFLICT
        );
        let mut malformed = input.clone();
        malformed["turnId"] = "".into();
        assert_eq!(
            app.request("POST", &endpoint, malformed).await.0,
            StatusCode::BAD_REQUEST
        );
        let (status, accepted) = app.request("POST", &endpoint, input.clone()).await;
        assert_eq!(status, StatusCode::OK, "{accepted}");
        assert_eq!(accepted["codingAgentSwitch"]["provider"], "claude");
        assert!(
            app.service
                .store
                .run_state(text(&input, "runId"))
                .await
                .unwrap()
                .cancel_requested_at
                .is_some()
        );
        // Accepted identities remain idempotent even if the account is paused meanwhile.
        for mut account in app.service.store.account_fixtures().await.unwrap() {
            account["enabled"] = false.into();
            app.service
                .store
                .save_account_fixture(account)
                .await
                .unwrap();
        }
        assert_eq!(
            app.request("POST", &endpoint, input.clone()).await.0,
            StatusCode::OK
        );
        let mut reused = input.clone();
        reused["provider"] = "opencode".into();
        assert_eq!(
            app.request("POST", &endpoint, reused).await.0,
            StatusCode::CONFLICT
        );
        assert_eq!(
            app.request("POST", &format!("{path}/stop"), json!({}))
                .await
                .0,
            StatusCode::OK
        );
        app.service.worker.tick(&app.service).await.unwrap();
        let stopped = app.get(&path).await.1;
        assert!(stopped["codingAgentSwitch"].is_null());
        assert_eq!(stopped["run"]["snapshot"]["agent"]["provider"], "codex");
        assert_eq!(stopped["paused"], true);
        assert_eq!(
            stopped["run"]["status"],
            if failed { "failed" } else { "cancelled" }
        );
    }
}

#[tokio::test]
async fn accepted_quota_switch_recovers_after_restart_without_releasing_paused_followups() {
    for pause_before_acceptance in [true, false] {
        let app = App::new().await;
        let (path, input) = quota_switch_fixture(&app).await;
        let endpoint = format!("{path}/coding-agent");
        if pause_before_acceptance {
            assert_eq!(
                app.request("POST", &format!("{path}/pause"), json!({"paused":true}))
                    .await
                    .0,
                StatusCode::OK
            );
        }

        assert_eq!(
            app.request("POST", &endpoint, input.clone()).await.0,
            StatusCode::OK
        );
        if !pause_before_acceptance {
            assert_eq!(
                app.request("POST", &format!("{path}/pause"), json!({"paused":true}))
                    .await
                    .0,
                StatusCode::OK
            );
        }
        let followup_id = id();
        assert_eq!(
            app.request(
                "POST",
                &format!("{path}/messages"),
                json!({"id":followup_id,"text":"Keep this follow-up paused","mode":"queue"})
            )
            .await
            .0,
            StatusCode::OK
        );
        // Keep the new execution queued so this test observes recovery without invoking a CLI.
        for mut account in app.service.store.account_fixtures().await.unwrap() {
            account["enabled"] = false.into();
            app.service
                .store
                .save_account_fixture(account)
                .await
                .unwrap();
        }
        let restarted = Service::new(app.service.config.clone()).await.unwrap();
        restarted.worker.tick(&restarted).await.unwrap();
        common::eventually(
            std::time::Duration::from_secs(5),
            std::time::Duration::from_millis(20),
            async || {
                let run = restarted.store.run(text(&input, "runId")).await.unwrap();
                (run["snapshot"]["agent"]["provider"] == "claude").then_some(())
            },
        )
        .await;
        let resumed = app.get(&path).await.1;
        assert_eq!(resumed["run"]["id"], input["runId"]);
        assert_eq!(
            resumed["run"]["chatExecution"]["messageId"],
            input["turnId"]
        );
        assert_eq!(resumed["run"]["status"], "queued");
        assert!(resumed["run"]["cancelRequestedAt"].is_null());
        assert!(resumed["codingAgentSwitch"].is_null());
        assert_eq!(resumed["paused"], true);
        assert_eq!(resumed["messages"].as_array().unwrap().len(), 2);
        assert!(
            resumed["messages"]
                .as_array()
                .unwrap()
                .iter()
                .any(|message| message["id"] == followup_id && message["status"] == "queued")
        );
        restarted.shutdown.cancel();
        restarted.worker.close().await;
    }
}

#[tokio::test]
async fn completed_quota_switch_replay_does_not_cancel_the_replacement_or_a_later_turn() {
    let app = App::new().await;
    let (path, input) = quota_switch_fixture(&app).await;
    let endpoint = format!("{path}/coding-agent");
    assert_eq!(
        app.request("POST", &endpoint, input.clone()).await.0,
        StatusCode::OK
    );
    // The same turn can return to its original engine after another quota switch.
    // Both the latest receipt and an older receipt must leave active work untouched.
    for latest_request in [
        input["requestId"].clone(),
        uuid::Uuid::new_v4().to_string().into(),
    ] {
        let chat_id = path.rsplit('/').next().unwrap().to_owned();
        let original = input["requestId"].as_str().unwrap().to_owned();
        let saved_chat = chat_id.clone();
        let completed = app
            .service
            .store
            .transaction(move |db| {
                let mut receipt = db.engine_switch_receipt(&chat_id, &original)?.unwrap();
                receipt.request_id = latest_request.as_str().unwrap().to_owned();
                receipt.pending = false;
                db.save_engine_switch(&receipt)?;
                Ok(serde_json::to_value(db.engine_switch(&chat_id)?)?)
            })
            .await
            .unwrap();
        app.service
            .store
            .change_run(
                text(&input, "runId"),
                cairn_installation::runs::RunChanges {
                    cancel_requested_at: Some(None),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let token = tokio_util::sync::CancellationToken::new();
        app.service
            .worker
            .active
            .lock()
            .await
            .insert(text(&input, "runId").to_owned(), token.clone());
        assert_eq!(
            app.request("POST", &endpoint, input.clone()).await.0,
            StatusCode::OK
        );
        assert!(
            !token.is_cancelled(),
            "An idempotent replay must not cancel the replacement run"
        );
        assert_eq!(
            app.service
                .store
                .read(move |db| Ok(serde_json::to_value(db.engine_switch(&saved_chat)?)?))
                .await
                .unwrap(),
            completed
        );
        app.service.worker.active.lock().await.clear();
    }
}

#[tokio::test]
async fn quota_switch_does_not_offer_or_accept_an_engine_that_cannot_preserve_the_sandbox() {
    let app = App::new().await;
    let (path, mut input) = quota_switch_fixture(&app).await;
    let mut run = app.service.store.run(text(&input, "runId")).await.unwrap();
    run["snapshot"]["agent"]["access"]["sandbox"] = "workspace-write".into();
    app.service
        .store
        .change_run(
            text(&input, "runId"),
            cairn_installation::runs::RunChanges {
                snapshot: Some(serde_json::from_value(run["snapshot"].clone()).unwrap()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let mut account = serde_json::to_value(
        app.service
            .accounts
            .create(
                &app.service,
                cairn_installation::provider::Provider::Opencode,
                "Alternative",
            )
            .await
            .unwrap(),
    )
    .unwrap();
    account["state"] = "ready".into();
    account["modelProvider"] = "opencode".into();
    app.service
        .store
        .save_account_fixture(account)
        .await
        .unwrap();
    let endpoint = format!("{path}/coding-agent");
    let (status, options) = app.get(&endpoint).await;
    assert_eq!(status, StatusCode::OK, "{options}");
    assert!(
        options["choices"]
            .as_array()
            .unwrap()
            .iter()
            .all(|choice| choice["provider"] != "opencode")
    );
    input["provider"] = "opencode".into();
    assert_eq!(
        app.request("POST", &endpoint, input.clone()).await.0,
        StatusCode::CONFLICT
    );
    assert!(
        app.service
            .store
            .run_state(text(&input, "runId"))
            .await
            .unwrap()
            .cancel_requested_at
            .is_none()
    );
}

#[tokio::test]
async fn quota_switch_recognizes_current_structured_limits_without_reusing_recovered_failures() {
    for (field, code) in [
        ("code", "usage_limit_reached"),
        ("codexErrorInfo", "usageLimitExceeded"),
    ] {
        let app = App::new().await;
        let (path, input) = quota_switch_fixture(&app).await;
        let run_id = text(&input, "runId");
        let message = "Account usage is exhausted.";
        app.service
            .store
            .change_run(
                run_id,
                cairn_installation::runs::RunChanges {
                    status: Some(RunStatus::Failed),
                    error: Some(Some(message.into())),
                    account_wait_reason: Some(None),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let mut payload = json!({"type":"turn.failed","error":{"message":message}});
        payload["error"][field] = code.into();
        app.service
            .store
            .event(
                run_id,
                &EventKind::TurnFailed,
                message,
                Some(payload.clone()),
            )
            .await
            .unwrap();
        // A recovered failure in an earlier attempt is insufficient for switching this one.
        app.service
            .store
            .event(run_id, &EventKind::TurnStarted, "", None)
            .await
            .unwrap();
        let endpoint = format!("{path}/coding-agent");
        assert_eq!(app.get(&endpoint).await.1["choices"], json!([]));
        assert_eq!(
            app.request("POST", &endpoint, input.clone()).await.0,
            StatusCode::CONFLICT
        );
        app.service
            .store
            .event(run_id, &EventKind::TurnFailed, message, Some(payload))
            .await
            .unwrap();
        assert_eq!(
            app.get(&endpoint).await.1["choices"],
            json!([{"provider":"claude","label":"Claude Code"}])
        );
        assert_eq!(
            app.request("POST", &endpoint, input).await.0,
            StatusCode::OK
        );
    }
}

#[tokio::test]
async fn quota_switch_keeps_a_plan_turn_on_an_engine_that_enforces_it() {
    let app = App::new().await;
    let (path, mut input) = quota_switch_fixture(&app).await;
    let run_id = text(&input, "runId").to_owned();
    let mut account = serde_json::to_value(
        app.service
            .accounts
            .create(
                &app.service,
                cairn_installation::provider::Provider::Opencode,
                "Alternative",
            )
            .await
            .unwrap(),
    )
    .unwrap();
    account["state"] = "ready".into();
    account["modelProvider"] = "opencode".into();
    app.service
        .store
        .save_account_fixture(account)
        .await
        .unwrap();
    let endpoint = format!("{path}/coding-agent");
    let mut execution = app.service.store.run(&run_id).await.unwrap()["chatExecution"].clone();
    execution["plan"] = true.into();
    app.service
        .store
        .change_run_fixture(&run_id, json!({ "chatExecution": execution }))
        .await
        .unwrap();

    // OpenCode cannot enforce a plan turn's boundary: it is not accepted.
    let (status, options) = app.get(&endpoint).await;
    assert_eq!(status, StatusCode::OK, "{options}");
    assert_eq!(
        options["choices"],
        json!([{"provider":"claude","label":"Claude Code"}])
    );
    let mut opencode = input.clone();
    opencode["provider"] = "opencode".into();
    let (status, refused) = app.request("POST", &endpoint, opencode).await;
    assert!(status.is_client_error(), "{refused}");
    assert!(app.get(&path).await.1["codingAgentSwitch"].is_null());

    // Claude Code takes the same plan turn over, still as a plan turn.
    input["requestId"] = uuid::Uuid::new_v4().to_string().into();
    assert_eq!(
        app.request("POST", &endpoint, input.clone()).await.0,
        StatusCode::OK
    );
    for mut account in app.service.store.account_fixtures().await.unwrap() {
        account["enabled"] = false.into();
        app.service
            .store
            .save_account_fixture(account)
            .await
            .unwrap();
    }
    let restarted = Service::new(app.service.config.clone()).await.unwrap();
    restarted.worker.tick(&restarted).await.unwrap();
    common::eventually(
        std::time::Duration::from_secs(5),
        std::time::Duration::from_millis(20),
        async || {
            let run = restarted.store.run(&run_id).await.unwrap();
            (run["snapshot"]["agent"]["provider"] == "claude").then_some(())
        },
    )
    .await;
    let switched = restarted.store.run(&run_id).await.unwrap();
    assert_eq!(switched["chatExecution"]["messageId"], input["turnId"]);
    assert_eq!(switched["chatExecution"]["plan"], true);
    restarted.shutdown.cancel();
    restarted.worker.close().await;
}
