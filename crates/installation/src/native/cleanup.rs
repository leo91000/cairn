//! Retained disk cleanup is independent of request, Tokio and node lifetimes.
use crate::{
    error::{Error, Result},
    validation::uuid,
};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, VecDeque},
    io::Write,
    os::unix::fs::{DirBuilderExt, MetadataExt},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::watch;

const MAX_WORKERS: usize = 128;
pub(super) const WAIT: Duration = Duration::from_secs(20);

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Record {
    version: u8,
    pub run: String,
    pub attempt: String,
    boot: String,
    #[serde(default)]
    message: String,
}

#[derive(Clone, Debug)]
pub(super) enum Outcome {
    Pending(String),
    Complete,
}

#[derive(Clone)]
pub(super) struct Ticket {
    attempt: String,
    outcome: watch::Receiver<Outcome>,
}

impl Ticket {
    pub fn current(&self) -> Outcome {
        self.outcome.borrow().clone()
    }

    pub async fn wait(&mut self, duration: Duration) -> Outcome {
        let waiting = async {
            loop {
                let current = self.outcome.borrow_and_update().clone();
                if matches!(current, Outcome::Complete) {
                    return current;
                }
                if self.outcome.changed().await.is_err() {
                    return self.outcome.borrow().clone();
                }
            }
        };
        tokio::time::timeout(duration, waiting)
            .await
            .unwrap_or_else(|_| self.outcome.borrow().clone())
    }
}

#[async_trait]
pub(super) trait Work: Send + 'static {
    /// Errors retain this value and every lease it owns for a later retry.
    async fn run(&mut self) -> Result<()>;

    async fn pending(&mut self, _message: &str) -> Result<()> {
        Ok(())
    }
}

// An early registry/worker failure cannot release the physical ownership it
// was handed. Recovery after controller exit reconciles the durable registry.
struct Retained<W>(Option<W>);

impl<W> Drop for Retained<W> {
    fn drop(&mut self) {
        if let Some(work) = self.0.take() {
            std::mem::forget(work);
        }
    }
}

#[derive(Clone, Default)]
pub(super) struct Registry {
    workers: Arc<Mutex<Workers>>,
    tickets: Arc<Mutex<HashMap<String, Ticket>>>,
}

#[derive(Default)]
struct Workers {
    queued: VecDeque<Job>,
    running: usize,
}

struct Job {
    root: PathBuf,
    path: PathBuf,
    record: Record,
    sender: watch::Sender<Outcome>,
    work: Retained<Box<dyn Work>>,
}

fn directory(state: &Path) -> PathBuf {
    state.join("native-cleanup")
}

fn marker(state: &Path, run: &str) -> PathBuf {
    directory(state).join(format!("{run}.json"))
}

