//! Sealing is local and short; reconstruction and consolidation happen later.
use super::*;
use rusqlite::OptionalExtension;
use std::sync::{RwLockWriteGuard, TryLockError};

const RECONSTRUCTION_CONCURRENCY: usize = 16;

impl LazyDisk {
    /// Seal a durable journal prefix while later writes enter the next generation.
    /// Guest filesystem flushing, when required, belongs to the capture caller.
    pub fn seal(&self) -> io::Result<i64> {
        self.seal_with_completion(false)
    }

    pub fn seal_completed(&self) -> io::Result<i64> {
        self.seal_with_completion(true)
    }

    fn seal_with_completion(&self, completed: bool) -> io::Result<i64> {
        let _write = self.write_gate.write().map_err(failure)?;
        let _geometry = self.publication.read().map_err(failure)?;
        let mut journal = self.journal.lock().map_err(failure)?;
        journal.sync()?;
        let mut db = self.db.lock().map_err(failure)?;
        let tx = db.transaction().map_err(failure)?;
        let generation: i64 = tx
            .query_row("SELECT generation FROM state WHERE id=1", [], |r| r.get(0))
            .map_err(failure)?;
        let next = generation
            .checked_add(1)
            .ok_or_else(|| failure("Generation exhausted"))?;
        tx.execute("INSERT INTO sealed(generation) VALUES (?1)", [generation])
            .map_err(failure)?;
        tx.execute(
            "UPDATE state SET generation=?1,next_sequence=?2 WHERE id=1",
            params![next, journal.next],
        )
        .map_err(failure)?;
        if completed {
            let receipt =
                serde_json::json!({ "generation": generation, "capturedAt": crate::config::now() });
            tx.execute("INSERT INTO settings(key,value) VALUES ('completed',?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value", [receipt.to_string()]).map_err(failure)?;
        }
        if let Err(error) = tx.commit() {
            journal.fail();
            return Err(failure(error));
        }
        journal.sealed(next);
        self.sealed_pending.store(true, Ordering::Release);
        self.update_accounting(&journal)?;
        Ok(generation)
    }

    pub(crate) fn sealed_pending(&self) -> bool {
        self.sealed_pending.load(Ordering::Acquire)
    }

    /// Bytes of every journal segment still owned, sealed or not.
    pub(crate) fn journal_bytes(&self) -> u64 {
        self.journal_bytes.load(Ordering::Acquire)
    }

    /// A new owner is waiting for this journal. A background consolidation
    /// stops before its next block and releases the disk; the next owner
    /// consolidates the same sealed generations again.
    pub(crate) fn yield_consolidation(&self) {
        self.yield_requested.store(true, Ordering::Release);
    }

