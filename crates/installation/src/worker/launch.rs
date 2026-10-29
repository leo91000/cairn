use super::{
    Admission,
    checkpoint::{Checkpoint, RunCheckpoint},
    output,
};
use crate::{
    accounts::{Lease, broker},
    config::now,
    error::{Error, Result},
    execution,
    nodes::{self, placement},
    performance::{Activity, Operation},
    process::Environment,
    provider::Provider,
    recovery, run_output,
    run_status::RunStatus,
    service::{Service, covers, policy},
    skills::{atomic_write, private_dir},
    storage::policy::Policy,
    supervisor::Supervised,
    validation::text,
};
use cairn_contracts::EventKind;
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::Instrument;

/// Exit code reported for processes the worker had to stop.
const STOPPED: i32 = 143;
const SUMMARY_LIMIT: usize = 100_000;
const HISTORY_LIMIT: usize = 16_000;
const RESUME_PROMPT: &str = "Continue the same task from the saved conversation and current workspace. Execution was interrupted. Resume the original task from its last completed step. Preserve completed work and verify external effects before repeating any action.";

/// Runs `run` until it finishes, retrying on another account when usage runs out.
pub(super) async fn execute(
    s: &Arc<Service>,
    run: &mut Value,
    admission: &mut Admission,
    cancel: &CancellationToken,
    checkpoint: &Checkpoint,
    existing: bool,
    sensitive: &mut Vec<String>,
) -> Result<()> {
    let span = tracing::info_span!(target: "cairn_performance", "agent_execution", run_id = text(run, "id"));
    execute_inner(s, run, admission, cancel, checkpoint, existing, sensitive)
        .instrument(span)
        .await
}

async fn execute_inner(
    s: &Arc<Service>,
    run: &mut Value,
    admission: &mut Admission,
    cancel: &CancellationToken,
    checkpoint: &Checkpoint,
    existing: bool,
    sensitive: &mut Vec<String>,
) -> Result<()> {
    let id = text(run, "id").to_owned();
    let mut timing = Operation::new("agent_prepare", &id, "mark_running");
    let mut execution = Execution {
        s,
        provider: Provider::of_run(run)?,
        directory: s.config.data_dir.join("runs").join(&id),
        id,
        run,
        account: &mut admission.account,
        preparation: admission.preparation.take(),
        cancel,
        checkpoint,
        sensitive,
        attempt_id: String::new(),
        reservation: None,
    };
    execution.mark_running().await?;
    timing.next("access");
    execution.refresh_access().await?;
    execution.resolve_backend().await?;
    timing.next("workspace");
    let mut workspace = execution.prepare(existing).await?;
    timing.next("session_lookup");
    let mut resume = execution.resume_session(&workspace).await?;
    timing.finish();
    let mut log_total = 0;
    loop {
        match execution
            .attempt(&mut workspace, resume.as_deref(), &mut log_total)
            .await?
        {
            Attempt::Finished => return Ok(()),
            Attempt::Resume(session) => resume = Some(session),
        }
    }
}

