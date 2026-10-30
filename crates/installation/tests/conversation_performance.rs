//! Explicit, credential-free benchmarks of the real conversation list interface.
//! Run alone through scripts/test-backend.mjs with --ignored --nocapture.
mod common;

use axum::body::Body;
use cairn_installation::{config::MAIN_AGENT_ID, service::Service};
use common::run_fixture::DbRunFixture;
use serde_json::json;
use std::time::Instant;
use tower::ServiceExt;

#[tokio::test]
#[ignore = "explicit conversation read benchmark"]
async fn conversation_list_by_saved_execution_size() {
    for kib in [0, 128, 512] {
        let root = tempfile::tempdir().unwrap();
        common::managed_codex_home(&root.path().join("home"));
        let service = Service::new(common::config(root.path())).await.unwrap();
        service.store.transaction(move |db| {
            let instructions = "x".repeat(kib * 1024);
            for index in 0..100 {
                let chat = format!("chat-{index}");
                let run = format!("run-{index}");
                db.add_run_fixture(&json!({
                    "id": run, "taskId": chat, "status": "succeeded",
                    "sessionId": "fixture-session",
                    "finishedAt": 1, "summary": "## Completed\n\nFixture **answer**.",
                    "snapshot": { "agent": { "instructions": instructions } },
                }), None)?;
                db.save_conversation_boundary(&json!({
                    "id": chat, "agentId": MAIN_AGENT_ID, "runId": run,
                }))?;
                db.0.execute("INSERT INTO run_checkpoints(run_id,state) VALUES(?,?)", rusqlite::params![
                    run, json!({ "prepared": { "instructions": instructions }, "freshSession": false }).to_string(),
                ])?;
            }
            Ok(())
        }).await.unwrap();
        let mut samples = Vec::new();
        let mut bytes = 0;
        for _ in 0..6 {
            let started = Instant::now();
            let chats = service.chat_list().await.unwrap();
            samples.push(started.elapsed().as_micros());
            assert_eq!(chats.len(), 100);
            assert!(
                chats
                    .iter()
                    .all(|chat| chat["compaction"]["available"] == true)
            );
            bytes = serde_json::to_vec(&chats).unwrap().len();
        }
        println!(
            "{}",
            json!({ "benchmark": "conversation_list", "chats": 100, "snapshotKiB": kib,
            "checkpointKiB": kib, "elapsedMicros": samples, "responseBytes": bytes })
        );
        let context = common::relay_fixture::context(&service).await;
        service
            .store
            .set("read-since:fixture-owner", json!(0), None)
            .await
            .unwrap();
        let router = common::relay_fixture::router(service.clone())
            .await
            .unwrap();
        let mut samples = Vec::new();
        for _ in 0..6 {
            let started = Instant::now();
            let request = common::request("GET", "/api/chats")
                .header("x-test-relay-token", context["value"].as_str().unwrap())
                .body(Body::empty())
                .unwrap();
            let response = router.clone().oneshot(request).await.unwrap();
            assert_eq!(response.status(), axum::http::StatusCode::OK);
            let chats = common::read_json(response).await;
            samples.push(started.elapsed().as_micros());
            let chats = chats.as_array().unwrap();
            assert_eq!(chats.len(), 100);
            assert!(
                chats.iter().all(|chat| chat["unread"] == true
                    && chat["preview"] == "Completed. Fixture answer.")
            );
            bytes = serde_json::to_vec(chats).unwrap().len();
        }
        println!(
            "{}",
            json!({ "benchmark": "unread_conversation_http", "chats": 100,
            "snapshotKiB": kib, "checkpointKiB": kib, "elapsedMicros": samples, "responseBytes": bytes })
        );
        service.shutdown.cancel();
    }
}
