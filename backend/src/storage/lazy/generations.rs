//! Sealing is local and short; reconstruction and remote publication happen later.
use super::*;
use std::collections::BTreeSet;
impl LazyDisk {
    /// The caller freezes the guest filesystem and drains guest I/O before sealing.
    pub fn seal(&self) -> io::Result<i64> {
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
        tx.execute("UPDATE state SET generation=?1 WHERE id=1", [next])
            .map_err(failure)?;
        tx.commit().map_err(failure)?;
        Ok(generation)
    }
    pub fn capture(&self, generation: i64) -> io::Result<Value> {
        let _publication = self.publication.read().map_err(failure)?;
        let (mut manifest, changed) = {
            let db = self.db.lock().map_err(failure)?;
            let previous: Option<String> = db
                .query_row(
                    "SELECT manifest FROM sealed WHERE generation=?1",
                    [generation],
                    |r| r.get(0),
                )
                .map_err(failure)?;
            if let Some(manifest) = previous {
                return serde_json::from_str(&manifest).map_err(failure);
            }
            let manifest: String = db
                .query_row("SELECT manifest FROM state WHERE id=1", [], |r| r.get(0))
                .map_err(failure)?;
            let mut changed = BTreeSet::new();
            let mut statement = db
                .prepare("SELECT start,end FROM writes WHERE generation<=?1")
                .map_err(failure)?;
            let mut rows = statement.query([generation]).map_err(failure)?;
            while let Some(row) = rows.next().map_err(failure)? {
                let start = row.get::<_, i64>(0).map_err(failure)? as u64;
                let end = row.get::<_, i64>(1).map_err(failure)? as u64;
                changed.extend(start / BLOCK..end.div_ceil(BLOCK));
            }
            (
                serde_json::from_str::<Value>(&manifest).map_err(failure)?,
                changed,
            )
        };
        for index in changed {
            let offset = index * BLOCK;
            let mut bytes = vec![0; (self.size - offset).min(BLOCK) as usize];
            self.read_generation(generation, offset, &mut bytes)?;
            manifest["blocks"][index as usize]["hash"] = if bytes.iter().all(|b| *b == 0) {
                Value::Null
            } else {
                hex::encode(Sha256::digest(&bytes)).into()
            };
        }
        self.db
            .lock()
            .map_err(failure)?
            .execute(
                "UPDATE sealed SET manifest=?1 WHERE generation=?2",
                params![manifest.to_string(), generation],
            )
            .map_err(failure)?;
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
        if hex::encode(Sha256::digest(&bytes)) != hash {
            return Err(failure("Captured generation changed"));
        }
        Ok(bytes)
    }
    /// Call only after the master has durably published every dependency and
    /// authorized reads of the new base. Completion drains readers of the old base.
    pub fn commit_published(&self, generation: i64, backup_id: &str) -> io::Result<()> {
        let _publication = self.publication.write().map_err(failure)?;
        let mut db = self.db.lock().map_err(failure)?;
        let tx = db.transaction().map_err(failure)?;
        use rusqlite::OptionalExtension;
        let previous: Option<String> = tx
            .query_row(
                "SELECT value FROM settings WHERE key='published'",
                [],
                |r| r.get(0),
            )
            .optional()
            .map_err(failure)?;
        let receipt = serde_json::json!({"generation":generation,"backupId":backup_id});
        if previous
            .as_ref()
            .is_some_and(|p| serde_json::from_str::<Value>(p).is_ok_and(|p| p == receipt))
        {
            return Ok(());
        }
        let manifest: String = tx
            .query_row(
                "SELECT manifest FROM sealed WHERE generation=?1",
                [generation],
                |r| r.get(0),
            )
            .map_err(failure)?;
        let next_base = Arc::new(serde_json::from_str::<Value>(&manifest).map_err(failure)?);
        validate(&next_base)?;
        let mut base = self.base.lock().map_err(failure)?;
        tx.execute("UPDATE state SET manifest=?1 WHERE id=1", [manifest])
            .map_err(failure)?;
        tx.execute("DELETE FROM epochs WHERE generation<=?1", [generation])
            .map_err(failure)?;
        tx.execute("DELETE FROM writes WHERE generation<=?1", [generation])
            .map_err(failure)?;
        tx.execute("DELETE FROM sealed WHERE generation<=?1", [generation])
            .map_err(failure)?;
        tx.execute("INSERT INTO settings(key,value) VALUES ('published',?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value",[receipt.to_string()]).map_err(failure)?;
        tx.commit().map_err(failure)?;
        *base = next_base;
        db.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")
            .map_err(failure)?;
        File::open(&self.directory)?.sync_all()
    }
}
