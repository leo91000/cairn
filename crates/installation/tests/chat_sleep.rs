mod common;

use cairn_installation::{
    chats::sleep,
    config::{id, now},
    project_workspaces,
    service::Service,
    validation::text,
};
use serde_json::{Value, json};
use std::{collections::HashSet, sync::Arc};
use tempfile::TempDir;

struct Fixture {
    _root: TempDir,
    service: Arc<Service>,
    chat: String,
    run: String,
    token: String,
}

impl Fixture {
    async fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        common::managed_codex_home(&root.path().join("home"));
        let service = Service::new(cairn_installation::config::Config {
            runner_url: "http://unused-runner:4311".into(),
            ..common::config(root.path())
        })
        .await
        .unwrap();
        let chat = service.chat_create(json!({})).await.unwrap();
        let chat = text(&chat, "id").to_owned();
        let message = id();
        service
            .chat_send(
                &chat,
                json!({ "id": message, "text": "Finish the original task after CI" }),
            )
            .await
            .unwrap();
        service.chat_tick(&HashSet::new()).await.unwrap();
        let run = service.chat_detail(&chat).await.unwrap()["run"].clone();
        let run_id = text(&run, "id").to_owned();
        service
            .store
            .patch_run(&run_id, json!({ "status": "running" }))
            .await
            .unwrap();
        service.chat_acknowledge(&run_id, &message).await.unwrap();
        let config = service
            .mcps
            .run_configuration(&service, &run)
            .await
            .unwrap();
        let token = text(&config["env"], "CAIRN_MCP_RUN_TOKEN").to_owned();
        service
            .store
            .set(
                &format!("run-checkpoint:{run_id}"),
                json!({ "completed": true, "launched": true }),
                None,
            )
            .await
            .unwrap();
        Self {
            _root: root,
            service,
            chat,
            run: run_id,
            token,
        }
    }

    async fn schedule(&self) -> Value {
        sleep::schedule(
            &self.service,
            &self.token,
            &json!({ "seconds": 420, "reason": "Wait for CI" }),
        )
        .await
        .unwrap()
    }

    async fn finish(&self) {
        self.service.store.patch_run(&self.run, json!({
            "status": "succeeded", "recoveryPending": false, "sessionId": "saved-session", "workspace": "/saved/workspace",
        })).await.unwrap();
    }

    async fn due(&self) {
        let mut chat = self.service.get("chats", &self.chat).await.unwrap();
        chat["wakeup"]["wakeAt"] = (now() - 1000).into();
        self.service.store.put("chats", chat).await.unwrap();
    }

    async fn tick(&self) -> Value {
        self.service.chat_tick(&HashSet::new()).await.unwrap();
        self.service.chat_detail(&self.chat).await.unwrap()
    }
}

