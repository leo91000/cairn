//! Durable connection context and accounting independent of the mounted view.
use super::*;

pub(super) fn encode_context(value: &Value) -> io::Result<String> {
    let encoded = value.to_string();
    if encoded.len() > 16384 {
        return Err(failure("Disk connection context too large"));
    }
    Ok(encoded)
}

fn stored_manifest(directory: &Path, timeout: std::time::Duration) -> io::Result<Value> {
    let path = if directory.join("journal-v2.sqlite").exists() {
        directory.join("journal-v2.sqlite")
    } else {
        directory.join("journal.sqlite")
    };
    let db = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(failure)?;
    db.busy_timeout(timeout).map_err(failure)?;
    let encoded: String = db
        .query_row("SELECT manifest FROM state WHERE id=1", [], |row| {
            row.get(0)
        })
        .map_err(failure)?;
    let manifest: Value = serde_json::from_str(&encoded).map_err(failure)?;
    validate(&manifest)?;
    Ok(manifest)
}

impl LazyDisk {
    /// Bootstrap only needs geometry when no resize/recovery is pending. The
    /// subsequent mounted Volume still performs the complete integrity scan.
    pub(crate) fn geometry(directory: &Path) -> io::Result<(u64, super::super::layout::Layout)> {
        let manifest = stored_manifest(directory, std::time::Duration::from_secs(5))?;
        Ok((
            manifest["size"].as_u64().unwrap(),
            super::super::layout::Layout::from_manifest(&manifest)?,
        ))
    }

    /// Conservative local backing: only exclusively allocated decoded cache
    /// objects referenced by the current base. Journal frames and SQLite are
    /// deliberately withheld; old append frames are not current backing.
    pub(crate) fn cached_backing_bytes(directory: &Path) -> io::Result<u64> {
        use std::os::unix::fs::MetadataExt;
        let cache = directory.join("cache");
        let metadata = match std::fs::symlink_metadata(&cache) {
            Ok(metadata) if metadata.is_dir() => metadata,
            _ => return Ok(0),
        };
        if metadata.file_type().is_symlink() {
            return Ok(0);
        }
        // Withhold uncertain credit rather than delaying a lease heartbeat.
        let manifest = stored_manifest(directory, std::time::Duration::ZERO)?;
        let referenced = manifest["blocks"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|block| {
                block["hash"]
                    .as_str()
                    .map(|hash| (hash, block["size"].as_u64().unwrap()))
            })
            .collect::<std::collections::HashMap<_, _>>();
        let mut bytes = 0u64;
        for entry in std::fs::read_dir(cache)? {
            let entry = entry?;
            let name = entry.file_name();
            let Some(size) = name.to_str().and_then(|name| referenced.get(name)) else {
                continue;
            };
            let Ok(metadata) = std::fs::symlink_metadata(entry.path()) else {
                continue;
            };
            if metadata.is_file() && metadata.nlink() == 1 && metadata.len() == *size {
                bytes = bytes.saturating_add(metadata.blocks().saturating_mul(512).min(*size));
            }
        }
        Ok(bytes)
    }

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
        let encoded = encode_context(value)?;
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
