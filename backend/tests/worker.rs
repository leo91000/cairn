use leo_agent_manager::{
    accounts::{self, KIND},
    config::{Config, id, now},
    provider::Provider,
    service::Service,
    validation::text,
};
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use tempfile::TempDir;
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::{Child, Command},
};
struct Fixture {
    root: TempDir,
    service: Arc<Service>,
    process: Option<Child>,
    url: String,
}
/// A signed-in Claude Code account whose CLI home holds fixture credentials.
async fn claude_account(s: &Service, parallel_runs: u64) -> String {
    let account = s
        .accounts
        .create(s, Provider::Claude, "Claude fixture")
        .await
        .unwrap();
    let id = text(&account, "id").to_owned();
    let home = accounts::claude::account_home(&s.config, &id);
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(home.join(".credentials.json"), json!({"claudeAiOauth":{"accessToken":"fixture-access","refreshToken":"private-refresh","expiresAt":now()+3600000,"scopes":["user:inference"]}}).to_string()).unwrap();
    let mut account = account;
    account["state"] = "ready".into();
    account["maxConcurrentRuns"] = parallel_runs.into();
    s.store.put(KIND, account).await.unwrap();
    id
}
fn run_claude_home(s: &Service, run: &str) -> std::path::PathBuf {
    s.config
        .data_dir
        .join("runs")
        .join(run)
        .join("home/.claude")
}

