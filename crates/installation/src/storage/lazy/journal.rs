//! Payloads are durable append frames. SQLite owns only publication metadata.
//! No acknowledged payload is reclaimed before the durable publication cutoff.
use super::*;
use rusqlite::OptionalExtension;
use std::{
    collections::BTreeMap,
    os::unix::fs::{FileExt, OpenOptionsExt},
};

const HEADER: usize = 112;
const SEGMENT_BYTES: u64 = 64 * 1024 * 1024;
const MAGIC: &[u8; 8] = b"CAIRNJ03";

#[cfg(test)]
pub(super) fn crash_point(directory: &Path, phase: &str) {
    if std::env::var_os("CAIRN_JOURNAL_CRASH_DIR").as_deref() == Some(directory.as_os_str())
        && std::env::var("CAIRN_JOURNAL_CRASH_PHASE").is_ok_and(|p| p == phase)
    {
        use std::io::Write;
        println!("JOURNAL_CRASH_READY");
        std::io::stdout().flush().unwrap();
        loop {
            std::thread::park();
        }
    }
}

#[derive(Clone)]
pub(super) struct Record {
    pub sequence: i64,
    pub generation: i64,
    pub start: u64,
    pub end: u64,
    zero: bool,
    written_at: i64,
    file: Arc<File>,
    position: u64,
    header: [u8; HEADER],
}

impl Record {
    pub fn read(&self) -> io::Result<Vec<u8>> {
        let sample = crate::performance::publication::start(
            crate::performance::publication::Stage::NodeLocalRead,
        );
        let mut actual = [0; HEADER];
        self.file.read_exact_at(&mut actual, self.position)?;
        let mut data = vec![0; (self.end - self.start) as usize];
        self.file
            .read_exact_at(&mut data, self.position + HEADER as u64)?;
        sample.finish(actual.len() + data.len());
        if actual != self.header
            || digest(&actual[..48], &[]) != actual[48..80]
            || digest(&actual[..80], &data) != actual[80..]
        {
            return Err(failure("Local journal integrity check failed"));
        }
        Ok(data)
    }

    pub fn cache_key(&self) -> String {
        format!(
            "write:{}:{}:{}:{}",
            self.sequence, self.generation, self.start, self.end
        )
    }
}

pub(super) fn digest(header: &[u8], data: &[u8]) -> [u8; 32] {
    let sample =
        crate::performance::publication::start(crate::performance::publication::Stage::JournalHash);
    let mut hasher = blake3::Hasher::new();
    hasher.update(header);
    hasher.update(data);
    let digest = *hasher.finalize().as_bytes();
    sample.finish(header.len() + data.len());
    digest
}

fn integer(header: &[u8], offset: usize) -> i64 {
    i64::from_le_bytes(header[offset..offset + 8].try_into().unwrap())
}

struct Segment {
    name: String,
    generation: i64,
    file: Arc<File>,
    length: u64,
}

/// A stable prefix captured under the journal lock. Appends may continue while
/// these files are synced; only this prefix is acknowledged by the barrier.
pub(super) struct CommitBatch {
    through: i64,
    pub frames: u64,
    files: Vec<Arc<File>>,
}

