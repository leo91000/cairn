//! Physical close phases retain the conversation's disk leases through retirement.
use crate::{error::Result, file_lock, storage::environment};

#[async_trait::async_trait]
pub(super) trait Operations: Send + Sync {
    async fn flush(&self) -> std::io::Result<Option<u64>>;

    async fn abort(&self) -> Result<()>;

    async fn unmount(&self) -> Result<()>;

    async fn record_usage(&self, used: u64) -> Result<()>;

    fn initialized(&self) -> bool;

    async fn seal(&self) -> Result<()>;

    async fn retire(&self) -> Result<()>;
}

pub(super) struct State {
    pub mounted: bool,
    pub completed: bool,
    pub aborted: bool,
    pub flush_failed: bool,
    pub retired: bool,
    retained_usage: Option<u64>,
    owner: Option<environment::OwnerLease>,
    physical: Option<file_lock::Guard>,
}

impl State {
    pub fn new(owner: environment::OwnerLease, physical: file_lock::Guard, aborted: bool) -> Self {
        Self {
            mounted: true,
            completed: false,
            aborted,
            flush_failed: false,
            retired: false,
            retained_usage: None,
            owner: Some(owner),
            physical: Some(physical),
        }
    }

    pub async fn close(&mut self, operations: &impl Operations) -> Result<super::Completion> {
        if self.mounted {
            if !self.aborted {
                match operations.flush().await {
                    Ok(used) => self.retained_usage = used,
                    Err(error) if matches!(error.raw_os_error(), Some(libc::EROFS | libc::EIO)) => {
                        tracing::error!(%error, "Native filesystem flush failed; retaining a crash prefix after fencing");
                        self.flush_failed = true;
                        self.aborted = true;
                        operations.abort().await?;
                    }
                    Err(error) => return Err(error.into()),
                }
            }
            operations.unmount().await?;
            self.mounted = false;
        }
        if let Some(used) = self.retained_usage.take()
            && let Err(error) = operations.record_usage(used).await
        {
            tracing::warn!(%error, "Native retained capacity credit unavailable");
        }
        if !self.completed && operations.initialized() {
            operations.seal().await?;
            self.completed = true;
        }
        operations.retire().await?;
        self.retired = true;
        Ok(if self.flush_failed {
            super::Completion::FailedFlush
        } else if self.aborted {
            super::Completion::Aborted
        } else {
            super::Completion::Flushed
        })
    }
}

