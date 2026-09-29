//! Demand-paged immutable base with a durable local write journal.
use super::Disk;
use rusqlite::{Connection, params};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, RwLock},
};

const BLOCK: u64 = 4 * 1024 * 1024;
const MAX_IO: usize = 8 * 1024 * 1024;
fn failure(error: impl std::fmt::Display) -> io::Error {
    io::Error::other(error.to_string())
}
fn block_digest(bytes: &[u8]) -> String {
    hex::encode(aws_lc_rs::digest::digest(&aws_lc_rs::digest::SHA256, bytes).as_ref())
}

/// Immutable plaintext blocks. Implementations must bound transfers and propagate
/// unavailable data as an error; the mount/controller owns retry and cancellation.
pub trait BlockSource: Send + Sync {
    fn fetch(&self, hash: &str) -> io::Result<Vec<u8>>;
}

pub struct LazyDisk {
    directory: PathBuf,
    size: u64,
    db: Mutex<Connection>,
    base: Mutex<Arc<Value>>,
    source: Arc<dyn BlockSource>,
    cache: Mutex<()>,
    memory: Mutex<std::collections::VecDeque<(String, Arc<Vec<u8>>)>>,
    publication: RwLock<()>,
    _lock: File,
    metrics: super::metrics::Metrics,
}

fn validate(manifest: &Value) -> io::Result<u64> {
    let size = manifest["size"]
        .as_u64()
        .filter(|s| *s > 0 && *s <= 1 << 40)
        .ok_or_else(|| failure("Invalid disk size"))?;
    let blocks = manifest["blocks"]
        .as_array()
        .ok_or_else(|| failure("Missing disk blocks"))?;
    if manifest["version"] != 1
        || manifest["blockSize"] != BLOCK
        || blocks.len() as u64 != size.div_ceil(BLOCK)
    {
        return Err(failure("Invalid disk manifest"));
    }
    for (i, block) in blocks.iter().enumerate() {
        let offset = i as u64 * BLOCK;
        if block["offset"] != offset
            || block["size"] != (size - offset).min(BLOCK)
            || !(block["hash"].is_null()
                || block["hash"].as_str().is_some_and(|h| {
                    h.len() == 64
                        && h.bytes()
                            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                }))
        {
            return Err(failure("Invalid disk extent"));
        }
    }
    Ok(size)
}

impl LazyDisk {
    fn identity(directory: &Path) -> &str {
        directory
            .parent()
            .and_then(Path::file_name)
            .and_then(|name| name.to_str())
            .filter(|name| uuid::Uuid::parse_str(name).is_ok())
            .unwrap_or("")
    }

    fn node_state(&self) -> Option<&Path> {
        if self
            .directory
            .file_name()
            .is_some_and(|name| name == "lazy")
        {
            self.directory
                .parent()
                .and_then(Path::parent)
                .filter(|disks| disks.file_name().is_some_and(|name| name == "disks"))
                .and_then(Path::parent)
        } else {
            None
        }
    }