impl CommitBatch {
    pub fn sync(&self) -> io::Result<()> {
        for file in &self.files {
            file.sync_data()?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod commit_tests {
    use super::*;

    struct Empty;

    impl BlockSource for Empty {
        fn fetch(&self, _: &str) -> io::Result<Vec<u8>> {
            Err(failure("Unexpected block fetch"))
        }
    }

    #[test]
    fn captured_prefix_keeps_later_appends_pending_across_segment_rotation() {
        let root = tempfile::tempdir().unwrap();
        let manifest = serde_json::json!({
            "version": 1,
            "size": 4096,
            "blockSize": BLOCK,
            "blocks": [{"offset": 0, "size": 4096, "hash": null}]
        });
        let disk = LazyDisk::create(root.path(), &manifest, Arc::new(Empty)).unwrap();
        let mut journal = disk.journal.lock().unwrap();
        let first = journal.append(root.path(), &disk.db, 0, b"first").unwrap();
        let batch = journal.commit_batch(first).unwrap().unwrap();
        assert_eq!(batch.frames, 1);
        let second = journal.append(root.path(), &disk.db, 8, b"second").unwrap();
        journal.active = None;
        let third = journal.append(root.path(), &disk.db, 16, b"third").unwrap();
        batch.sync().unwrap();
        journal.committed(&batch).unwrap();
        assert_eq!(journal.pending.len(), 2);
        assert!(journal.sync().is_err());
        assert!(journal.commit_batch(first).unwrap().is_none());
        let batch = journal.commit_batch(second).unwrap().unwrap();
        assert_eq!(batch.files.len(), 2);
        assert_eq!(batch.frames, 2);
        batch.sync().unwrap();
        journal.committed(&batch).unwrap();
        assert!(journal.commit_batch(third).unwrap().is_none());
        journal.sync().unwrap();
        drop(journal);
        drop(disk);
        let disk = LazyDisk::open(root.path(), Arc::new(Empty)).unwrap();
        let mut bytes = [0; 21];
        disk.read_at(0, &mut bytes).unwrap();
        assert_eq!(&bytes[..5], b"first");
        assert_eq!(&bytes[8..14], b"second");
        assert_eq!(&bytes[16..], b"third");
    }
}

pub(super) struct Journal {
    records: BTreeMap<i64, Record>,
    blocks: BTreeMap<u64, BTreeMap<i64, Record>>,
    latest: std::collections::HashMap<(i64, u64, u64), i64>,
    epochs: BTreeMap<i64, (u64, i64)>,
    segments: Vec<Segment>,
    active: Option<usize>,
    pub generation: i64,
    pub next: i64,
    failed: bool,
    durable_through: i64,
    pending: BTreeMap<String, (Arc<File>, i64)>,
    dirty_bytes: u64,
    dirty_since: Option<i64>,
    journal_bytes: u64,
}

impl Journal {
    pub fn open(directory: &Path, db: &Connection, size: u64) -> io::Result<Self> {
        Self::recover(directory, db, size, false)
    }

    /// Shared prepared bases must be checked before they are offered to a
    /// conversation, including payloads that a normal lazy open does not read.
    pub fn open_verified(directory: &Path, db: &Connection, size: u64) -> io::Result<Self> {
        Self::recover(directory, db, size, true)
    }

    fn recover(
        directory: &Path,
        db: &Connection,
        size: u64,
        verify_payloads: bool,
    ) -> io::Result<Self> {
        let (generation, next): (i64, i64) = db
            .query_row(
                "SELECT generation,next_sequence FROM state WHERE id=1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map_err(failure)?;
        if generation < 1 || next < 1 {
            return Err(failure("Invalid journal counters"));
        }
        let cutoff = published(db)?["generation"].as_i64().unwrap_or(0);
        if cutoff < 0 || cutoff >= generation {
            return Err(failure("Invalid publication cutoff"));
        }
        let mut journal = Self {
            records: BTreeMap::new(),
            blocks: BTreeMap::new(),
            latest: std::collections::HashMap::new(),
            epochs: BTreeMap::new(),
            segments: Vec::new(),
            active: None,
            generation,
            next,
            failed: false,
            durable_through: next - 1,
            pending: BTreeMap::new(),
            dirty_bytes: 0,
            dirty_since: None,
            journal_bytes: 0,
        };
        let mut statement = db
            .prepare("SELECT name,generation,first_sequence FROM segments ORDER BY first_sequence")
            .map_err(failure)?;
        let descriptors = statement
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, i64>(2)?,
                ))
            })
            .map_err(failure)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(failure)?;
        let mut previous = journal
            .records
            .values()
            .map(|r| r.sequence)
            .max()
            .unwrap_or(0);
        for (index, (name, generation, first)) in descriptors.iter().enumerate() {
            if *generation <= cutoff
                || *generation > journal.generation
                || *first < 1
                || name != &format!("payload-{generation}-{first}.segment")
            {
                return Err(failure("Invalid journal segment descriptor"));
            }
            let file = Arc::new(
                std::fs::OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(directory.join(name))?,
            );
            let length = file.metadata()?.len();
            let tail = index + 1 == descriptors.len() && *generation == journal.generation;
            let mut position = 0;
            while position < length {
                if length - position < HEADER as u64 {
                    if !tail {
                        return Err(failure("Truncated sealed journal"));
                    }
                    break;
                }
                let mut header = [0; HEADER];
                file.read_exact_at(&mut header, position)?;
                // Validate the length/location independently before treating a
                // short final payload as an interrupted, unacknowledged append.
                if digest(&header[..48], &[]) != header[48..80] {
                    return Err(failure("Invalid journal header checksum"));
                }
                let sequence = integer(&header, 8);
                let stored_generation = integer(&header, 16);
                let start = integer(&header, 24);
                let count = integer(&header, 32);
                let written_at = integer(&header, 40);
                if &header[..8] != MAGIC
                    || sequence < *first
                    || sequence <= previous
                    || (position == 0 && sequence != *first)
                    || (position > 0 && sequence != previous + 1)
                    || stored_generation != *generation
                    || start < 0
                    || count <= 0
                    || count > MAX_IO as i64
                    || (start as u64).saturating_add(count as u64) > size
                    || written_at < 0
                {
                    return Err(failure("Invalid journal frame"));
                }
                if length - position - (HEADER as u64) < count as u64 {
                    if !tail {
                        return Err(failure("Truncated sealed journal"));
                    }
                    break;
                }
                let mut record = Record {
                    sequence,
                    generation: *generation,
                    start: start as u64,
                    end: start as u64 + count as u64,
                    zero: false,
                    written_at,
                    file: file.clone(),
                    position,
                    header,
                };
                // The authenticated header and file length suffice to recover
                // this frame's location. Record::read authenticates its payload
                // before serving or publishing bytes. Re-reading obsolete
                // payloads here makes every status query scan the whole backlog.
                // Recovered frames carry no trusted zero-range hint.
                if verify_payloads {
                    record.zero = record.read()?.iter().all(|byte| *byte == 0);
                }
                journal.insert(record);
                previous = sequence;
                journal.next = journal.next.max(
                    sequence
                        .checked_add(1)
                        .ok_or_else(|| failure("Journal sequence exhausted"))?,
                );
                position += HEADER as u64 + count as u64;
            }
            if position != length {
                file.set_len(position)?;
            }
            // Recovery can discover complete, unacknowledged frames surviving
            // a process crash in the page cache. Make them durable before any
            // subsequent seal or acknowledgement relies on the recovered prefix.
            file.sync_all()?;
            journal.segments.push(Segment {
                name: name.clone(),
                generation: *generation,
                file,
                length: position,
            });
            journal.journal_bytes += position;
            if tail {
                journal.active = Some(index);
            }
        }
        // Descriptor removal is committed before unlink. Orphans cannot contain
        // acknowledged writes: a new descriptor is durable before its first append.
        let needed: std::collections::HashSet<_> =
            journal.segments.iter().map(|s| s.name.as_str()).collect();
        for entry in std::fs::read_dir(directory)? {
            let entry = entry?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with("payload-")
                && name.ends_with(".segment")
                && !needed.contains(name.as_ref())
            {
                std::fs::remove_file(entry.path())?;
            }
        }
        File::open(directory)?.sync_all()?;
        journal.durable_through = journal.next - 1;
        Ok(journal)
    }

