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

/// A single-block manifest; a `null` hash is an empty (zero) block.
fn single_block(size: u64, block_size: u64, hash: &serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "version": 1,
        "size": size,
        "blockSize": block_size,
        "blocks": [{ "offset": 0, "size": size, "hash": hash }]
    })
}

#[test]
fn published_journal_reclaims_disk_space() {
    let root = tempfile::tempdir().unwrap();
    let source = Arc::new(Source {
        reads: AtomicUsize::new(0),
    });
    let manifest = single_block(BLOCK, BLOCK, &serde_json::Value::Null);
    let disk = LazyDisk::create(root.path(), &manifest, source).unwrap();
    disk.write_at(0, &vec![7; BLOCK as usize]).unwrap();
    disk.sync().unwrap();
    let before = std::fs::metadata(root.path().join("journal.sqlite"))
        .unwrap()
        .len();
    let generation = disk.seal().unwrap();
    disk.capture(generation).unwrap();
    disk.commit_published(generation, "published").unwrap();
    let after = std::fs::metadata(root.path().join("journal.sqlite"))
        .unwrap()
        .len();
    assert!(
        after < before / 4,
        "published journal retained {after} of {before} bytes"
    );
    let mut bytes = [0; 4];
    disk.read_at(0, &mut bytes).unwrap();
    assert_eq!(bytes, [7; 4]);
}

#[test]
fn publication_retires_obsolete_clean_cache_without_evicting_current_blocks() {
    let root = tempfile::tempdir().unwrap();
    let directory = root.path().join("disks/conversation/lazy");
    let mut policy = super::super::policy::Policy {
        reserve_mi_b: 64,
        ..Default::default()
    };
    std::fs::write(
        root.path().join("storage-policy.json"),
        serde_json::to_vec(&policy).unwrap(),
    )
    .unwrap();
    let source = Arc::new(Source {
        reads: AtomicUsize::new(0),
    });
    let hash = block_digest(&vec![7; BLOCK as usize]);
    let manifest = serde_json::json!({
        "version": 1,
        "size": 2 * BLOCK,
        "blockSize": BLOCK,
        "blocks": [
            { "offset": 0, "size": BLOCK, "hash": hash },
            { "offset": BLOCK, "size": BLOCK, "hash": null }
        ]
    });
    let disk = LazyDisk::create(&directory, &manifest, source).unwrap();
    disk.read_at(0, &mut [0]).unwrap();
    let obsolete = "a".repeat(64);
    let old_file = directory.join("cache").join(&obsolete);
    std::fs::write(&old_file, b"obsolete verified cache").unwrap();
    // Register the historical file as an existing clean cache entry.
    let node = root.path();
    let reservation = super::super::cache::reserve(node, &policy, 0)
        .unwrap()
        .unwrap();
    reservation
        .filled(&old_file, std::fs::metadata(&old_file).unwrap().len())
        .unwrap();
    drop(reservation);
    let generation = disk.seal().unwrap();
    disk.capture(generation).unwrap();
    disk.write_at(BLOCK, b"unpublished").unwrap();
    disk.commit_published(generation, "new-base").unwrap();
    assert!(
        !old_file.exists(),
        "obsolete clean generations should not fill the node cache"
    );
    policy.cache_mi_b = 4;
    super::super::cache::make_room(node, &policy, 0).unwrap();
    assert!(directory.join("cache").join(hash).exists());
    let mut bytes = [0; 11];
    disk.read_at(BLOCK, &mut bytes).unwrap();
    assert_eq!(&bytes, b"unpublished");
}

