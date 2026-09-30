//! Durable connection context and accounting independent of the mounted view.
use super::*;

impl LazyDisk {
    pub fn performance(&self) -> Value {
        let mut status = self.metrics.snapshot();
        if let Ok(cache) = self.blocks.bytes.lock() {
            status["blockCache"] = cache.status();
        }
        if let Ok(cache) = self.records.lock() {
            status["journalCache"] = cache.status();
        }
        status
    }

    /// Private controller context, never included in exported recovery manifests.
    pub fn set_context(&self, value: &Value) -> io::Result<()> {
        let policy = super::super::policy::Policy::for_node(&value["policy"]).map_err(failure)?;
        self.memory_budget(policy.memory_cache_mi_b as usize * 1024 * 1024)?;
        let encoded = value.to_string();
        if encoded.len() > 16384 {
            return Err(failure("Disk connection context too large"));
        }
        self.db
            .lock()
            .map_err(failure)?
            .execute(
                "INSERT INTO settings(key,value) VALUES ('context',?1) ON CONFLICT(key) DO \
            UPDATE SET value=excluded.value",
                [encoded],
            )
            .map_err(failure)?;
        Ok(())
    }

    pub fn context(directory: &Path) -> io::Result<Value> {
        let db = Connection::open_with_flags(
            if directory.join("journal-v2.sqlite").exists() {
                directory.join("journal-v2.sqlite")
            } else {
                directory.join("journal.sqlite")
            },
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .map_err(failure)?;
        let encoded: String = db
            .query_row("SELECT value FROM settings WHERE key='context'", [], |r| {
                r.get(0)
            })
            .map_err(failure)?;
        if encoded.len() > 16384 {
            return Err(failure("Disk connection context too large"));
        }
        serde_json::from_str(&encoded).map_err(failure)
    }

    pub fn accounting(&self) -> io::Result<Value> {
        Ok(self.accounting.lock().map_err(failure)?.clone())
    }

    pub(crate) fn dirty_since(&self) -> Option<i64> {
        let at = self.dirty_since.load(Ordering::Acquire);
        (at >= 0).then_some(at)
    }

    pub(super) fn update_accounting(&self, journal: &journal::Journal) -> io::Result<()> {
        let mut accounting = self.accounting.lock().map_err(failure)?;
        let published = accounting["published"].take();
        *accounting = journal.stats();
        self.dirty_since.store(
            accounting["dirtySince"].as_i64().unwrap_or(-1),
            Ordering::Release,
        );
        accounting["published"] = published;
        Ok(())
    }
}

pub(super) fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}
