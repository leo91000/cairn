pub(crate) mod checkpoint;

use cairn_contracts::EventKind;

pub(crate) mod control;
mod execution;
pub(crate) mod held;
pub(crate) mod jobs;
mod launch;
mod output;
pub(crate) mod progress;
mod retry;
mod schedule;

pub use control::routes;

use crate::{
    config::now,
    error::{Error, Result},
    run_status::RunStatus,
    service::Service,
    store::{Db, Store},
};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{Arc, Weak, atomic::AtomicI64},
    time::Duration,
};
use tokio::sync::{Mutex, Notify, OnceCell, OwnedMutexGuard};
use tokio_util::{sync::CancellationToken, task::TaskTracker};

/// Optional selection evidence that must stay tied to this resume through its commit.
#[derive(Default)]
pub(crate) struct ResumeSelectionContext {
    pub(crate) expected_turn_id: Option<String>,
    pub(crate) selection: Option<cairn_contracts::ResumeExecutionSelection>,
    pub(crate) selected_provider: Option<crate::provider::Provider>,
    pub(crate) saved_auto_catalog: Option<Value>,
}

#[derive(Default)]
pub struct Worker {
    pub active: Mutex<HashMap<String, CancellationToken>>,
    held: Mutex<std::collections::HashSet<String>>,
    pub(crate) job_redactions: Mutex<HashMap<String, crate::accounts::output_redactions::History>>,
    tick_lock: Mutex<()>,
    preparation: Arc<Mutex<()>>,
    run_preparation_locks: Mutex<HashMap<String, Weak<Mutex<()>>>>,
    wake: Notify,
    conversation_cleanup: Notify,
    retirement: Notify,
    retirement_admission: Notify,
    pub(crate) retirement_cleanup: Mutex<()>,
    initialized: OnceCell<()>,
    maintenance: AtomicI64,
    tasks: TaskTracker,
}

/// The account lease and a short guard until VM placement becomes durable.
struct Admission {
    account: Option<crate::accounts::Lease>,
    preparation: Option<OwnedMutexGuard<()>>,
}

impl Worker {
    /// Keep durable conversation recovery alive until graceful shutdown completes.
    pub(crate) fn spawn_recovery(
        &self,
        task: impl std::future::Future<Output = ()> + Send + 'static,
    ) {
        self.tasks.spawn(task);
    }

    /// Coordinates admission/recovery filesystem writes with permanent run cleanup.
    pub(crate) async fn run_preparation_lock(&self, run_id: &str) -> Arc<Mutex<()>> {
        let mut locks = self.run_preparation_locks.lock().await;
        locks.retain(|_, lock| lock.strong_count() > 0);
        let lock = locks
            .get(run_id)
            .and_then(Weak::upgrade)
            .unwrap_or_default();
        locks.insert(run_id.to_owned(), Arc::downgrade(&lock));
        lock
    }

    pub async fn initialize(&self, s: &Service) -> Result<()> {
        self.initialized
            .get_or_try_init(|| s.store.transaction(restore_after_restart))
            .await?;
        Ok(())
    }

    pub async fn start(self: &Arc<Self>, s: Arc<Service>) -> Result<()> {
        self.initialize(&s).await?;
        self.tasks.spawn(crate::chat_titles::run(s.clone()));
        self.tasks
            .spawn(crate::project_archives::maintain(s.clone()));
        self.tasks
            .spawn(crate::plugins::conditions::maintain(s.clone()));
        // Disks are local to their node: no publication, eviction or remote
        // object collection runs. Storage status is still observed.
        self.tasks.spawn(crate::nodes::storage::monitor(s.clone()));
        self.tasks
            .spawn(crate::nodes::exclusive::maintain(s.clone()));
        self.tasks.spawn(clean_conversations(s.clone()));
        self.tasks.spawn(detach_environments(s.clone()));
        self.tasks.spawn(retire_environments(s.clone()));
        self.tasks.spawn(schedule_tasks(s.clone()));
        self.tasks.spawn(observe_background_jobs(s.clone()));
        self.tasks.spawn(self.clone().run_forever(s));
        Ok(())
    }

