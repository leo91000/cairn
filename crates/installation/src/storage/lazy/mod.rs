//! Demand-paged immutable base with a durable local write journal.
use super::{Disk, DiskWrite};
use rusqlite::{Connection, params};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, RwLock,
        atomic::{AtomicI64, Ordering},
    },
};

const BLOCK: u64 = 4 * 1024 * 1024;
const MAX_IO: usize = 8 * 1024 * 1024;

fn failure(error: impl std::fmt::Display) -> io::Error {
    io::Error::other(error.to_string())
}

fn block_digest(bytes: &[u8]) -> String {
    super::digest::block(bytes)
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
    commit: Mutex<()>,
    write_gate: RwLock<()>,
    #[cfg(test)]
    commit_failure: std::sync::atomic::AtomicBool,
    accounting: Mutex<Value>,
    dirty_since: AtomicI64,
    base: Mutex<Arc<Value>>,
    source: Arc<dyn BlockSource>,
    cache: Mutex<()>,
    blocks: Arc<memory::BlockCache>,
    records: Mutex<memory::BytesCache>,
    foreground: Arc<Mutex<memory::WorkingSet>>,
    publication: RwLock<()>,
    _lock: File,
    metrics: super::metrics::Metrics,
}

/// Keeps the node's bounded verified cache alive while its controller is running,
/// including intervals with no mounted conversation disks.
pub(crate) struct NodeBlockCache {
    blocks: Arc<memory::BlockCache>,
}

impl NodeBlockCache {
    pub(crate) fn new(state: &Path) -> io::Result<Self> {
        Ok(Self {
            blocks: memory::BlockCache::for_directory(state)?,
        })
    }

    pub(crate) fn resize(&self, bytes: usize) -> io::Result<()> {
        self.blocks.bytes.lock().map_err(failure)?.resize(bytes);
        Ok(())
    }
}

impl Drop for LazyDisk {
    fn drop(&mut self) {
        // A concurrently forked child can briefly inherit the open description
        // before exec closes it. Release ownership explicitly when the disk dies.
        let _ = self._lock.unlock();
    }
}

fn validate(manifest: &Value) -> io::Result<u64> {
    let layout = super::layout::Layout::from_manifest(manifest)?;
    let size = manifest["size"]
        .as_u64()
        .filter(|s| *s > 0 && *s <= 1 << 40)
        .ok_or_else(|| failure("Invalid disk size"))?;
    let blocks = manifest["blocks"]
        .as_array()
        .ok_or_else(|| failure("Missing disk blocks"))?;
    if manifest["version"] != layout.manifest_version()
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
                || block["hash"].as_str().is_some_and(super::digest::valid))
        {
            return Err(failure("Invalid disk extent"));
        }
    }
    Ok(size)
}

impl LazyDisk {
    pub fn layout(&self) -> io::Result<super::layout::Layout> {
        let manifest = self.base.lock().map_err(failure)?;
        super::layout::Layout::from_manifest(&manifest)
    }

