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
const LEGACY_MAGIC: &[u8; 8] = b"LEOJNL02";
const MAGIC: &[u8; 8] = b"LEOJNL03";

#[cfg(test)]
pub(super) fn crash_point(directory: &Path, phase: &str) {
    if std::env::var_os("LEO_JOURNAL_CRASH_DIR").as_deref() == Some(directory.as_os_str())
        && std::env::var("LEO_JOURNAL_CRASH_PHASE").is_ok_and(|p| p == phase)
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
enum Location {
    Segment {
        file: Arc<File>,
        position: u64,
        header: [u8; HEADER],
    },
    Legacy {
        db: Arc<Mutex<Connection>>,
        checksum: String,
    },
}

#[derive(Clone)]
pub(super) struct Record {
    pub sequence: i64,
    pub generation: i64,
    pub start: u64,
    pub end: u64,
    written_at: i64,
    location: Location,
}

impl Record {
    pub fn read(&self, size: u64) -> io::Result<Vec<u8>> {
        match &self.location {
            Location::Segment {
                file,
                position,
                header,
            } => {
                let mut actual = [0; HEADER];
                file.read_exact_at(&mut actual, *position)?;
                let mut data = vec![0; (self.end - self.start) as usize];
                file.read_exact_at(&mut data, position + HEADER as u64)?;
                if &actual != header
                    || digest(&actual[..48], &[]) != actual[48..80]
                    || digest(&actual[..80], &data) != actual[80..]
                {
                    return Err(failure("Local journal integrity check failed"));
                }
                Ok(data)
            }
            Location::Legacy { db, checksum } => {
                let db = db.lock().map_err(failure)?;
                db.query_row(
                    "SELECT start,data,checksum,seq,generation,end FROM writes WHERE seq=?1",
                    [self.sequence],
                    |row| Ok((LazyDisk::record(row, size), row.get::<_, String>(2)?)),
                )
                .map_err(failure)
                .and_then(|(record, actual)| {
                    let (start, bytes) = record?;
                    if start != self.start || &actual != checksum {
                        return Err(failure("Legacy journal changed"));
                    }
                    Ok(bytes)
                })
            }
        }
    }

    pub fn cache_key(&self) -> String {
        format!(
            "write:{}:{}:{}:{}",
            self.sequence, self.generation, self.start, self.end
        )
    }
}

pub(super) fn digest(header: &[u8], data: &[u8]) -> [u8; 32] {
    if &header[..8] == MAGIC {
        let mut hasher = blake3::Hasher::new();
        hasher.update(header);
        hasher.update(data);
        return *hasher.finalize().as_bytes();
    }
    let mut context = aws_lc_rs::digest::Context::new(&aws_lc_rs::digest::SHA256);
    context.update(header);
    context.update(data);
    context.finish().as_ref().try_into().unwrap()
}

fn integer(header: &[u8], offset: usize) -> i64 {
    i64::from_le_bytes(header[offset..offset + 8].try_into().unwrap())
}

fn legacy_guard(directory: &Path) -> io::Result<()> {
    use std::io::Write;
    const GUARD: &[u8] = b"LEO segmented journal v2; upgrade required\n";
    let path = directory.join("journal.sqlite");
    if std::fs::metadata(&path).is_ok_and(|m| m.len() == GUARD.len() as u64)
        && std::fs::read(&path).is_ok_and(|bytes| bytes == GUARD)
    {
        return Ok(());
    }
    let mut guard = tempfile::NamedTempFile::new_in(directory)?;
    guard.write_all(GUARD)?;
    guard.as_file().sync_all()?;
    guard
        .persist(directory.join("journal.sqlite"))
        .map_err(failure)?;
    File::open(directory)?.sync_all()
}

struct Segment {
    name: String,
    generation: i64,
    file: Arc<File>,
    length: u64,
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
    legacy_through: i64,
    dirty_bytes: u64,
    dirty_since: Option<i64>,
    journal_bytes: u64,
}

impl Journal {
    pub fn open(directory: &Path, db: &Connection, size: u64) -> io::Result<Self> {
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
        let legacy_through = db
            .query_row(
                "SELECT value FROM settings WHERE key='legacyThrough'",
                [],
                |r| r.get::<_, String>(0),
            )
            .optional()
            .map_err(failure)?
            .map(|v| v.parse::<i64>().map_err(failure))
            .transpose()?
            .unwrap_or(-1);
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
            legacy_through,
            dirty_bytes: 0,
            dirty_since: None,
            journal_bytes: 0,
        };
        if legacy_through > cutoff {
            let path = directory.join("journal.sqlite");
            if !path.exists() {
                return Err(failure("Legacy journal is missing"));
            }
            let legacy = Arc::new(Mutex::new(
                Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
                    .map_err(failure)?,
            ));
            {
                let connection = legacy.lock().map_err(failure)?;
                let mut statement = connection
                    .prepare(
                        "SELECT start,data,checksum,seq,w.generation,end,COALESCE(e.written_at,0) \
                    FROM writes w LEFT JOIN epochs e ON e.generation=w.generation \
                    WHERE w.generation>?1 ORDER BY seq",
                    )
                    .map_err(failure)?;
                let mut rows = statement.query([cutoff]).map_err(failure)?;
                while let Some(row) = rows.next().map_err(failure)? {
                    let (start, bytes) = LazyDisk::record(row, size)?;
                    let sequence = row.get::<_, i64>(3).map_err(failure)?;
                    let generation = row.get::<_, i64>(4).map_err(failure)?;
                    if sequence >= next || generation > journal.generation {
                        return Err(failure("Invalid legacy journal ordering"));
                    }
                    journal.insert(Record {
                        sequence,
                        generation,
                        start,
                        end: start + bytes.len() as u64,
                        written_at: row.get(6).map_err(failure)?,
                        location: Location::Legacy {
                            db: legacy.clone(),
                            checksum: row.get(2).map_err(failure)?,
                        },
                    });
                }
            }
        }
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
                if (&header[..8] != MAGIC && &header[..8] != LEGACY_MAGIC)
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
                let record = Record {
                    sequence,
                    generation: *generation,
                    start: start as u64,
                    end: start as u64 + count as u64,
                    written_at,
                    location: Location::Segment {
                        file: file.clone(),
                        position,
                        header,
                    },
                };
                record.read(size)?;
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
                file.sync_all()?;
            }
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
        journal.reclaim_legacy(directory, cutoff)?;
        File::open(directory)?.sync_all()?;
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
    ) -> io::Result<()> {
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
        let write = (|| {
            segment.file.write_all_at(&header, position)?;
            segment.file.write_all_at(data, position + HEADER as u64)?;
            segment.file.sync_data()
        })();
        if let Err(error) = write {
            self.failed = true;
            return Err(error);
        }
        segment.length += HEADER as u64 + data.len() as u64;
        self.journal_bytes += HEADER as u64 + data.len() as u64;
        let record = Record {
            sequence: self.next,
            generation: self.generation,
            start: offset,
            end: offset + data.len() as u64,
            written_at,
            location: Location::Segment {
                file: segment.file.clone(),
                position,
                header,
            },
        };
        self.insert(record);
        self.next = next;
        Ok(())
    }

    pub fn sealed(&mut self, generation: i64) {
        self.generation = generation;
        self.active = None;
    }

    pub fn sync(&self) -> io::Result<()> {
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
        if self.legacy_through >= 0 && generation >= self.legacy_through {
            for suffix in ["", "-wal", "-shm"] {
                retired.push(PathBuf::from(format!("journal.sqlite{suffix}")));
            }
        }
        retired
    }

    fn reclaim_legacy(&self, directory: &Path, cutoff: i64) -> io::Result<()> {
        if self.legacy_through < 0 || cutoff < self.legacy_through {
            return Ok(());
        }
        legacy_guard(directory)?;
        for suffix in ["-wal", "-shm"] {
            match std::fs::remove_file(directory.join(format!("journal.sqlite{suffix}"))) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    pub fn reclaim(directory: &Path, retired: &[PathBuf]) -> io::Result<()> {
        for name in retired {
            if name == Path::new("journal.sqlite") {
                legacy_guard(directory)?;
                continue;
            }
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
    let legacy = directory.join("journal.sqlite");
    if initial.is_some() && (path.exists() || legacy.exists()) {
        return Err(failure("Disk already exists"));
    }
    if !path.exists() {
        if initial.is_none() && !legacy.exists() {
            return Err(failure("Disk journal is missing"));
        }
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
        if let Some((manifest, generation)) = initial {
            db.execute(
                "INSERT INTO state VALUES (1,?1,?2,1)",
                params![manifest.to_string(), generation],
            )
            .map_err(failure)?;
        } else {
            db.execute(
                "ATTACH DATABASE ?1 AS legacy",
                [legacy.to_string_lossy().as_ref()],
            )
            .map_err(failure)?;
            db.execute_batch("INSERT INTO state SELECT * FROM legacy.state;
                INSERT INTO sealed SELECT * FROM legacy.sealed;
                INSERT INTO settings SELECT * FROM legacy.settings;
                INSERT INTO settings VALUES ('legacyThrough',(SELECT COALESCE(MAX(generation),0) FROM legacy.writes));").map_err(failure)?;
        }
        db.execute_batch("COMMIT").map_err(failure)?;
        db.close().map_err(|(_, e)| failure(e))?;
        File::open(&temporary)?.sync_all()?;
        std::fs::rename(&temporary, &path)?;
        File::open(directory)?.sync_all()?;
        if initial.is_some() {
            legacy_guard(directory)?;
        }
    }
    let db = Connection::open(path).map_err(failure)?;
    if !legacy.exists() {
        legacy_guard(directory)?;
    }
    db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;")
        .map_err(failure)?;
    // A legacy application must fail closed instead of serving stale bytes after
    // a downgrade. Copy counters durably first, then invalidate the old writer.
    if db
        .query_row(
            "SELECT value FROM settings WHERE key='legacyThrough'",
            [],
            |r| r.get::<_, String>(0),
        )
        .optional()
        .map_err(failure)?
        .is_some()
        && legacy.exists()
    {
        use std::io::Read;
        let mut header = [0; 16];
        let length = File::open(&legacy)?.read(&mut header)?;
        if length == 16 && &header == b"SQLite format 3\0" {
            let old =
                Connection::open_with_flags(&legacy, rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE)
                    .map_err(failure)?;
            old.execute_batch("PRAGMA synchronous=FULL; UPDATE state SET generation=0 WHERE id=1 AND generation<>0;").map_err(failure)?;
        }
    }
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