    async fn run_forever(self: Arc<Self>, s: Arc<Service>) {
        let mut timer = tokio::time::interval(Duration::from_secs(1));
        timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                () = s.shutdown.cancelled() => break,
                _ = timer.tick() => {},
                () = self.wake.notified() => {},
            }
            if let Err(error) = self.tick(&s).await {
                audit(
                    &s.store,
                    "worker.error",
                    json!({ "message": error.message }),
                )
                .await;
            }
        }
    }

    pub fn notify_conversation_cleanup(&self) {
        self.conversation_cleanup.notify_one();
    }

    /// Wakes the retirement of expired environments, after an expiration or a stopped run.
    pub fn notify_retirement(&self) {
        self.retirement.notify_one();
        self.retirement_admission.notify_one();
    }

    /// Coalesce wakeups without losing work queued during an active scheduling pass.
    pub fn notify(&self) {
        self.wake.notify_one();
    }

    pub async fn deployment_lease(
        &self,
        s: &Service,
        owner: String,
        release: bool,
    ) -> Result<Value> {
        // Finish an in-flight scheduling pass before reporting the worker idle.
        // Otherwise account selection could launch a run after the lease returns.
        let tick = self.tick_lock.lock().await;
        s.store
            .transaction(move |db| {
                if db
                    .kv("deployment-lease")?
                    .is_some_and(|value| value != owner)
                {
                    return Err(Error::conflict(
                        "Another deployment holds the worker lease.",
                    ));
                }
                if release {
                    db.delete("deployment-lease")?;
                } else {
                    db.set("deployment-lease", &owner.into(), Some(now() + 20 * 60000))?;
                }
                Ok(())
            })
            .await?;
        let active_runs = self.active.lock().await.len();
        drop(tick);
        if release {
            self.notify();
        }
        Ok(json!({ "paused": !release, "activeRuns": active_runs }))
    }

    pub async fn close(&self) {
        let _guard = self.tick_lock.lock().await;
        self.tasks.close();
        self.tasks.wait().await;
    }
}

/// Beacon author checks and snapshot preparation never hold the launch tick.
async fn schedule_tasks(s: Arc<Service>) {
    let mut timer = tokio::time::interval(Duration::from_secs(1));
    timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            () = s.shutdown.cancelled() => break,
            _ = timer.tick() => {},
        }

        tokio::select! {
            () = s.shutdown.cancelled() => break,
            result = s.schedule() => {
                if let Err(error) = result {
                    audit(&s.store, "schedule.failed", json!({ "error": error.message })).await;
                }
            },
        }
    }
}

