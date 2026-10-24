mod common;

use axum::{Json, Router};
use cairn_installation::{
    config::now,
    plugins::{
        connections::{self, Connection},
        registry, runtime, work,
    },
    service::Service,
    validation::text,
};
use serde_json::{Value, json};
use std::{path::PathBuf, sync::Arc, time::Duration};
use tempfile::TempDir;

fn worker() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_cairn"))
}

async fn service() -> (TempDir, Arc<Service>, Value) {
    let root = TempDir::new().unwrap();
    let s = Service::new(common::config(root.path())).await.unwrap();
    common::relay_fixture::claimed(&s).await.unwrap();
    s.plugins.use_worker(worker());
    let agent = s
        .agent(
            json!({ "name": "Handler fixture agent", "access": {
        "projects": [], "skills": [], "mcps": [], "github": false, "installation": false,
    } }),
            None,
        )
        .await
        .unwrap();
    (root, s, agent)
}

/// Real worker and native pump; deletion occurs while a capability is in flight.
/// This is a queue-lifetime fixture, not a model completion or Slack exchange.
#[tokio::test]
async fn deleting_an_instance_during_a_handler_cannot_recreate_its_queued_payload() {
    for (index, outcome) in [
        "return { retryAfter = 90 }",
        "error('synthetic handler failure')",
    ]
    .into_iter()
    .enumerate()
    {
        let (_root, s, agent) = service().await;
        let entered = Arc::new(tokio::sync::Notify::new());
        let released = Arc::new(tokio::sync::Notify::new());
        let app = Router::new().fallback({
            let entered = entered.clone();
            let released = released.clone();
            move || {
                let entered = entered.clone();
                let released = released.clone();
                async move {
                    entered.notify_one();
                    released.notified().await;
                    Json(json!({ "ok": true }))
                }
            }
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        s.plugins
            .route_http_to(Some(format!("http://{}", listener.local_addr().unwrap())));
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let mut manifest = registry::builtin("discord").unwrap().manifest;
        manifest.id = "handler-deletion".into();
        manifest.instance_form = false;
        manifest.version = format!("99.0.0-handler-deletion-{index}");
        let source = format!(
            r#"return {{ interaction = function()
            pcall(cairn.http.request, {{ method = "GET", url = "https://discord.com/api/v10/gateway" }})
            {outcome}
        end }}"#
        );
        let installed = registry::install(&s, &json!({ "manifest": manifest, "source": source }))
            .await
            .unwrap();
        registry::approve(
            &s,
            &manifest.id,
            &manifest.version,
            text(&installed, "digest"),
        )
        .await
        .unwrap();
        let value = connections::save(
            &s,
            &manifest.id,
            &json!({
                "name": "Handler deletion fixture", "enabled": true,
                "settings": { "guildId": "100", "channelId": "200", "botName": "Cairn" },
                "agentId": agent["id"], "userIds": ["300"], "token": "synthetic_discord_token",
            }),
            None,
        )
        .await
        .unwrap();
        let connection: Connection = serde_json::from_value(value).unwrap();
        let package = registry::active(&s, &manifest.id).await.unwrap();
        let mut session = s
            .plugins
            .host
            .start(&worker(), &package.manifest, &package.source)
            .await
            .unwrap();
        let event = json!({
            "id": cairn_installation::config::id(), "connectionId": connection.id,
            "revision": connection.revision, "routing": serde_json::to_value(&connection).unwrap()["routing"],
            "name": "interaction", "authorId": "300", "payload": { "synthetic": "discard this admitted content" },
            "attempts": 0, "createdAt": now(), "nextAttemptAt": now(),
        });
        s.store.put(work::EVENTS, event).await.unwrap();
        let pumping = tokio::spawn({
            let s = s.clone();
            let connection = connection.clone();
            async move {
                let result = runtime::pump(&s, &connection, &mut session).await;
                session.stop().await;
                result
            }
        });
        tokio::time::timeout(Duration::from_secs(5), entered.notified())
            .await
            .unwrap();
        connections::remove(&s, &manifest.id, &connection.id)
            .await
            .unwrap();
        released.notify_one();
        tokio::time::timeout(Duration::from_secs(5), pumping)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        server.abort();
        assert!(
            s.store.list(work::EVENTS).await.unwrap().is_empty(),
            "Handler outcome recreated a deleted instance's payload: {outcome}"
        );
    }
}
