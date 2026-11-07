//! Durable identities awaiting manager-side corruption reconciliation. The disk's
//! existing context owns authentication; no URL, key or credential is copied here.
use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, OnceLock, RwLock, RwLockReadGuard, Weak,
        atomic::{AtomicBool, Ordering},
    },
};

pub(super) fn fences(state: &Path) -> Result<Arc<Reports>> {
    use std::os::unix::fs::PermissionsExt;
    static REGISTRY: OnceLock<Mutex<BTreeMap<PathBuf, Weak<Reports>>>> = OnceLock::new();
    let directory = state.join("publication-fences");
    let mut registry = REGISTRY
        .get_or_init(Mutex::default)
        .lock()
        .map_err(Error::internal)?;
    if let Some(fences) = registry.get(&directory).and_then(Weak::upgrade) {
        return Ok(fences);
    }
    std::fs::create_dir_all(&directory)?;
    std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700))?;
    // A synced report and child directory cannot preserve an unsynced parent
    // entry. Make the fence directory durable before any ACK can rely on it.
    File::open(state)?.sync_all()?;
    let fences = Arc::new(Reports::open(&directory)?);
    registry.retain(|_, entry| entry.strong_count() > 0);
    registry.insert(directory, Arc::downgrade(&fences));
    Ok(fences)
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Report {
    pub object: String,
    pub hash: String,
}

#[derive(Default)]
pub(super) struct Reports {
    file: Option<PathBuf>,
    state: RwLock<State>,
    retained: AtomicBool,
    pub sending: tokio::sync::Mutex<()>,
}