/// Recovers executions and unfinished cleanup from the previous worker process.
fn restore_after_restart(db: &mut Db<'_>) -> Result<()> {
    db.restore_chat_summaries(None)?;
    crate::nodes::placement::release_unclaimed(db, None)?;
    // Earlier versions removed an expired environment before the next message
    // and moved environments between nodes. Neither survives an update.
    crate::environment_expiration::finish_legacy_cleaning(db)?;
    crate::nodes::moves::abandon_legacy_moves(db)?;
    crate::backend_migration::abandon_legacy_conversions(db)?;
    // A controller may still own jobs after manager loss. Invalidate their
    // outcome explicitly and enqueue existing fencing, never replay commands.
    let held = db.0.prepare("SELECT run_id,state FROM run_checkpoints WHERE json_type(state,'$.heldEnvironment')='object'")?
        .query_map([], |row| Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (id, state) in held {
        let mut saved: checkpoint::RunCheckpoint = serde_json::from_str(&state)?;
        if let Some(held) = saved.held_environment.take() {
            let closing =
                saved.closing_held_generation.take().as_deref() == Some(held.generation.as_str());
            saved.retired_environment_deadline = Some(held.expires_at);
            jobs::unknown(
                db,
                &id,
                &held.attempt,
                "The manager restarted while background work was active; completion is unknown.",
            )?;
            let current = db
                .run_state(&id)?
                .ok_or_else(|| Error::not_found("Run not found"))?;
            if closing && current.status == RunStatus::Queued && saved.turn_pending == Some(true) {
                // The accepted fresh turn already owns its own input boundary.
                saved.retired_environment_deadline = None;
            }
            let running_chat = if current.status == RunStatus::Running && current.trigger == "chat"
            {
                db.run(&id)?
            } else {
                None
            };
            let message = running_chat
                .as_ref()
                .and_then(|run| run["chatExecution"]["messageId"].as_str());
            // The retained controller can start before the client process
            // is recorded. A launch claim keeps its normal reconciliation.
            let awaiting_launch = saved.turn_pending == Some(true)
                && message.is_some()
                && saved.launched_turn.as_deref() != message;
            let received = saved
                .confirmed_retained_completion(&held.attempt, message)
                .filter(|_| current.cancel_requested_at.is_none() && saved.settled().is_none());
            let received_completion = received.is_some();
            let expired = !closing && now() >= held.expires_at;
            let held_between_turns =
                matches!(current.status, RunStatus::Waiting | RunStatus::Succeeded)
                    || (current.status == RunStatus::Queued && saved.turn_pending == Some(true));
            if let (Some(receipt), Some(run)) = (received, running_chat.as_ref()) {
                let mut patch = crate::runs::RunChanges {
                    status: Some(RunStatus::Succeeded),
                    summary: Some(
                        receipt
                            .output_summary
                            .as_deref()
                            .unwrap_or(saved.last_message())
                            .to_owned(),
                    ),
                    finished_at: Some(Some(receipt.finished_at)),
                    error: Some(None),
                    retry: Some(None),
                    resume_available: Some(run["sessionId"].is_string()),
                    ..Default::default()
                };
                crate::chats::keep_reply_after_compaction(run, &mut patch)?;
                db.change_run(&id, patch)?;
                if !expired {
                    let completed = db
                        .run_state(&id)?
                        .ok_or_else(|| Error::not_found("Run not found"))?;
                    saved.settled = Some(Some(checkpoint::Settled::from_state(&completed)));
                }
            } else if !expired
                && current.cancel_requested_at.is_none()
                && saved.settled().is_none()
                && (held::completed_chat(db, &current)?
                    || (closing
                        && current.status == RunStatus::Succeeded
                        && current.trigger == "chat"))
            {
                // Older checkpoints already published success without a turn
                // receipt. Fencing the environment still cannot undo it.
                saved.settled = Some(Some(checkpoint::Settled::from_state(&current)));
            }
            if expired
                && saved.settled().is_none()
                && (held_between_turns || awaiting_launch || received_completion)
            {
                held::settle_retirement(
                    db,
                    &id,
                    &mut saved,
                    held::Retirement::Deadline,
                    held.expires_at,
                    Some(held.expires_at),
                )?;
                saved.store_in(db, &id)?;
                continue;
            }
            saved.store_in(db, &id)?;
            let status = if saved
                .settled()
                .is_some_and(|outcome| outcome.status == RunStatus::Succeeded)
            {
                RunStatus::Succeeded
            } else if matches!(current.status, RunStatus::Queued | RunStatus::Running) {
                RunStatus::Queued
            } else {
                RunStatus::Waiting
            };
            db.event(&id, &EventKind::Status,
                "Background work was interrupted by a manager restart; the old environment will be stopped before resuming.", None)?;
            db.change_run(
                &id,
                crate::runs::RunChanges {
                    status: Some(status),
                    recovery_pending: Some(true),
                    ..Default::default()
                },
            )?;
        }
    }
    for run in db.active_run_states()? {
        let id = &run.id;
        if run.status != RunStatus::Running {
            // Waiting was committed before fencing and release. A restart must
            // finish that cleanup even when the wakeup is still far away.
            if run.status == RunStatus::Waiting || run.cancel_requested_at.is_some() {
                db.change_run(
                    id,
                    crate::runs::RunChanges {
                        recovery_pending: Some(true),
                        ..Default::default()
                    },
                )?;
            }
            continue;
        }
        if checkpoint::RunCheckpoint::load_in(db, id)?.is_none() && run.started_at.is_some() {
            let patch = crate::runs::RunChanges {
                status : Some(RunStatus::Interrupted),
                finished_at : Some(Some(now())),
                summary : Some(("This run has no restart checkpoint. Review its working files before retrying.").to_owned()),
                ..Default::default()
            };
            db.change_run(id, patch)?;
            continue;
        }
        let patch = crate::runs::RunChanges {
            status: Some(RunStatus::Queued),
            recovery_pending: Some(true),
            finished_at: Some(None),
            capacity_wait_until: Some(None),
            account_wait_reason: Some(Some("Recovering after worker restart.".into())),
            ..Default::default()
        };
        db.change_run(id, patch)?;
        db.event(
            id,
            &EventKind::Status,
            "Recovering after worker restart",
            None,
        )?;
    }
    Ok(())
}

async fn clean_conversations(s: Arc<Service>) {
    let mut timer = tokio::time::interval(Duration::from_secs(15));
    timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            () = s.shutdown.cancelled() => break,
            _ = timer.tick() => {}
            () = s.worker.conversation_cleanup.notified() => {}
        }
        tokio::select! {
            () = s.shutdown.cancelled() => break,
            result = s.cleanup_conversations() => {
                if let Err(error) = result {
                    let detail = json!({ "message": error.message });
                    audit(&s.store, "conversation.cleanup_failed", detail).await;
                }
            }
        }
    }
}

