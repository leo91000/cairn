use super::{
    Admission,
    checkpoint::{Checkpoint, RetainedCompletion, RunCheckpoint},
    held::HELD_DEADLINE_CODE,
    output,
};
use crate::{
    accounts::{
        Lease, broker,
        output_redactions::{History, Observation},
    },
    config::now,
    error::{Error, Result},
    execution,
    nodes::{self, placement},
    performance::{Activity, Operation},
    process::Environment,
    provider::Provider,
    recovery,
    run_output::{self, RESUME_PROMPT},
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
use tokio_util::{sync::CancellationToken, task::AbortOnDropHandle};
use tracing::Instrument;

/// Exit code reported for processes the worker had to stop.
const STOPPED: i32 = 143;
const SUMMARY_LIMIT: usize = 100_000;
const HISTORY_LIMIT: usize = 16_000;

#[cfg(test)]
#[path = "launch_stream_tests.rs"]
mod stream_tests;

/// Runs `run` until it finishes, retrying on another account when usage runs out.
pub(super) async fn execute(
    s: &Arc<Service>,
    run: &mut Value,
    admission: &mut Admission,
    cancel: &CancellationToken,
    checkpoint: &Checkpoint,
    existing: bool,
    sensitive: &History,
) -> Result<()> {
    let span = tracing::info_span!(target: "cairn_performance", "agent_execution", run_id = text(run, "id"));
    Box::pin(execute_inner(
        s, run, admission, cancel, checkpoint, existing, sensitive,
    ))
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
    sensitive: &History,
) -> Result<()> {
    let id = text(run, "id").to_owned();
    if !run["environmentExpiredAt"].is_null() {
        if !s.config.runner_url.is_empty() {
            crate::conversation_deletion::ensure_local_retirement_protocol(s)
                .await
                .map_err(|error| {
                    Error::unavailable(format!("{} {}", placement::NO_CAPACITY, error.message))
                })?;
        }
        crate::conversation_deletion::retired::mark_manager_namespace(s, &id).await?;
    }
    let mut timing = Operation::new("agent_prepare", &id, "mark_running");
    let mut execution = Execution {
        s,
        provider: Provider::of_run(run)?,
        directory: s.config.data_dir.join("runs").join(&id),
        id,
        admission_revision: run["environmentAdmissionRevision"].clone(),
        run,
        account: &mut admission.account,
        preparation: admission.preparation.take(),
        cancel,
        checkpoint,
        sensitive,
        observation: None,
        attempt_id: String::new(),
        reservation: None,
        retained: false,
        turn: String::new(),
        #[cfg(test)]
        output_probe: None,
    };
    let result = async {
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
            match Box::pin(execution.attempt(&mut workspace, resume.as_deref(), &mut log_total))
                .await?
            {
                Attempt::Finished => return Ok(()),
                Attempt::Held => {
                    if !Box::pin(execution.wait_for_next_turn(&mut workspace)).await? {
                        return Ok(());
                    }
                    resume = execution.resume_session(&workspace).await?;
                }
                Attempt::Resume(session) => resume = Some(session),
            }
        }
    }
    .await;
    if let Some((runner, _)) = execution.reservation.take() {
        // No controller received this attempt: ownership did not reach its
        // durable checkpoint, so preparation must release the reservation.
        placement::release(s, &runner).await?;
    }
    result
}

/// A controller may have accepted a first attempt before its engine created a
/// session. Prefer any recoverable session; only that controlled native restart
/// can start afresh when none was recorded. Ordinary and VM recovery stay strict.
async fn saved_session(
    s: &Service,
    run: &Value,
    home: &Path,
    cwd: &Path,
    allow_fresh_native_session: bool,
) -> Result<Option<String>> {
    match recovery::session(s, run, home, cwd).await {
        Ok(session) => Ok(Some(session)),
        Err(error)
            if error.status == 409
                && allow_fresh_native_session
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
    admission_revision: Value,
    account: &'a mut Option<Lease>,
    preparation: Option<tokio::sync::OwnedMutexGuard<()>>,
    cancel: &'a CancellationToken,
    checkpoint: &'a Checkpoint,
    sensitive: &'a History,
    observation: Option<Observation>,
    provider: Provider,
    directory: PathBuf,
    attempt_id: String,
    reservation: Option<(String, Value)>,
    retained: bool,
    turn: String,
    #[cfg(test)]
    output_probe: Option<stream_tests::OutputProbe>,
}

/// The prepared execution environment shared by every launch attempt.
struct Workspace {
    prepared: Value,
    state_home: PathBuf,
    env: Environment,
    mcp: Value,
    authorization: crate::plugins::run_credentials::Authorization,
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
    Held,
    /// Usage ran out; continue the saved session, possibly on another account.
    Resume(String),
}

enum HeldTurn {
    Resume,
    Close,
    Expired,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    environment_protocol: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    turn_id: Option<&'a str>,
}

#[derive(Serialize)]
struct RunnerStorage<'a> {
    owner: &'a str,
    policy: Policy,
}