pub(super) fn check(state: &Path, run: &str) -> Result<()> {
    uuid(run)?;
    match std::fs::symlink_metadata(marker(state, run)) {
        Ok(metadata) => {
            if !metadata.is_file() || metadata.len() > 16384 {
                return Err(Error::conflict("Invalid native cleanup record."));
            }
            let detail = std::fs::read(marker(state, run))
                .ok()
                .and_then(|bytes| serde_json::from_slice::<Record>(&bytes).ok())
                .filter(|record| record.run == run && record.message.len() <= 8192)
                .map(|record| record.message)
                .unwrap_or_default();
            Err(Error::unavailable(format!(
                "This conversation's native disk remains quarantined: {detail}"
            )))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

pub(super) fn records(state: &Path) -> Result<Vec<Record>> {
    let entries = match std::fs::read_dir(directory(state)) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let mut records = Vec::new();
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        // Interrupted atomic writes have no authority. Only committed UUID
        // receipts participate in admission and recovery.
        if path.extension().is_none_or(|extension| extension != "json") {
            continue;
        }
        if !entry.file_type()?.is_file() || entry.metadata()?.len() > 16384 {
            return Err(Error::conflict("Invalid native cleanup record."));
        }
        let record: Record = serde_json::from_slice(&std::fs::read(entry.path())?)?;
        uuid(&record.run)?;
        uuid(&record.attempt)?;
        uuid(&record.boot)?;
        if record.message.len() > 8192 {
            return Err(Error::conflict("Invalid native cleanup diagnostic."));
        }
        if record.version != 1 || entry.path() != marker(state, &record.run) {
            return Err(Error::conflict("Invalid native cleanup identity."));
        }
        records.push(record);
    }
    Ok(records)
}

fn publish(state: &Path, run: &str, attempt: &str) -> Result<Record> {
    uuid(run)?;
    uuid(attempt)?;
    let root = directory(state);
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&root)?;
    if !std::fs::symlink_metadata(&root)?.is_dir() {
        return Err(Error::conflict("Invalid native cleanup directory."));
    }
    let path = marker(state, run);
    if path.try_exists()? {
        let metadata = std::fs::symlink_metadata(&path)?;
        if !metadata.is_file() || metadata.len() > 16384 {
            return Err(Error::conflict("Invalid native cleanup receipt."));
        }
        let record: Record = serde_json::from_slice(&std::fs::read(&path)?)?;
        if record.version != 1 || record.run != run || record.attempt != attempt {
            return Err(Error::conflict(
                "Native cleanup belongs to another attempt.",
            ));
        }
        return Ok(record);
    }
    let record = Record {
        version: 1,
        run: run.into(),
        attempt: attempt.into(),
        message: "Native disk cleanup is still in progress; ownership retained.".into(),
        boot: std::fs::read_to_string("/proc/sys/kernel/random/boot_id")?
            .trim()
            .into(),
    };
    let mut file = tempfile::NamedTempFile::new_in(&root)?;
    file.write_all(&serde_json::to_vec(&record)?)?;
    file.as_file().sync_all()?;
    file.persist_noclobber(&path)
        .map_err(|error| Error::from(error.error))?;
    std::fs::File::open(&root)?.sync_all()?;
    Ok(record)
}

pub(super) fn quarantine(state: &Path, run: &str, attempt: &str) -> Result<()> {
    publish(state, run, attempt).map(|_| ())
}

pub(super) async fn read_attempt_evidence(
    path: &Path,
    limit: usize,
    allow_unlinked_snapshot: bool,
) -> std::io::Result<Option<Vec<u8>>> {
    let file = match tokio::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .await
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    read_evidence(file, limit, allow_unlinked_snapshot)
        .await
        .map(Some)
}

async fn read_evidence(
    file: tokio::fs::File,
    limit: usize,
    allow_unlinked_snapshot: bool,
) -> std::io::Result<Vec<u8>> {
    let metadata = file.metadata().await?;
    let links = metadata.nlink();
    if !metadata.is_file()
        || metadata.len() > limit as u64
        || links > 1
        || (links == 0 && !allow_unlinked_snapshot)
    {
        return Err(std::io::Error::other("Invalid native cleanup evidence."));
    }
    crate::process::read_bounded(file, limit)
        .await
        .map_err(std::io::Error::other)
}

async fn quarantine_retired(path: &Path, run: &str, attempt: Option<&str>) -> bool {
    let Some(attempt) = attempt else {
        return matches!(
            tokio::fs::symlink_metadata(path).await,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound
        );
    };
    // Reading an opened quarantine snapshot is safe across atomic replacement
    // or unlink. The ticket check still observes a same-attempt worker.
    let bytes = match read_attempt_evidence(path, 16384, true).await {
        Ok(None) => return true,
        Ok(Some(bytes)) => bytes,
        Err(_) => return false,
    };
    // A later attempt's quarantine cannot undo this attempt's retirement.
    serde_json::from_slice::<Record>(&bytes).is_ok_and(|record| {
        record.version == 1
            && record.run == run
            && uuid(&record.attempt).is_ok()
            && uuid(&record.boot).is_ok()
            && record.message.len() <= 8192
            && record.attempt != attempt
    })
}

impl Registry {
    pub(super) async fn retired(&self, state: &Path, run: &str, attempt: Option<&str>) -> bool {
        if !tokio::fs::symlink_metadata(directory(state))
            .await
            .is_ok_and(|metadata| metadata.is_dir())
            || !quarantine_retired(&marker(state, run), run, attempt).await
        {
            return false;
        }

        // A live worker unlinks quarantine before syncing the directory and
        // dropping its leases. Observe its completion after checking the path.
        self.tickets.lock().is_ok_and(|tickets| {
            tickets.get(run).is_none_or(|ticket| {
                attempt.is_some_and(|attempt| ticket.attempt != attempt)
                    || matches!(*ticket.outcome.borrow(), Outcome::Complete)
            })
        })
    }

    pub fn start(&self, state: &Path, run: &str, attempt: &str, work: impl Work) -> Result<Ticket> {
        uuid(run)?;
        uuid(attempt)?;
        let work = Retained(Some(Box::new(work) as Box<dyn Work>));
        let mut workers = self.workers.lock().map_err(Error::internal)?;
        let (sender, receiver) = watch::channel(Outcome::Pending(
            "Native disk cleanup registration is in progress.".into(),
        ));
        let ticket = Ticket {
            attempt: attempt.into(),
            outcome: receiver,
        };
        {
            let mut tickets = self.tickets.lock().map_err(Error::internal)?;
            if let Some(ticket) = tickets.get(run)
                && !matches!(*ticket.outcome.borrow(), Outcome::Complete)
            {
                return Err(Error::conflict(
                    "This native disk already has a cleanup worker; retained ownership cannot be replaced.",
                ));
            }
            tickets.retain(|_, ticket| !matches!(*ticket.outcome.borrow(), Outcome::Complete));
            tickets.insert(run.into(), ticket.clone());
        }

        // Retirement checks must see this registration without waiting for
        // its filesystem writes or other runs' worker dispatch.
        // A publication error retains the work's leases, so its ticket also
        // remains pending until controller restart.
        let record = publish(state, run, attempt)?;
        sender.send_replace(Outcome::Pending(record.message.clone()));
        let root = directory(state);
        let path = marker(state, run);
        workers.queued.push_back(Job {
            root,
            path,
            record,
            sender,
            work,
        });
        drop(workers);
        self.dispatch();
        Ok(ticket)
    }

    fn dispatch(&self) {
        let Ok(mut workers) = self.workers.lock() else {
            return;
        };
        while workers.running < MAX_WORKERS {
            let Some(job) = workers.queued.pop_front() else {
                break;
            };
            workers.running += 1;
            let registry = self.clone();
            // Each active disk has an independent runtime. Queued work remains
            // owned and durably quarantined even when every worker is blocked.
            let spawned = std::thread::Builder::new()
                .name("native-disk-cleanup".into())
                .spawn(move || {
                crate::storage::runtime::isolated_native_io();
                let Job {
                    root,
                    path,
                    mut record,
                    sender,
                    mut work,
                } = job;
                let runtime = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(error) => {
                        sender.send_replace(Outcome::Pending(error.to_string()));
                        if let Ok(mut workers) = registry.workers.lock() {
                            workers.running -= 1;
                        }
                        registry.dispatch();
                        return;
                    }
                };
                runtime.block_on(async move {
                    loop {
                        match work.0.as_mut().unwrap().run().await {
                            Ok(()) => {
                                let retired = match std::fs::remove_file(&path) {
                                    Ok(()) => Ok(()),
                                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                                    Err(error) => Err(error),
                                }.and_then(|()| std::fs::File::open(&root)?.sync_all());
                                if retired.is_ok() {
                                    drop(work.0.take());
                                    sender.send_replace(Outcome::Complete);
                                    break;
                                }
                                sender.send_replace(Outcome::Pending(
                                    "Native cleanup completed but its durable receipt could not be cleared.".into(),
                                ));
                            }
                            Err(error) => {
                                let message: String = error.message.chars()
                                    .filter(|c| !c.is_control()).take(2048).collect();
                                tracing::error!(run = record.run, attempt = record.attempt, %message,
                                    "Native disk cleanup remains isolated");
                                if let Err(error) = work.0.as_mut().unwrap().pending(&message).await {
                                    tracing::error!(%error, "Could not publish native recovery diagnostic");
                                }
                                record.message = message.clone();
                                if let Ok(bytes) = serde_json::to_vec(&record) {
                                    let _ = super::atomic_write(&path, &bytes).await;
                                }
                                sender.send_replace(Outcome::Pending(message));
                            }
                        }
                        tokio::time::sleep(Duration::from_secs(2)).await;
                    }
                });
                // Physical retirement is complete. A journal maintenance
                // retry must not reserve a physical cleanup worker slot.
                if let Ok(mut workers) = registry.workers.lock() {
                    workers.running -= 1;
                }
                registry.dispatch();
                runtime.block_on(async {
                    while runtime.metrics().num_alive_tasks() > 0 {
                        tokio::time::sleep(Duration::from_millis(100)).await;
                    }
                });
            });
            if let Err(error) = spawned {
                // The closure drops a Retained value; its physical ownership
                // survives. Its durable marker also survives for restart.
                tracing::error!(%error, "Native cleanup worker could not start; disk remains quarantined");
                workers.running -= 1;
            }
        }
    }
}

