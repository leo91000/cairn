use crate::common::run_fixture::DbRunFixture;
use common::AgentFixture;
use common::MissionFixture;
use common::StoreAgentFixture;
use common::account_fixture::AccountFixtures;

use common::attempt_fixture::*;
use common::node_fixture::*;
use common::run_fixture::*;

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

async fn api_post(app: &Router, token: &str, path: &str, body: Value) -> (StatusCode, Value) {
    let request = common::request("POST", path)
        .header("x-test-relay-token", token)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let response = common::send(app, request).await;
    let status = response.status();
    (status, common::read_json(response).await)
}

/// Signal only the checkpoint supervisor's private runner child.
async fn kill_fixture_runner_child(saved: &Value) {
    let process = saved["process"].clone();
    let pid = u32::try_from(process["pid"].as_u64().unwrap()).unwrap();
    let identity = cairn_installation::recovery::process_identity(pid)
        .await
        .unwrap();
    assert_eq!(serde_json::to_value(identity).unwrap(), process);
    // The checkpoint identifies the supervisor. Find only its runner
    // child, validating the exact private attempt before sending a signal.
    let runner = eventually(
        Duration::from_secs(3),
        Duration::from_millis(10),
        async || {
            // Per-thread children files require an optional kernel feature.
            // Parent metadata is available on ordinary proc mounts as well.
            let mut processes = tokio::fs::read_dir("/proc").await.unwrap();
            let mut runner = None;
            while let Some(process) = processes.next_entry().await.unwrap() {
                let Some(child) = process
                    .file_name()
                    .to_str()
                    .and_then(|name| name.parse::<i32>().ok())
                else {
                    continue;
                };
                let status = match tokio::fs::read_to_string(process.path().join("status")).await {
                    Ok(status) => status,
                    Err(error)
                        if matches!(
                            error.kind(),
                            std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied
                        ) =>
                    {
                        continue;
                    }
                    Err(error) => panic!("Cannot inspect fixture parent: {error}"),
                };
                let parent = status
                    .lines()
                    .find_map(|line| line.strip_prefix("PPid:"))
                    .and_then(|value| value.trim().parse::<u32>().ok());
                if parent != Some(pid) {
                    continue;
                }
                let args = match tokio::fs::read(process.path().join("cmdline")).await {
                    Ok(args) => args,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                    Err(error) => panic!("Cannot inspect fixture child: {error}"),
                };
                let args = args.split(|byte| *byte == 0).collect::<Vec<_>>();
                let command_matches = args.contains(&b"runner-client".as_slice());
                let attempt_matches = args.contains(&text(saved, "runnerId").as_bytes());
                if command_matches && attempt_matches {
                    runner = Some(child);
                }
            }
            runner
        },
    )
    .await;
    assert_eq!(
        serde_json::to_value(
            cairn_installation::recovery::process_identity(pid)
                .await
                .unwrap()
        )
        .unwrap(),
        process
    );
    assert_eq!(unsafe { libc::kill(runner, libc::SIGKILL) }, 0);
}

/// The real manager runtime in a separate process, with the synthetic relay
/// transport for installation-only worker tests. Browser access is tested
/// through the Beacon and real installation relay.
struct Fixture {
    root: TempDir,
    service: Arc<Service>,
    process: Option<Child>,
    url: String,
    clock: Option<FixtureClock>,
}

struct FixtureClock {
    value: PathBuf,
    millis: i64,
}

fn worker_fixture_binary(
    target: &Path,
    fixtures: Option<&std::ffi::OsStr>,
    allow_remap: bool,
) -> PathBuf {
    if let Some(fixtures) = fixtures {
        let fixtures: Vec<Value> = serde_json::from_slice(fixtures.as_encoded_bytes()).unwrap();
        fixtures
            .iter()
            .find(|fixture| {
                fixture["target"]
                    .as_str()
                    .is_some_and(|path| Path::new(path) == target)
            })
            .or_else(|| {
                if !allow_remap {
                    return None;
                }
                if fixtures.len() == 1 {
                    return fixtures.first();
                }
                let suffix = PathBuf::from(target.parent()?.file_name()?).join(target.file_name()?);
                let mut matches = fixtures.iter().filter(|fixture| {
                    fixture["target"]
                        .as_str()
                        .is_some_and(|path| Path::new(path).ends_with(&suffix))
                });
                let matching = matches.next()?;
                matches.next().is_none().then_some(matching)
            })
            .and_then(|fixture| fixture["binary"].as_str())
            .map(PathBuf::from)
            .expect("launcher supplied no worker fixture for this target")
    } else {
        target.join("examples/worker_fixture")
    }
}

#[test]
fn worker_fixture_uses_the_matching_compilation_target() {
    let root = TempDir::new().unwrap();
    let first = root.path().join("first/debug");
    let second = root.path().join("second/debug");
    let fixtures = std::ffi::OsString::from(
        json!([
            { "target": first, "binary": root.path().join("first-fixture") },
            { "target": second, "binary": root.path().join("second-fixture") },
        ])
        .to_string(),
    );
    assert_eq!(
        worker_fixture_binary(&first, Some(&fixtures), false),
        root.path().join("first-fixture")
    );
    assert_eq!(
        worker_fixture_binary(&second, Some(&fixtures), false),
        root.path().join("second-fixture")
    );
    assert_eq!(
        worker_fixture_binary(
            &root.path().join("remapped/first/debug"),
            Some(&fixtures),
            true
        ),
        root.path().join("first-fixture")
    );
    let single = std::ffi::OsString::from(
        json!([{ "target": first, "binary": root.path().join("first-fixture") }]).to_string(),
    );
    assert_eq!(
        worker_fixture_binary(&root.path().join("remapped"), Some(&single), true),
        root.path().join("first-fixture")
    );
}

#[test]
#[should_panic(expected = "launcher supplied no worker fixture for this target")]
fn worker_fixture_rejects_a_missing_compilation_target() {
    worker_fixture_binary(
        Path::new("missing"),
        Some(std::ffi::OsStr::new("[]")),
        false,
    );
}

#[test]
#[should_panic(expected = "launcher supplied no worker fixture for this target")]
fn worker_fixture_does_not_remap_an_ordinary_build() {
    let fixtures = std::ffi::OsStr::new(r#"[{"target":"first","binary":"first-fixture"}]"#);
    worker_fixture_binary(Path::new("second"), Some(fixtures), false);
}

#[test]
#[should_panic(expected = "launcher supplied no worker fixture for this target")]
fn worker_fixture_rejects_an_ambiguous_remap() {
    let fixtures = std::ffi::OsStr::new(
        r#"[{"target":"one/triple/debug","binary":"one"},{"target":"two/triple/debug","binary":"two"}]"#,
    );
    worker_fixture_binary(Path::new("other/triple/debug"), Some(fixtures), true);
}

impl FixtureClock {
    fn new(root: &Path) -> Self {
        require_fixture_hooks(std::env::var_os("CAIRN_TEST_WORKER_FIXTURES").as_deref());
        let value = root.join("fixture-clock.txt");
        let millis = now();
        std::fs::write(&value, millis.to_string()).unwrap();
        Self { value, millis }
    }

    fn advance(&mut self, millis: i64) {
        self.millis += millis;
        let next = self.value.with_extension("next");
        std::fs::write(&next, self.millis.to_string()).unwrap();
        std::fs::rename(next, &self.value).unwrap();
    }
}

fn require_fixture_hooks(fixtures: Option<&std::ffi::OsStr>) {
    assert!(
        fixtures.is_some(),
        "fixture clock requires worker hooks; run this test with pnpm test:backend"
    );
}

#[test]
#[should_panic(
    expected = "fixture clock requires worker hooks; run this test with pnpm test:backend"
)]
fn fixture_clock_requires_the_backend_launcher() {
    require_fixture_hooks(None);
}

// Readiness is emitted only after the manager has initialized durable state,
// bound its socket, and started the worker. Allow slow storage to finish while
// still bounding a child that never becomes ready.
const WORKER_STARTUP_TIMEOUT: Duration = Duration::from_secs(60);

async fn wait_for_worker_ready(child: &mut Child, limit: Duration) -> Result<String, String> {
    let stdout = child
        .stdout
        .take()
        .ok_or("worker fixture stdout is missing")?;
    let mut lines = BufReader::new(stdout).lines();
    let line = tokio::time::timeout(limit, lines.next_line())
        .await
        .map_err(|_| format!("worker fixture did not become ready within {limit:?}"))?
        .map_err(|error| format!("reading worker fixture readiness: {error}"))?;
    let Some(line) = line else {
        let status = child.try_wait().map_err(|error| error.to_string())?;
        return Err(format!(
            "worker fixture closed stdout before readiness (status: {status:?})"
        ));
    };
    line.strip_prefix("Listening on ")
        .map(str::to_owned)
        .ok_or_else(|| format!("unexpected worker fixture readiness: {line}"))
}

