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
                db.0.execute("DELETE FROM records WHERE kind='node-exclusive-requests' AND json_extract(data,'$.state') IN ('released','failed','expired') AND json_extract(data,'$.createdAt')<?", [now() - 30 * DAY_MS])?;
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

        // Preparation can leave the durable status queued until the launch completes.
        // Its existing reservation must keep owning the execution and its stop token.
        if self.active.lock().await.contains_key(&run_id) {
            return Ok(());
        }

        let run_preparation = self.run_preparation_lock(&run_id).await;
        let _run_preparation = run_preparation.lock_owned().await;
        let current_id = run_id.clone();
        let Some(run) = s.store.read(move |db| db.run(&current_id)).await? else {
            return Ok(());
        };

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
        let eligibility_id = run_id.clone();
        if !s
            .store
            .read(move |db| eligible_run(db, &eligibility_id))
            .await?
        {
            return Ok(());
        }
        let current = s.store.run(&run_id).await?;
        if current["status"] != RunStatus::Queued
            || crate::run_retry::Retry::waiting(&current, now())
        {
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
                return report_capacity_wait(s, &current, &error.message).await;
            }
        }
        let provider = Provider::of_run(&run)?;
        timing.next("account_acquire");
        let model = text(&run["snapshot"]["agent"], "model");
        let account = match s.accounts.acquire(s, &run_id, provider, model).await {
            Ok(account) => account,
            Err(error) => return report_account_wait(s, &run, provider, &error).await,
        };
        let cancel = CancellationToken::new();
        match self.register(s, &run_id, cancel.clone()).await {
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
    async fn register(&self, s: &Service, run_id: &str, cancel: CancellationToken) -> Result<bool> {
        let mut active = self.active.lock().await;
        if s.shutdown.is_cancelled() || active.contains_key(run_id) {
            return Ok(false);
        }
        let id = run_id.to_owned();
        let eligible = s.store.read(move |db| eligible_run(db, &id)).await?;
        if !eligible {
            return Ok(false);
        }
        active.insert(run_id.to_owned(), cancel);
        Ok(true)
    }
}

fn eligible_run(db: &Db<'_>, id: &str) -> Result<bool> {
    let Some(run) = db.run(id)? else {
        return Ok(false);
    };
    if run["status"] != RunStatus::Queued || !run["cancelRequestedAt"].is_null() {
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
    if run["accountWaitReason"] != RECOVERY_WAIT_REASON
        && !text(run, "accountWaitReason").starts_with(CAPACITY_WAIT_REASON)
    {
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
        s.store.patch_run(run_id, cancelled_recovery()).await?;
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
    // A cancellation committed since the check above left this recovering run
    // to the worker. Check again in the same transaction that ends recovery.
    let id = run_id.to_owned();
    s.store
        .transaction(move |db| {
            let current = db
                .run(&id)?
                .ok_or_else(|| Error::not_found("Run not found"))?;
            if !current["cancelRequestedAt"].is_null() {
                db.patch_run(&id, &cancelled_recovery())?;
                return Ok(false);
            }
            db.patch_run(&id, &json!({ "recoveryPending": false }))?;
            Ok(true)
        })
        .await
}

/// Settles a recovering run whose cancellation was requested.
fn cancelled_recovery() -> Value {
    json!({
        "status": RunStatus::Cancelled,
        "finishedAt": now(),
        "accountWaitReason": null,
        "recoveryPending": false,
        "retry": null,
    })
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

#[cfg(test)]
mod tests {
    use super::*;
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
            gh_bin: "unused".into(),
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
                let run = json!({ "id": run_id, "status": RunStatus::Queued });
                db.0.execute(
                    "INSERT INTO runs(id,task_id,project_id,status,created_at,data) VALUES(?1,?1,'','queued',0,?2)",
                    rusqlite::params![run_id, run.to_string()],
                )?;
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
                    diagnostic: false,
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
        let admission = service.worker.try_launch(&service, run, &mut projects);
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
                .kv(&format!("run-checkpoint:{run_id}"))
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
    async fn deletion_committed_before_registration_prevents_stale_admission() {
        let (_root, service, chat_id, run_id, workspace) = fixture().await;

        assert!(
            service
                .worker
                .register(&service, &run_id, CancellationToken::new())
                .await
                .unwrap()
        );
        assert!(
            !service
                .worker
                .register(&service, &run_id, CancellationToken::new())
                .await
                .unwrap()
        );
        service.worker.active.lock().await.remove(&run_id);

        let active = service.worker.active.lock().await;
        let registration = service
            .worker
            .register(&service, &run_id, CancellationToken::new());
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
                .kv(&format!("run-checkpoint:{run_id}"))
                .await
                .unwrap()
                .is_none()
        );
        assert!(!workspace.exists());
        assert!(
            !service
                .worker
                .register(&service, &run_id, CancellationToken::new())
                .await
                .unwrap()
        );
    }
}