    /// Fold every sealed generation into a new local base, then retire its
    /// journal segments and the block files no base references. Each block
    /// file and its directory are durable before the base names it. A crash
    /// before the commit keeps the previous base and journal; a crash after it
    /// leaves only unreferenced files, which recovery or the next run removes.
    /// Guest reads and writes continue throughout; the commit drains readers.
    pub fn consolidate(&self) -> io::Result<Option<i64>> {
        let _running = match self.consolidating.try_lock() {
            Ok(running) => running,
            Err(TryLockError::WouldBlock) => return Ok(None),
            Err(error) => return Err(failure(error)),
        };
        let generation: Option<i64> = self
            .db
            .lock()
            .map_err(failure)?
            .query_row("SELECT MAX(generation) FROM sealed", [], |row| row.get(0))
            .map_err(failure)?;
        let Some(generation) = generation else {
            self.sealed_pending.store(false, Ordering::Release);
            return Ok(None);
        };
        let mut timing = crate::performance::Operation::new(
            "journal_consolidate",
            Self::identity(&self.directory),
            "reconstruct_blocks",
        );
        let reserve = self.consolidation_reserve()?;
        let (base, manifest) = {
            let _publication = self.publication.read().map_err(failure)?;
            let base = self.base.lock().map_err(failure)?.clone();
            let changed = self
                .journal
                .lock()
                .map_err(failure)?
                .changed(generation)
                .into_iter()
                .collect();
            let manifest = self.rebuild(generation, &base, changed, |hash, bytes| {
                self.persist_block(hash, bytes, reserve)
            })?;
            (base, manifest)
        };
        #[cfg(test)]
        journal::crash_point(&self.directory, "consolidation_blocks_written");
        match File::open(self.directory.join("cache")) {
            Ok(directory) => directory.sync_all()?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        #[cfg(test)]
        journal::crash_point(&self.directory, "consolidation_blocks_durable");
        timing.next("reader_lock");
        let publication = self.publication.write().map_err(failure)?;
        if !Arc::ptr_eq(&base, &*self.base.lock().map_err(failure)?) {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "Disk base changed during consolidation",
            ));
        }
        let receipt = serde_json::json!({ "generation": generation, "local": true });
        self.install_base(
            generation,
            manifest.to_string(),
            receipt,
            publication,
            timing,
            "consolidation",
        )?;
        Ok(Some(generation))
    }

    /// Reconstruct the blocks changed through `generation` over `base`. The
    /// generation is immutable, so independent blocks are rebuilt in parallel;
    /// cache admission follows the original working-set order.
    fn rebuild(
        &self,
        generation: i64,
        base: &Value,
        mut changed: Vec<u64>,
        store: impl Fn(&str, &[u8]) -> io::Result<()> + Sync,
    ) -> io::Result<Value> {
        let mut manifest = base.clone();
        let capacity = self.blocks.bytes.lock().map_err(failure)?.block_capacity();
        let foreground = self.foreground.lock().map_err(failure)?.ranks(capacity);
        changed.sort_by_key(|index| foreground.get(index).map_or(0, |rank| rank + 1));
        let changed_blocks = changed.len();
        let mut retained_blocks = 0;
        let reconstruct = |index: u64| -> io::Result<_> {
            if self.yield_requested.load(Ordering::Acquire) {
                return Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "Journal reconstruction yielded to a new disk owner",
                ));
            }
            let offset = index * BLOCK;
            let mut bytes = vec![0; (self.size() - offset).min(BLOCK) as usize];
            self.read_generation(generation, offset, &mut bytes)?;
            let hash = (!bytes.iter().all(|byte| *byte == 0)).then(|| block_digest(&bytes));
            if let Some(hash) = &hash {
                store(hash, &bytes)?;
            }
            Ok((index, bytes, hash))
        };
        for batch in changed.chunks(RECONSTRUCTION_CONCURRENCY) {
            let blocks = if batch.len() == 1 {
                vec![reconstruct(batch[0])?]
            } else {
                // A sealed generation is immutable. Overlap its independent base
                // reads and block writes; admit cache entries in working-set order.
                std::thread::scope(|scope| -> io::Result<Vec<_>> {
                    let mut workers = Vec::with_capacity(batch.len());
                    for &index in batch {
                        let reconstruct = &reconstruct;
                        workers.push(
                            std::thread::Builder::new()
                                .name("cairn-journal-rebuild".into())
                                .spawn_scoped(scope, move || reconstruct(index))
                                .map_err(failure)?,
                        );
                    }
                    let outcomes = workers
                        .into_iter()
                        .map(|worker| {
                            worker
                                .join()
                                .map_err(|_| failure("Journal reconstruction worker panicked"))
                                .and_then(|outcome| outcome)
                        })
                        .collect::<Vec<_>>();
                    outcomes.into_iter().collect()
                })?
            };
            for (index, bytes, hash) in blocks {
                let Some(hash) = hash else {
                    manifest["blocks"][index as usize]["hash"] = Value::Null;
                    continue;
                };
                manifest["blocks"][index as usize]["hash"] = hash.clone().into();
                // Keep recently read blocks hot without promoting a background scan.
                if foreground.contains_key(&index) {
                    let mut cache = self.blocks.bytes.lock().map_err(failure)?;
                    if cache.get(&hash, false).is_none() {
                        cache.insert(&hash, bytes, true);
                        retained_blocks += 1;
                    }
                }
            }
        }
        tracing::info!(target: "cairn_performance", operation = "journal_rebuild", generation, changed_blocks, retained_blocks);
        Ok(manifest)
    }


    /// Durable completed-turn boundary, including after controller restart.
    pub fn completed(&self) -> io::Result<Option<Value>> {
        let encoded: Option<String> = self
            .db
            .lock()
            .map_err(failure)?
            .query_row(
                "SELECT value FROM settings WHERE key='completed'",
                [],
                |row| row.get(0),
            )
            .optional()
            .map_err(failure)?;
        encoded
            .map(|value| serde_json::from_str(&value).map_err(failure))
            .transpose()
    }

    pub fn has_sealed(&self, generation: i64) -> io::Result<bool> {
        self.db
            .lock()
            .map_err(failure)?
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sealed WHERE generation=?1)",
                [generation],
                |row| row.get(0),
            )
            .map_err(failure)
    }

    pub fn capture(&self, generation: i64) -> io::Result<Value> {
        let timing = crate::performance::Operation::new(
            "journal_capture",
            Self::identity(&self.directory),
            "reconstruct_blocks",
        );
        // Consolidation retires block files that only a sealed manifest names.
        let _consolidation = self.consolidating.lock().map_err(failure)?;
        let _publication = self.publication.read().map_err(failure)?;
        let (manifest, changed) = {
            let journal = self.journal.lock().map_err(failure)?;
            let db = self.db.lock().map_err(failure)?;
            let previous: Option<String> = db
                .query_row(
                    "SELECT manifest FROM sealed WHERE generation=?1",
                    [generation],
                    |r| r.get(0),
                )
                .map_err(failure)?;
            if let Some(manifest) = previous {
                let manifest = serde_json::from_str(&manifest).map_err(failure)?;
                timing.finish();
                return Ok(manifest);
            }
            let manifest: String = db
                .query_row("SELECT manifest FROM state WHERE id=1", [], |r| r.get(0))
                .map_err(failure)?;
            let changed = journal.changed(generation).into_iter().collect::<Vec<_>>();
            (
                serde_json::from_str::<Value>(&manifest).map_err(failure)?,
                changed,
            )
        };
        // Persisting reconstructed blocks is optional for a remote publication.
        let manifest = self.rebuild(generation, &manifest, changed, |hash, bytes| {
            let _ = self.persist_block(hash, bytes, None);
            Ok(())
        })?;
        self.db
            .lock()
            .map_err(failure)?
            .execute(
                "UPDATE sealed SET manifest=?1 WHERE generation=?2",
                params![manifest.to_string(), generation],
            )
            .map_err(failure)?;
        timing.finish();
        Ok(manifest)
    }

    pub fn captured_block(&self, generation: i64, hash: &str) -> io::Result<Vec<u8>> {
        let metadata = crate::performance::publication::start(
            crate::performance::publication::Stage::NodeMetadata,
        );
        let _publication = self.publication.read().map_err(failure)?;
        let manifest: String = self
            .db
            .lock()
            .map_err(failure)?
            .query_row(
                "SELECT manifest FROM sealed WHERE generation=?1",
                [generation],
                |r| r.get(0),
            )
            .map_err(failure)?;
        let manifest: Value = serde_json::from_str(&manifest).map_err(failure)?;
        let block = manifest["blocks"]
            .as_array()
            .ok_or_else(|| failure("Invalid captured manifest"))?
            .iter()
            .find(|b| b["hash"] == hash)
            .ok_or_else(|| failure("Block outside captured generation"))?;
        let offset = block["offset"]
            .as_u64()
            .ok_or_else(|| failure("Invalid extent"))?;
        let length = block["size"]
            .as_u64()
            .filter(|size| *size <= BLOCK)
            .ok_or_else(|| failure("Invalid extent"))?;
        metadata.finish(0);
        let mut bytes = vec![0; length as usize];
        self.read_generation(generation, offset, &mut bytes)?;
        if !super::super::digest::matches(hash, &bytes) {
            return Err(failure("Captured generation changed"));
        }
        Ok(bytes)
    }

    /// Call only after the master has durably published every dependency and
    /// authorized reads of the new base. Completion drains readers of the old base.
    pub fn commit_published(&self, generation: i64, backup_id: &str) -> io::Result<()> {
        self.commit_published_guarded(generation, backup_id, false, || Ok(()))
    }

    pub(crate) fn try_commit_published_guarded<G>(
        &self,
        generation: i64,
        backup_id: &str,
        guard: impl FnOnce() -> io::Result<G>,
    ) -> io::Result<()> {
        self.commit_published_guarded(generation, backup_id, true, guard)
    }

    fn commit_published_guarded<G>(
        &self,
        generation: i64,
        backup_id: &str,
        nonblocking: bool,
        guard: impl FnOnce() -> io::Result<G>,
    ) -> io::Result<()> {
        let mut timing =
            crate::performance::Operation::new("journal_publish", backup_id, "reader_lock");
        let publication = if nonblocking {
            self.publication.try_write().map_err(|error| match error {
                std::sync::TryLockError::WouldBlock => io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "Journal readers must finish before acknowledgement.",
                ),
                error => failure(error),
            })?
        } else {
            self.publication.write().map_err(failure)?
        };
        // Readers take publication before reporting; never reverse that order.
        let _guard = guard()?;
        timing.next("commit");
        let receipt = serde_json::json!({
            "generation": generation,
            "backupId": backup_id
        });
        let manifest: String = {
            let db = self.db.lock().map_err(failure)?;
            let previous: Option<String> = db
                .query_row(
                    "SELECT value FROM settings WHERE key='published'",
                    [],
                    |r| r.get(0),
                )
                .optional()
                .map_err(failure)?;
            let already_published = previous
                .as_ref()
                .is_some_and(|p| serde_json::from_str::<Value>(p).is_ok_and(|p| p == receipt));
            if already_published
                && self.accounting.lock().map_err(failure)?["published"] == receipt
            {
                timing.finish();
                return Ok(());
            }
            if already_published {
                // A previous commit may have succeeded but lost its local response.
                // Reconcile the in-memory base before acknowledging a repeated receipt.
                db.query_row("SELECT manifest FROM state WHERE id=1", [], |r| r.get(0))
                    .map_err(failure)?
            } else {
                db.query_row(
                    "SELECT manifest FROM sealed WHERE generation=?1",
                    [generation],
                    |r| r.get(0),
                )
                .map_err(failure)?
            }
        };
        self.install_base(
            generation,
            manifest,
            receipt,
            publication,
            timing,
            "publication",
        )
    }

    /// Atomically replace the base with `manifest`, record `receipt` as the
    /// journal cutoff and drop the descriptors of generations through it. The
    /// caller holds the exclusive publication lock, so no reader observes a
    /// partly replaced base. Segment and block file removal follow the commit
    /// and never undo it; recovery or a later consolidation finishes them.
    fn install_base(
        &self,
        generation: i64,
        manifest: String,
        receipt: Value,
        publication: RwLockWriteGuard<'_, ()>,
        mut timing: crate::performance::Operation,
        phase: &str,
    ) -> io::Result<()> {
        let mut journal = self.journal.lock().map_err(failure)?;
        let mut db = self.db.lock().map_err(failure)?;
        let next_base = Arc::new(serde_json::from_str::<Value>(&manifest).map_err(failure)?);
        validate(&next_base)?;
        let tx = db.transaction().map_err(failure)?;
        let mut base = self.base.lock().map_err(failure)?;
        tx.execute("UPDATE state SET manifest=?1 WHERE id=1", [manifest])
            .map_err(failure)?;
        tx.execute("DELETE FROM segments WHERE generation<=?1", [generation])
            .map_err(failure)?;
        tx.execute("DELETE FROM sealed WHERE generation<=?1", [generation])
            .map_err(failure)?;
        tx.execute(
            "INSERT INTO settings(key,value) VALUES ('published',?1) ON CONFLICT(key) \
            DO UPDATE SET value=excluded.value",
            [receipt.to_string()],
        )
        .map_err(failure)?;
        let sealed_remaining: bool = tx
            .query_row("SELECT EXISTS(SELECT 1 FROM sealed)", [], |row| row.get(0))
            .map_err(failure)?;
        // A newer captured generation may name block files the new base does not.
        let captured = tx
            .prepare("SELECT manifest FROM sealed WHERE manifest IS NOT NULL")
            .map_err(failure)?
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(failure)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(failure)?
            .iter()
            .map(|manifest| serde_json::from_str::<Value>(manifest).map_err(failure))
            .collect::<io::Result<Vec<_>>>()?;
        #[cfg(test)]
        journal::crash_point(&self.directory, &format!("{phase}_transaction"));
        tx.commit().map_err(failure)?;
        *base = next_base.clone();
        #[cfg(test)]
        journal::crash_point(&self.directory, &format!("{phase}_committed"));
        let retired = journal.retire(generation);
        self.sealed_pending
            .store(sealed_remaining, Ordering::Release);
        self.update_accounting(&journal)?;
        self.accounting.lock().map_err(failure)?["published"] = receipt;
        drop(base);
        drop(db);
        drop(journal);
        drop(publication);
        timing.next("reclaim");
        // Reclamation failures do not undo the durable base. Recovery retries
        // orphan deletion before serving the disk; newer frames stay owned.
        if let Err(error) = journal::Journal::reclaim(&self.directory, &retired) {
            tracing::warn!(target: "cairn_performance", operation="journal_reclaim_failed", kind=?error.kind());
        }
        #[cfg(test)]
        journal::crash_point(&self.directory, &format!("{phase}_reclaimed"));
        timing.next("retire_blocks");
        let needed = std::iter::once(next_base.as_ref())
            .chain(&captured)
            .flat_map(|manifest| manifest["blocks"].as_array().unwrap())
            .filter_map(|block| block["hash"].as_str())
            .collect();
        match super::super::cache::retire(&self.directory.join("cache"), &needed) {
            Ok(bytes) => {
                tracing::info!(target: "cairn_performance", operation = "blocks_retired", id = Self::identity(&self.directory), phase, bytes);
            }
            // Unreferenced files only cost space until the next consolidation.
            Err(error) => {
                tracing::warn!(target: "cairn_performance", operation = "block_retire_failed", id = Self::identity(&self.directory), kind = ?error.kind());
            }
        }
        timing.finish();
        Ok(())
    }
}