#[test]
fn legacy_journal_conversion_waits_for_all_unpublished_writes() {
    let root = tempfile::tempdir().unwrap();
    let source = Arc::new(Source {
        reads: AtomicUsize::new(0),
    });
    let manifest = single_block(BLOCK, BLOCK, &serde_json::Value::Null);
    let disk = LazyDisk::create(root.path(), &manifest, source.clone()).unwrap();
    disk.write_at(0, &vec![7; BLOCK as usize]).unwrap();
    drop(disk);
    let db = Connection::open(root.path().join("journal.sqlite")).unwrap();
    db.execute_batch("PRAGMA auto_vacuum=NONE; VACUUM;")
        .unwrap();
    drop(db);
    let disk = LazyDisk::open(root.path(), source.clone()).unwrap();
    let mode = || {
        disk.db
            .lock()
            .unwrap()
            .query_row::<u32, _, _>("PRAGMA auto_vacuum", [], |r| r.get(0))
            .unwrap()
    };
    assert_eq!(mode(), 0, "opening a dirty legacy disk must not vacuum it");
    let first = disk.seal().unwrap();
    disk.capture(first).unwrap();
    disk.write_at(1, &[7]).unwrap();
    disk.commit_published(first, "first").unwrap();
    assert_eq!(mode(), 0, "a newer write still needs its journal");
    let second = disk.seal().unwrap();
    disk.capture(second).unwrap();
    disk.commit_published(second, "second").unwrap();
    assert_eq!(mode(), 1);
    assert!(
        std::fs::metadata(root.path().join("journal.sqlite"))
            .unwrap()
            .len()
            < BLOCK / 4
    );
    drop(disk);
    let reopened = LazyDisk::open(root.path(), source).unwrap();
    let mut bytes = [0; 8];
    reopened.read_at(0, &mut bytes).unwrap();
    assert_eq!(bytes, [7; 8]);
}

impl BlockSource for Source {
    fn fetch(&self, _hash: &str) -> io::Result<Vec<u8>> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        Ok(vec![7; 4 * 1024 * 1024])
    }
}

#[test]
fn node_cache_evicts_the_oldest_clean_block_after_a_verified_read() {
    struct Blocks(std::collections::HashMap<String, Vec<u8>>);
    impl BlockSource for Blocks {
        fn fetch(&self, hash: &str) -> io::Result<Vec<u8>> {
            self.0
                .get(hash)
                .cloned()
                .ok_or_else(|| io::Error::other("Missing block"))
        }
    }
    let root = tempfile::tempdir().unwrap();
    let policy = serde_json::json!({
        "enabled": true,
        "cacheMiB": 8,
        "reserveMiB": 64,
        "reservePercent": 1,
        "backupSeconds": 60,
        "maxDirtySeconds": 300
    });
    std::fs::write(root.path().join("storage-policy.json"), policy.to_string()).unwrap();
    let mut blocks = std::collections::HashMap::new();
    let mut disks = Vec::new();
    for (name, value) in [("first", 1_u8), ("second", 2), ("third", 3)] {
        let bytes = vec![value; 4 * 1024 * 1024];
        let hash = hex::encode(Sha256::digest(&bytes));
        blocks.insert(hash.clone(), bytes);
        let directory = root.path().join("disks").join(name).join("lazy");
        let manifest = single_block(4194304, 4194304, &hash.as_str().into());
        disks.push((directory, manifest, hash, value));
    }
    let source = Arc::new(Blocks(blocks));
    let read = |index: usize| {
        let (directory, manifest, _, value) = &disks[index];
        let disk = if directory.join("journal.sqlite").exists() {
            LazyDisk::open(directory, source.clone()).unwrap()
        } else {
            LazyDisk::create(directory, manifest, source.clone()).unwrap()
        };
        let mut byte = [0];
        disk.read_at(0, &mut byte).unwrap();
        assert_eq!(byte[0], *value);
    };
    read(0);
    read(1);
    read(0); // A new LazyDisk verifies the disk cache and refreshes its LRU position.
    read(2); // Filling this block must evict the second conversation's older cache.
    for (index, present) in [(0, true), (1, false), (2, true)] {
        let (directory, _, hash, _) = &disks[index];
        assert_eq!(directory.join("cache").join(hash).exists(), present);
        assert!(directory.join("journal.sqlite").exists());
    }
}