    fn insert(&mut self, record: Record) {
        let epoch = self
            .epochs
            .entry(record.generation)
            .or_insert((0, record.written_at));
        epoch.0 += record.end - record.start;
        epoch.1 = epoch.1.min(record.written_at);
        self.dirty_bytes += record.end - record.start;
        self.dirty_since = Some(
            self.dirty_since
                .map_or(record.written_at, |at| at.min(record.written_at)),
        );
        // Within one generation, an exact range overwrite makes its earlier
        // index entry unnecessary. Keep physical-byte/age accounting in epochs
        // so repeated hot writes neither grow the index nor hide backup lag.
        if let Some(previous) = self.latest.insert(
            (record.generation, record.start, record.end),
            record.sequence,
        ) {
            self.records.remove(&previous);
            for block in record.start / BLOCK..record.end.div_ceil(BLOCK) {
                if let Some(index) = self.blocks.get_mut(&block) {
                    index.remove(&previous);
                }
            }
        }
        for block in record.start / BLOCK..record.end.div_ceil(BLOCK) {
            self.blocks
                .entry(block)
                .or_default()
                .insert(record.sequence, record.clone());
        }
        self.records.insert(record.sequence, record);
    }

    pub fn overlapping(&self, generation: i64, start: u64, end: u64) -> Vec<Record> {
        // Merge at most three block-local sequence indexes. Stop once newer
        // records cover the request instead of sorting every historical overlap.
        let mut indexes = (start / BLOCK..end.div_ceil(BLOCK))
            .filter_map(|block| self.blocks.get(&block))
            .map(|block| block.values().rev().peekable())
            .collect::<Vec<_>>();
        let mut records = Vec::new();
        let mut missing = vec![(start, end)];
        let mut previous = None;
        loop {
            let newest = indexes
                .iter_mut()
                .enumerate()
                .filter_map(|(i, iter)| iter.peek().map(|r| (i, r.sequence)))
                .max_by_key(|(_, seq)| *seq);
            let Some((index, _)) = newest else {
                break;
            };
            let record = indexes[index].next().unwrap();
            if previous == Some(record.sequence) {
                continue;
            }
            previous = Some(record.sequence);
            if record.generation > generation || record.start >= end || record.end <= start {
                continue;
            }
            let mut next = Vec::new();
            let mut needed = false;
            for (from, to) in missing {
                if record.start >= to || record.end <= from {
                    next.push((from, to));
                    continue;
                }
                needed = true;
                if from < record.start {
                    next.push((from, record.start));
                }
                if record.end < to {
                    next.push((record.end, to));
                }
            }
            if needed {
                records.push(record.clone());
            }
            missing = next;
            if missing.is_empty() {
                break;
            }
        }
        records
    }

