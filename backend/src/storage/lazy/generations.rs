//! Sealing is local and short; reconstruction and remote publication happen later.
use super::*;
use rusqlite::OptionalExtension;

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
        self.update_accounting(&journal)?;
        Ok(generation)
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
        let _publication = self.publication.read().map_err(failure)?;
        let (mut manifest, mut changed) = {
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
        let capacity = self.blocks.bytes.lock().map_err(failure)?.block_capacity();
        let foreground = self.foreground.lock().map_err(failure)?.ranks(capacity);
        changed.sort_by_key(|index| foreground.get(index).map_or(0, |rank| rank + 1));
        let changed_blocks = changed.len();
        let mut retained_blocks = 0;
        let reconstruct = |index: u64| -> io::Result<_> {
            let offset = index * BLOCK;
            let mut bytes = vec![0; (self.size - offset).min(BLOCK) as usize];
            self.read_generation(generation, offset, &mut bytes)?;
            let hash = (!bytes.iter().all(|byte| *byte == 0)).then(|| block_digest(&bytes));
            Ok((index, bytes, hash))
        };
        for batch in changed.chunks(RECONSTRUCTION_CONCURRENCY) {
            let blocks = if batch.len() == 1 {
                vec![reconstruct(batch[0])?]
            } else {
                // A sealed generation is immutable. Overlap its independent base
                // reads; apply cache admission in the original working-set order.
                std::thread::scope(|scope| -> io::Result<Vec<_>> {
                    let mut workers = Vec::with_capacity(batch.len());
                    for &index in batch {
                        let reconstruct = &reconstruct;
                        workers.push(
                            std::thread::Builder::new()
                                .name("leo-journal-rebuild".into())
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
                // Persist published bytes without promoting a background scan.
                let _ = self.cache_block(&hash, &bytes);
                if foreground.contains_key(&index) {
                    let mut cache = self.blocks.bytes.lock().map_err(failure)?;
                    if cache.get(&hash, false).is_none() {
                        cache.insert(&hash, bytes, true);
                        retained_blocks += 1;
                    }
                }
            }
        }
        self.db
            .lock()
            .map_err(failure)?
            .execute(
                "UPDATE sealed SET manifest=?1 WHERE generation=?2",
                params![manifest.to_string(), generation],
            )
            .map_err(failure)?;
        tracing::info!(target: "leo_performance", operation = "journal_capture", generation, changed_blocks, retained_blocks);
        timing.finish();
        Ok(manifest)
    }

    pub fn captured_block(&self, generation: i64, hash: &str) -> io::Result<Vec<u8>> {
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
        let mut timing =
            crate::performance::Operation::new("journal_publish", backup_id, "reader_lock");
        let _publication = self.publication.write().map_err(failure)?;
        timing.next("commit");
        let mut journal = self.journal.lock().map_err(failure)?;
        let mut db = self.db.lock().map_err(failure)?;
        let tx = db.transaction().map_err(failure)?;
        let previous: Option<String> = tx
            .query_row(
                "SELECT value FROM settings WHERE key='published'",
                [],
                |r| r.get(0),
            )
            .optional()
            .map_err(failure)?;
        let receipt = serde_json::json!({
            "generation": generation,
            "backupId": backup_id
        });
        let already_published = previous
            .as_ref()
            .is_some_and(|p| serde_json::from_str::<Value>(p).is_ok_and(|p| p == receipt));
        if already_published && self.accounting.lock().map_err(failure)?["published"] == receipt {
            timing.finish();
            return Ok(());
        }
        let manifest: String = if already_published {
            // A previous commit may have succeeded but lost its local response.
            // Reconcile the in-memory base before acknowledging a repeated receipt.
            tx.query_row("SELECT manifest FROM state WHERE id=1", [], |r| r.get(0))
                .map_err(failure)?
        } else {
            tx.query_row(
                "SELECT manifest FROM sealed WHERE generation=?1",
                [generation],
                |r| r.get(0),
            )
            .map_err(failure)?
        };
        let next_base = Arc::new(serde_json::from_str::<Value>(&manifest).map_err(failure)?);
        validate(&next_base)?;
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
        #[cfg(test)]
        journal::crash_point(&self.directory, "publication_transaction");
        tx.commit().map_err(failure)?;
        *base = next_base.clone();
        #[cfg(test)]
        journal::crash_point(&self.directory, "publication_committed");
        let retired = journal.retire(generation);
        self.update_accounting(&journal)?;
        self.accounting.lock().map_err(failure)?["published"] = receipt;
        drop(base);
        drop(db);
        drop(journal);
        drop(_publication);
        timing.next("reclaim");
        // Reclamation failures do not undo the durable receipt. Recovery retries
        // orphan deletion before serving the disk; unpublished frames stay owned.
        if let Err(error) = journal::Journal::reclaim(&self.directory, &retired) {
            tracing::warn!(target: "leo_performance", operation="journal_reclaim_failed", kind=?error.kind());
        }
        timing.next("retire_clean_cache");
        let needed = next_base["blocks"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|block| block["hash"].as_str())
            .collect();
        match super::super::cache::retire(self.node_state(), &self.directory.join("cache"), &needed)
        {
            Ok(bytes) => {
                tracing::info!(target: "leo_performance", operation = "cache_retired", id = Self::identity(&self.directory), bytes);
            }
            // Clean cache eviction is optional; a local eviction failure must
            // not report an already durable publication as unacknowledged.
            Err(error) => {
                tracing::warn!(target: "leo_performance", operation = "cache_retire_failed", id = Self::identity(&self.directory), kind = ?error.kind());
            }
        }
        timing.finish();
        Ok(())
    }
}
