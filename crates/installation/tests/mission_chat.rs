use crate::common::run_fixture::DbRunFixture;
use crate::common::run_fixture::RunFixture;
use common::MissionFixture;

mod common;

use cairn_installation::{
    config::{MAIN_AGENT_ID, id},
    service::Service,
    validation::text,
};
use serde_json::{Value, json};
use std::collections::HashSet;

async fn fixture() -> (tempfile::TempDir, std::sync::Arc<Service>, Value, Value) {
    let root = tempfile::tempdir().unwrap();
    common::managed_codex_home(&root.path().join("home"));
    let service = Service::new(common::config(root.path())).await.unwrap();
    let task = service
        .task_fixture(
            json!({
                "name": "Review a deployment",
                "prompt": "Check the deployment",
                "agentId": MAIN_AGENT_ID,
                "cron": "0 9 * * *",
            }),
            None,
        )
        .await
        .unwrap();
    let mut run = service.snapshot(task.clone(), "manual").await.unwrap();
    run["nativeProvenanceUnknown"] = true.into();
    run["nativePrincipals"] = json!([]);
    run["nativeAuthorityNodes"] = json!([]);
    run["status"] = "succeeded".into();
    run["sessionId"] = "saved-session".into();
    run["summary"] = "Deployment reviewed".into();
    run["workspace"] = "/saved/workspace".into();
    let saved = run.clone();
    service.store.transaction(move |db| {
        db.add_run_fixture(&saved, None)?;
        db.event(text(&saved, "id"), "item.completed", "Deployment reviewed", Some(&json!({
            "item": { "id": "original-answer", "type": "agent_message", "text": "Deployment reviewed" },
        })))?;
        db.set(&format!("run-checkpoint:{}", text(&saved, "id")), &json!({
            "completed": true,
            "launched": true,
            "prepared": { "workspace": "/saved/workspace" },
        }), None)
    }).await.unwrap();
    (root, service, task, run)
}

#[tokio::test]
async fn mission_conversion_preserves_history_session_workspace_and_schedule() {
    let (_root, service, task, original) = fixture().await;
    let run_id = text(&original, "id");
    let checkpoint_key = format!("run-checkpoint:{run_id}");
    let checkpoint = service.store.kv(&checkpoint_key).await.unwrap();
    let (first, second) =
        tokio::join!(service.chat_from_run(run_id), service.chat_from_run(run_id));
    let chat = first.unwrap();
    assert_eq!(second.unwrap()["id"], chat["id"]);
    assert_eq!(service.chat_list().await.unwrap().len(), 1);
    assert_eq!(chat["title"], task["name"]);
    assert_eq!(chat["runId"], original["id"]);
    assert_eq!(
        service
            .mission_snapshot_fixture(text(&task, "id"))
            .await
            .unwrap(),
        task
    );
    assert_eq!(service.store.kv(&checkpoint_key).await.unwrap(), checkpoint);
    let converted = service.store.run(run_id).await.unwrap();
    assert_eq!(converted["taskId"], chat["id"]);
    assert_eq!(converted["sourceTaskId"], task["id"]);
    let chat_id = text(&chat, "id").to_owned();
    let task_id = text(&task, "id").to_owned();
    service
        .store
        .read(move |db| {
            assert_eq!(db.runs(None, Some(&chat_id), 10, 0, true)?.len(), 1);
            assert!(db.runs(None, Some(&task_id), 10, 0, true)?.is_empty());
            Ok(())
        })
        .await
        .unwrap();
    for field in [
        "sessionId",
        "workspace",
        "summary",
        "snapshot",
        "status",
        "createdAt",
    ] {
        assert_eq!(converted[field], original[field], "{field}");
    }
    let detail = service.chat_detail(text(&chat, "id")).await.unwrap();
    assert_eq!(detail["run"]["summary"], "Deployment reviewed");
    assert!(detail["messages"].as_array().unwrap().is_empty());
    let message = id();
    service
        .chat_send(
            text(&chat, "id"),
            json!({ "id": message, "text": "What should I monitor next?" }),
        )
        .await
        .unwrap();
    service.chat_tick(&HashSet::new()).await.unwrap();
    let continued = service.store.run(run_id).await.unwrap();
    assert_eq!(continued["status"], "queued");
    // Codex keeps the unattended instructions the mission thread started with.
    let turn = text(&continued["chatExecution"], "text");
    assert!(
        turn.contains(cairn_installation::run_output::INTERACTIVE),
        "{turn}"
    );
    assert!(turn.ends_with("\n\nWhat should I monitor next?"), "{turn}");
    assert_eq!(continued["sourceSessionId"], "saved-session");
    assert_eq!(continued["sessionId"], "saved-session");
    assert_eq!(continued["workspace"], "/saved/workspace");
    assert_eq!(continued["snapshot"]["task"]["id"], chat["id"]);
    assert_eq!(continued["chatExecution"]["context"], Value::Null);
    let checkpoint = service.store.kv(&checkpoint_key).await.unwrap().unwrap();
    assert_eq!(checkpoint["completed"], false);
    assert_eq!(checkpoint["prepared"]["workspace"], "/saved/workspace");
    let events = service
        .store
        .read(move |db| db.events(text(&original, "id"), 0, 100))
        .await
        .unwrap();
    assert_eq!(events[0]["text"], "Deployment reviewed");
}