    pub fn create(
        directory: &Path,
        manifest: &Value,
        source: Arc<dyn BlockSource>,
    ) -> io::Result<Self> {
        Self::create_at_generation(directory, manifest, source, 1)
    }
    /// A rebuilt disk keeps its grant and advances the same publication sequence.
    pub(crate) fn create_at_generation(
        directory: &Path,
        manifest: &Value,
        source: Arc<dyn BlockSource>,
        generation: i64,
    ) -> io::Result<Self> {
        validate(manifest)?;
        if generation < 1 || generation == i64::MAX {
            return Err(failure("Invalid initial journal generation"));
        }
        Self::connect(directory, Some((manifest, generation)), source)
    }
    pub fn open(directory: &Path, source: Arc<dyn BlockSource>) -> io::Result<Self> {
        Self::connect(directory, None, source)
    }
    fn connect(
        directory: &Path,
        initial: Option<(&Value, i64)>,
        source: Arc<dyn BlockSource>,
    ) -> io::Result<Self> {
        let timing = crate::performance::Operation::new(
            "journal_open",
            Self::identity(directory),
            "integrity_scan",
        );
        use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
        if !directory.exists() {
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(directory)?;
        }
        let lock = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .mode(0o600)
            .open(directory.join("journal.lock"))?;
        lock.try_lock().map_err(failure)?;
        let path = directory.join("journal.sqlite");
        if initial.is_some() && path.exists() {
            return Err(failure("Disk already exists"));
        }
        if initial.is_none() && !path.exists() {
            return Err(failure("Disk journal is missing"));
        }
        let db = Connection::open(&path).map_err(failure)?;
        db.execute_batch(
            "PRAGMA auto_vacuum=FULL; PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;",
        )
        .map_err(failure)?;
        if let Some((manifest, generation)) = initial {
            db.execute_batch("BEGIN IMMEDIATE;
                CREATE TABLE state (id INTEGER PRIMARY KEY CHECK(id=1), manifest TEXT NOT NULL, generation INTEGER NOT NULL, next_sequence INTEGER NOT NULL);
                CREATE TABLE writes (seq INTEGER PRIMARY KEY AUTOINCREMENT, generation INTEGER NOT NULL, start INTEGER NOT NULL, end INTEGER NOT NULL, data BLOB NOT NULL, checksum TEXT NOT NULL);
                CREATE INDEX write_ranges ON writes(end);
                COMMIT;").map_err(failure)?;
            db.execute(
                "INSERT INTO state VALUES (1, ?1, ?2, 1)",
                params![manifest.to_string(), generation],
            )
            .map_err(failure)?;
            File::open(directory)?.sync_all()?;
        }
        db.execute_batch(
            "CREATE TABLE IF NOT EXISTS sealed (generation INTEGER PRIMARY KEY, manifest TEXT);
            CREATE TABLE IF NOT EXISTS settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS epochs (generation INTEGER PRIMARY KEY, written_at INTEGER NOT NULL);
            INSERT OR IGNORE INTO epochs SELECT DISTINCT generation,0 FROM writes;",
        )
        .map_err(failure)?;
        let manifest: String = db
            .query_row("SELECT manifest FROM state WHERE id=1", [], |row| {
                row.get(0)
            })
            .map_err(failure)?;
        let manifest: Value = serde_json::from_str(&manifest).map_err(failure)?;
        let size = validate(&manifest)?;
        let (generation, next): (i64, i64) = db
            .query_row(
                "SELECT generation, next_sequence FROM state WHERE id=1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map_err(failure)?;
        if generation < 1 || next < 1 {
            return Err(failure("Invalid journal counters"));
        }
        {
            let mut statement = db
                .prepare(
                    "SELECT start, data, checksum, seq, generation, end FROM writes ORDER BY seq",
                )
                .map_err(failure)?;
            let mut rows = statement.query([]).map_err(failure)?;
            while let Some(row) = rows.next().map_err(failure)? {
                Self::record(row, size)?;
                if row.get::<_, i64>(3).map_err(failure)? >= next
                    || row.get::<_, i64>(4).map_err(failure)? > generation
                {
                    return Err(failure("Invalid journal ordering"));
                }
            }
        }
        Self::reclaim_empty_legacy_journal(&db)?;
        timing.finish();
        Ok(Self {
            directory: directory.to_owned(),
            size,
            db: Mutex::new(db),
            base: Mutex::new(Arc::new(manifest)),
            source,
            cache: Mutex::new(()),
            memory: Mutex::new(Default::default()),
            publication: RwLock::new(()),
            _lock: lock,
            metrics: Default::default(),
        })
    }
    fn record(row: &rusqlite::Row<'_>, size: u64) -> io::Result<(u64, Vec<u8>)> {
        let start: i64 = row.get(0).map_err(failure)?;
        let reference = row.get_ref(1).map_err(failure)?;
        let data = reference.as_blob().map_err(failure)?;
        let checksum: String = row.get(2).map_err(failure)?;
        let sequence: i64 = row.get(3).map_err(failure)?;
        let generation: i64 = row.get(4).map_err(failure)?;
        let end: i64 = row.get(5).map_err(failure)?;
        if start < 0
            || sequence < 1
            || generation < 1
            || data.is_empty()
            || data.len() > MAX_IO
            || end < start
            || end as u64 > size
            || end as u64 != (start as u64).saturating_add(data.len() as u64)
            || Self::checksum(sequence, generation, start as u64, data) != checksum
        {
            return Err(failure("Local journal integrity check failed"));
        }
        Ok((start as u64, data.to_vec()))
    }
    fn checksum(sequence: i64, generation: i64, offset: u64, bytes: &[u8]) -> String {
        let mut hash = Sha256::new();
        hash.update(sequence.to_le_bytes());
        hash.update(generation.to_le_bytes());
        hash.update(offset.to_le_bytes());
        hash.update((bytes.len() as u64).to_le_bytes());
        hash.update(bytes);
        hex::encode(hash.finalize())
    }
    fn check(&self, offset: u64, length: usize) -> io::Result<()> {
        if length > MAX_IO {
            return Err(failure("Disk request exceeds the I/O limit"));
        }
        super::device::range(self.size, offset, length)
    }
    fn cached(&self, key: &str) -> io::Result<Option<Arc<Vec<u8>>>> {
        let mut memory = self.memory.lock().map_err(failure)?;
        Ok(memory
            .iter()
            .position(|(stored, _)| stored == key)
            .map(|index| {
                let entry = memory.remove(index).unwrap();
                let bytes = entry.1.clone();
                memory.push_back(entry);
                bytes
            }))
    }
    fn cached_record(&self, row: &rusqlite::Row<'_>) -> io::Result<(u64, Arc<Vec<u8>>)> {
        let start: i64 = row.get(0).map_err(failure)?;
        let key = format!(
            "write:{}:{}:{start}:{}:{}",
            row.get::<_, i64>(3).map_err(failure)?,
            row.get::<_, i64>(4).map_err(failure)?,
            row.get::<_, i64>(5).map_err(failure)?,
            row.get::<_, String>(2).map_err(failure)?
        );
        if let Some(bytes) = self.cached(&key)? {
            return Ok((start as u64, bytes));
        }
        let (offset, bytes) = Self::record(row, self.size)?;
        Ok((offset, self.remember(&key, bytes)?))
    }
    fn remember(&self, hash: &str, bytes: Vec<u8>) -> io::Result<Arc<Vec<u8>>> {
        let bytes = Arc::new(bytes);
        let mut memory = self.memory.lock().map_err(failure)?;
        memory.retain(|(key, _)| key != hash);
        while memory.iter().map(|(_, v)| v.len()).sum::<usize>() + bytes.len() > 32 * 1024 * 1024 {
            memory.pop_front();
        }
        memory.push_back((hash.to_owned(), bytes.clone()));
        Ok(bytes)
    }
    fn base_block(&self, block: &Value) -> io::Result<Arc<Vec<u8>>> {
        let length = block["size"]
            .as_u64()
            .ok_or_else(|| failure("Invalid extent"))? as usize;
        let hash = block["hash"]
            .as_str()
            .ok_or_else(|| failure("Missing base block hash"))?;
        if let Some(bytes) = self.cached(hash)? {
            self.metrics
                .memory_hits
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            return Ok(bytes);
        }
        let directory = self.directory.join("cache");
        let target = directory.join(hash);
        if let Ok(file) = File::open(&target) {
            use std::io::Read;
            let mut bytes = Vec::new();
            (&file).take(BLOCK + 1).read_to_end(&mut bytes)?;
            if bytes.len() == length && block_digest(&bytes) == hash {
                self.metrics
                    .disk_hits
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let now = std::time::SystemTime::now();
                if file.set_modified(now).is_ok()
                    && let Some(state) = self.node_state()
                {
                    let _ = super::cache::touched(state, &target, now);
                }
                return self.remember(hash, bytes);
            }
            let _ = std::fs::remove_file(&target);
        }
        let fetch = self.metrics.remote.start();
        let bytes = self.source.fetch(hash)?;
        if bytes.len() != length || block_digest(&bytes) != hash {
            return Err(failure("Remote block integrity check failed"));
        }
        fetch.finish(bytes.len());
        // Cache persistence is optional: it never determines whether an acknowledged
        // write survives. Refusing a cache fill must not turn a valid read into EIO.
        let _ = (|| -> io::Result<()> {
            use std::io::Write;
            let _cache = self.cache.lock().map_err(failure)?;
            let node_reservation = if let Some(state) = self.node_state() {
                let policy = std::fs::read(state.join("storage-policy.json"))
                    .ok()
                    .and_then(|bytes| serde_json::from_slice::<super::policy::Policy>(&bytes).ok())
                    .unwrap_or_default();
                let Some(reservation) = super::cache::reserve(state, &policy, bytes.len() as u64)?
                else {
                    return Ok(());
                };
                Some(reservation)
            } else {
                None
            };
            std::fs::create_dir_all(&directory)?;
            // Standalone disks retain a small fallback cache; controller volumes
            // share the node budget and can use its available working set.
            if node_reservation.is_none() {
                let mut entries = std::fs::read_dir(&directory)?
                    .filter_map(|entry| entry.ok())
                    .filter_map(|entry| {
                        entry.metadata().ok().map(|m| {
                            (
                                entry.path(),
                                m.len(),
                                m.modified().unwrap_or(std::time::UNIX_EPOCH),
                            )
                        })
                    })
                    .collect::<Vec<_>>();
                entries.sort_by_key(|entry| entry.2);
                let mut used = entries.iter().map(|entry| entry.1).sum::<u64>();
                for (path, size, _) in entries {
                    if used.saturating_add(bytes.len() as u64) <= 32 * 1024 * 1024 {
                        break;
                    }
                    std::fs::remove_file(path)?;
                    used = used.saturating_sub(size);
                }
            }
            let mut file = tempfile::NamedTempFile::new_in(&directory)?;
            file.write_all(&bytes)?;
            file.persist_noclobber(&target).map_err(failure)?;
            if let Some(reservation) = &node_reservation {
                reservation.filled(&target, bytes.len() as u64)?;
            }
            Ok(())
        })();
        self.remember(hash, bytes)
    }
}
impl LazyDisk {
    /// Older journals enabled auto-vacuum after entering WAL mode, which left it
    /// disabled. Convert only an empty journal: never copy unsynchronized blobs
    /// or make startup proportional to the dirty working set.
    fn reclaim_empty_legacy_journal(db: &Connection) -> io::Result<()> {
        let mode: u32 = db
            .query_row("PRAGMA auto_vacuum", [], |r| r.get(0))
            .map_err(failure)?;
        if mode != 0 {
            return Ok(());
        }
        let dirty: bool = db
            .query_row("SELECT EXISTS(SELECT 1 FROM writes)", [], |r| r.get(0))
            .map_err(failure)?;
        if dirty {
            return Ok(());
        }
        let timing =
            crate::performance::Operation::new("journal_reclaim", "", "empty_legacy_journal");
        db.execute_batch("PRAGMA auto_vacuum=FULL; VACUUM; PRAGMA wal_checkpoint(TRUNCATE)")
            .map_err(failure)?;
        timing.finish();
        Ok(())
    }

    fn read_generation(&self, generation: i64, offset: u64, bytes: &mut [u8]) -> io::Result<()> {
        self.check(offset, bytes.len())?;
        if bytes.is_empty() {
            return Ok(());
        }
        // Newer writes win. Track only the ranges still needing data, so a
        // multi-megabyte read does not visit every byte once per journal row.
        let mut missing = vec![(0, bytes.len())];
        let manifest = self.base.lock().map_err(failure)?.clone();
        {
            let db = self.db.lock().map_err(failure)?;
            let end = (offset + bytes.len() as u64) as i64;
            let start = offset as i64;
            // An overlapping write ends before the read end plus MAX_IO,
            // because no journal write can exceed MAX_IO bytes. Bound both
            // ends of the indexed range instead of scanning the whole journal
            // in sequence order for every small read.
            let mut statement = db.prepare_cached("SELECT start, data, checksum, seq, generation, end FROM writes INDEXED BY write_ranges WHERE end > ?1 AND end < ?2 AND start < ?3 AND generation <= ?4 ORDER BY seq DESC").map_err(failure)?;
            let mut rows = statement
                .query(params![start, end + MAX_IO as i64, end, generation])
                .map_err(failure)?;
            while let Some(row) = rows.next().map_err(failure)? {
                self.metrics
                    .journal_rows
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let (start, data) = self.cached_record(row)?;
                let begin = (start.max(offset) - offset) as usize;
                let end = ((start + data.len() as u64).min(offset + bytes.len() as u64) - offset)
                    as usize;
                let mut next = Vec::with_capacity(missing.len() + 1);
                for (gap_start, gap_end) in missing {
                    let from = gap_start.max(begin);
                    let to = gap_end.min(end);
                    if from < to {
                        bytes[from..to].copy_from_slice(
                            &data[(offset + from as u64 - start) as usize
                                ..(offset + to as u64 - start) as usize],
                        );
                        if gap_start < from {
                            next.push((gap_start, from));
                        }
                        if to < gap_end {
                            next.push((to, gap_end));
                        }
                    } else {
                        next.push((gap_start, gap_end));
                    }
                }
                missing = next;
                if missing.is_empty() {
                    break;
                }
            }
        }
        // Never hold the journal lock across remote I/O. This read observes a
        // consistent journal prefix; later writes are independent of cache arrival.
        if missing.is_empty() {
            return Ok(());
        }
        let end = offset + bytes.len() as u64;
        let mut position = offset;
        while position < end {
            let index = position / BLOCK;
            let limit = ((index + 1) * BLOCK).min(end);
            let start = (position - offset) as usize;
            let finish = (limit - offset) as usize;
            if missing
                .iter()
                .any(|&(from, to)| from < finish && to > start)
            {
                let extent = &manifest["blocks"][index as usize];
                let block = if extent["hash"].is_null() {
                    None
                } else {
                    Some(self.base_block(extent)?)
                };
                for &(from, to) in &missing {
                    let from = from.max(start);
                    let to = to.min(finish);
                    if from < to {
                        if let Some(block) = &block {
                            let block_start = (offset + from as u64 - index * BLOCK) as usize;
                            bytes[from..to]
                                .copy_from_slice(&block[block_start..block_start + to - from]);
                        } else {
                            bytes[from..to].fill(0);
                        }
                    }
                }
            }
            position = limit;
        }
        Ok(())
    }
}
mod generations;
mod state;
impl Disk for LazyDisk {
    fn size(&self) -> u64 {
        self.size
    }
    fn read_at(&self, offset: u64, bytes: &mut [u8]) -> io::Result<()> {
        let sample = self.metrics.reads.start();
        let _publication = self.publication.read().map_err(failure)?;
        self.read_generation(i64::MAX, offset, bytes)?;
        sample.finish(bytes.len());
        Ok(())
    }
    fn write_at(&self, offset: u64, bytes: &[u8]) -> io::Result<()> {
        let sample = self.metrics.writes.start();
        self.check(offset, bytes.len())?;
        if bytes.is_empty() {
            sample.finish(0);
            return Ok(());
        }
        let mut db = self.db.lock().map_err(failure)?;
        let tx = db.transaction().map_err(failure)?;
        let (generation, sequence): (i64, i64) = tx
            .query_row(
                "SELECT generation, next_sequence FROM state WHERE id=1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map_err(failure)?;
        let next = sequence
            .checked_add(1)
            .ok_or_else(|| failure("Journal sequence exhausted"))?;
        tx.execute(
            "INSERT INTO writes(seq,generation,start,end,data,checksum) VALUES (?1,?2,?3,?4,?5,?6)",
            params![
                sequence,
                generation,
                offset as i64,
                (offset + bytes.len() as u64) as i64,
                bytes,
                Self::checksum(sequence, generation, offset, bytes)
            ],
        )
        .map_err(failure)?;
        tx.execute(
            "INSERT OR IGNORE INTO epochs VALUES (?1,?2)",
            params![generation, state::now()],
        )
        .map_err(failure)?;
        tx.execute("UPDATE state SET next_sequence=?1 WHERE id=1", [next])
            .map_err(failure)?;
        tx.commit().map_err(failure)?;
        sample.finish(bytes.len());
        Ok(())
    }
    fn sync(&self) -> io::Result<()> {
        let sample = self.metrics.syncs.start();
        let db = self.db.lock().map_err(failure)?;
        db.execute_batch("PRAGMA wal_checkpoint(FULL)")
            .map_err(failure)?;
        File::open(&self.directory)?.sync_all()?;
        sample.finish(0);
        Ok(())
    }
}
#[cfg(test)]
mod tests;
