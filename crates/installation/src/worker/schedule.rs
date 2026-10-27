use super::{
    Admission, Worker, audit,
    checkpoint::{RunCheckpoint, Settled},
};
use crate::{
    accounts::Lease,
    config::now,
    error::{Error, Result},
    execution,
    provider::Provider,
    recovery,
    run_status::RunStatus,
    service::{Service, run_projects},
    store::{Db, Store},
    validation::text,
};
use cairn_contracts::EventKind;
use cairn_contracts::RunState;
use serde_json::{Value, json};
use std::{
    collections::HashSet,
    sync::{Arc, atomic::Ordering},
    time::Duration,
};
use tokio_util::sync::CancellationToken;

const MAINTENANCE_INTERVAL_MS: i64 = 3_600_000;
const DAY_MS: i64 = 86_400_000;
const RECOVERY_WAIT_REASON: &str = "Waiting for the previous execution to stop before recovery.";
/// Starts the reason of a run that stays queued until a node has room for it.
pub(super) const CAPACITY_WAIT_REASON: &str = "Waiting for capacity.";

impl Worker {
    pub async fn tick(self: &Arc<Self>, s: &Arc<Service>) -> Result<()> {
        let Ok(_tick) = self.tick_lock.try_lock() else {
            return Ok(());
        };
        if s.shutdown.is_cancelled() {
            return Ok(());
        }
        self.initialize(s).await?;
        self.maintain(s).await?;
        let active = self
            .active
            .lock()
            .await
            .keys()
            .cloned()
            .collect::<HashSet<_>>();
        // Parked missions must release any pre-crash execution independently
        // of their wakeup and of admission capacity for new processes.
        for state in s.store.read(|db| db.active_run_states()).await? {
            let run_id = state.id.as_str();
            if state.status == RunStatus::Waiting
                && state.recovery_pending
                && !active.contains(run_id)
            {
                let preparation = self.run_preparation_lock(run_id).await;
                let _preparation = preparation.lock().await;
                recover_or_wait(s, &s.store.run(run_id).await?).await?;
            }
        }
        crate::waits::tick_groups(s, &active).await?;
        s.chat_tick(&active).await?;
        crate::waits::tick_missions(s, &active).await?;
        if s.shutdown.is_cancelled() || s.store.kv("deployment-lease").await?.is_some() {
            return Ok(());
        }
        let mut projects = HashSet::new();
        for id in &active {
            projects.extend(locked_projects(s, &s.store.run(id).await?));
        }
        for record in s.store.read(|db| db.queued_run_records()).await? {
            let state = record.state.clone();
            let run = record.wire()?;
            if !execution::uses_vm(&run, &s.config) {
                let active_ids = self.active.lock().await.keys().cloned().collect::<Vec<_>>();
                let mut host_runs = 0;
                for id in active_ids {
                    if !execution::uses_vm(&s.store.run(&id).await?, &s.config) {
                        host_runs += 1;
                    }
                }
                if host_runs >= s.config.concurrency {
                    continue;
                }
            }
            self.try_launch(s, run, state, &mut projects).await?;
        }
        Ok(())
    }

    /// Hourly pruning of expired keys and old audit/event history.
    async fn maintain(&self, s: &Service) -> Result<()> {
        if now() - self.maintenance.load(Ordering::Relaxed) <= MAINTENANCE_INTERVAL_MS {
            return Ok(());
        }
        s.store
            .write(|db| {
                db.0.execute("DELETE FROM kv WHERE expires IS NOT NULL AND expires<=?", [now()])?;
                db.0.execute("DELETE FROM audit WHERE created_at<?", [now() - 90 * DAY_MS])?;
                db.0.execute("DELETE FROM records WHERE kind='node-exclusive-requests' AND json_extract(data,'$.state') IN ('released','failed','expired') AND json_extract(data,'$.createdAt')<?", [now() - 30 * DAY_MS])?;
                db.0.execute("DELETE FROM events WHERE created_at<? AND run_id IN (SELECT id FROM runs WHERE status NOT IN ('queued','running','waiting') AND trigger!='chat')", [now() - 30 * DAY_MS])?;
                Ok(())
            })
            .await?;
        self.maintenance.store(now(), Ordering::Relaxed);
        Ok(())
    }

