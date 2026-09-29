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
use leo_agent_manager::{
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

/// A service whose worker runs in a separate `leo serve` process.
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
            setup_token: String::new(),
            codex_bin: common::fixture("codex.mjs"),
            claude_bin: common::fixture("claude.mjs"),
            concurrency: 2,
            worker_enabled: true,
            ..common::config(root.path())
        };
        let service = Service::new(config).await.unwrap();
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
            let mut command = Command::new(env!("CARGO_BIN_EXE_leo"));
            command.arg("serve");
            command
        };
        let usage = self.root.path().join("usage.json");
        if usage.exists() {
            command.env("LEO_FIXTURE_USAGE", usage);
        }
        let mut child = command
            .env("LEO_CONFIG", file)
            .env_remove("LEO_TOOLKIT_DIR")
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
        assert!(
            events
                .iter()
                .filter(|e| e["payload"]["item"]["type"] == "command_execution")
                .count()
                < 60,
            "Verbose tool history must still be bounded"
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
    if path.ends_with("/lease") {
        return Json(json!({})).into_response();
    }
    if path.ends_with("/snapshot") {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    if request.method() == "DELETE" {
        return Json(json!({})).into_response();
    }
    let attempt = path.split('/').nth(2).unwrap();
    if path.ends_with("/logs") {
        let mut output =
            String::from("{\"type\":\"thread.started\",\"thread_id\":\"fixture-session\"}\n");
        if state.attempt_index(attempt).await >= state.failures {
            output.push_str("{\"type\":\"item.completed\",\"item\":{\"id\":\"reply\",\"type\":\"agent_message\",\"text\":\"resumed VM\"}}\n{\"type\":\"turn.completed\",\"usage\":{}}\n");
        }
        let frame = json!({ "type": "output", "stderr": false, "data": STANDARD.encode(output) });
        return Body::from(format!("{frame}\n")).into_response();
    }
    if path.ends_with("/wait") {
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
async fn controller_interruptions_resume_saved_threads_and_stop_after_three_recoveries() {
    for failures in [1, usize::MAX] {
        let mut fixture = Fixture::new().await;
        fixture.stop(false).await;
        let controller = Arc::new(Controller {
            data: fixture.service.config.data_dir.clone(),
            plans: tokio::sync::Mutex::new(Vec::new()),
            failures,
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
    let session = s.auth.session().await.unwrap();
    let client = reqwest::Client::new();
    let command = |action: &str| {
        client
            .post(format!("{}/api/runs/{id}/{action}", fixture.url))
            .header("cookie", format!("leo_session={}", text(&session, "value")))
            .header("x-csrf-token", text(&session, "csrf"))
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