/// Admission owns only execution fencing and logical namespace detachment.
/// Slow physical erasure has a separate loop and never delays this one.
async fn detach_environments(s: Arc<Service>) {
    let mut timer = tokio::time::interval(Duration::from_secs(5));
    timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            () = s.shutdown.cancelled() => break,
            _ = timer.tick() => {},
            () = s.worker.retirement_admission.notified() => {},
        }
        let work = async {
            s.detach_stopped_environments().await?;
            s.detach_retired_environments().await?;
            schedule::recover_completed_environments(&s).await
        };
        tokio::select! {
            () = s.shutdown.cancelled() => break,
            result = work => {
                if let Err(error) = result {
                    audit(&s.store, "environment.detachment_failed", json!({ "message": error.message })).await;
                }
            },
        }
    }
}

/// Detaches stopped expired environments and retires their old disks and files.
async fn retire_environments(s: Arc<Service>) {
    let mut timer = tokio::time::interval(Duration::from_secs(5));
    timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            () = s.shutdown.cancelled() => break,
            _ = timer.tick() => {}
            () = s.worker.retirement.notified() => {}
        }
        tokio::select! {
            () = s.shutdown.cancelled() => break,
            result = s.cleanup_retired_environments() => {
                if let Err(error) = result {
                    let detail = json!({ "message": error.message });
                    audit(&s.store, "environment.retirement_failed", detail).await;
                }
            }
        }
    }
}

/// Records an audit entry for a failure the worker already handled.
async fn audit(store: &Store, action: &str, detail: Value) {
    if let Err(error) = store.audit(action, detail).await {
        tracing::warn!(action, error = %error, "Could not record worker audit entry");
    }
}

