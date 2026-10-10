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
    recovery, run_limits, run_output,
    run_status::RunStatus,
    service::{Service, covers, policy},
    skills::{atomic_write, private_dir},
    storage::policy::Policy,
    store::merge,
    supervisor::Supervised,
    validation::text,
};
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
        provider: Provider::of_run(run),
        directory: s.config.data_dir.join("runs").join(&id),
        id,
        run,
        account: &mut admission.account,
        preparation: admission.preparation.take(),
        cancel,
        checkpoint,
        sensitive,
        attempt_id: String::new(),
    };
    execution.mark_running().await?;
    timing.next("access");
    execution.refresh_access().await?;
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
}

/// The prepared execution environment shared by every launch attempt.
struct Workspace {
    prepared: Value,
    codex_home: PathBuf,
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
    args: &'a [String],
    cwd: &'a Value,
    prompt: String,
    imports: Vec<Value>,
    expires: Option<i64>,
    sandbox: Value,
    mcp_env: &'a Value,
    resources: &'a Value,
    node_lease_required: bool,
    storage: RunnerStorage<'a>,
    #[serde(skip_serializing_if = "Option::is_none")]
    chat: Option<Value>,
}

#[derive(Serialize)]
struct RunnerStorage<'a> {
    master: &'a str,
    grant: String,
    policy: Policy,
}

