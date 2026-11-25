use crate::common::run_fixture::DbRunFixture;
use common::AgentFixture;
use common::MissionFixture;
use common::StoreAgentFixture;
use common::account_fixture::AccountFixtures;

use common::attempt_fixture::*;
use common::node_fixture::*;
use common::run_fixture::*;

mod common;

#[path = "worker/background_jobs.rs"]
mod background_jobs;

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
    quiescence_observed: Option<Arc<tokio::sync::Notify>>,
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
            antigravity_bin: "agy".into(),
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
            quiescence_observed: None,
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

    /// A session exists before the restart fixture's first turn saves its work.
    async fn until_restart_ready(&self, id: &str) -> Value {
        let marker = self.run_directory(id).join("codex/fixture-restart.json");
        self.until(id, |run| {
            let ready = std::fs::read(&marker)
                .ok()
                .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok());

            run["sessionId"] == "fixture-chat"
                && ready.is_some_and(|saved| saved["cwd"] == run["workspace"])
        })
        .await
    }

    /// Returns the active execution count reported by the live worker process.
    async fn worker_active_runs(&self) -> Option<u64> {
        let response = self
            .service
            .http
            .get(format!("{}/health", self.url))
            .send()
            .await
            .ok()?;
        let health = response.json::<Value>().await.ok()?;
        health["activeRuns"].as_u64()
    }

    /// Waits for the live worker to remove every execution from its active set.
    async fn until_worker_quiescent(&self) -> u64 {
        eventually(
            Duration::from_secs(60),
            Duration::from_millis(30),
            async || {
                let active_runs = self.worker_active_runs().await?;
                if active_runs > 0 {
                    if let Some(observed) = &self.quiescence_observed {
                        observed.notify_one();
                    }
                    return None;
                }
                Some(active_runs)
            },
        )
        .await
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

const PNG: &[u8] = b"\x89PNG\r\n\x1a\nfixture";

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
            "diskRetirementProtocol": 2,
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
    if path.starts_with("/disks/") && path.ends_with("/delete") && request.method() == "POST" {
        let bytes = axum::body::to_bytes(request.into_body(), 4096)
            .await
            .unwrap();
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        return Json(json!({ "deleted":true,"retirementId":body["retirementId"] })).into_response();
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
    if request.method() == "DELETE" {
        return Json(json!({})).into_response();
    }
    if !path.starts_with("/runs/") {
        return StatusCode::NOT_FOUND.into_response();
    }
    let attempt = path.split('/').nth(2).unwrap();
    if (path.ends_with("/logs") || path.ends_with("/wait"))
        && !state
            .plans
            .lock()
            .await
            .iter()
            .any(|plan| plan["id"] == attempt)
    {
        return StatusCode::NOT_FOUND.into_response();
    }
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

#[path = "worker/accounts_and_artifacts.rs"]
mod accounts_and_artifacts;
#[path = "worker/capacity_and_access.rs"]
mod capacity_and_access;
#[path = "worker/lifecycle.rs"]
mod lifecycle;
#[path = "worker/opencode_resume.rs"]
mod opencode_resume;
#[path = "worker/retries_and_recovery.rs"]
mod retries_and_recovery;