/// Controller observations run independently from admission and heartbeat.
async fn observe_background_jobs(s: Arc<Service>) {
    let mut timer = tokio::time::interval(Duration::from_secs(1));
    timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            () = s.shutdown.cancelled() => return,
            _ = timer.tick() => {}
        }
        tokio::select! {
            () = s.shutdown.cancelled() => return,
            observed = jobs::refresh_all(&s) => {
                if let Err(error) = observed {
                    tracing::warn!(error = %error, "Background job observation failed");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{mission_fixture::MissionFixture, validation::text};

    #[tokio::test]
    async fn restart_preserves_queued_turn_while_fencing_held_background_work() {
        use crate::run_fixture::DbRunFixture;
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path()).unwrap();
        store
            .transaction(|db| {
                db.add_run_fixture(
                    &json!({"id":"held", "taskId":"task", "status":"queued"}),
                    None,
                )?;
                let mut saved = checkpoint::RunCheckpoint::fresh(None);
                saved.runner_id = Some(Some("attempt".into()));
                saved.completed = Some(false);
                saved.turn_pending = Some(true);
                saved.held_environment = Some(checkpoint::HeldEnvironment {
                    attempt: "attempt".into(),
                    generation: "generation".into(),
                    expires_at: now() + 60_000,
                });
                saved.store_in(db, "held")?;
                restore_after_restart(db)?;
                let run = db.run_state("held")?.unwrap();
                assert_eq!(run.status, RunStatus::Queued);
                assert!(run.recovery_pending);
                let saved = checkpoint::RunCheckpoint::load_in(db, "held")?.unwrap();
                assert_eq!(saved.turn_pending, Some(true));
                assert!(!saved.completed());
                assert!(saved.held_environment.is_none());
                Ok(())
            })
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn restart_recovers_a_receipt_committed_before_run_success() {
        use crate::run_fixture::DbRunFixture;
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path()).unwrap();
        store
            .transaction(|db| {
                let finished_at = now() - 2_000;
                for (id, receipt, expires_at) in [
                    ("matched-expired", "current", finished_at + 1_000),
                    ("matched-live", "current", finished_at + 60_000),
                    ("stale-expired", "previous", finished_at + 1_000),
                    ("stale-live", "previous", finished_at + 60_000),
                ] {
                    let chat_id = format!("chat-{id}");
                    db.add_run_fixture(&json!({
                    "id": id, "taskId": chat_id, "status": "running", "trigger": "chat",
                    "chatExecution": {"messageId": "current", "text": "Work", "attachments": []},
                }), None)?;
                    db.save_conversation_boundary(&json!({"id": chat_id, "runId": id}))?;
                    let mut saved = checkpoint::RunCheckpoint::fresh(None);
                    saved.runner_id = Some(Some("attempt".into()));
                    saved.completed = Some(true);
                    saved.last_message = Some("The delivered answer".to_owned());
                    saved.held_environment = Some(checkpoint::HeldEnvironment {
                        attempt: "attempt".into(),
                        generation: "generation".into(),
                        expires_at,
                    });
                    saved.retained_completion = Some(checkpoint::RetainedCompletion {
                        message_id: receipt.into(),
                        finished_at,
                        output_summary: None,
                    });
                    saved.store_in(db, id)?;
                }
                db.add_run_fixture(&json!({
                    "id": "legacy", "taskId": "chat-legacy", "status": "succeeded", "trigger": "chat",
                    "summary": "The delivered answer", "finishedAt": finished_at,
                }), None)?;
                db.save_conversation_boundary(&json!({"id": "chat-legacy", "runId": "legacy"}))?;
                let mut legacy = checkpoint::RunCheckpoint::fresh(None);
                legacy.runner_id = Some(Some("attempt".into()));
                legacy.completed = Some(true);
                legacy.held_environment = Some(checkpoint::HeldEnvironment {
                    attempt: "attempt".into(),
                    generation: "generation".into(),
                    expires_at: finished_at + 60_000,
                });
                legacy.store_in(db, "legacy")?;
                restore_after_restart(db)?;
                for id in ["matched-expired", "matched-live", "legacy"] {
                    let saved = checkpoint::RunCheckpoint::load_in(db, id)?.unwrap();
                    let settled = saved.settled().unwrap();
                    assert_eq!(settled.status, RunStatus::Succeeded);
                    assert_eq!(settled.finished_at, Some(finished_at));
                    assert_eq!(settled.summary.as_deref(), Some("The delivered answer"));
                    let run = db.run_state(id)?.unwrap();
                    assert!(run.recovery_pending);
                    assert_eq!(run.status, RunStatus::Succeeded);
                    assert_eq!(run.finished_at, Some(finished_at));
                }
                for id in ["stale-expired", "stale-live"] {
                    let saved = checkpoint::RunCheckpoint::load_in(db, id)?.unwrap();
                    assert!(saved.settled().is_none());
                }
                Ok(())
            })
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn restart_after_expiry_keeps_the_result_already_waiting_for_a_fence() {
        use crate::run_fixture::DbRunFixture;
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path()).unwrap();
        store
            .transaction(|db| {
                db.add_run_fixture(
                &json!({"id":"held", "taskId":"task", "status":"queued", "recoveryPending":true}),
                None,
            )?;
                let mut saved = checkpoint::RunCheckpoint::fresh(None);
                saved.runner_id = Some(Some("attempt".into()));
                saved.held_environment = Some(checkpoint::HeldEnvironment {
                    attempt: "attempt".into(),
                    generation: "generation".into(),
                    expires_at: now() - 1,
                });
                saved.settled = Some(Some(checkpoint::Settled::failure(
                    RunStatus::Failed,
                    "The previous provider turn failed.",
                )));
                saved.store_in(db, "held")?;
                restore_after_restart(db)?;
                let saved = checkpoint::RunCheckpoint::load_in(db, "held")?.unwrap();
                assert_eq!(
                    saved.settled().unwrap().error.as_deref(),
                    Some("The previous provider turn failed.")
                );
                assert!(saved.held_environment.is_none());
                assert!(db.run_state("held")?.unwrap().recovery_pending);
                Ok(())
            })
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn restart_preserves_a_wait_when_ordinary_close_started_before_the_cutoff() {
        use crate::run_fixture::DbRunFixture;
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path()).unwrap();
        store.transaction(|db| {
            let finished_at = now() - 2_000;
            db.add_run_fixture(&json!({
                "id": "closing", "taskId": "chat", "status": "succeeded", "trigger": "chat", "finishedAt": finished_at,
            }), None)?;
            db.save_conversation_boundary(&json!({"id": "chat", "runId": "closing"}))?;
            let mut chat = db.conversation("chat")?.unwrap();
            chat.wakeup = Some(serde_json::from_value(json!({
                "id": "wait", "messageId": "previous", "reason": "Waiting for a delay",
                "seconds": 60, "createdAt": finished_at, "wakeAt": finished_at + 60_000,
            }))?);
            db.save_conversation(&chat)?;
            let mut saved = checkpoint::RunCheckpoint::fresh(None);
            saved.runner_id = Some(Some("attempt".into()));
            saved.closing_held_generation = Some("generation".into());
            saved.held_environment = Some(checkpoint::HeldEnvironment {
                attempt: "attempt".into(), generation: "generation".into(), expires_at: finished_at + 1_000,
            });
            saved.store_in(db, "closing")?;
            restore_after_restart(db)?;
            let saved = checkpoint::RunCheckpoint::load_in(db, "closing")?.unwrap();
            assert!(saved.closing_held_generation.is_none());
            assert!(saved.held_environment.is_none());
            assert_eq!(saved.settled().unwrap().status, RunStatus::Succeeded);
            let run = db.run_state("closing")?.unwrap();
            assert!(run.recovery_pending);
            assert_eq!(run.status, RunStatus::Succeeded);
            assert_eq!(run.finished_at, Some(finished_at));
            assert!(db.conversation("chat")?.unwrap().wakeup.is_some());
            Ok(())
        }).await.unwrap();
    }

    #[tokio::test]
    async fn restart_releases_an_unclaimed_fresh_environment_reservation_but_keeps_execution_proofs()
     {
        use crate::{attempt_fixture::DbAttemptFixture, run_fixture::DbRunFixture};
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path()).unwrap();
        store.transaction(|db| {
            db.add_run_fixture(&json!({ "id": "run", "taskId": "task", "status": "succeeded", "createdAt": 0, "environmentExpiredAt": 1 }), None)?;
            checkpoint::RunCheckpoint { runner_id: Some(Some("bound".into())), ..Default::default() }.store_in(db, "run")?;
            for (attempt, leased) in [("unclaimed", false), ("bound", false), ("leased", true)] {
                db.put_attempt_fixture(&json!({ "id": attempt, "runId": "run", "released": false, "leaseRequired": leased, "diskMaterialized": false }))?;
            }
            restore_after_restart(db)?;
            assert_eq!(db.attempt_fixture("unclaimed")?.unwrap()["released"], true);
            assert_eq!(db.attempt_fixture("bound")?.unwrap()["released"], false);
            assert_eq!(db.attempt_fixture("leased")?.unwrap()["released"], false);
            Ok(())
        }).await.unwrap();
    }

    #[tokio::test]
    async fn local_storage_observation_never_schedules_a_publication() {
        use crate::{node_fixture::StoreNodeFixture, run_fixture::DbRunFixture};
        use std::sync::atomic::{AtomicUsize, Ordering};
        let root = tempfile::tempdir().unwrap();
        let observed = Arc::new(AtomicUsize::new(0));
        let calls = observed.clone();
        let controller = axum::Router::new().fallback(move |request: axum::extract::Request| {
            let calls = calls.clone();
            async move {
                if request.uri().path().ends_with("/storage-status") {
                    calls.fetch_add(1, Ordering::SeqCst);
                }
                axum::Json(json!({ "mode": "on-demand", "dirtyBytes": 1, "localBytes": 4096 }))
            }
        });
        let (listener, address) = crate::common::bind().await;
        let server = crate::common::serve(listener, controller);
        let mut config = crate::common::config(root.path());
        config.runner_url = format!("http://{address}");
        let service = Service::new(config).await.unwrap();
        service
            .store
            .put_node_fixture(json!({ "id": crate::nodes::LOCAL_NODE_ID, "revoked": false }))
            .await
            .unwrap();
        let run = crate::config::id();
        service
            .store
            .transaction({
                let run = run.clone();
                move |db| {
                    db.add_run_fixture(
                        &json!({
                            "id": run, "taskId": run, "status": "succeeded", "createdAt": now(),
                            "isolated": true, "nodeId": crate::nodes::LOCAL_NODE_ID,
                            "storage": { "mode": "on-demand", "dirtyBytes": 1 }
                        }),
                        None,
                    )?;
                    Ok(())
                }
            })
            .await
            .unwrap();
        service.worker.start(service.clone()).await.unwrap();
        tokio::time::timeout(Duration::from_secs(10), async {
            while observed.load(Ordering::SeqCst) == 0 {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(service.store.run(&run).await.unwrap()["backup"].is_null());
        service.shutdown.cancel();
        server.abort();
    }

    #[tokio::test]
    async fn committed_work_and_deployment_release_wake_the_scheduler() {
        let root = tempfile::TempDir::new().unwrap();
        std::fs::create_dir(root.path().join("home")).unwrap();
        let config = crate::config::Config {
            data_dir: root.path().join("data"),
            home: root.path().join("home"),
            workspace_roots: vec![root.path().to_owned()],
            public_url: "http://localhost:4310".into(),
            host: "127.0.0.1".into(),
            port: 0,
            codex_bin: "unused".into(),
            claude_bin: "claude".into(),
            opencode_bin: "opencode".into(),
            opencode_go_usage_url: crate::accounts::opencode::USAGE_URL.into(),
            antigravity_bin: "agy".into(),
            concurrency: 1,
            logger: false,
            worker_enabled: false,
            runner_url: String::new(),
        };
        let s = Service::new(config).await.unwrap();
        let chat = s
            .chat_create(json!({"agentId":crate::config::MAIN_AGENT_ID}))
            .await
            .unwrap();
        let chat_id = text(&chat, "id");
        s.chat_send(chat_id, json!({"id":crate::config::id(),"text":"Hello"}))
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_millis(100), s.worker.wake.notified())
            .await
            .unwrap();
        assert_eq!(
            s.chat_detail(chat_id).await.unwrap()["messages"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert!(s.chat_send(chat_id, json!({"text":""})).await.is_err());
        assert!(
            tokio::time::timeout(Duration::from_millis(10), s.worker.wake.notified())
                .await
                .is_err()
        );
        let task = s
            .task_fixture(
                json!({"name":"Wake test","prompt":"Hello","agentId":crate::config::MAIN_AGENT_ID}),
                None,
            )
            .await
            .unwrap();
        let run = s.enqueue(text(&task, "id"), "manual", None).await.unwrap();
        tokio::time::timeout(Duration::from_millis(100), s.worker.wake.notified())
            .await
            .unwrap();
        s.worker
            .deployment_lease(&s, "test".into(), false)
            .await
            .unwrap();
        s.worker.tick(&s).await.unwrap();
        assert!(s.worker.active.lock().await.is_empty());
        assert_eq!(
            s.store.run(text(&run, "id")).await.unwrap()["status"],
            "queued"
        );
        s.worker
            .deployment_lease(&s, "test".into(), true)
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_millis(100), s.worker.wake.notified())
            .await
            .unwrap();
        assert!(s.store.kv("deployment-lease").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn scheduling_wakeups_survive_a_busy_worker_and_coalesce() {
        let worker = Worker::default();
        let busy = worker.tick_lock.lock().await;
        worker.notify();
        worker.notify();
        drop(busy);
        tokio::time::timeout(Duration::from_millis(100), worker.wake.notified())
            .await
            .unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(10), worker.wake.notified())
                .await
                .is_err()
        );
        worker.notify();
        tokio::time::timeout(Duration::from_millis(100), worker.wake.notified())
            .await
            .unwrap();
    }
}