/// A controller may have accepted a first attempt before its engine created a
/// session. Prefer any recoverable session; only that controlled native restart
/// can start afresh when none was recorded. Ordinary and VM recovery stay strict.
async fn saved_session(
    s: &Service,
    run: &Value,
    home: &Path,
    cwd: &Path,
) -> Result<Option<String>> {
    match recovery::session(s, run, home, cwd).await {
        Ok(session) => Ok(Some(session)),
        Err(error)
            if error.status == 409
                && crate::native::policy::native(&run["snapshot"]["agent"])
                && run["nativeModeInterruption"].is_string()
                && text(run, "sessionId").is_empty() =>
        {
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

struct Execution<'a> {
    s: &'a Arc<Service>,
    id: String,
    run: &'a mut Value,
    account: &'a mut Option<Lease>,
    preparation: Option<tokio::sync::OwnedMutexGuard<()>>,
    cancel: &'a CancellationToken,
    checkpoint: &'a Checkpoint,
    sensitive: &'a mut Vec<String>,
    provider: Provider,
    directory: PathBuf,
    attempt_id: String,
    reservation: Option<(String, Value)>,
}

/// The prepared execution environment shared by every launch attempt.
struct Workspace {
    prepared: Value,
    state_home: PathBuf,
    env: Environment,
    mcp: Value,
}

impl Workspace {
    fn isolated(&self) -> bool {
        self.prepared["isolated"] == true
    }

    fn cwd(&self) -> &Path {
        Path::new(text(&self.prepared, "cwd"))
    }

    fn output(&self) -> &str {
        text(&self.prepared, "output")
    }
}

struct Launch {
    binary: String,
    args: Vec<String>,
    prompt: String,
}

#[derive(Default)]
struct Exit {
    code: Option<i32>,
    timed_out: bool,
    exhausted: bool,
}

enum Attempt {
    Finished,
    /// Usage ran out; continue the saved session, possibly on another account.
    Resume(String),
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RunnerPlan<'a> {
    id: &'a str,
    run_id: &'a str,
    resume_workspace: bool,
    args: &'a [String],
    cwd: &'a Value,
    project_root: &'a Value,
    prompt: String,
    imports: Vec<Value>,
    expires: Option<i64>,
    sandbox: Value,
    mcp_env: &'a Value,
    resources: &'a Value,
    node_lease_required: bool,
    native_mode: Value,
    backend: &'a Value,
    runtime_id: &'a Value,
    storage: RunnerStorage<'a>,
    chat: Value,
}

#[derive(Serialize)]
struct RunnerStorage<'a> {
    master: &'a str,
    grant: String,
    policy: Policy,
}

impl Execution<'_> {
    /// Reserve before preparing paths: VM and native environments differ. Binding
    /// the concrete backend shares the atomic admission transaction, so retries
    /// cannot turn an already-created environment into another execution kind.
    async fn resolve_backend(&mut self) -> Result<()> {
        if policy(&self.run["snapshot"]["agent"])["backend"] != "auto" {
            return Ok(());
        }
        let runner = crate::config::id();
        let placement = placement::reserve(self.s, self.run, &runner).await?;
        *self.run = self.s.store.run(&self.id).await?;
        if !execution::uses_vm(self.run, &self.s.config) {
            // Development's shared process has no controller or retained node disk.
            // Keep the selected backend, but never give recovery a node to contact.
            placement::release(self.s, &runner).await?;
            return Ok(());
        }
        let saved_runner = runner.clone();
        let node = placement["nodeId"].as_str().map(str::to_owned);
        let runtime = placement["runtimeId"].clone();
        self.checkpoint
            .update(|checkpoint| {
                checkpoint.runner_id = Some(Some(saved_runner));
                checkpoint.node_id = Some(node);
                checkpoint.runtime_id = Some(Some(runtime));
            })
            .await?;
        self.reservation = Some((runner, placement));
        Ok(())
    }

    fn stopping(&self) -> bool {
        self.cancel.is_cancelled() || self.s.shutdown.is_cancelled()
    }

    async fn mark_running(&mut self) -> Result<()> {
        let s = self.s;
        let account_name = match self.account.as_ref() {
            Some(account) => {
                serde_json::to_value(s.accounts.account(s, &account.account_id).await?.name)?
            }
            None => Value::Null,
        };
        let mut patch = crate::runs::RunChanges {
            status: Some(RunStatus::Running),
            started_at: Some(Some(self.run["startedAt"].as_i64().unwrap_or_else(now))),
            finished_at: Some(None),
            account_wait_reason: Some(None),
            account_required: Some(None),
            account_id: Some(
                self.account
                    .as_ref()
                    .map(|account| account.account_id.clone()),
            ),
            account_name: Some(account_name.as_str().map(str::to_owned)),
            ..Default::default()
        };
        if let Some(mut retry) = s.store.run_state(&self.id).await?.retry {
            retry.next_attempt_at = None;
            patch.retry = Some(Some(retry));
        }
        let id = self.id.clone();
        let account_message = account_name
            .as_str()
            .map(|name| format!("Using {} account: {name}", self.provider.label()));
        self.checkpoint
            .update_and_publish(
                |state| state.capacity_retry_at = None,
                move |db| {
                    db.change_run(&id, patch)?;
                    if let Some(message) = account_message {
                        db.event(&id, &EventKind::Status, &message, None)?;
                    }
                    // Marks where an attempt begins, for labels that last one attempt.
                    db.event(
                        &id,
                        &EventKind::Status,
                        "Preparing workspace",
                        Some(&json!({ "attempt": true })),
                    )
                },
            )
            .await?;
        private_dir(&self.directory).await
    }

    /// Refuses grants reduced since the run was queued. The run keeps the grants it
    /// was queued with, except node grants: placement follows the current ones,
    /// including whether execution needs a VM.
    async fn refresh_access(&mut self) -> Result<()> {
        let s = self.s;
        let current = s
            .agent_snapshot(text(&self.run["snapshot"]["agent"], "id"))
            .await?;
        nodes::require_node(&current)?;
        if !covers(&current, &self.run["snapshot"]["agent"]) {
            return Err(Error::conflict(
                "Agent access was reduced after this run was queued. Run the task again with the current policy.",
            ));
        }
        let current_policy = policy(&current);
        self.run["snapshot"]["agent"]["access"]["nodes"] = current_policy["nodes"].clone();
        let snapshot = serde_json::from_value(self.run["snapshot"].clone())?;
        s.store
            .change_run(
                &self.id,
                crate::runs::RunChanges {
                    snapshot: Some(snapshot),
                    ..Default::default()
                },
            )
            .await?;
        Ok(())
    }

    async fn prepare(&mut self, existing: bool) -> Result<Workspace> {
        let s = self.s;
        let mut timing = Operation::new("workspace_prepare", &self.id, "execution");
        let (prepared, credentials) = self.prepare_execution(existing).await?;
        // Plugin credentials written into the run are masked wherever they appear.
        for secret in &credentials.secrets {
            if !self.sensitive.contains(secret) {
                self.sensitive.push(secret.clone());
            }
        }
        timing.next("account_home");
        let state_home = self.prepare_state_home(&prepared).await?;
        let patch = crate::runs::RunChanges {
            workspace: Some(serde_json::from_value(prepared["cwd"].clone())?),
            boundary: Some(serde_json::json!({
                "workspaces": prepared["workspaces"],
                "isolated": prepared["isolated"],
                "executionBackend": prepared["backend"],
            })),
            ..Default::default()
        };
        s.store.change_run(&self.id, patch).await?;
        *self.run = s.store.run(&self.id).await?;
        timing.next("environment");
        let mut env = self
            .environment(prepared["isolated"] == true, &state_home)
            .await?;
        timing.next("mcp_configuration");
        let mcp = s.mcps.run_configuration(s, self.run).await?;
        self.sensitive.extend(
            mcp["redactions"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(str::to_owned),
        );
        if let Some(account) = self.account.as_ref() {
            self.sensitive
                .extend(s.accounts.redactions(s, account).await?);
        }
        for (key, value) in mcp["env"].as_object().unwrap() {
            env.insert(key.clone(), value.as_str().unwrap_or("").into());
        }
        timing.finish();
        Ok(Workspace {
            prepared,
            state_home,
            env,
            mcp,
        })
    }

    /// Prepares the workspace, or restores the one saved before a restart.
    async fn prepare_execution(
        &self,
        existing: bool,
    ) -> Result<(Value, crate::plugins::run_credentials::RunCredentials)> {
        let s = self.s;
        if existing && self.checkpoint.read(|c| c.prepared().is_none()).await {
            // A fresh generation keeps a half-prepared workspace from being reused.
            let generation = crate::config::id()[..8].to_owned();
            self.checkpoint
                .update(|c| c.generation = Some(generation))
                .await?;
        }
        let saved = self.checkpoint.snapshot().await;
        let agent_id = text(&self.run["snapshot"]["agent"], "id");
        let projects = crate::project_workspaces::catalog(self.run);
        let credentials = crate::plugins::run_credentials::for_run(s, agent_id, &projects).await?;
        let prepared = match saved.prepared() {
            Some(prepared) => {
                execution::restore(self.run, prepared.clone(), &s.config, &credentials).await?
            }
            None => {
                let account_home = self.account.as_ref().map(|a| a.home.as_path());
                execution::prepare(
                    self.run,
                    &s.config,
                    &credentials,
                    account_home,
                    saved.generation.as_deref(),
                )
                .await?
            }
        };
        let saved_prepared = prepared.clone();
        self.checkpoint
            .update(|c| c.prepared = Some(Some(saved_prepared)))
            .await?;
        Ok((prepared, credentials))
    }

    async fn prepare_state_home(&mut self, prepared: &Value) -> Result<PathBuf> {
        let isolated = prepared["isolated"] == true;
        let adapter = self.provider.adapter();
        let home = adapter.state_home(&self.directory, isolated);
        adapter
            .prepare_home(self.s, &home, isolated, self.account.as_mut())
            .await?;
        Ok(home)
    }

    async fn environment(&self, isolated: bool, state_home: &Path) -> Result<Environment> {
        let config = &self.s.config;
        let mut env = if isolated {
            std::env::vars().collect::<Environment>()
        } else {
            crate::toolkit::environment(&config.home, std::env::vars().collect()).await?
        };
        crate::process::remove_storage_environment(&mut env);
        env.insert("HOME".into(), config.home.to_string_lossy().into_owned());
        let home = self
            .account
            .as_ref()
            .map_or(state_home, |account| &account.home);
        self.provider
            .configure_run_environment(&mut env, &self.directory, isolated, home);
        Ok(env)
    }

    /// Finds the conversation to continue when a previous launch already started it.
    async fn resume_session(&self, workspace: &Workspace) -> Result<Option<String>> {
        let s = self.s;
        if !self.checkpoint.read(RunCheckpoint::launched).await {
            return Ok(None);
        }
        let pending_startup = self
            .checkpoint
            .read(|saved| {
                saved
                    .retry_cause
                    .as_ref()
                    .and_then(Option::as_ref)
                    .is_some_and(|cause| cause.startup)
            })
            .await;
        if !self.run["sessionId"].is_string() && pending_startup {
            // The adapter confirmed no new turn was submitted; preserve the same
            // prepared workspace and retry its initial authentication safely.
            return Ok(None);
        }
        let session = match saved_session(s, self.run, &workspace.state_home, workspace.cwd()).await
        {
            Ok(session) => session,
            Err(error) => {
                // Local session discovery happens before any new turn is submitted.
                // Preserve its specific transient failure instead of the previous cause.
                if let Some(failure) = crate::run_retry::startup_failure(&error) {
                    let cause = crate::run_retry::Cause::classify(&failure["error"]);
                    let id = self.id.clone();
                    let message = error.message.clone();
                    self.checkpoint
                        .update_and_publish(
                            |saved| {
                                saved.completed = Some(false);
                                saved.last_error = Some(Some(error.message.clone()));
                                saved.retry_cause = Some(cause);
                            },
                            move |db| {
                                db.event(&id, &EventKind::TurnFailed, &message, Some(&failure))
                            },
                        )
                        .await?;
                }
                return Err(error);
            }
        };
        let Some(session) = session else {
            s.store.event(
                &self.id, &EventKind::Status,
                "Restarting with preserved files after the node mode change; no resumable engine session was recorded.",
                None,
            ).await?;
            return Ok(None);
        };
        let patch = crate::runs::RunChanges {
            session_id: Some(Some((session).to_owned())),
            resume_count: Some((self.run["resumeCount"].as_u64().unwrap_or(0) + 1) as u32),
            resume_available: Some(true),
            ..Default::default()
        };
        let id = self.id.clone();
        s.store
            .transaction(move |db| {
                db.change_run(&id, patch)?;
                db.event(
                    &id,
                    &EventKind::Status,
                    "Resuming saved conversation and workspace",
                    None,
                )
            })
            .await?;
        Ok(Some(session))
    }

    async fn attempt(
        &mut self,
        workspace: &mut Workspace,
        resume: Option<&str>,
        log_total: &mut usize,
    ) -> Result<Attempt> {
        let s = self.s;
        self.attempt_id = crate::config::id();
        if self.stopping() {
            return Err(Error::conflict("Execution stopped."));
        }
        if self.checkpoint.expired() {
            return Err(Error::conflict("Run exceeded its time limit."));
        }
        let output = workspace.output().to_owned();
        match tokio::fs::remove_file(&output).await {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        let adapter = self.provider.adapter();
        let isolated = workspace.isolated();
        let home = self
            .account
            .as_ref()
            .map_or(&workspace.state_home, |account| &account.home);
        self.provider.configure_run_environment(
            &mut workspace.env,
            &self.directory,
            isolated,
            home,
        );
        let mut launch = Launch {
            binary: adapter.binary(&s.config).to_owned(),
            args: Vec::new(),
            prompt: self.prompt(resume).await?,
        };
        let auth_broker = match self.account.as_ref() {
            Some(account) => Some(broker::serve(s, account).await?),
            None => None,
        };
        // A VM reaches its run-scoped MCP only through this channel.
        let granted = workspace.mcp["env"]
            .get(crate::mcps::RUN_TOKEN_ENV)
            .is_some();
        let mcp_channel = if workspace.isolated() && granted {
            let home = self.directory.join("home");
            Some(crate::mcps::channel::serve(s, &home, &self.id).await?)
        } else {
            None
        };
        let chat = self.chat_plan(workspace, resume, &mut launch).await?;
        if workspace.isolated() {
            self.prepare_runner(workspace, resume, chat, &mut launch)
                .await?;
        }
        let exit = self.supervise(workspace, launch, log_total).await?;
        drop(auth_broker);
        drop(mcp_channel);
        self.checkpoint.update(|c| c.process = Some(None)).await?;
        let saved = self.checkpoint.snapshot().await;
        if s.shutdown.is_cancelled() && !self.cancel.is_cancelled() && !saved.completed() {
            return Err(Error::conflict("Worker is restarting"));
        }
        let session = s.store.run(&self.id).await?["sessionId"]
            .as_str()
            .map(str::to_owned);
        let stopped_by_controller =
            !self.cancel.is_cancelled() && !exit.timed_out && !exit.exhausted;
        let managed = matches!(
            workspace.prepared["backend"].as_str(),
            Some("firecracker" | "native")
        );
        if managed
            && stopped_by_controller
            && s.store.run(&self.id).await?["nativeModeInterruption"].is_string()
        {
            return Err(Error::unavailable(
                "Native node mode changed. Managed executions stopped; saved files and conversation history are retained for restart.",
            ));
        }
        let interrupted = exit.code == Some(crate::runner::CONTROLLER_INTERRUPTED);
        if managed && stopped_by_controller && interrupted {
            if saved.start_rejected == Some(true) {
                return Err(Error::unavailable(format!(
                    "{} Working files were preserved; retry when the controller is available.",
                    saved.last_error(),
                )));
            }

            let controller = controller_name(self.run);
            let message = if session.is_some() {
                format!(
                    "{controller} interrupted execution. The saved conversation and workspace have been preserved."
                )
            } else {
                format!(
                    "{controller} interrupted execution. Working files were preserved, but no resumable agent session was confirmed."
                )
            };
            return Err(Error::unavailable(message));
        }

        // Remote nodes retry unavailable attempts without a cap; bound this
        // possibly deterministic failure here instead.
        let controller_failed = exit.code == Some(crate::runner::CONTROLLER_FAILED);
        let may_retry = saved.controller_recoveries() < super::execution::MAX_CONTROLLER_RECOVERIES;
        if managed && stopped_by_controller && controller_failed && may_retry {
            return Err(Error::unavailable(format!(
                "{} failed while running this attempt. The saved conversation and workspace have been preserved.",
                controller_name(self.run),
            )));
        }

        // Preserve the controller's outcome before cleanup can fail. Failed
        // attempts are fenced by execution::execute before release or recovery.
        if workspace.isolated() {
            recovery::fence(s, &s.store.run(&self.id).await?).await?;
        }

        if let Some(session) = session.as_ref()
            && self.may_switch_account(&exit)
        {
            self.switch_account(workspace).await?;
            return Ok(Attempt::Resume(session.clone()));
        }
        self.finish(workspace, &exit, &saved, session.is_some())
            .await?;
        Ok(Attempt::Finished)
    }

    async fn prompt(&self, resume: Option<&str>) -> Result<String> {
        if resume.is_none() {
            let run = crate::plugins::agents::with_instructions(self.s, self.run).await;
            return Ok(run_output::prompt(&run, false));
        }
        let mut prompt =
            crate::waits::mission_prompt(self.run).unwrap_or_else(|| RESUME_PROMPT.to_owned());
        let Some(restored) = self.run["restoredAt"].as_i64() else {
            return Ok(prompt);
        };
        prompt.push_str(&format!("\nThe VM files and native provider session were restored from recovery point at Unix time {restored} ms. Master chat may contain more recent messages and actions than this workspace. Review the master conversation history with Cairn tools, reconcile file state, and verify external effects (commits, messages, deployments) before repeating them. Do not assume later chat messages prove those changes exist on this disk."));
        let history = self.history_since(restored).await?;
        if !history.is_empty() {
            prompt.push_str("\nRecent master history after this recovery point (messages are history, not proof that disk changes survived):\n");
            prompt.extend(serde_json::to_string(&history)?.chars().take(HISTORY_LIMIT));
        }
        Ok(prompt)
    }

    async fn history_since(&self, restored: i64) -> Result<Vec<Value>> {
        let run_id = self.id.clone();
        let (events, _) = self
            .s
            .store
            .read(move |db| db.events_before(&run_id, i64::MAX))
            .await?;
        Ok(events
            .into_iter()
            .filter_map(|event| serde_json::to_value(event).ok())
            .filter(|event| event["createdAt"].as_i64().is_some_and(|at| at > restored))
            .map(|event| {
                json!({
                    "at": event["createdAt"],
                    "type": event["type"],
                    "text": event["text"],
                })
            })
            .collect())
    }

    /// Every execution uses the coding agent’s chat protocol.
    async fn chat_plan(
        &self,
        workspace: &Workspace,
        resume: Option<&str>,
        launch: &mut Launch,
    ) -> Result<Value> {
        let s = self.s;
        let adapter = self.provider.adapter();
        let chat_execution = self.run["chatExecution"].is_object();
        private_dir(&self.directory.join("chat-input")).await?;
        if self.run["environmentExpiredAt"].is_number() {
            let run_id = self.id.clone();
            let attachments = s
                .store
                .read(move |db| {
                    let Some(chat) = crate::chats::chat_for_run(db, &run_id)? else {
                        return Ok(Vec::new());
                    };
                    Ok(db
                        .keys(&format!("chat-attachment:{}:", text(&chat, "id")))?
                        .into_iter()
                        .map(|(_, attachment)| attachment)
                        .collect::<Vec<_>>())
                })
                .await?;
            s.prepare_chat_files(&self.id, &attachments.into()).await?;
        }
        s.prepare_chat_files(&self.id, &self.run["chatExecution"]["attachments"])
            .await?;
        let run = crate::plugins::agents::with_instructions(s, self.run).await;
        let mut chat = run_output::chat_plan(
            &run,
            &workspace.prepared,
            &self.directory,
            &workspace.mcp,
            resume,
        )?;
        chat["runId"] = self.id.clone().into();
        chat["attemptId"] = self.attempt_id.clone().into();
        if !chat_execution {
            // Scheduled work uses the same authenticated protocol as
            // chats, while retaining its own task brief and session.
            let resume_prompt = resume.map(|_| launch.prompt.as_str());
            chat["execution"] = run_output::scheduled_turn(self.run, resume_prompt);
        }
        adapter
            .configure_plan(s, &mut chat, self.account.as_ref())
            .await?;
        launch.binary = current_exe()?;
        launch.args = vec!["chat".into(), adapter.binary(&s.config).to_owned()];
        launch.prompt = chat.to_string();
        Ok(chat)
    }

    /// Places the run on a node and routes the launch through its isolated runner.
    async fn prepare_runner(
        &mut self,
        workspace: &mut Workspace,
        resume: Option<&str>,
        chat: Value,
        launch: &mut Launch,
    ) -> Result<()> {
        let s = self.s;
        crate::object_storage::Storage::configured(s)?;
        let mut timing = Operation::new("runner_prepare", &self.id, "placement");
        let (runner, placement) = match self.reservation.take() {
            Some(reservation) => reservation,
            None => {
                let runner = crate::config::id();
                let placement =
                    placement::reserve(s, &s.store.run(&self.id).await?, &runner).await?;
                (runner, placement)
            }
        };
        timing.next("materialize");
        placement::materialize(s, &runner).await?;
        let node_id = placement["nodeId"].as_str().map(str::to_owned);
        let runtime_id = placement["runtimeId"].clone();
        self.checkpoint
            .update(|c| {
                c.runner_id = Some(Some(runner.clone()));
                c.node_id = Some(node_id);
                c.runtime_id = Some(Some(runtime_id));
            })
            .await?;
        drop(self.preparation.take());
        s.worker.notify();
        timing.next("plan");
        self.write_runner_plan(workspace, &runner, &placement, resume, chat, launch)
            .await?;
        timing.next("remote_prepare");
        let runner_url = nodes::transport::url(s, &self.id).await?;
        if placement["nodeId"] != nodes::LOCAL_NODE_ID {
            self.prepare_remote(&runner_url, &runner, text(&placement, "nodeId"))
                .await?;
        }
        timing.next("placement_commit");
        let patch = json!({
            "nodeId": placement["nodeId"],
            "resources": placement["resources"],
            "nodeState": null,
            "movementError": null,
        });
        s.store.update_run_boundary(&self.id, patch).await?;
        let runner_token = execution::secret(&s.config.data_dir, "runner-secret").await?;
        workspace.env.insert("RUNNER_URL".into(), runner_url);
        workspace.env.insert("RUNNER_TOKEN".into(), runner_token);
        launch.binary = current_exe()?;
        launch.args = vec!["runner-client".into(), runner];
        timing.finish();
        Ok(())
    }

    async fn write_runner_plan(
        &self,
        workspace: &Workspace,
        runner: &str,
        placement: &Value,
        resume: Option<&str>,
        mut chat: Value,
        launch: &Launch,
    ) -> Result<()> {
        let s = self.s;
        let prepared = &workspace.prepared;
        let plans = s.config.data_dir.join("runner-plans");
        private_dir(&plans).await?;
        let mut imports = prepared["mounts"].as_array().unwrap().clone();
        imports.push(json!({
            "source": self.directory.join("chat-input"),
            "target": "/run/cairn-chat",
            "readOnly": true,
        }));
        let mut context = crate::plugins::agents::with_instructions(s, self.run).await;
        context["snapshot"]["skills"] = prepared["skills"].clone();
        context["nativeMode"] = placement["nativeMode"].clone();
        if prepared["backend"] == "native" {
            run_output::native_chat_privileges(&mut chat, &context, &placement["nativeMode"]);
        }
        let prompt = if resume.is_some() {
            launch.prompt.clone()
        } else {
            run_output::prompt(&context, false)
        };
        let prompt = if prepared["backend"] == "native" {
            format!(
                "{}\n\n{prompt}",
                run_output::native_privileges(&placement["nativeMode"])
            )
        } else {
            prompt
        };
        let storage_node = text(placement, "nodeId");
        let node_storage = if prepared["backend"] == "native" {
            let (run, attempt, placement) = (self.id.clone(), runner.to_owned(), placement.clone());
            s.store
                .transaction(move |db| {
                    nodes::native_mode::acknowledge_plan(db, &run, &attempt, &placement)
                })
                .await?
        } else {
            s.store
                .node_boundary(storage_node)
                .await?
                .unwrap_or_default()["storage"]
                .clone()
        };
        let storage_policy = crate::storage::policy::for_node(&node_storage)?;
        let grant = nodes::disk_grants::new_disk(s, self.run, storage_node).await?;
        s.store
            .update_run_boundary(&self.id, json!({ "storage": { "mode": "on-demand" } }))
            .await?;
        placement::renew_local(s, &self.id).await?;
        let resume_workspace = resume.is_some()
            || self
                .checkpoint
                .read(|saved| saved.workspace_initialized == Some(true))
                .await;
        let plan = RunnerPlan {
            id: runner,
            run_id: &self.id,
            resume_workspace,
            args: &launch.args,
            cwd: &prepared["cwd"],
            project_root: &prepared["projectRoot"],
            prompt,
            imports,
            expires: self.checkpoint.deadline(),
            sandbox: policy(&self.run["snapshot"]["agent"])["sandbox"].clone(),
            mcp_env: &workspace.mcp["env"],
            resources: &placement["resources"],
            node_lease_required: true,
            native_mode: placement["nativeMode"].clone(),
            backend: &prepared["backend"],
            runtime_id: &placement["runtimeId"],
            storage: RunnerStorage {
                master: &s.config.public_url,
                grant,
                policy: storage_policy,
            },
            chat,
        };
        atomic_write(
            &plans.join(format!("{runner}.json")),
            &serde_json::to_vec(&plan)?,
        )
        .await
    }

    async fn prepare_remote(&self, runner_url: &str, runner: &str, node_id: &str) -> Result<()> {
        let s = self.s;
        let response = s
            .http
            .post(format!("{runner_url}/prepare/{runner}"))
            .bearer_auth(execution::secret(&s.config.data_dir, "runner-secret").await?)
            .timeout(Duration::from_secs(300))
            .send()
            .await
            .map_err(|_| Error::unavailable("Remote workspace preparation interrupted."))?;
        if !response.status().is_success() {
            let status = response.status().as_u16();
            if status == 504 {
                return Err(Error::unavailable(
                    "Remote workspace preparation timed out after 280 seconds; source files are preserved.",
                ));
            }
            return Err(Error::new(
                status,
                format!(
                    "Remote workspace preparation failed (HTTP {status}); source files are preserved.",
                ),
            ));
        }
        let message = format!("Executing on node {node_id}");
        s.store
            .event(&self.id, &EventKind::Status, &message, None)
            .await
    }

    /// Launches the supervised process and records its output until it exits.
    async fn supervise(
        &mut self,
        workspace: &Workspace,
        launch: Launch,
        log_total: &mut usize,
    ) -> Result<Exit> {
        let s = self.s;
        let previously_launched = self.checkpoint.read(RunCheckpoint::launched).await;
        let mut child = Supervised::spawn(
            &launch.binary,
            &launch.args,
            &workspace.env,
            workspace.cwd(),
        )
        .await?;
        let identity = recovery::process_identity(child.child.id().unwrap()).await?;
        if self.run["chatExecution"].is_object() {
            self.run["chatExecution"]["recovery"] = true.into();
            let patch = json!({ "chatExecution": self.run["chatExecution"] });
            s.store.update_run_boundary(&self.id, patch).await?;
        }
        self.checkpoint
            .update(|c| {
                c.process = Some(identity);
                c.launched = Some(true);
                c.start_rejected = Some(false);
                c.retry_cause = Some(None);
                c.completed = Some(false);
                c.last_error = Some(None);
            })
            .await?;
        if self.stopping() {
            child.stop().await;
            return Err(Error::conflict("Execution stopped before launch."));
        }
        let stdout = child.child.stdout.take().unwrap();
        let stderr = child.child.stderr.take().unwrap();
        let (tx, mut events) = mpsc::channel(32);
        let out = tokio::spawn(output::read_output(
            stdout,
            false,
            tx.clone(),
            self.sensitive.to_vec(),
        ));
        let err = tokio::spawn(output::read_output(stderr, true, tx, Vec::new()));
        if let Err(error) = child.start(launch.prompt).await {
            child.stop().await;
            out.abort();
            err.abort();
            return Err(error);
        }
        let span = tracing::info_span!(target: "cairn_performance", "agent_attempt", run_id = self.id, attempt_id = self.attempt_id);
        let exit = self
            .wait(&mut child, &mut events, log_total, previously_launched)
            .instrument(span)
            .await?;
        let _ = out.await;
        let _ = err.await;
        Ok(exit)
    }

    async fn wait(
        &mut self,
        child: &mut Supervised,
        events: &mut mpsc::Receiver<output::Output>,
        log_total: &mut usize,
        previously_launched: bool,
    ) -> Result<Exit> {
        let s = self.s;
        let cancel = self.cancel;
        let mut exit = Exit::default();
        let mut open = true;
        let mut activity = Activity::new(&self.id, &self.attempt_id, "worker", self.provider);
        let mut heartbeat = crate::performance::heartbeat();
        let mut timeout = tokio::time::interval(Duration::from_millis(250));
        timeout.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        while exit.code.is_none() || open {
            let running = exit.code.is_none();
            tokio::select! {
                () = cancel.cancelled(), if running => {
                    child.stop().await;
                    exit.code = Some(STOPPED);
                }
                () = s.shutdown.cancelled(), if running => {
                    child.stop().await;
                    exit.code = Some(STOPPED);
                }
                _ = timeout.tick(), if running => {
                    // The controller owns confirmed suspension timing. An unavailable
                    // observation must not turn an administratively paused VM into a timeout.
                    if self.checkpoint.expired()
                        && self.checkpoint.refresh_pause(s).await.is_ok()
                        && self.checkpoint.expired()
                    {
                        exit.timed_out = true;
                        child.stop().await;
                        exit.code = Some(STOPPED);
                    }
                }
                code = child.child.wait(), if running => {
                    exit.code = Some(code?.code().unwrap_or(STOPPED));
                }
                _ = heartbeat.tick() => activity.heartbeat("receive_output"),
                event = events.recv(), if open => match event {
                    Some(output) => {
                        let queue_ms = output.received.elapsed().as_millis() as u64;
                        if queue_ms >= 100 {
                            tracing::info!(target: "cairn_performance", operation = "agent_output", run_id = self.id, attempt_id = self.attempt_id, phase = "queue", elapsed_ms = queue_ms);
                        }
                        if output.kind == output::OutputKind::Stdout {
                            activity.output();
                        }
                        crate::performance::wait("refresh_redactions", self.refresh_redactions()).await?;
                        exit.exhausted |= crate::performance::wait("persist_output", output::record(
                            s,
                            &output,
                            self.checkpoint,
                            previously_launched,
                            self.sensitive,
                            log_total,
                            &mut activity,
                        )).await?;
                    }
                    None => open = false,
                },
            }
        }
        activity.finish();
        Ok(exit)
    }

    /// Account credentials may rotate while the process runs.
    async fn refresh_redactions(&mut self) -> Result<()> {
        let Some(account) = self.account.as_ref() else {
            return Ok(());
        };
        for secret in self.s.accounts.redactions(self.s, account).await? {
            if !self.sensitive.contains(&secret) {
                self.sensitive.push(secret);
            }
        }
        Ok(())
    }

    fn may_switch_account(&self, exit: &Exit) -> bool {
        exit.exhausted
            && exit.code != Some(0)
            && self.account.is_some()
            && !self.stopping()
            && !exit.timed_out
    }

    /// Parks the exhausted account and waits for another one to continue the session.
    async fn switch_account(&mut self, workspace: &Workspace) -> Result<()> {
        let s = self.s;
        let label = self.provider.label();
        let previous = self.account.take().unwrap();
        let model = text(&self.run["snapshot"]["agent"], "model").to_owned();
        s.accounts
            .exhausted(s, &previous.account_id, &model)
            .await?;
        s.accounts.release(s, &previous).await?;
        let patch = crate::runs::RunChanges {
            account_wait_reason: Some(Some(
                (format!("Usage exhausted. Waiting for an available {label} account to resume."))
                    .to_owned(),
            )),
            account_id: Some(None),
            account_name: Some(None),
            ..Default::default()
        };
        let id = self.id.clone();
        s.store.transaction(move |db| {
            db.change_run(&id, patch)?;
            db.event(&id, &EventKind::Status, "Usage exhausted. Saving this session and restoring capacity or switching accounts.", None)
        }).await?;
        s.accounts.refresh(s, &previous.account_id).await?;
        self.wait_for_account(&model).await?;
        let Some(account) = self.account.as_mut() else {
            return Ok(());
        };
        self.provider
            .adapter()
            .rotate_account(s, account, &previous.home, workspace.isolated())
            .await?;
        self.sensitive
            .extend(s.accounts.redactions(s, account).await?);
        let name = serde_json::to_value(s.accounts.account(s, &account.account_id).await?.name)?;
        let patch = crate::runs::RunChanges {
            account_wait_reason: Some(None),
            account_id: Some(Some((account.account_id).to_owned())),
            account_name: Some(name.as_str().map(str::to_owned)),
            ..Default::default()
        };
        let message = format!(
            "Resuming saved session with {label} account: {}",
            name.as_str().unwrap_or("")
        );
        let id = self.id.clone();
        s.store
            .transaction(move |db| {
                db.change_run(&id, patch)?;
                db.event(&id, &EventKind::Status, &message, None)
            })
            .await
    }

    async fn wait_for_account(&mut self, model: &str) -> Result<()> {
        let s = self.s;
        while self.account.is_none() && !self.stopping() && !self.checkpoint.expired() {
            let speed = text(&self.run["snapshot"]["agent"], "speed");
            match s
                .accounts
                .acquire_for(s, &self.id, self.provider, model, speed)
                .await
            {
                Ok(account) => *self.account = account,
                Err(error) if error.is_conflict() => {
                    tokio::select! {
                        () = tokio::time::sleep(Duration::from_secs(1)) => {}
                        () = self.cancel.cancelled() => {}
                        () = s.shutdown.cancelled() => {}
                    }
                }
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }

    async fn finish(
        &self,
        workspace: &Workspace,
        exit: &Exit,
        saved: &RunCheckpoint,
        has_session: bool,
    ) -> Result<()> {
        let s = self.s;
        let output_summary = crate::skills::small_file(Path::new(workspace.output()))
            .await
            .unwrap_or_default();
        let status = if self.cancel.is_cancelled() {
            RunStatus::Cancelled
        } else if exit.timed_out {
            RunStatus::Failed
        } else if exit.code == Some(0) || (s.shutdown.is_cancelled() && saved.completed()) {
            RunStatus::Succeeded
        } else {
            RunStatus::Failed
        };
        if status == RunStatus::Failed
            && !exit.timed_out
            && !self.stopping()
            && let Some(cause) = saved.retry_cause.as_ref().and_then(Option::as_ref)
            && (has_session || cause.startup)
        {
            // The provider confirmed this turn failed. Recovery is scheduled by
            // the worker after the previous execution is fenced, not replayed here.
            return Err(Error::conflict(cause.message.clone()));
        }
        let error = self.failure_message(status, exit, saved);
        let summary = if status != RunStatus::Succeeded {
            &error
        } else if !output_summary.is_empty() {
            &output_summary
        } else {
            saved.last_message()
        };
        let summary = run_output::redact(summary, self.sensitive)
            .chars()
            .take(SUMMARY_LIMIT)
            .collect::<String>();
        let resume_available = (status != RunStatus::Succeeded
            || self.run["chatExecution"].is_object())
            && has_session;
        let error = (!error.is_empty()).then(|| run_output::redact(&error, self.sensitive));
        // A failed run's final status says why it ended when no earlier event did.
        let details = error
            .as_ref()
            .map(|message| json!({ "error": { "message": message } }));
        let mut patch = crate::runs::RunChanges {
            status: Some(status),
            finished_at: Some(Some(now())),
            resume_available: Some(resume_available),
            summary: Some((summary).to_owned()),
            error: Some(error.clone()),
            boundary: Some(serde_json::json!({})),
            ..Default::default()
        };
        if status == RunStatus::Succeeded {
            patch.retry = Some(None);
        }
        let id = self.id.clone();
        s.store.transaction(move |db| {
            let current = db.run(&id)?.ok_or_else(|| Error::not_found("Run not found"))?;
            let status = if !current["cancelRequestedAt"].is_null() {
                RunStatus::Cancelled
            } else if status == RunStatus::Succeeded && current["trigger"] != "chat" && current["wakeup"].is_object() {
                if !has_session {
                    return Err(Error::conflict("Cannot wait without a saved agent session. Working files were preserved."));
                }
                RunStatus::Waiting
            } else { status };
            patch.status = Some(status);
            if status == RunStatus::Succeeded {
                crate::chats::keep_reply_after_compaction(&current, &mut patch)?;
            }
            if status == RunStatus::Waiting {
                patch.finished_at = Some(None);
                patch.resume_available = Some(true);
                patch.boundary.as_mut().unwrap()["outcome"] = Value::Null;
            } else if current["trigger"] != "chat" {
                patch.wakeup = Some(None);
            }
            db.change_run(&id, patch)?;
            let details = details.filter(|_| status == RunStatus::Failed);
            db.event(&id, &EventKind::Status, status.as_str(), details.as_ref())
        }).await
    }

    fn failure_message(&self, status: RunStatus, exit: &Exit, saved: &RunCheckpoint) -> String {
        if self.cancel.is_cancelled() {
            return "Run stopped. You can resume this conversation.".into();
        }
        if exit.timed_out {
            return "Run exceeded its time limit.".into();
        }
        if status == RunStatus::Succeeded {
            return String::new();
        }
        if !saved.last_error().is_empty() {
            return saved.last_error().to_owned();
        }
        match exit.code {
            Some(crate::runner::CONTROLLER_FAILED) => {
                format!(
                    "{} failed while running this attempt. Resume the conversation to continue.",
                    controller_name(self.run)
                )
            }
            Some(code) => {
                format!("Process exited with status {code}. Resume the conversation to continue.")
            }
            None => "Process stopped before finishing. Resume the conversation to continue.".into(),
        }
    }
}

fn controller_name(run: &Value) -> &'static str {
    if crate::native::policy::native(&run["snapshot"]["agent"]) {
        "Native controller"
    } else {
        "VM controller"
    }
}

fn current_exe() -> Result<String> {
    Ok(std::env::current_exe()?.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn controller_messages_follow_the_execution_backend() {
        assert_eq!(
            controller_name(
                &json!({ "snapshot": { "agent": { "access": { "backend": "native" } } } })
            ),
            "Native controller"
        );
        assert_eq!(
            controller_name(
                &json!({ "snapshot": { "agent": { "access": { "backend": "firecracker" } } } })
            ),
            "VM controller"
        );
        assert_eq!(controller_name(&json!({})), "VM controller");
    }

    #[tokio::test]
    async fn mode_restart_before_first_session_preserves_available_sessions_and_other_recovery_rules()
     {
        let root = tempfile::tempdir().unwrap();
        let s = Service::new(crate::config::Config {
            data_dir: root.path().join("data"),
            home: root.path().join("home"),
            workspace_roots: vec![root.path().into()],
            public_url: "http://localhost:4310".into(),
            host: "127.0.0.1".into(),
            port: 0,
            codex_bin: Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../tests/fixtures/codex.mjs")
                .to_string_lossy()
                .into_owned(),
            claude_bin: "/bin/false".into(),
            opencode_bin: "/bin/false".into(),
            opencode_go_usage_url: crate::accounts::opencode::USAGE_URL.into(),
            concurrency: 1,
            logger: false,
            worker_enabled: false,
            runner_url: String::new(),
        })
        .await
        .unwrap();
        for provider in ["codex", "claude", "opencode"] {
            let mut run = json!({
                "isolated": true, "nativeModeInterruption": crate::config::id(),
                "snapshot": { "agent": { "provider": provider, "access": { "backend": "native" } } },
            });
            assert!(
                saved_session(&s, &run, root.path(), root.path())
                    .await
                    .unwrap()
                    .is_none(),
                "{provider}"
            );
            run["sessionId"] = if provider == "opencode" {
                "ses_saved"
            } else {
                "saved-session"
            }
            .into();
            assert_eq!(
                saved_session(&s, &run, root.path(), root.path())
                    .await
                    .unwrap()
                    .as_deref(),
                run["sessionId"].as_str(),
                "{provider}"
            );
            run["sessionId"] = Value::Null;
            run["nativeModeInterruption"] = Value::Null;
            assert!(
                saved_session(&s, &run, root.path(), root.path())
                    .await
                    .is_err(),
                "{provider}"
            );
            run["nativeModeInterruption"] = crate::config::id().into();
            run["snapshot"]["agent"]["access"]["backend"] = "firecracker".into();
            assert!(
                saved_session(&s, &run, root.path(), root.path())
                    .await
                    .is_err(),
                "{provider}"
            );
        }
    }
}