#[tokio::test]
async fn mission_conversion_keeps_delegated_work_attached_to_its_requester() {
    let (_root, service, task, original) = fixture().await;
    let run_id = text(&original, "id");
    let mut child = service.snapshot(task.clone(), "manual").await.unwrap();
    child["requestedBy"] = json!({ "runId": run_id, "taskId": task["id"] });
    child["status"] = "succeeded".into();
    child["outcome"] = json!({
        "status": "blocked", "reason": "Needs requester input", "waitingOn": "requester",
    });
    let mut unrelated = service.snapshot(task.clone(), "manual").await.unwrap();
    unrelated["requestedBy"] = json!({ "runId": unrelated["id"], "taskId": task["id"] });
    let saved_child = child.clone();
    let saved_unrelated = unrelated.clone();
    service
        .store
        .transaction(move |db| {
            db.add_run_fixture(&saved_child, None)?;
            db.add_run_fixture(&saved_unrelated, None)?;
            Ok(())
        })
        .await
        .unwrap();

    let chat = service.chat_from_run(run_id).await.unwrap();
    service.chat_from_run(run_id).await.unwrap();
    let delegated = service.store.run(text(&child, "id")).await.unwrap();
    child["requestedBy"] = json!({ "runId": run_id, "chatId": chat["id"] });
    assert_eq!(delegated, child);
    assert_eq!(
        service.store.run(text(&unrelated, "id")).await.unwrap(),
        unrelated
    );
    assert_eq!(
        service
            .mission_snapshot_fixture(text(&task, "id"))
            .await
            .unwrap(),
        task
    );
}

#[tokio::test]
async fn converted_mission_keeps_its_original_request_when_changing_provider() {
    let (_root, service, _task, run) = fixture().await;
    let chat = service.chat_from_run(text(&run, "id")).await.unwrap();
    service
        .chat_send(
            text(&chat, "id"),
            json!({
                "id": id(), "text": "Explain the result", "provider": "claude",
            }),
        )
        .await
        .unwrap();
    service.chat_tick(&HashSet::new()).await.unwrap();
    let run = service.store.run(text(&run, "id")).await.unwrap();
    let context = text(&run["chatExecution"], "context");
    assert!(context.contains("Check the deployment"));
    assert!(context.contains("Deployment reviewed"));
    assert_eq!(run["workspace"], "/saved/workspace");
}

#[tokio::test]
async fn mission_conversion_keeps_summary_only_results_in_the_chat_history() {
    let (_root, service, _task, run) = fixture().await;
    let run_id = text(&run, "id").to_owned();
    let original = run_id.clone();
    service
        .store
        .write(move |db| {
            db.0.execute("DELETE FROM events WHERE run_id=?", [&original])?;
            Ok(())
        })
        .await
        .unwrap();
    let chat = service.chat_from_run(&run_id).await.unwrap();
    service.chat_from_run(&run_id).await.unwrap();
    service
        .chat_send(
            text(&chat, "id"),
            json!({ "id": id(), "text": "Follow up" }),
        )
        .await
        .unwrap();
    service.chat_tick(&HashSet::new()).await.unwrap();
    assert_eq!(service.store.run(&run_id).await.unwrap()["summary"], "");
    let events = service
        .store
        .read(move |db| db.events(&run_id, 0, 100))
        .await
        .unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["payload"]["item"]["type"], "agent_message");
    assert_eq!(events[0]["payload"]["item"]["text"], "Deployment reviewed");
}

#[tokio::test]
async fn mission_conversion_rejects_unavailable_and_unfinished_conversations_atomically() {
    for patch in [
        json!({ "status": "running" }),
        json!({ "status": "queued" }),
        json!({ "status": "failed" }),
        json!({ "workspaceCleanedAt": 42 }),
        json!({ "sessionId": null }),
        json!({ "recoveryPending": true }),
    ] {
        let (_root, service, _task, run) = fixture().await;
        let run_id = text(&run, "id");
        let original = service
            .store
            .change_run_fixture(run_id, patch)
            .await
            .unwrap();
        assert!(service.chat_from_run(run_id).await.is_err());
        assert!(service.chat_list().await.unwrap().is_empty());
        assert_eq!(service.store.run(run_id).await.unwrap(), original);
    }
    let (_root, service, _task, run) = fixture().await;
    let run_id = text(&run, "id");
    service
        .store
        .delete(&format!("run-checkpoint:{run_id}"))
        .await
        .unwrap();
    assert!(service.chat_from_run(run_id).await.is_err());
    assert!(service.chat_list().await.unwrap().is_empty());
}