    /// Starts `run` when it is queued, its projects are free and an account is available.
    async fn try_launch(
        self: &Arc<Self>,
        s: &Arc<Service>,
        run: Value,
        state: RunState,
        projects: &mut HashSet<String>,
    ) -> Result<()> {
        let run_id = text(&run, "id").to_owned();
        if state.status != RunStatus::Queued
            || locked_projects(s, &run).any(|project| projects.contains(&project))
        {
            return Ok(());
        }

        // Preparation can leave the durable status queued until the launch completes.
        // Its existing reservation must keep owning the execution and its stop token.
        if self.active.lock().await.contains_key(&run_id) {
            return Ok(());
        }

        let run_preparation = self.run_preparation_lock(&run_id).await;
        let _run_preparation = run_preparation.lock_owned().await;
        let current_id = run_id.clone();
        let Some(record) = s.store.read(move |db| db.run_record(&current_id)).await? else {
            return Ok(());
        };

        let state = record.state.clone();
        let run = record.wire()?;
        let state = &state;
        let recovering = state.recovery_pending;
        let launched = RunCheckpoint::load(&s.store, &run_id)
            .await?
            .is_some_and(|checkpoint| checkpoint.launched());
        // A recovering run may still own its workspace, so keep it reserved even
        // when it cannot start in this pass.
        if recovering || launched {
            projects.extend(locked_projects(s, &run));
        }
        if recovering && !recover_or_wait(s, &run).await? {
            return Ok(());
        }
        let eligibility_id = run_id.clone();
        if !s
            .store
            .read(move |db| eligible_run(db, &eligibility_id))
            .await?
        {
            return Ok(());
        }
        let current = s.store.run_state(&run_id).await?;
        if current.status != RunStatus::Queued
            || crate::run_retry::Retry::waiting(current.retry.as_ref(), now())
        {
            return Ok(());
        }
        if super::retry::expire(s, &run_id).await? {
            return Ok(());
        }
        // Serialize only the preparation window, until placement has reserved
        // the node slot. Running VMs use node slots without a global ceiling.
        let preparation = if execution::uses_vm(&run, &s.config) {
            let Ok(guard) = self.preparation.clone().try_lock_owned() else {
                tracing::info!(
                    target: "cairn_performance",
                    operation = "manager_admission",
                    id = crate::performance::identity(&run_id),
                    phase = "preparation_lock",
                    event = "deferred"
                );
                return Ok(());
            };
            Some(guard)
        } else {
            None
        };
        let mut timing =
            crate::performance::Operation::new("manager_admission", &run_id, "capacity_check");
        // Waiting here keeps the account free and retries on every tick.
        if execution::uses_vm(&run, &s.config) {
            let current = s.store.run(&run_id).await?;
            if let Err(error) = crate::nodes::placement::check(s, &current).await
                && error.is_unavailable()
            {
                if crate::nodes::placement::is_no_capacity(&error)
                    && crate::nodes::moves::queue_capacity_move(s, &current).await?
                {
                    return Ok(());
                }
                return report_capacity_wait(s, &s.store.run_state(&run_id).await?, &error.message)
                    .await;
            }
        }
        let provider = Provider::of_run(&run)?;
        timing.next("account_acquire");
        let model = text(&run["snapshot"]["agent"], "model");
        let speed = text(&run["snapshot"]["agent"], "speed");
        let account = match s
            .accounts
            .acquire_for(s, &run_id, provider, model, speed)
            .await
        {
            Ok(account) => account,
            Err(error) => return report_account_wait(s, state, provider, &error).await,
        };
        let cancel = CancellationToken::new();
        match self.register(s, &run, cancel.clone()).await {
            Ok(true) => {}
            result => {
                if let Some(account) = account {
                    s.accounts.release(&account).await?;
                }
                return result.map(|_| ());
            }
        }
        projects.extend(locked_projects(s, &run));
        timing.next("launch");
        let worker = self.clone();
        let s = s.clone();
        self.tasks.spawn(async move {
            run_to_completion(
                &s,
                run,
                Admission {
                    account,
                    preparation,
                },
                cancel,
            )
            .await;
            worker.active.lock().await.remove(&run_id);
            worker.notify();
        });
        timing.finish();
        Ok(())
    }

    /// Rechecks persisted eligibility under the cancellation/cleanup lock before admission.
    async fn register(
        &self,
        s: &Service,
        prepared: &Value,
        cancel: CancellationToken,
    ) -> Result<bool> {
        let run_id = text(prepared, "id");
        let mut active = self.active.lock().await;
        if s.shutdown.is_cancelled() || active.contains_key(run_id) {
            return Ok(false);
        }
        let id = run_id.to_owned();
        let expected_agent = prepared["snapshot"]["agent"].clone();
        let expected_message = crate::waits::execution_id(prepared).clone();
        let eligible = s
            .store
            .transaction(move |db| {
                if !eligible_run(db, &id)? || super::retry::expire_in(db, &id)? {
                    return Ok(false);
                }
                let current = db
                    .run(&id)?
                    .ok_or_else(|| Error::not_found("Run not found"))?;
                // Account preparation can outlive cancellation, merge and resume.
                // Its account and captured execution belong to this exact selection.
                Ok(current["snapshot"]["agent"] == expected_agent
                    && *crate::waits::execution_id(&current) == expected_message)
            })
            .await?;
        if !eligible {
            return Ok(false);
        }
        active.insert(run_id.to_owned(), cancel);
        Ok(true)
    }
}