    pub fn changed(&self, generation: i64) -> std::collections::BTreeSet<u64> {
        self.records
            .values()
            .filter(|r| r.generation <= generation)
            .flat_map(|r| r.start / BLOCK..r.end.div_ceil(BLOCK))
            .collect()
    }

    /// Metadata-only no-op detection. Only live, known-zero writes carry this
    /// hint; recovered frames conservatively require a new write or data read.
    pub fn zeroed(&self, start: u64, end: u64, base_zero: bool) -> bool {
        let mut records = self.overlapping(i64::MAX, start, end);
        if records.iter().any(|record| !record.zero) {
            return false;
        }
        if base_zero {
            return true;
        }
        records.sort_unstable_by_key(|record| record.start);
        let mut through = start;
        for record in records {
            if record.start > through {
                return false;
            }
            through = through.max(record.end);
        }
        through >= end
    }

    pub fn stats(&self) -> Value {
        serde_json::json!({
            "dirtyBytes": self.dirty_bytes,
            "dirtySince": self.dirty_since,
            "generation": self.generation,
            "journalBytes": self.journal_bytes,
            "journalFreeBytes": 0,
            "journalFormat": "segments-v2"
        })
    }

    pub fn append(
        &mut self,
        directory: &Path,
        db: &Mutex<Connection>,
        offset: u64,
        data: &[u8],
    ) -> io::Result<i64> {
        if self.failed {
            return Err(failure("Journal needs recovery after failed append"));
        }
        let next = self
            .next
            .checked_add(1)
            .ok_or_else(|| failure("Journal sequence exhausted"))?;
        if self.active.is_none_or(|index| {
            self.segments[index].length + HEADER as u64 + data.len() as u64 > SEGMENT_BYTES
        }) {
            let name = format!("payload-{}-{}.segment", self.generation, self.next);
            let file = Arc::new(
                std::fs::OpenOptions::new()
                    .create_new(true)
                    .read(true)
                    .write(true)
                    .mode(0o600)
                    .open(directory.join(&name))?,
            );
            file.sync_all()?;
            File::open(directory)?.sync_all()?;
            // Preserve the sequence floor for empty segments as well as frames.
            let mut db = db.lock().map_err(failure)?;
            let tx = db.transaction().map_err(failure)?;
            tx.execute(
                "INSERT INTO segments VALUES (?1,?2,?3)",
                params![name, self.generation, self.next],
            )
            .map_err(failure)?;
            tx.execute("UPDATE state SET next_sequence=?1 WHERE id=1", [self.next])
                .map_err(failure)?;
            tx.commit().map_err(failure)?;
            self.segments.push(Segment {
                name,
                generation: self.generation,
                file,
                length: 0,
            });
            self.active = Some(self.segments.len() - 1);
        }
        let segment = &mut self.segments[self.active.unwrap()];
        let written_at = state::now();
        let mut header = [0; HEADER];
        header[..8].copy_from_slice(MAGIC);
        for (position, value) in [
            (8, self.next),
            (16, self.generation),
            (24, offset as i64),
            (32, data.len() as i64),
            (40, written_at),
        ] {
            header[position..position + 8].copy_from_slice(&value.to_le_bytes());
        }
        let header_checksum = digest(&header[..48], &[]);
        header[48..80].copy_from_slice(&header_checksum);
        let checksum = digest(&header[..80], data);
        header[80..].copy_from_slice(&checksum);
        let position = segment.length;
        let zero = data.iter().all(|byte| *byte == 0);
        let write = (|| {
            segment.file.write_all_at(&header, position)?;
            if zero {
                // Appends always extend the file. Sparse zeros have precisely
                // the same frame bytes/checksums as ordinary zero writes.
                // Never punch an earlier frame or reclaim before publication.
                segment
                    .file
                    .set_len(position + HEADER as u64 + data.len() as u64)
            } else {
                segment.file.write_all_at(data, position + HEADER as u64)
            }
        })();
        if let Err(error) = write {
            self.failed = true;
            return Err(error);
        }
        segment.length += HEADER as u64 + data.len() as u64;
        self.journal_bytes += HEADER as u64 + data.len() as u64;
        self.pending
            .insert(segment.name.clone(), (segment.file.clone(), self.next));
        let record = Record {
            sequence: self.next,
            generation: self.generation,
            start: offset,
            end: offset + data.len() as u64,
            zero,
            written_at,
            file: segment.file.clone(),
            position,
            header,
        };
        self.insert(record);
        let sequence = self.next;
        self.next = next;
        Ok(sequence)
    }