#[tokio::test]
async fn verbose_tools_do_not_hide_chat_answers_or_failures() {
    for fail in [false, true] {
        let mut fixture = Fixture::new().await;
        let s = &fixture.service;
        let chat = s.chat_create(json!({})).await.unwrap();
        let chat_id = text(&chat, "id");
        let prompt = if fail {
            "fixture:verbose-tools-fail"
        } else {
            "fixture:verbose-tools"
        };
        s.chat_send(chat_id, json!({"id":id(),"text":prompt}))
            .await
            .unwrap();
        let run_id = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if let Some(id) = s.chat_detail(chat_id).await.unwrap()["runId"].as_str() {
                    break id.to_owned();
                }
                tokio::time::sleep(Duration::from_millis(30)).await;
            }
        })
        .await
        .unwrap();
        let run = fixture
            .until(&run_id, |r| {
                r["status"] == "succeeded" || r["status"] == "failed"
            })
            .await;
        assert_eq!(run["status"], if fail { "failed" } else { "succeeded" });
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
                tail.as_array()
                    .unwrap()
                    .iter()
                    .any(|event| event["payload"]["item"]["type"] == "agent_message"
                        && text(&event["payload"]["item"], "text")
                            .contains("Ready for the next step.")),
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
    let chat = s.chat_create(json!({})).await.unwrap();
    let chat_id = text(&chat, "id");
    s.chat_send(
        chat_id,
        json!({"id":id(),"text":"Keep the existing design and inspect the workspace."}),
    )
    .await
    .unwrap();
    let run_id = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let detail = s.chat_detail(chat_id).await.unwrap();
            if let Some(id) = detail["runId"].as_str() {
                break id.to_owned();
            }
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
    })
    .await
    .unwrap();
    let first = fixture.until(&run_id, |r| r["status"] == "succeeded").await;
    let marker = std::path::Path::new(text(&first, "workspace")).join("preserved.txt");
    std::fs::write(&marker, "completed work").unwrap();
    let message =
        json!({"id":id(),"text":"Continue with Claude.","provider":"claude","model":"opus[1m]"});
    s.chat_send(chat_id, message.clone()).await.unwrap();
    let second = fixture
        .until(&run_id, |r| {
            r["status"] == "succeeded" && r["chatExecution"]["messageId"] == message["id"]
        })
        .await;
    assert_eq!(second["snapshot"]["agent"]["provider"], "claude");
    // The native session belongs to the run, not to the account.
    let claude_home = run_claude_home(s, &run_id);
    assert_eq!(second["workspace"], first["workspace"]);
    assert_eq!(std::fs::read_to_string(&marker).unwrap(), "completed work");
    let messages = std::fs::read_to_string(claude_home.join("user-messages.jsonl")).unwrap();
    assert!(messages.contains("Keep the existing design"));
    assert!(messages.contains("The approach looks good"));
    assert!(messages.contains("Continue with Claude"));
    let invocations = std::fs::read_to_string(claude_home.join("invocations.jsonl")).unwrap();
    assert!(!invocations.contains("--resume"));
    // Retry the same delivery after the provider changed: it stays one message.
    s.chat_send(chat_id, message.clone()).await.unwrap();
    assert_eq!(
        s.chat_detail(chat_id).await.unwrap()["messages"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    let continuation = json!({"id":id(),"text":"Keep using Claude."});
    s.chat_send(chat_id, continuation.clone()).await.unwrap();
    fixture
        .until(&run_id, |r| {
            r["status"] == "succeeded" && r["chatExecution"]["messageId"] == continuation["id"]
        })
        .await;
    assert!(
        std::fs::read_to_string(claude_home.join("invocations.jsonl"))
            .unwrap()
            .contains("--resume")
    );
    let back =
        json!({"id":id(),"text":"Return to Codex and preserve the decisions.","provider":"codex"});
    s.chat_send(chat_id, back.clone()).await.unwrap();
    let last = fixture
        .until(&run_id, |r| {
            r["status"] == "succeeded" && r["chatExecution"]["messageId"] == back["id"]
        })
        .await;
    assert_eq!(last["snapshot"]["agent"]["provider"], "codex");
    assert_eq!(last["workspace"], first["workspace"]);
    assert!(text(&last, "summary").contains("Claude fixture completed"));
    assert!(text(&last, "summary").contains("Keep the existing design"));
    assert_eq!(std::fs::read_to_string(&marker).unwrap(), "completed work");
    assert_eq!(s.chat_detail(chat_id).await.unwrap()["runId"], run_id);
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
    let chat = s.chat_create(json!({})).await.unwrap();
    let chat_id = text(&chat, "id");
    let request = "$review the workspace and keep $HOME untouched.";
    s.chat_send(chat_id, json!({"id":id(),"text":request}))
        .await
        .unwrap();
    let run_id = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(id) = s.chat_detail(chat_id).await.unwrap()["runId"].as_str() {
                break id.to_owned();
            }
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
    })
    .await
    .unwrap();
    let run = fixture.until(&run_id, |r| r["status"] == "succeeded").await;
    let execution = text(&run["chatExecution"], "text");
    assert!(execution.starts_with(request), "{execution}");
    assert!(execution.contains("<invoked_skills>"), "{execution}");
    assert!(execution.contains("\n- review\n"), "{execution}");
    assert!(!execution.contains("- HOME"), "{execution}");
    assert_eq!(
        s.chat_detail(chat_id).await.unwrap()["messages"][0]["text"],
        request
    );
    fixture.stop(false).await;
}
impl Fixture {
    async fn new() -> Self {
        let root = TempDir::new().unwrap();
        std::fs::create_dir(root.path().join("home")).unwrap();
        let config = Config {
            data_dir: root.path().join("data"),
            home: root.path().join("home"),
            workspace_roots: vec![root.path().to_owned()],
            public_url: "http://localhost:4310".into(),
            host: "127.0.0.1".into(),
            port: 0,
            setup_token: String::new(),
            codex_bin: std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../tests/fixtures/codex.mjs")
                .to_string_lossy()
                .into_owned(),
            claude_bin: std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../tests/fixtures/claude.mjs")
                .to_string_lossy()
                .into_owned(),
            gh_bin: "gh".into(),
            concurrency: 2,
            logger: false,
            worker_enabled: true,
            runner_url: String::new(),
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
    async fn start_backend(&mut self, legacy: bool) {
        let file = self.root.path().join("config.json");
        tokio::fs::write(&file, serde_json::to_vec(&self.service.config).unwrap())
            .await
            .unwrap();
        let mut command = if legacy {
            let mut command = Command::new("node");
            command.args(["--import", "tsx"]);
            command.arg(
                std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../tests/fixtures/legacy-worker.mjs"),
            );
            command
        } else {
            let mut command = Command::new(env!("CARGO_BIN_EXE_leo"));
            command.arg("serve");
            command
        };
        if self.root.path().join("usage.json").exists() {
            command.env("LEO_FIXTURE_USAGE", self.root.path().join("usage.json"));
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
    async fn stop(&mut self, abrupt: bool) {
        if let Some(mut process) = self.process.take() {
            if abrupt {
                process.kill().await.unwrap();
            } else {
                unsafe {
                    libc::kill(process.id().unwrap() as i32, libc::SIGTERM);
                }
                tokio::time::timeout(Duration::from_secs(15), process.wait())
                    .await
                    .unwrap()
                    .unwrap();
            }
        }
    }
    async fn enqueue(&self, prompt: &str) -> Value {
        let task = self.service.task(json!({"name":"Fixture run","prompt":prompt,"worktree":false,"agentId":leo_agent_manager::config::MAIN_AGENT_ID}), None).await.unwrap();
        self.service
            .enqueue(text(&task, "id"), "manual", None)
            .await
            .unwrap()
    }
    async fn until(&self, id: &str, condition: impl Fn(&Value) -> bool) -> Value {
        tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                let run = self.service.store.run(id).await.unwrap();
                if condition(&run) {
                    return run;
                }
                tokio::time::sleep(Duration::from_millis(30)).await;
            }
        })
        .await
        .unwrap()
    }
}

#[tokio::test]
async fn migration_resumes_a_checkpoint_written_by_the_node_backend() {
    let mut fixture = Fixture::new().await;
    fixture.stop(false).await;
    fixture.start_backend(true).await;
    let run = fixture.enqueue("fixture:restart").await;
    let id = text(&run, "id");
    let running = fixture
        .until(id, |r| r["sessionId"] == "fixture-session")
        .await;
    fixture.stop(true).await;
    fixture.start().await;
    let completed = fixture
        .until(id, |r| ["succeeded", "failed"].contains(&text(r, "status")))
        .await;
    assert_eq!(completed["status"], "succeeded", "{completed}");
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
        let limits = json!({"ordinaryUsageAllowed":true,"rateLimits":{"limitId":"codex","primary":{"usedPercent":used,"windowDurationMins":300,"resetsAt":now()/1000+7200}}});
        account["state"] = "ready".into();
        account["usage"] = accounts::codex::normalize(&limits);
        s.store.put(KIND, account).await.unwrap();
        s.vault.set(&format!("codex-account:{id}"), &json!({"tokens":{"account_id":id,"access_token":"synthetic-access","refresh_token":"synthetic-refresh"}})).await.unwrap();
        usage[&id] = limits;
        accounts.push(id);
    }
    tokio::fs::write(fixture.root.path().join("usage.json"), usage.to_string())
        .await
        .unwrap();
    fixture.start().await;
    let run = fixture.enqueue("fixture:exhaust").await;
    let id = text(&run, "id");
    let completed = fixture
        .until(id, |r| ["succeeded", "failed"].contains(&text(r, "status")))
        .await;
    assert_eq!(completed["status"], "succeeded", "{completed}");
    assert_eq!(completed["accountId"], accounts[1]);
    assert_eq!(completed["accountName"], "Backup");
    assert_eq!(completed["sessionId"], "fixture-chat");
    fixture.stop(false).await;
}
#[tokio::test]
async fn native_worker_records_artifacts_and_completes_task() {
    let mut fixture = Fixture::new().await;
    let run = fixture.enqueue("Inspect the fixture").await;
    let id = text(&run, "id");
    let complete = fixture
        .until(id, |r| {
            r["status"] == "succeeded" || r["status"] == "failed"
        })
        .await;
    assert_eq!(complete["status"], "succeeded", "{complete}");
    assert_eq!(complete["sessionId"], "fixture-session");
    let id = id.to_owned();
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
    let mut account = fixture
        .service
        .accounts
        .create(&fixture.service, Provider::Codex, "Shared")
        .await
        .unwrap();
    account["state"] = "ready".into();
    fixture
        .service
        .store
        .put(KIND, account.clone())
        .await
        .unwrap();
    let account_id = text(&account, "id");
    fixture.service.vault.set(&format!("codex-account:{account_id}"), &json!({"tokens":{"access_token":"synthetic","refresh_token":"secret-refresh","account_id":"shared"}})).await.unwrap();
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
                .service
                .config
                .data_dir
                .join("runs")
                .join(text(run, "id"))
                .join("codex/auth.json")
                .exists()
        );
    }
    fixture.stop(true).await;
    fixture.start().await;
    for run in [&first, &second] {
        let completed = fixture
            .until(text(run, "id"), |r| {
                ["succeeded", "failed"].contains(&text(r, "status"))
            })
            .await;
        assert_eq!(completed["status"], "succeeded", "{completed}");
        assert_eq!(completed["sessionId"], "fixture-chat");
        assert_eq!(completed["accountId"], account_id);
    }
    assert_eq!(
        fixture
            .service
            .vault
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
    let completed = fixture
        .until(id, |r| {
            r["status"] == "succeeded" || r["status"] == "failed"
        })
        .await;
    assert_eq!(completed["status"], "succeeded", "{completed}");
    assert_eq!(completed["workspace"], running["workspace"]);
    assert_eq!(completed["sessionId"], running["sessionId"]);
    assert_eq!(completed["resumeCount"], 1);
    assert!(text(&completed, "summary").contains("saved conversation"));
    assert_eq!(completed["snapshot"]["agent"]["timeoutMinutes"], 0);
    assert_eq!(
        fixture
            .service
            .store
            .kv(&format!("run-checkpoint:{id}"))
            .await
            .unwrap()
            .unwrap()
            .get("remainingMs"),
        Some(&Value::Null)
    );
    fixture.stop(false).await;
}
#[tokio::test]
async fn native_chat_turns_reuse_the_same_run_and_conversation() {
    let mut fixture = Fixture::new().await;
    let chat = fixture.service.chat_create(json!({})).await.unwrap();
    let chat_id = text(&chat, "id");
    fixture
        .service
        .chat_send(chat_id, json!({"id":id(),"text":"First message"}))
        .await
        .unwrap();
    let run_id = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let chat = fixture.service.chat_detail(chat_id).await.unwrap();
            if let Some(id) = chat["runId"].as_str() {
                break id.to_owned();
            }
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
    })
    .await
    .unwrap();
    let first = fixture
        .until(&run_id, |r| {
            ["succeeded", "failed"].contains(&text(r, "status"))
        })
        .await;
    assert_eq!(first["status"], "succeeded", "{first}");
    fixture
        .service
        .chat_send(chat_id, json!({"id":id(),"text":"Second message"}))
        .await
        .unwrap();
    let second = fixture
        .until(&run_id, |r| {
            r["status"] == "succeeded" && text(r, "summary").contains("Second message")
        })
        .await;
    assert_eq!(second["sessionId"], "fixture-chat");
    assert_eq!(
        fixture.service.chat_detail(chat_id).await.unwrap()["runId"],
        run_id
    );
    fixture
        .service
        .chat_send(chat_id, json!({"id":id(),"text":"fixture:disconnect"}))
        .await
        .unwrap();
    let failed = fixture.until(&run_id, |r| r["status"] == "failed").await;
    assert!(
        !text(&failed, "summary").contains("Second message"),
        "A failed attempt reused the previous result: {failed}"
    );
    assert!(text(&failed, "error").contains("Codex"), "{failed}");
    fixture.stop(false).await;
}