fn eligible_run(db: &Db<'_>, id: &str) -> Result<bool> {
    let Some(record) = db.run_record(id)? else {
        return Ok(false);
    };
    if record.boundary["backendMigration"].is_object()
        || record.state.status != RunStatus::Queued
        || record.state.cancel_requested_at.is_some()
    {
        return Ok(false);
    }
    if RunCheckpoint::load_in(db, id)?
        .and_then(|checkpoint| checkpoint.capacity_retry_at)
        .is_some_and(|deadline| deadline > now())
    {
        return Ok(false);
    }
    match crate::conversation_lifecycle::require_active_run(db, id) {
        Ok(()) => Ok(true),
        Err(error) if error.is_conflict() => Ok(false),
        Err(error) => Err(error),
    }
}

async fn run_to_completion(
    s: &Arc<Service>,
    run: Value,
    admission: Admission,
    cancel: CancellationToken,
) {
    let run_id = text(&run, "id").to_owned();
    let Err(error) = super::execution::execute(s, run, admission, cancel).await else {
        return;
    };
    // Keep the slot occupied until recovery is durable. A transient
    // disk/database failure must not strand a running record or
    // launch a second process before the previous one is fenced.
    loop {
        let lease = s.accounts.lease(&run_id).await;
        match requeue_after_error(&s.store, &run_id, lease).await {
            Ok(()) => break,
            Err(error) => {
                tracing::warn!(run_id = %run_id, error = %error, "Could not requeue run after a worker error");
            }
        }
        tokio::select! {
            () = s.shutdown.cancelled() => break,
            () = tokio::time::sleep(Duration::from_secs(1)) => {}
        }
    }
    let detail = json!({ "runId": run_id, "message": error.message });
    audit(&s.store, "worker.execution_failed", detail).await;
}

/// Projects whose checkouts a run holds on this host; VM runs work on their own disk.
fn locked_projects(s: &Service, run: &Value) -> impl Iterator<Item = String> {
    let projects = if execution::uses_vm(run, &s.config) {
        Vec::new()
    } else {
        run_projects(run)
    };
    projects
        .into_iter()
        .map(|project| text(&project, "id").to_owned())
}

async fn report_account_wait(
    s: &Service,
    run: &RunState,
    provider: Provider,
    error: &Error,
) -> Result<()> {
    if run.account_wait_reason.as_deref() == Some(error.message.as_str()) {
        return Ok(());
    }
    let run_id = &run.id;
    // Clients offer Connections when the wait needs the user.
    let required = s
        .accounts
        .needs_attention(s, provider)
        .await?
        .then_some(provider);
    let patch = crate::runs::RunChanges {
        account_wait_reason: Some(Some((error.message).to_owned())),
        account_required: Some(serde_json::from_value(serde_json::json!(required))?),
        ..Default::default()
    };
    s.store.change_run(run_id, patch).await?;
    s.store
        .event(run_id, &EventKind::Status, &error.message, None)
        .await
}

/// Shows why `run` waits for a node. Updated figures replace the reason
/// without adding another status event.
pub(super) async fn report_capacity_wait(s: &Service, run: &RunState, detail: &str) -> Result<()> {
    let reason = format!("{CAPACITY_WAIT_REASON} {detail}");
    let previous = run.account_wait_reason.as_deref().unwrap_or_default();
    if previous == reason {
        return Ok(());
    }
    let run_id = &run.id;
    let patch = crate::runs::RunChanges {
        account_wait_reason: Some(Some((reason).to_owned())),
        account_required: Some(None),
        ..Default::default()
    };
    s.store.change_run(run_id, patch).await?;
    if previous.starts_with(CAPACITY_WAIT_REASON) {
        return Ok(());
    }
    s.store
        .event(run_id, &EventKind::Status, &reason, None)
        .await
}

/// Returns whether a recovering run may launch again now.
async fn recover_or_wait(s: &Service, run: &Value) -> Result<bool> {
    if let Ok(ready) = recover(s, run).await {
        return Ok(ready);
    }
    if run["accountWaitReason"] != RECOVERY_WAIT_REASON
        && !text(run, "accountWaitReason").starts_with(CAPACITY_WAIT_REASON)
    {
        let patch = crate::runs::RunChanges {
            account_wait_reason: Some(Some((RECOVERY_WAIT_REASON).to_owned())),
            ..Default::default()
        };
        s.store.change_run(text(run, "id"), patch).await?;
    }
    Ok(false)
}

