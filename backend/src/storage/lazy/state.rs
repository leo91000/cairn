//! Durable connection context and accounting independent of the mounted view.
use super::*;

impl LazyDisk {
    pub fn performance(&self) -> Value {
        self.metrics.snapshot()
    }

    /// Private controller context, never included in exported recovery manifests.
    pub fn set_context(&self, value: &Value) -> io::Result<()> {
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

    pub(super) fn update_accounting(&self, journal: &journal::Journal) -> io::Result<()> {
        let mut accounting = self.accounting.lock().map_err(failure)?;
        let published = accounting["published"].take();
        *accounting = journal.stats();
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
