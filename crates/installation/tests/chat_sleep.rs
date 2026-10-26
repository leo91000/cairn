use common::MissionFixture;
use common::StoreAgentFixture;

mod common;

use cairn_installation::{
    chats::wait_for,
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
            .change_run(
                &run_id,
                cairn_installation::runs::RunChanges {
                    status: Some(cairn_contracts::RunStatus::Running),
                    ..Default::default()
                },
            )
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
        wait_for::schedule(
            &self.service,
            &self.token,
            &json!({ "conditions": [{ "type": "delay", "seconds": 420 }], "reason": "Wait for CI" }),
        )
        .await
        .unwrap()["wakeup"]
            .clone()
    }

    async fn finish(&self) {
        self.service
            .store
            .change_run(
                &self.run,
                cairn_installation::runs::RunChanges {
                    status: Some(cairn_contracts::RunStatus::Succeeded),
                    recovery_pending: Some(false),
                    session_id: Some(Some("saved-session".into())),
                    workspace: Some(Some("/saved/workspace".into())),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
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
async fn delay_is_idempotent_durable_and_resumes_same_workspace_once_without_user_input() {
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
            .any(|tool| tool["name"] == "wait_for")
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
        wait_for::schedule(
            &restarted,
            &f.token,
            &json!({ "conditions": [{ "type": "delay", "seconds": 1 }], "reason": "stale" })
        )
        .await
        .is_err()
    );
}

#[tokio::test]
async fn delay_waits_for_success_and_pause_holds_the_original_deadline() {
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
    let integral_float = wait_for::schedule(
        &f.service,
        &f.token,
        &json!({ "conditions": [{ "type": "delay", "seconds": 420.0 }], "reason": "Wait for CI" }),
    )
    .await
    .unwrap()["wakeup"]
        .clone();
    assert_eq!(original, integral_float);
    let replacement = wait_for::schedule(
        &f.service,
        &f.token,
        &json!({ "conditions": [{ "type": "delay", "seconds": 1 }], "reason": "Wait for review" }),
    )
    .await
    .unwrap()["wakeup"]
        .clone();
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
        .change_run(
            &f.run,
            cairn_installation::runs::RunChanges {
                status: Some(cairn_contracts::RunStatus::Running),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let configuration = f
        .service
        .mcps
        .run_configuration(&f.service, &detail["run"])
        .await
        .unwrap();
    let token = text(&configuration["env"], "CAIRN_MCP_RUN_TOKEN");
    let next = wait_for::schedule(
        &f.service,
        token,
        &json!({ "conditions": [{ "type": "delay", "seconds": 420 }], "reason": "Next wait" }),
    )
    .await
    .unwrap()["wakeup"]
        .clone();
    assert!(
        wait_for::schedule(
            &f.service,
            &f.token,
            &json!({ "conditions": [{ "type": "delay", "seconds": 1 }], "reason": "Old turn" })
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
        .agent_snapshot(cairn_installation::config::MAIN_AGENT_ID)
        .await
        .unwrap();
    agent["access"]["sandbox"] = "read-only".into();
    f.service.store.put_agent_fixture(agent).await.unwrap();
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
                    .change_run(
                        &f.run,
                        cairn_installation::runs::RunChanges {
                            status: Some(cairn_contracts::RunStatus::Failed),
                            ..Default::default()
                        },
                    )
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
            .change_run(
                &f.run,
                cairn_installation::runs::RunChanges {
                    status: Some(cairn_contracts::RunStatus::Failed),
                    resume_available: Some(true),
                    recovery_pending: Some(false),
                    ..Default::default()
                },
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
            .change_run(
                &f.run,
                cairn_installation::runs::RunChanges {
                    recovery_pending: Some(false),
                    ..Default::default()
                },
            )
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
async fn delay_rejects_invalid_inputs_and_pending_steering_but_accepts_missions() {
    let f = Fixture::new().await;
    for input in [
        json!({ "conditions": [{ "type": "delay", "seconds": 0 }], "reason": "CI" }),
        json!({ "conditions": [{ "type": "delay", "seconds": 86401 }], "reason": "CI" }),
        json!({ "conditions": [{ "type": "delay", "seconds": 1.5 }], "reason": "CI" }),
        json!({ "conditions": [{ "type": "delay", "seconds": 1 }], "reason": " " }),
        json!({ "conditions": [{ "type": "delay", "seconds": 1 }], "reason": "CI", "extra": true }),
    ] {
        assert!(
            wait_for::schedule(&f.service, &f.token, &input)
                .await
                .is_err()
        );
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
        wait_for::schedule(
            &f.service,
            &f.token,
            &json!({ "conditions": [{ "type": "delay", "seconds": 1 }], "reason": "CI" })
        )
        .await
        .is_err()
    );
    let task = f
        .service
        .task_fixture(
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
        .change_run(
            text(&run, "id"),
            cairn_installation::runs::RunChanges {
                status: Some(cairn_contracts::RunStatus::Running),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let config = f
        .service
        .mcps
        .run_configuration(&f.service, &run)
        .await
        .unwrap();
    let token = text(&config["env"], "CAIRN_MCP_RUN_TOKEN");
    let wait = wait_for::schedule(
        &f.service,
        token,
        &json!({
            "conditions": [{ "type": "delay", "seconds": 1 }], "reason": "CI",
        }),
    )
    .await
    .unwrap();
    assert_eq!(wait["deferred"], true);
    assert_eq!(
        f.service.store.run(text(&run, "id")).await.unwrap()["wakeup"],
        wait["wakeup"]
    );
}

#[tokio::test]
async fn replacing_a_native_session_retires_its_pending_questions_before_startup() {
    for (provider, fresh_session, expected_pending) in [
        ("codex", false, 1),
        ("opencode", false, 0),
        ("claude", false, 0),
        ("codex", true, 0),
    ] {
        let f = Fixture::new().await;
        let question = "a".repeat(64);
        f.service
            .question_receive(
                &f.run,
                json!({
                    "id": question,
                    "blocking": false,
                    "fields": [{ "id": "scope", "title": "Which scope?" }],
                }),
            )
            .await
            .unwrap();
        f.service
            .store
            .update_run_boundary(&f.run, json!({ "outcome": { "status": "needs_input" } }))
            .await
            .unwrap();
        f.finish().await;
        if fresh_session {
            f.service
                .store
                .set(
                    &format!("run-checkpoint:{}", f.run),
                    json!({ "completed": true, "launched": true, "freshSession": true }),
                    None,
                )
                .await
                .unwrap();
        }
        let model = if provider == "opencode" {
            "opencode/fixture"
        } else {
            ""
        };
        f.service
            .chat_send(
                &f.chat,
                json!({
                    "id": id(),
                    "text": "Continue the task",
                    "provider": provider,
                    "model": model,
                }),
            )
            .await
            .unwrap();
        let detail = f.tick().await;

        assert_eq!(detail["run"]["id"], f.run);
        assert_eq!(
            detail["run"]["status"], "queued",
            "provider={provider}: {detail}"
        );
        assert_eq!(detail["run"]["snapshot"]["agent"]["provider"], provider);
        assert_eq!(
            detail["pendingQuestions"], expected_pending,
            "provider={provider}, fresh_session={fresh_session}"
        );
        if expected_pending == 0 {
            assert_eq!(detail["questions"][0]["status"], "cancelled");
            assert!(detail["run"]["sessionId"].is_null());
            assert!(
                f.service
                    .question_answer(
                        &f.chat,
                        &question,
                        json!({ "id": id(), "answers": { "scope": ["Obsolete answer"] } }),
                    )
                    .await
                    .is_err()
            );
        }
    }
}

#[tokio::test]
async fn time_conditions_are_available_without_plugin_or_installation_access() {
    let f = Fixture::new().await;
    let mut agent = f
        .service
        .agent_snapshot(cairn_installation::config::MAIN_AGENT_ID)
        .await
        .unwrap();
    agent["access"]["installation"] = false.into();
    f.service
        .store
        .put_agent_fixture(agent.clone())
        .await
        .unwrap();
    let mut run = f.service.store.run(&f.run).await.unwrap();
    run["snapshot"]["agent"] = agent;
    f.service
        .store
        .update_run_boundary(&f.run, json!({ "snapshot": run["snapshot"] }))
        .await
        .unwrap();
    let listing = project_workspaces::rpc(&f.service, &f.token, "tools/list", &json!({}))
        .await
        .unwrap();
    let tools = listing["tools"].as_array().unwrap();
    assert!(!tools.iter().any(|tool| tool["name"] == "sleep"));
    assert!(
        project_workspaces::rpc(
            &f.service,
            &f.token,
            "tools/call",
            &json!({ "name": "sleep", "arguments": { "seconds": 1, "reason": "Removed" } })
        )
        .await
        .is_err()
    );
    let schema = &tools
        .iter()
        .find(|tool| tool["name"] == "wait_for")
        .unwrap()["inputSchema"];
    for condition in [
        json!({ "type": "delay", "seconds": 60 }),
        json!({ "type": "at", "date": "2026-01-01T00:00:00Z" }),
    ] {
        let input = json!({ "conditions": [condition], "reason": "Resume later" });
        assert!(jsonschema::is_valid(schema, &input));
        let result = project_workspaces::rpc(
            &f.service,
            &f.token,
            "tools/call",
            &json!({ "name": "wait_for", "arguments": input }),
        )
        .await
        .unwrap();
        assert_ne!(result["isError"], true, "{result}");
    }
    for input in [
        json!({ "conditions": [{ "type": "delay", "seconds": 60 }], "reason": "Wait", "timeoutSeconds": 100 }),
        json!({ "conditions": [{ "type": "run_finished", "runId": id() }], "reason": "Wait" }),
    ] {
        assert!(!jsonschema::is_valid(schema, &input));
        assert!(
            wait_for::schedule(&f.service, &f.token, &input)
                .await
                .is_err()
        );
    }
}

#[tokio::test]
async fn dates_are_absolute_timezone_aware_and_repeated_calls_keep_the_deadline() {
    let f = Fixture::new().await;
    let deadline = now() + 3_600_000;
    let date = chrono::DateTime::from_timestamp_millis(deadline).unwrap();
    let request = json!({ "conditions": [{ "type": "at", "date": date.to_rfc3339() }], "reason": "Wait until date" });
    let result = wait_for::schedule(&f.service, &f.token, &request)
        .await
        .unwrap();
    assert_eq!(result["deferred"], true);
    let wake = &result["wakeup"];
    assert_eq!(wake["wakeAt"], deadline);
    assert!(
        wake["condition"].is_null(),
        "Keep the legacy timer shape for older binaries"
    );
    let offset = chrono::FixedOffset::east_opt(7200).unwrap();
    let equivalent = json!({ "conditions": [{ "type": "at", "date": date.with_timezone(&offset).to_rfc3339() }], "reason": "Wait until date" });
    assert_eq!(
        wait_for::schedule(&f.service, &f.token, &equivalent)
            .await
            .unwrap(),
        result
    );
    assert_ne!(
        f.schedule().await["id"],
        wake["id"],
        "A duration replaces the date wait"
    );

    for condition in [
        json!({ "type": "at", "date": "tomorrow" }),
        json!({ "type": "at", "date": "2026-10-23T09:00:00" }),
        json!({ "type": "at", "date": "2026-02-30T09:00:00Z" }),
        json!({ "type": "at", "date": chrono::DateTime::from_timestamp_millis(now() + 86_500_000).unwrap().to_rfc3339() }),
        json!({ "type": "at", "date": date.to_rfc3339(), "seconds": 10 }),
    ] {
        assert!(
            wait_for::schedule(
                &f.service,
                &f.token,
                &json!({ "conditions": [condition], "reason": "Wait" })
            )
            .await
            .is_err()
        );
    }
}

#[tokio::test]
async fn past_dates_complete_immediately_and_replace_pending_waits() {
    let f = Fixture::new().await;
    f.schedule().await;
    let deadline = now() - 1000;
    let result = wait_for::schedule(&f.service, &f.token, &json!({
        "conditions": [{ "type": "at", "date": chrono::DateTime::from_timestamp_millis(deadline).unwrap().to_rfc3339() }],
        "reason": "Time already reached",
    })).await.unwrap();
    assert_eq!(
        result,
        json!({ "deferred": false, "observation": { "state": "completed", "type": "time", "wakeAt": deadline } })
    );
    let detail = f.service.chat_detail(&f.chat).await.unwrap();
    assert!(detail["wakeup"].is_null());
    assert_eq!(detail["run"]["status"], "running");
}

#[tokio::test]
async fn timer_completion_is_a_result_and_existing_timer_records_still_resume() {
    let f = Fixture::new().await;
    let wake = f.schedule().await;
    // Exact persisted shape used by sleep before the unified interface.
    let mut chat = f.service.get("chats", &f.chat).await.unwrap();
    let deadline = now() - 1000;
    chat["wakeup"] = json!({
        "id": wake["id"], "messageId": wake["messageId"], "reason": wake["reason"],
        "createdAt": deadline - 420_000, "wakeAt": deadline, "seconds": 420,
    });
    f.service.store.put("chats", chat).await.unwrap();
    f.finish().await;
    let restarted = Service::new(f.service.config.clone()).await.unwrap();
    restarted.chat_tick(&HashSet::new()).await.unwrap();
    let detail = restarted.chat_detail(&f.chat).await.unwrap();
    assert_eq!(detail["run"]["status"], "queued");
    let run = f.run.clone();
    let events = restarted
        .store
        .read(move |db| db.events(&run, 0, 100))
        .await
        .unwrap();
    let event = events
        .iter()
        .find(|event| event["type"] == "chat.wakeup")
        .unwrap();
    assert_eq!(
        event["payload"]["observation"],
        json!({ "state": "completed", "type": "time", "wakeAt": deadline })
    );
}

#[tokio::test]
async fn fractional_dates_never_schedule_before_the_requested_instant() {
    let f = Fixture::new().await;
    let millis = now() + 60_000;
    let date =
        chrono::DateTime::from_timestamp_millis(millis).unwrap() + chrono::Duration::nanoseconds(1);
    let result = wait_for::schedule(&f.service, &f.token, &json!({ "conditions": [{ "type": "at", "date": date.to_rfc3339() }], "reason": "Precise date" })).await.unwrap();
    assert_eq!(result["wakeup"]["wakeAt"], millis + 1);
}

#[tokio::test]
async fn switching_between_a_date_and_duration_replaces_the_request() {
    let f = Fixture::new().await;
    let date = chrono::DateTime::from_timestamp_millis(now() + 420_000).unwrap();
    let at = wait_for::schedule(
        &f.service,
        &f.token,
        &json!({
            "conditions": [{ "type": "at", "date": date.to_rfc3339() }], "reason": "Wait for CI",
        }),
    )
    .await
    .unwrap();
    let delay = f.schedule().await;
    assert_ne!(delay["id"], at["wakeup"]["id"]);
    assert_eq!(
        delay["timeCondition"],
        json!({ "type": "delay", "seconds": 420 })
    );
    let same_instant =
        chrono::DateTime::from_timestamp_millis(delay["wakeAt"].as_i64().unwrap()).unwrap();
    let at = wait_for::schedule(&f.service, &f.token, &json!({
        "conditions": [{ "type": "at", "date": same_instant.to_rfc3339() }], "reason": "Wait for CI",
    })).await.unwrap();
    assert_ne!(at["wakeup"]["id"], delay["id"]);
}
