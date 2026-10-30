use crate::common::run_fixture::DbRunFixture;
use common::AgentFixture;
use common::MissionFixture;
use common::StoreAgentFixture;
use common::account_fixture::AccountFixtures;
use common::node_fixture::*;

mod common;

use axum::{
    Json, Router,
    body::Body,
    extract::{Request, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::any,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use cairn_installation::{
    accounts,
    config::{Config, MAIN_AGENT_ID, id, now},
    nodes::LOCAL_NODE_ID,
    provider::Provider,
    run_status::RunStatus,
    service::Service,
    validation::text,
};
use common::eventually;
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tempfile::TempDir;
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::{Child, Command},
};

/// The real manager runtime in a separate process, with the synthetic relay
/// transport for installation-only worker tests. Browser access is tested
/// through the Beacon and real installation relay.
struct Fixture {
    root: TempDir,
    service: Arc<Service>,
    process: Option<Child>,
    url: String,
}

impl Fixture {
    async fn new() -> Self {
        Self::with_concurrency(2).await
    }

    async fn with_concurrency(concurrency: usize) -> Self {
        let root = TempDir::new().unwrap();
        std::fs::create_dir(root.path().join("home")).unwrap();
        let config = Config {
            codex_bin: common::fixture("codex.mjs"),
            claude_bin: common::fixture("claude.mjs"),
            opencode_bin: "opencode".into(),
            opencode_go_usage_url: cairn_installation::accounts::opencode::USAGE_URL.into(),
            concurrency,
            worker_enabled: true,
            ..common::config(root.path())
        };
        let service = Service::new(config).await.unwrap();
        // Keep the fixture authority alive while these journeys stop the worker process.
        common::relay_fixture::claimed(&service).await.unwrap();

        let mut fixture = Self {
            root,
            service,
            process: None,
            url: String::new(),
        };
        fixture.start().await;
        fixture
    }

    async fn start(&mut self) {
        let file = self.root.path().join("config.json");
        tokio::fs::write(&file, serde_json::to_vec(&self.service.config).unwrap())
            .await
            .unwrap();
        let binary = std::path::Path::new(env!("CARGO_BIN_EXE_cairn"))
            .parent()
            .unwrap()
            .join("examples/worker_fixture");
        let mut command = Command::new(binary);
        command.arg("serve");
        let usage = self.root.path().join("usage.json");
        if usage.exists() {
            command.env("CAIRN_FIXTURE_USAGE", usage);
        }
        let mut child = command
            .env("CAIRN_CONFIG", file)
            .env_remove("CAIRN_TOOLKIT_DIR")
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
        let line = tokio::time::timeout(Duration::from_secs(10), lines.next_line())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(line.starts_with("Listening on"), "{line}");
        self.url = line.trim_start_matches("Listening on ").to_owned();
        self.process = Some(child);
    }

    /// Stops the worker with SIGTERM, or kills it when `abrupt`.
    async fn stop(&mut self, abrupt: bool) {
        let Some(mut process) = self.process.take() else {
            return;
        };
        if abrupt {
            process.kill().await.unwrap();
            return;
        }
        let pid = libc::pid_t::try_from(process.id().unwrap()).unwrap();
        unsafe {
            libc::kill(pid, libc::SIGTERM);
        }
        tokio::time::timeout(Duration::from_secs(15), process.wait())
            .await
            .unwrap()
            .unwrap();
    }

    async fn enqueue(&self, prompt: &str) -> Value {
        let task = json!({
            "name": "Fixture run",
            "prompt": prompt,
            "worktree": false,
            "agentId": MAIN_AGENT_ID,
        });
        let task = self.service.task_fixture(task, None).await.unwrap();
        self.service
            .enqueue(text(&task, "id"), "manual", None)
            .await
            .unwrap()
    }

    /// Waits until the run `id` satisfies `condition`.
    async fn until(&self, id: &str, condition: impl Fn(&Value) -> bool) -> Value {
        eventually(
            Duration::from_secs(20),
            Duration::from_millis(30),
            async || {
                let run = self.service.store.run(id).await.unwrap();
                condition(&run).then_some(run)
            },
        )
        .await
    }

    async fn until_finished(&self, id: &str) -> Value {
        self.until(id, finished).await
    }

    /// Creates a chat and sends it `message`.
    async fn start_chat(&self, message: Value) -> String {
        let chat = self.service.chat_create(json!({})).await.unwrap();
        let chat_id = text(&chat, "id").to_owned();
        self.service.chat_send(&chat_id, message).await.unwrap();
        chat_id
    }

    /// Waits until the chat has a run and returns its id.
    async fn chat_run(&self, chat_id: &str) -> String {
        eventually(
            Duration::from_secs(10),
            Duration::from_millis(30),
            async || {
                self.service.chat_detail(chat_id).await.unwrap()["runId"]
                    .as_str()
                    .map(str::to_owned)
            },
        )
        .await
    }

    /// Waits until `condition` holds for the chat's details.
    async fn until_chat(&self, chat_id: &str, condition: impl AsyncFn(&Value) -> bool) {
        eventually(
            Duration::from_secs(10),
            Duration::from_millis(30),
            async || {
                let detail = self.service.chat_detail(chat_id).await.unwrap();
                condition(&detail).await.then_some(())
            },
        )
        .await;
    }

    fn run_directory(&self, run: &str) -> PathBuf {
        self.service.config.data_dir.join("runs").join(run)
    }

    async fn checkpoint(&self, run: &str) -> Value {
        self.service
            .store
            .checkpoint_boundary(run)
            .await
            .unwrap()
            .unwrap()
    }
}

fn finished(run: &Value) -> bool {
    matches!(
        RunStatus::of(run),
        Some(RunStatus::Succeeded | RunStatus::Failed)
    )
}

fn message(text: &str) -> Value {
    json!({ "id": id(), "text": text })
}

#[tokio::test]
async fn transient_agent_failure_resumes_same_session_and_workspace_after_restart() {
    let mut fixture = Fixture::new().await;
    let prompt = "fixture:agent-failures:[{\"codexErrorInfo\":\"other\",\"message\":\"workspace routing discovery timed out\"}]";
    let chat_id = fixture.start_chat(message(prompt)).await;
    let run_id = fixture.chat_run(&chat_id).await;
    let mut waiting = fixture
        .until(&run_id, |run| run["retry"].is_object() || finished(run))
        .await;
    assert_eq!(waiting["status"], RunStatus::Queued, "{waiting}");
    assert_eq!(waiting["retry"]["attempt"], 1);
    assert_eq!(waiting["retry"]["limit"], 5);
    assert_eq!(waiting["retry"]["cause"]["kind"], "timeout");
    assert!(waiting["retry"]["nextAttemptAt"].as_i64().unwrap() > now());
    assert_eq!(waiting["sessionId"], "fixture-chat");
    waiting["retry"]["nextAttemptAt"] = (now() + 60_000).into();
    fixture
        .service
        .store
        .change_run(
            &run_id,
            cairn_installation::runs::RunChanges {
                retry: Some(Some(
                    serde_json::from_value(waiting["retry"].clone()).unwrap(),
                )),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    fixture.stop(false).await;
    fixture.start().await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    let retained = fixture.service.store.run(&run_id).await.unwrap();
    assert_eq!(retained["status"], RunStatus::Queued);
    assert_eq!(retained["retry"], waiting["retry"]);
    let mut retry = retained["retry"].clone();
    retry["nextAttemptAt"] = now().into();
    fixture
        .service
        .store
        .change_run(
            &run_id,
            cairn_installation::runs::RunChanges {
                retry: Some(Some(serde_json::from_value(retry.clone()).unwrap())),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let completed = fixture.until_finished(&run_id).await;
    assert_eq!(completed["status"], RunStatus::Succeeded, "{completed}");
    assert_eq!(completed["sessionId"], waiting["sessionId"]);
    assert_eq!(completed["workspace"], waiting["workspace"]);
    let bytes =
        tokio::fs::read(Path::new(text(&completed, "workspace")).join("fixture-retry-work.json"))
            .await
            .unwrap();
    let marker: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(marker["attempts"], 2);
    assert_eq!(marker["cwd"], completed["workspace"]);
    let chat = fixture.service.chat_detail(&chat_id).await.unwrap();
    assert_eq!(chat["paused"], false);
    fixture.stop(false).await;
}

#[tokio::test]
async fn startup_disconnect_before_first_turn_retries_without_losing_the_pending_message() {
    let mut fixture = Fixture::new().await;
    fixture.stop(false).await;
    let chat = fixture
        .start_chat(message("Complete the pending request once."))
        .await;
    fixture
        .service
        .chat_tick(&std::collections::HashSet::default())
        .await
        .unwrap();
    let run_id = fixture.chat_run(&chat).await;
    let home = fixture.run_directory(&run_id).join("codex");
    tokio::fs::create_dir_all(&home).await.unwrap();
    let marker = home.join("fixture-startup-failures.json");
    tokio::fs::write(&marker, br#"{"method":"initialize","remaining":1}"#)
        .await
        .unwrap();
    fixture.start().await;
    let waiting = fixture
        .until(&run_id, |run| run["retry"].is_object() || finished(run))
        .await;
    assert_eq!(waiting["status"], RunStatus::Queued, "{waiting}");
    assert_eq!(waiting["retry"]["cause"]["startup"], true);
    assert!(!waiting["sessionId"].is_string());
    let mut retry = waiting["retry"].clone();
    retry["nextAttemptAt"] = now().into();
    fixture
        .service
        .store
        .change_run(
            &run_id,
            cairn_installation::runs::RunChanges {
                retry: Some(Some(serde_json::from_value(retry.clone()).unwrap())),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let completed = fixture.until_finished(&run_id).await;
    assert_eq!(completed["status"], RunStatus::Succeeded, "{completed}");
    assert_eq!(completed["sessionId"], "fixture-chat");
    assert!(completed["retry"].is_null());
    let attempts: Value = serde_json::from_slice(&tokio::fs::read(marker).await.unwrap()).unwrap();
    assert_eq!(attempts["remaining"], 0);
    assert_eq!(attempts["pids"].as_array().unwrap().len(), 1);
    let chat = fixture.service.chat_detail(&chat).await.unwrap();
    assert_eq!(chat["messages"].as_array().unwrap().len(), 1);
    fixture.stop(false).await;
}

#[tokio::test]
async fn first_start_recovery_remains_manually_resumable_after_stop_or_exhaustion() {
    for ending in ["cancel", "deadline", "exhaustion"] {
        let mut fixture = Fixture::new().await;
        fixture.stop(false).await;
        let chat = fixture
            .start_chat(message("Keep the original pending request."))
            .await;
        fixture
            .service
            .chat_tick(&std::collections::HashSet::default())
            .await
            .unwrap();
        let run_id = fixture.chat_run(&chat).await;
        let home = fixture.run_directory(&run_id).join("codex");
        tokio::fs::create_dir_all(&home).await.unwrap();
        let marker = home.join("fixture-startup-failures.json");
        let failures = if ending == "exhaustion" { 6 } else { 1 };
        tokio::fs::write(
            &marker,
            serde_json::to_vec(&json!({ "method": "initialize", "remaining": failures })).unwrap(),
        )
        .await
        .unwrap();
        fixture.start().await;
        let mut waiting = fixture
            .until(&run_id, |run| run["retry"].is_object() || finished(run))
            .await;
        assert_eq!(waiting["status"], RunStatus::Queued, "{waiting}");
        let workspace = waiting["workspace"].clone();
        if ending == "cancel" {
            fixture
                .service
                .worker
                .cancel(&fixture.service, &run_id)
                .await
                .unwrap();
            fixture
                .until(&run_id, |run| run["status"] == RunStatus::Cancelled)
                .await;
        } else {
            for attempt in 1..=5 {
                assert_eq!(waiting["retry"]["attempt"], attempt);
                let mut retry = waiting["retry"].clone();
                retry["nextAttemptAt"] = now().into();
                if ending == "deadline" {
                    retry["deadlineAt"] = (now() - 1).into();
                }
                fixture
                    .service
                    .store
                    .change_run(
                        &run_id,
                        cairn_installation::runs::RunChanges {
                            retry: Some(Some(serde_json::from_value(retry.clone()).unwrap())),
                            ..Default::default()
                        },
                    )
                    .await
                    .unwrap();
                waiting = fixture
                    .until(&run_id, |run| {
                        finished(run)
                            || (run["status"] == RunStatus::Queued
                                && run["retry"]["attempt"]
                                    .as_u64()
                                    .is_some_and(|n| n > attempt))
                    })
                    .await;
                if finished(&waiting) {
                    break;
                }
            }
            assert_eq!(waiting["status"], RunStatus::Failed, "{waiting}");
        }
        let resumed = fixture
            .service
            .worker
            .resume(&fixture.service, &run_id)
            .await
            .unwrap();
        assert!(
            resumed["retry"].is_null(),
            "Manual resume starts a new budget"
        );
        let completed = fixture.until_finished(&run_id).await;
        assert_eq!(
            completed["status"],
            RunStatus::Succeeded,
            "{ending}: {completed}"
        );
        assert_eq!(completed["workspace"], workspace);
        let detail = fixture.service.chat_detail(&chat).await.unwrap();
        assert_eq!(detail["messages"].as_array().unwrap().len(), 1);
        assert_eq!(detail["messages"][0]["status"], "delivered");
        fixture.stop(false).await;
    }
}

#[tokio::test]
async fn native_recovery_preflight_disconnect_consumes_the_same_retry_budget() {
    let mut fixture = Fixture::new().await;
    let chat = fixture
        .start_chat(message(
            "fixture:agent-failures:[{\"code\":\"server_overloaded\"}]",
        ))
        .await;
    let run_id = fixture.chat_run(&chat).await;
    let waiting = fixture
        .until(&run_id, |run| run["retry"].is_object() || finished(run))
        .await;
    assert_eq!(waiting["status"], RunStatus::Queued, "{waiting}");
    fixture.stop(false).await;
    let marker = fixture
        .run_directory(&run_id)
        .join("codex/fixture-startup-failures.json");
    tokio::fs::write(&marker, br#"{"method":"thread/read","remaining":1}"#)
        .await
        .unwrap();
    let mut retry = waiting["retry"].clone();
    retry["nextAttemptAt"] = now().into();
    fixture
        .service
        .store
        .change_run(
            &run_id,
            cairn_installation::runs::RunChanges {
                retry: Some(Some(serde_json::from_value(retry.clone()).unwrap())),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    fixture.start().await;
    let recovering = fixture
        .until(&run_id, |run| {
            (run["status"] == RunStatus::Queued && run["retry"]["attempt"] == 2) || finished(run)
        })
        .await;
    assert_eq!(recovering["status"], RunStatus::Queued, "{recovering}");
    assert_eq!(recovering["retry"]["cause"]["startup"], true);
    assert_eq!(
        recovering["retry"]["startedAt"],
        waiting["retry"]["startedAt"]
    );
    let mut retry = recovering["retry"].clone();
    retry["nextAttemptAt"] = now().into();
    fixture
        .service
        .store
        .change_run(
            &run_id,
            cairn_installation::runs::RunChanges {
                retry: Some(Some(serde_json::from_value(retry.clone()).unwrap())),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let completed = fixture.until_finished(&run_id).await;
    assert_eq!(completed["status"], RunStatus::Succeeded, "{completed}");
    assert_eq!(completed["workspace"], waiting["workspace"]);
    assert_eq!(completed["sessionId"], waiting["sessionId"]);
    fixture.stop(false).await;
}

#[tokio::test]
async fn an_expired_retry_window_does_not_admit_another_agent() {
    let mut fixture = Fixture::new().await;
    let chat = fixture
        .start_chat(message(
            "fixture:agent-failures:[{\"code\":\"server_overloaded\"}]",
        ))
        .await;
    let run_id = fixture.chat_run(&chat).await;
    let waiting = fixture
        .until(&run_id, |run| run["retry"].is_object() || finished(run))
        .await;
    assert_eq!(waiting["status"], RunStatus::Queued, "{waiting}");
    let mut retry = waiting["retry"].clone();
    retry["nextAttemptAt"] = (now() - 100).into();
    retry["deadlineAt"] = (now() - 1).into();
    fixture
        .service
        .store
        .change_run(
            &run_id,
            cairn_installation::runs::RunChanges {
                retry: Some(Some(serde_json::from_value(retry.clone()).unwrap())),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let stopped = fixture.until_finished(&run_id).await;
    assert_eq!(stopped["status"], RunStatus::Failed);
    assert!(text(&stopped, "error").contains("10-minute limit"));
    let marker: Value = serde_json::from_slice(
        &tokio::fs::read(Path::new(text(&stopped, "workspace")).join("fixture-retry-work.json"))
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(marker["attempts"], 1);
    fixture.stop(false).await;
}

#[tokio::test]
async fn agent_failure_without_a_message_keeps_its_code_for_the_retry() {
    let mut fixture = Fixture::new().await;
    let prompt = "fixture:agent-failures:[{\"codexErrorInfo\":\"serverOverloaded\"}]";
    let chat_id = fixture.start_chat(message(prompt)).await;
    let run_id = fixture.chat_run(&chat_id).await;
    let waiting = fixture
        .until(&run_id, |run| run["retry"].is_object() || finished(run))
        .await;
    assert_eq!(waiting["status"], RunStatus::Queued, "{waiting}");
    assert_eq!(waiting["retry"]["cause"]["kind"], "server");
    let id = run_id.clone();
    let events = fixture
        .service
        .store
        .read(move |db| db.events_fixture(&id, 0, 1000))
        .await
        .unwrap();
    let failures = events
        .iter()
        .filter(|event| event["type"] == "turn.failed")
        .map(|event| event["payload"]["error"].clone())
        .collect::<Vec<_>>();
    assert_eq!(
        failures,
        [json!({ "codexErrorInfo": "serverOverloaded", "message": "Conversation interrupted." })]
    );
    fixture.stop(false).await;
}

#[tokio::test]
async fn agent_retries_stop_after_five_attempts_or_when_the_error_becomes_terminal() {
    for terminal in [false, true] {
        let mut fixture = Fixture::new().await;
        // Codex's report for a model at capacity.
        let capacity = "Selected model is at capacity. Please try a different model.";
        let transient = json!({ "codexErrorInfo": "serverOverloaded", "message": capacity });
        let failures = if terminal {
            vec![
                transient,
                json!({ "code": "authentication_error", "message": "Reconnect account" }),
            ]
        } else {
            vec![transient; 6]
        };
        let prompt = format!("fixture:agent-failures:{}", json!(failures));
        let chat = fixture.start_chat(message(&prompt)).await;
        let run_id = fixture.chat_run(&chat).await;
        let attempts = if terminal { 1 } else { 5 };
        for attempt in 1..=attempts {
            let waiting = fixture
                .until(&run_id, |run| {
                    (run["status"] == RunStatus::Queued && run["retry"]["attempt"] == attempt)
                        || finished(run)
                })
                .await;
            assert_eq!(waiting["status"], RunStatus::Queued, "{waiting}");
            assert_eq!(waiting["retry"]["attempt"], attempt);
            let mut retry = waiting["retry"].clone();
            retry["nextAttemptAt"] = now().into();
            fixture
                .service
                .store
                .change_run(
                    &run_id,
                    cairn_installation::runs::RunChanges {
                        retry: Some(Some(serde_json::from_value(retry.clone()).unwrap())),
                        ..Default::default()
                    },
                )
                .await
                .unwrap();
        }
        let failed = fixture.until_finished(&run_id).await;
        assert_eq!(failed["status"], RunStatus::Failed);
        assert_eq!(failed["retry"]["attempt"], attempts);
        let expected = if terminal {
            "Reconnect account"
        } else {
            capacity
        };
        assert_eq!(failed["summary"], expected);
        let id = run_id.clone();
        let events = fixture
            .service
            .store
            .read(move |db| db.events_fixture(&id, 0, 1000))
            .await
            .unwrap();
        let failures = events
            .iter()
            .filter(|event| event["type"] == "turn.failed")
            .map(|event| text(&event["payload"]["error"], "message"))
            .collect::<Vec<_>>();
        let mut reported = vec![capacity; attempts as usize];
        reported.push(expected);
        assert_eq!(
            failures, reported,
            "Each failed attempt reports its reason once"
        );
        let marker: Value = serde_json::from_slice(
            &tokio::fs::read(Path::new(text(&failed, "workspace")).join("fixture-retry-work.json"))
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(marker["attempts"], attempts + 1);
        fixture.stop(false).await;
    }
}

#[tokio::test]
async fn cancellation_during_retry_wait_does_not_launch_another_agent() {
    let mut fixture = Fixture::new().await;
    let chat = fixture.start_chat(message("fixture:agent-failures:[{\"code\":\"request_timeout\",\"message\":\"Request timed out\"}]")).await;
    let run_id = fixture.chat_run(&chat).await;
    let waiting = fixture
        .until(&run_id, |run| run["retry"].is_object() || finished(run))
        .await;
    assert_eq!(waiting["status"], RunStatus::Queued, "{waiting}");
    fixture
        .service
        .worker
        .cancel(&fixture.service, &run_id)
        .await
        .unwrap();
    fixture
        .until(&run_id, |run| run["status"] == RunStatus::Cancelled)
        .await;
    // The conversation shows the stop: it ends the retry Cairn had scheduled.
    let id = run_id.clone();
    let events = fixture
        .service
        .store
        .read(move |db| db.events_fixture(&id, 0, 1000))
        .await
        .unwrap();
    let after_retry = events
        .iter()
        .skip_while(|event| event["type"] != "retry.scheduled")
        .filter(|event| event["type"] == "status")
        .map(|event| text(event, "text"))
        .collect::<Vec<_>>();
    assert_eq!(after_retry, ["cancelled"], "{events:?}");
    let marker: Value = serde_json::from_slice(
        &tokio::fs::read(Path::new(text(&waiting, "workspace")).join("fixture-retry-work.json"))
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(marker["attempts"], 1);
    fixture.stop(false).await;
}

#[tokio::test]
async fn managed_task_retry_releases_its_account_during_backoff() {
    let mut fixture = Fixture::new().await;
    fixture.stop(false).await;
    let s = fixture.service.clone();
    let mut account = serde_json::to_value(
        s.accounts
            .create(&s, Provider::Codex, "Retry fixture")
            .await
            .unwrap(),
    )
    .unwrap();
    account["state"] = "ready".into();
    account["maxConcurrentRuns"] = 1.into();
    s.store.save_account_fixture(account.clone()).await.unwrap();
    let account_id = text(&account, "id");
    codex_credentials(
        &s,
        account_id,
        json!({
            "access_token": "synthetic",
            "refresh_token": "synthetic-refresh",
            "account_id": "retry-fixture",
        }),
    )
    .await;
    fixture.start().await;
    let run = fixture
        .enqueue("fixture:agent-failures:[{\"message\":\"workspace routing discovery timed out\"}]")
        .await;
    let run_id = text(&run, "id");
    let waiting = fixture
        .until(run_id, |run| run["retry"].is_object() || finished(run))
        .await;
    assert_eq!(waiting["status"], RunStatus::Queued, "{waiting}");
    let mut retry = waiting["retry"].clone();
    retry["nextAttemptAt"] = (now() + 120_000).into();
    s.store
        .change_run(
            run_id,
            cairn_installation::runs::RunChanges {
                retry: Some(Some(serde_json::from_value(retry.clone()).unwrap())),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let other = fixture
        .enqueue("Inspect another workspace during backoff")
        .await;
    let completed = fixture.until_finished(text(&other, "id")).await;
    assert_eq!(completed["status"], RunStatus::Succeeded, "{completed}");
    assert_eq!(completed["accountId"], account_id);
    assert_eq!(
        s.store.run(run_id).await.unwrap()["status"],
        RunStatus::Queued
    );
    retry["nextAttemptAt"] = now().into();
    s.store
        .change_run(
            run_id,
            cairn_installation::runs::RunChanges {
                retry: Some(Some(serde_json::from_value(retry.clone()).unwrap())),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let completed = fixture.until_finished(run_id).await;
    assert_eq!(completed["status"], RunStatus::Succeeded, "{completed}");
    assert_eq!(completed["sessionId"], waiting["sessionId"]);
    assert_eq!(completed["workspace"], waiting["workspace"]);
    fixture.stop(false).await;
}

/// A signed-in Claude Code account whose CLI home holds fixture credentials.
async fn claude_account(s: &Service, parallel_runs: u64) -> String {
    let mut account = serde_json::to_value(
        s.accounts
            .create(s, Provider::Claude, "Claude fixture")
            .await
            .unwrap(),
    )
    .unwrap();
    let id = text(&account, "id").to_owned();
    let home = accounts::claude::account_home(&s.config, &id);
    std::fs::create_dir_all(&home).unwrap();
    let credentials = json!({
        "claudeAiOauth": {
            "accessToken": "fixture-access",
            "refreshToken": "private-refresh",
            "expiresAt": now() + 3_600_000,
            "scopes": ["user:inference"],
        },
    });
    std::fs::write(home.join(".credentials.json"), credentials.to_string()).unwrap();
    account["state"] = "ready".into();
    account["maxConcurrentRuns"] = parallel_runs.into();
    s.store.save_account_fixture(account).await.unwrap();
    id
}

/// Stores synthetic credentials for a Codex account.
async fn codex_credentials(s: &Service, account: &str, tokens: Value) {
    s.vault
        .set(
            &format!("codex-account:{account}"),
            &json!({ "tokens": tokens }),
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn preparing_runs_keep_their_original_cancellation_reservation() {
    let root = TempDir::new().unwrap();
    std::fs::create_dir(root.path().join("home")).unwrap();
    let service = Service::new(Config {
        concurrency: 2,
        codex_bin: common::fixture("codex.mjs"),
        claude_bin: common::fixture("claude.mjs"),
        ..common::config(root.path())
    })
    .await
    .unwrap();
    common::relay_fixture::claimed(&service).await.unwrap();
    let task = service
        .task_fixture(
            json!({
                "name": "Preparing run",
                "prompt": "Inspect the workspace.",
                "worktree": false,
                "agentId": MAIN_AGENT_ID,
            }),
            None,
        )
        .await
        .unwrap();
    let run = service
        .enqueue(text(&task, "id"), "manual", None)
        .await
        .unwrap();
    let run_id = text(&run, "id");
    service.worker.initialize(&service).await.unwrap();

    // Admission reserves the run before asynchronous preparation marks it running.
    // Another tick in that window must preserve the original execution and its stop token.
    let reservation = tokio_util::sync::CancellationToken::new();
    service
        .worker
        .active
        .lock()
        .await
        .insert(run_id.to_owned(), reservation.clone());
    service.worker.tick(&service).await.unwrap();
    reservation.cancel();
    let reservation_retained = service.worker.active.lock().await[run_id].is_cancelled();

    service.shutdown.cancel();
    service.worker.active.lock().await.remove(run_id);
    service.worker.close().await;
    assert!(
        reservation_retained,
        "A queued run already in preparation must not launch a second execution"
    );
}

#[tokio::test]
async fn verbose_tools_do_not_hide_chat_answers_or_failures() {
    for fail in [false, true] {
        let mut fixture = Fixture::new().await;
        let s = &fixture.service;
        let prompt = if fail {
            "fixture:verbose-tools-fail"
        } else {
            "fixture:verbose-tools"
        };
        let chat_id = fixture.start_chat(message(prompt)).await;
        let run_id = fixture.chat_run(&chat_id).await;
        let run = fixture.until_finished(&run_id).await;
        let expected = if fail {
            RunStatus::Failed
        } else {
            RunStatus::Succeeded
        };
        assert_eq!(run["status"], expected);
        let (events, tail) = s
            .store
            .read(move |db| {
                Ok((
                    db.events_fixture(&run_id, 0, 500)?,
                    serde_json::to_value(db.events_before(&run_id, i64::MAX)?.0)?,
                ))
            })
            .await
            .unwrap();
        let tools: Vec<_> = events
            .iter()
            .filter(|e| text(&e["payload"]["item"], "id").starts_with("verbose-"))
            .collect();
        assert_eq!(
            tools.len(),
            60,
            "Every tool outcome must survive verbose output"
        );
        assert!(
            tools.iter().all(|event| {
                event["payload"]["item"]["details_truncated"] == true
                    && serde_json::to_vec(event).unwrap().len() < 16 * 1024
            }),
            "Tool results must be abbreviated instead of dropping later calls"
        );
        if fail {
            assert!(events.iter().any(|e| e["type"] == "turn.failed"
                && text(&e["payload"]["error"], "message") == "Failure after verbose tools"));
        } else {
            assert!(text(&run, "summary").contains("Ready for the next step."));
            assert!(
                tail.as_array().unwrap().iter().any(|event| {
                    let item = &event["payload"]["item"];
                    item["type"] == "agent_message"
                        && text(item, "text").contains("Ready for the next step.")
                }),
                "The completed answer must reach the newest conversation page even after verbose tools"
            );
            assert!(events.iter().any(|e| e["type"] == "item.updated"
                && e["payload"]["item"]["text"] == "Still responding after verbose tools."));
            assert!(events.iter().any(|event| event["type"] == "turn.completed"));
        }
        fixture.stop(false).await;
    }
}

#[tokio::test]
async fn chat_switches_codex_claude_and_back_without_losing_workspace_or_replaying_turns() {
    let mut fixture = Fixture::new().await;
    let s = &fixture.service;
    claude_account(s, 4).await;
    let chat_id = fixture
        .start_chat(message(
            "Keep the existing design and inspect the workspace.",
        ))
        .await;
    let run_id = fixture.chat_run(&chat_id).await;
    let delivered = |message: &Value| {
        let id = message["id"].clone();
        move |run: &Value| {
            run["status"] == RunStatus::Succeeded && run["chatExecution"]["messageId"] == id
        }
    };
    let first = fixture
        .until(&run_id, |r| r["status"] == RunStatus::Succeeded)
        .await;
    let marker = Path::new(text(&first, "workspace")).join("preserved.txt");
    std::fs::write(&marker, "completed work").unwrap();
    let mut to_claude = message("Continue with Claude.");
    to_claude["provider"] = "claude".into();
    to_claude["model"] = "opus[1m]".into();
    s.chat_send(&chat_id, to_claude.clone()).await.unwrap();
    let second = fixture.until(&run_id, delivered(&to_claude)).await;
    assert_eq!(second["snapshot"]["agent"]["provider"], "claude");
    // The native session belongs to the run, not to the account.
    let claude_home = fixture.run_directory(&run_id).join("home/.claude");
    assert_eq!(second["workspace"], first["workspace"]);
    assert_eq!(std::fs::read_to_string(&marker).unwrap(), "completed work");
    let messages = std::fs::read_to_string(claude_home.join("user-messages.jsonl")).unwrap();
    assert!(messages.contains("Keep the existing design"));
    assert!(messages.contains("The approach looks good"));
    assert!(messages.contains("Continue with Claude"));
    let invocations = || std::fs::read_to_string(claude_home.join("invocations.jsonl")).unwrap();
    assert!(!invocations().contains("--resume"));
    // Retry the same delivery after the provider changed: it stays one message.
    s.chat_send(&chat_id, to_claude.clone()).await.unwrap();
    assert_eq!(
        s.chat_detail(&chat_id).await.unwrap()["messages"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    let continuation = message("Keep using Claude.");
    s.chat_send(&chat_id, continuation.clone()).await.unwrap();
    fixture.until(&run_id, delivered(&continuation)).await;
    assert!(invocations().contains("--resume"));
    let mut back = message("Return to Codex and preserve the decisions.");
    back["provider"] = "codex".into();
    s.chat_send(&chat_id, back.clone()).await.unwrap();
    let last = fixture.until(&run_id, delivered(&back)).await;
    assert_eq!(last["snapshot"]["agent"]["provider"], "codex");
    assert_eq!(last["workspace"], first["workspace"]);
    assert!(text(&last, "summary").contains("Claude fixture completed"));
    assert!(text(&last, "summary").contains("Keep the existing design"));
    assert_eq!(std::fs::read_to_string(&marker).unwrap(), "completed work");
    assert_eq!(s.chat_detail(&chat_id).await.unwrap()["runId"], run_id);
    fixture.stop(false).await;
}

#[tokio::test]
async fn chats_continue_with_equivalent_access_but_not_reduced_access() {
    let mut fixture = Fixture::new().await;
    let s = fixture.service.clone();
    let chat_id = fixture.start_chat(message("Inspect the workspace.")).await;
    let run_id = fixture.chat_run(&chat_id).await;
    fixture
        .until(&run_id, |r| r["status"] == RunStatus::Succeeded)
        .await;
    let follow_up = message("Continue with the same access.");
    s.chat_send(&chat_id, follow_up.clone()).await.unwrap();
    fixture
        .until(&run_id, |r| {
            r["status"] == RunStatus::Succeeded
                && r["chatExecution"]["messageId"] == follow_up["id"]
        })
        .await;

    let mut agent = s.agent_snapshot(MAIN_AGENT_ID).await.unwrap();
    agent["access"]["sandbox"] = "read-only".into();
    s.store.put_agent_fixture(agent).await.unwrap();
    s.chat_send(&chat_id, message("Continue with less access."))
        .await
        .unwrap();
    // The queue pauses with the reason instead of reusing the wider workspace.
    let error = eventually(
        Duration::from_secs(10),
        Duration::from_millis(30),
        async || {
            let error = s.store.kv(&format!("chat-error:{chat_id}")).await.unwrap();
            error.and_then(|error| error.as_str().map(str::to_owned))
        },
    )
    .await;
    assert!(error.contains("Agent access was reduced"), "{error}");
    fixture.stop(false).await;
}

#[tokio::test]
async fn chat_messages_invoke_dollar_skills_without_changing_the_visible_text() {
    let mut fixture = Fixture::new().await;
    let s = &fixture.service;
    s.skills
        .save(
            "review",
            "---\nname: review\ndescription: Review the current changes\n---\nReview carefully.\n",
            None,
        )
        .await
        .unwrap();
    let request = "$review the workspace and keep $HOME untouched.";
    let chat_id = fixture.start_chat(message(request)).await;
    let run_id = fixture.chat_run(&chat_id).await;
    let run = fixture
        .until(&run_id, |r| r["status"] == RunStatus::Succeeded)
        .await;
    let execution = text(&run["chatExecution"], "text");
    assert!(execution.starts_with(request), "{execution}");
    assert!(execution.contains("<invoked_skills>"), "{execution}");
    assert!(execution.contains("\n- review\n"), "{execution}");
    assert!(!execution.contains("- HOME"), "{execution}");
    assert_eq!(
        s.chat_detail(&chat_id).await.unwrap()["messages"][0]["text"],
        request
    );
    fixture.stop(false).await;
}

#[tokio::test]
async fn usage_exhaustion_switches_accounts_and_preserves_the_conversation() {
    let mut fixture = Fixture::new().await;
    fixture.stop(false).await;
    let s = &fixture.service;
    let mut accounts = Vec::new();
    let mut usage = json!({});
    for (name, used) in [("More capacity", 10), ("Backup", 30)] {
        let mut account =
            serde_json::to_value(s.accounts.create(s, Provider::Codex, name).await.unwrap())
                .unwrap();
        let id = text(&account, "id").to_owned();
        let limits = json!({
            "ordinaryUsageAllowed": true,
            "rateLimits": {
                "limitId": "codex",
                "primary": {
                    "usedPercent": used,
                    "windowDurationMins": 300,
                    "resetsAt": now() / 1000 + 7200,
                },
            },
        });
        account["state"] = "ready".into();
        account["usage"] = accounts::codex::normalize(&limits);
        s.store.save_account_fixture(account).await.unwrap();
        let tokens = json!({
            "account_id": id,
            "access_token": "synthetic-access",
            "refresh_token": "synthetic-refresh",
        });
        codex_credentials(s, &id, tokens).await;
        usage[&id] = limits;
        accounts.push(id);
    }
    tokio::fs::write(fixture.root.path().join("usage.json"), usage.to_string())
        .await
        .unwrap();
    fixture.start().await;
    let run = fixture.enqueue("fixture:exhaust").await;
    let completed = fixture.until_finished(text(&run, "id")).await;
    assert_eq!(completed["status"], RunStatus::Succeeded, "{completed}");
    assert_eq!(completed["accountId"], accounts[1]);
    assert_eq!(completed["accountName"], "Backup");
    assert_eq!(completed["sessionId"], "fixture-chat");
    fixture.stop(false).await;
}

#[tokio::test]
async fn claude_individual_spend_limit_waits_for_capacity_and_remains_cancellable() {
    let mut fixture = Fixture::new().await;
    let s = fixture.service.clone();
    let account = claude_account(&s, 4).await;
    let mut sent = message("fixture:spend-limit");
    sent["provider"] = "claude".into();
    let chat = fixture.start_chat(sent).await;
    let run_id = fixture.chat_run(&chat).await;
    let waiting = fixture
        .until(&run_id, |run| {
            run["accountWaitReason"].as_str().is_some_and(|reason| {
                reason.contains("available usage") || reason.starts_with("Usage exhausted.")
            })
        })
        .await;
    assert_eq!(waiting["status"], RunStatus::Running, "{waiting}");
    assert!(waiting["sessionId"].is_string(), "{waiting}");
    assert!(waiting["error"].is_null(), "{waiting}");
    let saved = s.store.account_fixture(&account).await.unwrap().unwrap();
    assert!(saved["exhausted"].is_object(), "{saved}");
    let session = common::relay_fixture::context(&s).await;
    reqwest::Client::new()
        .post(format!("{}/api/runs/{run_id}/cancel", fixture.url))
        .header("x-test-relay-token", text(&session, "value"))
        .json(&json!({}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    let cancelled = fixture
        .until(&run_id, |run| run["status"] == RunStatus::Cancelled)
        .await;
    assert_eq!(cancelled["status"], RunStatus::Cancelled, "{cancelled}");
    assert_eq!(cancelled["sessionId"], waiting["sessionId"]);
    assert_eq!(cancelled["workspace"], waiting["workspace"]);
    fixture.stop(false).await;
}

#[tokio::test]
async fn native_worker_records_artifacts_and_completes_task() {
    let mut fixture = Fixture::new().await;
    let run = fixture.enqueue("Inspect the fixture").await;
    let id = text(&run, "id").to_owned();
    let complete = fixture.until_finished(&id).await;
    assert_eq!(complete["status"], RunStatus::Succeeded, "{complete}");
    assert_eq!(complete["sessionId"], "fixture-chat");
    let events = fixture
        .service
        .store
        .read(move |db| db.events_fixture(&id, 0, 500))
        .await
        .unwrap();
    assert!(events.iter().any(|e| e["payload"].is_object()));
    fixture.stop(false).await;
}

#[tokio::test]
async fn parallel_managed_tasks_resume_after_restart_without_refresh_credentials_in_runs() {
    let mut fixture = Fixture::new().await;
    fixture.stop(false).await;
    let s = fixture.service.clone();
    let mut account = serde_json::to_value(
        s.accounts
            .create(&s, Provider::Codex, "Shared")
            .await
            .unwrap(),
    )
    .unwrap();
    account["state"] = "ready".into();
    s.store.save_account_fixture(account.clone()).await.unwrap();
    let account_id = text(&account, "id");
    let tokens = json!({
        "access_token": "synthetic",
        "refresh_token": "secret-refresh",
        "account_id": "shared",
    });
    codex_credentials(&s, account_id, tokens).await;
    fixture.start().await;
    let first = fixture.enqueue("fixture:chat-hang first task").await;
    let second = fixture.enqueue("fixture:chat-hang second task").await;
    for run in [&first, &second] {
        let running = fixture
            .until(text(run, "id"), |r| r["sessionId"] == "fixture-chat")
            .await;
        assert_eq!(running["accountId"], account_id);
        assert!(
            !fixture
                .run_directory(text(run, "id"))
                .join("codex/auth.json")
                .exists()
        );
    }
    fixture.stop(true).await;
    fixture.start().await;
    for run in [&first, &second] {
        let completed = fixture.until_finished(text(run, "id")).await;
        assert_eq!(completed["status"], RunStatus::Succeeded, "{completed}");
        assert_eq!(completed["sessionId"], "fixture-chat");
        assert_eq!(completed["accountId"], account_id);
    }
    assert_eq!(
        s.vault
            .get(&format!("codex-account:{account_id}"))
            .await
            .unwrap()
            .unwrap()["tokens"]["refresh_token"],
        "secret-refresh"
    );
    fixture.stop(false).await;
}

#[tokio::test]
async fn abrupt_restart_fences_previous_process_and_resumes_workspace() {
    let mut fixture = Fixture::new().await;
    let run = fixture.enqueue("fixture:restart").await;
    let id = text(&run, "id");
    let running = fixture
        .until(id, |r| r["sessionId"] == "fixture-chat")
        .await;
    fixture.stop(true).await;
    fixture.start().await;
    let completed = fixture.until_finished(id).await;
    assert_eq!(completed["status"], RunStatus::Succeeded, "{completed}");
    assert_eq!(completed["workspace"], running["workspace"]);
    assert_eq!(completed["sessionId"], running["sessionId"]);
    assert_eq!(completed["resumeCount"], 1);
    assert!(text(&completed, "summary").contains("saved conversation"));
    assert_eq!(completed["snapshot"]["agent"]["timeoutMinutes"], 0);
    assert_eq!(
        fixture.checkpoint(id).await.get("remainingMs"),
        Some(&Value::Null)
    );
    fixture.stop(false).await;
}

#[tokio::test]
async fn automatic_shared_process_turns_do_not_acquire_a_managed_node_owner() {
    let mut fixture = Fixture::new().await;
    let s = fixture.service.clone();
    s.agent_fixture(
        json!({ "access": { "backend": "auto" } }),
        Some(MAIN_AGENT_ID),
    )
    .await
    .unwrap();
    let chat = fixture.start_chat(message("First automatic message")).await;
    let run = fixture.chat_run(&chat).await;
    let first = fixture.until_finished(&run).await;
    assert_eq!(first["status"], RunStatus::Succeeded, "{first}");
    assert_eq!(
        first["snapshot"]["agent"]["access"]["backend"],
        "firecracker"
    );
    let checkpoint = fixture.checkpoint(&run).await;
    assert!(checkpoint["nodeId"].is_null(), "{checkpoint}");
    assert!(checkpoint["runnerId"].is_null(), "{checkpoint}");
    assert!(s.store.list("node-volumes").await.unwrap().is_empty());
    assert!(
        s.store
            .list("node-attempts")
            .await
            .unwrap()
            .iter()
            .all(|attempt| attempt["released"] == true)
    );
    s.chat_send(&chat, message("Second automatic message"))
        .await
        .unwrap();
    let second = fixture
        .until(&run, |r| {
            r["status"] == RunStatus::Succeeded && r["resumeCount"] == 1
        })
        .await;
    assert_eq!(second["sessionId"], first["sessionId"]);
    assert_eq!(second["workspace"], first["workspace"]);
    fixture.stop(false).await;
}

#[tokio::test]
async fn native_chat_turns_reuse_the_same_run_and_conversation() {
    let mut fixture = Fixture::new().await;
    let s = fixture.service.clone();
    let chat_id = fixture.start_chat(message("First message")).await;
    let run_id = fixture.chat_run(&chat_id).await;
    let first = fixture.until_finished(&run_id).await;
    assert_eq!(first["status"], RunStatus::Succeeded, "{first}");
    s.chat_send(&chat_id, message("Second message"))
        .await
        .unwrap();
    let second = fixture
        .until(&run_id, |r| {
            r["status"] == RunStatus::Succeeded && text(r, "summary").contains("Second message")
        })
        .await;
    assert_eq!(second["sessionId"], "fixture-chat");
    assert_eq!(s.chat_detail(&chat_id).await.unwrap()["runId"], run_id);
    s.chat_send(&chat_id, message("fixture:disconnect"))
        .await
        .unwrap();
    let failed = fixture
        .until(&run_id, |r| r["status"] == RunStatus::Failed)
        .await;
    assert!(
        !text(&failed, "summary").contains("Second message"),
        "A failed attempt reused the previous result: {failed}"
    );
    assert!(text(&failed, "error").contains("Codex"), "{failed}");
    fixture.stop(false).await;
}

#[tokio::test]
async fn sleeping_chat_survives_worker_restart_and_releases_capacity_before_waking() {
    let mut fixture = Fixture::with_concurrency(1).await;
    let s = fixture.service.clone();
    let chat = fixture
        .start_chat(message(
            "fixture:slow-finish Wait for CI then finish the original task",
        ))
        .await;
    let run = fixture.chat_run(&chat).await;
    let current = fixture
        .until(&run, |r| {
            r["status"] == "running" && r["sessionId"].is_string()
        })
        .await;
    fixture
        .until_chat(&chat, async |detail| {
            detail["messages"][0]["status"] == "delivered"
        })
        .await;
    // Host-only fixtures use a synthetic workspace grant; production VMs receive
    // this same turn-bound grant through run_configuration.
    let token = "synthetic-sleep-token";
    s.store
        .transaction({
            let grant: cairn_installation::mcps::record::RunGrant = serde_json::from_value(json!({
                "runId": run,
                "messageId": current["chatExecution"]["messageId"],
                "servers": {},
                "installation": false, "workspace": true,
            }))
            .unwrap();
            move |db| {
                db.save_mcp_run_grant(&cairn_installation::auth::hex_digest(token), &grant, None)
            }
        })
        .await
        .unwrap();
    let wake = cairn_installation::chats::wait_for::schedule(
        &s,
        token,
        &json!({ "conditions": [{ "type": "delay", "seconds": 15 }], "reason": "Wait for CI" }),
    )
    .await
    .unwrap()["wakeup"]
        .clone();
    let first = fixture.until_finished(&run).await;
    assert_eq!(first["status"], "succeeded");
    let conversation = fixture
        .run_directory(&run)
        .join("codex/fixture-conversation.json");
    let first_thread: Value =
        serde_json::from_slice(&std::fs::read(&conversation).unwrap()).unwrap();
    // Use the actual workspace: preparation may select a generation directory.
    let preserved = Path::new(text(&first, "workspace")).join("sleep-preserved.txt");
    std::fs::write(&preserved, "work before the wait").unwrap();
    fixture.stop(false).await;
    fixture.start().await;
    let other = fixture
        .enqueue("Another conversation uses capacity during the wait")
        .await;
    assert_eq!(
        fixture.until_finished(text(&other, "id")).await["status"],
        "succeeded"
    );
    assert_eq!(s.chat_detail(&chat).await.unwrap()["wakeup"], wake);
    let resumed = fixture
        .until(&run, |r| {
            r["status"] == "succeeded"
                && text(r, "summary").contains("Cairn resumed this conversation")
        })
        .await;
    assert_eq!(resumed["sessionId"], first["sessionId"]);
    let run_id = run.clone();
    let events = s
        .store
        .read(move |db| db.events_fixture(&run_id, 0, 100))
        .await
        .unwrap();
    let wake_event = events
        .iter()
        .find(|event| event["type"] == "chat.wakeup")
        .unwrap();
    assert!(wake_event["createdAt"].as_i64().unwrap() >= wake["wakeAt"].as_i64().unwrap());
    assert_eq!(resumed["workspace"], first["workspace"]);
    let resumed_thread: Value =
        serde_json::from_slice(&std::fs::read(conversation).unwrap()).unwrap();
    assert_eq!(
        resumed_thread["fixtureGeneration"],
        first_thread["fixtureGeneration"]
    );
    assert_eq!(resumed_thread["turns"].as_array().unwrap().len(), 2);
    assert_eq!(resumed_thread["turns"][0], first_thread["turns"][0]);
    assert_eq!(
        std::fs::read_to_string(preserved).unwrap(),
        "work before the wait"
    );
    let detail = s.chat_detail(&chat).await.unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|event| event["type"] == "chat.user")
            .count(),
        1
    );
    assert!(detail["wakeup"].is_null());
    fixture.stop(false).await;
}

#[tokio::test]
async fn waiting_mission_survives_restart_and_releases_capacity_without_finishing() {
    let mut fixture = Fixture::with_concurrency(1).await;
    let s = fixture.service.clone();
    fixture.stop(false).await;
    let mut account = serde_json::to_value(
        s.accounts
            .create(&s, Provider::Codex, "Mission wait fixture")
            .await
            .unwrap(),
    )
    .unwrap();
    account["state"] = "ready".into();
    account["maxConcurrentRuns"] = 1.into();
    s.store.save_account_fixture(account.clone()).await.unwrap();
    codex_credentials(&s, text(&account, "id"), json!({
        "access_token": "synthetic", "refresh_token": "synthetic-refresh", "account_id": "mission-wait-fixture",
    })).await;
    fixture.start().await;
    let run = fixture
        .enqueue("fixture:slow-finish Complete this mission after CI")
        .await;
    let run_id = text(&run, "id");
    let first = fixture
        .until(run_id, |run| {
            run["status"] == "running" && run["sessionId"].is_string()
        })
        .await;
    let token = "synthetic-mission-wait-token";
    s.store
        .transaction({
            let grant: cairn_installation::mcps::record::RunGrant = serde_json::from_value(json!({
                "runId": run_id, "messageId": null, "servers": {}, "workspace": true, "installation": false,
            }))
            .unwrap();
            move |db| {
                db.save_mcp_run_grant(&cairn_installation::auth::hex_digest(token), &grant, None)
            }
        })
        .await
        .unwrap();
    let wait = cairn_installation::waits::schedule(
        &s,
        token,
        &json!({
            "conditions": [{ "type": "delay", "seconds": 15 }], "reason": "Wait for mission CI",
        }),
    )
    .await
    .unwrap();
    assert_eq!(wait["deferred"], true);
    let waiting = fixture
        .until(run_id, |run| run["status"] == "waiting")
        .await;
    assert!(waiting["finishedAt"].is_null(), "{waiting}");
    assert!(waiting["outcome"].is_null());
    assert!(
        s.enqueue(text(&run, "taskId"), "manual", None)
            .await
            .is_err(),
        "A waiting mission still owns its task slot"
    );
    let preserved = Path::new(text(&waiting, "workspace")).join("mission-wait.txt");
    std::fs::write(&preserved, "work before mission wait").unwrap();
    fixture.stop(false).await;
    fixture.start().await;
    let other = fixture
        .enqueue("Use the only slot while the mission waits")
        .await;
    assert_eq!(
        fixture.until_finished(text(&other, "id")).await["status"],
        "succeeded"
    );
    assert_eq!(s.store.run(run_id).await.unwrap()["wakeup"], wait["wakeup"]);
    let resumed = fixture
        .until(run_id, |run| {
            run["status"] == "succeeded"
                && text(run, "summary").contains("Cairn resumed this mission")
        })
        .await;
    assert_eq!(resumed["sessionId"], first["sessionId"]);
    assert_eq!(resumed["workspace"], waiting["workspace"]);
    assert_eq!(
        std::fs::read_to_string(preserved).unwrap(),
        "work before mission wait"
    );
    assert!(resumed["wakeup"].is_null());
    let thread: Value = serde_json::from_slice(
        &std::fs::read(
            fixture
                .run_directory(run_id)
                .join("codex/fixture-conversation.json"),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(thread["turns"].as_array().unwrap().len(), 2);
    assert!(
        s.store.conversations_boundary().await.unwrap().is_empty(),
        "Mission waits never create a hidden chat"
    );
    let id = run_id.to_owned();
    let events = s
        .store
        .read(move |db| db.events_fixture(&id, 0, 300))
        .await
        .unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|event| event["type"] == "status" && event["text"] == "succeeded")
            .count(),
        1
    );
    let wake = events
        .iter()
        .find(|event| event["type"] == "run.wakeup")
        .unwrap();
    assert!(wake["createdAt"].as_i64().unwrap() >= wait["wakeup"]["wakeAt"].as_i64().unwrap());
    fixture.stop(false).await;
}

#[tokio::test]
async fn stopped_waiting_mission_manual_resume_starts_a_new_provider_turn() {
    let mut fixture = Fixture::with_concurrency(1).await;
    fixture.stop(false).await;
    let s = fixture.service.clone();
    let mut account = serde_json::to_value(
        s.accounts
            .create(&s, Provider::Codex, "Manual mission resume")
            .await
            .unwrap(),
    )
    .unwrap();
    account["state"] = "ready".into();
    s.store.save_account_fixture(account.clone()).await.unwrap();
    codex_credentials(&s, text(&account, "id"), json!({
        "access_token": "synthetic", "refresh_token": "synthetic-refresh", "account_id": "manual-mission-resume",
    })).await;
    fixture.start().await;
    let run = fixture
        .enqueue("fixture:slow-finish Finish after the remote result")
        .await;
    let run_id = text(&run, "id");
    fixture
        .until(run_id, |run| {
            run["status"] == "running" && run["sessionId"].is_string()
        })
        .await;
    let token = "synthetic-manual-mission-token";
    s.store
        .transaction({
            let grant: cairn_installation::mcps::record::RunGrant = serde_json::from_value(json!({
                "runId": run_id, "messageId": null, "servers": {}, "workspace": true, "installation": false,
            }))
            .unwrap();
            move |db| {
                db.save_mcp_run_grant(&cairn_installation::auth::hex_digest(token), &grant, None)
            }
        })
        .await
        .unwrap();
    cairn_installation::waits::schedule(&s, token, &json!({
        "conditions": [{ "type": "delay", "seconds": 600 }], "reason": "Wait for the remote result",
    })).await.unwrap();
    let waiting = fixture
        .until(run_id, |run| run["status"] == "waiting")
        .await;
    fixture.stop(false).await;
    s.worker.cancel(&s, run_id).await.unwrap();
    assert_eq!(s.store.run(run_id).await.unwrap()["status"], "cancelled");
    s.worker.resume(&s, run_id).await.unwrap();
    fixture.start().await;
    let completed = fixture
        .until(run_id, |run| {
            run["status"] == "succeeded"
                && text(run, "summary").contains("Execution was interrupted")
        })
        .await;
    assert_eq!(completed["sessionId"], waiting["sessionId"]);
    assert_eq!(completed["workspace"], waiting["workspace"]);
    assert!(completed["missionTurnId"].is_string());
    assert_ne!(completed["missionTurnId"], waiting["missionTurnId"]);
    assert!(completed["waitContinuation"].is_null());
    let thread: Value = serde_json::from_slice(
        &std::fs::read(
            fixture
                .run_directory(run_id)
                .join("codex/fixture-conversation.json"),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(
        thread["turns"].as_array().unwrap().len(),
        2,
        "The completed wait turn must never be replayed as a successful continuation"
    );
    fixture.stop(false).await;
}

const PNG: &[u8] = b"\x89PNG\r\n\x1a\nfixture";

#[tokio::test]
async fn condition_wait_yields_the_only_slot_to_its_dependency_and_resumes_the_same_session() {
    let mut fixture = Fixture::with_concurrency(1).await;
    let s = fixture.service.clone();
    s.agent_fixture(
        json!({ "name": "Main agent", "access": { "installation": true } }),
        Some(MAIN_AGENT_ID),
    )
    .await
    .unwrap();
    let chat = fixture
        .start_chat(message("fixture:slow-finish Finish after other work"))
        .await;
    let run = fixture.chat_run(&chat).await;
    let current = fixture
        .until(&run, |r| {
            r["status"] == "running" && r["sessionId"].is_string()
        })
        .await;
    fixture
        .until_chat(&chat, async |detail| {
            detail["messages"][0]["status"] == "delivered"
        })
        .await;
    let other = fixture
        .enqueue("Dependency runs while the conversation waits")
        .await;
    let token = "synthetic-condition-token";
    s.store
        .transaction({
            let grant: cairn_installation::mcps::record::RunGrant = serde_json::from_value(json!({
                "runId": run,
                "messageId": current["chatExecution"]["messageId"],
                "servers": {},
                "workspace": true,
                "installation": true,
            }))
            .unwrap();
            move |db| {
                db.save_mcp_run_grant(&cairn_installation::auth::hex_digest(token), &grant, None)
            }
        })
        .await
        .unwrap();
    let wait = cairn_installation::chats::wait_for::schedule(
        &s,
        token,
        &json!({
            "conditions": [{ "type": "run_finished", "runId": other["id"] }],
            "reason": "Join the dependency",
            "timeoutSeconds": 60,
        }),
    )
    .await
    .unwrap();
    assert_eq!(wait["deferred"], true);
    assert_eq!(
        fixture.until_finished(text(&other, "id")).await["status"],
        "succeeded"
    );
    let resumed = fixture
        .until(&run, |r| {
            r["status"] == "succeeded"
                && text(r, "summary").contains("Cairn resumed this conversation")
        })
        .await;
    assert_eq!(resumed["sessionId"], current["sessionId"]);
    assert!(
        now() < wait["wakeup"]["wakeAt"].as_i64().unwrap(),
        "The result resumes the conversation before the fallback timer"
    );
    assert!(text(&resumed, "summary").contains(text(&other, "id")));
    let thread: Value = serde_json::from_slice(
        &std::fs::read(
            fixture
                .run_directory(&run)
                .join("codex/fixture-conversation.json"),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(thread["turns"].as_array().unwrap().len(), 2);
    let detail = s.chat_detail(&chat).await.unwrap();
    assert!(detail["wakeup"].is_null());
    let run_id = run.clone();
    let events = s
        .store
        .read(move |db| db.events_fixture(&run_id, 0, 100))
        .await
        .unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|event| event["type"] == "chat.user")
            .count(),
        1
    );
    fixture.stop(false).await;
}

#[tokio::test]
async fn chat_attachments_survive_worker_restart_and_reach_codex() {
    let mut fixture = Fixture::new().await;
    let s = fixture.service.clone();
    let chat = s.chat_create(json!({})).await.unwrap();
    let chat_id = text(&chat, "id");
    let attachment_id = id();
    let upload = axum::http::Request::builder()
        .method("PUT")
        .uri("/?name=design.png")
        .body(Body::from(PNG))
        .unwrap();
    s.attachment_http(chat_id, &attachment_id, upload)
        .await
        .unwrap();
    let mut hanging = message("fixture:chat-hang");
    hanging["attachmentIds"] = json!([attachment_id]);
    s.chat_send(chat_id, hanging).await.unwrap();
    let run_id = fixture.chat_run(chat_id).await;
    fixture
        .until(&run_id, |r| r["sessionId"] == "fixture-chat")
        .await;
    // Wait for the original input receipt, so recovery resumes the accepted turn.
    fixture
        .until_chat(chat_id, async |chat: &Value| {
            chat["messages"][0]["status"] == "delivered"
        })
        .await;
    fixture.stop(true).await;
    fixture.start().await;
    let completed = fixture.until_finished(&run_id).await;
    assert_eq!(completed["status"], RunStatus::Succeeded, "{completed}");
    assert_eq!(completed["resumeCount"], 1);
    let conversation = fixture
        .run_directory(&run_id)
        .join("codex/fixture-conversation.json");
    let thread: Value = serde_json::from_slice(&std::fs::read(conversation).unwrap()).unwrap();
    let turns = thread["turns"].as_array().unwrap();
    assert_eq!(turns.len(), 2);
    for turn in turns {
        let find = |items: &Value, kind: &str| {
            items
                .as_array()
                .unwrap()
                .iter()
                .find(|item| item["type"] == kind)
                .unwrap()
                .clone()
        };
        let user = find(&turn["items"], "userMessage");
        let image = find(&user["content"], "localImage");
        assert_eq!(std::fs::read(text(&image, "path")).unwrap(), PNG);
    }
    fixture.stop(false).await;
}

/// A runner controller whose VM attempts are interrupted `failures` times.
struct Controller {
    data: PathBuf,
    plans: tokio::sync::Mutex<Vec<Value>>,
    failures: usize,
    slots: usize,
    hold: std::sync::atomic::AtomicBool,
}

impl Controller {
    /// Position of `attempt` among the launched plans.
    async fn attempt_index(&self, attempt: &str) -> usize {
        let plans = self.plans.lock().await;
        plans.iter().position(|plan| plan["id"] == attempt).unwrap()
    }
}

async fn serve_controller(State(state): State<Arc<Controller>>, request: Request) -> Response {
    let path = request.uri().path();
    if path == "/health" {
        return Json(json!({
            "status": "ok",
            "nodeProtocol": 2,
            "dataRoot": state.data,
            "sharedResources": true,
            "budget": { "slots": state.slots, "limits": { "cpu": 7, "memoryMiB": 15872, "diskMiB": 104857 } },
            "runtimeId": "development",

            "capabilities": {
                "os": "linux",
                "arch": "x86_64",
                "kvm": true,
                "cpu": 8,
                "memoryMiB": 16384,
                "diskMiB": 131_072,
            },
        }))
        .into_response();
    }
    if path == "/node-budget" {
        return (StatusCode::CONFLICT, Json(json!({ "error": "Shared RAM budget is below current usage plus the controller reserve." }))).into_response();
    }
    if path.ends_with("/lease") {
        return Json(json!({})).into_response();
    }
    if path.ends_with("/administrative-pause") {
        return Json(json!({ "administrativePausedMs": 0 })).into_response();
    }
    if path.ends_with("/snapshot") {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    if request.method() == "DELETE" {
        return Json(json!({})).into_response();
    }
    if !path.starts_with("/runs/") {
        return StatusCode::NOT_FOUND.into_response();
    }
    let attempt = path.split('/').nth(2).unwrap();
    if path.ends_with("/logs") {
        let mut output =
            String::from("{\"type\":\"thread.started\",\"thread_id\":\"fixture-session\"}\n");
        let plans = state.plans.lock().await;
        let plan = plans.iter().find(|plan| plan["id"] == attempt).unwrap();
        if plan["chat"]["execution"]["messageId"].is_string() {
            let delivered = json!({
                "type": "chat.delivered",
                "messageId": plan["chat"]["execution"]["messageId"],
            });
            output.push_str(&format!("{delivered}\n"));
        }
        drop(plans);
        if state.attempt_index(attempt).await >= state.failures {
            output.push_str("{\"type\":\"item.completed\",\"item\":{\"id\":\"reply\",\"type\":\"agent_message\",\"text\":\"resumed VM\"}}\n{\"type\":\"turn.completed\",\"usage\":{}}\n");
        }
        let frame = json!({ "type": "output", "stderr": false, "data": STANDARD.encode(output) });
        return Body::from(format!("{frame}\n")).into_response();
    }
    if path.ends_with("/wait") {
        while state.hold.load(std::sync::atomic::Ordering::Relaxed) {
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
        let code = if state.attempt_index(attempt).await < state.failures {
            143
        } else {
            0
        };
        return Json(json!({ "StatusCode": code })).into_response();
    }
    if request.method() != "POST" {
        return StatusCode::NOT_FOUND.into_response();
    }
    let plan = state
        .data
        .join("runner-plans")
        .join(format!("{attempt}.json"));
    let bytes = tokio::fs::read(plan).await.unwrap();
    state
        .plans
        .lock()
        .await
        .push(serde_json::from_slice(&bytes).unwrap());
    Json(json!({})).into_response()
}

#[tokio::test]
async fn controller_reads_do_not_launch_vm_attempts() {
    let root = TempDir::new().unwrap();
    let controller = Arc::new(Controller {
        data: root.path().to_owned(),
        plans: tokio::sync::Mutex::new(Vec::new()),
        failures: 0,
        slots: 1,
        hold: std::sync::atomic::AtomicBool::new(false),
    });
    let request = |path| {
        Request::builder()
            .method("GET")
            .uri(path)
            .body(Body::empty())
            .unwrap()
    };
    let response = serve_controller(
        State(controller.clone()),
        request("/runs/fixture/administrative-pause"),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), 1024)
        .await
        .unwrap();
    let value: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(value["administrativePausedMs"], 0);

    let unknown = serve_controller(
        State(controller.clone()),
        request("/runs/fixture/unknown-read"),
    )
    .await;
    assert_eq!(unknown.status(), StatusCode::NOT_FOUND);
    assert!(controller.plans.lock().await.is_empty());
}

#[tokio::test]
async fn node_slots_exceed_manager_concurrency_without_overpreparing_the_queue() {
    let mut fixture = Fixture::new().await;
    fixture.stop(false).await;
    let controller = Arc::new(Controller {
        data: fixture.service.config.data_dir.clone(),
        plans: tokio::sync::Mutex::new(Vec::new()),
        failures: 0,
        slots: 12,
        hold: std::sync::atomic::AtomicBool::new(true),
    });
    let app = Router::new()
        .fallback(any(serve_controller))
        .with_state(controller.clone());
    let (listener, address) = common::bind().await;
    common::reconfigure(&mut fixture.service, |config| {
        config.runner_url = format!("http://{address}");
    })
    .await;
    let server = common::serve(listener, app);
    let home = fixture.service.config.home.join(".codex");
    tokio::fs::create_dir_all(&home).await.unwrap();
    tokio::fs::write(home.join("auth.json"), "{}")
        .await
        .unwrap();
    tokio::fs::write(
        fixture.service.config.data_dir.join("storage-s3.json"),
        json!({ "bucket": "fixture-storage", "endpoint": "https://127.0.0.1:1" }).to_string(),
    )
    .await
    .unwrap();
    cairn_installation::nodes::refresh_local(&fixture.service)
        .await
        .unwrap();
    let mut node = fixture
        .service
        .store
        .node_boundary(LOCAL_NODE_ID)
        .await
        .unwrap()
        .unwrap();
    node["slots"] = 12.into();
    fixture.service.store.put_node_fixture(node).await.unwrap();
    let mut runs = Vec::new();
    for _ in 0..13 {
        runs.push(fixture.enqueue("Inspect the VM fixture").await);
    }
    fixture.start().await;
    eventually(
        Duration::from_secs(20),
        Duration::from_millis(30),
        async || (controller.plans.lock().await.len() == 12).then_some(()),
    )
    .await;
    tokio::time::sleep(Duration::from_millis(1200)).await;
    let mut queued = Vec::new();
    let mut running = 0;
    for run in &runs {
        let current = fixture.service.store.run(text(run, "id")).await.unwrap();
        if current["status"] == RunStatus::Queued {
            queued.push(current);
        } else {
            assert_eq!(current["status"], RunStatus::Running, "{current}");
            running += 1;
        }
    }
    assert_eq!(running, 12);
    assert_eq!(queued.len(), 1);
    assert!(
        queued[0]["startedAt"].is_null(),
        "queued work must never enter recovery: {}",
        queued[0]
    );
    assert_eq!(queued[0]["recoveryPending"], false);
    assert_eq!(controller.plans.lock().await.len(), 12);
    let attempts = fixture.service.store.list("node-attempts").await.unwrap();
    assert_eq!(attempts.len(), 12);
    controller
        .hold
        .store(false, std::sync::atomic::Ordering::Relaxed);
    for run in &runs {
        assert_eq!(
            fixture.until_finished(text(run, "id")).await["status"],
            RunStatus::Succeeded
        );
    }
    assert_eq!(controller.plans.lock().await.len(), 13);
    fixture.stop(false).await;
    server.abort();
}

/// The local VM controller of an installation, which calls `cairn_workspace`
/// while an attempt runs, as its guest would: through the run's MCP channel.
struct WorkspaceProbe {
    controller: Arc<Controller>,
    other_run_token: String,
    calls: tokio::sync::Mutex<Vec<Value>>,
}

impl WorkspaceProbe {
    async fn call(&self, plan: &Value) -> Value {
        let home = plan["imports"]
            .as_array()
            .unwrap()
            .iter()
            .find(|import| import["target"] == "/home/node")
            .map(|import| PathBuf::from(text(import, "source")))
            .unwrap();
        let url = text(
            &plan["chat"]["codexConfig"]["mcp_servers"]["cairn_workspace"],
            "url",
        )
        .to_owned();
        let token = text(&plan["mcpEnv"], "CAIRN_MCP_RUN_TOKEN");
        let client = reqwest::Client::builder()
            .unix_socket(home.join("cairn-mcp.sock"))
            .timeout(Duration::from_secs(10))
            .build()
            .unwrap();
        let request = |url: &str, token: &str| {
            let body = json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": { "name": "list_nodes", "arguments": {} },
            });
            client
                .post(url)
                .bearer_auth(token)
                .header("mcp-protocol-version", "2025-11-25")
                .json(&body)
                .send()
        };
        let Ok(response) = request(&url, token).await else {
            return json!({ "url": url, "reachable": false });
        };
        let status = response.status().as_u16();
        let reply: Value = response.json().await.unwrap_or_default();
        let other_run = request(&url, &self.other_run_token).await.unwrap().status();
        // Only `/mcp-workspace` and `/mcp-gateway/{id}` exist on the channel.
        let mut outside = Vec::new();
        for path in [
            "/api/mcp".to_owned(),
            format!("/mcp-gateway/{}/extra", id()),
            "/mcp-gateway/".to_owned(),
        ] {
            let url = format!("http://127.0.0.1:5202{path}");
            let status = request(&url, token).await.unwrap().status();
            outside.push(json!([path, status.as_u16()]));
        }
        json!({
            "url": url,
            "reachable": true,
            "status": status,
            "reply": reply,
            "otherRun": other_run.as_u16(),
            "outside": outside,
        })
    }
}

async fn serve_workspace_probe(
    State(state): State<Arc<WorkspaceProbe>>,
    request: Request,
) -> Response {
    let path = request.uri().path().to_owned();
    if path.starts_with("/runs/") && path.ends_with("/wait") {
        let attempt = path.split('/').nth(2).unwrap();
        let plan = state
            .controller
            .data
            .join("runner-plans")
            .join(format!("{attempt}.json"));
        let plan: Value = serde_json::from_slice(&tokio::fs::read(plan).await.unwrap()).unwrap();
        let call = state.call(&plan).await;
        state.calls.lock().await.push(call);
    }
    serve_controller(State(state.controller.clone()), request).await
}

#[tokio::test]
async fn vm_agents_call_workspace_tools_through_their_run_channel_without_a_public_url() {
    let mut fixture = Fixture::new().await;
    fixture.stop(false).await;
    let controller = Arc::new(Controller {
        data: fixture.service.config.data_dir.clone(),
        plans: tokio::sync::Mutex::new(Vec::new()),
        failures: 0,
        slots: 4,
        hold: std::sync::atomic::AtomicBool::new(false),
    });
    // A token granted to another run of the same installation.
    let other_run = json!({ "id": id(), "snapshot": { "agent": { "id": MAIN_AGENT_ID, "provider": "codex" } } });
    let other = fixture
        .service
        .mcps
        .run_configuration(&fixture.service, &other_run)
        .await
        .unwrap();
    let probe = Arc::new(WorkspaceProbe {
        controller: controller.clone(),
        other_run_token: text(&other["env"], "CAIRN_MCP_RUN_TOKEN").to_owned(),
        calls: tokio::sync::Mutex::new(Vec::new()),
    });
    let app = Router::new()
        .fallback(any(serve_workspace_probe))
        .with_state(probe.clone());
    let (listener, address) = common::bind().await;
    // The installer's private manager origin: unreachable from inside a VM.
    common::reconfigure(&mut fixture.service, |config| {
        config.public_url = "http://manager:4310".into();
        config.runner_url = format!("http://{address}");
    })
    .await;
    let server = common::serve(listener, app);
    let home = fixture.service.config.home.join(".codex");
    tokio::fs::create_dir_all(&home).await.unwrap();
    tokio::fs::write(home.join("auth.json"), "{}")
        .await
        .unwrap();
    tokio::fs::write(
        fixture.service.config.data_dir.join("storage-s3.json"),
        json!({ "bucket": "fixture-storage", "endpoint": "https://127.0.0.1:1" }).to_string(),
    )
    .await
    .unwrap();
    cairn_installation::nodes::refresh_local(&fixture.service)
        .await
        .unwrap();
    let run = fixture.enqueue("Open a project").await;
    fixture.start().await;

    let finished = fixture.until_finished(text(&run, "id")).await;

    assert_eq!(finished["status"], RunStatus::Succeeded, "{finished}");
    let calls = probe.calls.lock().await.clone();
    assert_eq!(calls.len(), 1, "{calls:?}");
    let call = &calls[0];
    assert_eq!(call["url"], "http://127.0.0.1:5202/mcp-workspace", "{call}");
    assert_eq!(call["reachable"], true, "{call}");
    assert_eq!(call["status"], 200, "{call}");
    assert!(call["reply"]["error"].is_null(), "{call}");
    assert_ne!(call["reply"]["result"]["isError"], true, "{call}");
    assert_eq!(call["otherRun"], 401, "{call}");
    for outside in call["outside"].as_array().unwrap() {
        assert_eq!(outside[1], 404, "{outside}");
    }
    // Disk reads still use the private origin; the agent never does.
    let args = controller.plans.lock().await[0]["args"].to_string();
    assert!(!args.contains("manager:4310"), "{args}");
    fixture.stop(false).await;
    server.abort();
}

#[tokio::test]
async fn refused_local_budget_keeps_presence_and_exposes_the_reason() {
    let mut fixture = Fixture::new().await;
    fixture.stop(false).await;
    let controller = Arc::new(Controller {
        data: fixture.service.config.data_dir.clone(),
        plans: tokio::sync::Mutex::new(Vec::new()),
        failures: 0,
        slots: 4,
        hold: std::sync::atomic::AtomicBool::new(false),
    });
    let app = Router::new()
        .fallback(any(serve_controller))
        .with_state(controller);
    let (listener, address) = common::bind().await;
    common::reconfigure(&mut fixture.service, |config| {
        config.runner_url = format!("http://{address}");
    })
    .await;
    let server = common::serve(listener, app);
    cairn_installation::nodes::refresh_local(&fixture.service)
        .await
        .unwrap();
    let mut node = fixture
        .service
        .store
        .node_boundary(LOCAL_NODE_ID)
        .await
        .unwrap()
        .unwrap();
    node["limits"]["memoryMiB"] = 256.into();
    node["lastSeen"] = 0.into();
    fixture.service.store.put_node_fixture(node).await.unwrap();
    cairn_installation::nodes::refresh_local(&fixture.service)
        .await
        .unwrap();
    let node = fixture
        .service
        .store
        .node_boundary(LOCAL_NODE_ID)
        .await
        .unwrap()
        .unwrap();
    assert!(node["lastSeen"].as_i64().unwrap() > now() - 1000);
    assert_eq!(node["executionReady"], false);
    assert_eq!(node["capabilities"]["cpu"], 8);
    assert!(text(&node, "budgetError").contains("below current usage"));
    let mut restored = node;
    restored["limits"]["memoryMiB"] = 15872.into();
    fixture
        .service
        .store
        .put_node_fixture(restored)
        .await
        .unwrap();
    cairn_installation::nodes::refresh_local(&fixture.service)
        .await
        .unwrap();
    let node = fixture
        .service
        .store
        .node_boundary(LOCAL_NODE_ID)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(node["executionReady"], true);
    assert_eq!(node["budgetError"], Value::Null);
    server.abort();
}

#[tokio::test]
async fn controller_capacity_rejection_waits_and_retries_without_an_agent_session() {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    for (status, reason, capacity, deferred_fence) in [
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "All VM slots are occupied.",
            true,
            true,
        ),
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "Shared node resources are under pressure.",
            true,
            false,
        ),
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "Temporary admission failure.",
            false,
            false,
        ),
        (
            StatusCode::FORBIDDEN,
            "All VM slots are occupied.",
            false,
            false,
        ),
    ] {
        let mut fixture = Fixture::new().await;
        fixture.stop(false).await;
        let controller = Arc::new(Controller {
            data: fixture.service.config.data_dir.clone(),
            plans: tokio::sync::Mutex::new(Vec::new()),
            failures: 0,
            slots: 4,
            hold: AtomicBool::new(false),
        });
        let busy = Arc::new(AtomicBool::new(true));
        let rejections = Arc::new(AtomicUsize::new(0));
        let fence_blocked = Arc::new(AtomicBool::new(deferred_fence));
        let reject = busy.clone();
        let count = rejections.clone();
        let fence = fence_blocked.clone();
        let app = Router::new()
            .fallback(any(
                move |state: State<Arc<Controller>>, request: Request| {
                    let busy = reject.clone();
                    let rejections = count.clone();
                    let fence_blocked = fence.clone();
                    async move {
                        let path = request.uri().path();
                        if request.method() == "DELETE" && fence_blocked.load(Ordering::SeqCst) {
                            return StatusCode::SERVICE_UNAVAILABLE.into_response();
                        }
                        let starting = request.method() == "POST"
                            && path.starts_with("/runs/")
                            && path.trim_matches('/').split('/').count() == 2;
                        if starting && busy.load(Ordering::SeqCst) {
                            rejections.fetch_add(1, Ordering::SeqCst);
                            return (status, Json(json!({ "error": reason }))).into_response();
                        }
                        serve_controller(state, request).await
                    }
                },
            ))
            .with_state(controller.clone());
        let (listener, address) = common::bind().await;
        common::reconfigure(&mut fixture.service, |config| {
            config.runner_url = format!("http://{address}");
        })
        .await;
        let server = common::serve(listener, app);
        let s = fixture.service.clone();
        let home = s.config.home.join(".codex");
        tokio::fs::create_dir_all(&home).await.unwrap();
        tokio::fs::write(home.join("auth.json"), "{}")
            .await
            .unwrap();
        tokio::fs::write(
            s.config.data_dir.join("storage-s3.json"),
            json!({ "bucket": "fixture-storage", "endpoint": "https://127.0.0.1:1" }).to_string(),
        )
        .await
        .unwrap();
        let run = fixture.enqueue("Wait for an available VM slot").await;
        let run_id = text(&run, "id");
        fixture.start().await;
        let waiting = fixture
            .until(run_id, |run| {
                finished(run)
                    || (run["status"] == RunStatus::Queued
                        && (text(run, "accountWaitReason").starts_with("Waiting for capacity.")
                            || (deferred_fence
                                && rejections.load(Ordering::SeqCst) > 0
                                && run["recoveryPending"] == true)))
            })
            .await;
        if !capacity {
            assert_eq!(waiting["status"], RunStatus::Failed, "{waiting}");
            fixture.stop(false).await;
            server.abort();
            continue;
        }
        assert_eq!(waiting["status"], RunStatus::Queued, "{waiting}");
        assert!(waiting["sessionId"].is_null());
        assert!(waiting["error"].is_null());
        assert!(waiting["finishedAt"].is_null());
        assert!(waiting["retry"].is_null());
        assert_eq!(fixture.checkpoint(run_id).await["launched"], false);
        if deferred_fence {
            tokio::time::sleep(Duration::from_millis(1200)).await;
            assert_eq!(
                rejections.load(Ordering::SeqCst),
                1,
                "No relaunch before fencing"
            );
            assert!(fixture.checkpoint(run_id).await["settled"].is_null());
            fence_blocked.store(false, Ordering::SeqCst);
        }
        let waiting = fixture
            .until(run_id, |run| {
                finished(run) || text(run, "accountWaitReason").starts_with("Waiting for capacity.")
            })
            .await;
        assert_eq!(waiting["status"], RunStatus::Queued, "{waiting}");
        assert!(text(&waiting, "accountWaitReason").contains(reason));
        let workspace = text(&waiting, "workspace").to_owned();
        let marker = Path::new(&workspace).join("preserved.txt");
        std::fs::write(&marker, "preserved while waiting").unwrap();

        // Capacity waits survive a manager restart and never consume the bounded
        // recovery budget intended for interrupted native agent sessions.
        fixture.stop(false).await;
        fixture.start().await;
        let previous_rejections = rejections.load(Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(1200)).await;
        assert_eq!(
            rejections.load(Ordering::SeqCst),
            previous_rejections,
            "Capacity wait must survive restart without relaunching immediately"
        );
        for _ in 0..4 {
            fixture.stop(false).await;
            let mut saved = fixture.checkpoint(run_id).await;
            saved["capacityRetryAt"] = (cairn_installation::config::now() - 1).into();
            s.store
                .save_checkpoint_boundary(run_id, saved)
                .await
                .unwrap();
            let previous = rejections.load(Ordering::SeqCst);
            fixture.start().await;
            eventually(
                Duration::from_secs(10),
                Duration::from_millis(30),
                async || (rejections.load(Ordering::SeqCst) > previous).then_some(()),
            )
            .await;
            fixture
                .until(run_id, |run| run["status"] == RunStatus::Queued)
                .await;
        }
        assert!(controller.plans.lock().await.is_empty());
        fixture.stop(false).await;
        let mut saved = fixture.checkpoint(run_id).await;
        saved["capacityRetryAt"] = (cairn_installation::config::now() - 1).into();
        s.store
            .save_checkpoint_boundary(run_id, saved)
            .await
            .unwrap();
        busy.store(false, Ordering::SeqCst);
        fixture.start().await;
        let completed = fixture.until_finished(run_id).await;
        assert_eq!(completed["status"], RunStatus::Succeeded, "{completed}");
        assert_eq!(completed["workspace"], workspace);
        assert_eq!(
            std::fs::read_to_string(marker).unwrap(),
            "preserved while waiting"
        );
        assert_eq!(controller.plans.lock().await.len(), 1);
        fixture.stop(false).await;
        server.abort();
    }
}

#[tokio::test]
async fn failed_provider_startup_can_retry_without_replaying_an_uncertain_launch() {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    for (provider, rejected, deferred_fence) in [
        ("claude", true, true),
        ("claude", true, false),
        ("claude", false, false),
        ("codex", true, false),
    ] {
        let mut fixture = Fixture::new().await;
        fixture.stop(false).await;
        let controller = Arc::new(Controller {
            data: fixture.service.config.data_dir.clone(),
            plans: tokio::sync::Mutex::new(Vec::new()),
            failures: 0,
            slots: 4,
            hold: AtomicBool::new(false),
        });
        let failing = Arc::new(AtomicBool::new(false));
        let failures = failing.clone();
        let fence_failures = Arc::new(AtomicUsize::new(0));
        let app = Router::new()
            .fallback(any(move |state: State<Arc<Controller>>, request: Request| {
                let failing = failures.clone();
                let fence_failures = fence_failures.clone();
                async move {
                    let path = request.uri().path();
                    let starting = request.method() == "POST"
                        && path.starts_with("/runs/")
                        && path.trim_matches('/').split('/').count() == 2;
                    let reject_start = rejected && starting;
                    let lose_output = !rejected && path.ends_with("/logs");
                    let fence_failed = request.method() == "DELETE"
                        && fence_failures
                            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| {
                                remaining.checked_sub(1)
                            })
                            .is_ok();
                    if fence_failed {
                        return StatusCode::SERVICE_UNAVAILABLE.into_response();
                    }
                    if failing.load(Ordering::SeqCst) && (reject_start || lose_output) {
                        if deferred_fence && reject_start {
                            // Client cleanup and the first worker fence fail;
                            // execution cleanup must settle the rejection later.
                            fence_failures.store(2, Ordering::SeqCst);
                        }
                        return (
                            StatusCode::SERVICE_UNAVAILABLE,
                            Json(json!({
                                "error": "Temporary admission failure (Bearer fixture-private-token).",
                            })),
                        ).into_response();
                    }
                    serve_controller(state, request).await
                }
            }))
            .with_state(controller.clone());
        let (listener, address) = common::bind().await;
        common::reconfigure(&mut fixture.service, |config| {
            config.runner_url = format!("http://{address}");
        })
        .await;
        let server = common::serve(listener, app);
        let s = fixture.service.clone();
        claude_account(&s, 4).await;
        let home = s.config.home.join(".codex");
        tokio::fs::create_dir_all(&home).await.unwrap();
        tokio::fs::write(home.join("auth.json"), "{}")
            .await
            .unwrap();
        tokio::fs::write(
            s.config.data_dir.join("storage-s3.json"),
            json!({
                "bucket": "fixture-storage",
                "endpoint": "https://127.0.0.1:1",
            })
            .to_string(),
        )
        .await
        .unwrap();
        fixture.start().await;

        let original = message("Keep the existing workspace and decisions.");
        let chat_id = fixture.start_chat(original).await;
        let run_id = fixture.chat_run(&chat_id).await;
        let first = fixture
            .until(&run_id, |run| run["status"] == RunStatus::Succeeded)
            .await;
        let marker = Path::new(text(&first, "workspace")).join("preserved.txt");
        std::fs::write(&marker, "completed work").unwrap();

        failing.store(true, Ordering::SeqCst);
        let mut next = message("Continue the same work.");
        next["provider"] = provider.into();
        s.chat_send(&chat_id, next.clone()).await.unwrap();
        let failed = fixture
            .until(&run_id, |run| run["status"] == RunStatus::Failed)
            .await;
        let new_session = provider == "claude";
        assert_eq!(failed["sessionId"].is_null(), new_session);
        assert_eq!(
            fixture.checkpoint(&run_id).await["launched"],
            !new_session || !rejected
        );
        assert_eq!(failed["workspace"], first["workspace"]);
        assert_eq!(std::fs::read_to_string(&marker).unwrap(), "completed work");
        if rejected {
            let id = run_id.clone();
            let events = s
                .store
                .read(move |db| db.events_fixture(&id, 0, 500))
                .await
                .unwrap();
            assert!(events.iter().any(|event| {
                text(event, "text").contains("HTTP 503")
                    && text(event, "text").contains("Temporary admission failure")
            }));
            assert!(
                !events
                    .iter()
                    .any(|event| text(event, "text").contains("fixture-private-token"))
            );
        }
        fixture
            .until_chat(&chat_id, async |chat| {
                chat["paused"] == true && chat["messages"][1]["status"] == "queued"
            })
            .await;

        failing.store(false, Ordering::SeqCst);
        let router = common::relay_fixture::router(s.clone()).await.unwrap();
        let session = common::RelayContext::new(&common::relay_fixture::context(&s).await);
        let request = session
            .authorize(common::request(
                "POST",
                &format!("/api/chats/{chat_id}/pause"),
            ))
            .header("content-type", "application/json")
            .body(Body::from(json!({ "paused": false }).to_string()))
            .unwrap();
        let response = common::send(&router, request).await;
        if new_session && !rejected {
            assert_eq!(response.status(), StatusCode::CONFLICT);
            assert_eq!(s.chat_detail(&chat_id).await.unwrap()["paused"], true);
        } else {
            assert_eq!(
                response.status(),
                StatusCode::OK,
                "{}",
                common::read_json(response).await
            );
            let completed = fixture
                .until(&run_id, |run| {
                    run["status"] == RunStatus::Succeeded
                        && run["chatExecution"]["messageId"] == next["id"]
                })
                .await;
            assert_eq!(completed["workspace"], first["workspace"]);
            assert_eq!(std::fs::read_to_string(&marker).unwrap(), "completed work");

            let chat = s.chat_detail(&chat_id).await.unwrap();
            assert_eq!(chat["messages"].as_array().unwrap().len(), 2);
            assert_eq!(chat["messages"][1]["id"], next["id"]);
            assert_eq!(chat["messages"][1]["status"], "delivered");

            let plans = controller.plans.lock().await;
            let accepted_attempts = plans
                .iter()
                .filter(|plan| plan["chat"]["execution"]["messageId"] == next["id"])
                .count();
            assert_eq!(
                accepted_attempts, 1,
                "The interrupted message must be accepted exactly once",
            );
            let retried = plans.last().unwrap();
            assert_eq!(retried["chat"]["sessionId"].is_null(), new_session);
            assert_eq!(retried["chat"]["execution"]["recovery"], !new_session);
            if new_session {
                assert!(
                    retried["chat"]["execution"]["context"]
                        .as_str()
                        .unwrap()
                        .contains("Keep the existing workspace")
                );
            }
        }
        fixture.stop(false).await;
        server.abort();
    }
}

#[tokio::test]
async fn controller_interruptions_resume_saved_threads_and_stop_after_three_recoveries() {
    for failures in [1, usize::MAX] {
        let mut fixture = Fixture::new().await;
        fixture.stop(false).await;
        let controller = Arc::new(Controller {
            data: fixture.service.config.data_dir.clone(),
            plans: tokio::sync::Mutex::new(Vec::new()),
            failures,
            slots: 4,
            hold: std::sync::atomic::AtomicBool::new(false),
        });
        let app = Router::new()
            .fallback(any(serve_controller))
            .with_state(controller.clone());
        let (listener, address) = common::bind().await;
        common::reconfigure(&mut fixture.service, |config| {
            config.runner_url = format!("http://{address}");
        })
        .await;
        let server = common::serve(listener, app);
        let home = fixture.service.config.home.join(".codex");
        tokio::fs::create_dir_all(&home).await.unwrap();
        tokio::fs::write(home.join("auth.json"), "{}")
            .await
            .unwrap();
        tokio::fs::write(
            fixture.service.config.data_dir.join("storage-s3.json"),
            json!({ "bucket": "fixture-storage", "endpoint": "https://127.0.0.1:1" }).to_string(),
        )
        .await
        .unwrap();
        fixture.start().await;
        let run = fixture.enqueue("Inspect the VM fixture").await;
        let run_id = text(&run, "id");
        let completed = fixture.until_finished(run_id).await;
        let (expected, attempts) = if failures == 1 {
            (RunStatus::Succeeded, 2)
        } else {
            (RunStatus::Failed, 4)
        };
        assert_eq!(completed["status"], expected, "{completed}");
        assert_eq!(completed["sessionId"], "fixture-session");
        let plans = controller.plans.lock().await;
        assert_eq!(plans.len(), attempts);
        for plan in plans.iter().skip(1) {
            assert_eq!(plan["chat"]["sessionId"], "fixture-session", "{plan}");
            assert_eq!(plan["runId"], run_id);
            assert_eq!(plan["cwd"], plans[0]["cwd"]);
        }
        drop(plans);
        fixture.stop(false).await;
        server.abort();
    }
}

/// Exercises the real manager -> runner client -> disposable HTTP controller.
/// Even at the replay ceiling a read of the same attempt is permitted.
#[tokio::test]
async fn controller_evidence_is_reconciled_before_replay_or_terminal_failure() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    for (completed, terminal, expected) in [
        (true, 0, RunStatus::Succeeded),
        (true, 70, RunStatus::Failed),
        (false, 75, RunStatus::Failed),
    ] {
        let mut fixture = Fixture::new().await;
        fixture.stop(false).await;
        let controller = Arc::new(Controller {
            data: fixture.service.config.data_dir.clone(),
            plans: tokio::sync::Mutex::new(Vec::new()),
            failures: if completed { 0 } else { usize::MAX },
            slots: 4,
            hold: std::sync::atomic::AtomicBool::new(false),
        });
        let waits = Arc::new(AtomicUsize::new(0));
        let count = waits.clone();
        let app = Router::new()
            .fallback(any(
                move |state: State<Arc<Controller>>, request: Request| {
                    let count = count.clone();
                    async move {
                        if request.uri().path().ends_with("/wait") {
                            let observation = count.fetch_add(1, Ordering::SeqCst);
                            // The client's completion channel was cut; the manager can
                            // subsequently read the already available controller exit.
                            let code = if observation == 0 { 75 } else { terminal };
                            return Json(json!({ "StatusCode": code })).into_response();
                        }
                        serve_controller(state, request).await
                    }
                },
            ))
            .with_state(controller.clone());
        let (listener, address) = common::bind().await;
        common::reconfigure(&mut fixture.service, |config| {
            config.runner_url = format!("http://{address}");
        })
        .await;
        let server = common::serve(listener, app);
        let home = fixture.service.config.home.join(".codex");
        tokio::fs::create_dir_all(&home).await.unwrap();
        tokio::fs::write(home.join("auth.json"), "{}")
            .await
            .unwrap();
        tokio::fs::write(
            fixture.service.config.data_dir.join("storage-s3.json"),
            json!({ "bucket": "fixture-storage", "endpoint": "https://127.0.0.1:1" }).to_string(),
        )
        .await
        .unwrap();
        let run = fixture
            .enqueue("Reconcile only this disposable controller attempt")
            .await;
        let run_id = text(&run, "id");
        fixture
            .service
            .store
            .transaction({
                let run_id = run_id.to_owned();
                move |db| {
                    db.save_checkpoint_boundary(
                        &run_id,
                        &json!({ "controllerRecoveries": 3, "remainingMs": null }),
                    )
                }
            })
            .await
            .unwrap();
        fixture.start().await;
        let result = fixture.until_finished(run_id).await;
        assert_eq!(result["status"], expected, "{result}");
        assert_eq!(
            controller.plans.lock().await.len(),
            1,
            "A read must never submit another attempt"
        );
        assert_eq!(fixture.checkpoint(run_id).await["controllerRecoveries"], 3);
        assert!(waits.load(Ordering::SeqCst) >= 2);
        if completed && terminal != 0 {
            assert!(text(&result, "error").contains("finalization"), "{result}");
        }
        fixture.stop(false).await;
        server.abort();
    }
}

/// Manager SIGTERM is a local stop, not a controller completion observation.
/// A recorded turn must retain the original attempt for reconciliation on restart.
#[tokio::test]
async fn manager_shutdown_between_turn_completion_and_wait_preserves_same_attempt_evidence() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    for interruption in ["shutdown", "cancel", "signal", "client-error", "timeout"] {
        let cancel = interruption == "cancel";
        let mut fixture = Fixture::new().await;
        fixture.stop(false).await;
        let controller = Arc::new(Controller {
            data: fixture.service.config.data_dir.clone(),
            plans: tokio::sync::Mutex::new(Vec::new()),
            failures: 0,
            slots: 4,
            hold: std::sync::atomic::AtomicBool::new(false),
        });
        let waits = Arc::new(AtomicUsize::new(0));
        let count = waits.clone();
        let app = Router::new()
            .fallback(any(
                move |state: State<Arc<Controller>>, request: Request| {
                    let count = count.clone();
                    async move {
                        if request.uri().path().ends_with("/wait") {
                            let observation = count.fetch_add(1, Ordering::SeqCst);
                            if interruption == "client-error" && observation < 4 {
                                // Initial wait and all three client recovery reads fail
                                // decoding. The CLI returns local exit 1, not remote 1.
                                return Json(json!({})).into_response();
                            }
                            if interruption != "client-error" && observation == 0 {
                                // Only the first client read is interrupted. Persisted
                                // controller evidence is available to subsequent reads.
                                return std::future::pending::<Response>().await;
                            }
                            return Json(json!({ "StatusCode": 0 })).into_response();
                        }
                        serve_controller(state, request).await
                    }
                },
            ))
            .with_state(controller.clone());
        let (listener, address) = common::bind().await;
        common::reconfigure(&mut fixture.service, |config| {
            config.runner_url = format!("http://{address}");
        })
        .await;
        let server = common::serve(listener, app);
        let home = fixture.service.config.home.join(".codex");
        tokio::fs::create_dir_all(&home).await.unwrap();
        tokio::fs::write(home.join("auth.json"), "{}")
            .await
            .unwrap();
        tokio::fs::write(
            fixture.service.config.data_dir.join("storage-s3.json"),
            json!({ "bucket": "fixture-storage", "endpoint": "https://127.0.0.1:1" }).to_string(),
        )
        .await
        .unwrap();
        let run = fixture
            .enqueue("Stop only this disposable manager after turn.completed")
            .await;
        let run_id = text(&run, "id");
        fixture
            .service
            .store
            .transaction({
                let run_id = run_id.to_owned();
                move |db| {
                    db.save_checkpoint_boundary(
                        &run_id,
                        &json!({
                            "controllerRecoveries": 3,
                            "remainingMs": if interruption == "timeout" { json!(4000) } else { Value::Null },
                        }),
                    )
                }
            })
            .await
            .unwrap();
        fixture.start().await;
        eventually(
            Duration::from_secs(10),
            Duration::from_millis(10),
            async || {
                let saved = fixture
                    .service
                    .store
                    .checkpoint_boundary(run_id)
                    .await
                    .unwrap()?;
                (saved["completed"] == true && waits.load(Ordering::SeqCst) >= 1).then_some(())
            },
        )
        .await;
        if cancel {
            // Persisted user cancellation takes precedence even when shutdown
            // interrupts the client before the controller's healthy exit arrives.
            fixture
                .service
                .store
                .change_run(
                    run_id,
                    cairn_installation::runs::RunChanges {
                        cancel_requested_at: Some(Some(now())),
                        ..Default::default()
                    },
                )
                .await
                .unwrap();
        }
        if interruption == "signal" {
            let saved = fixture.checkpoint(run_id).await;
            let process = saved["process"].clone();
            let pid = u32::try_from(process["pid"].as_u64().unwrap()).unwrap();
            let identity = cairn_installation::recovery::process_identity(pid)
                .await
                .unwrap();
            assert_eq!(serde_json::to_value(identity).unwrap(), process);
            // The checkpoint identifies the supervisor. Find only its runner
            // child, validating the exact private attempt before sending a signal.
            let mut tasks = tokio::fs::read_dir(format!("/proc/{pid}/task"))
                .await
                .unwrap();
            let mut runner = None;
            while let Some(task) = tasks.next_entry().await.unwrap() {
                let children = tokio::fs::read_to_string(task.path().join("children"))
                    .await
                    .unwrap();
                for child in children.split_whitespace() {
                    let args = tokio::fs::read(format!("/proc/{child}/cmdline"))
                        .await
                        .unwrap();
                    let args = args.split(|byte| *byte == 0).collect::<Vec<_>>();
                    let command_matches = args.contains(&b"runner-client".as_slice());
                    let attempt_matches = args.contains(&text(&saved, "runnerId").as_bytes());
                    if command_matches && attempt_matches {
                        runner = Some(child.parse::<i32>().unwrap());
                    }
                }
            }
            let runner = runner.expect("Fixture supervisor must own its runner-client");
            assert_eq!(unsafe { libc::kill(runner, libc::SIGKILL) }, 0);
        }
        if matches!(interruption, "shutdown" | "cancel") {
            fixture.stop(false).await;
            let stopped = fixture.service.store.run(run_id).await.unwrap();
            let checkpoint = fixture.checkpoint(run_id).await;
            assert_eq!(checkpoint["completed"], true);
            assert!(
                checkpoint["terminalCode"].is_null(),
                "Local stop must not fabricate terminal proof: {checkpoint}"
            );
            if !cancel {
                assert_eq!(stopped["status"], RunStatus::Queued, "{stopped}");
                assert!(checkpoint["settled"].is_null(), "{checkpoint}");
            }
            fixture.start().await;
        }
        if cancel {
            let result = fixture
                .until(run_id, |run| run["status"] == "cancelled")
                .await;
            assert_eq!(result["status"], RunStatus::Cancelled);
            assert_eq!(waits.load(Ordering::SeqCst), 1);
        } else if interruption == "timeout" {
            let result = fixture.until_finished(run_id).await;
            assert_eq!(result["status"], RunStatus::Failed, "{result}");
            assert!(text(&result, "error").contains("time limit"), "{result}");
            assert!(fixture.checkpoint(run_id).await["terminalCode"].is_null());
            assert_eq!(waits.load(Ordering::SeqCst), 1);
        } else {
            let result = fixture.until_finished(run_id).await;
            assert_eq!(
                result["status"],
                RunStatus::Succeeded,
                "{interruption}: {result}"
            );
            assert_eq!(fixture.checkpoint(run_id).await["terminalCode"], 0);
            assert_eq!(
                waits.load(Ordering::SeqCst),
                if interruption == "client-error" { 5 } else { 2 }
            );
        }
        assert_eq!(
            controller.plans.lock().await.len(),
            1,
            "Reconciliation must never launch another agent"
        );
        assert_eq!(fixture.checkpoint(run_id).await["controllerRecoveries"], 3);
        fixture.stop(false).await;
        server.abort();
    }
}

#[tokio::test]
async fn vm_chat_protocol_runs_receive_one_frame_with_plugin_instructions() {
    let mut fixture = Fixture::new().await;
    fixture.stop(false).await;
    let controller = Arc::new(Controller {
        data: fixture.service.config.data_dir.clone(),
        plans: tokio::sync::Mutex::new(Vec::new()),
        failures: 0,
        slots: 4,
        hold: std::sync::atomic::AtomicBool::new(false),
    });
    let app = Router::new()
        .fallback(any(serve_controller))
        .with_state(controller.clone());
    let (listener, address) = common::bind().await;
    common::reconfigure(&mut fixture.service, |config| {
        config.runner_url = format!("http://{address}");
    })
    .await;
    let server = common::serve(listener, app);
    let s = fixture.service.clone();
    claude_account(&s, 4).await;
    tokio::fs::write(
        s.config.data_dir.join("storage-s3.json"),
        json!({ "bucket": "fixture-storage", "endpoint": "https://127.0.0.1:1" }).to_string(),
    )
    .await
    .unwrap();
    let agent = s
        .agent_fixture(json!({ "name": "Claude", "provider": "claude" }), None)
        .await
        .unwrap();
    common::save_plugin(
        &s,
        "onepassword",
        &json!({
            "name": "Production", "enabled": true,
            "secrets": { "token": "ops_fixture" }, "agentIds": [agent["id"]],
        }),
        None,
    )
    .await
    .unwrap();
    let plugin = "cairn plugin onepassword read --account ACCOUNT_ID";
    fixture.start().await;

    // Scheduled work: the frame is in the instructions, the turn is the brief.
    // A leading command stays text: Claude Code would run `/clear` itself.
    let brief = "/clear stale branches, then check the deployment.";
    let task = json!({
        "name": "Scheduled", "prompt": brief, "worktree": false, "agentId": agent["id"],
    });
    let task = s.task_fixture(task, None).await.unwrap();
    let run = s.enqueue(text(&task, "id"), "manual", None).await.unwrap();
    let finished = fixture.until_finished(text(&run, "id")).await;
    assert_eq!(finished["status"], RunStatus::Succeeded, "{finished}");
    let plan = controller.plans.lock().await[0]["chat"].clone();
    let instructions = text(&plan, "instructions");
    assert!(instructions.contains(plugin), "{instructions}");
    assert!(instructions.contains("This unattended task"));
    assert!(!instructions.contains("This is an interactive chat"));
    assert!(!instructions.contains(brief));
    assert_eq!(plan["execution"]["text"], format!("Task brief:\n{brief}"));
    assert_eq!(plan["execution"]["recovery"], false);

    // Chat turns stay interactive and also receive plugin instructions.
    let chat = s
        .chat_create(json!({ "agentId": agent["id"] }))
        .await
        .unwrap();
    let chat_id = text(&chat, "id").to_owned();
    s.chat_send(&chat_id, message("Hello")).await.unwrap();
    let chat_run = fixture.chat_run(&chat_id).await;
    fixture
        .until(&chat_run, |run| run["status"] == RunStatus::Succeeded)
        .await;
    let plan = controller.plans.lock().await.last().unwrap()["chat"].clone();
    let instructions = text(&plan, "instructions");
    assert!(instructions.contains(plugin), "{instructions}");
    assert!(instructions.contains("This is an interactive chat"));
    assert!(!instructions.contains("This unattended task"));
    assert_eq!(plan["execution"]["text"], "Hello");
    fixture.stop(false).await;
    server.abort();
}

#[tokio::test]
async fn claude_conversations_run_together_and_lowering_limit_does_not_cancel_them() {
    let mut fixture = Fixture::new().await;
    let s = fixture.service.clone();
    let account = claude_account(&s, 2).await;
    let mut chats = Vec::new();
    let mut runs = Vec::new();
    for prompt in [
        "fixture:batch",
        "fixture:batch",
        "Complete third conversation",
    ] {
        let mut sent = message(prompt);
        sent["provider"] = "claude".into();
        sent["model"] = "sonnet".into();
        let chat_id = fixture.start_chat(sent).await;
        runs.push(fixture.chat_run(&chat_id).await);
        chats.push(chat_id);
    }
    for run in &runs[..2] {
        fixture
            .until(run, |r| {
                r["status"] == RunStatus::Running && r["sessionId"].is_string()
            })
            .await;
    }
    // Both real fixture subprocesses hold independent turns.
    for run in &runs[..2] {
        let credentials = std::fs::read_to_string(
            fixture
                .run_directory(run)
                .join("home/.claude/.credentials.json"),
        )
        .unwrap();
        assert!(!credentials.contains("refresh"));
        assert!(credentials.contains("fixture-access"));
    }
    common::account_fixture::update_account_fixture(
        &s,
        &account,
        &json!({ "maxConcurrentRuns": 1 }),
    )
    .await
    .unwrap();
    for run in &runs[..2] {
        assert_eq!(
            s.store.run(run).await.unwrap()["status"],
            RunStatus::Running
        );
    }
    let answer = async |chat: &str| {
        let mut sent = message("fixture:batch Finish the independent work");
        sent["mode"] = "steer".into();
        s.chat_send(chat, sent).await.unwrap();
    };
    answer(&chats[0]).await;
    fixture
        .until(&runs[0], |r| r["status"] == RunStatus::Succeeded)
        .await;
    fixture
        .until(&runs[2], |r| {
            text(r, "accountWaitReason").contains("free Claude Code account slot")
        })
        .await;
    assert_eq!(
        s.store.run(&runs[1]).await.unwrap()["status"],
        RunStatus::Running
    );
    answer(&chats[1]).await;
    for run in &runs[1..] {
        fixture
            .until(run, |r| r["status"] == RunStatus::Succeeded)
            .await;
    }
    fixture.stop(false).await;
}

#[tokio::test]
async fn unlimited_runs_cancel_and_resume_without_creating_a_deadline() {
    let mut fixture = Fixture::new().await;
    let s = fixture.service.clone();
    let run = fixture.enqueue("fixture:restart").await;
    let id = text(&run, "id");
    assert_eq!(run["snapshot"]["agent"]["timeoutMinutes"], 0);
    fixture
        .until(id, |r| r["sessionId"] == "fixture-chat")
        .await;
    assert_eq!(s.store.run(id).await.unwrap()["status"], RunStatus::Running);
    assert_eq!(
        fixture.checkpoint(id).await.get("remainingMs"),
        Some(&Value::Null)
    );
    let session = common::relay_fixture::context(&s).await;
    let client = reqwest::Client::new();
    let command = |action: &str| {
        client
            .post(format!("{}/api/runs/{id}/{action}", fixture.url))
            .header("x-test-relay-token", text(&session, "value"))
            .json(&json!({}))
    };
    command("cancel")
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    fixture
        .until(id, |r| r["status"] == RunStatus::Cancelled)
        .await;
    // The worker records the final status event right after settling the run.
    let events = eventually(
        Duration::from_secs(5),
        Duration::from_millis(30),
        async || {
            let id = id.to_owned();
            let events = s
                .store
                .read(move |db| db.events_fixture(&id, 0, 1000))
                .await
                .unwrap();
            events
                .iter()
                .any(|event| event["type"] == "status" && event["text"] == "cancelled")
                .then_some(events)
        },
    )
    .await;
    assert!(
        events.iter().all(|event| event["type"] != "turn.failed"),
        "A stop Cairn requested does not fail the turn: {events:?}"
    );
    // "Run cancelled" says it all: the status repeats no reason.
    let cancelled = events
        .iter()
        .find(|event| event["type"] == "status" && event["text"] == "cancelled")
        .unwrap();
    assert!(cancelled["payload"].is_null(), "{cancelled}");
    eventually(
        Duration::from_secs(5),
        Duration::from_millis(30),
        async || {
            let response = command("resume").send().await.unwrap();
            if response.status() == StatusCode::CONFLICT {
                return None;
            }
            response.error_for_status().unwrap();
            Some(())
        },
    )
    .await;
    fixture
        .until(id, |r| r["status"] == RunStatus::Succeeded)
        .await;
    assert_eq!(
        fixture.checkpoint(id).await.get("remainingMs"),
        Some(&Value::Null)
    );
    fixture.stop(false).await;
}

#[tokio::test]
async fn quiet_chat_protocol_keeps_a_finite_checkpoint_deadline() {
    let mut fixture = Fixture::new().await;
    fixture.stop(false).await;
    let s = fixture.service.clone();
    let run = fixture.enqueue("fixture:hang").await;
    let id = text(&run, "id");
    let mut snapshot = run["snapshot"].clone();
    snapshot["agent"]["timeoutMinutes"] = 1.into();
    s.store
        .change_run(
            id,
            cairn_installation::runs::RunChanges {
                snapshot: Some(serde_json::from_value(snapshot).unwrap()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    // Resume the final second of an existing one-minute budget.
    common::set_checkpoint(
        &s.store,
        id,
        json!({ "launched": false, "remainingMs": 1000 }),
    )
    .await;
    fixture.start().await;
    fixture
        .until(id, |r| r["sessionId"] == "fixture-chat")
        .await;
    let failed = tokio::time::timeout(
        Duration::from_secs(8),
        fixture.until(id, |r| r["status"] == RunStatus::Failed),
    )
    .await
    .expect("a quiet chat must stop when its finite checkpoint budget expires");
    assert!(text(&failed, "summary").contains("time limit"), "{failed}");
    fixture.stop(false).await;
}

#[tokio::test]
async fn queued_work_uses_current_node_grants_without_rejecting_unrelated_policy() {
    let mut fixture = Fixture::new().await;
    fixture.stop(false).await;
    let s = &fixture.service;
    let mut agent = s.agent_snapshot(MAIN_AGENT_ID).await.unwrap();
    agent["access"]["nodes"] = Value::Null;
    s.store.put_agent_fixture(agent.clone()).await.unwrap();
    let run = fixture
        .enqueue("Inspect the fixture after a node policy change")
        .await;
    agent["access"]["nodes"] = json!([LOCAL_NODE_ID]);
    s.store.put_agent_fixture(agent).await.unwrap();
    fixture.start().await;
    let completed = fixture.until_finished(text(&run, "id")).await;
    assert_eq!(completed["status"], RunStatus::Succeeded, "{completed}");
    assert_eq!(
        completed["snapshot"]["agent"]["access"]["nodes"],
        json!([LOCAL_NODE_ID])
    );
    fixture.stop(false).await;
}

#[tokio::test]
async fn queued_work_continues_with_equivalent_or_wider_access_but_not_reduced_access() {
    let mut fixture = Fixture::new().await;
    fixture.stop(false).await;
    let s = fixture.service.clone();
    let run = fixture
        .enqueue("Inspect the fixture after an equivalent access change")
        .await;
    let id = text(&run, "id");
    let mut agent = s.agent_snapshot(MAIN_AGENT_ID).await.unwrap();
    agent["name"] = "Renamed agent".into();
    s.store.put_agent_fixture(agent.clone()).await.unwrap();
    fixture.start().await;
    let completed = fixture.until_finished(id).await;
    assert_eq!(completed["status"], RunStatus::Succeeded, "{completed}");

    fixture.stop(false).await;
    let reduced_run = fixture
        .enqueue("Inspect the fixture after a reduction")
        .await;
    agent["access"]["sandbox"] = "read-only".into();
    s.store.put_agent_fixture(agent).await.unwrap();
    fixture.start().await;
    let failed = fixture.until_finished(text(&reduced_run, "id")).await;
    assert_eq!(failed["status"], RunStatus::Failed, "{failed}");
    assert!(
        text(&failed, "error").contains("Agent access was reduced"),
        "{failed}"
    );
    fixture.stop(false).await;
}

#[tokio::test]
async fn queued_work_waits_for_node_capacity_without_failing() {
    let mut fixture = Fixture::new().await;
    fixture.stop(false).await;
    let s = fixture.service.clone();
    let node = id();
    let put_node = |name: &str| {
        json!({
            "id": node,
            "name": name,
            "accepting": true,
            "executionReady": true,
            "runtimeId": "development",

            "lastSeen": now() + 60_000,
            "capabilities": {"kvm": true },
            "limits": {"cpu": 1, "memoryMiB": 8192, "diskMiB": 65536},
            "pressure": "memory"
        })
    };
    s.store.put_node_fixture(put_node("Laptop")).await.unwrap();
    let mut agent = s
        .agent_snapshot(cairn_installation::config::MAIN_AGENT_ID)
        .await
        .unwrap();
    agent["access"]["nodes"] = json!([node]);
    s.store.put_agent_fixture(agent).await.unwrap();
    let run = fixture.enqueue("Waits for shared RAM headroom").await;
    let run_id = text(&run, "id");
    let resources = json!({"cpu": 2, "memoryMiB": 4096, "diskMiB": 32768});
    s.store
        .update_run_boundary(run_id, json!({"requestedResources": resources}))
        .await
        .unwrap();
    fixture.start().await;
    let waiting = fixture
        .until(run_id, |r| {
            text(r, "accountWaitReason").starts_with("Waiting for capacity.")
        })
        .await;
    let reason = text(&waiting, "accountWaitReason");
    assert!(reason.contains("shared headroom"), "{reason}");
    assert!(
        reason.contains("Laptop has 4 free slots; check memory"),
        "{reason}"
    );
    assert_eq!(waiting["status"], "queued");
    // New figures replace the reason without repeating the status event.
    s.store.put_node_fixture(put_node("Desk")).await.unwrap();
    let updated = fixture
        .until(run_id, |r| {
            text(r, "accountWaitReason").contains("Desk has")
        })
        .await;
    assert_eq!(updated["status"], "queued");
    let id = run_id.to_owned();
    let events = s
        .store
        .read(move |db| db.events_fixture(&id, 0, 500))
        .await
        .unwrap();
    let waits = events
        .iter()
        .filter(|e| text(e, "text").starts_with("Waiting for capacity."))
        .count();
    assert_eq!(waits, 1, "{events:?}");
    let attempts = s.store.list("node-attempts").await.unwrap();
    assert!(attempts.is_empty(), "{attempts:?}");
    fixture.stop(false).await;
}
