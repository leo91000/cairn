//! One CPU decision for storage protection and exclusive reservations.
use super::{atomic_write, systemd};
use crate::{
    error::{Error, Result},
    microvm::suspension::Reason,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio::sync::Mutex;

#[async_trait::async_trait]
pub(super) trait Cpu: Send + Sync {
    async fn set_paused(&self, attempt: &str, paused: bool) -> Result<()>;
}

pub(super) struct Systemd;

#[async_trait::async_trait]
impl Cpu for Systemd {
    async fn set_paused(&self, attempt: &str, paused: bool) -> Result<()> {
        systemd::set_paused(attempt, paused).await
    }
}

#[derive(Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct Record {
    ready: bool,
    finished: bool,
    reasons: BTreeSet<Reason>,
    confirmed: Option<bool>,
    administrative_since: Option<u64>,
    administrative_paused_ms: u64,
}

impl Record {
    fn administrative(&self) -> bool {
        self.reasons
            .iter()
            .any(|reason| matches!(reason, Reason::Exclusive { .. }))
    }

    fn account(&mut self) {
        if self.ready && self.confirmed == Some(true) && self.administrative() {
            self.administrative_since
                .get_or_insert(crate::nodes::boot_ms());
        } else if let Some(since) = self.administrative_since.take() {
            self.administrative_paused_ms = self
                .administrative_paused_ms
                .saturating_add(crate::nodes::boot_ms().saturating_sub(since));
        }
    }

    fn elapsed(&self) -> u64 {
        self.administrative_paused_ms.saturating_add(
            self.administrative_since
                .map_or(0, |since| crate::nodes::boot_ms().saturating_sub(since)),
        )
    }
}

pub(super) struct Suspension {
    attempt: String,
    path: PathBuf,
    record: Mutex<Record>,
    cpu: Arc<dyn Cpu>,
}

impl Suspension {
    pub fn new(state: &Path, attempt: &str) -> Self {
        Self {
            attempt: attempt.into(),
            path: state.join(format!("{attempt}.native-suspension.json")),
            record: Mutex::default(),
            cpu: Arc::new(Systemd),
        }
    }

    async fn save(&self, record: &Record) -> Result<()> {
        atomic_write(&self.path, &serde_json::to_vec(record)?).await
    }

    async fn apply(&self, record: &mut Record) -> Result<bool> {
        let paused = !record.reasons.is_empty();
        if record.ready && record.confirmed != Some(paused) {
            // Unknown or lost acknowledgement must be retried, never credited.
            record.confirmed = None;
            record.account();
            self.save(record).await?;
            self.cpu.set_paused(&self.attempt, paused).await?;
            record.confirmed = Some(paused);
        }
        record.account();
        self.save(record).await?;
        Ok(record.finished
            || (!record.ready && !paused)
            || (record.ready && record.confirmed == Some(paused)))
    }

    pub async fn set_reason(&self, reason: &Reason, enabled: bool) -> Result<bool> {
        let mut record = self.record.lock().await;
        if record.finished {
            return Ok(true);
        }
        if enabled {
            record.reasons.insert(reason.clone());
        } else {
            record.reasons.remove(reason);
        }
        self.apply(&mut record).await
    }

    /// Called only after systemd reports the workload unit active.
    pub async fn bind(&self) -> Result<()> {
        let mut record = self.record.lock().await;
        if record.finished {
            return Err(Error::conflict("Native execution has stopped."));
        }
        record.ready = true;
        record.confirmed = Some(false);
        self.apply(&mut record).await?;
        Ok(())
    }

    /// Must follow physical cgroup fencing, including on controller recovery.
    pub async fn finish(&self) -> Result<()> {
        let mut record = self.record.lock().await;
        record.ready = false;
        record.finished = true;
        record.account();
        self.save(&record).await
    }
}