    pub(crate) fn has_remote_base(&self) -> io::Result<bool> {
        let base = self.base.lock().map_err(failure)?;
        Ok(base["blocks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|block| !block["hash"].is_null()))
    }

    pub(crate) fn start_write_admission(&self) -> super::metrics::Sample<'_> {
        self.metrics.write_admission.start()
    }

    fn identity(directory: &Path) -> &str {
        directory
            .parent()
            .and_then(Path::file_name)
            .and_then(|name| name.to_str())
            .filter(|name| uuid::Uuid::parse_str(name).is_ok())
            .unwrap_or("")
    }

    fn node_state(&self) -> Option<&Path> {
        node_state(&self.directory)
    }

    pub(crate) fn memory_budget(&self, bytes: usize) -> io::Result<()> {
        self.blocks.bytes.lock().map_err(failure)?.resize(bytes);
        Ok(())
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
        let dirty_since = accounting["dirtySince"].as_i64().unwrap_or(-1);
        accounting["published"] = journal::published(&db)?;
        timing.finish();
        let blocks = memory::BlockCache::for_directory(directory)?;
        let foreground = blocks.working_set(directory)?;
        Ok(Self {
            directory: directory.to_owned(),
            size,
            db: Mutex::new(db),
            journal: Mutex::new(journal),
            commit: Mutex::new(()),
            write_gate: RwLock::new(()),
            #[cfg(test)]
            commit_failure: std::sync::atomic::AtomicBool::new(false),
            accounting: Mutex::new(accounting),
            dirty_since: AtomicI64::new(dirty_since),
            base: Mutex::new(Arc::new(manifest)),
            source,
            cache: Mutex::new(()),
            blocks,
            records: Mutex::new(memory::BytesCache::new(32 * 1024 * 1024)),
            foreground,
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

    fn cached_record(
        &self,
        record: &journal::Record,
        foreground: bool,
    ) -> io::Result<(u64, Arc<Vec<u8>>)> {
        let key = record.cache_key();
        if let Some(bytes) = self.records.lock().map_err(failure)?.get(&key, foreground) {
            return Ok((record.start, bytes));
        }
        let bytes = record.read(self.size)?;
        let bytes = self
            .records
            .lock()
            .map_err(failure)?
            .insert(&key, bytes, foreground);
        Ok((record.start, bytes))
    }

    fn cached_block(&self, hash: &str, foreground: bool) -> io::Result<Option<Arc<Vec<u8>>>> {
        Ok(self
            .blocks
            .bytes
            .lock()
            .map_err(failure)?
            .get(hash, foreground))
    }

    fn remember_block(
        &self,
        hash: &str,
        bytes: Vec<u8>,
        foreground: bool,
    ) -> io::Result<Arc<Vec<u8>>> {
        Ok(self
            .blocks
            .bytes
            .lock()
            .map_err(failure)?
            .insert(hash, bytes, foreground))
    }

    fn verified(&self, hash: &str, bytes: &[u8]) -> bool {
        let sample = self.metrics.verification.start();
        let valid = super::digest::matches(hash, bytes);
        if valid {
            sample.finish(bytes.len());
        }
        valid
    }

    fn base_block(&self, block: &Value, foreground: bool) -> io::Result<Arc<Vec<u8>>> {
        let length = block["size"]
            .as_u64()
            .ok_or_else(|| failure("Invalid extent"))? as usize;
        let hash = block["hash"]
            .as_str()
            .ok_or_else(|| failure("Missing base block hash"))?;
        if let Some(bytes) = self.cached_block(hash, foreground)? {
            if bytes.len() != length {
                return Err(failure("Invalid cached block extent"));
            }
            self.metrics
                .memory_hits
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            return Ok(bytes);
        }
        // Concurrent FUSE readers and snapshot reconstruction may need the same
        // cold block. Hold only its lock across I/O; unrelated blocks stay parallel.
        let fetching = self.blocks.fetching(hash, &self.source)?;
        let _fetching = fetching.lock().map_err(failure)?;
        if let Some(bytes) = self.cached_block(hash, foreground)? {
            if bytes.len() != length {
                return Err(failure("Invalid cached block extent"));
            }
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
            if bytes.len() == length && self.verified(hash, &bytes) {
                self.metrics
                    .disk_hits
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let now = std::time::SystemTime::now();
                if file.set_modified(now).is_ok()
                    && let Some(state) = self.node_state()
                {
                    let _ = super::cache::touched(state, &target, now);
                }
                return self.remember_block(hash, bytes, foreground);
            }
            let _ = std::fs::remove_file(&target);
        }
        let fetch = self.metrics.remote.start();
        let bytes = self.source.fetch(hash)?;
        if bytes.len() != length || !self.verified(hash, &bytes) {
            return Err(failure("Remote block integrity check failed"));
        }
        fetch.finish(bytes.len());
        // Cache persistence is optional: it never determines whether an acknowledged
        // write survives. Refusing a cache fill must not turn a valid read into EIO.
        let _ = self.cache_block(hash, &bytes);
        self.remember_block(hash, bytes, foreground)
    }

    /// Optional immutable bytes, bounded by the same node quota for downloads
    /// and locally reconstructed publications. Reopened entries are verified.
    fn cache_block(&self, hash: &str, bytes: &[u8]) -> io::Result<()> {
        use std::io::Write;
        let directory = self.directory.join("cache");
        let target = directory.join(hash);
        let _cache = self.cache.lock().map_err(failure)?;
        if target.exists() {
            return Ok(());
        }
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
        file.write_all(bytes)?;
        file.persist_noclobber(&target).map_err(failure)?;
        if let Some(reservation) = &node_reservation {
            reservation.filled(&target, bytes.len() as u64)?;
        }
        Ok(())
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
                let (start, data) = self.cached_record(&record, generation == i64::MAX)?;
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
                    Some(self.base_block(extent, generation == i64::MAX)?)
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

fn node_state(directory: &Path) -> Option<&Path> {
    if directory.file_name().is_some_and(|name| name == "lazy") {
        directory
            .parent()
            .and_then(Path::parent)
            .filter(|root| {
                root.file_name()
                    .is_some_and(|name| name == "disks" || name == "environments")
            })
            .and_then(Path::parent)
    } else {
        None
    }
}

mod generations;
mod journal;
mod memory;
mod state;

impl LazyDisk {
    fn append_writes(&self, writes: &[DiskWrite<'_>]) -> io::Result<()> {
        if writes.len() > 128 {
            return Err(failure("Too many disk writes in one batch"));
        }
        let mut total = 0usize;
        for write in writes {
            self.check(write.offset, write.bytes.len())?;
            total = total
                .checked_add(write.bytes.len())
                .filter(|total| *total <= MAX_IO)
                .ok_or_else(|| failure("Disk write batch exceeds its byte limit"))?;
        }
        if total == 0 {
            return Ok(());
        }
        let _write = self.write_gate.read().map_err(failure)?;
        let _publication = self.publication.read().map_err(failure)?;
        let mut journal = self.journal.lock().map_err(failure)?;
        let mut last = None;
        for write in writes {
            if write.bytes.is_empty() {
                continue;
            }
            match journal.append(&self.directory, &self.db, write.offset, write.bytes) {
                Ok(sequence) => last = Some(sequence),
                Err(error) => {
                    journal.fail();
                    return Err(error);
                }
            }
        }
        if let Err(error) = self.update_accounting(&journal) {
            journal.fail();
            return Err(error);
        }
        drop(journal);
        self.commit_write(last.expect("nonempty batch has a sequence"))
    }

    fn commit_write(&self, sequence: i64) -> io::Result<()> {
        // One caller syncs a captured prefix while other callers stage frames.
        // The next owner includes all frames that accumulated during that sync.
        // No timer, worker thread or separate queue is needed.
        let _commit = self.commit.lock().map_err(failure)?;
        let batch = self
            .journal
            .lock()
            .map_err(failure)?
            .commit_batch(sequence)?;
        let Some(batch) = batch else {
            return Ok(());
        };
        let sample = self.metrics.journal_commits.start();
        #[cfg(test)]
        journal::crash_point(&self.directory, "before_group_sync");
        #[cfg(test)]
        let sync = if self.commit_failure.load(Ordering::Relaxed) {
            Err(failure("Injected journal sync failure"))
        } else {
            batch.sync()
        };
        #[cfg(not(test))]
        let sync = batch.sync();
        if let Err(error) = sync {
            self.journal.lock().map_err(failure)?.fail();
            return Err(error);
        }
        #[cfg(test)]
        journal::crash_point(&self.directory, "after_group_sync");
        self.journal.lock().map_err(failure)?.committed(&batch)?;
        self.metrics
            .committed_frames
            .fetch_add(batch.frames, Ordering::Relaxed);
        self.metrics
            .max_commit_frames
            .fetch_max(batch.frames, Ordering::Relaxed);
        sample.finish(0);
        Ok(())
    }
}

impl Disk for LazyDisk {
    fn size(&self) -> u64 {
        self.size
    }

    fn read_at(&self, offset: u64, bytes: &mut [u8]) -> io::Result<()> {
        let sample = self.metrics.reads.start();
        let _publication = self.publication.read().map_err(failure)?;
        self.read_generation(i64::MAX, offset, bytes)?;
        let capacity = self.blocks.bytes.lock().map_err(failure)?.block_capacity();
        self.foreground
            .lock()
            .map_err(failure)?
            .record(offset, bytes.len(), capacity);
        sample.finish(bytes.len());
        Ok(())
    }

    fn write_at(&self, offset: u64, bytes: &[u8]) -> io::Result<()> {
        let sample = self.metrics.writes.start();
        self.append_writes(&[DiskWrite { offset, bytes }])?;
        sample.finish(bytes.len());
        Ok(())
    }

    fn write_batch(&self, writes: &[DiskWrite<'_>]) -> io::Result<()> {
        if writes.len() == 1 {
            return self.write_at(writes[0].offset, writes[0].bytes);
        }
        if writes.len() > 128 {
            return Err(failure("Too many disk writes in one batch"));
        }
        let samples: Vec<_> = writes.iter().map(|_| self.metrics.writes.start()).collect();
        self.append_writes(writes)?;
        for (sample, write) in samples.into_iter().zip(writes) {
            sample.finish(write.bytes.len());
        }
        Ok(())
    }

    fn sync(&self) -> io::Result<()> {
        let sample = self.metrics.syncs.start();
        // Drain writes through their durable acknowledgements before the fence.
        let _write = self.write_gate.write().map_err(failure)?;
        let journal = self.journal.lock().map_err(failure)?;
        journal.sync()?;
        File::open(&self.directory)?.sync_all()?;
        sample.finish(0);
        Ok(())
    }
}

#[cfg(test)]
mod tests;