#[tokio::test]
async fn chat_attachments_survive_worker_restart_and_reach_codex() {
    use axum::{body::Body, http::Request};
    let mut fixture = Fixture::new().await;
    let chat = fixture.service.chat_create(json!({})).await.unwrap();
    let chat_id = text(&chat, "id");
    let attachment_id = id();
    let request = Request::builder()
        .method("PUT")
        .uri("/?name=design.png")
        .body(Body::from(b"\x89PNG\r\n\x1a\nfixture".to_vec()))
        .unwrap();
    fixture
        .service
        .attachment_http(chat_id, &attachment_id, request)
        .await
        .unwrap();
    fixture
        .service
        .chat_send(
            chat_id,
            json!({"id":id(),"text":"fixture:chat-hang","attachmentIds":[attachment_id]}),
        )
        .await
        .unwrap();
    let run_id = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let chat = fixture.service.chat_detail(chat_id).await.unwrap();
            if let Some(id) = chat["runId"].as_str() {
                break id.to_owned();
            }
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
    })
    .await
    .unwrap();
    fixture
        .until(&run_id, |r| r["sessionId"] == "fixture-chat")
        .await;
    // Wait for the original input receipt, so recovery resumes the accepted turn.
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if fixture.service.chat_detail(chat_id).await.unwrap()["messages"][0]["status"]
                == "delivered"
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
    })
    .await
    .unwrap();
    fixture.stop(true).await;
    fixture.start().await;
    let completed = fixture
        .until(&run_id, |r| {
            ["succeeded", "failed"].contains(&text(r, "status"))
        })
        .await;
    assert_eq!(completed["status"], "succeeded", "{completed}");
    assert_eq!(completed["resumeCount"], 1);
    let path = fixture.service.config.data_dir.join("runs").join(&run_id);
    let thread: Value = serde_json::from_slice(
        &std::fs::read(path.join("codex/fixture-conversation.json")).unwrap(),
    )
    .unwrap();
    let turns = thread["turns"].as_array().unwrap();
    assert_eq!(turns.len(), 2);
    for turn in turns {
        let user = turn["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["type"] == "userMessage")
            .unwrap();
        let image = user["content"]
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["type"] == "localImage")
            .unwrap();
        assert_eq!(
            std::fs::read(text(image, "path")).unwrap(),
            b"\x89PNG\r\n\x1a\nfixture"
        );
    }
    fixture.stop(false).await;
}