pub(super) async fn administrative_ms(state: &Path, attempt: &str) -> Result<u64> {
    let path = state.join(format!("{attempt}.native-suspension.json"));
    match tokio::fs::read(path).await {
        Ok(bytes) => Ok(serde_json::from_slice::<Record>(&bytes)?.elapsed()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(0),
        Err(error) => Err(error.into()),
    }
}

pub(super) async fn recover(state: &Path, attempt: &str) -> Result<()> {
    let pause = Suspension::new(state, attempt);
    match tokio::fs::read(&pause.path).await {
        Ok(bytes) => {
            *pause.record.lock().await = serde_json::from_slice(&bytes)?;
            pause.finish().await?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

#[cfg(test)]
#[derive(Default)]
pub(super) struct TestCpu {
    pub transitions: Mutex<Vec<bool>>,
    pub fail: std::sync::atomic::AtomicBool,
    pub fail_next: std::sync::atomic::AtomicBool,
}

#[cfg(test)]
#[async_trait::async_trait]
impl Cpu for TestCpu {
    async fn set_paused(&self, _attempt: &str, paused: bool) -> Result<()> {
        self.transitions.lock().await.push(paused);
        if self.fail.load(std::sync::atomic::Ordering::SeqCst)
            || self
                .fail_next
                .swap(false, std::sync::atomic::Ordering::SeqCst)
        {
            return Err(Error::timeout("Synthetic lost systemd acknowledgement."));
        }
        Ok(())
    }
}

#[cfg(test)]
impl Suspension {
    pub fn testing(state: &Path, attempt: &str, cpu: Arc<TestCpu>) -> Self {
        Self {
            cpu,
            ..Self::new(state, attempt)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;

    fn exclusive(epoch: u64) -> Reason {
        Reason::Exclusive {
            reservation_id: "reservation".into(),
            epoch,
        }
    }

    #[tokio::test]
    async fn storage_and_exclusive_owners_never_release_each_others_cpu_fence() {
        let root = tempfile::tempdir().unwrap();
        let cpu = Arc::new(TestCpu::default());
        let pause = Suspension::testing(root.path(), "attempt", cpu.clone());
        assert!(!pause.set_reason(&exclusive(1), true).await.unwrap());
        assert!(cpu.transitions.lock().await.is_empty());
        pause.bind().await.unwrap();
        pause.set_reason(&Reason::Storage, true).await.unwrap();
        pause.set_reason(&exclusive(2), true).await.unwrap();
        pause.set_reason(&exclusive(1), false).await.unwrap();
        pause.set_reason(&exclusive(2), false).await.unwrap();
        assert_eq!(*cpu.transitions.lock().await, vec![true]);
        pause.set_reason(&Reason::Storage, false).await.unwrap();
        assert_eq!(*cpu.transitions.lock().await, vec![true, false]);
        pause.set_reason(&Reason::Storage, true).await.unwrap();
        pause.set_reason(&exclusive(3), true).await.unwrap();
        pause.set_reason(&Reason::Storage, false).await.unwrap();
        assert_eq!(*cpu.transitions.lock().await, vec![true, false, true]);
        pause.set_reason(&exclusive(3), false).await.unwrap();
        assert_eq!(
            *cpu.transitions.lock().await,
            vec![true, false, true, false]
        );
    }

    #[tokio::test]
    async fn unconfirmed_freeze_is_not_credited_and_recovery_finalizes_accounting() {
        let root = tempfile::tempdir().unwrap();
        let cpu = Arc::new(TestCpu::default());
        let pause = Suspension::testing(root.path(), "attempt", cpu.clone());
        pause.bind().await.unwrap();
        cpu.fail.store(true, Ordering::SeqCst);
        assert!(pause.set_reason(&exclusive(1), true).await.is_err());
        assert_eq!(administrative_ms(root.path(), "attempt").await.unwrap(), 0);
        cpu.fail.store(false, Ordering::SeqCst);
        assert!(pause.set_reason(&exclusive(1), true).await.unwrap());
        tokio::time::sleep(std::time::Duration::from_millis(15)).await;
        recover(root.path(), "attempt").await.unwrap();
        let finished = administrative_ms(root.path(), "attempt").await.unwrap();
        assert!(finished >= 15);
        tokio::time::sleep(std::time::Duration::from_millis(15)).await;
        assert_eq!(
            administrative_ms(root.path(), "attempt").await.unwrap(),
            finished
        );
    }

    #[tokio::test]
    async fn stopped_workload_cannot_be_thawed_by_a_late_release() {
        let root = tempfile::tempdir().unwrap();
        let cpu = Arc::new(TestCpu::default());
        let pause = Suspension::testing(root.path(), "attempt", cpu.clone());
        pause.bind().await.unwrap();
        pause.set_reason(&exclusive(1), true).await.unwrap();
        pause.finish().await.unwrap();
        pause.set_reason(&exclusive(1), false).await.unwrap();
        assert_eq!(*cpu.transitions.lock().await, vec![true]);
        assert!(pause.bind().await.is_err());
    }
}

#[cfg(test)]
#[path = "suspension_measurements.rs"]
mod measurements;