/// Own native mount preparation on a private runtime. If its observer leaves,
/// close a late mount here while its physical leases remain held.
pub(super) fn prepare(
    owner: Option<Arc<crate::file_lock::Guard>>,
    run: impl FnOnce() -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<super::directory::Mounted>> + Send>,
    > + Send
    + 'static,
) -> Result<tokio::sync::oneshot::Receiver<Result<super::directory::Mounted>>> {
    let (send, receive) = tokio::sync::oneshot::channel();
    std::thread::Builder::new()
        .name("native-disk-prepare".into())
        .spawn(move || {
            crate::storage::runtime::isolated_native_io();
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build();
            let Ok(runtime) = runtime else {
                return;
            };
            runtime.block_on(async move {
                let result = run().await;
                if let Err(Ok(mut mounted)) = send.send(result) {
                    loop {
                        match mounted.close().await {
                            Ok(_) => break,
                            Err(error) => {
                                tracing::warn!(%error, "Late native preparation cleanup deferred");
                            }
                        }
                        tokio::time::sleep(Duration::from_secs(2)).await;
                    }
                }
                // The directory has no journal-maintenance task to keep alive.
                // The mount retains physical ownership after a successful handoff.
                drop(owner);
            });
        })
        .map_err(Error::internal)?;
    Ok(receive)
}