impl Drop for State {
    fn drop(&mut self) {
        if !self.retired {
            if let Some(owner) = self.owner.take() {
                std::mem::forget(owner);
            }
            if let Some(physical) = self.physical.take() {
                std::mem::forget(physical);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        native::{block::Completion, service::StorageClose, workspace},
        storage::{self, Disk},
    };
    use serde_json::json;
    use std::{
        future::{Future, poll_fn},
        path::Path,
        sync::{
            Arc,
            atomic::{AtomicBool, AtomicI32, Ordering},
        },
        task::Poll,
        time::Duration,
    };
    use tokio::sync::{Notify, Semaphore};

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Phase {
        Flush,
        Unmount,
        Seal,
        Retirement,
    }

    struct Pause {
        phase: Phase,
        entered: Notify,
        release: Semaphore,
    }

    struct Filesystem {
        pause: Option<Arc<Pause>>,
        volume: Arc<storage::runtime::Volume>,
        root: std::path::PathBuf,
        flush_error: AtomicI32,
        retire_error: AtomicBool,
    }

    impl Filesystem {
        async fn phase(&self, phase: Phase) {
            if let Some(pause) = &self.pause
                && pause.phase == phase
            {
                pause.entered.notify_one();
                pause.release.acquire().await.unwrap().forget();
            }
        }
    }

    #[async_trait::async_trait]
    impl Operations for Filesystem {
        async fn flush(&self) -> std::io::Result<Option<u64>> {
            self.phase(Phase::Flush).await;
            let error = self.flush_error.swap(0, Ordering::SeqCst);
            if error != 0 {
                return Err(std::io::Error::from_raw_os_error(error));
            }
            std::fs::File::open(self.root.join("payload"))?.sync_all()?;
            Ok(Some(9))
        }

        async fn abort(&self) -> Result<()> {
            self.volume.stop.cancel();
            Ok(())
        }

        async fn unmount(&self) -> Result<()> {
            self.phase(Phase::Unmount).await;
            Ok(())
        }

        async fn record_usage(&self, _: u64) -> Result<()> {
            Ok(())
        }

        fn initialized(&self) -> bool {
            true
        }

        async fn seal(&self) -> Result<()> {
            self.phase(Phase::Seal).await;
            self.volume.seal_completed().await?;
            Ok(())
        }

        async fn retire(&self) -> Result<()> {
            self.phase(Phase::Retirement).await;
            if self.retire_error.swap(false, Ordering::SeqCst) {
                return Err(crate::error::Error::unavailable(
                    "Retirement must be retried",
                ));
            }
            Ok(())
        }
    }

    async fn fixture(
        state: &Path,
        pause: Option<Arc<Pause>>,
    ) -> (String, State, Filesystem, file_lock::Guard) {
        let run = crate::config::id();
        let workspace_owner = workspace::execution_ownership(state, &run).await.unwrap();
        let owner = environment::ownership(state, &run, "fixture")
            .await
            .unwrap();
        let physical = file_lock::exclusive(&owner.directory.join("lock"), "fixture").unwrap();
        std::fs::write(owner.directory.join("payload"), b"preserved").unwrap();
        let policy = storage::policy::Policy {
            reserve_mi_b: 64,
            reserve_percent: 1,
            ..storage::policy::Policy::default()
        };
        drop(
            storage::runtime::create(
                &owner.directory.join("lazy"),
                &json!({
                    "version": 1,
                    "size": 4096,
                    "blockSize": 4194304,
                    "blocks": [{ "offset": 0, "size": 4096, "hash": null }]
                }),
                &json!({
                    "master": "http://127.0.0.1:1/",
                    "grant": "synthetic-close-fixture",
                    "policy": policy
                }),
            )
            .await
            .unwrap(),
        );
        let volume = storage::runtime::load(&owner.directory).await.unwrap();
        volume.write_at(0, b"preserved").unwrap();
        let filesystem = Filesystem {
            pause,
            volume,
            root: owner.directory.clone(),
            flush_error: AtomicI32::new(0),
            retire_error: AtomicBool::new(false),
        };
        (
            run,
            State::new(owner, physical, false),
            filesystem,
            workspace_owner,
        )
    }

    #[tokio::test]
    async fn unrelated_close_finishes_while_each_physical_phase_retains_its_owner() {
        for phase in [Phase::Flush, Phase::Unmount, Phase::Seal, Phase::Retirement] {
            let root = tempfile::tempdir().unwrap();
            let state = root.path().to_owned();
            let pause = Arc::new(Pause {
                phase,
                entered: Notify::new(),
                release: Semaphore::new(0),
            });
            let (run, mut closing, operations, workspace_owner) =
                fixture(&state, Some(pause.clone())).await;
            let volume = operations.volume.clone();
            let terminal = Arc::new(AtomicBool::new(false));
            let first = tokio::spawn({
                let terminal = terminal.clone();
                async move {
                    let _workspace_owner = workspace_owner;
                    let result = StorageClose::default()
                        .finish(closing.close(&operations))
                        .await;
                    terminal.store(true, Ordering::SeqCst);
                    result
                }
            });
            tokio::time::timeout(Duration::from_secs(2), pause.entered.notified())
                .await
                .unwrap();
            assert!(!terminal.load(Ordering::SeqCst));
            let replaced = crate::native::block::published(
                &state,
                &run,
                &json!({
                    "generation": 1,
                    "grantId": "wrong-disk",
                    "backupId": crate::config::id()
                }),
            )
            .await
            .unwrap_err();
            assert!(replaced.is_conflict());
            assert!(workspace::execution_ownership(&state, &run).await.is_err());
            assert!(environment::lock(&state, &run, "probe").await.is_err());
            assert!(
                crate::native::block::action(&state, &run, true)
                    .await
                    .is_err()
            );
            assert!(
                crate::native::block::evict(root.path(), &state, &run, &json!({}))
                    .await
                    .is_err()
            );
            let (_, mut second_state, second_operations, second_owner) =
                fixture(&state, None).await;
            let second_started = std::time::Instant::now();
            let second_entered = AtomicBool::new(false);
            let mut storage_close = StorageClose::default();
            let second = storage_close.finish(async {
                second_entered.store(true, Ordering::SeqCst);
                second_state.close(&second_operations).await
            });
            tokio::pin!(second);

            // An uncontended storage gate drains in one poll. Check admission
            // before awaiting real fsync/journal work, whose latency is not an SLA.
            let first_poll = poll_fn(|cx| Poll::Ready(second.as_mut().poll(cx))).await;
            let independently_admitted = second_entered.load(Ordering::SeqCst);
            if !independently_admitted {
                // Settle both fixtures even when a retained gate regresses.
                pause.release.add_permits(1);
            }
            let second = match first_poll {
                Poll::Ready(result) => result,
                Poll::Pending => second.await,
            };
            eprintln!(
                "close_sample phase={phase:?} independent_admission={independently_admitted} independent_elapsed_us={}",
                second_started.elapsed().as_micros()
            );
            if independently_admitted {
                assert!(!terminal.load(Ordering::SeqCst));
                pause.release.add_permits(1);
            }
            assert_eq!(first.await.unwrap().unwrap(), Completion::Flushed);
            drop(second_owner);
            assert!(
                independently_admitted,
                "{phase:?} retained the storage gate during physical close"
            );
            assert_eq!(second.unwrap(), Completion::Flushed);
            let mut bytes = [0; 9];
            volume.read_at(0, &mut bytes).unwrap();
            assert_eq!(&bytes, b"preserved");
            assert!(volume.disk.completed().unwrap().is_some());
            assert!(terminal.load(Ordering::SeqCst));
            assert!(environment::lock(&state, &run, "probe").await.is_ok());
        }
    }

    #[tokio::test]
    async fn cancelled_close_after_unmount_retains_its_physical_and_logical_leases() {
        for phase in [Phase::Seal, Phase::Retirement] {
            let root = tempfile::tempdir().unwrap();
            let pause = Arc::new(Pause {
                phase,
                entered: Notify::new(),
                release: Semaphore::new(0),
            });
            let (run, mut closing, operations, workspace_owner) =
                fixture(root.path(), Some(pause.clone())).await;
            let directory = operations.root.clone();
            let pending = tokio::spawn(async move { closing.close(&operations).await });
            tokio::time::timeout(Duration::from_secs(2), pause.entered.notified())
                .await
                .unwrap();
            pending.abort();
            assert!(pending.await.unwrap_err().is_cancelled());
            let logical_retained = environment::ownership(root.path(), &run, "probe")
                .await
                .is_err();
            let physical_retained = file_lock::exclusive(&directory.join("lock"), "probe").is_err();
            drop(workspace_owner);
            assert!(
                logical_retained,
                "cancellation released logical ownership before device retirement"
            );
            assert!(
                physical_retained,
                "cancellation released physical ownership before device retirement"
            );
        }
    }

    #[tokio::test]
    async fn failed_retirement_keeps_ownership_and_retries_the_same_completed_prefix() {
        let root = tempfile::tempdir().unwrap();
        let (run, mut closing, operations, workspace_owner) = fixture(root.path(), None).await;
        operations.retire_error.store(true, Ordering::SeqCst);
        assert!(closing.close(&operations).await.is_err());
        assert!(!closing.mounted);
        assert!(closing.completed);
        assert!(!closing.retired);
        let completed = operations.volume.disk.completed().unwrap();
        assert!(completed.is_some());
        assert!(environment::lock(root.path(), &run, "probe").await.is_err());
        assert_eq!(
            closing.close(&operations).await.unwrap(),
            Completion::Flushed
        );
        assert_eq!(operations.volume.disk.completed().unwrap(), completed);
        assert!(environment::lock(root.path(), &run, "probe").await.is_err());
        drop((closing, workspace_owner));
        assert!(environment::lock(root.path(), &run, "probe").await.is_ok());
    }

    #[tokio::test]
    async fn physical_retry_does_not_wait_for_an_unrelated_mutation_after_the_first_drain() {
        let root = tempfile::tempdir().unwrap();
        let (run, mut closing, operations, workspace_owner) = fixture(root.path(), None).await;
        operations.retire_error.store(true, Ordering::SeqCst);
        let mut storage_close = StorageClose::default();
        assert!(
            storage_close
                .finish(closing.close(&operations))
                .await
                .is_err()
        );
        assert!(environment::lock(root.path(), &run, "probe").await.is_err());
        let unrelated = crate::native::disk::lock(root.path()).await.unwrap();
        let retry = tokio::time::timeout(
            Duration::from_millis(250),
            storage_close.finish(closing.close(&operations)),
        )
        .await;
        drop(unrelated);
        // Always settle the fixture before asserting a failed independence check.
        if retry.is_err() {
            closing.close(&operations).await.unwrap();
        }
        drop(workspace_owner);
        assert!(
            retry.is_ok(),
            "physical retry reacquired an unrelated storage gate"
        );
        assert_eq!(retry.unwrap().unwrap(), Completion::Flushed);
    }

    #[tokio::test]
    async fn flush_errors_preserve_retry_or_failed_flush_evidence() {
        for errno in [libc::EIO, libc::EROFS, libc::EAGAIN] {
            let root = tempfile::tempdir().unwrap();
            let (run, mut closing, operations, _workspace_owner) = fixture(root.path(), None).await;
            operations.flush_error.store(errno, Ordering::SeqCst);
            let result = closing.close(&operations).await;
            assert!(environment::lock(root.path(), &run, "probe").await.is_err());
            if errno == libc::EAGAIN {
                assert!(result.is_err());
                assert!(closing.mounted);
                assert!(!closing.completed);
                assert!(!closing.retired);
                assert!(!operations.volume.stop.is_cancelled());
                assert_eq!(
                    closing.close(&operations).await.unwrap(),
                    Completion::Flushed
                );
                continue;
            }
            assert_eq!(result.unwrap(), Completion::FailedFlush);
            assert!(operations.volume.stop.is_cancelled());
            assert!(closing.completed);
            assert!(closing.retired);
        }
    }
}
