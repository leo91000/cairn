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
    journal: Mutex<journal::Journal>,
    accounting: Mutex<Value>,
    base: Mutex<Arc<Value>>,
    source: Arc<dyn BlockSource>,
    cache: Mutex<()>,
    fetching: Mutex<std::collections::HashMap<String, std::sync::Weak<Mutex<()>>>>,
    memory: Mutex<std::collections::VecDeque<(String, Arc<Vec<u8>>)>>,
    publication: RwLock<()>,
    _lock: File,
    metrics: super::metrics::Metrics,
}

impl Drop for LazyDisk {
    fn drop(&mut self) {
        // A concurrently forked child can briefly inherit the open description
        // before exec closes it. Release ownership explicitly when the disk dies.
        let _ = self._lock.unlock();
    }
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
        use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
        let timing = crate::performance::Operation::new(
            "journal_open",
            Self::identity(directory),
            "integrity_scan",
        );
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
        let db = journal::metadata(directory, initial)?;
        let manifest: String = db
            .query_row("SELECT manifest FROM state WHERE id=1", [], |row| {
                row.get(0)
            })
            .map_err(failure)?;
        let manifest: Value = serde_json::from_str(&manifest).map_err(failure)?;
        let size = validate(&manifest)?;
        let journal = journal::Journal::open(directory, &db, size)?;
        let mut accounting = journal.stats();
        accounting["published"] = journal::published(&db)?;
        timing.finish();
        Ok(Self {
            directory: directory.to_owned(),
            size,
            db: Mutex::new(db),
            journal: Mutex::new(journal),
            accounting: Mutex::new(accounting),
            base: Mutex::new(Arc::new(manifest)),
            source,
            cache: Mutex::new(()),
            fetching: Mutex::default(),
            memory: Mutex::default(),
            publication: RwLock::new(()),
            _lock: lock,
            metrics: super::metrics::Metrics::default(),
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

    fn cached_record(&self, record: &journal::Record) -> io::Result<(u64, Arc<Vec<u8>>)> {
        let key = record.cache_key();
        if let Some(bytes) = self.cached(&key)? {
            return Ok((record.start, bytes));
        }
        Ok((record.start, self.remember(&key, record.read(self.size)?)?))
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
        // Concurrent FUSE readers and snapshot reconstruction may need the same
        // cold block. Hold only its lock across I/O; unrelated blocks stay parallel.
        let fetching = {
            let mut pending = self.fetching.lock().map_err(failure)?;
            if let Some(lock) = pending.get(hash).and_then(std::sync::Weak::upgrade) {
                lock
            } else {
                pending.retain(|_, lock| lock.strong_count() > 0);
                let lock = Arc::new(Mutex::new(()));
                pending.insert(hash.to_owned(), Arc::downgrade(&lock));
                lock
            }
        };
        let _fetching = fetching.lock().map_err(failure)?;
        if let Some(bytes) = self.cached(hash)? {
            self.metrics
                .coalesced_reads
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
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
                    .filter_map(Result::ok)
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
            let records = self.journal.lock().map_err(failure)?.overlapping(
                generation,
                offset,
                offset + bytes.len() as u64,
            );
            for record in records {
                self.metrics
                    .journal_rows
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let (start, data) = self.cached_record(&record)?;
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
mod journal;
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
        let mut journal = self.journal.lock().map_err(failure)?;
        if let Err(error) = journal.append(&self.directory, &self.db, offset, bytes) {
            journal.fail();
            return Err(error);
        }
        self.update_accounting(&journal)?;
        sample.finish(bytes.len());
        Ok(())
    }

    fn sync(&self) -> io::Result<()> {
        let sample = self.metrics.syncs.start();
        // Every acknowledged append is already synced. Serialize with an append
        // and propagate a prior sync failure instead of checkpointing payloads.
        let journal = self.journal.lock().map_err(failure)?;
        journal.sync()?;
        File::open(&self.directory)?.sync_all()?;
        sample.finish(0);
        Ok(())
    }
}

#[cfg(test)]
mod tests;