/// Drive run-local filesystem work on an owned runtime whose blocking tasks
/// cannot hold the controller runtime open during shutdown.
pub(super) fn spawn<T: Send + 'static>(
    work: impl std::future::Future<Output = T> + Send + 'static,
) -> Result<tokio::sync::oneshot::Receiver<T>> {
    let (send, receive) = tokio::sync::oneshot::channel();
    std::thread::Builder::new()
        .name("native-run-io".into())
        .spawn(move || {
            crate::storage::runtime::isolated_native_io();
            let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            else {
                return;
            };
            let result = runtime.block_on(work);
            let _ = send.send(result);
            runtime.block_on(async {
                while runtime.metrics().num_alive_tasks() > 0 {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            });
        })
        .map_err(Error::internal)?;
    Ok(receive)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        mpsc,
    };

    struct Blocking {
        _owner: crate::file_lock::Guard,
        entered: mpsc::Sender<()>,
        release: mpsc::Receiver<()>,
        calls: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl Work for Blocking {
        async fn run(&mut self) -> Result<()> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.entered.send(()).unwrap();
            self.release.recv().unwrap();
            Ok(())
        }
    }

    #[test]
    fn inventory_ignores_incomplete_atomic_writes_and_preserves_attempt_identity() {
        let state = tempfile::tempdir().unwrap();
        let run = crate::config::id();
        let attempt = crate::config::id();
        quarantine(state.path(), &run, &attempt).unwrap();
        std::fs::write(
            directory(state.path()).join(".tmp-interrupted"),
            b"{partial",
        )
        .unwrap();
        assert_eq!(records(state.path()).unwrap().len(), 1);
        assert_eq!(records(state.path()).unwrap()[0].attempt, attempt);
        assert!(quarantine(state.path(), &run, &crate::config::id()).is_err());
        assert_eq!(records(state.path()).unwrap()[0].attempt, attempt);
    }

    #[tokio::test]
    async fn cancelled_observer_does_not_join_or_release_stalled_native_operation() {
        let (entered, receive) = std::sync::mpsc::channel();
        let (release, blocked) = std::sync::mpsc::channel();
        let mut observer = spawn(async move {
            entered.send(()).unwrap();
            tokio::task::spawn_blocking(move || blocked.recv().unwrap())
                .await
                .unwrap();
        })
        .unwrap();
        receive.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(20), &mut observer)
                .await
                .is_err()
        );
        release.send(()).unwrap();
        observer.await.unwrap();
    }

    #[tokio::test]
    async fn queued_cleanup_retains_ownership_and_is_admitted_after_capacity_returns() {
        let state = tempfile::tempdir().unwrap();
        let registry = Registry::default();
        registry.workers.lock().unwrap().running = MAX_WORKERS;
        let run = crate::config::id();
        let lock = state.path().join("physical.lock");
        let (entered, received) = mpsc::channel();
        let (release, released) = mpsc::channel();
        let mut ticket = registry
            .start(
                state.path(),
                &run,
                &crate::config::id(),
                Blocking {
                    _owner: crate::file_lock::exclusive(&lock, "busy").unwrap(),
                    entered,
                    release: released,
                    calls: Arc::new(AtomicUsize::new(0)),
                },
            )
            .unwrap();
        assert!(received.try_recv().is_err());
        assert!(check(state.path(), &run).is_err());
        assert!(crate::file_lock::exclusive(&lock, "busy").is_err());
        registry.workers.lock().unwrap().running -= 1;
        registry.dispatch();
        received.recv_timeout(Duration::from_secs(3)).unwrap();
        release.send(()).unwrap();
        assert!(matches!(
            ticket.wait(Duration::from_secs(3)).await,
            Outcome::Complete
        ));
        assert!(crate::file_lock::exclusive(&lock, "busy").is_ok());
    }

    #[tokio::test]
    async fn completed_cleanup_releases_worker_capacity_before_background_maintenance() {
        struct Maintenance(Option<tokio::sync::oneshot::Receiver<()>>);
        #[async_trait]
        impl Work for Maintenance {
            async fn run(&mut self) -> Result<()> {
                if let Some(release) = self.0.take() {
                    tokio::spawn(async move {
                        let _ = release.await;
                    });
                }
                Ok(())
            }
        }
        let state = tempfile::tempdir().unwrap();
        let registry = Registry::default();
        registry.workers.lock().unwrap().running = MAX_WORKERS - 1;
        let (release, wait) = tokio::sync::oneshot::channel();
        let mut first = registry
            .start(
                state.path(),
                &crate::config::id(),
                &crate::config::id(),
                Maintenance(Some(wait)),
            )
            .unwrap();
        assert!(matches!(
            first.wait(Duration::from_secs(3)).await,
            Outcome::Complete
        ));
        let mut next = registry
            .start(
                state.path(),
                &crate::config::id(),
                &crate::config::id(),
                Maintenance(None),
            )
            .unwrap();
        let next = next.wait(Duration::from_secs(3)).await;
        let _ = release.send(());
        assert!(matches!(next, Outcome::Complete));
    }

    #[tokio::test]
    async fn quarantine_snapshot_survives_unlink_with_the_descriptor_open() {
        let state = tempfile::tempdir().unwrap();
        let run = crate::config::id();
        let attempt = crate::config::id();
        quarantine(state.path(), &run, &attempt).unwrap();
        let path = marker(state.path(), &run);
        let alias = path.with_extension("alias");
        tokio::fs::hard_link(&path, &alias).await.unwrap();
        assert!(read_attempt_evidence(&path, 16384, true).await.is_err());
        tokio::fs::remove_file(alias).await.unwrap();

        let snapshot = tokio::fs::File::open(&path).await.unwrap();
        let strict = tokio::fs::File::open(&path).await.unwrap();
        tokio::fs::remove_file(&path).await.unwrap();
        let bytes = read_evidence(snapshot, 16384, true).await.unwrap();
        let record = serde_json::from_slice::<Record>(&bytes).unwrap();
        assert_eq!(record.attempt, attempt);
        assert!(read_evidence(strict, 16384, false).await.is_err());
    }

    #[tokio::test]
    async fn a_missing_quarantine_does_not_override_a_live_cleanup_worker() {
        let state = tempfile::tempdir().unwrap();
        let registry = Registry::default();
        let run = crate::config::id();
        let attempt = crate::config::id();
        let other = crate::config::id();
        let lock = state.path().join("physical.lock");
        let (entered, received) = mpsc::channel();
        let (release, released) = mpsc::channel();
        let mut ticket = registry
            .start(
                state.path(),
                &run,
                &attempt,
                Blocking {
                    _owner: crate::file_lock::exclusive(&lock, "busy").unwrap(),
                    entered,
                    release: released,
                    calls: Arc::new(AtomicUsize::new(0)),
                },
            )
            .unwrap();
        received.recv_timeout(Duration::from_secs(3)).unwrap();
        // Reproduce the observable state between quarantine unlink and lease release.
        std::fs::remove_file(marker(state.path(), &run)).unwrap();
        assert!(crate::file_lock::exclusive(&lock, "busy").is_err());
        assert!(!registry.retired(state.path(), &run, None).await);
        assert!(!registry.retired(state.path(), &run, Some(&attempt)).await);
        assert!(registry.retired(state.path(), &run, Some(&other)).await);
        release.send(()).unwrap();
        assert!(matches!(
            ticket.wait(Duration::from_secs(3)).await,
            Outcome::Complete
        ));
        assert!(crate::file_lock::exclusive(&lock, "busy").is_ok());
    }

    #[tokio::test]
    async fn cleanup_registration_cannot_hide_another_runs_retirement() {
        let state = tempfile::tempdir().unwrap();
        let registry = Registry::default();
        let run = crate::config::id();
        std::fs::create_dir(directory(state.path())).unwrap();
        let (entered, received) = tokio::sync::oneshot::channel();
        let (release, released) = mpsc::channel();
        let registering = registry.clone();
        let registration = tokio::task::spawn_blocking(move || {
            let _workers = registering.workers.lock().unwrap();
            entered.send(()).unwrap();
            released.recv_timeout(Duration::from_secs(5)).unwrap();
        });
        received.await.unwrap();
        let retired = registry.retired(state.path(), &run, None).await;
        release.send(()).unwrap();
        registration.await.unwrap();
        assert!(retired);
    }

    #[tokio::test]
    async fn retirement_requires_an_intact_quarantine_directory() {
        let state = tempfile::tempdir().unwrap();
        let registry = Registry::default();
        let run = crate::config::id();
        let root = directory(state.path());
        assert!(!registry.retired(state.path(), &run, None).await);

        let target = state.path().join("different-directory");
        std::os::unix::fs::symlink(&target, &root).unwrap();
        assert!(!registry.retired(state.path(), &run, None).await);
        std::fs::create_dir(&target).unwrap();
        assert!(!registry.retired(state.path(), &run, None).await);

        std::fs::remove_file(&root).unwrap();
        std::fs::create_dir(&root).unwrap();
        assert!(registry.retired(state.path(), &run, None).await);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn blocked_cleanup_keeps_ownership_without_blocking_other_runs_or_request_runtime() {
        let state = tempfile::tempdir().unwrap();
        let registry = Registry::default();
        let run = crate::config::id();
        let attempt = crate::config::id();
        let lock = state.path().join("physical.lock");
        let owner = crate::file_lock::exclusive(&lock, "busy").unwrap();
        let (entered, received) = mpsc::channel();
        let (release, released) = mpsc::channel();
        let calls = Arc::new(AtomicUsize::new(0));
        let mut ticket = registry
            .start(
                state.path(),
                &run,
                &attempt,
                Blocking {
                    _owner: owner,
                    entered,
                    release: released,
                    calls: calls.clone(),
                },
            )
            .unwrap();
        received.recv_timeout(Duration::from_secs(3)).unwrap();
        assert!(matches!(
            ticket.wait(Duration::from_millis(20)).await,
            Outcome::Pending(_)
        ));
        assert!(crate::file_lock::exclusive(&lock, "busy").is_err());
        assert!(check(state.path(), &run).is_err());
        assert!(check(state.path(), &crate::config::id()).is_ok());
        assert_eq!(records(state.path()).unwrap()[0].attempt, attempt);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        drop(registry);
        tokio::time::sleep(Duration::from_millis(1)).await;
        assert!(crate::file_lock::exclusive(&lock, "busy").is_err());
        release.send(()).unwrap();
        assert!(matches!(
            ticket.wait(Duration::from_secs(3)).await,
            Outcome::Complete
        ));
        assert!(check(state.path(), &run).is_ok());
        assert!(crate::file_lock::exclusive(&lock, "busy").is_ok());
    }
}
