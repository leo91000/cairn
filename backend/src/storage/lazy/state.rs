//! Durable connection context and accounting independent of the mounted view.
use super::*;
impl LazyDisk {
    /// Private controller context, never included in exported recovery manifests.
    pub fn set_context(&self, value: &Value) -> io::Result<()> {
        let encoded = value.to_string();
        if encoded.len() > 16384 {
            return Err(failure("Disk connection context too large"));
        }
        self.db.lock().map_err(failure)?.execute("INSERT INTO settings(key,value) VALUES ('context',?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value", [encoded]).map_err(failure)?;
        Ok(())
    }
    pub fn context(directory: &Path) -> io::Result<Value> {
        let db = Connection::open_with_flags(
            directory.join("journal.sqlite"),
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
        let db = self.db.lock().map_err(failure)?;
        let dirty: i64 = db
            .query_row(
                "SELECT COALESCE(SUM(length(data)),0) FROM writes",
                [],
                |r| r.get(0),
            )
            .map_err(failure)?;
        let since: Option<i64> = db
            .query_row("SELECT MIN(written_at) FROM epochs", [], |r| r.get(0))
            .map_err(failure)?;
        let generation: i64 = db
            .query_row("SELECT generation FROM state WHERE id=1", [], |r| r.get(0))
            .map_err(failure)?;
        use rusqlite::OptionalExtension;
        let published: Option<String> = db
            .query_row(
                "SELECT value FROM settings WHERE key='published'",
                [],
                |r| r.get(0),
            )
            .optional()
            .map_err(failure)?;
        let published = published
            .map(|v| serde_json::from_str::<Value>(&v))
            .transpose()
            .map_err(failure)?
            .unwrap_or(Value::Null);
        Ok(
            serde_json::json!({"dirtyBytes":dirty,"dirtySince":since,"generation":generation,"published":published}),
        )
    }
}
pub(super) fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}
