//! A bounded, deduplicated queue. Waiting warmers never acquire foreground
//! intent; interrupted builds keep their lease until all owned I/O has ended.
use super::bases::Bases;
use crate::{
    error::{Error, Result},
    project_archives::Archive,
};
use serde_json::Value;
use std::{
    collections::{HashMap, VecDeque},
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
struct Job {
    archives: Vec<Archive>,
    disk_mib: u64,
    key: String,
}

struct Failure {
    attempts: u32,
    retry_at: Instant,
}

#[derive(Default)]
struct Queue {
    jobs: VecDeque<Job>,
    running: bool,
    failures: HashMap<String, Failure>,
}

#[derive(Default)]
pub(crate) struct Warmer {
    queue: Mutex<Queue>,
}

impl Warmer {
    pub async fn wait_idle(&self) {
        while self.queue.lock().await.running {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    pub async fn enqueue(
        self: &Arc<Self>,
        bases: Bases,
        value: Value,
        budget: Arc<Mutex<Value>>,
        stop: CancellationToken,
    ) -> Result<()> {
        let archives: Vec<Archive> = serde_json::from_value(value["archives"].clone())?;
        let disk_mib = value["diskMiB"]
            .as_u64()
            .ok_or_else(|| Error::bad("Missing native base size."))?;
        let groups = if value["individual"] == true {
            archives.into_iter().map(|archive| vec![archive]).collect()
        } else {
            vec![archives]
        };
        let mut queue = self.queue.lock().await;
        for archives in groups {
            if !Bases::eligible(&archives, disk_mib) {
                continue;
            }
            let key = bases.key(&archives, disk_mib)?;
            if value["individual"] != true {
                queue.failures.remove(&key);
            }
            if queue
                .failures
                .get(&key)
                .is_some_and(|failure| failure.retry_at > Instant::now())
            {
                continue;
            }
            if queue.jobs.iter().any(|job| job.key == key) {
                continue;
            }
            if queue.jobs.len() == 64 {
                queue.jobs.pop_front();
            }
            queue.jobs.push_back(Job {
                archives,
                disk_mib,
                key,
            });
        }
        if queue.running {
            return Ok(());
        }
        queue.running = true;
        let warmer = self.clone();
        tokio::spawn(async move {
            warmer.work(bases, budget, stop).await;
        });
        Ok(())
    }

    async fn work(
        self: Arc<Self>,
        bases: Bases,
        budget: Arc<Mutex<Value>>,
        stop: CancellationToken,
    ) {
        loop {
            let job = {
                let mut queue = self.queue.lock().await;
                if stop.is_cancelled() {
                    queue.jobs.clear();
                }
                let Some(job) = queue.jobs.pop_front() else {
                    queue.running = false;
                    return;
                };
                job
            };
            let limits = budget.lock().await;
            let limit = limits["limits"]["diskMiB"]
                .as_u64()
                .unwrap_or(0)
                .saturating_mul(super::disk::MIB);
            if job.disk_mib.saturating_mul(super::disk::MIB) > limit || stop.is_cancelled() {
                continue;
            }
            let priority = match crate::file_lock::exclusive(
                &bases.state.join("storage-foreground.lock"),
                "Foreground storage is waiting.",
            ) {
                Ok(probe) => probe,
                Err(error) if error.status == 409 => {
                    drop(limits);
                    self.retry(job).await;
                    continue;
                }
                Err(_) => continue,
            };
            let owner = match crate::file_lock::exclusive(
                &bases.state.join("storage.lock"),
                "Native storage is busy.",
            ) {
                Ok(owner) => owner,
                Err(error) if error.status == 409 => {
                    drop(priority);
                    drop(limits);
                    self.retry(job).await;
                    continue;
                }
                Err(_) => continue,
            };
            drop(priority);
            drop(limits);
            let build_stop = stop.child_token();
            let cancellation = build_stop.clone().drop_guard();
            let (watched_state, watched_stop) = (bases.state.clone(), build_stop.clone());
            let watcher = tokio::spawn(async move {
                tokio::select! {
                    () = watched_stop.cancelled() => {},
                    () = crate::project_archives::foreground_waiter(&watched_state) => { watched_stop.cancel(); },
                }
            });
            let (worker, work, worker_stop) = (bases.clone(), job.clone(), build_stop.clone());
            let handle = tokio::runtime::Handle::current();
            let result = tokio::task::spawn_blocking(move || {
                let _owner = owner;
                handle.block_on(async {
                    let policy =
                        match tokio::fs::read(worker.state.join("storage-policy.json")).await {
                            Ok(bytes) => serde_json::from_slice(&bytes)?,
                            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                                crate::storage::policy::Policy::default()
                            }
                            Err(error) => return Err(Error::from(error)),
                        };
                    worker
                        .warm(&work.archives, work.disk_mib, limit, &policy, &worker_stop)
                        .await
                })
            })
            .await;
            watcher.abort();
            let _ = watcher.await;
            if build_stop.is_cancelled() && !stop.is_cancelled() {
                self.retry(job).await;
            } else if !matches!(result, Ok(Ok(()))) {
                let mut queue = self.queue.lock().await;
                if queue.failures.len() >= 128 && !queue.failures.contains_key(&job.key) {
                    let oldest = queue
                        .failures
                        .iter()
                        .min_by_key(|(_, failure)| failure.retry_at)
                        .map(|(key, _)| key.clone());
                    if let Some(key) = oldest {
                        queue.failures.remove(&key);
                    }
                }
                let failure = queue.failures.entry(job.key.clone()).or_insert(Failure {
                    attempts: 0,
                    retry_at: Instant::now(),
                });
                failure.attempts = failure.attempts.saturating_add(1).min(6);
                let seconds = if failure.attempts >= 6 {
                    3600
                } else {
                    10u64 << (failure.attempts - 1)
                };
                failure.retry_at = Instant::now() + Duration::from_secs(seconds);
                tracing::debug!(key = %job.key, retry_seconds = seconds, "Native base warming deferred");
            } else {
                self.queue.lock().await.failures.remove(&job.key);
            }
            drop(cancellation);
        }
    }

    async fn retry(&self, job: Job) {
        {
            let mut queue = self.queue.lock().await;
            if !queue.jobs.iter().any(|entry| entry.key == job.key) && queue.jobs.len() < 64 {
                queue.jobs.push_back(job);
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}