/// Fences the previous execution, then either settles the run from its
/// checkpoint or clears it for relaunch (`true`).
async fn recover(s: &Service, run: &Value) -> Result<bool> {
    let run_id = text(run, "id");
    let checkpoint = RunCheckpoint::load(&s.store, run_id).await?;
    let state = s.store.run_state(run_id).await?;
    let parked = state.status == RunStatus::Waiting
        || (state.trigger != "chat"
            && run["wakeup"].is_object()
            && checkpoint.as_ref().is_some_and(RunCheckpoint::completed));
    // A parked turn only needs fencing/release, including a completed turn
    // requeued because its first fencing attempt failed. Placement and possible
    // disk relocation belong to admission after its wakeup.
    if !parked && !crate::nodes::moves::advance(s, run).await? {
        return Ok(false);
    }
    recovery::fence(s, run).await?;
    s.mcps.revoke_run(s, run_id).await?;
    s.accounts.recover_run(s, run).await?;
    let checkpoint = RunCheckpoint::load(&s.store, run_id).await?;
    let id = run_id.to_owned();
    s.store
        .transaction(move |db| finish_recovery(db, &id, checkpoint.as_ref()))
        .await
}

/// Cancellation and the final recovery state commit together. In particular,
/// a completed provider turn must never restore a wait cancellation removed.
fn finish_recovery(db: &Db<'_>, id: &str, checkpoint: Option<&RunCheckpoint>) -> Result<bool> {
    let record = db
        .run_record(id)?
        .ok_or_else(|| Error::not_found("Run not found"))?;
    let current = &record.state;
    if current.cancel_requested_at.is_some() {
        cancel_recovery(db, id, current)?;
        return Ok(false);
    }
    if let Some(checkpoint) = checkpoint {
        if let Some(settled) = checkpoint.settled() {
            let mut patch = settled.run_changes();
            if settled.status == RunStatus::Waiting {
                patch.resume_available = Some(true);
                patch.boundary = Some(json!({"outcome": null}));
            }
            db.change_run(id, patch)?;
            return Ok(false);
        }
        if checkpoint.completed() {
            let summary = match checkpoint.last_message() {
                "" => {
                    "Conversation completed before worker restart. See Activity for the recorded result."
                }
                message => message,
            };
            let waiting = current.trigger != "chat" && record.boundary["wakeup"].is_object();
            if waiting && current.session_id.is_none() {
                db.change_run(id,crate::runs::RunChanges {
                    status: Some(RunStatus::Failed),
                    finished_at: Some(Some(now())),
                    recovery_pending: Some(false),
                    account_wait_reason: Some(None),
                    error: Some(Some("Cannot wait without a saved agent session. Working files were preserved.".into())),
                    boundary: Some(json!({"wakeup": null})),
                    ..Default::default()
                })?;
                return Ok(false);
            }
            let mut patch = crate::runs::RunChanges {
                status: Some(if waiting {
                    RunStatus::Waiting
                } else {
                    RunStatus::Succeeded
                }),
                finished_at: Some(if waiting { None } else { Some(now()) }),
                resume_available: Some(waiting),
                account_wait_reason: Some(None),
                recovery_pending: Some(false),
                summary: Some((summary).to_owned()),
                boundary: Some(json!({})),
                ..Default::default()
            };
            if waiting {
                patch.boundary.as_mut().unwrap()["outcome"] = Value::Null;
            } else {
                // A compaction completed before the restart settles like one that did not.
                crate::chats::keep_reply_after_compaction(&record.boundary, &mut patch)?;
            }
            db.change_run(id, patch)?;
            return Ok(false);
        }
    }
    db.change_run(
        id,
        crate::runs::RunChanges {
            recovery_pending: Some(false),
            ..Default::default()
        },
    )?;
    Ok(true)
}

/// Settles a recovering run whose cancellation was requested. One waiting for an automatic
/// retry shows the stop in its conversation; a wait's own card already shows its cancellation.
fn cancel_recovery(db: &Db<'_>, id: &str, run: &RunState) -> Result<()> {
    db.change_run(
        id,
        crate::runs::RunChanges {
            status: Some(RunStatus::Cancelled),
            finished_at: Some(Some(now())),
            account_wait_reason: Some(None),
            recovery_pending: Some(false),
            retry: Some(None),
            boundary: Some(json!({"wakeup": null})),
            ..Default::default()
        },
    )?;
    if run
        .retry
        .as_ref()
        .and_then(|retry| retry.next_attempt_at)
        .is_some()
    {
        db.event(id, &EventKind::Status, RunStatus::Cancelled.as_str(), None)?;
    }
    Ok(())
}