#[tokio::test]
async fn controller_interruptions_resume_saved_threads_and_stop_after_three_recoveries() {
    use axum::{
        Json, Router,
        body::Body,
        extract::{Request, State},
        response::IntoResponse,
        routing::any,
    };
    use base64::{Engine, engine::general_purpose::STANDARD};
    struct Controller {
        data: std::path::PathBuf,
        plans: tokio::sync::Mutex<Vec<Value>>,
        failures: usize,
    }
    for failures in [1, usize::MAX] {
        let mut fixture = Fixture::new().await;
        fixture.stop(false).await;
        let controller = Arc::new(Controller {
            data: fixture.service.config.data_dir.clone(),
            plans: tokio::sync::Mutex::new(Vec::new()),
            failures,
        });
        let app = Router::new().fallback(any(|State(state): State<Arc<Controller>>, request: Request| async move {
            let path = request.uri().path();
            if path == "/health" { return Json(json!({"status":"ok","runtimeId":"fixture","runtimes":["fixture"],"capabilities":{"os":"linux","arch":"x86_64","kvm":true,"cpu":8,"memoryMiB":16384,"diskMiB":131072}})).into_response(); }
            if path.ends_with("/lease") { return Json(json!({})).into_response(); }
            if path.ends_with("/snapshot") { return axum::http::StatusCode::SERVICE_UNAVAILABLE.into_response(); }
            if request.method() == "DELETE" { return Json(json!({})).into_response(); }
            let attempt = path.split('/').nth(2).unwrap();
            if path.ends_with("/logs") {
                let plans = state.plans.lock().await;
                let index = plans.iter().position(|plan| plan["id"] == attempt).unwrap();
                let mut output = String::from("{\"type\":\"thread.started\",\"thread_id\":\"fixture-session\"}\n");
                if index >= state.failures {
                    output.push_str("{\"type\":\"item.completed\",\"item\":{\"id\":\"reply\",\"type\":\"agent_message\",\"text\":\"resumed VM\"}}\n{\"type\":\"turn.completed\",\"usage\":{}}\n");
                }
                return Body::from(format!("{}\n",json!({"type":"output","stderr":false,"data":STANDARD.encode(output)}))).into_response();
            }
            if path.ends_with("/wait") {
                let plans = state.plans.lock().await;
                let index = plans.iter().position(|plan| plan["id"] == attempt).unwrap();
                return Json(json!({"StatusCode":if index < state.failures {143} else {0}})).into_response();
            }
            let bytes = tokio::fs::read(state.data.join("runner-plans").join(format!("{attempt}.json"))).await.unwrap();
            state.plans.lock().await.push(serde_json::from_slice(&bytes).unwrap());
            Json(json!({})).into_response()
        })).with_state(controller.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        Arc::get_mut(&mut fixture.service)
            .unwrap()
            .config
            .runner_url = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let home = fixture.service.config.home.join(".codex");
        tokio::fs::create_dir_all(&home).await.unwrap();
        tokio::fs::write(home.join("auth.json"), "{}")
            .await
            .unwrap();
        fixture.start().await;
        let run = fixture.enqueue("Inspect the VM fixture").await;
        let run_id = text(&run, "id");
        let completed = fixture
            .until(run_id, |run| {
                ["succeeded", "failed"].contains(&text(run, "status"))
            })
            .await;
        assert_eq!(
            completed["status"],
            if failures == 1 { "succeeded" } else { "failed" },
            "{completed}"
        );
        assert_eq!(completed["sessionId"], "fixture-session");
        let plans = controller.plans.lock().await;
        assert_eq!(plans.len(), if failures == 1 { 2 } else { 4 });
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
    let s = &fixture.service;
    let account = claude_account(s, 2).await;
    let mut chats = Vec::new();
    let mut runs = Vec::new();
    for prompt in [
        "fixture:question",
        "fixture:question",
        "Complete third conversation",
    ] {
        let chat = s.chat_create(json!({})).await.unwrap();
        let chat_id = text(&chat, "id").to_owned();
        s.chat_send(
            &chat_id,
            json!({"id":id(),"text":prompt,"provider":"claude","model":"sonnet"}),
        )
        .await
        .unwrap();
        let run_id = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let c = s.chat_detail(&chat_id).await.unwrap();
                if let Some(id) = c["runId"].as_str() {
                    break id.to_owned();
                }
                tokio::time::sleep(Duration::from_millis(30)).await;
            }
        })
        .await
        .unwrap();
        chats.push(chat_id);
        runs.push(run_id);
    }
    for run in &runs[..2] {
        fixture
            .until(run, |r| {
                r["status"] == "running" && r["sessionId"].is_string()
            })
            .await;
    }
    // Both real fixture subprocesses have received their prompts and are waiting on separate questions.
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if s.chat_detail(&chats[0]).await.unwrap()["pendingQuestions"] == 1
                && s.chat_detail(&chats[1]).await.unwrap()["pendingQuestions"] == 1
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
    })
    .await
    .unwrap();
    for run in &runs[..2] {
        let credentials = std::fs::read_to_string(
            s.config
                .data_dir
                .join("runs")
                .join(run)
                .join("home/.claude/.credentials.json"),
        )
        .unwrap();
        assert!(!credentials.contains("refresh"));
        assert!(credentials.contains("fixture-access"));
    }
    s.accounts
        .update(s, &account, &json!({"maxConcurrentRuns":1}))
        .await
        .unwrap();
    for run in &runs[..2] {
        assert_eq!(s.store.run(run).await.unwrap()["status"], "running");
    }
    let first_question = s.chat_detail(&chats[0]).await.unwrap()["questions"][0]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    s.question_answer(
        &chats[0],
        &first_question,
        json!({"id":id(),"answers":{"0":["Small change"]}}),
    )
    .await
    .unwrap();
    fixture
        .until(&runs[0], |r| r["status"] == "succeeded")
        .await;
    fixture
        .until(&runs[2], |r| {
            text(r, "accountWaitReason").contains("free Claude Code account slot")
        })
        .await;
    assert_eq!(s.store.run(&runs[1]).await.unwrap()["status"], "running");
    let second_question = s.chat_detail(&chats[1]).await.unwrap()["questions"][0]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    s.question_answer(
        &chats[1],
        &second_question,
        json!({"id":id(),"answers":{"0":["Small change"]}}),
    )
    .await
    .unwrap();
    fixture
        .until(&runs[1], |r| r["status"] == "succeeded")
        .await;
    fixture
        .until(&runs[2], |r| r["status"] == "succeeded")
        .await;
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
    assert_eq!(s.store.run(id).await.unwrap()["status"], "running");
    assert_eq!(
        s.store
            .kv(&format!("run-checkpoint:{id}"))
            .await
            .unwrap()
            .unwrap()
            .get("remainingMs"),
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
    fixture.until(id, |r| r["status"] == "cancelled").await;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let response = command("resume").send().await.unwrap();
            if response.status().as_u16() != 409 {
                response.error_for_status().unwrap();
                break;
            }
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
    })
    .await
    .unwrap();
    fixture.until(id, |r| r["status"] == "succeeded").await;
    assert_eq!(
        s.store
            .kv(&format!("run-checkpoint:{id}"))
            .await
            .unwrap()
            .unwrap()
            .get("remainingMs"),
        Some(&Value::Null)
    );
    fixture.stop(false).await;
    let run = fixture.enqueue("fixture:hang").await;
    let id = text(&run, "id");
    let mut snapshot = run["snapshot"].clone();
    snapshot["agent"]["timeoutMinutes"] = 1.into();
    s.store
        .patch_run(id, json!({"snapshot":snapshot}))
        .await
        .unwrap();
    // Resume the final five seconds of an existing one-minute budget.
    s.store
        .set(
            &format!("run-checkpoint:{id}"),
            json!({"launched":false,"remainingMs":5000}),
            None,
        )
        .await
        .unwrap();
    fixture.start().await;
    fixture
        .until(id, |r| r["sessionId"] == "fixture-session")
        .await;
    let failed = fixture.until(id, |r| r["status"] == "failed").await;
    assert!(text(&failed, "summary").contains("time limit"), "{failed}");
    fixture.stop(false).await;
}

#[tokio::test]
async fn queued_work_uses_current_node_grants_without_rejecting_unrelated_policy() {
    let mut fixture = Fixture::new().await;
    fixture.stop(false).await;
    let s = &fixture.service;
    let mut agent = s
        .get("agents", leo_agent_manager::config::MAIN_AGENT_ID)
        .await
        .unwrap();
    agent["access"]["nodes"] = Value::Null;
    s.store.put("agents", agent.clone()).await.unwrap();
    let run = fixture
        .enqueue("Inspect the fixture after a node policy change")
        .await;
    agent["access"]["nodes"] = json!([leo_agent_manager::nodes::LOCAL_NODE_ID]);
    s.store.put("agents", agent).await.unwrap();
    fixture.start().await;
    let completed = fixture
        .until(text(&run, "id"), |r| {
            ["succeeded", "failed"].contains(&text(r, "status"))
        })
        .await;
    assert_eq!(completed["status"], "succeeded", "{completed}");
    assert_eq!(
        completed["snapshot"]["agent"]["access"]["nodes"],
        json!([leo_agent_manager::nodes::LOCAL_NODE_ID])
    );
    fixture.stop(false).await;
}
