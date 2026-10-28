use crate::common::AgentFixture;

use super::*;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

#[tokio::test]
async fn a_staged_attachment_is_reused_after_authority_failure_and_worker_restart() {
    Box::pin(post_upload_retry(Retry::Unchanged)).await;
}

#[tokio::test]
async fn changed_source_bytes_cannot_overwrite_an_already_staged_attachment() {
    Box::pin(post_upload_retry(Retry::Changed)).await;
}

#[tokio::test]
async fn reusing_a_staged_attachment_does_not_spend_chat_quota_twice() {
    Box::pin(post_upload_retry(Retry::FullQuota)).await;
}

enum Retry {
    Unchanged,
    Changed,
    FullQuota,
}

async fn staged_source_files(s: &Service) -> Vec<(String, Value)> {
    s.store
        .read(|db| {
            Ok(db
                .keys("chat-attachment:")?
                .into_iter()
                .filter(|(_, file)| file["name"] == "source.txt")
                .collect())
        })
        .await
        .unwrap()
}

async fn post_upload_retry(case: Retry) {
    let changed = matches!(case, Retry::Changed);
    let (_root, s, instance, resource, app) = fixture().await;
    let agent = s
        .agent_fixture(
            json!({"name": "Attachment retry", "access": {"projects": []}}),
            None,
        )
        .await
        .unwrap();
    create_route(
        &app,
        &instance,
        &resource,
        json!({
            "name": "Support", "enabled": true, "match": {"channel": "C1"},
            "agentId": agent["id"], "userIds": ["alice"],
        }),
    )
    .await;
    let seen = Arc::new(AtomicUsize::new(0));
    let (service, published) = (s.clone(), seen.clone());
    let authority = axum::Router::new().route(
        "/api/relay/00000000-0000-4000-8000-000000000055/task-authors",
        axum::routing::get(move || {
            let (service, published) = (service.clone(), published.clone());
            async move {
                let count = staged_source_files(&service).await.len();
                if count > published.load(Ordering::SeqCst) {
                    published.store(count, Ordering::SeqCst);
                    return (StatusCode::SERVICE_UNAVAILABLE, "Synthetic failure after attachment publication").into_response();
                }
                axum::Json(json!({"owner": {"account_id": "fixture-owner", "access_id": "fixture-owner"}, "members": []})).into_response()
            }
        }),
    );
    let (origin, authority_server) = common::serve_locally(authority).await;
    let identity_path = s.config.data_dir.join("installation-relay/identity.json");
    let mut identity: Value =
        serde_json::from_slice(&std::fs::read(&identity_path).unwrap()).unwrap();
    identity["origin"] = json!(origin);
    std::fs::write(identity_path, serde_json::to_vec(&identity).unwrap()).unwrap();
    let replace = Arc::new(AtomicBool::new(false));
    let fetched = Arc::new(AtomicUsize::new(0));
    let (replacement, downloads) = (replace.clone(), fetched.clone());
    let provider = axum::Router::new().route(
        "/attachment.txt",
        axum::routing::get(move || {
            let (replacement, downloads) = (replacement.clone(), downloads.clone());
            async move {
                downloads.fetch_add(1, Ordering::SeqCst);
                let byte = if replacement.load(Ordering::SeqCst) {
                    b'B'
                } else {
                    b'A'
                };
                ([("content-type", "text/plain")], vec![byte; 1024])
            }
        }),
    );
    let (origin, provider_server) = common::serve_locally(provider).await;
    s.plugins.route_http_to(Some(origin));
    let inbound = json!({
        "id": "staged-file", "resource": resource, "author": {"id": "alice"},
        "context": {"channel": "C1", "thread": "attachment-thread"},
        "body": {"text": "Keep one copy of this file", "files": [{"url": "https://api.example.test/attachment.txt", "name": "source.txt"}]},
    });
    assert_eq!(
        deliver(&app, &instance, inbound.clone()).await.1["status"],
        "accepted"
    );
    let connection = connections::get(&s, "router", &instance).await.unwrap();
    let package = registry::active(&s, "router").await.unwrap();
    let mut session = s
        .plugins
        .host
        .start(
            &PathBuf::from(env!("CARGO_BIN_EXE_cairn")),
            &package.manifest,
            &package.source,
        )
        .await
        .unwrap();
    for attempt in 0..2 {
        let mut pending = s.store.list("plugin-events").await.unwrap();
        assert_eq!(pending.len(), 1);
        pending[0]["nextAttemptAt"] = json!(0);
        s.store
            .put("plugin-events", pending.remove(0))
            .await
            .unwrap();
        let result = runtime::pump(&s, &connection, &mut session).await;
        assert!(result.is_ok() || result.as_ref().is_err_and(|error| error.status == 503));
        let files = staged_source_files(&s).await;
        assert_eq!(
            files.len(),
            1,
            "retry {attempt} duplicated a published attachment"
        );
        if attempt == 0 {
            assert_eq!(
                seen.load(Ordering::SeqCst),
                1,
                "the post-publication outage was not exercised"
            );
            replace.store(changed, Ordering::SeqCst);
            if matches!(case, Retry::FullQuota) {
                let chat = text(&files[0].1, "chatId");
                let bytes = vec![b'Q'; 10 * 1024 * 1024];
                let mut remaining = 200 * 1024 * 1024 - 1024;
                let mut index = 0;
                while remaining > 0 {
                    let size = remaining.min(bytes.len());
                    s.store_attachment(
                        chat,
                        &cairn_installation::config::id(),
                        &format!("quota-{index}.bin"),
                        &bytes[..size],
                    )
                    .await
                    .unwrap();
                    remaining -= size;
                    index += 1;
                }
            }
            session.stop().await;
            session = s
                .plugins
                .host
                .start(
                    &PathBuf::from(env!("CARGO_BIN_EXE_cairn")),
                    &package.manifest,
                    &package.source,
                )
                .await
                .unwrap();
        }
    }
    let chats = s.chat_list().await.unwrap();
    assert_eq!(chats.len(), 1);
    let chat = text(&chats[0], "id");
    let detail = s.chat_detail(chat).await.unwrap();
    let files = staged_source_files(&s).await;
    if changed {
        assert!(detail["messages"].as_array().unwrap().is_empty());
        let pending = s.store.list("plugin-events").await.unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0]["attempts"], 1);
    } else {
        assert_eq!(detail["messages"].as_array().unwrap().len(), 1);
        assert_eq!(
            detail["messages"][0]["attachments"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            detail["messages"][0]["attachments"][0]["id"],
            files[0].1["id"]
        );
        assert!(s.store.list("plugin-events").await.unwrap().is_empty());
        assert_eq!(
            deliver(&app, &instance, inbound.clone()).await.1["status"],
            "duplicate"
        );
    }
    assert_eq!(fetched.load(Ordering::SeqCst), 2);
    if matches!(case, Retry::Unchanged) {
        replace.store(true, Ordering::SeqCst);
        let mut next = inbound;
        next["id"] = json!("distinct-file-request");
        next["body"]["text"] = json!("Keep this distinct file too");
        assert_eq!(deliver(&app, &instance, next).await.1["status"], "accepted");
        for _ in 0..2 {
            let mut pending = s.store.list("plugin-events").await.unwrap();
            assert_eq!(pending.len(), 1);
            pending[0]["nextAttemptAt"] = json!(0);
            s.store
                .put("plugin-events", pending.remove(0))
                .await
                .unwrap();
            let result = runtime::pump(&s, &connection, &mut session).await;
            assert!(result.is_ok() || result.as_ref().is_err_and(|error| error.status == 503));
        }
        assert!(s.store.list("plugin-events").await.unwrap().is_empty());
        let detail = s.chat_detail(chat).await.unwrap();
        let messages = detail["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0]["attachments"][0]["id"], files[0].1["id"]);
        assert_ne!(messages[1]["attachments"][0]["id"], files[0].1["id"]);
        assert_eq!(staged_source_files(&s).await.len(), 2);
        let path = format!(
            "/api/chats/{chat}/attachments/{}",
            text(&messages[1]["attachments"][0], "id")
        );
        let response = common::send(&app, owner_request("GET", &path, Value::Null)).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            axum::body::to_bytes(response.into_body(), 2048)
                .await
                .unwrap()
                .as_ref(),
            &[b'B'; 1024]
        );
        assert_eq!(fetched.load(Ordering::SeqCst), 4);
    }
    let path = format!("/api/chats/{chat}/attachments/{}", text(&files[0].1, "id"));
    let response = common::send(&app, owner_request("GET", &path, Value::Null)).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        axum::body::to_bytes(response.into_body(), 2048)
            .await
            .unwrap()
            .as_ref(),
        &[b'A'; 1024]
    );
    session.stop().await;
    authority_server.abort();
    provider_server.abort();
}