#[tokio::test]
async fn sleep_is_idempotent_durable_and_resumes_same_workspace_once_without_user_input() {
    let f = Fixture::new().await;
    let wake = f.schedule().await;
    assert_eq!(wake, f.schedule().await);
    assert_eq!(wake["seconds"], 420);
    assert_eq!(
        wake["wakeAt"].as_i64().unwrap() - wake["createdAt"].as_i64().unwrap(),
        420000
    );
    let listing = project_workspaces::rpc(&f.service, &f.token, "tools/list", &json!({}))
        .await
        .unwrap();
    assert!(
        listing["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool["name"] == "sleep")
    );
    f.finish().await;
    assert_eq!(
        f.tick().await["run"]["status"],
        "succeeded",
        "An early tick must not wake the conversation"
    );
    f.due().await;
    // Reopen the installation database with a fresh service; no in-memory timer survives.
    let restarted = Service::new(f.service.config.clone()).await.unwrap();
    let active = HashSet::from([f.run.clone()]);
    restarted.chat_tick(&active).await.unwrap();
    assert!(
        restarted.chat_detail(&f.chat).await.unwrap()["wakeup"].is_object(),
        "Do not wake before the old execution is released"
    );
    restarted.chat_tick(&HashSet::new()).await.unwrap();
    let detail = restarted.chat_detail(&f.chat).await.unwrap();
    assert!(detail["wakeup"].is_null());
    assert_eq!(detail["run"]["status"], "queued");
    assert_eq!(detail["run"]["id"], f.run);
    assert_eq!(detail["run"]["sessionId"], "saved-session");
    assert_eq!(detail["run"]["workspace"], "/saved/workspace");
    assert_eq!(detail["run"]["chatExecution"]["messageId"], wake["id"]);
    assert!(text(&detail["run"]["chatExecution"], "text").contains("original authorized task"));
    assert_eq!(detail["messages"].as_array().unwrap().len(), 1);
    restarted.chat_tick(&HashSet::new()).await.unwrap();
    let run = f.run.clone();
    let count = restarted
        .store
        .read(move |db| {
            Ok(db
                .events(&run, 0, 100)?
                .iter()
                .filter(|event| event["type"] == "chat.wakeup")
                .count())
        })
        .await
        .unwrap();
    assert_eq!(count, 1);
    assert!(
        sleep::schedule(
            &restarted,
            &f.token,
            &json!({ "seconds": 1, "reason": "stale" })
        )
        .await
        .is_err()
    );
}

#[tokio::test]
async fn sleep_waits_for_success_and_pause_holds_the_original_deadline() {
    let f = Fixture::new().await;
    f.schedule().await;
    f.due().await;
    assert_eq!(f.tick().await["run"]["status"], "running");
    f.finish().await;
    f.service.chat_pause(&f.chat, true).await.unwrap();
    let paused = f.tick().await;
    assert_eq!(paused["run"]["status"], "succeeded");
    assert!(paused["wakeup"].is_object());
    f.service.chat_pause(&f.chat, false).await.unwrap();
    assert_eq!(f.tick().await["run"]["status"], "queued");
}

#[tokio::test]
async fn replacing_waits_and_expired_turn_grants_cannot_reschedule_the_next_turn() {
    let f = Fixture::new().await;
    let original = f.schedule().await;
    let integral_float = sleep::schedule(
        &f.service,
        &f.token,
        &json!({ "seconds": 420.0, "reason": "Wait for CI" }),
    )
    .await
    .unwrap();
    assert_eq!(original, integral_float);
    let replacement = sleep::schedule(
        &f.service,
        &f.token,
        &json!({ "seconds": 1, "reason": "Wait for review" }),
    )
    .await
    .unwrap();
    assert_ne!(original["id"], replacement["id"]);
    f.finish().await;
    f.due().await;
    let detail = f.tick().await;
    assert_eq!(
        detail["run"]["chatExecution"]["messageId"],
        replacement["id"]
    );
    f.service
        .store
        .patch_run(&f.run, json!({ "status": "running" }))
        .await
        .unwrap();
    let configuration = f
        .service
        .mcps
        .run_configuration(&f.service, &detail["run"])
        .await
        .unwrap();
    let token = text(&configuration["env"], "CAIRN_MCP_RUN_TOKEN");
    let next = sleep::schedule(
        &f.service,
        token,
        &json!({ "seconds": 420, "reason": "Next wait" }),
    )
    .await
    .unwrap();
    assert!(
        sleep::schedule(
            &f.service,
            &f.token,
            &json!({ "seconds": 1, "reason": "Old turn" })
        )
        .await
        .is_err()
    );
    assert_eq!(
        f.service.chat_detail(&f.chat).await.unwrap()["wakeup"],
        next
    );
}

#[tokio::test]
async fn scheduled_continuation_rechecks_permissions_before_reusing_the_workspace() {
    let f = Fixture::new().await;
    f.schedule().await;
    f.finish().await;
    f.due().await;
    let mut agent = f
        .service
        .get("agents", cairn_installation::config::MAIN_AGENT_ID)
        .await
        .unwrap();
    agent["access"]["sandbox"] = "read-only".into();
    f.service.store.put("agents", agent).await.unwrap();
    let detail = f.tick().await;
    assert_eq!(detail["paused"], true);
    assert_eq!(detail["run"]["status"], "succeeded");
    let error = f
        .service
        .store
        .kv(&format!("chat-error:{}", f.chat))
        .await
        .unwrap()
        .unwrap();
    assert!(error.as_str().unwrap().contains("Agent access was reduced"));
}

#[tokio::test]
async fn user_input_cancel_and_trash_prevent_obsolete_wakeups() {
    for action in ["message", "cancel", "trash", "failure"] {
        let f = Fixture::new().await;
        f.schedule().await;
        f.finish().await;
        f.due().await;
        match action {
            "message" => {
                f.service
                    .chat_send(&f.chat, json!({ "id": id(), "text": "Change of plan" }))
                    .await
                    .unwrap();
            }
            "cancel" => f.service.worker.cancel(&f.service, &f.run).await.unwrap(),
            "trash" => {
                assert!(f.service.chat_trash(&f.chat, false).await.is_err());
                f.service.chat_trash(&f.chat, true).await.unwrap();
                assert_eq!(
                    f.service.chat_restore(&f.chat).await.unwrap_err().status,
                    410
                );
            }
            "failure" => {
                f.service
                    .store
                    .patch_run(&f.run, json!({ "status": "failed" }))
                    .await
                    .unwrap();
            }
            _ => unreachable!(),
        }
        let detail = f.tick().await;
        assert!(detail["wakeup"].is_null(), "{action}: {detail}");
        if action == "trash" {
            assert_eq!(
                f.service.store.run(&f.run).await.unwrap()["status"],
                "succeeded"
            );
        } else {
            assert_ne!(
                detail["run"]["chatExecution"]["messageId"],
                detail["wakeup"]["id"]
            );
        }
        if action != "message" {
            assert_ne!(detail["run"]["status"], "queued", "{action}");
        }
    }
}

#[tokio::test]
async fn failed_paused_turn_cannot_replay_its_wakeup_after_manual_resume() {
    for tick_before_resume in [true, false] {
        let f = Fixture::new().await;
        let wake = f.schedule().await;
        f.service.chat_pause(&f.chat, true).await.unwrap();
        f.due().await;
        f.service
            .store
            .patch_run(
                &f.run,
                json!({
                    "status": "failed",
                    "resumeAvailable": true,
                    "recoveryPending": false,
                }),
            )
            .await
            .unwrap();
        f.service
            .store
            .set(
                &format!("run-checkpoint:{}", f.run),
                json!({
                    "completed": true,
                    "launched": true,
                    "prepared": { "backend": "fixture" },
                }),
                None,
            )
            .await
            .unwrap();

        if tick_before_resume {
            let failed = f.tick().await;
            assert!(failed["wakeup"].is_null());
            assert_eq!(failed["paused"], true);
            assert_eq!(failed["run"]["status"], "failed");
        }

        // Resume can race ahead of the first worker tick after the failure.
        let resumed = f.service.worker.resume(&f.service, &f.run).await.unwrap();
        assert_eq!(resumed["chatExecution"]["messageId"], wake["messageId"]);
        assert!(f.service.chat_detail(&f.chat).await.unwrap()["wakeup"].is_null());
        f.service.chat_pause(&f.chat, false).await.unwrap();
        f.finish().await;
        f.service
            .store
            .patch_run(&f.run, json!({ "recoveryPending": false }))
            .await
            .unwrap();

        let finished = f.tick().await;
        assert_eq!(finished["run"]["status"], "succeeded");
        assert_eq!(
            finished["run"]["chatExecution"]["messageId"],
            wake["messageId"]
        );
        assert!(finished["wakeup"].is_null());
    }
}

#[tokio::test]
async fn queued_follow_ups_start_after_the_continuation_of_an_earlier_wait() {
    let f = Fixture::new().await;
    let follow_up = id();
    f.service
        .chat_send(
            &f.chat,
            json!({ "id": follow_up, "text": "Then update the changelog" }),
        )
        .await
        .unwrap();
    let wake = f.schedule().await;
    f.finish().await;

    let held = f.tick().await;
    assert_eq!(held["run"]["status"], "succeeded");
    assert_eq!(held["wakeup"], wake);
    assert_eq!(held["messages"][1]["status"], "queued");

    f.due().await;
    let resumed = f.tick().await;
    assert_eq!(resumed["run"]["chatExecution"]["messageId"], wake["id"]);
    assert_eq!(resumed["messages"][1]["status"], "queued");

    f.finish().await;
    let next = f.tick().await;
    assert_eq!(next["run"]["chatExecution"]["messageId"], follow_up);
    assert_eq!(next["messages"][1]["status"], "sending");
}

#[tokio::test]
async fn queued_follow_ups_are_not_held_by_a_wakeup_that_cannot_resume() {
    let f = Fixture::new().await;
    let follow_up = id();
    f.service
        .chat_send(&f.chat, json!({ "id": follow_up, "text": "Next request" }))
        .await
        .unwrap();
    f.schedule().await;
    f.finish().await;
    let mut chat = f.service.get("chats", &f.chat).await.unwrap();
    chat["sessionRestartRequested"] = true.into();
    f.service.store.put("chats", chat).await.unwrap();

    let detail = f.tick().await;
    assert_eq!(detail["run"]["chatExecution"]["messageId"], follow_up);
    assert!(detail["wakeup"].is_null());
}

#[tokio::test]
async fn sleep_rejects_invalid_inputs_non_chat_runs_and_pending_steering() {
    let f = Fixture::new().await;
    for input in [
        json!({ "seconds": 0, "reason": "CI" }),
        json!({ "seconds": 86401, "reason": "CI" }),
        json!({ "seconds": 1.5, "reason": "CI" }),
        json!({ "seconds": 1, "reason": " " }),
        json!({ "seconds": 1, "reason": "CI", "extra": true }),
    ] {
        assert!(sleep::schedule(&f.service, &f.token, &input).await.is_err());
    }
    assert!(f.service.chat_detail(&f.chat).await.unwrap()["wakeup"].is_null());
    f.service
        .chat_send(
            &f.chat,
            json!({ "id": id(), "text": "New instruction", "mode": "steer" }),
        )
        .await
        .unwrap();
    assert!(
        sleep::schedule(
            &f.service,
            &f.token,
            &json!({ "seconds": 1, "reason": "CI" })
        )
        .await
        .is_err()
    );
    let task = f
        .service
        .task(
            json!({ "name": "Scheduled task", "prompt": "Do work", "agentId": cairn_installation::config::MAIN_AGENT_ID }),
            None,
        )
        .await
        .unwrap();
    let run = f
        .service
        .enqueue(text(&task, "id"), "manual", None)
        .await
        .unwrap();
    f.service
        .store
        .patch_run(text(&run, "id"), json!({ "status": "running" }))
        .await
        .unwrap();
    let config = f
        .service
        .mcps
        .run_configuration(&f.service, &run)
        .await
        .unwrap();
    let token = text(&config["env"], "CAIRN_MCP_RUN_TOKEN");
    assert!(
        sleep::schedule(&f.service, token, &json!({ "seconds": 1, "reason": "CI" }))
            .await
            .is_err()
    );
}
