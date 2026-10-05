pub(crate) mod checkpoint;
mod control;
mod execution;
mod launch;
mod output;
mod schedule;

pub use control::routes;

use crate::{
    config::now,
    error::{Error, Result},
    run_status::RunStatus,
    service::Service,
    store::{Db, Store},
    validation::text,
};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{Arc, atomic::AtomicI64},
    time::Duration,
};
use tokio::sync::{Mutex, Notify, OnceCell, OwnedMutexGuard};
use tokio_util::{sync::CancellationToken, task::TaskTracker};

#[derive(Default)]
pub struct Worker {
    pub active: Mutex<HashMap<String, CancellationToken>>,
    tick_lock: Mutex<()>,
    preparation: Arc<Mutex<()>>,
    wake: Notify,
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
            .spawn(crate::nodes::publication::maintain(s.clone()));
        self.tasks
            .spawn(crate::nodes::shared_blocks::maintain(s.clone()));
        self.tasks.spawn(crate::nodes::storage::monitor(s.clone()));
        self.tasks.spawn(clean_conversations(s.clone()));
        self.tasks.spawn(self.clone().schedule_forever(s));
        Ok(())
    }

    async fn schedule_forever(self: Arc<Self>, s: Arc<Service>) {
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

/// Requeues runs that were executing when the previous worker process stopped.
fn restore_after_restart(db: &mut Db<'_>) -> Result<()> {
    db.restore_chat_summaries()?;
    for run in db.active()? {
        let id = text(&run, "id");
        if run["status"] != RunStatus::Running {
            if !run["cancelRequestedAt"].is_null() {
                db.patch_run(id, &json!({ "recoveryPending": true }))?;
            }
            continue;
        }
        if db.kv(&checkpoint::key(id))?.is_none() && !run["startedAt"].is_null() {
            let patch = json!({
                "status": RunStatus::Interrupted,
                "finishedAt": now(),
                "summary": "This older run has no restart checkpoint. Review its working files before retrying.",
            });
            db.patch_run(id, &patch)?;
            continue;
        }
        let patch = json!({
            "status": RunStatus::Queued,
            "recoveryPending": true,
            "finishedAt": null,
            "capacityWaitUntil": null,
            "accountWaitReason": "Recovering after worker restart.",
        });
        db.patch_run(id, &patch)?;
        db.event(id, "status", "Recovering after worker restart", None)?;
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

/// Records an audit entry for a failure the worker already handled.
async fn audit(store: &Store, action: &str, detail: Value) {
    if let Err(error) = store.audit(action, detail).await {
        tracing::warn!(action, error = %error, "Could not record worker audit entry");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
            gh_bin: "unused".into(),
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
            .task(
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