    pub fn commit_batch(&self, sequence: i64) -> io::Result<Option<CommitBatch>> {
        self.check_health()?;
        if sequence <= self.durable_through {
            return Ok(None);
        }
        if sequence >= self.next || self.pending.is_empty() {
            return Err(failure("Invalid journal commit sequence"));
        }
        Ok(Some(CommitBatch {
            through: self.next - 1,
            frames: (self.next - 1 - self.durable_through) as u64,
            files: self
                .pending
                .values()
                .map(|(file, _)| file.clone())
                .collect(),
        }))
    }

    pub fn committed(&mut self, batch: &CommitBatch) -> io::Result<()> {
        self.check_health()?;
        self.durable_through = batch.through;
        self.pending.retain(|_, (_, last)| *last > batch.through);
        Ok(())
    }

    pub fn sealed(&mut self, generation: i64) {
        self.generation = generation;
        self.active = None;
    }

    pub fn sync(&self) -> io::Result<()> {
        self.check_health()?;
        if !self.pending.is_empty() {
            return Err(failure("Journal still has uncommitted writes"));
        }
        Ok(())
    }

    fn check_health(&self) -> io::Result<()> {
        if self.failed {
            return Err(failure("Journal needs recovery after failed append"));
        }
        Ok(())
    }

    pub fn fail(&mut self) {
        self.failed = true;
    }

    pub fn retire(&mut self, generation: i64) -> Vec<PathBuf> {
        self.records.retain(|_, r| r.generation > generation);
        self.latest.retain(|(g, _, _), _| *g > generation);
        self.epochs.retain(|g, _| *g > generation);
        self.blocks.retain(|_, block| {
            block.retain(|_, r| r.generation > generation);
            !block.is_empty()
        });
        self.dirty_bytes = self.epochs.values().map(|(bytes, _)| *bytes).sum();
        self.dirty_since = self.epochs.values().map(|(_, at)| *at).min();
        self.active = None;
        let mut retired = Vec::new();
        self.segments.retain(|s| {
            if s.generation > generation {
                return true;
            }
            retired.push(PathBuf::from(&s.name));
            false
        });
        self.journal_bytes = self.segments.iter().map(|s| s.length).sum();
        retired
    }

    pub fn reclaim(directory: &Path, retired: &[PathBuf]) -> io::Result<()> {
        for name in retired {
            match std::fs::remove_file(directory.join(name)) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
            #[cfg(test)]
            crash_point(directory, "reclamation");
        }
        File::open(directory)?.sync_all()
    }
}