#[tokio::test]
async fn worker_readiness_waits_for_output_and_reports_blocked_or_exited_children() {
    let mut delayed = Command::new("sh")
        .args(["-c", "sleep 0.1; echo 'Listening on http://127.0.0.1:1234'"])
        .stdout(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    assert_eq!(
        wait_for_worker_ready(&mut delayed, Duration::from_secs(2))
            .await
            .unwrap(),
        "http://127.0.0.1:1234"
    );

    let mut blocked = Command::new("sh")
        .args(["-c", "exec sleep 5"])
        .stdout(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    assert!(
        wait_for_worker_ready(&mut blocked, Duration::from_millis(100))
            .await
            .unwrap_err()
            .contains("did not become ready")
    );

    let mut exited = Command::new("sh")
        .args(["-c", "exit 1"])
        .stdout(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    assert!(
        wait_for_worker_ready(&mut exited, Duration::from_secs(2))
            .await
            .unwrap_err()
            .contains("closed stdout before readiness")
    );
}

impl Fixture {
    async fn new() -> Self {
        Self::with_concurrency(2).await
    }

    async fn with_concurrency(concurrency: usize) -> Self {
        Self::with_opencode_bin(concurrency, "opencode".into()).await
    }

    async fn with_opencode() -> Self {
        Self::with_opencode_bin(2, common::fixture("opencode.mjs")).await
    }

    async fn with_opencode_bin(concurrency: usize, opencode_bin: String) -> Self {
        let root = TempDir::new().unwrap();
        std::fs::create_dir(root.path().join("home")).unwrap();
        let config = Config {
            codex_bin: common::fixture("codex.mjs"),
            claude_bin: common::fixture("claude.mjs"),
            opencode_bin,
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
            clock: None,
        };
        fixture.start().await;
        fixture
    }

    async fn start(&mut self) {
        let file = self.root.path().join("config.json");
        tokio::fs::write(&file, serde_json::to_vec(&self.service.config).unwrap())
            .await
            .unwrap();
        let target = std::path::Path::new(env!("CARGO_BIN_EXE_cairn"))
            .parent()
            .unwrap();
        let fixtures = std::env::var_os("CAIRN_TEST_WORKER_FIXTURES");
        let allow_remap = std::env::var_os("CAIRN_TEST_REUSED_BUILD").is_some();
        let binary = worker_fixture_binary(target, fixtures.as_deref(), allow_remap);
        let mut command = Command::new(binary);
        command.arg("serve");
        command.env_remove("CAIRN_FIXTURE_CLOCK_FILE");
        if let Some(clock) = &self.clock {
            command.env("CAIRN_FIXTURE_CLOCK_FILE", &clock.value);
        }
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
        self.url = wait_for_worker_ready(&mut child, WORKER_STARTUP_TIMEOUT)
            .await
            .unwrap_or_else(|error| panic!("{error}"));
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
            Duration::from_secs(60),
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
            .attempts_fixture()
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
        let plans = state.plans.lock().await;
        let plan = plans.iter().find(|plan| plan["id"] == attempt).unwrap();
        // OpenCode validates saved session IDs when resuming; keep the Codex fixture value intact.
        let provider = if plan.get("chat").is_some_and(|chat| !chat.is_null()) {
            cairn_installation::microvm::plan::Plan::new(plan.clone())
                .chat_provider()
                .unwrap()
        } else {
            None
        };
        let session_id = if provider == Some(Provider::Opencode) {
            "ses_fixture_session"
        } else {
            "fixture-session"
        };
        let mut output = format!(
            "{}\n",
            json!({
                "type": "thread.started",
                "thread_id": session_id,
            })
        );
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
        // The runner bounds reconciliation after EOF; held attempts must stay live.
        let stream = futures_util::stream::unfold(
            (Some(format!("{frame}\n")), state),
            |(frame, state)| async move {
                let bytes = if let Some(frame) = frame {
                    axum::body::Bytes::from(frame)
                } else if state.hold.load(std::sync::atomic::Ordering::Relaxed) {
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    axum::body::Bytes::from_static(b"{\"type\":\"heartbeat\"}\n")
                } else {
                    return None;
                };
                Some((Ok::<_, std::io::Error>(bytes), (None, state)))
            },
        );
        return Body::from_stream(stream).into_response();
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

fn classify_opencode_resume_error(error: Option<&str>) -> &'static str {
    const HTTP_PREFIX: &str = "Remote workspace preparation failed (HTTP ";
    const HTTP_SUFFIX: &str = "); source files are preserved.";

    let Some(error) = error.filter(|error| error.len() <= 512) else {
        return "unknown";
    };

    let known_errors = [
        (
            "Choose an OpenCode model in provider/model format.",
            "opencode_model_format_invalid",
        ),
        (
            "OpenCode currently requires YOLO access in an isolated execution environment. It does not implement the read-only or workspace-write sandbox.",
            "opencode_sandbox_invalid",
        ),
        (
            "OpenCode MCP server names must remain distinct after native normalization.",
            "opencode_mcp_server_name_collision",
        ),
        (
            "OpenCode MCP tool names must remain distinct after native normalization.",
            "opencode_mcp_tool_name_collision",
        ),
        (
            "Connect an OpenCode model-provider API key before starting this agent.",
            "opencode_account_lease_missing",
        ),
        ("Account not found.", "opencode_account_record_missing"),
        (
            "This OpenCode connection no longer offers the selected model. Choose another model or connection.",
            "opencode_auto_route_unavailable",
        ),
        (
            "Agent access was reduced after this run was queued. Run the task again with the current policy.",
            "agent_access_reduced",
        ),
        (
            "Isolated runner is not configured. This agent will not fall back to shared execution.",
            "isolated_runner_unconfigured",
        ),
        (
            "Configure a valid STORAGE_S3_BUCKET on the server.",
            "storage_bucket_invalid",
        ),
        (
            "Confirm that R2 public domains and bucket locks are disabled in the Cloudflare dashboard.",
            "storage_private_bucket_unconfirmed",
        ),
        (
            "STORAGE_S3_ENDPOINT must be an https:// URL.",
            "storage_endpoint_requires_https",
        ),
        (
            "Remote workspace preparation interrupted.",
            "remote_workspace_interrupted",
        ),
        (
            "Remote workspace preparation timed out after 280 seconds; source files are preserved.",
            "remote_workspace_timeout",
        ),
        (
            "An internal error occurred. Check the server logs.",
            "internal_error",
        ),
    ];
    if let Some((_, class)) = known_errors.iter().find(|(known, _)| *known == error) {
        return class;
    }

    if let Some(status) = error
        .strip_prefix(HTTP_PREFIX)
        .and_then(|error| error.strip_suffix(HTTP_SUFFIX))
        && status.len() == 3
        && status.bytes().all(|byte| byte.is_ascii_digit())
    {
        return "remote_workspace_http_error";
    }

    "unknown"
}

#[test]
fn opencode_resume_error_classification_is_exact_bounded_and_redacted() {
    let cases = [
        (
            "Choose an OpenCode model in provider/model format.",
            "opencode_model_format_invalid",
        ),
        (
            "OpenCode currently requires YOLO access in an isolated execution environment. It does not implement the read-only or workspace-write sandbox.",
            "opencode_sandbox_invalid",
        ),
        (
            "OpenCode MCP server names must remain distinct after native normalization.",
            "opencode_mcp_server_name_collision",
        ),
        (
            "OpenCode MCP tool names must remain distinct after native normalization.",
            "opencode_mcp_tool_name_collision",
        ),
        (
            "Connect an OpenCode model-provider API key before starting this agent.",
            "opencode_account_lease_missing",
        ),
        ("Account not found.", "opencode_account_record_missing"),
        (
            "This OpenCode connection no longer offers the selected model. Choose another model or connection.",
            "opencode_auto_route_unavailable",
        ),
        (
            "Agent access was reduced after this run was queued. Run the task again with the current policy.",
            "agent_access_reduced",
        ),
        (
            "Isolated runner is not configured. This agent will not fall back to shared execution.",
            "isolated_runner_unconfigured",
        ),
        (
            "Configure a valid STORAGE_S3_BUCKET on the server.",
            "storage_bucket_invalid",
        ),
        (
            "Confirm that R2 public domains and bucket locks are disabled in the Cloudflare dashboard.",
            "storage_private_bucket_unconfirmed",
        ),
        (
            "STORAGE_S3_ENDPOINT must be an https:// URL.",
            "storage_endpoint_requires_https",
        ),
        (
            "Remote workspace preparation interrupted.",
            "remote_workspace_interrupted",
        ),
        (
            "Remote workspace preparation timed out after 280 seconds; source files are preserved.",
            "remote_workspace_timeout",
        ),
        (
            "An internal error occurred. Check the server logs.",
            "internal_error",
        ),
    ];
    for (error, expected_class) in cases {
        assert_eq!(classify_opencode_resume_error(Some(error)), expected_class);
    }
    assert_eq!(
        classify_opencode_resume_error(Some(
            "Remote workspace preparation failed (HTTP 404); source files are preserved.",
        )),
        "remote_workspace_http_error"
    );
    assert_eq!(classify_opencode_resume_error(None), "unknown");
    assert_eq!(
        classify_opencode_resume_error(Some("remote workspace preparation timed out")),
        "unknown"
    );
    assert_eq!(
        classify_opencode_resume_error(Some("unrecognized provider diagnostic")),
        "unknown"
    );
    assert_eq!(
        classify_opencode_resume_error(Some(
            "Remote workspace preparation failed (HTTP 40x); source files are preserved.",
        )),
        "unknown"
    );
    assert_eq!(
        classify_opencode_resume_error(Some(
            "Remote workspace preparation failed (HTTP 404); source files are preserved. extra",
        )),
        "unknown"
    );
    let boundary_error = "x".repeat(512);
    assert_eq!(
        classify_opencode_resume_error(Some(boundary_error.as_str())),
        "unknown"
    );
    let oversized_error = "x".repeat(513);
    assert_eq!(
        classify_opencode_resume_error(Some(oversized_error.as_str())),
        "unknown"
    );
}

async fn report_opencode_resume_wait_state(
    fixture: &Fixture,
    run_id: &str,
    test: &'static str,
    wait: &'static str,
    plan_received: &std::sync::atomic::AtomicBool,
    log_request_seen: &std::sync::atomic::AtomicBool,
) {
    let run = fixture.service.store.run(run_id).await.ok();
    let run_record_present = run.is_some();
    let status = run
        .as_ref()
        .and_then(RunStatus::of)
        .map_or("other", |status| status.as_str());
    let session_present = run.as_ref().is_some_and(|run| run["sessionId"].is_string());
    // The initial session ID is persisted only when the manager accepts `thread.started`.
    let thread_started_observed = session_present;
    let resume_available = run
        .as_ref()
        .is_some_and(|run| run["resumeAvailable"] == true);
    let error_present = run.as_ref().is_some_and(|run| !run["error"].is_null());
    let error_class =
        classify_opencode_resume_error(run.as_ref().and_then(|run| run["error"].as_str()));
    let account_id_recorded = run.as_ref().is_some_and(|run| run["accountId"].is_string());
    let account_required_is_opencode = run
        .as_ref()
        .is_some_and(|run| run["accountRequired"] == "opencode");
    let account_id = run.as_ref().and_then(|run| run["accountId"].as_str());
    let (account_record_present, account_ready, account_enabled) =
        if let Some(account_id) = account_id {
            match fixture
                .service
                .accounts
                .account(&fixture.service, account_id)
                .await
            {
                Ok(account) => (true, account.is_ready(), account.enabled),
                Err(_) => (false, false, false),
            }
        } else {
            (false, false, false)
        };
    let checkpoint = fixture
        .service
        .store
        .checkpoint_boundary(run_id)
        .await
        .ok()
        .flatten();
    let checkpoint_record_present = checkpoint.is_some();
    // These checkpoint fields are retained state and may describe an earlier attempt;
    // they do not identify the current worker's failure phase.
    let checkpoint_prepared_recorded = checkpoint
        .as_ref()
        .is_some_and(|checkpoint| checkpoint["prepared"].is_object());
    let checkpoint_runner_id_recorded = checkpoint
        .as_ref()
        .is_some_and(|checkpoint| checkpoint["runnerId"].is_string());
    let checkpoint_process_recorded = checkpoint
        .as_ref()
        .is_some_and(|checkpoint| checkpoint["process"].is_object());
    let checkpoint_node_id_recorded = checkpoint
        .as_ref()
        .is_some_and(|checkpoint| checkpoint["nodeId"].is_string());
    let plan_received = plan_received.load(std::sync::atomic::Ordering::Relaxed);
    let log_request_observed = log_request_seen.load(std::sync::atomic::Ordering::Relaxed);
    let retained_state_category = if !run_record_present {
        "unknown"
    } else if plan_received {
        "test_controller_plan_post_observed"
    } else if checkpoint_process_recorded {
        "checkpoint_process_recorded"
    } else if checkpoint_runner_id_recorded {
        "checkpoint_runner_id_recorded"
    } else if checkpoint_prepared_recorded {
        "checkpoint_prepared_recorded"
    } else if account_id_recorded {
        "account_id_recorded"
    } else {
        "account_id_not_recorded"
    };
    eprintln!(
        "CAIRN_RESUME_TEST_DIAGNOSTIC test={test} wait={wait} status={status} runRecordPresent={run_record_present} checkpointRecordPresent={checkpoint_record_present} retainedStateCategory={retained_state_category} errorClass={error_class} accountIdRecorded={account_id_recorded} accountRequiredIsOpenCode={account_required_is_opencode} accountRecordPresent={account_record_present} accountReady={account_ready} accountEnabled={account_enabled} checkpointPreparedRecorded={checkpoint_prepared_recorded} checkpointRunnerIdRecorded={checkpoint_runner_id_recorded} checkpointProcessRecorded={checkpoint_process_recorded} checkpointNodeIdRecorded={checkpoint_node_id_recorded} sessionPresent={session_present} resumeAvailable={resume_available} errorPresent={error_present} planReceived={plan_received} logRequestObserved={log_request_observed} threadStartedObserved={thread_started_observed}"
    );
}

async fn await_opencode_resume_setup(
    fixture: &Fixture,
    run_id: &str,
    test: &'static str,
    plan_received: &std::sync::atomic::AtomicBool,
    log_request_seen: &std::sync::atomic::AtomicBool,
) -> Value {
    eprintln!("CAIRN_RESUME_TEST_PHASE test={test} phase=initial_run_wait_before");
    let running = tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            let run = fixture.service.store.run(run_id).await.unwrap();
            if run["status"] == RunStatus::Running && run["sessionId"].is_string() {
                return run;
            }
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
    })
    .await;
    if running.is_err() {
        report_opencode_resume_wait_state(
            fixture,
            run_id,
            test,
            "initial_run_wait",
            plan_received,
            log_request_seen,
        )
        .await;
        eprintln!(
            "CAIRN_RESUME_TEST_PHASE test={test} phase=initial_run_wait_after result=timeout"
        );
    } else {
        eprintln!(
            "CAIRN_RESUME_TEST_PHASE test={test} phase=initial_run_wait_after result=success"
        );
    }
    let running = running.expect("the awaited condition never held");

    eprintln!("CAIRN_RESUME_TEST_PHASE test={test} phase=controller_plan_wait_before");
    let plan_wait = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if plan_received.load(std::sync::atomic::Ordering::Relaxed) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
    })
    .await;
    if plan_wait.is_err() {
        report_opencode_resume_wait_state(
            fixture,
            run_id,
            test,
            "controller_plan_wait",
            plan_received,
            log_request_seen,
        )
        .await;
        eprintln!(
            "CAIRN_RESUME_TEST_PHASE test={test} phase=controller_plan_wait_after result=timeout"
        );
    } else {
        eprintln!(
            "CAIRN_RESUME_TEST_PHASE test={test} phase=controller_plan_wait_after result=success"
        );
    }
    plan_wait.expect("the awaited condition never held");

    running
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

#[tokio::test(start_paused = true)]
async fn controller_held_attempt_keeps_output_open_until_released() {
    use futures_util::StreamExt;

    let root = TempDir::new().unwrap();
    let controller = Arc::new(Controller {
        data: root.path().to_owned(),
        plans: tokio::sync::Mutex::new(vec![json!({ "id": "fixture" })]),
        failures: 0,
        slots: 1,
        hold: std::sync::atomic::AtomicBool::new(true),
    });
    let request = Request::builder()
        .uri("/runs/fixture/logs")
        .body(Body::empty())
        .unwrap();
    let response = serve_controller(State(controller.clone()), request).await;
    let mut stream = response.into_body().into_data_stream();
    let output: Value = serde_json::from_slice(&stream.next().await.unwrap().unwrap()).unwrap();
    assert_eq!(output["type"], "output");

    // The runner reconciles a clean output EOF after five seconds.
    tokio::time::advance(Duration::from_secs(6)).await;
    let heartbeat = stream
        .next()
        .await
        .expect("a held attempt must keep its output connection alive")
        .unwrap();
    let heartbeat: Value = serde_json::from_slice(&heartbeat).unwrap();
    assert_eq!(heartbeat["type"], "heartbeat");

    controller
        .hold
        .store(false, std::sync::atomic::Ordering::Relaxed);
    assert!(stream.next().await.is_none());
}

#[tokio::test]
async fn node_slots_exceed_manager_concurrency_without_overpreparing_the_queue() {
    let mut fixture = Fixture::new().await;
    // One extra node slot exercises the manager concurrency boundary.
    let slots = fixture.service.config.concurrency + 1;
    fixture.stop(false).await;
    let controller = Arc::new(Controller {
        data: fixture.service.config.data_dir.clone(),
        plans: tokio::sync::Mutex::new(Vec::new()),
        failures: 0,
        slots,
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
    node["slots"] = slots.into();
    fixture.service.store.put_node_fixture(node).await.unwrap();
    let mut runs = Vec::new();
    for _ in 0..=slots {
        runs.push(fixture.enqueue("Inspect the VM fixture").await);
    }
    fixture.start().await;
    eventually(
        Duration::from_secs(20),
        Duration::from_millis(30),
        async || (controller.plans.lock().await.len() == slots).then_some(()),
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
    assert_eq!(running, slots);
    assert_eq!(queued.len(), 1);
    assert!(
        queued[0]["startedAt"].is_null(),
        "queued work must never enter recovery: {}",
        queued[0]
    );
    assert_eq!(queued[0]["recoveryPending"], false);
    assert_eq!(controller.plans.lock().await.len(), slots);
    let attempts = fixture.service.store.attempts().await.unwrap();
    assert_eq!(attempts.len(), slots);
    controller
        .hold
        .store(false, std::sync::atomic::Ordering::Relaxed);
    for run in &runs {
        assert_eq!(
            fixture.until_finished(text(run, "id")).await["status"],
            RunStatus::Succeeded
        );
    }
    assert_eq!(controller.plans.lock().await.len(), slots + 1);
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
async fn paid_recovery_survives_confirmed_capacity_rejection_and_restart() {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    for provider in [false, true] {
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
        let (reject, seen) = (busy.clone(), rejections.clone());
        let app = Router::new()
            .fallback(any(
                move |state: State<Arc<Controller>>, request: Request| {
                    let (reject, seen) = (reject.clone(), seen.clone());
                    async move {
                        let path = request.uri().path();
                        if request.method() == "POST"
                            && path.starts_with("/runs/")
                            && path.trim_matches('/').split('/').count() == 2
                            && reject.load(Ordering::SeqCst)
                        {
                            seen.fetch_add(1, Ordering::SeqCst);
                            return (
                                StatusCode::SERVICE_UNAVAILABLE,
                                Json(json!({ "error": "All VM slots are occupied." })),
                            )
                                .into_response();
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
        let service = fixture.service.clone();
        let home = service.config.home.join(".codex");
        tokio::fs::create_dir_all(&home).await.unwrap();
        tokio::fs::write(home.join("auth.json"), "{}")
            .await
            .unwrap();
        tokio::fs::write(
            service.config.data_dir.join("storage-s3.json"),
            json!({
                "bucket": "fixture-storage", "endpoint": "https://127.0.0.1:1"
            })
            .to_string(),
        )
        .await
        .unwrap();
        let run = fixture
            .enqueue("Preserve paid admission after refusal")
            .await;
        let id = text(&run, "id").to_owned();
        fixture.start().await;
        let first = fixture
            .until(&id, |run| {
                finished(run)
                    || (rejections.load(Ordering::SeqCst) >= 1
                        && run["status"] == RunStatus::Queued
                        && !run["recoveryPending"].as_bool().unwrap_or(true))
            })
            .await;
        assert_eq!(first["status"], RunStatus::Queued, "{first}");
        fixture.stop(false).await;
        let mut saved = fixture.checkpoint(&id).await;
        let old_attempt = text(&saved, "runnerId").to_owned();
        assert_eq!(saved["startRejected"], true);
        saved["capacityRetryAt"] = (now() - 1).into();
        saved["controllerRecoveries"] = 3.into();
        if provider {
            let cause: cairn_installation::run_retry::Cause = serde_json::from_value(json!({
                "kind": "server", "message": "Provider busy", "startup": true
            }))
            .unwrap();
            let mut retry = cairn_installation::run_retry::Retry::next(None, cause, now()).unwrap();
            retry.next_attempt_at = Some(now() - 1);
            saved["retryCause"] = serde_json::to_value(&retry.cause).unwrap();
            saved["providerRetryReservation"] = old_attempt.into();
            service
                .store
                .change_run(
                    &id,
                    cairn_installation::runs::RunChanges {
                        retry: Some(Some(retry)),
                        recovery_pending: Some(true),
                        ..Default::default()
                    },
                )
                .await
                .unwrap();
        } else {
            saved["controllerRecoveryReservation"] = old_attempt.into();
            service
                .store
                .change_run(
                    &id,
                    cairn_installation::runs::RunChanges {
                        recovery_pending: Some(true),
                        ..Default::default()
                    },
                )
                .await
                .unwrap();
        }
        service
            .store
            .save_checkpoint_boundary(&id, saved)
            .await
            .unwrap();
        fixture.start().await;
        let waiting = fixture
            .until(&id, |run| {
                finished(run)
                    || (rejections.load(Ordering::SeqCst) >= 2
                        && run["status"] == RunStatus::Queued
                        && !run["recoveryPending"].as_bool().unwrap_or(true))
            })
            .await;
        assert_eq!(
            waiting["status"],
            RunStatus::Queued,
            "provider={provider}: {waiting}"
        );
        fixture.stop(false).await;
        let mut saved = fixture.checkpoint(&id).await;
        assert_eq!(saved["startRejected"], true);
        assert_eq!(saved["controllerRecoveries"], 3);
        assert!(controller.plans.lock().await.is_empty());
        saved["capacityRetryAt"] = (now() - 1).into();
        service
            .store
            .save_checkpoint_boundary(&id, saved)
            .await
            .unwrap();
        busy.store(false, Ordering::SeqCst);
        fixture.start().await;
        let completed = fixture.until_finished(&id).await;
        assert_eq!(
            completed["status"],
            RunStatus::Succeeded,
            "provider={provider}: {completed}"
        );
        assert_eq!(fixture.checkpoint(&id).await["controllerRecoveries"], 3);
        assert_eq!(controller.plans.lock().await.len(), 1);
        fixture.stop(false).await;
        server.abort();
    }
}

#[tokio::test]
async fn transferred_paid_move_launches_once_without_resetting_controller_count() {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
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
    let (reject, seen) = (busy.clone(), rejections.clone());
    let app = Router::new()
        .fallback(any(
            move |state: State<Arc<Controller>>, request: Request| {
                let (reject, seen) = (reject.clone(), seen.clone());
                async move {
                    let path = request.uri().path();
                    if request.method() == "POST"
                        && path.starts_with("/runs/")
                        && path.trim_matches('/').split('/').count() == 2
                        && reject.load(Ordering::SeqCst)
                    {
                        seen.fetch_add(1, Ordering::SeqCst);
                        return (
                            StatusCode::SERVICE_UNAVAILABLE,
                            Json(json!({ "error": "All VM slots are occupied." })),
                        )
                            .into_response();
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
    let service = fixture.service.clone();
    let home = service.config.home.join(".codex");
    tokio::fs::create_dir_all(&home).await.unwrap();
    tokio::fs::write(home.join("auth.json"), "{}")
        .await
        .unwrap();
    tokio::fs::write(
        service.config.data_dir.join("storage-s3.json"),
        json!({ "bucket": "fixture-storage", "endpoint": "https://127.0.0.1:1" }).to_string(),
    )
    .await
    .unwrap();
    let run = fixture.enqueue("Resume paid move").await;
    let id = text(&run, "id").to_owned();
    fixture.start().await;
    let waiting = fixture
        .until(&id, |run| {
            finished(run)
                || (rejections.load(Ordering::SeqCst) >= 1
                    && run["status"] == RunStatus::Queued
                    && run["recoveryPending"] == false)
        })
        .await;
    assert_eq!(waiting["status"], RunStatus::Queued, "{waiting}");
    fixture.stop(false).await;
    let mut saved = fixture.checkpoint(&id).await;
    let old = text(&saved, "runnerId").to_owned();
    saved["startRejected"] = false.into();
    saved["terminalCode"] = 75.into();
    saved["capacityRetryAt"] = (now() - 1).into();
    saved["controllerRecoveries"] = 4.into();
    saved["controllerRecoveryReservation"] = old.clone().into();
    saved["moveRecoveryReservation"] = old.into();
    service
        .store
        .save_checkpoint_boundary(&id, saved)
        .await
        .unwrap();
    service
        .store
        .change_run(
            &id,
            cairn_installation::runs::RunChanges {
                recovery_pending: Some(true),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    busy.store(false, Ordering::SeqCst);
    fixture.start().await;
    let completed = fixture.until_finished(&id).await;
    assert_eq!(completed["status"], RunStatus::Succeeded, "{completed}");
    let saved = fixture.checkpoint(&id).await;
    assert_eq!(saved["controllerRecoveries"], 4);
    assert!(saved["moveRecoveryReservation"].is_null());
    assert_eq!(controller.plans.lock().await.len(), 1);
    fixture.stop(false).await;
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

/// The real runner client and manager must retain a broken output stream even
/// when the controller reports a healthy physical completion.
#[tokio::test]
async fn interrupted_controller_output_after_healthy_exit_never_replays_the_turn() {
    use std::sync::atomic::{AtomicBool, Ordering};

    for (completed, truncated, clean_eof) in [
        (false, true, false),
        (true, true, false),
        (false, false, false),
        (true, false, false),
        (false, false, true),
        (true, false, true),
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
        let run = fixture.enqueue("Test interrupted controller output").await;
        let run_id = text(&run, "id").to_owned();
        let wait_returned = Arc::new(AtomicBool::new(false));
        let app = Router::new()
            .fallback(any({
                let store = fixture.service.store.clone();
                let id = run_id.clone();
                let wait_returned = wait_returned.clone();
                move |state: State<Arc<Controller>>, request: Request| {
                    let store = store.clone();
                    let id = id.clone();
                    let wait_returned = wait_returned.clone();
                    async move {
                        if request.uri().path().ends_with("/wait") {
                            wait_returned.store(true, Ordering::SeqCst);
                            return Json(json!({ "StatusCode": 0 })).into_response();
                        }
                        if request.uri().path().ends_with("/logs") {
                            let mut output = format!(
                                "{}\n",
                                json!({
                                    "type": "thread.started",
                                    "thread_id": "fixture-session",
                                })
                            );
                            if completed {
                                output.push_str(&format!(
                                    "{}\n{}\n",
                                    json!({
                                        "type": "item.completed",
                                        "item": {
                                            "id": "reply",
                                            "type": "agent_message",
                                            "text": "committed answer",
                                        },
                                    }),
                                    json!({ "type": "turn.completed", "usage": {} })
                                ));
                            }
                            let frame = format!(
                                "{}\n",
                                json!({
                                    "type": "output",
                                    "stderr": false,
                                    "data": STANDARD.encode(output),
                                })
                            );
                            let stream = futures_util::stream::unfold(
                                (0, store, id, wait_returned, frame),
                                move |(stage, store, id, wait_returned, frame)| async move {
                                    if stage == 0 {
                                        let bytes = axum::body::Bytes::from(frame.clone());
                                        return Some((
                                            Ok::<_, std::io::Error>(bytes),
                                            (1, store, id, wait_returned, frame),
                                        ));
                                    }
                                    if stage == 1 && clean_eof {
                                        return None;
                                    }
                                    if stage == 1 && truncated {
                                        loop {
                                            let completion_committed = !completed
                                                || store
                                                    .checkpoint_boundary(&id)
                                                    .await
                                                    .unwrap()
                                                    .is_some_and(|saved| {
                                                        saved["completed"] == true
                                                    });
                                            if wait_returned.load(Ordering::SeqCst)
                                                && completion_committed
                                            {
                                                break;
                                            }
                                            tokio::time::sleep(Duration::from_millis(10)).await;
                                        }
                                        // Let the immediate /wait response reach the client
                                        // before cutting the next output frame.
                                        tokio::time::sleep(Duration::from_millis(250)).await;
                                        return Some((
                                            Ok(axum::body::Bytes::from_static(b"{\"type\":")),
                                            (2, store, id, wait_returned, frame),
                                        ));
                                    }
                                    if stage == 1 {
                                        return std::future::pending().await;
                                    }
                                    None
                                },
                            );
                            return Body::from_stream(stream).into_response();
                        }
                        serve_controller(state, request).await
                    }
                }
            }))
            .with_state(controller.clone());
        let (listener, address) = common::bind().await;
        common::reconfigure(&mut fixture.service, |config| {
            config.runner_url = format!("http://{address}");
        })
        .await;
        let server = common::serve(listener, app);
        fixture.start().await;
        let result = fixture.until_finished(&run_id).await;
        assert_eq!(
            result["status"],
            if completed {
                RunStatus::Succeeded
            } else {
                RunStatus::Failed
            },
            "completed={completed}, truncated={truncated}: {result}"
        );
        let saved = fixture.checkpoint(&run_id).await;
        assert_eq!(saved["terminalCode"], 0);
        if clean_eof {
            assert!(saved["outputInterrupted"].is_null());
        } else {
            assert_eq!(saved["outputInterrupted"], true);
        }
        assert_eq!(saved["completed"], completed);
        if completed && !clean_eof {
            let id = run_id.clone();
            let events = fixture
                .service
                .store
                .read(move |db| db.events_fixture(&id, 0, 1000))
                .await
                .unwrap();
            let notices = events
                .iter()
                .filter(|event| text(event, "text").contains("Execution output was interrupted"))
                .count();
            assert_eq!(notices, 1, "{events:?}");
            assert!(
                text(&result, "summary").contains("committed answer"),
                "{result}"
            );
        }
        if !completed {
            let expected_error = if clean_eof {
                "completion was recorded"
            } else {
                "output was interrupted"
            };
            assert!(text(&result, "error").contains(expected_error), "{result}");
        }
        assert_eq!(
            controller.plans.lock().await.len(),
            1,
            "must not replay the turn"
        );
        fixture.stop(false).await;
        server.abort();
    }
}

/// Hold the SQLite writer after the manager reserves an observation and before
/// its healthy response can be committed. The live deadline must still win.
#[tokio::test]
async fn active_observation_expiring_during_result_persistence_fails_without_replay() {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    let mut fixture = Fixture::new().await;
    fixture.stop(false).await;
    let controller = Arc::new(Controller {
        data: fixture.service.config.data_dir.clone(),
        plans: tokio::sync::Mutex::new(Vec::new()),
        failures: 0,
        slots: 4,
        hold: AtomicBool::new(false),
    });
    let waits = Arc::new(AtomicUsize::new(0));
    let response_gate = Arc::new(tokio::sync::Semaphore::new(0));
    let app = Router::new()
        .fallback(any({
            let waits = waits.clone();
            let gate = response_gate.clone();
            move |state: State<Arc<Controller>>, request: Request| {
                let waits = waits.clone();
                let gate = gate.clone();
                async move {
                    if request.uri().path().ends_with("/wait") {
                        let index = waits.fetch_add(1, Ordering::SeqCst);
                        if index == 0 {
                            return Json(json!({ "StatusCode": 75 })).into_response();
                        }
                        gate.acquire().await.unwrap().forget();
                        return Json(json!({ "StatusCode": 0 })).into_response();
                    }
                    serve_controller(state, request).await
                }
            }
        }))
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
        .enqueue("Test expiry during controller result persistence")
        .await;
    let run_id = text(&run, "id").to_owned();
    fixture
        .service
        .store
        .save_checkpoint_boundary(&run_id, json!({ "remainingMs": 15000 }))
        .await
        .unwrap();
    fixture.start().await;
    let saved = eventually(
        Duration::from_secs(12),
        Duration::from_millis(10),
        async || {
            let saved = fixture.checkpoint(&run_id).await;
            (waits.load(Ordering::SeqCst) >= 2
                && saved["completed"] == true
                && saved["controllerObservation"]["inFlight"] == true)
                .then_some(saved)
        },
    )
    .await;
    let remaining = saved["remainingMs"].as_u64().unwrap();
    assert!((300..=15000).contains(&remaining), "{saved}");
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let store = fixture.service.store.clone();
    let blocker = tokio::spawn(async move {
        store
            .transaction(move |_| {
                let _ = entered_tx.send(());
                release_rx.recv().unwrap();
                Ok(())
            })
            .await
            .unwrap();
    });
    entered_rx.await.unwrap();
    response_gate.add_permits(1);
    tokio::time::sleep(Duration::from_millis(remaining + 250)).await;
    release_tx.send(()).unwrap();
    blocker.await.unwrap();
    let result = fixture.until_finished(&run_id).await;
    assert_eq!(result["status"], RunStatus::Failed, "{result}");
    assert!(text(&result, "error").contains("time limit"), "{result}");
    let saved = fixture.checkpoint(&run_id).await;
    assert_eq!(saved["terminalCode"], 0, "{saved}");
    assert_eq!(saved["remainingMs"], 0, "{saved}");
    assert_eq!(
        controller.plans.lock().await.len(),
        1,
        "must not replay the turn"
    );
    fixture.stop(false).await;
    server.abort();
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
                            if interruption == "client-error" && observation == 0 {
                                // The initial wait fails decoding. Only the manager's
                                // durably reserved observation may read again.
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
            kill_fixture_runner_child(&saved).await;
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
            assert_eq!(waits.load(Ordering::SeqCst), 2);
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
async fn managed_provider_retries_do_not_spend_controller_recoveries() {
    for ending in ["exhausted", "changed", "cancel"] {
        let mut fixture = Fixture::new().await;
        fixture.stop(false).await;
        let controller = Arc::new(Controller {
            data: fixture.service.config.data_dir.clone(),
            plans: tokio::sync::Mutex::new(Vec::new()),
            failures: usize::MAX,
            slots: 4,
            hold: std::sync::atomic::AtomicBool::new(false),
        });
        let app = Router::new().fallback(any(move |state: State<Arc<Controller>>, request: Request| async move {
            let path = request.uri().path().to_owned();
            if path.ends_with("/logs") {
                let attempt = path.split('/').nth(2).unwrap();
                let index = state.attempt_index(attempt).await;
                let error = if ending == "changed" && index >= 1 {
                    json!({ "code": "authentication_error", "message": "Reconnect account" })
                } else {
                    json!({ "code": "server_overloaded", "message": "Provider busy" })
                };
                let output = format!("{}\n{}\n", json!({ "type": "thread.started", "thread_id": "fixture-session" }),
                                     json!({ "type": "turn.failed", "error": error }));
                return Body::from(format!("{}\n", json!({ "type": "output", "stderr": false, "data": STANDARD.encode(output) }))).into_response();
            }
            if path.ends_with("/wait") {
                return Json(json!({ "StatusCode": 1 })).into_response();
            }
            serve_controller(state, request).await
        })).with_state(controller.clone());
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
        let chat = fixture
            .start_chat(message("Exercise a confirmed provider retry"))
            .await;
        let run_id = fixture.chat_run(&chat).await;
        let attempts = if ending == "exhausted" { 5 } else { 1 };
        for retry_number in 1..=attempts {
            let waiting = fixture
                .until(&run_id, |run| {
                    (run["status"] == RunStatus::Queued && run["retry"]["attempt"] == retry_number)
                        || finished(run)
                })
                .await;
            assert_eq!(waiting["status"], RunStatus::Queued, "{waiting}");
            let saved = fixture.checkpoint(&run_id).await;
            assert_eq!(saved["prepared"]["backend"], "firecracker");
            assert_eq!(
                saved["controllerRecoveries"].as_u64().unwrap_or(0),
                0,
                "{saved}"
            );
            if ending == "cancel" {
                fixture
                    .service
                    .worker
                    .cancel(&fixture.service, &run_id)
                    .await
                    .unwrap();
                let stopped = fixture
                    .until(&run_id, |run| run["status"] == RunStatus::Cancelled)
                    .await;
                assert_eq!(stopped["status"], RunStatus::Cancelled);
                break;
            }
            let mut retry = waiting["retry"].clone();
            retry["nextAttemptAt"] = now().into();
            fixture
                .service
                .store
                .change_run(
                    &run_id,
                    cairn_installation::runs::RunChanges {
                        retry: Some(Some(serde_json::from_value(retry).unwrap())),
                        ..Default::default()
                    },
                )
                .await
                .unwrap();
        }
        if ending != "cancel" {
            let failed = fixture.until_finished(&run_id).await;
            assert_eq!(failed["status"], RunStatus::Failed, "{failed}");
            assert_eq!(failed["retry"]["attempt"], attempts);
            assert_eq!(
                failed["summary"],
                if ending == "changed" {
                    "Reconnect account"
                } else {
                    "Provider busy"
                }
            );
        }
        assert_eq!(
            controller.plans.lock().await.len(),
            if ending == "exhausted" {
                6
            } else if ending == "changed" {
                2
            } else {
                1
            }
        );
        fixture.stop(false).await;
        server.abort();
    }
}

#[tokio::test]
async fn confirmed_provider_failure_survives_later_output_truncation_and_restart() {
    use std::sync::atomic::AtomicBool;
    for terminal in [1, 70] {
        let mut fixture = Fixture::new().await;
        fixture.stop(false).await;
        let controller = Arc::new(Controller {
            data: fixture.service.config.data_dir.clone(),
            plans: tokio::sync::Mutex::new(Vec::new()),
            failures: 0,
            slots: 4,
            hold: AtomicBool::new(false),
        });
        let app = Router::new().fallback(any(move |state: State<Arc<Controller>>, request: Request| async move {
        let path = request.uri().path().to_owned();
        if path.ends_with("/logs") || path.ends_with("/wait") {
            let attempt = path.split('/').nth(2).unwrap();
            if state.attempt_index(attempt).await == 0 {
                if path.ends_with("/wait") {
                    return Json(json!({ "StatusCode": terminal })).into_response();
                }
                let mut output = format!("{}\n{}\n",
                    json!({ "type": "thread.started", "thread_id": "fixture-session" }),
                    json!({ "type": "turn.failed", "error": { "code": "server_overloaded", "message": "Provider busy" } }));
                if terminal == 70 {
                    output.push_str(&format!("{}\n", json!({ "type": "runner.failed", "error": { "message": "Finalization failed: EIO/EROFS" } })));
                }
                let frame = json!({ "type": "output", "stderr": false, "data": STANDARD.encode(output) });
                return Body::from(format!("{frame}\n{{\"type\":" )).into_response();
            }
        }
        serve_controller(state, request).await
    })).with_state(controller.clone());
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
        let chat = fixture
            .start_chat(message("Retry after a confirmed provider failure"))
            .await;
        let run_id = fixture.chat_run(&chat).await;
        let waiting = fixture
            .until(&run_id, |run| {
                finished(run)
                    || (run["status"] == RunStatus::Queued && run["retry"]["attempt"] == 1)
            })
            .await;
        if terminal == 70 {
            assert_eq!(waiting["status"], RunStatus::Failed, "{waiting}");
            assert!(waiting["retry"].is_null());
            assert_eq!(controller.plans.lock().await.len(), 1);
            let saved = fixture.checkpoint(&run_id).await;
            assert_eq!(saved["terminalCode"], 70);
            assert_eq!(saved["outputInterrupted"], true);
            fixture.stop(false).await;
            server.abort();
            continue;
        }
        assert_eq!(waiting["status"], RunStatus::Queued, "{waiting}");
        assert_eq!(waiting["retry"]["cause"]["message"], "Provider busy");
        fixture.stop(false).await;
        let mut saved = fixture.checkpoint(&run_id).await;
        assert_eq!(saved["outputInterrupted"], true);
        assert_eq!(saved["retryCause"]["message"], "Provider busy");
        assert_eq!(saved["providerRetryReservation"], saved["runnerId"]);
        assert_eq!(saved["controllerRecoveries"].as_u64().unwrap_or(0), 0);
        // Model a crash after the observed failure and broken output were committed,
        // but before record_failure paid for the retry. Recovery must fence and pay it.
        saved
            .as_object_mut()
            .unwrap()
            .remove("providerRetryReservation");
        fixture
            .service
            .store
            .save_checkpoint_boundary(&run_id, saved)
            .await
            .unwrap();
        fixture
            .service
            .store
            .change_run(
                &run_id,
                cairn_installation::runs::RunChanges {
                    retry: Some(None),
                    recovery_pending: Some(true),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        fixture.start().await;
        let waiting = fixture
            .until(&run_id, |run| {
                finished(run)
                    || (run["status"] == RunStatus::Queued && run["retry"]["attempt"] == 1)
            })
            .await;
        assert_eq!(waiting["status"], RunStatus::Queued, "{waiting}");
        fixture.stop(false).await;
        let recovered = fixture.checkpoint(&run_id).await;
        assert_eq!(recovered["providerRetryReservation"], recovered["runnerId"]);
        assert_eq!(recovered["controllerRecoveries"].as_u64().unwrap_or(0), 0);
        let mut retry = waiting["retry"].clone();
        retry["nextAttemptAt"] = now().into();
        fixture
            .service
            .store
            .change_run(
                &run_id,
                cairn_installation::runs::RunChanges {
                    retry: Some(Some(serde_json::from_value(retry).unwrap())),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        fixture.start().await;
        let result = fixture.until_finished(&run_id).await;
        assert_eq!(result["status"], RunStatus::Succeeded, "{result}");
        assert_eq!(controller.plans.lock().await.len(), 2);
        let id = run_id.clone();
        let events = fixture
            .service
            .store
            .read(move |db| db.events_fixture(&id, 0, 1000))
            .await
            .unwrap();
        assert_eq!(
            events
                .iter()
                .filter(|event| text(event, "text").contains("Execution output was interrupted"))
                .count(),
            1
        );
        fixture.stop(false).await;
        server.abort();
    }
}

#[tokio::test]
async fn native_failed_flush_after_provider_failure_never_replays_with_intact_logs() {
    use std::sync::atomic::AtomicBool;
    let mut fixture = Fixture::new().await;
    fixture.stop(false).await;
    let controller = Arc::new(Controller {
        data: fixture.service.config.data_dir.clone(),
        plans: tokio::sync::Mutex::new(Vec::new()),
        failures: 0,
        slots: 4,
        hold: AtomicBool::new(false),
    });
    let app = Router::new().fallback(any(move |state: State<Arc<Controller>>, request: Request| async move {
        let path = request.uri().path();
        if path.ends_with("/wait") { return Json(json!({ "StatusCode": 70, "FinalizationFailed": true })).into_response(); }
        if path.ends_with("/logs") {
            let output = format!("{}\n{}\n", json!({ "type": "thread.started", "thread_id": "fixture-session" }), json!({ "type": "turn.failed", "error": { "code": "server_overloaded", "message": "Provider busy" } }));
            return Body::from(format!("{}\n", json!({ "type": "output", "stderr": false, "data": STANDARD.encode(output) }))).into_response();
        }
        serve_controller(state, request).await
    })).with_state(controller.clone());
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
    let chat = fixture
        .start_chat(message("Do not replay a failed flush"))
        .await;
    let id = fixture.chat_run(&chat).await;
    let result = fixture
        .until(&id, |run| {
            finished(run)
                || controller
                    .plans
                    .try_lock()
                    .is_ok_and(|plans| plans.len() > 1)
        })
        .await;
    assert_eq!(result["status"], RunStatus::Failed, "{result}");
    assert_eq!(controller.plans.lock().await.len(), 1);
    assert!(text(&result, "error").contains("flush"), "{result}");
    let saved = fixture.checkpoint(&id).await;
    assert!(saved["outputInterrupted"].is_null());
    assert_eq!(saved["finalizationFailureAttempt"], saved["runnerId"]);
    let events = fixture
        .service
        .store
        .read(move |db| db.events_fixture(&id, 0, 1000))
        .await
        .unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|event| text(event, "text").contains("filesystem could not be flushed"))
            .count(),
        1
    );
    fixture.stop(false).await;
    server.abort();
}

/// Exercises the real manager -> runner client -> disposable HTTP controller.
/// Even at the replay ceiling a read of the same attempt is permitted.
#[tokio::test]
async fn controller_evidence_is_reconciled_before_replay_or_terminal_failure_reconciliation() {
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

/// The real runner client and manager must retain a broken output stream even
/// when the controller reports a healthy physical completion.
#[tokio::test]
async fn interrupted_controller_output_after_healthy_exit_never_replays_the_turn_reconciliation() {
    use std::sync::atomic::{AtomicBool, Ordering};

    for (completed, truncated, clean_eof) in [
        (false, true, false),
        (true, true, false),
        (false, false, false),
        (true, false, false),
        (false, false, true),
        (true, false, true),
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
        let run = fixture.enqueue("Test interrupted controller output").await;
        let run_id = text(&run, "id").to_owned();
        let wait_returned = Arc::new(AtomicBool::new(false));
        let app = Router::new()
            .fallback(any({
                let store = fixture.service.store.clone();
                let id = run_id.clone();
                let wait_returned = wait_returned.clone();
                move |state: State<Arc<Controller>>, request: Request| {
                    let store = store.clone();
                    let id = id.clone();
                    let wait_returned = wait_returned.clone();
                    async move {
                        if request.uri().path().ends_with("/wait") {
                            wait_returned.store(true, Ordering::SeqCst);
                            return Json(json!({ "StatusCode": 0 })).into_response();
                        }
                        if request.uri().path().ends_with("/logs") {
                            let mut output = format!(
                                "{}\n",
                                json!({
                                    "type": "thread.started",
                                    "thread_id": "fixture-session",
                                })
                            );
                            if completed {
                                output.push_str(&format!(
                                    "{}\n{}\n",
                                    json!({
                                        "type": "item.completed",
                                        "item": {
                                            "id": "reply",
                                            "type": "agent_message",
                                            "text": "committed answer",
                                        },
                                    }),
                                    json!({ "type": "turn.completed", "usage": {} })
                                ));
                            }
                            let frame = format!(
                                "{}\n",
                                json!({
                                    "type": "output",
                                    "stderr": false,
                                    "data": STANDARD.encode(output),
                                })
                            );
                            let stream = futures_util::stream::unfold(
                                (0, store, id, wait_returned, frame),
                                move |(stage, store, id, wait_returned, frame)| async move {
                                    if stage == 0 {
                                        let bytes = axum::body::Bytes::from(frame.clone());
                                        return Some((
                                            Ok::<_, std::io::Error>(bytes),
                                            (1, store, id, wait_returned, frame),
                                        ));
                                    }
                                    if stage == 1 && clean_eof {
                                        return None;
                                    }
                                    if stage == 1 && truncated {
                                        loop {
                                            let completion_committed = !completed
                                                || store
                                                    .checkpoint_boundary(&id)
                                                    .await
                                                    .unwrap()
                                                    .is_some_and(|saved| {
                                                        saved["completed"] == true
                                                    });
                                            if wait_returned.load(Ordering::SeqCst)
                                                && completion_committed
                                            {
                                                break;
                                            }
                                            tokio::time::sleep(Duration::from_millis(10)).await;
                                        }
                                        // Let the immediate /wait response reach the client
                                        // before cutting the next output frame.
                                        tokio::time::sleep(Duration::from_millis(250)).await;
                                        return Some((
                                            Ok(axum::body::Bytes::from_static(b"{\"type\":")),
                                            (2, store, id, wait_returned, frame),
                                        ));
                                    }
                                    if stage == 1 {
                                        return std::future::pending().await;
                                    }
                                    None
                                },
                            );
                            return Body::from_stream(stream).into_response();
                        }
                        serve_controller(state, request).await
                    }
                }
            }))
            .with_state(controller.clone());
        let (listener, address) = common::bind().await;
        common::reconfigure(&mut fixture.service, |config| {
            config.runner_url = format!("http://{address}");
        })
        .await;
        let server = common::serve(listener, app);
        fixture.start().await;
        let result = fixture.until_finished(&run_id).await;
        assert_eq!(
            result["status"],
            if completed {
                RunStatus::Succeeded
            } else {
                RunStatus::Failed
            },
            "completed={completed}, truncated={truncated}: {result}"
        );
        let saved = fixture.checkpoint(&run_id).await;
        assert_eq!(saved["terminalCode"], 0);
        if clean_eof {
            assert!(saved["outputInterrupted"].is_null());
        } else {
            assert_eq!(saved["outputInterrupted"], true);
        }
        assert_eq!(saved["completed"], completed);
        if completed && !clean_eof {
            let id = run_id.clone();
            let events = fixture
                .service
                .store
                .read(move |db| db.events_fixture(&id, 0, 1000))
                .await
                .unwrap();
            let notices = events
                .iter()
                .filter(|event| text(event, "text").contains("Execution output was interrupted"))
                .count();
            assert_eq!(notices, 1, "{events:?}");
            assert!(
                text(&result, "summary").contains("committed answer"),
                "{result}"
            );
        }
        if !completed {
            let expected_error = if clean_eof {
                "completion was recorded"
            } else {
                "output was interrupted"
            };
            assert!(text(&result, "error").contains(expected_error), "{result}");
        }
        assert_eq!(
            controller.plans.lock().await.len(),
            1,
            "must not replay the turn"
        );
        fixture.stop(false).await;
        server.abort();
    }
}

/// Hold the SQLite writer after the manager reserves an observation and before
/// its healthy response can be committed. The live deadline must still win.
#[tokio::test]
async fn active_observation_expiring_during_result_persistence_fails_without_replay_reconciliation()
{
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    let mut fixture = Fixture::new().await;
    fixture.stop(false).await;
    let controller = Arc::new(Controller {
        data: fixture.service.config.data_dir.clone(),
        plans: tokio::sync::Mutex::new(Vec::new()),
        failures: 0,
        slots: 4,
        hold: AtomicBool::new(false),
    });
    let waits = Arc::new(AtomicUsize::new(0));
    let response_gate = Arc::new(tokio::sync::Semaphore::new(0));
    let app = Router::new()
        .fallback(any({
            let waits = waits.clone();
            let gate = response_gate.clone();
            move |state: State<Arc<Controller>>, request: Request| {
                let waits = waits.clone();
                let gate = gate.clone();
                async move {
                    if request.uri().path().ends_with("/wait") {
                        let index = waits.fetch_add(1, Ordering::SeqCst);
                        if index == 0 {
                            return Json(json!({ "StatusCode": 75 })).into_response();
                        }
                        gate.acquire().await.unwrap().forget();
                        return Json(json!({ "StatusCode": 0 })).into_response();
                    }
                    serve_controller(state, request).await
                }
            }
        }))
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
        .enqueue("Test expiry during controller result persistence")
        .await;
    let run_id = text(&run, "id").to_owned();
    fixture
        .service
        .store
        .save_checkpoint_boundary(&run_id, json!({ "remainingMs": 15000 }))
        .await
        .unwrap();
    fixture.start().await;
    let saved = eventually(
        Duration::from_secs(12),
        Duration::from_millis(10),
        async || {
            let saved = fixture.checkpoint(&run_id).await;
            (waits.load(Ordering::SeqCst) >= 2
                && saved["completed"] == true
                && saved["controllerObservation"]["inFlight"] == true)
                .then_some(saved)
        },
    )
    .await;
    let remaining = saved["remainingMs"].as_u64().unwrap();
    assert!((300..=15000).contains(&remaining), "{saved}");
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let store = fixture.service.store.clone();
    let blocker = tokio::spawn(async move {
        store
            .transaction(move |_| {
                let _ = entered_tx.send(());
                release_rx.recv().unwrap();
                Ok(())
            })
            .await
            .unwrap();
    });
    entered_rx.await.unwrap();
    response_gate.add_permits(1);
    tokio::time::sleep(Duration::from_millis(remaining + 250)).await;
    release_tx.send(()).unwrap();
    blocker.await.unwrap();
    let result = fixture.until_finished(&run_id).await;
    assert_eq!(result["status"], RunStatus::Failed, "{result}");
    assert!(text(&result, "error").contains("time limit"), "{result}");
    let saved = fixture.checkpoint(&run_id).await;
    assert_eq!(saved["terminalCode"], 0, "{saved}");
    assert_eq!(saved["remainingMs"], 0, "{saved}");
    assert_eq!(
        controller.plans.lock().await.len(),
        1,
        "must not replay the turn"
    );
    fixture.stop(false).await;
    server.abort();
}

/// Manager SIGTERM is a local stop, not a controller completion observation.
/// A recorded turn must retain the original attempt for reconciliation on restart.
#[tokio::test]
async fn manager_shutdown_between_turn_completion_and_wait_preserves_same_attempt_evidence_reconciliation()
 {
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
                            if interruption == "client-error" && observation == 0 {
                                // The initial wait fails decoding. Only the manager's
                                // durably reserved observation may read again.
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
            kill_fixture_runner_child(&saved).await;
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
            assert_eq!(waits.load(Ordering::SeqCst), 2);
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
async fn manual_resume_cancels_a_staged_switch_during_worker_fencing() {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    let mut fixture = Fixture::new().await;
    fixture.stop(false).await;
    let controller = Arc::new(Controller {
        data: fixture.service.config.data_dir.clone(),
        plans: tokio::sync::Mutex::new(Vec::new()),
        failures: 0,
        slots: 4,
        hold: AtomicBool::new(true),
    });
    let block_fence = Arc::new(AtomicBool::new(false));
    let fenced_attempt = Arc::new(std::sync::Mutex::new(None::<String>));
    let blocked_delete_count = Arc::new(AtomicUsize::new(0));
    let fence_entered = Arc::new(tokio::sync::Notify::new());
    let fence_release = Arc::new(tokio::sync::Notify::new());
    let app =
        Router::new()
            .fallback(any({
                let (
                    block_fence,
                    fenced_attempt,
                    blocked_delete_count,
                    fence_entered,
                    fence_release,
                ) = (
                    block_fence.clone(),
                    fenced_attempt.clone(),
                    blocked_delete_count.clone(),
                    fence_entered.clone(),
                    fence_release.clone(),
                );
                move |state: State<Arc<Controller>>, request: Request| {
                    let (
                        block_fence,
                        fenced_attempt,
                        blocked_delete_count,
                        fence_entered,
                        fence_release,
                    ) = (
                        block_fence.clone(),
                        fenced_attempt.clone(),
                        blocked_delete_count.clone(),
                        fence_entered.clone(),
                        fence_release.clone(),
                    );
                    async move {
                        let attempt_to_fence = fenced_attempt.lock().unwrap().clone();
                        let is_target_attempt =
                            request.uri().path().strip_prefix("/runs/").is_some_and(
                                |request_attempt| {
                                    attempt_to_fence.as_deref() == Some(request_attempt)
                                },
                            );
                        if request.method() == axum::http::Method::DELETE
                            && is_target_attempt
                            && block_fence.swap(false, Ordering::SeqCst)
                        {
                            blocked_delete_count.fetch_add(1, Ordering::SeqCst);
                            fence_entered.notify_one();
                            fence_release.notified().await;
                        }
                        serve_controller(state, request).await
                    }
                }
            }))
            .with_state(controller.clone());
    let (listener, address) = common::bind().await;
    common::reconfigure(&mut fixture.service, |config| {
        config.runner_url = format!("http://{address}");
    })
    .await;
    let s = fixture.service.clone();
    let server = common::serve(listener, app);
    let phase = |name: &'static str| {
        eprintln!(
            "CAIRN_RESUME_TEST_PHASE test=manual_resume_cancels_a_staged_switch_during_worker_fencing phase={name} blockedDeleteCount={}",
            blocked_delete_count.load(Ordering::SeqCst).min(16)
        );
    };
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
    cairn_installation::nodes::refresh_local(&s).await.unwrap();
    s.store
        .set(
            "codex-models:",
            json!({
                "checkedAt": now(),
                "models": [
                    {
                        "model": "gpt-6-sol",
                        "isDefault": true,
                        "supportedReasoningEfforts": [{ "reasoningEffort": "high" }],
                        "serviceTiers": [{ "id": "priority" }],
                    },
                    {
                        "model": "gpt-6-luna",
                        "isDefault": false,
                        "supportedReasoningEfforts": [{ "reasoningEffort": "xhigh" }],
                        "serviceTiers": [{ "id": "priority" }],
                    },
                ],
            }),
            None,
        )
        .await
        .unwrap();

    let chat = s.chat_create(json!({})).await.unwrap();
    let chat_id = text(&chat, "id").to_owned();
    let mut sent = message("fixture:chat-hang");
    sent["provider"] = "codex".into();
    sent["model"] = "gpt-6-sol".into();
    sent["reasoning"] = "high".into();
    sent["speed"] = "priority".into();
    let turn_id = text(&sent, "id").to_owned();
    s.chat_send(&chat_id, sent).await.unwrap();
    fixture.start().await;
    phase("chat_run_lookup_before");
    let run_id = fixture.chat_run(&chat_id).await;
    phase("chat_run_lookup_after");
    phase("initial_run_wait_before");
    let active = fixture
        .until(&run_id, |run| {
            run["status"] == RunStatus::Running && run["sessionId"].is_string()
        })
        .await;
    phase("initial_run_wait_after");
    phase("initial_controller_plan_wait_before");
    eventually(
        Duration::from_secs(20),
        Duration::from_millis(30),
        async || {
            controller
                .plans
                .lock()
                .await
                .iter()
                .any(|plan| plan["runId"] == run_id)
                .then_some(())
        },
    )
    .await;
    phase("initial_controller_plan_wait_after");
    let initial_attempt = controller
        .plans
        .lock()
        .await
        .iter()
        .find(|plan| plan["runId"] == run_id)
        .and_then(|plan| plan["id"].as_str())
        .expect("the initial run must have a controller attempt")
        .to_owned();
    *fenced_attempt.lock().unwrap() = Some(initial_attempt);
    let relay = common::relay_fixture::context(&s).await;
    let relay_token = text(&relay, "value");
    let client = reqwest::Client::new();
    let stopped = client
        .post(format!("{}/api/chats/{chat_id}/stop", fixture.url))
        .header("x-test-relay-token", relay_token)
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    stopped.error_for_status().unwrap();
    phase("stopped_run_wait_before");
    let terminal = fixture
        .until(&run_id, |run| {
            run["status"] == RunStatus::Cancelled && run["resumeAvailable"] == true
        })
        .await;
    phase("stopped_run_wait_after");
    assert_eq!(terminal["sessionId"], active["sessionId"]);
    fixture.stop(false).await;
    s.store
        .change_run(
            &run_id,
            cairn_installation::runs::RunChanges {
                status: Some(RunStatus::Failed),
                finished_at: Some(Some(now())),
                error: Some(Some("Usage limit has been reached".into())),
                cancel_requested_at: Some(None),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let failed_quota_run = s.store.run(&run_id).await.unwrap();
    assert_eq!(failed_quota_run["status"], RunStatus::Failed);
    assert_eq!(failed_quota_run["cancelRequestedAt"], Value::Null);
    assert!(
        failed_quota_run["error"]
            .as_str()
            .unwrap()
            .contains("Usage limit")
    );

    let mut alternative = serde_json::to_value(
        s.accounts
            .create(&s, Provider::Claude, "Switch target")
            .await
            .unwrap(),
    )
    .unwrap();
    let alternative_id = text(&alternative, "id").to_owned();
    alternative["state"] = "ready".into();
    s.store.save_account_fixture(alternative).await.unwrap();
    let claude_home = accounts::claude::account_home(&s.config, &alternative_id);
    std::fs::create_dir_all(&claude_home).unwrap();
    std::fs::write(
        claude_home.join(".credentials.json"),
        json!({
            "claudeAiOauth": {
                "accessToken": "fixture-access",
                "expiresAt": now() + 3_600_000,
                "scopes": ["user:inference"],
            },
        })
        .to_string(),
    )
    .unwrap();
    let api = common::relay_fixture::router(s.clone()).await.unwrap();
    let switch_id = id();
    let switch = json!({
        "runId": run_id,
        "turnId": turn_id,
        "requestId": switch_id,
        "provider": "claude",
    });
    let (stage_status, stage_response) = api_post(
        &api,
        relay_token,
        &format!("/api/chats/{chat_id}/coding-agent"),
        switch,
    )
    .await;
    assert_eq!(stage_status, StatusCode::OK, "{stage_response}");
    assert!(
        s.store
            .read({
                let chat_id = chat_id.clone();
                move |db| Ok(db.engine_switch(&chat_id)?.unwrap().pending)
            })
            .await
            .unwrap()
    );

    block_fence.store(true, Ordering::SeqCst);
    controller
        .hold
        .store(false, std::sync::atomic::Ordering::Relaxed);
    fixture.start().await;
    phase("first_fence_wait_before");
    tokio::time::timeout(Duration::from_secs(10), fence_entered.notified())
        .await
        .expect("the production switch worker should reach runner fencing");
    phase("first_fence_wait_after");

    let cancelled = client
        .post(format!("{}/api/chats/{chat_id}/stop", fixture.url))
        .header("x-test-relay-token", relay_token)
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    cancelled.error_for_status().unwrap();
    assert!(
        !s.store
            .read({
                let chat_id = chat_id.clone();
                move |db| Ok(db.engine_switch(&chat_id)?.unwrap().pending)
            })
            .await
            .unwrap()
    );

    fence_release.notify_one();
    let resumed = client
        .post(format!("{}/api/chats/{chat_id}/resume", fixture.url))
        .header("x-test-relay-token", relay_token)
        .json(&json!({
            "runId": run_id,
            "turnId": turn_id,
            "selection": {
                "model": "gpt-6-luna",
                "reasoning": "xhigh",
                "speed": "",
            },
        }))
        .send()
        .await
        .unwrap();
    assert!(resumed.status().is_success());
    phase("resumed_execution_wait_before");
    let completed = fixture.until_finished(&run_id).await;
    phase("resumed_execution_wait_after");
    assert_eq!(blocked_delete_count.load(Ordering::SeqCst), 1);
    assert_eq!(completed["status"], RunStatus::Succeeded, "{completed}");
    assert_eq!(completed["snapshot"]["agent"]["provider"], "codex");
    assert_eq!(completed["snapshot"]["agent"]["speed"], "priority");
    assert_eq!(completed["chatExecution"]["messageId"], turn_id);
    assert_eq!(completed["sessionId"], terminal["sessionId"]);
    let resumed_plan = controller
        .plans
        .lock()
        .await
        .iter()
        .find(|plan| {
            plan["runId"] == run_id
                && plan["chat"]["execution"]["messageId"] == turn_id
                && plan["chat"]["model"] == "gpt-6-luna"
        })
        .cloned()
        .expect("the selected resume must launch after the stale switch is fenced");
    assert_eq!(resumed_plan["chat"]["provider"], "codex");
    assert_eq!(resumed_plan["chat"]["serviceTier"], "priority");
    assert!(controller.plans.lock().await.iter().all(|plan| {
        plan["runId"] != run_id
            || plan["chat"]["execution"]["messageId"] != turn_id
            || plan["chat"]["provider"] != "claude"
    }));
    let receipt = s
        .store
        .read({
            let chat_id = chat_id.clone();
            move |db| db.engine_switch(&chat_id)
        })
        .await
        .unwrap()
        .unwrap();
    assert_eq!(receipt.request_id, switch_id);
    assert!(!receipt.pending);

    fixture.stop(false).await;
    server.abort();
}

#[tokio::test]
async fn chat_resume_selection_resolves_opencode_catalog_default_before_adapter_validation() {
    use std::sync::atomic::{AtomicBool, Ordering};

    let mut fixture = Fixture::with_opencode().await;
    fixture.stop(false).await;
    let data_dir = fixture.service.config.data_dir.clone();
    let controller = Arc::new(Controller {
        data: data_dir,
        plans: tokio::sync::Mutex::new(Vec::new()),
        failures: 0,
        slots: 4,
        hold: AtomicBool::new(true),
    });
    let log_request_seen = Arc::new(AtomicBool::new(false));
    let plan_received = Arc::new(AtomicBool::new(false));
    let app = Router::new()
        .fallback(any({
            let log_request_seen = log_request_seen.clone();
            let plan_received = plan_received.clone();
            move |state: State<Arc<Controller>>, request: Request| {
                let log_request_seen = log_request_seen.clone();
                let plan_received = plan_received.clone();
                async move {
                    let path = request.uri().path();
                    if path.starts_with("/runs/") && path.ends_with("/logs") {
                        log_request_seen.store(true, Ordering::Relaxed);
                    }
                    let is_plan_post = request.method() == axum::http::Method::POST
                        && path
                            .strip_prefix("/runs/")
                            .is_some_and(|attempt| !attempt.is_empty() && !attempt.contains('/'));
                    let response = serve_controller(state, request).await;
                    if is_plan_post && response.status().is_success() {
                        plan_received.store(true, Ordering::Relaxed);
                    }
                    response
                }
            }
        }))
        .with_state(controller.clone());
    let (listener, address) = common::bind().await;
    common::reconfigure(&mut fixture.service, |config| {
        config.runner_url = format!("http://{address}");
    })
    .await;
    let s = fixture.service.clone();
    let server = common::serve(listener, app);
    tokio::fs::write(
        s.config.data_dir.join("storage-s3.json"),
        json!({ "bucket": "fixture-storage", "endpoint": "https://127.0.0.1:1" }).to_string(),
    )
    .await
    .unwrap();
    let account = s
        .accounts
        .connect_api_key(
            &s,
            Provider::Opencode,
            "OpenCode Zen",
            "opencode",
            "synthetic-resume-selection-key",
            None,
        )
        .await
        .unwrap();
    let account_id = account.id.to_string();
    let catalog = s.opencode.models(&s).await.unwrap();
    let default_model = "auto/ling-3-1-flash-free";
    assert_eq!(
        catalog["models"]
            .as_array()
            .unwrap()
            .iter()
            .find(|model| model["isDefault"] == true)
            .unwrap()["model"],
        default_model
    );

    let mut agent = s.agent_snapshot(MAIN_AGENT_ID).await.unwrap();
    agent["id"] = id().into();
    agent["name"] = "OpenCode resume fixture".into();
    agent["provider"] = "opencode".into();
    agent["model"] = "opencode/ling-3.1-flash-free".into();
    agent["reasoning"] = "high".into();
    agent["speed"] = "".into();
    agent["access"]["sandbox"] = "yolo".into();
    let agent = s.store.put_agent_fixture(agent).await.unwrap();
    let chat = s
        .chat_create(json!({ "agentId": text(&agent, "id") }))
        .await
        .unwrap();
    let chat_id = text(&chat, "id").to_owned();
    let mut sent = message("fixture:opencode-hang");
    sent["provider"] = "opencode".into();
    sent["model"] = "opencode/ling-3.1-flash-free".into();
    sent["reasoning"] = "high".into();
    let turn_id = text(&sent, "id").to_owned();
    s.chat_send(&chat_id, sent).await.unwrap();
    fixture.start().await;
    let run_id = fixture.chat_run(&chat_id).await;
    let running = await_opencode_resume_setup(
        &fixture,
        &run_id,
        "opencode_catalog_default",
        &plan_received,
        &log_request_seen,
    )
    .await;
    assert_eq!(running["snapshot"]["agent"]["provider"], "opencode");
    assert_eq!(running["snapshot"]["agent"]["access"]["sandbox"], "yolo");

    let session = common::relay_fixture::context(&s).await;
    let client = reqwest::Client::new();
    let relay_token = text(&session, "value").to_owned();
    let fixture_url = fixture.url.clone();
    let resume = |selection: Value| {
        client
            .post(format!("{fixture_url}/api/chats/{chat_id}/resume"))
            .header("x-test-relay-token", &relay_token)
            .json(&json!({
                "runId": run_id,
                "turnId": turn_id,
                "selection": selection,
            }))
    };
    let stopped = client
        .post(format!("{}/api/chats/{chat_id}/stop", fixture.url))
        .header("x-test-relay-token", &relay_token)
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    stopped.error_for_status().unwrap();
    eprintln!("CAIRN_RESUME_TEST_PHASE test=opencode_catalog_default phase=stopped_resumable");
    let cancelled = fixture
        .until(&run_id, |run| {
            run["status"] == RunStatus::Cancelled && run["resumeAvailable"] == true
        })
        .await;
    let checkpoint_before = fixture.checkpoint(&run_id).await;
    fixture.stop(false).await;
    s.store
        .change_run(
            &run_id,
            cairn_installation::runs::RunChanges {
                status: Some(RunStatus::Failed),
                finished_at: Some(Some(now())),
                error: Some(Some("Usage limit has been reached".into())),
                ..Default::default()
            },
        )
        .await
        .unwrap();

    for model in ["not-a-model", "openrouter/unlisted-model"] {
        let response = resume(json!({ "model": model, "reasoning": "high", "speed": "" }))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{model}");
        let unchanged = s.store.run(&run_id).await.unwrap();
        assert_eq!(unchanged["status"], RunStatus::Failed);
        assert_eq!(unchanged["snapshot"], cancelled["snapshot"]);
    }

    common::account_fixture::update_account_fixture(&s, &account_id, &json!({ "enabled": false }))
        .await
        .unwrap();
    let no_default = resume(json!({ "model": "", "reasoning": "high", "speed": "" }))
        .send()
        .await
        .unwrap();
    assert_eq!(no_default.status(), StatusCode::BAD_REQUEST);
    let unchanged = s.store.run(&run_id).await.unwrap();
    assert_eq!(unchanged["status"], RunStatus::Failed);
    assert_eq!(unchanged["snapshot"], cancelled["snapshot"]);
    common::account_fixture::update_account_fixture(&s, &account_id, &json!({ "enabled": true }))
        .await
        .unwrap();

    let app = common::relay_fixture::router(s.clone()).await.unwrap();
    let (resume_status, resume_response) = api_post(
        &app,
        &relay_token,
        &format!("/api/chats/{chat_id}/resume"),
        json!({
            "runId": run_id,
            "turnId": turn_id,
            "selection": { "model": "", "reasoning": "high", "speed": "" },
        }),
    )
    .await;
    assert_eq!(resume_status, StatusCode::OK, "{resume_response}");
    let mut expected_snapshot = cancelled["snapshot"].clone();
    expected_snapshot["agent"]["model"] = default_model.into();
    expected_snapshot["agent"]["reasoning"] = "high".into();
    expected_snapshot["agent"]["speed"] = "".into();
    let queued = s.store.run(&run_id).await.unwrap();
    assert_eq!(queued["status"], RunStatus::Queued);
    assert_eq!(queued["snapshot"], expected_snapshot);
    assert_eq!(queued["snapshot"]["agent"]["provider"], "opencode");
    assert_eq!(queued["chatExecution"]["messageId"], turn_id);
    assert_eq!(queued["sessionId"], cancelled["sessionId"]);
    assert_eq!(queued["workspace"], cancelled["workspace"]);

    controller
        .hold
        .store(false, std::sync::atomic::Ordering::Relaxed);
    fixture.start().await;
    eprintln!(
        "CAIRN_RESUME_TEST_PHASE test=opencode_catalog_default phase=selected_turn_completed"
    );
    let completed = fixture.until_finished(&run_id).await;
    assert_eq!(completed["status"], RunStatus::Succeeded, "{completed}");
    assert_eq!(completed["snapshot"], expected_snapshot);
    for field in ["id", "taskId", "workspace", "sessionId", "chatExecution"] {
        assert_eq!(completed[field], cancelled[field], "{field}");
    }
    assert_eq!(completed["id"], run_id);
    assert_eq!(completed["chatExecution"]["messageId"], turn_id);
    assert_eq!(
        fixture.checkpoint(&run_id).await["generation"],
        checkpoint_before["generation"]
    );
    let selected_plan = controller
        .plans
        .lock()
        .await
        .iter()
        .find(|plan| {
            plan["runId"] == run_id
                && plan["chat"]["provider"] == "opencode"
                && plan["chat"]["model"] == default_model
                && plan["chat"]["execution"]["messageId"] == turn_id
        })
        .cloned()
        .expect("the catalog-default OpenCode selection must reach the worker plan");
    assert_eq!(selected_plan["sandbox"], "yolo");
    assert_eq!(selected_plan["chat"]["sessionId"], cancelled["sessionId"]);
    assert_eq!(selected_plan["chat"]["execution"]["messageId"], turn_id);
    assert_eq!(
        selected_plan["chat"]["execution"]["attachments"],
        cancelled["chatExecution"]["attachments"]
    );
    fixture.stop(false).await;
    server.abort();
}

#[tokio::test]
async fn chat_resume_rejects_hidden_opencode_auto_without_blocking_pinned_provider_route() {
    use std::sync::atomic::{AtomicBool, Ordering};

    let mut fixture =
        Fixture::with_opencode_bin(2, common::fixture("opencode-hidden-default.mjs")).await;
    fixture.stop(false).await;
    let data_dir = fixture.service.config.data_dir.clone();
    let controller = Arc::new(Controller {
        data: data_dir,
        plans: tokio::sync::Mutex::new(Vec::new()),
        failures: 0,
        slots: 4,
        hold: AtomicBool::new(true),
    });
    let log_request_seen = Arc::new(AtomicBool::new(false));
    let plan_received = Arc::new(AtomicBool::new(false));
    let runner = Router::new()
        .fallback(any({
            let log_request_seen = log_request_seen.clone();
            let plan_received = plan_received.clone();
            move |state: State<Arc<Controller>>, request: Request| {
                let log_request_seen = log_request_seen.clone();
                let plan_received = plan_received.clone();
                async move {
                    let path = request.uri().path();
                    if path.starts_with("/runs/") && path.ends_with("/logs") {
                        log_request_seen.store(true, Ordering::Relaxed);
                    }
                    let is_plan_post = request.method() == axum::http::Method::POST
                        && path
                            .strip_prefix("/runs/")
                            .is_some_and(|attempt| !attempt.is_empty() && !attempt.contains('/'));
                    let response = serve_controller(state, request).await;
                    if is_plan_post && response.status().is_success() {
                        plan_received.store(true, Ordering::Relaxed);
                    }
                    response
                }
            }
        }))
        .with_state(controller.clone());
    let (listener, address) = common::bind().await;
    common::reconfigure(&mut fixture.service, |config| {
        config.runner_url = format!("http://{address}");
    })
    .await;
    let s = fixture.service.clone();
    let server = common::serve(listener, runner);
    tokio::fs::write(
        s.config.data_dir.join("storage-s3.json"),
        json!({ "bucket": "fixture-storage", "endpoint": "https://127.0.0.1:1" }).to_string(),
    )
    .await
    .unwrap();
    s.accounts
        .connect_api_key(
            &s,
            Provider::Opencode,
            "OpenCode Zen",
            "opencode",
            "synthetic-hidden-default-resume-key",
            None,
        )
        .await
        .unwrap();

    let catalog = s.opencode.models(&s).await.unwrap();
    let hidden_default = catalog["models"]
        .as_array()
        .unwrap()
        .iter()
        .find(|model| model["model"] == "auto/retired-model")
        .unwrap();
    assert_eq!(hidden_default["isDefault"], true);
    assert_eq!(hidden_default["hidden"], true);
    assert_eq!(
        hidden_default["routes"][0]["model"],
        "opencode/retired-model"
    );
    assert_eq!(hidden_default["routes"][0]["hidden"], true);

    let mut agent = s.agent_snapshot(MAIN_AGENT_ID).await.unwrap();
    agent["id"] = id().into();
    agent["name"] = "OpenCode hidden default resume fixture".into();
    agent["provider"] = "opencode".into();
    agent["model"] = "opencode/ling-3.1-flash-free".into();
    agent["reasoning"] = "high".into();
    agent["speed"] = "".into();
    agent["access"]["sandbox"] = "yolo".into();
    let agent = s.store.put_agent_fixture(agent).await.unwrap();
    let chat = s
        .chat_create(json!({ "agentId": text(&agent, "id") }))
        .await
        .unwrap();
    let chat_id = text(&chat, "id").to_owned();
    let mut sent = message("fixture:opencode-hang");
    sent["provider"] = "opencode".into();
    sent["model"] = "opencode/ling-3.1-flash-free".into();
    sent["reasoning"] = "high".into();
    let turn_id = text(&sent, "id").to_owned();
    s.chat_send(&chat_id, sent).await.unwrap();
    fixture.start().await;
    let run_id = fixture.chat_run(&chat_id).await;
    let running = await_opencode_resume_setup(
        &fixture,
        &run_id,
        "opencode_hidden_auto",
        &plan_received,
        &log_request_seen,
    )
    .await;
    assert_eq!(
        running["snapshot"]["agent"]["model"],
        "opencode/ling-3.1-flash-free"
    );

    let relay = common::relay_fixture::context(&s).await;
    let relay_token = text(&relay, "value").to_owned();
    let client = reqwest::Client::new();
    let stopped = client
        .post(format!("{}/api/chats/{chat_id}/stop", fixture.url))
        .header("x-test-relay-token", &relay_token)
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    stopped.error_for_status().unwrap();
    eprintln!("CAIRN_RESUME_TEST_PHASE test=opencode_hidden_auto phase=stopped_resumable");
    let cancelled = fixture
        .until(&run_id, |run| {
            run["status"] == RunStatus::Cancelled && run["resumeAvailable"] == true
        })
        .await;
    let checkpoint_before = fixture.checkpoint(&run_id).await;
    fixture.stop(false).await;
    s.store
        .change_run(
            &run_id,
            cairn_installation::runs::RunChanges {
                status: Some(RunStatus::Failed),
                finished_at: Some(Some(now())),
                error: Some(Some("Usage limit has been reached".into())),
                ..Default::default()
            },
        )
        .await
        .unwrap();

    let before_rejection = s.store.run(&run_id).await.unwrap();
    let attempts_before = s
        .store
        .attempts_fixture()
        .await
        .unwrap()
        .into_iter()
        .filter(|attempt| attempt["runId"] == run_id)
        .count();
    let plans_before = controller
        .plans
        .lock()
        .await
        .iter()
        .filter(|plan| plan["runId"] == run_id)
        .count();
    assert_eq!(before_rejection["status"], RunStatus::Failed);
    assert_eq!(before_rejection["resumeAvailable"], true);

    let app = common::relay_fixture::router(s.clone()).await.unwrap();
    for (label, model) in [
        ("hidden default", ""),
        ("explicit hidden Auto", "auto/retired-model"),
    ] {
        let (status, response) = api_post(
            &app,
            &relay_token,
            &format!("/api/chats/{chat_id}/resume"),
            json!({
                "runId": run_id,
                "turnId": turn_id,
                "selection": { "model": model, "reasoning": "", "speed": "" },
            }),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{label}: {response}");
        assert_eq!(
            s.store.run(&run_id).await.unwrap(),
            before_rejection,
            "{label}"
        );
        assert_eq!(
            fixture.checkpoint(&run_id).await,
            checkpoint_before,
            "{label}"
        );
        assert_eq!(
            s.store
                .attempts_fixture()
                .await
                .unwrap()
                .into_iter()
                .filter(|attempt| attempt["runId"] == run_id)
                .count(),
            attempts_before,
            "{label} must not create an attempt",
        );
        assert_eq!(
            controller
                .plans
                .lock()
                .await
                .iter()
                .filter(|plan| plan["runId"] == run_id)
                .count(),
            plans_before,
            "{label} must not reach the worker",
        );
        assert_eq!(before_rejection["chatExecution"]["messageId"], turn_id);
        assert_eq!(before_rejection["sessionId"], cancelled["sessionId"]);
        assert_eq!(before_rejection["workspace"], cancelled["workspace"]);
    }

    let (status, response) = api_post(
        &app,
        &relay_token,
        &format!("/api/chats/{chat_id}/resume"),
        json!({
            "runId": run_id,
            "turnId": turn_id,
            "selection": {
                "model": "opencode/retired-model",
                "reasoning": "",
                "speed": "",
            },
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{response}");
    let queued = s.store.run(&run_id).await.unwrap();
    let mut expected_snapshot = cancelled["snapshot"].clone();
    expected_snapshot["agent"]["model"] = "opencode/retired-model".into();
    expected_snapshot["agent"]["reasoning"] = "".into();
    expected_snapshot["agent"]["speed"] = "".into();
    assert_eq!(queued["status"], RunStatus::Queued);
    assert_eq!(queued["snapshot"], expected_snapshot);
    for field in ["id", "taskId", "workspace", "sessionId", "chatExecution"] {
        assert_eq!(queued[field], cancelled[field], "{field}");
    }

    controller
        .hold
        .store(false, std::sync::atomic::Ordering::Relaxed);
    fixture.start().await;
    eprintln!("CAIRN_RESUME_TEST_PHASE test=opencode_hidden_auto phase=pinned_turn_completed");
    let completed = fixture.until_finished(&run_id).await;
    assert_eq!(completed["status"], RunStatus::Succeeded, "{completed}");
    assert_eq!(completed["snapshot"], expected_snapshot);
    for field in ["id", "taskId", "workspace", "sessionId", "chatExecution"] {
        assert_eq!(completed[field], cancelled[field], "{field}");
    }
    assert_eq!(
        fixture.checkpoint(&run_id).await["generation"],
        checkpoint_before["generation"]
    );
    let pinned_plan = controller
        .plans
        .lock()
        .await
        .iter()
        .find(|plan| {
            plan["runId"] == run_id
                && plan["chat"]["provider"] == "opencode"
                && plan["chat"]["model"] == "opencode/retired-model"
                && plan["chat"]["execution"]["messageId"] == turn_id
        })
        .cloned()
        .expect("a deprecated provider-qualified route stays pinned in the worker plan");
    assert_eq!(pinned_plan["sandbox"], "yolo");
    assert_eq!(pinned_plan["chat"]["sessionId"], cancelled["sessionId"]);
    fixture.stop(false).await;
    server.abort();
}

#[tokio::test]
async fn chat_resume_selection_keeps_saved_identity_and_reaches_the_worker_plan() {
    use std::sync::atomic::AtomicBool;

    let mut fixture = Fixture::new().await;
    fixture.stop(false).await;
    let data_dir = fixture.service.config.data_dir.clone();
    let controller = Arc::new(Controller {
        data: data_dir,
        plans: tokio::sync::Mutex::new(Vec::new()),
        failures: 0,
        slots: 4,
        hold: AtomicBool::new(true),
    });
    let app = Router::new()
        .fallback(any(serve_controller))
        .with_state(controller.clone());
    let (listener, address) = common::bind().await;
    common::reconfigure(&mut fixture.service, |config| {
        config.runner_url = format!("http://{address}");
    })
    .await;
    let s = fixture.service.clone();
    let server = common::serve(listener, app);
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
    cairn_installation::nodes::refresh_local(&s).await.unwrap();
    s.store
        .set(
            "codex-models:",
            json!({
                "checkedAt": now(),
                "models": [
                    {
                        "model": "gpt-6-sol",
                        "isDefault": true,
                        "supportedReasoningEfforts": [{ "reasoningEffort": "high" }],
                        "serviceTiers": [{ "id": "priority" }],
                    },
                    {
                        "model": "gpt-6-luna",
                        "isDefault": false,
                        "supportedReasoningEfforts": [{ "reasoningEffort": "xhigh" }],
                        "serviceTiers": [{ "id": "priority" }],
                    },
                    {
                        "model": "my-alias",
                        "isDefault": false,
                        "supportedReasoningEfforts": [{ "reasoningEffort": "high" }],
                        "serviceTiers": [{ "id": "priority" }],
                    },
                ],
            }),
            None,
        )
        .await
        .unwrap();

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
    let mut sent = message("fixture:chat-hang");
    sent["provider"] = "codex".into();
    sent["model"] = "gpt-6-sol".into();
    sent["reasoning"] = "high".into();
    sent["speed"] = "priority".into();
    sent["attachmentIds"] = json!([attachment_id]);
    s.chat_send(chat_id, sent).await.unwrap();
    fixture.start().await;
    let run_id = fixture.chat_run(chat_id).await;
    let active = fixture
        .until(&run_id, |run| {
            run["status"] == RunStatus::Running && run["sessionId"].is_string()
        })
        .await;
    eventually(
        Duration::from_secs(20),
        Duration::from_millis(30),
        async || {
            controller
                .plans
                .lock()
                .await
                .iter()
                .any(|plan| plan["runId"] == run_id)
                .then_some(())
        },
    )
    .await;

    let session = common::relay_fixture::context(&s).await;
    let client = reqwest::Client::new();
    let resume = |body: Value| {
        client
            .post(format!("{}/api/chats/{chat_id}/resume", fixture.url))
            .header("x-test-relay-token", text(&session, "value"))
            .json(&body)
    };
    let turn_id = text(&active["chatExecution"], "messageId").to_owned();
    let override_selection = json!({
        "model": "gpt-6-luna",
        "reasoning": "xhigh",
        "speed": "",
    });

    let active_refusal = resume(json!({
        "runId": run_id,
        "turnId": turn_id,
        "selection": override_selection,
    }))
    .send()
    .await
    .unwrap();
    assert_eq!(active_refusal.status(), StatusCode::CONFLICT);
    assert_eq!(
        s.store.run(&run_id).await.unwrap()["snapshot"],
        active["snapshot"]
    );

    let stopped = client
        .post(format!("{}/api/chats/{chat_id}/stop", fixture.url))
        .header("x-test-relay-token", text(&session, "value"))
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    stopped.error_for_status().unwrap();
    let before = fixture
        .until(&run_id, |run| {
            run["status"] == RunStatus::Cancelled && run["resumeAvailable"] == true
        })
        .await;
    let checkpoint_before = fixture.checkpoint(&run_id).await;
    assert!(checkpoint_before["prepared"].is_object());
    let checkpoint_generation_before = checkpoint_before.get("generation").cloned();

    let stale_run = resume(json!({
        "runId": id(),
        "turnId": turn_id,
        "selection": override_selection,
    }))
    .send()
    .await
    .unwrap();
    assert_eq!(stale_run.status(), StatusCode::CONFLICT);
    let stale_turn = resume(json!({
        "runId": run_id,
        "turnId": "stale-turn",
        "selection": override_selection,
    }))
    .send()
    .await
    .unwrap();
    assert_eq!(stale_turn.status(), StatusCode::CONFLICT);
    for body in [
        json!({ "selection": override_selection }),
        json!({ "runId": run_id, "selection": override_selection }),
        json!({ "turnId": turn_id, "selection": override_selection }),
    ] {
        let missing_guard = resume(body).send().await.unwrap();
        assert_eq!(missing_guard.status(), StatusCode::BAD_REQUEST);
    }
    assert_eq!(
        s.store.run(&run_id).await.unwrap()["snapshot"],
        before["snapshot"]
    );
    assert_eq!(
        s.store.run(&run_id).await.unwrap()["status"],
        RunStatus::Cancelled
    );
    let unknown_model = resume(json!({
        "runId": run_id,
        "turnId": turn_id,
        "selection": {
            "model": "gpt-6-not-in-catalog",
            "reasoning": "xhigh",
            "speed": "",
        },
    }))
    .send()
    .await
    .unwrap();
    assert_eq!(unknown_model.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        s.store.run(&run_id).await.unwrap()["snapshot"],
        before["snapshot"]
    );
    assert_eq!(
        s.store.run(&run_id).await.unwrap()["status"],
        RunStatus::Cancelled
    );
    let invalid_default_speed = resume(json!({
        "runId": run_id,
        "turnId": turn_id,
        "selection": {
            "model": "gpt-6-luna",
            "reasoning": "xhigh",
            "speed": "default",
        },
    }))
    .send()
    .await
    .unwrap();
    assert_eq!(invalid_default_speed.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        s.store.run(&run_id).await.unwrap()["snapshot"],
        before["snapshot"]
    );
    assert_eq!(
        s.store.run(&run_id).await.unwrap()["status"],
        RunStatus::Cancelled
    );

    fixture.stop(false).await;
    s.store
        .change_run(
            &run_id,
            cairn_installation::runs::RunChanges {
                status: Some(RunStatus::Failed),
                finished_at: Some(Some(now())),
                error: Some(Some("Usage limit has been reached".into())),
                cancel_requested_at: Some(None),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let failed_quota_run = s.store.run(&run_id).await.unwrap();
    assert_eq!(failed_quota_run["status"], RunStatus::Failed);
    assert_eq!(failed_quota_run["cancelRequestedAt"], Value::Null);
    assert!(
        failed_quota_run["error"]
            .as_str()
            .unwrap()
            .contains("Usage limit")
    );
    let mut alternative = serde_json::to_value(
        s.accounts
            .create(&s, Provider::Claude, "Switch target")
            .await
            .unwrap(),
    )
    .unwrap();
    let alternative_id = text(&alternative, "id").to_owned();
    alternative["state"] = "ready".into();
    s.store.save_account_fixture(alternative).await.unwrap();
    let claude_home = accounts::claude::account_home(&s.config, &alternative_id);
    std::fs::create_dir_all(&claude_home).unwrap();
    std::fs::write(
        claude_home.join(".credentials.json"),
        json!({
            "claudeAiOauth": {
                "accessToken": "fixture-access",
                "expiresAt": now() + 3_600_000,
                "scopes": ["user:inference"],
            },
        })
        .to_string(),
    )
    .unwrap();

    let app = common::relay_fixture::router(s.clone()).await.unwrap();
    let relay_token = text(&session, "value");
    let switch_id = id();
    let switch = json!({
        "runId": run_id,
        "turnId": turn_id,
        "requestId": switch_id,
        "provider": "claude",
    });
    let (switch_status, _) = api_post(
        &app,
        relay_token,
        &format!("/api/chats/{chat_id}/coding-agent"),
        switch.clone(),
    )
    .await;
    assert_eq!(switch_status, StatusCode::OK);
    let staged = s
        .store
        .read({
            let chat_id = chat_id.to_owned();
            move |db| db.engine_switch(&chat_id)
        })
        .await
        .unwrap()
        .unwrap();
    assert_eq!(staged.request_id, switch_id);
    assert!(staged.pending);

    let (switch_retry_status, _) = api_post(
        &app,
        relay_token,
        &format!("/api/chats/{chat_id}/coding-agent"),
        switch.clone(),
    )
    .await;
    assert_eq!(switch_retry_status, StatusCode::OK);
    let before_legacy_resume = s.store.run(&run_id).await.unwrap();
    let checkpoint_before_legacy_resume = fixture.checkpoint(&run_id).await;
    let (legacy_resume_status, legacy_resume_response) = api_post(
        &app,
        relay_token,
        &format!("/api/chats/{chat_id}/resume"),
        json!({ "runId": run_id, "turnId": turn_id }),
    )
    .await;
    assert_eq!(
        legacy_resume_status,
        StatusCode::CONFLICT,
        "{legacy_resume_response}"
    );
    assert_eq!(s.store.run(&run_id).await.unwrap(), before_legacy_resume);
    assert_eq!(
        fixture.checkpoint(&run_id).await,
        checkpoint_before_legacy_resume
    );
    let preserved_switch = s
        .store
        .read({
            let chat_id = chat_id.to_owned();
            move |db| db.engine_switch(&chat_id)
        })
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        serde_json::to_value(&preserved_switch).unwrap(),
        serde_json::to_value(&staged).unwrap()
    );
    assert_eq!(preserved_switch.request_id, switch_id);
    assert_eq!(preserved_switch.provider, "claude");
    assert!(preserved_switch.pending);

    let resume_body = json!({
        "runId": run_id,
        "turnId": turn_id,
        "selection": override_selection,
    });
    let (resume_status, resume_response) = api_post(
        &app,
        relay_token,
        &format!("/api/chats/{chat_id}/resume"),
        resume_body,
    )
    .await;
    assert_eq!(resume_status, StatusCode::OK, "{resume_response}");
    let queued_resume = s.store.run(&run_id).await.unwrap();
    assert_eq!(queued_resume["status"], RunStatus::Queued);
    assert_eq!(queued_resume["snapshot"]["agent"]["provider"], "codex");
    assert_eq!(queued_resume["snapshot"]["agent"]["speed"], "priority");
    assert_eq!(queued_resume["chatExecution"]["messageId"], turn_id);
    let cancelled_switch = s
        .store
        .read({
            let chat_id = chat_id.to_owned();
            move |db| db.engine_switch(&chat_id)
        })
        .await
        .unwrap()
        .unwrap();
    assert_eq!(cancelled_switch.request_id, switch_id);
    assert!(!cancelled_switch.pending);

    let late_switch = json!({
        "runId": run_id,
        "turnId": turn_id,
        "requestId": id(),
        "provider": "claude",
    });
    let (late_switch_status, _) = api_post(
        &app,
        relay_token,
        &format!("/api/chats/{chat_id}/coding-agent"),
        late_switch,
    )
    .await;
    assert_eq!(late_switch_status, StatusCode::CONFLICT);

    let (completed_switch_retry_status, _) = api_post(
        &app,
        relay_token,
        &format!("/api/chats/{chat_id}/coding-agent"),
        switch,
    )
    .await;
    assert_eq!(completed_switch_retry_status, StatusCode::OK);
    assert!(
        !s.store
            .read({
                let chat_id = chat_id.to_owned();
                move |db| Ok(db.engine_switch(&chat_id)?.unwrap().pending)
            })
            .await
            .unwrap()
    );

    controller
        .hold
        .store(false, std::sync::atomic::Ordering::Relaxed);
    fixture.start().await;
    let resume = |body: Value| {
        client
            .post(format!("{}/api/chats/{chat_id}/resume", fixture.url))
            .header("x-test-relay-token", text(&session, "value"))
            .json(&body)
    };
    let completed = fixture.until_finished(&run_id).await;
    assert_eq!(completed["status"], RunStatus::Succeeded, "{completed}");
    let mut expected_snapshot = before["snapshot"].clone();
    expected_snapshot["agent"]["model"] = "gpt-6-luna".into();
    expected_snapshot["agent"]["reasoning"] = "xhigh".into();
    expected_snapshot["agent"]["speed"] = "priority".into();
    assert_eq!(completed["snapshot"], expected_snapshot);
    for field in ["id", "taskId", "workspace", "sessionId", "chatExecution"] {
        assert_eq!(completed[field], before[field], "{field}");
    }
    assert_eq!(completed["chatExecution"]["messageId"], turn_id);
    let checkpoint_after = fixture.checkpoint(&run_id).await;
    assert_eq!(
        checkpoint_after.get("generation").cloned(),
        checkpoint_generation_before
    );
    let selected_plan = controller
        .plans
        .lock()
        .await
        .iter()
        .find(|plan| {
            plan["runId"] == run_id
                && plan["chat"]["model"] == "gpt-6-luna"
                && plan["chat"]["reasoning"] == "xhigh"
        })
        .cloned()
        .expect("the resumed worker plan must contain the selected model and reasoning");
    assert_eq!(selected_plan["chat"]["execution"]["messageId"], turn_id);
    assert_eq!(selected_plan["chat"]["serviceTier"], "priority");
    assert_eq!(selected_plan["chat"]["sessionId"], before["sessionId"]);
    assert_eq!(
        selected_plan["chat"]["execution"]["attachments"],
        before["chatExecution"]["attachments"]
    );

    let unpaused = client
        .post(format!("{}/api/chats/{chat_id}/pause", fixture.url))
        .header("x-test-relay-token", text(&session, "value"))
        .json(&json!({ "paused": false }))
        .send()
        .await
        .unwrap();
    unpaused.error_for_status().unwrap();
    controller
        .hold
        .store(true, std::sync::atomic::Ordering::Relaxed);
    let mut later_message = message("fixture:chat-hang");
    later_message["provider"] = "codex".into();
    later_message["model"] = "gpt-6-luna".into();
    later_message["reasoning"] = "xhigh".into();
    later_message["speed"] = "priority".into();
    let later_turn_id = text(&later_message, "id").to_owned();
    s.chat_send(chat_id, later_message).await.unwrap();
    let later_turn = fixture
        .until(&run_id, |run| {
            run["status"] == RunStatus::Running
                && run["chatExecution"]["messageId"] == later_turn_id
        })
        .await;
    assert_eq!(later_turn["id"], run_id);
    eventually(
        Duration::from_secs(20),
        Duration::from_millis(30),
        async || {
            controller
                .plans
                .lock()
                .await
                .iter()
                .any(|plan| {
                    plan["runId"] == run_id
                        && plan["chat"]["execution"]["messageId"] == later_turn_id
                })
                .then_some(())
        },
    )
    .await;
    let stopped = client
        .post(format!("{}/api/chats/{chat_id}/stop", fixture.url))
        .header("x-test-relay-token", text(&session, "value"))
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    stopped.error_for_status().unwrap();
    let later_turn_stopped = fixture
        .until(&run_id, |run| {
            run["status"] == RunStatus::Cancelled && run["resumeAvailable"] == true
        })
        .await;
    assert_eq!(
        later_turn_stopped["chatExecution"]["messageId"],
        later_turn_id
    );
    let stale_existing_turn = resume(json!({
        "runId": run_id,
        "turnId": turn_id,
        "selection": override_selection,
    }))
    .send()
    .await
    .unwrap();
    assert_eq!(stale_existing_turn.status(), StatusCode::CONFLICT);
    let after_stale_existing_turn = s.store.run(&run_id).await.unwrap();
    assert_eq!(after_stale_existing_turn["status"], RunStatus::Cancelled);
    assert_eq!(after_stale_existing_turn["snapshot"], expected_snapshot);
    assert_eq!(
        after_stale_existing_turn["chatExecution"]["messageId"],
        later_turn_id
    );

    controller
        .hold
        .store(false, std::sync::atomic::Ordering::Relaxed);
    let standard_plan_boundary = controller.plans.lock().await.len();
    let (standard_resume, standard_snapshot) = (
        resume(json!({
            "runId": run_id,
            "turnId": later_turn_id,
            "selection": {
                "model": "gpt-6-luna",
                "reasoning": "xhigh",
                "speed": "standard",
            },
        }))
        .send()
        .await
        .unwrap(),
        {
            let mut snapshot = expected_snapshot.clone();
            snapshot["agent"]["speed"] = "standard".into();
            snapshot
        },
    );
    assert!(standard_resume.status().is_success());
    let standard_completed = fixture.until_finished(&run_id).await;
    assert_eq!(standard_completed["status"], RunStatus::Succeeded);
    assert_eq!(standard_completed["snapshot"], standard_snapshot);
    for field in ["id", "taskId", "workspace", "sessionId"] {
        assert_eq!(
            standard_completed[field], later_turn_stopped[field],
            "{field}"
        );
    }
    assert_eq!(
        standard_completed["chatExecution"]["messageId"],
        later_turn_id
    );
    let standard_plan = controller
        .plans
        .lock()
        .await
        .iter()
        .skip(standard_plan_boundary)
        .find(|plan| {
            plan["runId"] == run_id
                && plan["chat"]["execution"]["messageId"] == later_turn_id
                && plan["chat"]["model"] == "gpt-6-luna"
        })
        .cloned()
        .expect("the explicit Standard request must reach the resumed worker plan");
    assert_eq!(standard_plan["chat"]["serviceTier"], "default");

    controller
        .hold
        .store(false, std::sync::atomic::Ordering::Relaxed);

    fixture.stop(false).await;
    s.store
        .change_run(
            &run_id,
            cairn_installation::runs::RunChanges {
                status: Some(RunStatus::Failed),
                finished_at: Some(Some(now())),
                error: Some(Some("Usage limit has been reached".into())),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let normal_switch_id = id();
    let normal_switch = json!({
        "runId": run_id,
        "turnId": later_turn_id,
        "requestId": normal_switch_id,
        "provider": "claude",
    });
    let (normal_stage_status, normal_stage_response) = api_post(
        &app,
        relay_token,
        &format!("/api/chats/{chat_id}/coding-agent"),
        normal_switch.clone(),
    )
    .await;
    assert_eq!(
        normal_stage_status,
        StatusCode::OK,
        "{normal_stage_response}"
    );
    let (normal_retry_status, _) = api_post(
        &app,
        relay_token,
        &format!("/api/chats/{chat_id}/coding-agent"),
        normal_switch.clone(),
    )
    .await;
    assert_eq!(normal_retry_status, StatusCode::OK);
    let normal_staged = s
        .store
        .read({
            let chat_id = chat_id.to_owned();
            move |db| db.engine_switch(&chat_id)
        })
        .await
        .unwrap()
        .unwrap();
    assert_eq!(normal_staged.request_id, normal_switch_id);
    assert!(normal_staged.pending);

    fixture.start().await;
    let switched = fixture
        .until(&run_id, |run| {
            run["snapshot"]["agent"]["provider"] == "claude"
        })
        .await;
    assert_eq!(switched["id"], run_id);
    assert_eq!(switched["chatExecution"]["messageId"], later_turn_id);
    let completed_switch = fixture.until_finished(&run_id).await;
    assert_eq!(completed_switch["snapshot"]["agent"]["provider"], "claude");
    let normal_receipt = s
        .store
        .read({
            let chat_id = chat_id.to_owned();
            move |db| db.engine_switch(&chat_id)
        })
        .await
        .unwrap()
        .unwrap();
    assert_eq!(normal_receipt.request_id, normal_switch_id);
    assert_eq!(normal_receipt.provider, "claude");
    assert!(!normal_receipt.pending);
    let claude_plan = controller
        .plans
        .lock()
        .await
        .iter()
        .find(|plan| {
            plan["runId"] == run_id
                && plan["chat"]["execution"]["messageId"] == later_turn_id
                && plan["chat"]["provider"] == "claude"
        })
        .cloned()
        .expect("the normal pending switch must reach the Claude worker plan");
    assert_eq!(claude_plan["chat"]["provider"], "claude");
    let plans_before_receipt_retry = controller.plans.lock().await.len();
    let (normal_receipt_retry_status, _) = api_post(
        &app,
        relay_token,
        &format!("/api/chats/{chat_id}/coding-agent"),
        normal_switch,
    )
    .await;
    assert_eq!(normal_receipt_retry_status, StatusCode::OK);
    assert_eq!(
        controller.plans.lock().await.len(),
        plans_before_receipt_retry
    );

    let compatibility_chat = s.chat_create(json!({})).await.unwrap();
    let compatibility_chat_id = text(&compatibility_chat, "id");
    let mut compatibility_message = message("fixture:chat-hang");
    compatibility_message["provider"] = "codex".into();
    compatibility_message["model"] = "gpt-6-sol".into();
    compatibility_message["reasoning"] = "high".into();
    s.chat_send(compatibility_chat_id, compatibility_message)
        .await
        .unwrap();
    controller
        .hold
        .store(true, std::sync::atomic::Ordering::Relaxed);
    let compatibility_run_id = fixture.chat_run(compatibility_chat_id).await;
    fixture
        .until(&compatibility_run_id, |run| {
            run["status"] == RunStatus::Running && run["sessionId"].is_string()
        })
        .await;
    let stopped = client
        .post(format!(
            "{}/api/chats/{compatibility_chat_id}/stop",
            fixture.url
        ))
        .header("x-test-relay-token", text(&session, "value"))
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    stopped.error_for_status().unwrap();
    let compatibility_before = fixture
        .until(&compatibility_run_id, |run| {
            run["status"] == RunStatus::Cancelled && run["resumeAvailable"] == true
        })
        .await;
    controller
        .hold
        .store(false, std::sync::atomic::Ordering::Relaxed);
    let compatible = client
        .post(format!(
            "{}/api/chats/{compatibility_chat_id}/resume",
            fixture.url
        ))
        .header("x-test-relay-token", text(&session, "value"))
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert!(compatible.status().is_success());
    let compatibility_completed = fixture.until_finished(&compatibility_run_id).await;
    assert_eq!(
        compatibility_completed["snapshot"],
        compatibility_before["snapshot"]
    );
    let compatibility_plans = controller
        .plans
        .lock()
        .await
        .iter()
        .filter(|plan| plan["runId"] == compatibility_run_id)
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(compatibility_plans.len(), 2);
    assert!(
        compatibility_plans
            .iter()
            .all(|plan| plan["chat"]["model"] == "gpt-6-sol")
    );

    controller
        .hold
        .store(true, std::sync::atomic::Ordering::Relaxed);
    let default_model_chat = s.chat_create(json!({})).await.unwrap();
    let default_model_chat_id = text(&default_model_chat, "id").to_owned();
    let mut default_model_message = message("fixture:chat-hang");
    default_model_message["provider"] = "codex".into();
    default_model_message["model"] = "".into();
    default_model_message["reasoning"] = "high".into();
    default_model_message["speed"] = "priority".into();
    let default_model_turn_id = text(&default_model_message, "id").to_owned();
    s.chat_send(&default_model_chat_id, default_model_message)
        .await
        .unwrap();
    let default_model_run_id = fixture.chat_run(&default_model_chat_id).await;
    let default_model_running = fixture
        .until(&default_model_run_id, |run| {
            run["status"] == RunStatus::Running && run["sessionId"].is_string()
        })
        .await;
    assert_eq!(default_model_running["snapshot"]["agent"]["model"], "");
    let stopped = client
        .post(format!(
            "{}/api/chats/{default_model_chat_id}/stop",
            fixture.url
        ))
        .header("x-test-relay-token", text(&session, "value"))
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    stopped.error_for_status().unwrap();
    let default_model_before = fixture
        .until(&default_model_run_id, |run| {
            run["status"] == RunStatus::Cancelled && run["resumeAvailable"] == true
        })
        .await;
    assert_eq!(
        default_model_before["sessionId"],
        default_model_running["sessionId"]
    );
    fixture.stop(false).await;
    s.store
        .change_run(
            &default_model_run_id,
            cairn_installation::runs::RunChanges {
                status: Some(RunStatus::Failed),
                finished_at: Some(Some(now())),
                error: Some(Some("Usage limit has been reached".into())),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let default_plan_count_before_resume = controller.plans.lock().await.len();
    let (default_resume_status, default_resume_response) = api_post(
        &app,
        text(&session, "value"),
        &format!("/api/chats/{default_model_chat_id}/resume"),
        json!({
            "runId": default_model_run_id,
            "turnId": default_model_turn_id,
            "selection": {
                "model": "",
                "reasoning": "high",
                "speed": "standard",
            },
        }),
    )
    .await;
    assert_eq!(
        default_resume_status,
        StatusCode::OK,
        "{default_resume_response}"
    );
    let default_resume_queued = s.store.run(&default_model_run_id).await.unwrap();
    assert_eq!(default_resume_queued["status"], RunStatus::Queued);
    assert_eq!(
        default_resume_queued["snapshot"]["agent"]["model"],
        "gpt-6-sol"
    );
    assert_eq!(
        default_resume_queued["snapshot"]["agent"]["reasoning"],
        "high"
    );
    assert_eq!(
        default_resume_queued["snapshot"]["agent"]["speed"],
        "standard"
    );
    assert_eq!(
        default_resume_queued["chatExecution"]["messageId"],
        default_model_turn_id
    );
    assert_eq!(
        default_resume_queued["sessionId"],
        default_model_before["sessionId"]
    );

    controller
        .hold
        .store(false, std::sync::atomic::Ordering::Relaxed);
    fixture.start().await;
    let default_model_completed = fixture.until_finished(&default_model_run_id).await;
    assert_eq!(default_model_completed["status"], RunStatus::Succeeded);
    assert_eq!(
        default_model_completed["snapshot"]["agent"]["model"],
        "gpt-6-sol"
    );
    assert_eq!(
        default_model_completed["chatExecution"]["messageId"],
        default_model_turn_id
    );
    assert_eq!(
        default_model_completed["sessionId"],
        default_model_before["sessionId"]
    );
    let default_model_plan = controller
        .plans
        .lock()
        .await
        .iter()
        .skip(default_plan_count_before_resume)
        .find(|plan| {
            plan["runId"] == default_model_run_id
                && plan["chat"]["execution"]["messageId"] == default_model_turn_id
        })
        .cloned()
        .expect("the resolved default model must reach the resumed worker plan");
    assert_eq!(default_model_plan["chat"]["provider"], "codex");
    assert_eq!(default_model_plan["chat"]["model"], "gpt-6-sol");
    assert_eq!(default_model_plan["chat"]["reasoning"], "high");
    assert_eq!(default_model_plan["chat"]["serviceTier"], "default");

    controller
        .hold
        .store(true, std::sync::atomic::Ordering::Relaxed);
    let preserved_model_chat = s.chat_create(json!({})).await.unwrap();
    let preserved_model_chat_id = text(&preserved_model_chat, "id").to_owned();
    let mut preserved_model_message = message("fixture:chat-hang");
    preserved_model_message["provider"] = "codex".into();
    preserved_model_message["model"] = "".into();
    preserved_model_message["reasoning"] = "".into();
    preserved_model_message["speed"] = "priority".into();
    let preserved_model_turn_id = text(&preserved_model_message, "id").to_owned();
    s.chat_send(&preserved_model_chat_id, preserved_model_message)
        .await
        .unwrap();
    let preserved_model_run_id = fixture.chat_run(&preserved_model_chat_id).await;
    let preserved_model_running = fixture
        .until(&preserved_model_run_id, |run| {
            run["status"] == RunStatus::Running && run["sessionId"].is_string()
        })
        .await;
    assert_eq!(preserved_model_running["snapshot"]["agent"]["model"], "");
    let stopped = client
        .post(format!(
            "{}/api/chats/{preserved_model_chat_id}/stop",
            fixture.url
        ))
        .header("x-test-relay-token", relay_token)
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    stopped.error_for_status().unwrap();
    let preserved_model_before = fixture
        .until(&preserved_model_run_id, |run| {
            run["status"] == RunStatus::Cancelled && run["resumeAvailable"] == true
        })
        .await;
    let preserved_checkpoint_before = fixture.checkpoint(&preserved_model_run_id).await;
    let preserved_generation_before = preserved_checkpoint_before.get("generation").cloned();
    let preserved_plan_count_before_resume = controller.plans.lock().await.len();
    let (preserve_status, preserve_response) = api_post(
        &app,
        relay_token,
        &format!("/api/chats/{preserved_model_chat_id}/resume"),
        json!({
            "runId": preserved_model_run_id,
            "turnId": preserved_model_turn_id,
            "selection": {
                "model": "",
                "preserveModel": true,
                "reasoning": "high",
                "speed": "standard",
            },
        }),
    )
    .await;
    assert_eq!(preserve_status, StatusCode::OK, "{preserve_response}");
    let preserved_queued = s.store.run(&preserved_model_run_id).await.unwrap();
    assert_eq!(preserved_queued["status"], RunStatus::Queued);
    assert_eq!(preserved_queued["snapshot"]["agent"]["model"], "");
    assert_eq!(preserved_queued["snapshot"]["agent"]["reasoning"], "high");
    assert_eq!(preserved_queued["snapshot"]["agent"]["speed"], "standard");
    for field in ["id", "taskId", "workspace", "sessionId", "chatExecution"] {
        assert_eq!(
            preserved_queued[field], preserved_model_before[field],
            "{field}"
        );
    }
    controller
        .hold
        .store(false, std::sync::atomic::Ordering::Relaxed);
    let preserved_completed = fixture.until_finished(&preserved_model_run_id).await;
    assert_eq!(preserved_completed["status"], RunStatus::Succeeded);
    assert_eq!(preserved_completed["snapshot"]["agent"]["model"], "");
    assert_eq!(
        preserved_completed["snapshot"]["agent"]["reasoning"],
        "high"
    );
    assert_eq!(
        preserved_completed["snapshot"]["agent"]["speed"],
        "standard"
    );
    assert_eq!(
        fixture
            .checkpoint(&preserved_model_run_id)
            .await
            .get("generation")
            .cloned(),
        preserved_generation_before
    );
    let preserved_plan = controller
        .plans
        .lock()
        .await
        .iter()
        .skip(preserved_plan_count_before_resume)
        .find(|plan| {
            plan["runId"] == preserved_model_run_id
                && plan["chat"]["execution"]["messageId"] == preserved_model_turn_id
        })
        .cloned()
        .expect(
            "the preserved blank model and explicit Standard must reach the resumed worker plan",
        );
    assert_eq!(preserved_plan["chat"]["model"], "");
    assert_eq!(preserved_plan["chat"]["reasoning"], "high");
    assert_eq!(preserved_plan["chat"]["serviceTier"], "default");
    assert_eq!(
        preserved_plan["chat"]["sessionId"],
        preserved_model_before["sessionId"]
    );
    assert_eq!(
        preserved_plan["chat"]["execution"]["attachments"],
        preserved_model_before["chatExecution"]["attachments"]
    );

    controller
        .hold
        .store(true, std::sync::atomic::Ordering::Relaxed);
    let alias_chat = s.chat_create(json!({})).await.unwrap();
    let alias_chat_id = text(&alias_chat, "id").to_owned();
    let mut alias_message = message("fixture:chat-hang");
    alias_message["provider"] = "codex".into();
    alias_message["model"] = "my-alias".into();
    alias_message["reasoning"] = "high".into();
    alias_message["speed"] = "priority".into();
    let alias_turn_id = text(&alias_message, "id").to_owned();
    let alias_initial_plan_boundary = controller.plans.lock().await.len();
    s.chat_send(&alias_chat_id, alias_message).await.unwrap();
    let alias_run_id = fixture.chat_run(&alias_chat_id).await;
    let alias_running = fixture
        .until(&alias_run_id, |run| {
            run["status"] == RunStatus::Running && run["sessionId"].is_string()
        })
        .await;
    assert_eq!(alias_running["snapshot"]["agent"]["model"], "my-alias");
    assert_eq!(alias_running["snapshot"]["agent"]["reasoning"], "high");
    assert_eq!(alias_running["snapshot"]["agent"]["speed"], "priority");
    eventually(
        Duration::from_secs(20),
        Duration::from_millis(30),
        async || {
            controller
                .plans
                .lock()
                .await
                .iter()
                .any(|plan| {
                    plan["runId"] == alias_run_id
                        && plan["chat"]["execution"]["messageId"] == alias_turn_id
                })
                .then_some(())
        },
    )
    .await;
    let alias_initial_plan = controller
        .plans
        .lock()
        .await
        .iter()
        .skip(alias_initial_plan_boundary)
        .find(|plan| {
            plan["runId"] == alias_run_id && plan["chat"]["execution"]["messageId"] == alias_turn_id
        })
        .cloned()
        .expect("the current catalog must route the saved alias before it is removed");
    assert_eq!(alias_initial_plan["chat"]["model"], "my-alias");
    assert_eq!(alias_initial_plan["chat"]["reasoning"], "high");
    assert_eq!(alias_initial_plan["chat"]["serviceTier"], "priority");
    let stopped = client
        .post(format!("{}/api/chats/{alias_chat_id}/stop", fixture.url))
        .header("x-test-relay-token", relay_token)
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    stopped.error_for_status().unwrap();
    let alias_before = fixture
        .until(&alias_run_id, |run| {
            run["status"] == RunStatus::Cancelled && run["resumeAvailable"] == true
        })
        .await;
    let alias_checkpoint_before = fixture.checkpoint(&alias_run_id).await;
    let alias_generation_before = alias_checkpoint_before.get("generation").cloned();

    fixture.stop(false).await;
    s.store
        .set(
            "codex-models:",
            json!({
                "checkedAt": now(),
                "models": [
                    {
                        "model": "gpt-6-sol",
                        "isDefault": true,
                        "supportedReasoningEfforts": [{ "reasoningEffort": "high" }],
                        "serviceTiers": [{ "id": "priority" }],
                    },
                    {
                        "model": "gpt-6-luna",
                        "isDefault": false,
                        "supportedReasoningEfforts": [{ "reasoningEffort": "xhigh" }],
                        "serviceTiers": [{ "id": "priority" }],
                    },
                ],
            }),
            None,
        )
        .await
        .unwrap();

    let changed_alias = json!({
        "runId": alias_run_id,
        "turnId": alias_turn_id,
        "selection": {
            "model": "new-alias",
            "reasoning": "high",
            "speed": "standard",
        },
    });
    let (changed_alias_status, _) = api_post(
        &app,
        relay_token,
        &format!("/api/chats/{alias_chat_id}/resume"),
        changed_alias,
    )
    .await;
    assert_eq!(changed_alias_status, StatusCode::BAD_REQUEST);
    let changed_alias_reasoning = json!({
        "runId": alias_run_id,
        "turnId": alias_turn_id,
        "selection": {
            "model": "my-alias",
            "reasoning": "xhigh",
            "speed": "standard",
        },
    });
    let (changed_reasoning_status, _) = api_post(
        &app,
        relay_token,
        &format!("/api/chats/{alias_chat_id}/resume"),
        changed_alias_reasoning,
    )
    .await;
    assert_eq!(changed_reasoning_status, StatusCode::BAD_REQUEST);
    assert_eq!(s.store.run(&alias_run_id).await.unwrap(), alias_before);
    assert_eq!(
        fixture.checkpoint(&alias_run_id).await,
        alias_checkpoint_before
    );

    let alias_plan_boundary = controller.plans.lock().await.len();
    let (alias_resume_status, alias_resume_response) = api_post(
        &app,
        relay_token,
        &format!("/api/chats/{alias_chat_id}/resume"),
        json!({
            "runId": alias_run_id,
            "turnId": alias_turn_id,
            "selection": {
                "model": "my-alias",
                "reasoning": "high",
                "speed": "standard",
            },
        }),
    )
    .await;
    assert_eq!(
        alias_resume_status,
        StatusCode::OK,
        "{alias_resume_response}"
    );
    let alias_queued = s.store.run(&alias_run_id).await.unwrap();
    assert_eq!(alias_queued["status"], RunStatus::Queued);
    assert_eq!(alias_queued["snapshot"]["agent"]["model"], "my-alias");
    assert_eq!(alias_queued["snapshot"]["agent"]["reasoning"], "high");
    assert_eq!(alias_queued["snapshot"]["agent"]["speed"], "standard");
    for field in ["id", "taskId", "workspace", "sessionId", "chatExecution"] {
        assert_eq!(alias_queued[field], alias_before[field], "{field}");
    }
    assert_eq!(alias_queued["chatExecution"]["messageId"], alias_turn_id);

    controller
        .hold
        .store(false, std::sync::atomic::Ordering::Relaxed);
    fixture.start().await;
    let alias_completed = fixture.until_finished(&alias_run_id).await;
    assert_eq!(alias_completed["status"], RunStatus::Succeeded);
    assert_eq!(alias_completed["snapshot"]["agent"]["model"], "my-alias");
    assert_eq!(alias_completed["snapshot"]["agent"]["reasoning"], "high");
    assert_eq!(alias_completed["snapshot"]["agent"]["speed"], "standard");
    assert_eq!(alias_completed["chatExecution"]["messageId"], alias_turn_id);
    for field in ["id", "taskId", "workspace", "sessionId", "chatExecution"] {
        assert_eq!(alias_completed[field], alias_before[field], "{field}");
    }
    assert_eq!(
        fixture
            .checkpoint(&alias_run_id)
            .await
            .get("generation")
            .cloned(),
        alias_generation_before
    );
    let alias_plan = controller
        .plans
        .lock()
        .await
        .iter()
        .skip(alias_plan_boundary)
        .find(|plan| {
            plan["runId"] == alias_run_id
                && plan["chat"]["execution"]["messageId"] == alias_turn_id
                && plan["chat"]["model"] == "my-alias"
        })
        .cloned()
        .expect("the retained saved Codex alias must reach the resumed worker plan");
    assert_eq!(alias_plan["chat"]["reasoning"], "high");
    assert_eq!(alias_plan["chat"]["serviceTier"], "default");
    assert_eq!(alias_plan["chat"]["sessionId"], alias_before["sessionId"]);

    fixture.stop(false).await;
    server.abort();
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
    fixture.clock = Some(FixtureClock::new(fixture.root.path()));
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
    // Ensure the quiet turn has started before advancing its resumed budget.
    eventually(
        Duration::from_secs(20),
        Duration::from_millis(30),
        async || {
            let path = fixture
                .run_directory(id)
                .join("codex/fixture-conversation.json");
            let data = tokio::fs::read(path).await.ok()?;
            let thread: Value = serde_json::from_slice(&data).ok()?;
            thread["turns"]
                .as_array()?
                .iter()
                .any(|turn| turn["status"] == "inProgress")
                .then_some(())
        },
    )
    .await;
    // Real time and the fixture's monotonic timers continue while its wall
    // clock is frozen; the resumed budget must still be intact.
    tokio::time::sleep(Duration::from_millis(1100)).await;
    let still_running = s.store.run(id).await.unwrap();
    assert_eq!(
        still_running["status"],
        RunStatus::Running,
        "{still_running}"
    );
    assert_eq!(fixture.checkpoint(id).await["remainingMs"], 1000);
    fixture.clock.as_mut().unwrap().advance(1250);
    let failed = tokio::time::timeout(
        Duration::from_secs(20),
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
        .update_run_boundary_fixture(run_id, json!({"requestedResources": resources}))
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
    let attempts = s.store.attempts_fixture().await.unwrap();
    assert!(attempts.is_empty(), "{attempts:?}");
    fixture.stop(false).await;
}