#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct State {
    pending: BTreeMap<String, String>,
    rejected: BTreeSet<String>,
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
                let reports: State = serde_json::from_slice(&bytes)?;
                for (object, hash) in &reports.pending {
                    crate::validation::uuid(object)?;
                    if !crate::nodes::snapshots::valid_hash(hash) {
                        return Err(Error::bad("Invalid retained integrity report."));
                    }
                }
                for publication in &reports.rejected {
                    crate::validation::uuid(publication)?;
                }
                reports
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => State::default(),
            Err(error) => return Err(error.into()),
        };
        Ok(Self {
            file: Some(file),
            retained: AtomicBool::new(!pending.pending.is_empty()),
            state: RwLock::new(pending),
            sending: tokio::sync::Mutex::new(()),
        })
    }

    fn persist(&self, pending: &State) -> Result<()> {
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
        let mut pending = self.state.write().map_err(Error::internal)?;
        pending
            .pending
            .insert(report.object.clone(), report.hash.clone());
        self.retained.store(true, Ordering::Release);
        // An fsync failure leaves the in-memory entry blocking acknowledgement.
        self.persist(&pending)
    }

    pub fn next(&self) -> Result<Option<Report>> {
        Ok(self
            .state
            .read()
            .map_err(Error::internal)?
            .pending
            .first_key_value()
            .map(|(object, hash)| Report {
                object: object.clone(),
                hash: hash.clone(),
            }))
    }

    pub fn settled(&self, report: &Report) -> Result<()> {
        let mut pending = self.state.write().map_err(Error::internal)?;
        let mut remaining = pending.clone();
        remaining.pending.remove(&report.object);
        self.persist(&remaining)?;
        *pending = remaining;
        self.retained
            .store(!pending.pending.is_empty(), Ordering::Release);
        Ok(())
    }

    pub fn pending(&self) -> bool {
        self.retained.load(Ordering::Acquire)
    }

    pub fn guard(&self, publication: &str) -> Result<RwLockReadGuard<'_, State>> {
        // Serialize with report insertion at the actual journal-reclamation
        // boundary, rather than checking earlier during receipt validation.
        let pending = self.state.read().map_err(Error::internal)?;
        if pending.rejected.contains(publication) {
            return Err(Error::conflict("This publication was durably fenced."));
        }
        if self.pending() || !pending.pending.is_empty() {
            return Err(Error::unavailable(
                "Disk integrity reports await manager acknowledgement.",
            ));
        }
        Ok(pending)
    }

    pub fn fence(&self, publication: &str) -> Result<()> {
        crate::validation::uuid(publication)?;
        let mut state = self.state.write().map_err(Error::internal)?;
        state.rejected.insert(publication.to_owned());
        self.persist(&state)
    }

    #[cfg(test)]
    pub fn acknowledge<T>(&self, commit: impl FnOnce() -> Result<T>) -> Result<T> {
        let _guard = self.guard("")?;
        commit()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::{BlockSource, Disk, LazyDisk};
    use std::sync::Arc;

    struct Cold {
        entered: std::sync::mpsc::SyncSender<()>,
        resume: Mutex<std::sync::mpsc::Receiver<()>>,
        reports: Arc<Reports>,
        bytes: Vec<u8>,
    }

    impl BlockSource for Cold {
        fn fetch(&self, _: &str) -> std::io::Result<Vec<u8>> {
            self.entered.send(()).unwrap();
            self.resume
                .lock()
                .unwrap()
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap();
            self.reports.next().map_err(std::io::Error::other)?;
            Ok(self.bytes.clone())
        }
    }

    #[test]
    fn actual_journal_cold_read_excludes_acknowledgement_without_lock_inversion() {
        let root = tempfile::tempdir().unwrap();
        let reports = Arc::new(Reports::open(root.path()).unwrap());
        let (entered, waiting) = std::sync::mpsc::sync_channel(1);
        let (resume, gate) = std::sync::mpsc::channel();
        let bytes = vec![42; 4096];
        let hash = crate::storage::digest::block(&bytes);
        let manifest = serde_json::json!({"version":1,"size":4096,"blockSize":4194304,"blocks":[{"offset":0,"size":4096,"hash":hash}]});
        let disk = Arc::new(
            LazyDisk::create(
                &root.path().join("lazy"),
                &manifest,
                Arc::new(Cold {
                    entered,
                    resume: Mutex::new(gate),
                    reports: reports.clone(),
                    bytes: bytes.clone(),
                }),
            )
            .unwrap(),
        );
        let reading = disk.clone();
        let reader = std::thread::spawn(move || {
            let mut bytes = vec![0; 4096];
            reading.read_at(0, &mut bytes).unwrap();
            bytes
        });
        waiting
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        let error = disk
            .try_commit_published_guarded(1, &crate::config::id(), || -> std::io::Result<()> {
                panic!("Must acquire the journal lock before the report guard")
            })
            .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::WouldBlock);
        resume.send(()).unwrap();
        assert_eq!(reader.join().unwrap(), bytes);
        let generation = disk.seal().unwrap();
        disk.capture(generation).unwrap();
        let publication = crate::config::id();
        disk.try_commit_published_guarded(generation, &publication, || {
            reports.guard(&publication).map_err(std::io::Error::other)
        })
        .unwrap();
    }

    #[test]
    fn durable_node_fence_rejects_a_late_ack_after_restart_but_allows_a_replacement() {
        struct Zero;
        impl BlockSource for Zero {
            fn fetch(&self, _: &str) -> std::io::Result<Vec<u8>> {
                panic!("Sparse base requires no remote reads")
            }
        }
        let root = tempfile::tempdir().unwrap();
        let state = root.path().join("state");
        let disk=LazyDisk::create(&root.path().join("lazy"),&serde_json::json!({"version":1,"size":4096,"blockSize":4194304,"blocks":[{"offset":0,"size":4096,"hash":null}]}),Arc::new(Zero)).unwrap();
        disk.write_at(0, b"healthy unpublished work").unwrap();
        let generation = disk.seal().unwrap();
        disk.capture(generation).unwrap();
        let old = crate::config::id();
        fences(&state).unwrap().fence(&old).unwrap();
        // No live registry Arc remains: this reload exercises the durable fence.
        let restarted = fences(&state).unwrap();
        assert!(
            disk.try_commit_published_guarded(generation, &old, || restarted
                .guard(&old)
                .map_err(std::io::Error::other))
                .is_err()
        );
        assert!(disk.accounting().unwrap()["dirtyBytes"].as_u64().unwrap() > 0);
        let replacement = crate::config::id();
        disk.try_commit_published_guarded(generation, &replacement, || {
            restarted.guard(&replacement).map_err(std::io::Error::other)
        })
        .unwrap();
        assert_eq!(disk.accounting().unwrap()["dirtyBytes"], 0);
    }

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