/// Hands a run whose execution failed unexpectedly back to recovery.
async fn requeue_after_error(store: &Store, run_id: &str, lease: Option<Lease>) -> Result<()> {
    let id = run_id.to_owned();
    store
        .transaction(move |db| {
            let Some(current) = db.run_state(&id)? else {
                return Ok(());
            };
            let mut checkpoint =
                RunCheckpoint::load_in(db, &id)?.unwrap_or_else(RunCheckpoint::exhausted);
            if !current.status.is_active() {
                checkpoint.settled = Some(Some(Settled::from_state(&current)));
            }
            checkpoint.store_in(db, &id)?;
            let mut patch = crate::runs::RunChanges {
                status: Some(RunStatus::Queued),
                recovery_pending: Some(true),
                finished_at: Some(None),
                account_wait_reason: Some(Some("Recovering after a worker error.".into())),
                ..Default::default()
            };
            if let Some(lease) = lease {
                patch.account_id = Some(Some((lease.account_id).to_owned()));
            }
            db.change_run(&id, patch)?;
            Ok(())
        })
        .await
}

#[cfg(test)]
use crate::run_fixture::DbRunFixture;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_fixture::AgentFixture;
    use std::{future::Future, task::Poll};

    async fn fixture() -> (
        tempfile::TempDir,
        Arc<Service>,
        String,
        String,
        std::path::PathBuf,
    ) {
        let root = tempfile::tempdir().unwrap();
        let service = Service::new(crate::config::Config {
            data_dir: root.path().join("data"),
            home: root.path().join("home"),
            workspace_roots: vec![root.path().into()],
            public_url: "http://localhost:4310".into(),
            host: "127.0.0.1".into(),
            port: 0,
            codex_bin: "unused".into(),
            claude_bin: "unused".into(),
            opencode_bin: "unused".into(),
            opencode_go_usage_url: crate::accounts::opencode::USAGE_URL.into(),
            concurrency: 1,
            logger: false,
            worker_enabled: false,
            runner_url: String::new(),
        })
        .await
        .unwrap();
        let mut chat = service.chat_create(json!({})).await.unwrap();
        let chat_id = text(&chat, "id").to_owned();
        let run_id = crate::config::id();
        service.store.transaction({
            let run_id = run_id.clone();
            move |db| {
                let run = json!({
                    "id": run_id,
                    "taskId": run_id,
                    "status": RunStatus::Queued,
                    "snapshot": { "agent": { "id": crate::config::MAIN_AGENT_ID, "provider": "codex" } },
                });
                db.add_run_fixture(&run, None)?;
                Ok(())
            }
        }).await.unwrap();
        chat["runId"] = run_id.clone().into();
        service.store.put("chats", chat).await.unwrap();
        let workspace = service.config.data_dir.join("runs").join(&run_id);
        tokio::fs::create_dir_all(&workspace).await.unwrap();
        tokio::fs::write(workspace.join("work.txt"), "Private work")
            .await
            .unwrap();

        (root, service, chat_id, run_id, workspace)
    }

    #[tokio::test]
    async fn waiting_mission_restart_releases_attempt_before_deadline_even_when_cancelled() {
        for (cancel, remote, queued) in [
            (false, false, false),
            (true, false, false),
            (false, true, false),
            (true, true, false),
            (false, false, true),
            (true, false, true),
            (false, true, true),
            (true, true, true),
        ] {
            let (_root, original, chat_id, run_id, _workspace) = fixture().await;
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let requests = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let observed = requests.clone();
            let remote_observed = requests.clone();
            let app = axum::Router::new()
                .route(
                    "/runs/{attempt}",
                    axum::routing::delete(move || async move {
                        observed.fetch_add(1, Ordering::Relaxed);
                        axum::http::StatusCode::NOT_FOUND
                    }),
                )
                .route(
                    "/internal/execution/{node}/runs/{attempt}",
                    axum::routing::delete(move || async move {
                        remote_observed.fetch_add(1, Ordering::Relaxed);
                        axum::http::StatusCode::SERVICE_UNAVAILABLE
                    }),
                );
            let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
            let mut config = original.config.clone();
            config.runner_url = format!("http://{address}");
            config.public_url = config.runner_url.clone();
            let node = if remote {
                "offline-node"
            } else {
                crate::nodes::LOCAL_NODE_ID
            };
            original
                .store
                .transaction(move |db| db.remove("chats", &chat_id))
                .await
                .unwrap();
            drop(original);
            let service = Service::new(config).await.unwrap();
            let wakeup = json!({
                "id": "wake", "seconds": 3600,
                "createdAt": now(), "wakeAt": now() + 3_600_000,
            });
            service
                .store
                .change_run(
                    &run_id,
                    crate::runs::RunChanges {
                        status: Some(if queued {
                            RunStatus::Queued
                        } else {
                            RunStatus::Waiting
                        }),
                        recovery_pending: Some(queued),
                        trigger: Some(("manual").to_owned()),
                        session_id: Some(Some("saved".into())),
                        boundary: Some(json!({"isolated": true,"wakeup": wakeup})),
                        ..Default::default()
                    },
                )
                .await
                .unwrap();
            let checkpoint = RunCheckpoint::from_value(json!({
                "completed": true, "launched": true, "runnerId": "old-attempt",
                "nodeId": node,
                "prepared": { "isolated": true, "backend": "native" },
            }))
            .unwrap();
            checkpoint.store(&service.store, &run_id).await.unwrap();
            service
                .store
                .put(
                    "node-attempts",
                    json!({
                        "id": "old-attempt", "runId": run_id,
                        "nodeId": node, "released": false,
                    }),
                )
                .await
                .unwrap();

            if remote {
                // The previous remote execution lease has expired; its node
                // cannot serve requests and has no available placement record.
                service.node_lease_deadlines.lock().await.insert(
                    "old-attempt".into(),
                    tokio::time::Instant::now() - Duration::from_secs(30),
                );
            }
            service.worker.initialize(&service).await.unwrap();
            if cancel {
                service.worker.cancel(&service, &run_id).await.unwrap();
            }
            service.worker.tick(&service).await.unwrap();

            assert_eq!(requests.load(Ordering::Relaxed), 1);
            let attempt = service
                .store
                .get("node-attempts", "old-attempt")
                .await
                .unwrap()
                .unwrap();
            assert_eq!(attempt["released"], true);
            let current = service.store.run(&run_id).await.unwrap();
            assert_eq!(current["recoveryPending"], false);
            assert_eq!(
                current["status"],
                if cancel { "cancelled" } else { "waiting" }
            );
            if cancel {
                assert!(current["wakeup"].is_null());
            } else {
                assert_eq!(current["wakeup"], wakeup);
                assert!(current["missionTurnId"].is_null());
            }
            assert!(service.worker.active.lock().await.is_empty());
            server.abort();
        }
    }

    #[tokio::test]
    async fn completed_mission_recovery_keeps_resume_and_cancellation_wins_atomically() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path()).unwrap();
        store
            .transaction(|db| {
                let record = json!({
                    "id": "mission", "taskId": "task", "status": "queued", "createdAt": 0,
                    "recoveryPending": true, "sessionId": "saved", "resumeAvailable": false,
                    "wakeup": { "id": "wake" },
                    "outcome": { "status": "completed" },
                });
                db.add_run_fixture(&record, None)?;
                let checkpoint = RunCheckpoint::from_value(json!({ "completed": true }))?;
                assert!(!finish_recovery(db, "mission", Some(&checkpoint))?);
                let waiting = db.run("mission")?.unwrap();
                assert_eq!(waiting["status"], "waiting");
                assert_eq!(waiting["resumeAvailable"], true);
                assert!(waiting["finishedAt"].is_null());
                assert!(waiting["outcome"].is_null());
                for settled in [false, true] {
                    // A stale recovery snapshot preceded this cancellation. Completion
                    // must read it in the same transaction that writes the next state.
                    db.change_run(
                        "mission",
                        crate::runs::RunChanges {
                            status: Some(cairn_contracts::RunStatus::Queued),
                            recovery_pending: Some(true),
                            cancel_requested_at: Some(Some(now())),
                            boundary: Some(json!({"wakeup": null})),
                            ..Default::default()
                        },
                    )?;
                    let mut checkpoint = checkpoint.clone();
                    if settled {
                        checkpoint.settled = Some(Some(Settled::from_state(
                            &db.run_state("mission")?.unwrap(),
                        )));
                    }
                    assert!(!finish_recovery(db, "mission", Some(&checkpoint))?);
                    let cancelled = db.run("mission")?.unwrap();
                    assert_eq!(cancelled["status"], "cancelled");
                    assert!(cancelled["wakeup"].is_null());
                    assert_eq!(cancelled["recoveryPending"], false);
                    assert!(db.active()?.is_empty());
                }
                Ok(())
            })
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn compaction_completed_before_restart_keeps_the_last_reply_and_outcome() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path()).unwrap();
        store
            .transaction(|db| {
                let outcome = json!({ "status": "completed", "messageId": "first" });
                let record = json!({
                    "id": "chat-run",
                    "taskId": "chat",
                    "trigger": "chat",
                    "status": "queued",
                    "createdAt": 0,
                    "recoveryPending": true,
                    "sessionId": "saved",
                    "summary": "Last reply",
                    "finishedAt": 500,
                    // Resuming the interrupted compaction cleared the run's outcome.
                    "outcome": null,
                    "chatExecution": {
                        "messageId": "compact:fixture",
                        "text": "",
                        "attachments": [],
                        "recovery": false,
                        "operation": "compact",
                        "previous": { "summary": "Last reply", "finishedAt": 500, "outcome": outcome },
                    },
                });
                db.add_run_fixture(&record, None)?;

                // The compaction's `turn.completed` was saved; the manager stopped before
                // settling the run, and a compaction leaves no reply in the checkpoint.
                let checkpoint = RunCheckpoint::from_value(json!({ "completed": true }))?;
                assert_eq!(checkpoint.last_message(), "");
                assert!(!finish_recovery(db, "chat-run", Some(&checkpoint))?);

                let recovered = db.run("chat-run")?.unwrap();
                assert_eq!(recovered["status"], "succeeded");
                assert_eq!(recovered["recoveryPending"], false);
                assert_eq!(recovered["summary"], "Last reply");
                assert_eq!(recovered["finishedAt"], 500);
                assert_eq!(recovered["outcome"], outcome);
                Ok(())
            })
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn completed_turn_with_late_controller_failure_settles_without_relaunch() {
        let (_root, service, _chat_id, run_id, _workspace) = fixture().await;
        let checkpoint = super::super::checkpoint::Checkpoint::new(
            service.store.clone(),
            run_id.clone(),
            RunCheckpoint::fresh(None),
        );
        let mut activity =
            crate::performance::Activity::new(&run_id, "fixture", "host", Provider::Codex);
        let mut log_total = 500_000;
        for event in [
            json!({ "type": "turn.completed" }),
            json!({ "type": "runner.failed", "error": { "message": "Workspace sealing failed" } }),
        ] {
            super::super::output::record(
                &service,
                &super::super::output::Output {
                    kind: super::super::output::OutputKind::Stdout,
                    raw: event.to_string(),
                    received: tokio::time::Instant::now(),
                },
                &checkpoint,
                false,
                &[],
                &mut log_total,
                &mut activity,
            )
            .await
            .unwrap();
        }
        let saved = RunCheckpoint::load(&service.store, &run_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(saved.last_error(), "Workspace sealing failed");
        let run = service.store.run(&run_id).await.unwrap();
        assert!(!recover(&service, &run).await.unwrap());
        assert_eq!(
            service.store.run(&run_id).await.unwrap()["status"],
            "succeeded"
        );
    }

    #[tokio::test]
    async fn deletion_during_account_preparation_leaves_no_orphan_files() {
        let (_root, service, chat_id, run_id, workspace) = fixture().await;
        service.accounts.initialize(&service).await.unwrap();
        let mut account = service
            .accounts
            .create(&service, Provider::Codex, "Ready")
            .await
            .unwrap();
        account["state"] = "ready".into();
        account["usage"] = json!({
            "allowed": true,
            "checkedAt": now(),
            "windows": [{ "id": "w", "usedPercent": 20, "models": [] }],
            "resets": null
        });
        service
            .store
            .put(crate::accounts::KIND, account.clone())
            .await
            .unwrap();
        let account_guard = service.accounts.lock(text(&account, "id")).await;
        let run = service.store.run(&run_id).await.unwrap();
        let mut projects = HashSet::new();
        let state = service.store.run_state(&run_id).await.unwrap();
        let admission = service
            .worker
            .try_launch(&service, run, state, &mut projects);
        tokio::pin!(admission);
        let account_mutex = tokio::sync::OwnedMutexGuard::mutex(&account_guard);
        let owners = Arc::strong_count(account_mutex);
        tokio::time::timeout(
            Duration::from_secs(2),
            std::future::poll_fn(|cx| {
                assert!(admission.as_mut().poll(cx).is_pending());
                if Arc::strong_count(account_mutex) > owners {
                    Poll::Ready(())
                } else {
                    Poll::Pending
                }
            }),
        )
        .await
        .unwrap();

        service.chat_trash(&chat_id, true).await.unwrap();
        let other = service.chat_create(json!({})).await.unwrap();
        let other_id = text(&other, "id");
        service.chat_trash(other_id, true).await.unwrap();
        service.cleanup_conversations().await.unwrap();
        service.cleanup_conversations().await.unwrap();
        assert!(
            service
                .store
                .get("chats", other_id)
                .await
                .unwrap()
                .is_none(),
            "Blocked preparation must not stall unrelated deletion"
        );

        drop(account_guard);
        admission.await.unwrap();
        assert!(service.worker.active.lock().await.is_empty());
        assert!(service.accounts.lease(&run_id).await.is_none());
        service.cleanup_conversations().await.unwrap();
        assert!(
            service
                .store
                .get("chats", &chat_id)
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(service.store.run(&run_id).await.unwrap_err().status, 404);
        assert!(
            service
                .store
                .checkpoint_boundary(&run_id)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            !workspace.exists(),
            "Account preparation must not recreate a purged run directory"
        );
    }

    #[tokio::test]
    async fn a_retry_that_expires_during_preparation_cannot_register() {
        let (_root, service, _chat_id, run_id, workspace) = fixture().await;
        let _prepared = service
            .store
            .change_run(
                &run_id,
                crate::runs::RunChanges {
                    retry: Some(Some(
                        serde_json::from_value(json!({
                            "attempt": 1,
                            "limit": 5,
                            "nextAttemptAt": now() - 1,
                            "deadlineAt": now() + 60_000,
                            "cause": {"kind":"timeout","message":"temporary"},
                        }))
                        .unwrap(),
                    )),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let prepared = service.store.run(&run_id).await.unwrap();
        // Admission captured an eligible run, then waits behind asynchronous work.
        let active = service.worker.active.lock().await;
        let registration = service
            .worker
            .register(&service, &prepared, CancellationToken::new());
        tokio::pin!(registration);
        std::future::poll_fn(|cx| {
            assert!(registration.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
        service
            .store
            .change_run(
                &run_id,
                crate::runs::RunChanges {
                    retry: Some({
                        let mut retry = service.store.run_state(&run_id).await.unwrap().retry;
                        retry.as_mut().unwrap().deadline_at = Some(now() - 1);
                        retry
                    }),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        drop(active);
        assert!(!registration.await.unwrap());
        let stopped = service.store.run(&run_id).await.unwrap();
        assert_eq!(stopped["status"], RunStatus::Failed);
        assert!(stopped["retry"]["nextAttemptAt"].is_null());
        assert!(service.worker.active.lock().await.is_empty());
        assert_eq!(
            tokio::fs::read_to_string(workspace.join("work.txt"))
                .await
                .unwrap(),
            "Private work"
        );
    }

    #[tokio::test]
    async fn deletion_committed_before_registration_prevents_stale_admission() {
        let (_root, service, chat_id, run_id, workspace) = fixture().await;
        let prepared = service.store.run(&run_id).await.unwrap();

        assert!(
            service
                .worker
                .register(&service, &prepared, CancellationToken::new())
                .await
                .unwrap()
        );
        assert!(
            !service
                .worker
                .register(&service, &prepared, CancellationToken::new())
                .await
                .unwrap()
        );
        service.worker.active.lock().await.remove(&run_id);

        let active = service.worker.active.lock().await;
        let registration = service
            .worker
            .register(&service, &prepared, CancellationToken::new());
        tokio::pin!(registration);
        std::future::poll_fn(|cx| {
            assert!(registration.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
        let deleting = service.clone();
        let deleting_id = chat_id.clone();
        let deletion = tokio::spawn(async move { deleting.chat_trash(&deleting_id, true).await });
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let chat = service.store.get("chats", &chat_id).await.unwrap().unwrap();
                let run = service.store.run(&run_id).await.unwrap();
                if chat["lifecycle"] == "trash" && !run["cancelRequestedAt"].is_null() {
                    assert_eq!(run["status"], RunStatus::Queued);
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        drop(active);

        assert!(
            !registration.await.unwrap(),
            "A deleted conversation must not gain an execution"
        );
        deletion.await.unwrap().unwrap();
        assert!(service.worker.active.lock().await.is_empty());
        service.cleanup_conversations().await.unwrap();
        assert!(
            service
                .store
                .get("chats", &chat_id)
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(service.store.run(&run_id).await.unwrap_err().status, 404);
        assert!(
            service
                .store
                .checkpoint_boundary(&run_id)
                .await
                .unwrap()
                .is_none()
        );
        assert!(!workspace.exists());
        assert!(
            !service
                .worker
                .register(&service, &prepared, CancellationToken::new())
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn an_admission_prepared_before_merge_cannot_register_after_resume() {
        let (_root, service, chat_id, run_id, _workspace) = fixture().await;
        let source = service
            .agent_fixture(json!({ "name": "Source" }), None)
            .await
            .unwrap();
        let run_id_copy = run_id.clone();
        let source_copy = source.clone();
        let prepared = service
            .store
            .transaction(move |db| {
                crate::run_fixture::change_run_fixture(
                    db,
                    &run_id_copy,
                    &json!({
                        "snapshot": { "agent": source_copy },
                        "chatExecution": { "messageId": "original-message" },
                    }),
                )
            })
            .await
            .unwrap();
        service
            .store
            .change_run(
                &run_id,
                crate::runs::RunChanges {
                    status: Some(RunStatus::Cancelled),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        service
            .store
            .transaction({
                let chat_id = chat_id.clone();
                let source = source.clone();
                move |db| {
                    let mut chat = db.get("chats", &chat_id)?.unwrap();
                    chat["agentId"] = source["id"].clone();
                    db.put("chats", &chat)?;
                    Ok(())
                }
            })
            .await
            .unwrap();
        service
            .merge_agents(serde_json::from_value(json!({
                "sourceId": source["id"], "targetId": crate::config::MAIN_AGENT_ID, "confirm": true,
            })).unwrap())
            .await
            .unwrap();
        let resumed = service.worker.resume(&service, &run_id).await.unwrap();
        assert!(
            !service
                .worker
                .register(&service, &prepared, CancellationToken::new())
                .await
                .unwrap()
        );
        assert!(service.worker.active.lock().await.is_empty());
        assert!(
            service
                .worker
                .register(&service, &resumed, CancellationToken::new())
                .await
                .unwrap()
        );
    }
}