impl Execution<'_> {
    fn claude(&self) -> bool {
        self.provider == Provider::Claude
    }

    fn stopping(&self) -> bool {
        self.cancel.is_cancelled() || self.s.shutdown.is_cancelled()
    }

    async fn mark_running(&mut self) -> Result<()> {
        let s = self.s;
        self.checkpoint.persist().await?;
        let account_name = match self.account.as_ref() {
            Some(account) => s.accounts.get(s, &account.account_id).await?["name"].clone(),
            None => Value::Null,
        };
        let mut patch = json!({
            "status": RunStatus::Running,
            "startedAt": self.run["startedAt"].as_i64().unwrap_or_else(now),
            "finishedAt": null,
            "accountWaitReason": null,
            "accountRequired": null,
            "accountId": self.account.as_ref().map(|account| &account.account_id),
            "accountName": account_name,
        });
        if self.run["retry"].is_object() {
            let mut retry = self.run["retry"].clone();
            retry["nextAttemptAt"] = Value::Null;
            patch["retry"] = retry;
        }
        s.store.patch_run(&self.id, patch).await?;
        if let Some(name) = account_name.as_str() {
            let message = format!("Using {} account: {name}", self.provider.label());
            s.store.event(&self.id, "status", &message, None).await?;
        }
        s.store
            .event(&self.id, "status", "Preparing workspace", None)
            .await?;
        private_dir(&self.directory).await
    }

    /// Refuses grants reduced since the run was queued. The run keeps the grants it
    /// was queued with, except node grants: placement follows the current ones,
    /// including whether execution needs a VM.
    async fn refresh_access(&mut self) -> Result<()> {
        let s = self.s;
        let current = s
            .get("agents", text(&self.run["snapshot"]["agent"], "id"))
            .await?;
        nodes::require_node(&current)?;
        if !covers(&current, &self.run["snapshot"]["agent"]) {
            return Err(Error::conflict(
                "Agent access was reduced after this run was queued. Run the task again with the current policy.",
            ));
        }
        let current_policy = policy(&current);
        self.run["snapshot"]["agent"]["access"]["nodes"] = current_policy["nodes"].clone();
        let patch = json!({ "snapshot": self.run["snapshot"] });
        s.store.patch_run(&self.id, patch).await?;
        Ok(())
    }

    async fn prepare(&mut self, existing: bool) -> Result<Workspace> {
        let s = self.s;
        let mut timing = Operation::new("workspace_prepare", &self.id, "execution");
        let prepared = self.prepare_execution(existing).await?;
        timing.next("account_home");
        let codex_home = self.prepare_codex_home(&prepared).await?;
        let patch = json!({
            "workspace": prepared["cwd"],
            "workspaces": prepared["workspaces"],
            "isolated": prepared["isolated"],
        });
        merge(self.run, &patch);
        s.store.patch_run(&self.id, patch).await?;
        timing.next("environment");
        let mut env = self
            .environment(prepared["isolated"] == true, &codex_home)
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
            codex_home,
            env,
            mcp,
        })
    }

    /// Prepares the workspace, or restores the one saved before a restart.
    async fn prepare_execution(&self, existing: bool) -> Result<Value> {
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
        let github = s.store.kv(&format!("agent-github:{agent_id}")).await?;
        let github = github.as_ref().and_then(Value::as_str);
        let prepared = match saved.prepared() {
            Some(prepared) => {
                execution::restore(self.run, prepared.clone(), &s.config, github).await?
            }
            None => {
                let account_home = self.account.as_ref().map(|a| a.home.as_path());
                execution::prepare(
                    self.run,
                    &s.config,
                    github,
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
        Ok(prepared)
    }

    async fn prepare_codex_home(&mut self, prepared: &Value) -> Result<PathBuf> {
        let s = self.s;
        let isolated = prepared["isolated"] == true;
        let codex_home = self
            .directory
            .join(if isolated { "home/.codex" } else { "codex" });
        private_dir(&codex_home).await?;
        let claude = self.claude();
        if !claude && !isolated {
            execution::codex_home(&s.config, &codex_home).await?;
        }
        if let Some(account) = self.account.as_mut()
            && isolated
            && !claude
        {
            s.accounts.relocate(s, account, &codex_home).await?;
        }
        Ok(codex_home)
    }

    async fn environment(&self, isolated: bool, codex_home: &Path) -> Result<Environment> {
        let config = &self.s.config;
        let mut env = if isolated {
            std::env::vars().collect::<Environment>()
        } else {
            crate::toolkit::environment(&config.home, std::env::vars().collect()).await?
        };
        crate::process::remove_storage_environment(&mut env);
        env.insert("HOME".into(), config.home.to_string_lossy().into_owned());
        env.insert(
            "CODEX_HOME".into(),
            codex_home.to_string_lossy().into_owned(),
        );
        for key in ["OPENAI_API_KEY", "CODEX_API_KEY", "CODEX_THREAD_ID"] {
            env.remove(key);
        }
        if self.claude() {
            // The session belongs to the run, so it can continue on another Claude account.
            let home = self.account.as_ref().map_or_else(
                || self.directory.join("home/.claude"),
                |account| account.home.clone(),
            );
            crate::claude::configure(&mut env, &home);
        }
        Ok(env)
    }

    /// Finds the conversation to continue when a previous launch already started it.
    async fn resume_session(&self, workspace: &Workspace) -> Result<Option<String>> {
        let s = self.s;
        if !self.checkpoint.read(RunCheckpoint::launched).await {
            return Ok(None);
        }
        let session =
            recovery::session(s, self.run, &workspace.codex_home, workspace.cwd()).await?;
        let patch = json!({
            "sessionId": session,
            "resumeCount": self.run["resumeCount"].as_u64().unwrap_or(0) + 1,
            "resumeAvailable": true,
        });
        s.store.patch_run(&self.id, patch).await?;
        s.store
            .event(
                &self.id,
                "status",
                "Resuming saved conversation and workspace",
                None,
            )
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
        if !self.claude() {
            let home = self
                .account
                .as_ref()
                .map_or(&workspace.codex_home, |account| &account.home);
            workspace
                .env
                .insert("CODEX_HOME".into(), home.to_string_lossy().into_owned());
        }
        let mut args = workspace.mcp["args"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect::<Vec<_>>();
        args.extend(run_output::args(self.run, &output, resume));
        let mut launch = Launch {
            binary: s.config.codex_bin.clone(),
            args,
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
        if workspace.isolated() {
            recovery::fence(s, &s.store.run(&self.id).await?).await?;
        }
        let saved = self.checkpoint.snapshot().await;
        if s.shutdown.is_cancelled() && !self.cancel.is_cancelled() && !saved.completed() {
            return Err(Error::conflict("Worker is restarting"));
        }
        let session = s.store.run(&self.id).await?["sessionId"]
            .as_str()
            .map(str::to_owned);
        let stopped_by_controller =
            !self.cancel.is_cancelled() && !exit.timed_out && !exit.exhausted;
        let firecracker = workspace.prepared["backend"] == "firecracker";
        let interrupted = exit.code == Some(crate::runner::CONTROLLER_INTERRUPTED);
        if firecracker && stopped_by_controller && interrupted {
            if saved.start_rejected == Some(true) {
                return Err(Error::unavailable(format!(
                    "{} Working files were preserved; retry when the controller is available.",
                    saved.last_error(),
                )));
            }

            let message = if session.is_some() {
                "VM controller interrupted execution. The saved conversation and workspace have been preserved."
            } else {
                "VM controller interrupted execution. Working files were preserved, but no resumable agent session was confirmed."
            };
            return Err(Error::unavailable(message));
        }

        // Remote nodes retry unavailable attempts without a cap; bound this
        // possibly deterministic failure here instead.
        let controller_failed = exit.code == Some(crate::runner::CONTROLLER_FAILED);
        let may_retry = saved.controller_recoveries() < super::execution::MAX_CONTROLLER_RECOVERIES;
        if firecracker && stopped_by_controller && controller_failed && may_retry {
            return Err(Error::unavailable(
                "VM controller failed while running this attempt. The saved conversation and workspace have been preserved.",
            ));
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
            return Ok(run_output::prompt(self.run, false));
        }
        let mut prompt = RESUME_PROMPT.to_owned();
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

    /// Chats, Claude runs and account-backed runs go through the `chat` protocol.
    async fn chat_plan(
        &self,
        workspace: &Workspace,
        resume: Option<&str>,
        launch: &mut Launch,
    ) -> Result<Option<Value>> {
        let s = self.s;
        let claude = self.claude();
        let chat_execution = self.run["chatExecution"].is_object();
        if !claude && !chat_execution && self.account.is_none() {
            return Ok(None);
        }
        private_dir(&self.directory.join("chat-input")).await?;
        s.prepare_chat_files(&self.id, &self.run["chatExecution"]["attachments"])
            .await?;
        let mut chat = run_output::chat_plan(
            self.run,
            &workspace.prepared,
            &self.directory,
            &workspace.mcp,
            resume,
        );
        // Optional fields: retained older adapters can ignore them.
        chat["runId"] = self.id.clone().into();
        chat["attemptId"] = self.attempt_id.clone().into();
        if claude {
            chat["claudeManagedAuth"] = self.account.is_some().into();
        }
        if !chat_execution {
            // Scheduled work uses the same authenticated protocol as
            // chats, while retaining its own task brief and session.
            chat["execution"] = json!({
                "messageId": self.id,
                "text": launch.prompt,
                "recovery": resume.is_some(),
                "attachments": [],
            });
        }
        if !claude && text(&chat, "reasoning").is_empty() {
            let source = self
                .account
                .as_ref()
                .map_or("", |account| account.account_id.as_str());
            if let Some((model, effort)) =
                crate::models::cached_defaults(s, source, text(&chat, "model")).await?
            {
                chat["model"] = model.into();
                chat["reasoning"] = effort.into();
            }
        }
        let agent_binary = if claude {
            &s.config.claude_bin
        } else {
            &s.config.codex_bin
        };
        launch.binary = current_exe()?;
        launch.args = vec!["chat".into(), agent_binary.clone()];
        launch.prompt = chat.to_string();
        Ok(Some(chat))
    }

    /// Places the run on a node and routes the launch through its isolated runner.
    async fn prepare_runner(
        &mut self,
        workspace: &mut Workspace,
        resume: Option<&str>,
        chat: Option<Value>,
        launch: &mut Launch,
    ) -> Result<()> {
        let s = self.s;
        crate::object_storage::Storage::configured(s)?;
        let mut timing = Operation::new("runner_prepare", &self.id, "placement");
        let runner = crate::config::id();
        let placement = placement::reserve(s, &s.store.run(&self.id).await?, &runner).await?;
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
        s.store.patch_run(&self.id, patch).await?;
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
        chat: Option<Value>,
        launch: &Launch,
    ) -> Result<()> {
        let s = self.s;
        let prepared = &workspace.prepared;
        let plans = s.config.data_dir.join("runner-plans");
        private_dir(&plans).await?;
        let mut imports = prepared["mounts"].as_array().unwrap().clone();
        if chat.is_some() {
            imports.push(json!({
                "source": self.directory.join("chat-input"),
                "target": "/run/cairn-chat",
                "readOnly": true,
            }));
        }
        let prompt = if resume.is_some() {
            launch.prompt.clone()
        } else {
            let mut context = self.run.clone();
            context["snapshot"]["skills"] = prepared["skills"].clone();
            run_output::prompt(&context, false)
        };
        let storage_node = text(placement, "nodeId");
        let node_storage = s
            .store
            .get("nodes", storage_node)
            .await?
            .unwrap_or_default()["storage"]
            .clone();
        let storage_policy = Policy::for_node(&node_storage)?;
        let grant = nodes::disk_grants::new_disk(s, self.run, storage_node).await?;
        s.store
            .patch_run(&self.id, json!({ "storage": { "mode": "on-demand" } }))
            .await?;
        placement::renew_local(s, &self.id).await?;
        let plan = RunnerPlan {
            id: runner,
            run_id: &self.id,
            args: &launch.args,
            cwd: &prepared["cwd"],
            prompt,
            imports,
            expires: self.checkpoint.deadline,
            sandbox: policy(&self.run["snapshot"]["agent"])["sandbox"].clone(),
            mcp_env: &workspace.mcp["env"],
            resources: &placement["resources"],
            node_lease_required: true,
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
            return Err(Error::unavailable(
                "Remote workspace preparation failed; source files are preserved.",
            ));
        }
        let message = format!("Executing on node {node_id}");
        s.store.event(&self.id, "status", &message, None).await
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
            s.store.patch_run(&self.id, patch).await?;
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
        let timeout = run_limits::wait_until(self.checkpoint.deadline);
        tokio::pin!(timeout);
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
                () = &mut timeout, if running => {
                    exit.timed_out = true;
                    child.stop().await;
                    exit.code = Some(STOPPED);
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
                        if !output.diagnostic {
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
        s.accounts.release(&previous).await?;
        let patch = json!({
            "accountWaitReason": format!("Usage exhausted. Waiting for an available {label} account to resume."),
            "accountId": null,
            "accountName": null,
        });
        s.store.patch_run(&self.id, patch).await?;
        s.store
            .event(
                &self.id,
                "status",
                "Usage exhausted. Saving this session and restoring capacity or switching accounts.",
                None,
            )
            .await?;
        s.accounts.refresh(s, &previous.account_id).await?;
        self.wait_for_account(&model).await?;
        let Some(account) = self.account.as_mut() else {
            return Ok(());
        };
        if workspace.isolated() || self.provider == Provider::Claude {
            s.accounts.relocate(s, account, &previous.home).await?;
        } else {
            execution::codex_home(&s.config, &account.home).await?;
        }
        self.sensitive
            .extend(s.accounts.redactions(s, account).await?);
        let name = s.accounts.get(s, &account.account_id).await?["name"].clone();
        let patch = json!({
            "accountWaitReason": null,
            "accountId": account.account_id,
            "accountName": name,
        });
        s.store.patch_run(&self.id, patch).await?;
        let message = format!(
            "Resuming saved session with {label} account: {}",
            name.as_str().unwrap_or("")
        );
        s.store.event(&self.id, "status", &message, None).await
    }

    async fn wait_for_account(&mut self, model: &str) -> Result<()> {
        let s = self.s;
        while self.account.is_none() && !self.stopping() && !self.checkpoint.expired() {
            match s.accounts.acquire(s, &self.id, self.provider, model).await {
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
            && has_session
            && let Some(cause) = saved.retry_cause.as_ref().and_then(Option::as_ref)
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
        let mut patch = json!({
            "status": status,
            "finishedAt": now(),
            "resumeAvailable": resume_available,
            "summary": summary,
            "error": error,
        });
        if status == RunStatus::Succeeded {
            patch["retry"] = Value::Null;
        }
        s.store.patch_run(&self.id, patch).await?;
        s.store
            .event(&self.id, "status", status.as_str(), None)
            .await
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
                "VM controller failed while running this attempt. Resume the conversation to continue.".into()
            }
            Some(code) => {
                format!("Process exited with status {code}. Resume the conversation to continue.")
            }
            None => "Process stopped before finishing. Resume the conversation to continue.".into(),
        }
    }
}

fn current_exe() -> Result<String> {
    Ok(std::env::current_exe()?.to_string_lossy().into_owned())
}