pub(super) fn published(db: &Connection) -> io::Result<Value> {
    db.query_row(
        "SELECT value FROM settings WHERE key='published'",
        [],
        |r| r.get::<_, String>(0),
    )
    .optional()
    .map_err(failure)?
    .map(|v| serde_json::from_str(&v).map_err(failure))
    .transpose()
    .map(|v| v.unwrap_or(Value::Null))
}

pub(super) fn metadata(directory: &Path, initial: Option<(&Value, i64)>) -> io::Result<Connection> {
    let path = directory.join("journal-v2.sqlite");
    if initial.is_some() && path.exists() {
        return Err(failure("Disk already exists"));
    }
    if !path.exists() {
        let (manifest, generation) = initial.ok_or_else(|| failure("Disk journal is missing"))?;
        let temporary = directory.join("journal-v2.partial");
        // A partial metadata copy is never authoritative.
        for suffix in ["", "-journal"] {
            let leftover = directory.join(format!("journal-v2.partial{suffix}"));
            if leftover.exists() {
                std::fs::remove_file(leftover)?;
            }
        }
        let db = Connection::open(&temporary).map_err(failure)?;
        db.execute_batch("PRAGMA synchronous=FULL; BEGIN IMMEDIATE;
            CREATE TABLE state (id INTEGER PRIMARY KEY CHECK(id=1),manifest TEXT NOT NULL,generation INTEGER NOT NULL,next_sequence INTEGER NOT NULL);
            CREATE TABLE sealed (generation INTEGER PRIMARY KEY,manifest TEXT);
            CREATE TABLE settings (key TEXT PRIMARY KEY,value TEXT NOT NULL);
            CREATE TABLE segments (name TEXT PRIMARY KEY,generation INTEGER NOT NULL,first_sequence INTEGER NOT NULL UNIQUE);").map_err(failure)?;
        db.execute(
            "INSERT INTO state VALUES (1,?1,?2,1)",
            params![manifest.to_string(), generation],
        )
        .map_err(failure)?;
        db.execute_batch("COMMIT").map_err(failure)?;
        db.close().map_err(|(_, e)| failure(e))?;
        File::open(&temporary)?.sync_all()?;
        std::fs::rename(&temporary, &path)?;
        File::open(directory)?.sync_all()?;
    }
    let db = Connection::open(path).map_err(failure)?;
    db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;")
        .map_err(failure)?;
    Ok(db)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Zero;

    impl BlockSource for Zero {
        fn fetch(&self, _: &str) -> io::Result<Vec<u8>> {
            Err(failure("unexpected remote read"))
        }
    }

    #[test]
    fn repeated_overwrites_bound_the_index_without_hiding_unprotected_bytes() {
        let root = tempfile::tempdir().unwrap();
        let manifest = serde_json::json!({"version":1,"size":4096,"blockSize":BLOCK,
            "blocks":[{"offset":0,"size":4096,"hash":null}]});
        let disk = LazyDisk::create(root.path(), &manifest, Arc::new(Zero)).unwrap();
        for value in 0..100 {
            disk.write_at(0, &[value; 4096]).unwrap();
        }
        assert_eq!(disk.journal.lock().unwrap().records.len(), 1);
        assert_eq!(disk.accounting().unwrap()["dirtyBytes"], 100 * 4096);
        let since = disk.accounting().unwrap()["dirtySince"].clone();
        let first = disk.seal().unwrap();
        disk.write_at(0, &[101; 4096]).unwrap();
        assert_eq!(disk.journal.lock().unwrap().records.len(), 2);
        let mut bytes = [0; 4096];
        disk.read_generation(first, 0, &mut bytes).unwrap();
        assert_eq!(bytes, [99; 4096]);
        disk.read_at(0, &mut bytes).unwrap();
        assert_eq!(bytes, [101; 4096]);
        assert_eq!(disk.accounting().unwrap()["dirtySince"], since);
        drop(disk);
        let disk = LazyDisk::open(root.path(), Arc::new(Zero)).unwrap();
        assert_eq!(disk.journal.lock().unwrap().records.len(), 2);
        assert_eq!(disk.accounting().unwrap()["dirtyBytes"], 101 * 4096);
        disk.read_generation(first, 0, &mut bytes).unwrap();
        assert_eq!(bytes, [99; 4096]);
    }
}