impl Execution<'_> {
    /// Reserve before preparing paths: VM and native environments differ. Binding
    /// the concrete backend shares the atomic admission transaction, so retries
    /// cannot turn an already-created environment into another execution kind.
    async fn resolve_backend(&mut self) -> Result<()> {
        let automatic = policy(&self.run["snapshot"]["agent"])["backend"] == "auto";
        let fresh_environment = self.run["environmentState"] == "expired";
        if !automatic && !fresh_environment {
            return Ok(());
        }
        let runner = crate::config::id();
        let mut admission = self.run.clone();
        admission["environmentAdmissionRevision"] = self.admission_revision.clone();
        let placement = placement::reserve(self.s, &admission, &runner).await?;
        self.reservation = Some((runner.clone(), placement.clone()));
        *self.run = self.s.store.run(&self.id).await?;
        if !execution::uses_vm(self.run, &self.s.config) {
            // Development's shared process has no controller or retained node disk.
            // Keep the selected backend, but never give recovery a node to contact.
            placement::release(self.s, &runner).await?;
            self.reservation = None;
            if fresh_environment {
                let revision = self.admission_revision.clone();
                let id = self.id.clone();
                let generation = crate::config::id();
                self.checkpoint
                    .update_and_publish(
                        |checkpoint| {
                            checkpoint.prepared = None;
                            checkpoint.node_id = None;
                            checkpoint.runner_id = None;
                            checkpoint.runtime_id = None;
                            checkpoint.workspace_initialized = None;
                            checkpoint.generation = Some(generation);
                        },
                        move |db| {
                            let current = db
                                .run(&id)?
                                .ok_or_else(|| Error::not_found("Run not found"))?;
                            crate::environment_expiration::require_admission_revision(
                                &current, &revision,
                            )
                        },
                    )
                    .await?;
            }
            return Ok(());
        }
        let saved_runner = runner.clone();
        let state = self.s.store.run_state(&self.id).await?;
        let node = placement["nodeId"].as_str().map(str::to_owned);
        let runtime = placement["runtimeId"].clone();
        let id = self.id.clone();
        let revision = self.admission_revision.clone();
        let generation = crate::config::id();
        self.checkpoint
            .update_and_publish(
                |checkpoint| {
                    if fresh_environment {
                        checkpoint.prepared = None;
                        checkpoint.workspace_initialized = None;
                        checkpoint.generation = Some(generation);
                    }
                    checkpoint.bind_paid_admission(&saved_runner, &state);
                    checkpoint.turn_pending = None;
                    checkpoint.recovery_deadline_ms = None;
                    checkpoint.terminal_code = None;
                    checkpoint.provider_failed = None;
                    checkpoint.node_id = Some(node);
                    checkpoint.runtime_id = Some(Some(runtime));
                },
                move |db| {
                    let current = db
                        .run(&id)?
                        .ok_or_else(|| Error::not_found("Run not found"))?;
                    crate::environment_expiration::require_admission_revision(&current, &revision)?;
                    if !current["cancelRequestedAt"].is_null() {
                        return Err(Error::conflict("Execution stopped before preparation."));
                    }
                    Ok(())
                },
            )
            .await?;
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
            // Each execution's adapter announces again whether it answers side questions.
            boundary: Some(json!({
                "sideQuestions": null,
                "sideQuestionsCompacting": null,
                "sideQuestionsClosed": null,
            })),
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
                |state| {
                    state.capacity_retry_at = None;
                    state.provider_failed = None;
                },
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
        let (id, revision) = (self.id.clone(), self.admission_revision.clone());
        s.store
            .read(move |db| {
                let current = db
                    .run(&id)?
                    .ok_or_else(|| Error::not_found("Run not found"))?;
                crate::environment_expiration::require_admission_revision(&current, &revision)
            })
            .await?;
        let current = s
            .agent_snapshot(text(&self.run["snapshot"]["agent"], "id"))
            .await?;
        nodes::require_node(&current)?;
        if !covers(&current, &self.run["snapshot"]["agent"]) {
            return Err(Error::conflict(
                "Agent access was reduced after this run was queued. Run the task again with the current policy.",
            ));
        }
        let saved = self.checkpoint.snapshot().await;
        if let Some(observation) = super::held::Observation::capture(self.run, &saved) {
            crate::delegation::authorize_async_with(s, self.run, observation.authority()).await?;
        } else {
            crate::delegation::authorize_async(s, self.run).await?;
        }
        // A held owner runs beside the scheduler. Never write its stale
        // snapshot over a newly queued message, model or grant selection.
        let nodes = policy(&current)["nodes"].clone();
        self.run["snapshot"]["agent"]["access"]["nodes"] = nodes.clone();
        let revision = self.admission_revision.clone();
        let id = self.id.clone();
        let expected = crate::waits::execution_id(self.run).clone();
        s.store
            .transaction(move |db| {
                let mut latest = db
                    .run(&id)?
                    .ok_or_else(|| Error::not_found("Run not found"))?;
                crate::environment_expiration::require_admission_revision(&latest, &revision)?;
                if *crate::waits::execution_id(&latest) != expected {
                    return Ok(());
                }
                latest["snapshot"]["agent"]["access"]["nodes"] = nodes;
                db.change_run(
                    &id,
                    crate::runs::RunChanges {
                        snapshot: Some(serde_json::from_value(latest["snapshot"].clone())?),
                        ..Default::default()
                    },
                )?;
                Ok(())
            })
            .await?;
        Ok(())
    }

    async fn prepare(&mut self, existing: bool) -> Result<Workspace> {
        let s = self.s;
        let mut timing = Operation::new("workspace_prepare", &self.id, "execution");
        let (prepared, credentials, authorization) = self.prepare_execution(existing).await?;
        // Plugin credentials written into the run are masked wherever they appear.
        self.sensitive.extend(credentials.secrets.iter().cloned());
        timing.next("account_home");
        authorization.recheck().await?;
        let state_home = self.prepare_state_home(&prepared).await?;
        let patch = crate::runs::RunChanges {
            workspace: Some(serde_json::from_value(prepared["cwd"].clone())?),
            placement: Some(Box::new(crate::nodes::run_placement::PlacementChanges {
                execution_backend: Some(serde_json::from_value(serde_json::json!(
                    prepared["backend"]
                ))?),
                ..Default::default()
            })),
            boundary: Some(
                serde_json::json!({"workspaces": prepared["workspaces"],"isolated": prepared["isolated"]}),
            ),
            ..Default::default()
        };
        let revision = self.admission_revision.clone();
        let id = self.id.clone();
        s.store
            .transaction(move |db| {
                let current = db
                    .run(&id)?
                    .ok_or_else(|| Error::not_found("Run not found"))?;
                crate::environment_expiration::require_admission_revision(&current, &revision)?;
                db.change_run(&id, patch)?;
                Ok(())
            })
            .await?;
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
        crate::delegation::authorize_async(s, self.run).await?;
        credentials.authorize(s).await?;
        authorization.recheck().await?;
        if !execution::uses_vm(self.run, &s.config) && self.run["environmentState"] == "expired" {
            let id = self.id.clone();
            let revision = self.admission_revision.clone();
            self.checkpoint
                .update_and_publish(
                    |_| {},
                    move |db| {
                        let current = db
                            .run(&id)?
                            .ok_or_else(|| Error::not_found("Run not found"))?;
                        crate::environment_expiration::require_admission_revision(
                            &current, &revision,
                        )?;
                        if !current["cancelRequestedAt"].is_null() {
                            return Err(Error::conflict("Execution stopped before admission."));
                        }
                        db.update_run_placement(
                            &id,
                            nodes::run_placement::PlacementChanges {
                                node_id: Some(None),
                                target_node_id: Some(None),
                                ..Default::default()
                            },
                        )?;
                        db.update_run_boundary(&id, &json!({ "environmentState":null }))
                    },
                )
                .await?;
            *self.run = s.store.run(&self.id).await?;
        }
        timing.finish();
        Ok(Workspace {
            prepared,
            state_home,
            env,
            mcp,
            authorization,
        })
    }

    /// Prepares the workspace, or restores the one saved before a restart.
    async fn prepare_execution(
        &self,
        existing: bool,
    ) -> Result<(
        Value,
        crate::plugins::run_credentials::RunCredentials,
        crate::plugins::run_credentials::Authorization,
    )> {
        let s = self.s;
        if existing && self.checkpoint.read(|c| c.prepared().is_none()).await {
            // A fresh generation keeps a half-prepared workspace from being reused.
            let generation = crate::config::id();
            self.checkpoint
                .update(|c| c.generation = Some(generation))
                .await?;
        }
        let saved = self.checkpoint.snapshot().await;
        let projects = crate::project_workspaces::catalog(self.run);
        let authority =
            crate::capabilities::plugin_packages::Authority::for_run(s, self.run).await?;
        let credentials = crate::plugins::run_credentials::for_execution_authorized(
            s,
            self.run,
            &projects,
            authority.clone(),
        )
        .await?;
        let authorization =
            crate::plugins::run_credentials::Authorization::new(s, authority, &credentials);
        let prepared = match saved.prepared() {
            Some(prepared) => {
                execution::restore_authorized(
                    self.run,
                    prepared.clone(),
                    &s.config,
                    &credentials,
                    &authorization,
                )
                .await?
            }
            None => {
                let account_home = self.account.as_ref().map(|a| a.home.as_path());
                execution::prepare_authorized(
                    self.run,
                    &s.config,
                    &credentials,
                    account_home,
                    saved.generation.as_deref(),
                    &authorization,
                )
                .await?
            }
        };
        let saved_prepared = prepared.clone();
        let publication = authorization.clone();
        let id = self.id.clone();
        let revision = self.admission_revision.clone();
        self.checkpoint
            .update_and_publish(
                |c| c.prepared = Some(Some(saved_prepared)),
                move |db| {
                    let current = db
                        .run(&id)?
                        .ok_or_else(|| Error::not_found("Run not found"))?;
                    crate::environment_expiration::require_admission_revision(&current, &revision)?;
                    publication.authorize_in(db)
                },
            )
            .await?;
        authorization.recheck().await?;
        Ok((prepared, credentials, authorization))
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
        if self.retained {
            return s.store.run(&self.id).await?["sessionId"]
                .as_str()
                .map(|session| Some(session.to_owned()))
                .ok_or_else(|| {
                    Error::conflict("Retained environment has no resumable provider session.")
                });
        }
        if !self.checkpoint.read(RunCheckpoint::launched).await {
            return Ok(None);
        }
        let (pending_startup, unconfirmed_antigravity_start) = self
            .checkpoint
            .read(|saved| {
                let pending_startup = saved.retries_antigravity_without_session(
                    self.run["chatExecution"]["messageId"].as_str(),
                ) || saved
                    .retry_cause
                    .as_ref()
                    .and_then(Option::as_ref)
                    .is_some_and(|cause| cause.startup);
                (pending_startup, saved.antigravity_start_unconfirmed())
            })
            .await;
        if !self.run["sessionId"].is_string() && pending_startup && !unconfirmed_antigravity_start {
            // The adapter confirmed no new turn was submitted; preserve the same
            // prepared workspace and start the same turn without a session.
            return Ok(None);
        }
        let session = match saved_session(
            s,
            self.run,
            &workspace.state_home,
            workspace.cwd(),
            !unconfirmed_antigravity_start,
        )
        .await
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
                                saved.provider_failed = Some(true);
                                saved.terminal_code = Some(None);
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
        let revision = self.admission_revision.clone();
        s.store
            .transaction(move |db| {
                let current = db
                    .run(&id)?
                    .ok_or_else(|| Error::not_found("Run not found"))?;
                crate::environment_expiration::require_admission_revision(&current, &revision)?;
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
        self.turn = crate::config::id();
        if self.stopping() {
            return Err(Error::conflict("Execution stopped."));
        }
        if self.checkpoint.expired() {
            return Err(Error::conflict("Run exceeded its time limit."));
        }
        // Each attempt's adapter of a conversation, such as one resumed on another account,
        // announces again whether it answers side questions, and receives them only then.
        if self.run["chatExecution"].is_object() {
            crate::chats::side_questions::withdraw_announcement(
                &s.store,
                &self.id,
                &self.directory,
            )
            .await?;
        }
        workspace.authorization.recheck().await?;
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
        self.observation = match self.account.as_ref() {
            Some(account) => {
                s.accounts
                    .observe_output_credentials(
                        s,
                        account,
                        &self.attempt_id,
                        self.sensitive.clone(),
                    )
                    .await?
            }
            None => None,
        };
        let broker_result = match self.account.as_ref() {
            Some(account) => match &self.observation {
                Some(observation) => broker::serve_observed(s, account, observation.handle())
                    .await
                    .map(Some),
                None => broker::serve(s, account).await.map(Some),
            },
            None => Ok(None),
        };
        let auth_broker = match broker_result {
            Ok(broker) => broker,
            Err(error) => {
                self.observation.take();
                return Err(error);
            }
        };
        // Capture every result, including preparation/wait/reconcile failures,
        // before completing the account-scope and broker cleanup barrier.
        let result = async {
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
            workspace.authorization.recheck().await?;
            if workspace.isolated() {
                if self.retained {
                    self.resume_environment(workspace, chat, &mut launch)
                        .await?;
                } else {
                    self.prepare_runner(workspace, resume, chat, &mut launch)
                        .await?;
                }
            }
            let mut exit = Box::pin(self.supervise(workspace, launch, log_total)).await?;
            let receipt =
                if workspace.env.contains_key("CAIRN_PROVIDER_TURN") && exit.code == Some(0) {
                    let receipt = super::held::turn_receipt(s, self.run, &self.turn).await?;
                    if receipt.code != 0 {
                        return Err(Error::unavailable(
                            "Provider turn failed despite successful client exit.",
                        ));
                    }
                    Some(receipt)
                } else {
                    None
                };
            let environment_expiry = if receipt.is_some() {
                let path = s
                    .config
                    .data_dir
                    .join("runner-plans")
                    .join(format!("{}.json", self.attempt_id));
                // The runner identity may predate this provider turn.
                let runner = self
                    .checkpoint
                    .snapshot()
                    .await
                    .runner_id()
                    .map(str::to_owned)
                    .ok_or_else(|| Error::conflict("Missing environment identity."))?;
                let path = path.with_file_name(format!("{runner}.json"));
                let plan: Value = serde_json::from_slice(&tokio::fs::read(path).await?)?;
                plan["expires"]
                    .as_i64()
                    .ok_or_else(|| Error::conflict("Missing environment deadline."))?
            } else {
                0
            };
            let receipt_can_complete = !exit.timed_out && !self.cancel.is_cancelled();
            let message_id = self.run["chatExecution"]["messageId"].as_str();
            let received_at = now();
            let output_summary =
                if receipt_can_complete && receipt.is_some() && message_id.is_some() {
                    self.output_summary(workspace).await
                } else {
                    None
                };
            self.checkpoint
                .update(|c| {
                    c.process = Some(None);
                    // The runner client can return zero only after parsing a healthy
                    // controller result. Nonzero process statuses are ambiguous: local
                    // errors, panics, signals and worker stops share the same channel.
                    // Only a read of this attempt can establish their provenance.
                    c.terminal_code =
                        (exit.code == Some(0) && receipt.is_none()).then_some(Some(0));
                    if let Some(receipt) = &receipt {
                        c.held_environment = Some(super::checkpoint::HeldEnvironment {
                            attempt: receipt.attempt_id.clone(),
                            generation: receipt.generation.clone(),
                            expires_at: c
                                .held_environment
                                .as_ref()
                                .map_or(environment_expiry, |held| held.expires_at),
                        });
                        if receipt_can_complete
                            && c.completed()
                            && c.output_error().is_none()
                            && !c.provider_failed()
                            && let Some(message_id) = message_id
                        {
                            c.retained_completion = Some(RetainedCompletion {
                                message_id: message_id.to_owned(),
                                finished_at: received_at,
                                output_summary,
                            });
                        }
                    }
                })
                .await?;
            if exit.code != Some(0) && !exit.timed_out {
                super::execution::reconcile(s, self.checkpoint, self.cancel).await?;
                if let Some(code) = self.checkpoint.snapshot().await.terminal_code() {
                    exit.code = Some(if code == STOPPED && !self.cancel.is_cancelled() {
                        crate::runner::CONTROLLER_INTERRUPTED
                    } else {
                        code
                    });
                }
            }
            drop(mcp_channel);
            Ok::<_, Error>(exit)
        }
        .await;
        self.close_account_output(auth_broker).await;
        let exit = result?;
        let saved = self.checkpoint.snapshot().await;
        let retained_completion = self.retained_completion(&saved, &exit);
        #[cfg(feature = "worker-fixture-hooks")]
        if retained_completion.is_some() {
            let path = self.directory.join("fixture-receipt.sock");
            if tokio::fs::try_exists(&path).await? {
                let _gate = tokio::net::UnixStream::connect(path).await?;
                s.shutdown.cancelled().await;
            }
        }
        if s.shutdown.is_cancelled()
            && !self.cancel.is_cancelled()
            && !exit.timed_out
            && retained_completion.is_none()
            && (!saved.completed() || saved.terminal_observation_pending())
        {
            // Shutdown may stop the local client before /wait responds. Keep
            // that same attempt readable after restart rather than settling
            // incomplete transport observations as failed finalization.
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
        if saved.output_error().is_some()
            || saved.finalization_error().is_some()
            || (managed
                && saved.completed()
                && saved.terminal_code() != Some(0)
                && saved.held_environment.is_none())
        {
            // A provider completion cannot repair EIO/EROFS or an unknown flush.
            // Existing execution cleanup fences it before releasing its resources.
            self.finish(workspace, &exit, &saved, session.is_some())
                .await?;
            return Ok(Attempt::Finished);
        }
        let interrupted = exit.code == Some(crate::runner::CONTROLLER_INTERRUPTED);
        if managed && stopped_by_controller && interrupted && !saved.provider_failed() {
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

        if let Some(session) = session.as_ref()
            && self.may_switch_account(&exit)
        {
            if has_live_environment(&saved, self.retained, &workspace.env) {
                // A failed provider turn must not implicitly replay jobs or
                // resume a controller that account rotation would fence.
                // A conversation continues with its next message (ADR-0091); a mission is resumed.
                return Err(Error::conflict(if self.run["trigger"] == "chat" {
                    "Provider account exhausted in a live environment. Background work will stop."
                } else {
                    "Provider account exhausted in a live environment. Background work will stop; resume explicitly with an available account."
                }));
            }
            // This adapter has exited; its side questions cannot wait for a different
            // account, even though the run and turn remain running during the switch.
            if self.run["chatExecution"].is_object() {
                crate::chats::side_questions::withdraw_announcement(
                    &s.store,
                    &self.id,
                    &self.directory,
                )
                .await?;
            }
            if workspace.isolated() {
                self.checkpoint
                    .fence(s, &s.store.run(&self.id).await?)
                    .await?;
            }
            self.switch_account(workspace).await?;
            return Ok(Attempt::Resume(session.clone()));
        }
        self.finish(workspace, &exit, &saved, session.is_some())
            .await?;
        if exit.code == Some(0) && saved.completed() && saved.held_environment.is_some() {
            return Ok(Attempt::Held);
        }
        Ok(Attempt::Finished)
    }

    /// Select retirement, or leave the environment available for its continuation.
    async fn close_idle_environment(&self) -> Result<Option<HeldTurn>> {
        let id = self.id.clone();
        self.checkpoint
            .decide(move |saved, db| {
                let run = db
                    .run_state(&id)?
                    .ok_or_else(|| Error::not_found("Run not found"))?;
                if run.status == RunStatus::Queued || saved.turn_pending == Some(true) {
                    return Ok(None);
                }
                let record = db
                    .run(&id)?
                    .ok_or_else(|| Error::not_found("Run not found"))?;
                let owner = crate::waits::Owner::for_run(db, &record)?;
                let owner_id = if record["trigger"] == "chat" {
                    db.conversation_for_run(&id)?.map(|chat| chat.id)
                } else {
                    Some(id.clone())
                };
                if owner.resumable(&record)
                    && let Some(owner_id) = owner_id
                    && crate::waits::observation(db, &owner_id, &record, now())?.is_some()
                {
                    // Completion can become visible before the scheduler queues
                    // its continuation. Keep its original environment for that handoff.
                    return Ok(None);
                }
                if saved.remaining_ms == Some(Some(0))
                    || saved
                        .held_environment
                        .as_ref()
                        .is_some_and(|held| now() >= held.expires_at)
                {
                    return Ok(Some(HeldTurn::Expired));
                }
                if let Some(held) = saved.held_environment.take() {
                    super::held::settle_retirement(
                        db,
                        &id,
                        saved,
                        super::held::Retirement::Close,
                        held.expires_at,
                        None,
                    )?;
                }
                Ok(Some(HeldTurn::Close))
            })
            .await
    }

    async fn wait_for_next_turn(&mut self, workspace: &mut Workspace) -> Result<bool> {
        let s = self.s;
        let held = self
            .checkpoint
            .snapshot()
            .await
            .held_environment
            .ok_or_else(|| Error::conflict("Missing retained environment identity."))?;
        s.worker.held.lock().await.insert(self.id.clone());
        let checkpoint = self.checkpoint;
        let expired = || checkpoint.expired() || now() >= held.expires_at;
        let result = async {
            if let Some(account) = self.account.take() {
                s.accounts.release(s, &account).await?;
            }
            s.mcps.revoke_run(s, &self.id).await?;
            let mut timer = tokio::time::interval(Duration::from_secs(1));
            loop {
                tokio::select! {
                    () = self.cancel.cancelled() => return Ok(HeldTurn::Close),
                    () = s.shutdown.cancelled() => return Ok(if expired() { HeldTurn::Expired } else { HeldTurn::Close }),
                    _ = timer.tick() => {}
                }
                if expired() {
                    return Ok(HeldTurn::Expired);
                }
                let current = s.store.run(&self.id).await?;
                if !current["cancelRequestedAt"].is_null() { return Ok(HeldTurn::Close); }
                let run_id = self.id.clone();
                s.store.read(move |db| crate::conversation_lifecycle::require_active_run(db, &run_id)).await?;
                *self.run = current;
                let expected_execution = crate::waits::execution_id(self.run).clone();
                let refreshed = match self.refresh_access().await {
                    Err(error) if error.code.as_deref() == Some(HELD_DEADLINE_CODE) => {
                        return Ok(HeldTurn::Expired);
                    }
                    Err(error) => Err(error),
                    Ok(()) => {
                        if expired() { return Ok(HeldTurn::Expired); }
                        let observed = super::jobs::refresh_redacted(s, self.run, &self.sensitive.snapshot()).await;
                        if expired() && observed.as_ref().err().is_none_or(super::held::observation_ended) {
                            return Ok(HeldTurn::Expired);
                        }
                        observed
                    }
                };
                if let Err(error) = refreshed {
                    if super::held::reported_control_failure(&error) {
                        return Err(error);
                    }
                    let current = s.store.run(&self.id).await?;
                    if crate::waits::execution_id(&current) != &expected_execution
                        && current["status"] == RunStatus::Queued
                        && current["cancelRequestedAt"].is_null()
                    {
                        // A scheduler handoff invalidates the previous turn's
                        // observation authority. Recheck the new selection on
                        // the next tick instead of stopping its environment.
                        continue;
                    }
                    return Err(error);
                }
                if self.run["status"] == RunStatus::Queued {
                    // The scheduler has queued a fresh turn. Keep the existing
                    // owner and lease; acquiring another placement would deadlock
                    // against our still-live environment.
                    let saved = RunCheckpoint::load(&s.store, &self.id).await?
                        .ok_or_else(|| Error::conflict("Continuation checkpoint missing."))?;
                    if saved.held_environment.as_ref().is_none_or(|value| value.attempt != held.attempt || value.generation != held.generation) {
                        return Err(Error::conflict("Continuation environment changed."));
                    }
                    self.checkpoint.adopt_continuation().await?;
                    let model = text(&self.run["snapshot"]["agent"], "model").to_owned();
                    if !can_reuse_environment(self.provider, self.run)? {
                        // Keep the accepted fresh-session turn queued while this
                        // owner stops its old environment and background work.
                        return Ok(HeldTurn::Close);
                    }
                    match self.wait_for_account(&model).await {
                        Err(error) if error.code.as_deref() == Some(HELD_DEADLINE_CODE) => {
                            return Ok(HeldTurn::Expired);
                        }
                        result => result?,
                    }
                    if expired() {
                        return Ok(HeldTurn::Expired);
                    }
                    if self.stopping() { return Ok(HeldTurn::Close); }
                    match self.prepare_retained_turn(workspace).await {
                        Err(error) if error.code.as_deref() == Some(HELD_DEADLINE_CODE) => {
                            return Ok(HeldTurn::Expired);
                        }
                        result => result?,
                    }
                    if expired() {
                        return Ok(HeldTurn::Expired);
                    }
                    self.retained = true;
                    return Ok(HeldTurn::Resume);
                }
                let id = self.id.clone();
                let pending = s.store.read(move |db| {
                    let jobs: i64 = db.0.query_row("SELECT count(*) FROM background_jobs WHERE run_id=? AND (outcome IS NULL OR json_extract(outcome,'$.state')='running')", [id], |row| row.get(0))?;
                    Ok(jobs > 0)
                }).await?;
                let activity = super::held::request(s,self.run,&crate::background_jobs::protocol::Request::Activity { request_id: crate::config::id() }).await;
                if expired()
                    && (matches!(activity, Ok(crate::background_jobs::protocol::Event::Activity { .. }))
                        || activity.as_ref().is_err_and(super::held::observation_ended))
                {
                    return Ok(HeldTurn::Expired);
                }
                match activity? {
                    crate::background_jobs::protocol::Event::Activity { jobs, containers, .. } => {
                        if !pending && !jobs && !containers {
                            if let Some(retirement) = self.close_idle_environment().await? {
                                return Ok(retirement);
                            }
                            s.worker.notify();
                        }
                    }
                    crate::background_jobs::protocol::Event::Refused { message, .. } => return Err(Error::unavailable(message)),
                    _ => return Err(Error::bad_gateway("Invalid environment activity observation.")),
                }
            }
        }.await;
        if matches!(result, Ok(HeldTurn::Resume)) {
            s.worker.held.lock().await.remove(&self.id);
            return Ok(true);
        }

        let close_after_settlement = !matches!(result, Ok(HeldTurn::Close));
        let close_request = crate::config::id();
        let close = crate::background_jobs::protocol::Request::Close {
            request_id: close_request.clone(),
        };
        // The helper's TERM/reap barrier comes before the outer cgroup/VM
        // fence. Ordinary closure keeps its hold until the helper responds.
        let close_environment =
            || super::held::request_bound(s, self.run, Some(&held.attempt), &close);
        let early_failure = if close_after_settlement {
            None
        } else {
            let attempt = held.attempt.clone();
            let generation = held.generation.clone();
            self.checkpoint
                .decide(move |state, _db| {
                    if state.held_environment.as_ref().is_some_and(|current| {
                        current.attempt == attempt && current.generation == generation
                    }) {
                        state.closing_held_generation = Some(generation);
                    }
                    Ok(())
                })
                .await?;
            super::held::close_control_failure(close_environment().await, &close_request)
        };
        // Cleanup time cannot change the cause selected by the owner.
        let reached_deadline = matches!(result, Ok(HeldTurn::Expired));
        let observed_close_failure = early_failure.is_some();
        let expired_at = (reached_deadline
            || ((result.is_err() || observed_close_failure) && expired()))
        .then(|| {
            checkpoint
                .deadline()
                .map_or(held.expires_at, |deadline| deadline.min(held.expires_at))
        });
        let interruption = result
            .err()
            .or(early_failure)
            .map(|error| run_output::redact(&error.message, &self.sensitive.snapshot()));
        let id = self.id.clone();
        let original_hold = held.clone();
        self.checkpoint
            .decide(move |state, db| {
                super::jobs::unknown(
                    db,
                    &id,
                    &original_hold.attempt,
                    "Background supervision ended before job completion was confirmed.",
                )?;
                let Some(retiring) = state.held_environment.take() else {
                    // Idle closure released the hold before the helper request.
                    // A follow-up may already own the checkpoint now.
                    if observed_close_failure && let Some(message) = interruption.as_deref() {
                        super::held::record_close_failure(
                            db,
                            &id,
                            state,
                            &original_hold,
                            expired_at,
                            message,
                        )?;
                    }
                    return Ok(());
                };
                state.closing_held_generation = None;
                let reason = match interruption.as_deref() {
                    Some(message) => super::held::Retirement::Interrupted(message),
                    None if reached_deadline => super::held::Retirement::Deadline,
                    None => super::held::Retirement::Close,
                };
                super::held::settle_retirement(
                    db,
                    &id,
                    state,
                    reason,
                    retiring.expires_at,
                    expired_at,
                )
            })
            .await?;

        if close_after_settlement
            && let Some(error) =
                super::held::close_control_failure(close_environment().await, &close_request)
        {
            let id = self.id.clone();
            let message = run_output::redact(&error.message, &self.sensitive.snapshot());
            self.checkpoint
                .decide(move |state, db| {
                    super::held::record_close_failure(db, &id, state, &held, expired_at, &message)
                })
                .await?;
        }
        Ok(false)
    }

    async fn prepare_retained_turn(&mut self, workspace: &mut Workspace) -> Result<()> {
        let s = self.s;
        self.mark_running().await?;
        self.refresh_access().await?;
        let authority =
            crate::capabilities::plugin_packages::Authority::for_run(s, self.run).await?;
        let credentials = crate::plugins::run_credentials::for_execution_authorized(
            s,
            self.run,
            &crate::project_workspaces::catalog(self.run),
            authority.clone(),
        )
        .await?;
        self.sensitive.extend(credentials.secrets.iter().cloned());
        let previous_files = execution::credential_files(&self.directory).await;
        if workspace.prepared["backend"] == "native"
            && credentials
                .files
                .iter()
                .any(|(path, _)| !previous_files.contains(path))
        {
            return Err(Error::conflict(
                "New credential paths require a fresh environment.",
            ));
        }
        execution::run_credentials(&self.directory.join("home"), &self.directory, &credentials)
            .await?;
        workspace.authorization =
            crate::plugins::run_credentials::Authorization::new(s, authority, &credentials);
        let home = self.prepare_state_home(&workspace.prepared).await?;
        workspace.env = self.environment(true, &home).await?;
        workspace.state_home = home;
        workspace.mcp = s.mcps.run_configuration(s, self.run).await?;
        self.sensitive.extend(
            workspace.mcp["redactions"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(str::to_owned),
        );
        for (key, value) in workspace.mcp["env"].as_object().unwrap() {
            workspace
                .env
                .insert(key.clone(), value.as_str().unwrap_or("").into());
        }
        Ok(())
    }
}

#[path = "launch_runner.rs"]
mod runner;

use runner::*;

#[cfg(test)]
#[path = "launch_tests.rs"]
mod tests;
