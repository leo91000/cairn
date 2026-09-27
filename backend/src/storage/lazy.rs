//! Demand-paged immutable base with a durable local write journal.
use super::Disk;
use rusqlite::{Connection, params};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

const BLOCK: u64 = 4 * 1024 * 1024;
const MAX_IO: usize = 8 * 1024 * 1024;
fn failure(error: impl std::fmt::Display) -> io::Error {
    io::Error::other(error.to_string())
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
    source: Arc<dyn BlockSource>,
    cache: Mutex<()>,
    _lock: File,
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
    pub fn create(
        directory: &Path,
        manifest: &Value,
        source: Arc<dyn BlockSource>,
    ) -> io::Result<Self> {
        validate(manifest)?;
        Self::connect(directory, Some(manifest), source)
    }
    pub fn open(directory: &Path, source: Arc<dyn BlockSource>) -> io::Result<Self> {
        Self::connect(directory, None, source)
    }
    fn connect(
        directory: &Path,
        initial: Option<&Value>,
        source: Arc<dyn BlockSource>,
    ) -> io::Result<Self> {
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
            "PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA auto_vacuum=INCREMENTAL;",
        )
        .map_err(failure)?;
        if let Some(manifest) = initial {
            db.execute_batch("BEGIN IMMEDIATE;
                CREATE TABLE state (id INTEGER PRIMARY KEY CHECK(id=1), manifest TEXT NOT NULL, generation INTEGER NOT NULL, next_sequence INTEGER NOT NULL);
                CREATE TABLE writes (seq INTEGER PRIMARY KEY AUTOINCREMENT, generation INTEGER NOT NULL, start INTEGER NOT NULL, end INTEGER NOT NULL, data BLOB NOT NULL, checksum TEXT NOT NULL);
                CREATE INDEX write_ranges ON writes(end);
                COMMIT;").map_err(failure)?;
            db.execute(
                "INSERT INTO state VALUES (1, ?1, 1, 1)",
                [manifest.to_string()],
            )
            .map_err(failure)?;
            File::open(directory)?.sync_all()?;
        }
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
        Ok(Self {
            directory: directory.to_owned(),
            size,
            db: Mutex::new(db),
            source,
            cache: Mutex::new(()),
            _lock: lock,
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
    fn base_block(&self, block: &Value) -> io::Result<Vec<u8>> {
        let length = block["size"]
            .as_u64()
            .ok_or_else(|| failure("Invalid extent"))? as usize;
        let Some(hash) = block["hash"].as_str() else {
            return Ok(vec![0; length]);
        };
        let directory = self.directory.join("cache");
        let target = directory.join(hash);
        if let Ok(file) = File::open(&target) {
            use std::io::Read;
            let mut bytes = Vec::new();
            (&file).take(BLOCK + 1).read_to_end(&mut bytes)?;
            if bytes.len() == length && hex::encode(Sha256::digest(&bytes)) == hash {
                let _ = file.set_modified(std::time::SystemTime::now());
                return Ok(bytes);
            }
            let _ = std::fs::remove_file(&target);
        }
        let bytes = self.source.fetch(hash)?;
        if bytes.len() != length || hex::encode(Sha256::digest(&bytes)) != hash {
            return Err(failure("Remote block integrity check failed"));
        }
        // Cache persistence is optional: it never determines whether an acknowledged
        // write survives. Refusing a cache fill must not turn a valid read into EIO.
        let _ = (|| -> io::Result<()> {
            use std::io::Write;
            let _cache = self.cache.lock().map_err(failure)?;
            std::fs::create_dir_all(&directory)?;
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
            // Experimental per-disk ceiling. Node-wide accounting is an additional
            // prerequisite for enabling this adapter in the controller.
            for (path, size, _) in entries {
                if used + bytes.len() as u64 <= 32 * 1024 * 1024 {
                    break;
                }
                std::fs::remove_file(path)?;
                used = used.saturating_sub(size);
            }
            let mut file = tempfile::NamedTempFile::new_in(&directory)?;
            file.write_all(&bytes)?;
            file.persist_noclobber(&target).map_err(failure)?;
            Ok(())
        })();
        Ok(bytes)
    }
}
impl Disk for LazyDisk {
    fn size(&self) -> u64 {
        self.size
    }
    fn read_at(&self, offset: u64, bytes: &mut [u8]) -> io::Result<()> {
        self.check(offset, bytes.len())?;
        let mut covered = vec![false; bytes.len()];
        let mut missing = bytes.len();
        let manifest: Value = {
            let db = self.db.lock().map_err(failure)?;
            let manifest: String = db
                .query_row("SELECT manifest FROM state WHERE id=1", [], |row| {
                    row.get(0)
                })
                .map_err(failure)?;
            let mut statement = db.prepare("SELECT start, data, checksum, seq, generation, end FROM writes WHERE start < ?1 AND end > ?2 ORDER BY seq DESC").map_err(failure)?;
            let mut rows = statement
                .query(params![(offset + bytes.len() as u64) as i64, offset as i64])
                .map_err(failure)?;
            while let Some(row) = rows.next().map_err(failure)? {
                let (start, data) = Self::record(row, self.size)?;
                let begin = start.max(offset);
                let end = (start + data.len() as u64).min(offset + bytes.len() as u64);
                for position in begin..end {
                    let dest = (position - offset) as usize;
                    if !covered[dest] {
                        bytes[dest] = data[(position - start) as usize];
                        covered[dest] = true;
                        missing -= 1;
                    }
                }
                if missing == 0 {
                    break;
                }
            }
            serde_json::from_str(&manifest).map_err(failure)?
        };
        // Never hold the journal lock across remote I/O. This read observes a
        // consistent journal prefix; later writes are independent of cache arrival.
        if missing == 0 {
            return Ok(());
        }
        let end = offset + bytes.len() as u64;
        let mut position = offset;
        while position < end {
            let index = position / BLOCK;
            let limit = ((index + 1) * BLOCK).min(end);
            let start = (position - offset) as usize;
            let finish = (limit - offset) as usize;
            if covered[start..finish].iter().any(|v| !*v) {
                let block = self.base_block(&manifest["blocks"][index as usize])?;
                for dest in start..finish {
                    if !covered[dest] {
                        bytes[dest] = block[(offset + dest as u64 - index * BLOCK) as usize];
                    }
                }
            }
            position = limit;
        }
        Ok(())
    }
    fn write_at(&self, offset: u64, bytes: &[u8]) -> io::Result<()> {
        self.check(offset, bytes.len())?;
        if bytes.is_empty() {
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
        tx.execute("UPDATE state SET next_sequence=?1 WHERE id=1", [next])
            .map_err(failure)?;
        tx.commit().map_err(failure)
    }
    fn sync(&self) -> io::Result<()> {
        let db = self.db.lock().map_err(failure)?;
        db.execute_batch("PRAGMA wal_checkpoint(FULL)")
            .map_err(failure)?;
        File::open(&self.directory)?.sync_all()
    }
}
#[cfg(test)]
mod tests {
    use super::Disk;
    use super::*;
    use sha2::{Digest, Sha256};
    use std::{
        io,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
    };
    struct Source {
        reads: AtomicUsize,
    }
    impl BlockSource for Source {
        fn fetch(&self, _hash: &str) -> io::Result<Vec<u8>> {
            self.reads.fetch_add(1, Ordering::SeqCst);
            Ok(vec![7; 4 * 1024 * 1024])
        }
    }
    #[test]
    fn acknowledged_write_survives_killing_the_storage_process() {
        use std::io::BufRead;
        let root = tempfile::tempdir().unwrap();
        let manifest = serde_json::json!({"version":1,"size":4096,"blockSize":4194304,"blocks":[{"offset":0,"size":4096,"hash":null}]});
        drop(
            LazyDisk::create(
                root.path(),
                &manifest,
                Arc::new(Source {
                    reads: AtomicUsize::new(0),
                }),
            )
            .unwrap(),
        );
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["crash_writer", "--nocapture"])
            .env("LEO_STORAGE_CRASH_TEST_DIR", root.path())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let output = std::io::BufReader::new(child.stdout.take().unwrap());
        let mut acknowledged = false;
        for line in output.lines() {
            if line.unwrap().contains("WRITE_ACKNOWLEDGED") {
                acknowledged = true;
                break;
            }
        }
        child.kill().unwrap();
        child.wait().unwrap();
        assert!(acknowledged);
        let disk = LazyDisk::open(
            root.path(),
            Arc::new(Source {
                reads: AtomicUsize::new(0),
            }),
        )
        .unwrap();
        let mut bytes = [0; 8];
        disk.read_at(3, &mut bytes).unwrap();
        assert_eq!(&bytes, b"survives");
    }
    #[test]
    fn crash_writer() {
        let Some(root) = std::env::var_os("LEO_STORAGE_CRASH_TEST_DIR") else {
            return;
        };
        let disk = LazyDisk::open(
            std::path::Path::new(&root),
            Arc::new(Source {
                reads: AtomicUsize::new(0),
            }),
        )
        .unwrap();
        disk.write_at(3, b"survives").unwrap();
        println!("WRITE_ACKNOWLEDGED");
        std::io::Write::flush(&mut std::io::stdout()).unwrap();
        loop {
            std::thread::park();
        }
    }
    #[test]
    fn journal_integrity_covers_the_write_location() {
        let root = tempfile::tempdir().unwrap();
        let source = Arc::new(Source {
            reads: AtomicUsize::new(0),
        });
        let manifest = serde_json::json!({"version":1,"size":4096,"blockSize":4194304,"blocks":[{"offset":0,"size":4096,"hash":null}]});
        let disk = LazyDisk::create(root.path(), &manifest, source.clone()).unwrap();
        disk.write_at(3, b"retained").unwrap();
        drop(disk);
        let corruptor = rusqlite::Connection::open(root.path().join("journal.sqlite")).unwrap();
        corruptor
            .execute("UPDATE writes SET start=4, end=12", [])
            .unwrap();
        drop(corruptor);
        assert!(
            LazyDisk::open(root.path(), source).is_err(),
            "corrupt journal metadata must be rejected before any read"
        );
    }
    #[test]
    fn unavailable_or_corrupt_base_never_becomes_a_zero_block() {
        struct Missing;
        impl BlockSource for Missing {
            fn fetch(&self, _: &str) -> io::Result<Vec<u8>> {
                Err(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "Storage temporarily unavailable",
                ))
            }
        }
        let root = tempfile::tempdir().unwrap();
        let hash = hex::encode(Sha256::digest(vec![8; 4 * 1024 * 1024]));
        let manifest = serde_json::json!({"version":1,"size":4194304,"blockSize":4194304,"blocks":[{"offset":0,"size":4194304,"hash":hash}]});
        let disk = LazyDisk::create(root.path(), &manifest, Arc::new(Missing)).unwrap();
        assert_eq!(
            disk.read_at(0, &mut [0; 8]).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        drop(disk);
        let disk = LazyDisk::open(
            root.path(),
            Arc::new(Source {
                reads: AtomicUsize::new(0),
            }),
        )
        .unwrap();
        assert!(disk.read_at(0, &mut [0; 8]).is_err());
    }
    #[test]
    fn partial_write_is_durable_without_fetching_its_remote_base() {
        let root = tempfile::tempdir().unwrap();
        let source = Arc::new(Source {
            reads: AtomicUsize::new(0),
        });
        let hash = hex::encode(Sha256::digest(vec![7; 4 * 1024 * 1024]));
        let manifest = serde_json::json!({"version":1,"size":4194304,"blockSize":4194304,"blocks":[{"offset":0,"size":4194304,"hash":hash}]});
        let disk = LazyDisk::create(root.path(), &manifest, source.clone()).unwrap();
        disk.write_at(123, b"retained").unwrap();
        assert_eq!(source.reads.load(Ordering::SeqCst), 0);
        drop(disk);
        let disk = LazyDisk::open(root.path(), source.clone()).unwrap();
        let mut bytes = [0; 8];
        disk.read_at(123, &mut bytes).unwrap();
        assert_eq!(&bytes, b"retained");
        assert_eq!(source.reads.load(Ordering::SeqCst), 0);
        let mut bytes = [0; 10];
        disk.read_at(122, &mut bytes).unwrap();
        assert_eq!(&bytes, b"\x07retained\x07");
        assert_eq!(source.reads.load(Ordering::SeqCst), 1);
        disk.read_at(122, &mut bytes).unwrap();
        assert_eq!(
            source.reads.load(Ordering::SeqCst),
            1,
            "a verified clean block should be reused"
        );
    }
}
