//! Durable identities awaiting manager-side corruption reconciliation. The disk's
//! existing context owns authentication; no URL, key or credential is copied here.
use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs::{File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Report {
    pub object: String,
    pub hash: String,
}

#[derive(Default)]
pub(super) struct Reports {
    file: Option<PathBuf>,
    pending: Mutex<BTreeMap<String, String>>,
    retained: AtomicBool,
    pub sending: tokio::sync::Mutex<()>,
}

impl Reports {
    pub fn open(directory: &Path) -> Result<Self> {
        let file = directory.join("direct-read-failures.json");
        let pending = match File::open(&file) {
            Ok(input) => {
                let mut bytes = Vec::new();
                input.take(16 * 1024 * 1024 + 1).read_to_end(&mut bytes)?;
                if bytes.len() > 16 * 1024 * 1024 {
                    return Err(Error::conflict("Disk integrity report queue is too large."));
                }
                let reports: BTreeMap<String, String> = serde_json::from_slice(&bytes)?;
                for (object, hash) in &reports {
                    crate::validation::uuid(object)?;
                    if !crate::nodes::snapshots::valid_hash(hash) {
                        return Err(Error::bad("Invalid retained integrity report."));
                    }
                }
                reports
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
            Err(error) => return Err(error.into()),
        };
        Ok(Self {
            file: Some(file),
            retained: AtomicBool::new(!pending.is_empty()),
            pending: Mutex::new(pending),
            sending: tokio::sync::Mutex::new(()),
        })
    }

    fn persist(&self, pending: &BTreeMap<String, String>) -> Result<()> {
        let Some(path) = &self.file else {
            return Ok(());
        };
        let parent = path
            .parent()
            .ok_or_else(|| Error::bad("Missing report directory."))?;
        let temporary = parent.join(format!(".{}.tmp", crate::config::id()));
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)?;
        file.write_all(&serde_json::to_vec(pending)?)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(temporary, path)?;
        File::open(parent)?.sync_all()?;
        Ok(())
    }

    pub fn record(&self, report: &Report) -> Result<()> {
        let mut pending = self.pending.lock().map_err(Error::internal)?;
        pending.insert(report.object.clone(), report.hash.clone());
        self.retained.store(true, Ordering::Release);
        // An fsync failure leaves the in-memory entry blocking acknowledgement.
        self.persist(&pending)
    }

    pub fn next(&self) -> Result<Option<Report>> {
        Ok(self
            .pending
            .lock()
            .map_err(Error::internal)?
            .first_key_value()
            .map(|(object, hash)| Report {
                object: object.clone(),
                hash: hash.clone(),
            }))
    }

    pub fn settled(&self, report: &Report) -> Result<()> {
        let mut pending = self.pending.lock().map_err(Error::internal)?;
        let mut remaining = pending.clone();
        remaining.remove(&report.object);
        self.persist(&remaining)?;
        *pending = remaining;
        self.retained.store(!pending.is_empty(), Ordering::Release);
        Ok(())
    }

    pub fn pending(&self) -> bool {
        self.retained.load(Ordering::Acquire)
    }

    pub fn acknowledge<T>(&self, commit: impl FnOnce() -> Result<T>) -> Result<T> {
        // Serialize with report insertion at the actual journal-reclamation
        // boundary, rather than checking earlier during receipt validation.
        let pending = self.pending.lock().map_err(Error::internal)?;
        if self.pending() || !pending.is_empty() {
            return Err(Error::unavailable(
                "Disk integrity reports await manager acknowledgement.",
            ));
        }
        commit()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn pending_reports_survive_reopen_and_fence_journal_reclamation() {
        let root = tempfile::tempdir().unwrap();
        let report = Report {
            object: crate::config::id(),
            hash: format!("b3-{}", "a".repeat(64)),
        };
        let reports = Reports::open(root.path()).unwrap();
        reports.record(&report).unwrap();
        reports.record(&report).unwrap();
        assert!(
            reports
                .acknowledge::<()>(|| panic!("Must retain the local journal"))
                .is_err()
        );
        drop(reports);
        let reports = Reports::open(root.path()).unwrap();
        assert!(reports.pending());
        assert_eq!(reports.next().unwrap().unwrap().object, report.object);
        reports.settled(&report).unwrap();
        reports.acknowledge(|| Ok(())).unwrap();
        assert!(!Reports::open(root.path()).unwrap().pending());
    }

    #[test]
    fn report_insertion_serializes_with_the_actual_acknowledgement() {
        let root = tempfile::tempdir().unwrap();
        let reports = Arc::new(Reports::open(root.path()).unwrap());
        let (entered, waiting) = std::sync::mpsc::channel();
        let (release, gate) = std::sync::mpsc::channel();
        let committing = reports.clone();
        let worker = std::thread::spawn(move || {
            committing.acknowledge(|| {
                entered.send(()).unwrap();
                gate.recv_timeout(std::time::Duration::from_secs(5))
                    .unwrap();
                Ok(())
            })
        });
        waiting
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        let recording = reports.clone();
        let reporter = std::thread::spawn(move || {
            recording.record(&Report {
                object: crate::config::id(),
                hash: format!("b3-{}", "b".repeat(64)),
            })
        });
        release.send(()).unwrap();
        worker.join().unwrap().unwrap();
        reporter.join().unwrap().unwrap();
        assert!(
            reports
                .acknowledge::<()>(|| panic!("Pending report raced through reclamation"))
                .is_err()
        );
    }
}
