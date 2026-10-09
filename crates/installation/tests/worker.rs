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
use common::eventually;
use cairn_installation::{
    accounts::{self, KIND},
    config::{Config, MAIN_AGENT_ID, id, now},
    nodes::LOCAL_NODE_ID,
    provider::Provider,
    run_status::RunStatus,
    service::Service,
    validation::text,
};
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
/// through the official service and real installation relay.
struct Fixture {
    root: TempDir,
    service: Arc<Service>,
    process: Option<Child>,
    url: String,
}

impl Fixture {
    async fn new() -> Self {
        let root = TempDir::new().unwrap();
        std::fs::create_dir(root.path().join("home")).unwrap();
        let config = Config {
            codex_bin: common::fixture("codex.mjs"),
            claude_bin: common::fixture("claude.mjs"),
            concurrency: 2,
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
        self.start_backend(false).await;
    }

    /// Starts the worker process, or the former Node backend when `legacy`.
    async fn start_backend(&mut self, legacy: bool) {
        let file = self.root.path().join("config.json");
        tokio::fs::write(&file, serde_json::to_vec(&self.service.config).unwrap())
            .await
            .unwrap();
        let mut command = if legacy {
            let mut command = Command::new("node");
            command
                .args(["--import", "tsx"])
                .arg(common::fixture_path("legacy-worker.mjs"));
            command
        } else {
            let binary = std::path::Path::new(env!("CARGO_BIN_EXE_leo"))
                .parent()
                .unwrap()
                .join("examples/worker_fixture");
            let mut command = Command::new(binary);
            command.arg("serve");
            command
        };
        let usage = self.root.path().join("usage.json");
        if usage.exists() {
            command.env("CAIRN_FIXTURE_USAGE", usage);
        }
        let mut child = command
            .env("CAIRN_CONFIG", file)
            .env_remove("CAIRN_TOOLKIT_DIR")
            .env("NODE_ENV", "test")
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
        let task = self.service.task(task, None).await.unwrap();
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
            .kv(&format!("run-checkpoint:{run}"))
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
    let prompt = "fixture:agent-failures:[{\"message\":\"workspace routing discovery timed out\"}]";
    let chat_id = fixture.start_chat(message(prompt)).await;
    let run_id = fixture.chat_run(&chat_id).await;
    let waiting = fixture
        .until(&run_id, |run| run["retry"].is_object() || finished(run))
        .await;
    assert_eq!(waiting["status"], RunStatus::Queued, "{waiting}");
    assert_eq!(waiting["retry"]["attempt"], 1);
    assert_eq!(waiting["retry"]["limit"], 3);
    assert!(waiting["retry"]["nextAttemptAt"].as_i64().unwrap() > now());
    assert_eq!(waiting["sessionId"], "fixture-chat");
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
        .patch_run(&run_id, json!({ "retry": retry }))
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
async fn agent_retries_stop_after_three_attempts_or_when_the_error_becomes_terminal() {
    for terminal in [false, true] {
        let mut fixture = Fixture::new().await;
        let transient = json!({ "code": "server_error", "message": "Temporary upstream error" });
        let failures = if terminal {
            vec![
                transient,
                json!({ "code": "authentication_error", "message": "Reconnect account" }),
            ]
        } else {
            vec![transient; 4]
        };
        let prompt = format!("fixture:agent-failures:{}", json!(failures));
        let chat = fixture.start_chat(message(&prompt)).await;
        let run_id = fixture.chat_run(&chat).await;
        let attempts = if terminal { 1 } else { 3 };
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
                .patch_run(&run_id, json!({ "retry": retry }))
                .await
                .unwrap();
        }
        let failed = fixture.until_finished(&run_id).await;
        assert_eq!(failed["status"], RunStatus::Failed);
        assert_eq!(failed["retry"]["attempt"], attempts);
        let expected = if terminal {
            "Reconnect account"
        } else {
            "Temporary upstream error"
        };
        assert_eq!(failed["summary"], expected);
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
    let mut account = s
        .accounts
        .create(&s, Provider::Codex, "Retry fixture")
        .await
        .unwrap();
    account["state"] = "ready".into();
    account["maxConcurrentRuns"] = 1.into();
    s.store.put(KIND, account.clone()).await.unwrap();
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
        .patch_run(run_id, json!({ "retry": retry }))
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
        .patch_run(run_id, json!({ "retry": retry }))
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
    let mut account = s
        .accounts
        .create(s, Provider::Claude, "Claude fixture")
        .await
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
    s.store.put(KIND, account).await.unwrap();
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
                    db.events(&run_id, 0, 500)?,
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
    // Started by an older version that saved a setting the policy no longer has.
    let mut snapshot = s.store.run(&run_id).await.unwrap()["snapshot"].clone();
    snapshot["agent"]["access"]["maxResources"] = Value::Null;
    s.store
        .patch_run(&run_id, json!({ "snapshot": snapshot }))
        .await
        .unwrap();
    let follow_up = message("Continue with the same access.");
    s.chat_send(&chat_id, follow_up.clone()).await.unwrap();
    fixture
        .until(&run_id, |r| {
            r["status"] == RunStatus::Succeeded
                && r["chatExecution"]["messageId"] == follow_up["id"]
        })
        .await;

    let mut agent = s.get("agents", MAIN_AGENT_ID).await.unwrap();
    agent["access"]["sandbox"] = "read-only".into();
    s.store.put("agents", agent).await.unwrap();
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
async fn migration_resumes_a_checkpoint_written_by_the_node_backend() {
    let mut fixture = Fixture::new().await;
    fixture.stop(false).await;

    // Seed an actual version-4 fixture before asking the historical backend to
    // create its checkpoint. A version-5 database must reject that executable.
    fixture
        .service
        .store
        .transaction(|db| {
            let objects: i64 =
                db.0.query_row("SELECT count(*) FROM shared_objects", [], |row| row.get(0))?;
            assert_eq!(objects, 0);
            db.0.execute_batch(
                "DROP TABLE shared_references;
                 DROP TABLE shared_publications;
                 DROP TABLE shared_objects;
                 DROP TABLE remote_deletions;
                 PRAGMA user_version=4;",
            )?;
            Ok(())
        })
        .await
        .unwrap();

    fixture.start_backend(true).await;
    let run = fixture.enqueue("fixture:restart").await;
    let id = text(&run, "id");
    let running = fixture
        .until(id, |r| r["sessionId"] == "fixture-session")
        .await;
    fixture.stop(true).await;
    fixture.start().await;
    let completed = fixture.until_finished(id).await;
    assert_eq!(completed["status"], RunStatus::Succeeded, "{completed}");
    assert_eq!(completed["workspace"], running["workspace"]);
    assert_eq!(completed["sessionId"], running["sessionId"]);
    assert_eq!(completed["resumeCount"], 1);
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
        let mut account = s.accounts.create(s, Provider::Codex, name).await.unwrap();
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
        s.store.put(KIND, account).await.unwrap();
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
async fn native_worker_records_artifacts_and_completes_task() {
    let mut fixture = Fixture::new().await;
    let run = fixture.enqueue("Inspect the fixture").await;
    let id = text(&run, "id").to_owned();
    let complete = fixture.until_finished(&id).await;
    assert_eq!(complete["status"], RunStatus::Succeeded, "{complete}");
    assert_eq!(complete["sessionId"], "fixture-session");
    let events = fixture
        .service
        .store
        .read(move |db| db.events(&id, 0, 500))
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
    let mut account = s
        .accounts
        .create(&s, Provider::Codex, "Shared")
        .await
        .unwrap();
    account["state"] = "ready".into();
    s.store.put(KIND, account.clone()).await.unwrap();
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
        .until(id, |r| r["sessionId"] == "fixture-session")
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

const PNG: &[u8] = b"\x89PNG\r\n\x1a\nfixture";

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
            "sharedResources": true,
            "budget": { "slots": state.slots, "limits": { "cpu": 7, "memoryMiB": 15872, "diskMiB": 104857 } },
            "runtimeId": "fixture",
            "runtimes": ["fixture"],
            "capabilities": {
                "os": "linux",
                "arch": "x86_64",
                "kvm": true,
                "fuse": true,
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
        .get("nodes", LOCAL_NODE_ID)
        .await
        .unwrap()
        .unwrap();
    node["slots"] = 12.into();
    fixture.service.store.put("nodes", node).await.unwrap();
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
    assert_eq!(queued[0]["recoveryPending"], Value::Null);
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
        .get("nodes", LOCAL_NODE_ID)
        .await
        .unwrap()
        .unwrap();
    node["limits"]["memoryMiB"] = 256.into();
    node["lastSeen"] = 0.into();
    fixture.service.store.put("nodes", node).await.unwrap();
    cairn_installation::nodes::refresh_local(&fixture.service)
        .await
        .unwrap();
    let node = fixture
        .service
        .store
        .get("nodes", LOCAL_NODE_ID)
        .await
        .unwrap()
        .unwrap();
    assert!(node["lastSeen"].as_i64().unwrap() > now() - 1000);
    assert_eq!(node["executionReady"], false);
    assert_eq!(node["capabilities"]["cpu"], 8);
    assert!(text(&node, "budgetError").contains("below current usage"));
    let mut restored = node;
    restored["limits"]["memoryMiB"] = 15872.into();
    fixture.service.store.put("nodes", restored).await.unwrap();
    cairn_installation::nodes::refresh_local(&fixture.service)
        .await
        .unwrap();
    let node = fixture
        .service
        .store
        .get("nodes", LOCAL_NODE_ID)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(node["executionReady"], true);
    assert_eq!(node["budgetError"], Value::Null);
    server.abort();
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
                .read(move |db| db.events(&id, 0, 500))
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
            assert!(
                plan["args"]
                    .as_array()
                    .unwrap()
                    .contains(&json!("fixture-session")),
                "{plan}"
            );
            assert_eq!(plan["runId"], run_id);
            assert_eq!(plan["cwd"], plans[0]["cwd"]);
        }
        drop(plans);
        fixture.stop(false).await;
        server.abort();
    }
}

#[tokio::test]
async fn claude_conversations_run_together_and_lowering_limit_does_not_cancel_them() {
    let mut fixture = Fixture::new().await;
    let s = fixture.service.clone();
    let account = claude_account(&s, 2).await;
    let mut chats = Vec::new();
    let mut runs = Vec::new();
    for prompt in [
        "fixture:question",
        "fixture:question",
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
    // Both real fixture subprocesses have received their prompts and are waiting on separate questions.
    eventually(
        Duration::from_secs(10),
        Duration::from_millis(30),
        async || {
            for chat in &chats[..2] {
                if s.chat_detail(chat).await.unwrap()["pendingQuestions"] != 1 {
                    return None;
                }
            }
            Some(())
        },
    )
    .await;
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
    s.accounts
        .update(&s, &account, &json!({ "maxConcurrentRuns": 1 }))
        .await
        .unwrap();
    for run in &runs[..2] {
        assert_eq!(
            s.store.run(run).await.unwrap()["status"],
            RunStatus::Running
        );
    }
    let answer = async |chat: &str| {
        let question = s.chat_detail(chat).await.unwrap()["questions"][0]["id"]
            .as_str()
            .unwrap()
            .to_owned();
        s.question_answer(
            chat,
            &question,
            json!({ "id": id(), "answers": { "0": ["Small change"] } }),
        )
        .await
        .unwrap();
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
async fn unlimited_runs_can_be_cancelled_and_finite_checkpoints_still_expire() {
    let mut fixture = Fixture::new().await;
    let s = fixture.service.clone();
    let run = fixture.enqueue("fixture:restart").await;
    let id = text(&run, "id");
    assert_eq!(run["snapshot"]["agent"]["timeoutMinutes"], 0);
    fixture
        .until(id, |r| r["sessionId"] == "fixture-session")
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
    let run = fixture.enqueue("fixture:hang").await;
    let id = text(&run, "id");
    let mut snapshot = run["snapshot"].clone();
    snapshot["agent"]["timeoutMinutes"] = 1.into();
    s.store
        .patch_run(id, json!({ "snapshot": snapshot }))
        .await
        .unwrap();
    // Resume the final five seconds of an existing one-minute budget.
    common::set_checkpoint(
        &s.store,
        id,
        json!({ "launched": false, "remainingMs": 5000 }),
    )
    .await;
    fixture.start().await;
    fixture
        .until(id, |r| r["sessionId"] == "fixture-session")
        .await;
    let failed = fixture
        .until(id, |r| r["status"] == RunStatus::Failed)
        .await;
    assert!(text(&failed, "summary").contains("time limit"), "{failed}");
    fixture.stop(false).await;
}

#[tokio::test]
async fn queued_work_uses_current_node_grants_without_rejecting_unrelated_policy() {
    let mut fixture = Fixture::new().await;
    fixture.stop(false).await;
    let s = &fixture.service;
    let mut agent = s.get("agents", MAIN_AGENT_ID).await.unwrap();
    agent["access"]["nodes"] = Value::Null;
    s.store.put("agents", agent.clone()).await.unwrap();
    let run = fixture
        .enqueue("Inspect the fixture after a node policy change")
        .await;
    agent["access"]["nodes"] = json!([LOCAL_NODE_ID]);
    s.store.put("agents", agent).await.unwrap();
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
    // Queued by an older version that saved a setting the policy no longer has.
    let mut snapshot = s.store.run(id).await.unwrap()["snapshot"].clone();
    snapshot["agent"]["access"]["maxResources"] = Value::Null;
    s.store
        .patch_run(id, json!({ "snapshot": snapshot }))
        .await
        .unwrap();
    let mut agent = s.get("agents", MAIN_AGENT_ID).await.unwrap();
    agent["name"] = "Renamed agent".into();
    s.store.put("agents", agent.clone()).await.unwrap();
    fixture.start().await;
    let completed = fixture.until_finished(id).await;
    assert_eq!(completed["status"], RunStatus::Succeeded, "{completed}");

    fixture.stop(false).await;
    let reduced_run = fixture
        .enqueue("Inspect the fixture after a reduction")
        .await;
    agent["access"]["sandbox"] = "read-only".into();
    s.store.put("agents", agent).await.unwrap();
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
            "lastSeen": now() + 60_000,
            "capabilities": {"kvm": true, "fuse": true},
            "limits": {"cpu": 1, "memoryMiB": 8192, "diskMiB": 65536},
            "pressure": "memory"
        })
    };
    s.store.put("nodes", put_node("Laptop")).await.unwrap();
    let mut agent = s
        .get("agents", cairn_installation::config::MAIN_AGENT_ID)
        .await
        .unwrap();
    agent["access"]["nodes"] = json!([node]);
    s.store.put("agents", agent).await.unwrap();
    let run = fixture.enqueue("Waits for shared RAM headroom").await;
    let run_id = text(&run, "id");
    let resources = json!({"cpu": 2, "memoryMiB": 4096, "diskMiB": 32768});
    s.store
        .patch_run(run_id, json!({"requestedResources": resources}))
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
    s.store.put("nodes", put_node("Desk")).await.unwrap();
    let updated = fixture
        .until(run_id, |r| {
            text(r, "accountWaitReason").contains("Desk has")
        })
        .await;
    assert_eq!(updated["status"], "queued");
    let id = run_id.to_owned();
    let events = s
        .store
        .read(move |db| db.events(&id, 0, 500))
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
