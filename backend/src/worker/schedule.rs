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
    store::Store,
    validation::text,
};
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
        s.chat_tick(&active).await?;
        s.schedule().await?;
        if s.shutdown.is_cancelled() || s.store.kv("deployment-lease").await?.is_some() {
            return Ok(());
        }
        let mut projects = HashSet::new();
        for id in &active {
            projects.extend(locked_projects(s, &s.store.run(id).await?));
        }
        for run in s.store.read(|db| db.active()).await? {
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
            self.try_launch(s, run, &mut projects).await?;
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
                db.0.execute("DELETE FROM events WHERE created_at<? AND run_id IN (SELECT id FROM runs WHERE status NOT IN ('queued','running') AND json_extract(data,'$.trigger')!='chat')", [now() - 30 * DAY_MS])?;
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
        projects: &mut HashSet<String>,
    ) -> Result<()> {
        let run_id = text(&run, "id").to_owned();
        if run["status"] != RunStatus::Queued
            || locked_projects(s, &run).any(|project| projects.contains(&project))
        {
            return Ok(());
        }
        let recovering = run["recoveryPending"] == true;
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
        // Serialize only the preparation window, until placement has reserved
        // the node slot. Running VMs use node slots without a global ceiling.
        let preparation = if execution::uses_vm(&run, &s.config) {
            let Ok(guard) = self.preparation.clone().try_lock_owned() else {
                return Ok(());
            };
            Some(guard)
        } else {
            None
        };
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
                return report_capacity_wait(s, &current, &error.message).await;
            }
        }
        let provider = Provider::of_run(&run);
        let model = text(&run["snapshot"]["agent"], "model");
        let account = match s.accounts.acquire(s, &run_id, provider, model).await {
            Ok(account) => account,
            Err(error) => return report_account_wait(s, &run, provider, &error).await,
        };
        if s.shutdown.is_cancelled() || s.store.run(&run_id).await?["status"] != RunStatus::Queued {
            if let Some(account) = account {
                s.accounts.release(&account).await?;
            }
            return Ok(());
        }
        projects.extend(locked_projects(s, &run));
        let cancel = CancellationToken::new();
        self.active
            .lock()
            .await
            .insert(run_id.clone(), cancel.clone());
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
        Ok(())
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
    run: &Value,
    provider: Provider,
    error: &Error,
) -> Result<()> {
    if run["accountWaitReason"] == error.message.as_str() {
        return Ok(());
    }
    let run_id = text(run, "id");
    // Clients offer Connections when the wait needs the user.
    let required = s
        .accounts
        .needs_attention(s, provider)
        .await?
        .then_some(provider);
    let patch = json!({ "accountWaitReason": error.message, "accountRequired": required });
    s.store.patch_run(run_id, patch).await?;
    s.store.event(run_id, "status", &error.message, None).await
}

/// Shows why `run` waits for a node. Updated figures replace the reason
/// without adding another status event.
pub(super) async fn report_capacity_wait(s: &Service, run: &Value, detail: &str) -> Result<()> {
    let reason = format!("{CAPACITY_WAIT_REASON} {detail}");
    let previous = run["accountWaitReason"].as_str().unwrap_or_default();
    if previous == reason {
        return Ok(());
    }
    let run_id = text(run, "id");
    let patch = json!({ "accountWaitReason": reason, "accountRequired": null });
    s.store.patch_run(run_id, patch).await?;
    if previous.starts_with(CAPACITY_WAIT_REASON) {
        return Ok(());
    }
    s.store.event(run_id, "status", &reason, None).await
}

/// Returns whether a recovering run may launch again now.
async fn recover_or_wait(s: &Service, run: &Value) -> Result<bool> {
    if let Ok(ready) = recover(s, run).await {
        return Ok(ready);
    }
    if run["accountWaitReason"] != RECOVERY_WAIT_REASON {
        let patch = json!({ "accountWaitReason": RECOVERY_WAIT_REASON });
        s.store.patch_run(text(run, "id"), patch).await?;
    }
    Ok(false)
}

/// Fences the previous execution, then either settles the run from its
/// checkpoint or clears it for relaunch (`true`).
async fn recover(s: &Service, run: &Value) -> Result<bool> {
    let run_id = text(run, "id");
    if !crate::nodes::moves::advance(s, run).await? {
        return Ok(false);
    }
    recovery::fence(s, run).await?;
    s.mcps.revoke_run(s, run_id).await?;
    s.accounts.recover_run(s, run).await?;
    if !s.store.run(run_id).await?["cancelRequestedAt"].is_null() {
        let patch = json!({
            "status": RunStatus::Cancelled,
            "finishedAt": now(),
            "accountWaitReason": null,
            "recoveryPending": false,
        });
        s.store.patch_run(run_id, patch).await?;
        return Ok(false);
    }
    if let Some(checkpoint) = RunCheckpoint::load(&s.store, run_id).await? {
        if let Some(settled) = checkpoint.settled() {
            s.store.patch_run(run_id, settled.run_patch()?).await?;
            return Ok(false);
        }
        if checkpoint.completed() {
            let summary = match checkpoint.last_message() {
                "" => {
                    "Conversation completed before worker restart. See Activity for the recorded result."
                }
                message => message,
            };
            let patch = json!({
                "status": RunStatus::Succeeded,
                "finishedAt": now(),
                "resumeAvailable": false,
                "accountWaitReason": null,
                "recoveryPending": false,
                "summary": summary,
            });
            s.store.patch_run(run_id, patch).await?;
            return Ok(false);
        }
    }
    s.store
        .patch_run(run_id, json!({ "recoveryPending": false }))
        .await?;
    Ok(true)
}

/// Hands a run whose execution failed unexpectedly back to recovery.
async fn requeue_after_error(store: &Store, run_id: &str, lease: Option<Lease>) -> Result<()> {
    let id = run_id.to_owned();
    store
        .transaction(move |db| {
            let Some(current) = db.run(&id)? else {
                return Ok(());
            };
            let mut checkpoint =
                RunCheckpoint::load_in(db, &id)?.unwrap_or_else(RunCheckpoint::exhausted);
            if !RunStatus::of(&current).is_some_and(RunStatus::is_active)
                && let Some(settled) = Settled::from_run(&current)
            {
                checkpoint.settled = Some(Some(settled));
            }
            checkpoint.store_in(db, &id)?;
            let mut patch = json!({
                "status": RunStatus::Queued,
                "recoveryPending": true,
                "finishedAt": null,
                "accountWaitReason": "Recovering after a worker error.",
            });
            if let Some(lease) = lease {
                patch["accountId"] = lease.account_id.into();
            }
            db.patch_run(&id, &patch)?;
            Ok(())
        })
        .await
}