#[test]
fn acknowledged_write_survives_killing_the_storage_process() {
    use std::io::BufRead;
    let root = tempfile::tempdir().unwrap();
    let manifest = single_block(4096, 4194304, &serde_json::Value::Null);
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
    let manifest = single_block(4096, 4194304, &serde_json::Value::Null);
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
    let manifest = single_block(4194304, 4194304, &hash.as_str().into());
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
fn sealed_generation_excludes_later_writes_and_publication_preserves_them() {
    struct Remote(std::sync::Mutex<std::collections::HashMap<String, Vec<u8>>>);
    impl BlockSource for Remote {
        fn fetch(&self, hash: &str) -> io::Result<Vec<u8>> {
            self.0
                .lock()
                .unwrap()
                .get(hash)
                .cloned()
                .ok_or_else(|| io::Error::other("Not published"))
        }
    }
    let root = tempfile::tempdir().unwrap();
    let source = Arc::new(Remote(std::sync::Mutex::default()));
    let initial = single_block(4096, 4194304, &serde_json::Value::Null);
    let disk = LazyDisk::create(root.path(), &initial, source.clone()).unwrap();
    disk.write_at(7, b"before").unwrap();
    let generation = disk.seal().unwrap();
    disk.write_at(7, b"after!").unwrap();
    let manifest = disk.capture(generation).unwrap();
    let hash = manifest["blocks"][0]["hash"].as_str().unwrap();
    let bytes = disk.captured_block(generation, hash).unwrap();
    assert_eq!(&bytes[7..13], b"before");
    source.0.lock().unwrap().insert(hash.to_owned(), bytes);
    disk.commit_published(generation, "point-one").unwrap();
    disk.commit_published(generation, "point-one").unwrap();
    assert!(
        disk.commit_published(generation, "different-point")
            .is_err(),
        "a stale publication cannot advance the disk twice"
    );
    drop(disk);
    let disk = LazyDisk::open(root.path(), source).unwrap();
    let mut bytes = [0; 6];
    disk.read_at(7, &mut bytes).unwrap();
    assert_eq!(&bytes, b"after!");
}

#[test]
fn partial_write_is_durable_without_fetching_its_remote_base() {
    let root = tempfile::tempdir().unwrap();
    let source = Arc::new(Source {
        reads: AtomicUsize::new(0),
    });
    let hash = hex::encode(Sha256::digest(vec![7; 4 * 1024 * 1024]));
    let manifest = single_block(4194304, 4194304, &hash.as_str().into());
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

#[test]
#[ignore = "explicit storage interface performance run"]
fn journal_io_performance() {
    use std::time::Instant;
    let root = tempfile::tempdir().unwrap();
    let size = 4 * 1024 * 1024;
    let manifest = single_block(size, size, &serde_json::Value::Null);
    let disk = LazyDisk::create(
        root.path(),
        &manifest,
        Arc::new(Source {
            reads: AtomicUsize::new(0),
        }),
    )
    .unwrap();
    let started = Instant::now();
    for index in 0..128 {
        disk.write_at(index * 4096, &[7; 4096]).unwrap();
    }
    let write_ms = started.elapsed().as_secs_f64() * 1000.0;
    let mut bytes = vec![0; size as usize];
    let started = Instant::now();
    for _ in 0..5 {
        disk.read_at(0, &mut bytes).unwrap();
    }
    let read_ms = started.elapsed().as_secs_f64() * 1000.0 / 5.0;
    assert_eq!(bytes[0], 7);
    assert_eq!(bytes[128 * 4096], 0);
    let started = Instant::now();
    for _ in 0..10 {
        disk.sync().unwrap();
    }
    let sync_ms = started.elapsed().as_secs_f64() * 1000.0 / 10.0;
    println!(
        "STORAGE_PERF {}",
        serde_json::json!({
            "write128x4kMs": write_ms,
            "read4MiBMs": read_ms,
            "syncMs": sync_ms
        })
    );
}

#[test]
fn overlapping_journal_writes_preserve_latest_bytes_and_zero_gaps() {
    let root = tempfile::tempdir().unwrap();
    let manifest = single_block(4096, 4194304, &serde_json::Value::Null);
    let disk = LazyDisk::create(
        root.path(),
        &manifest,
        Arc::new(Source {
            reads: AtomicUsize::new(0),
        }),
    )
    .unwrap();
    disk.write_at(4, b"aaaaaaaa").unwrap();
    disk.write_at(8, b"bbbbbbbb").unwrap();
    disk.write_at(10, b"cc").unwrap();
    let mut bytes = [0; 20];
    disk.read_at(0, &mut bytes).unwrap();
    assert_eq!(&bytes, b"\0\0\0\0aaaabbccbbbb\0\0\0\0");
}

#[test]
fn read_includes_the_end_of_a_maximum_size_journal_write() {
    let root = tempfile::tempdir().unwrap();
    let manifest = serde_json::json!({
        "version": 1,
        "size": MAX_IO + 1,
        "blockSize": BLOCK,
        "blocks": [
            {"offset": 0, "size": BLOCK, "hash": null},
            {"offset": BLOCK, "size": BLOCK, "hash": null},
            {"offset": 2 * BLOCK, "size": 1, "hash": null}
        ]
    });
    let disk = LazyDisk::create(
        root.path(),
        &manifest,
        Arc::new(Source {
            reads: AtomicUsize::new(0),
        }),
    )
    .unwrap();
    let mut write = vec![0; MAX_IO];
    write[MAX_IO - 1] = 42;
    disk.write_at(0, &write).unwrap();
    let mut read = [0; 2];
    disk.read_at(MAX_IO as u64 - 1, &mut read).unwrap();
    assert_eq!(read, [42, 0]);
}

#[test]
fn concurrent_cold_reads_share_one_verified_transfer() {
    struct SlowSource {
        calls: AtomicUsize,
    }
    impl BlockSource for SlowSource {
        fn fetch(&self, _: &str) -> io::Result<Vec<u8>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            std::thread::sleep(std::time::Duration::from_millis(100));
            Ok(vec![7; BLOCK as usize])
        }
    }
    let root = tempfile::tempdir().unwrap();
    let source = Arc::new(SlowSource {
        calls: AtomicUsize::new(0),
    });
    let hash = block_digest(&vec![7; BLOCK as usize]);
    let manifest = single_block(BLOCK, BLOCK, &hash.into());
    let disk = Arc::new(LazyDisk::create(root.path(), &manifest, source.clone()).unwrap());
    let gate = Arc::new(std::sync::Barrier::new(8));
    std::thread::scope(|scope| {
        for _ in 0..8 {
            let (disk, gate) = (disk.clone(), gate.clone());
            scope.spawn(move || {
                gate.wait();
                let mut bytes = [0; 4096];
                disk.read_at(0, &mut bytes).unwrap();
                assert_eq!(bytes, [7; 4096]);
            });
        }
    });
    assert_eq!(
        source.calls.load(Ordering::SeqCst),
        1,
        "overlapping FUSE reads must share their cold block"
    );
}

#[test]
fn distinct_cold_blocks_still_download_concurrently() {
    struct GatedSource {
        entered: std::sync::mpsc::Sender<String>,
        gate: Arc<(Mutex<bool>, std::sync::Condvar)>,
        first: String,
    }
    impl BlockSource for GatedSource {
        fn fetch(&self, hash: &str) -> io::Result<Vec<u8>> {
            self.entered.send(hash.into()).unwrap();
            let (lock, wake) = &*self.gate;
            let mut ready = lock.lock().unwrap();
            while !*ready {
                ready = wake.wait(ready).unwrap();
            }
            Ok(vec![if hash == self.first { 7 } else { 9 }; BLOCK as usize])
        }
    }
    let root = tempfile::tempdir().unwrap();
    let first = block_digest(&vec![7; BLOCK as usize]);
    let second = block_digest(&vec![9; BLOCK as usize]);
    let (entered, arrivals) = std::sync::mpsc::channel();
    let gate = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
    let source = Arc::new(GatedSource {
        entered,
        gate: gate.clone(),
        first: first.clone(),
    });
    let manifest = serde_json::json!({
        "version": 1,
        "size": 2 * BLOCK,
        "blockSize": BLOCK,
        "blocks": [
            { "offset": 0, "size": BLOCK, "hash": first },
            { "offset": BLOCK, "size": BLOCK, "hash": second }
        ]
    });
    let disk = Arc::new(LazyDisk::create(root.path(), &manifest, source).unwrap());
    std::thread::scope(|scope| {
        for (offset, expected) in [(0, 7), (BLOCK, 9)] {
            let disk = disk.clone();
            scope.spawn(move || {
                let mut bytes = [0; 4096];
                disk.read_at(offset, &mut bytes).unwrap();
                assert_eq!(bytes, [expected; 4096]);
            });
        }
        let a = arrivals.recv_timeout(std::time::Duration::from_secs(1));
        let b = arrivals.recv_timeout(std::time::Duration::from_secs(1));
        *gate.0.lock().unwrap() = true;
        gate.1.notify_all();
        assert_ne!(
            a.unwrap(),
            b.expect("a different block must not wait for this transfer")
        );
    });
}
