use super::Disk;
use super::*;
use std::{
    io,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

mod consolidation;
mod discard;
mod group_commit;

#[test]
#[ignore = "explicit local journal recovery measurement; run alone"]
fn journal_reopen_measurement() {
    use std::os::unix::fs::MetadataExt;
    use std::time::Instant;

    fn cpu_micros() -> u64 {
        let mut usage = std::mem::MaybeUninit::<libc::rusage>::zeroed();
        assert_eq!(
            unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) },
            0
        );
        let usage = unsafe { usage.assume_init() };
        ((usage.ru_utime.tv_sec + usage.ru_stime.tv_sec) * 1_000_000
            + usage.ru_utime.tv_usec
            + usage.ru_stime.tv_usec) as u64
    }

    fn read_bytes() -> u64 {
        std::fs::read_to_string("/proc/self/io")
            .unwrap()
            .lines()
            .find_map(|line| line.strip_prefix("rchar: "))
            .unwrap()
            .trim()
            .parse()
            .unwrap()
    }

    let gib: u64 =
        std::env::var("CAIRN_REOPEN_BENCH_GIB").map_or(16, |value| value.parse().unwrap());
    let frame: u64 = std::env::var("CAIRN_REOPEN_BENCH_FRAME_KIB")
        .map_or(BLOCK, |value| value.parse::<u64>().unwrap() * 1024);
    assert!(frame > 0 && frame <= BLOCK && BLOCK.is_multiple_of(frame));
    let root = tempfile::tempdir_in("/var/tmp").unwrap();
    let source = Arc::new(Source {
        reads: AtomicUsize::new(0),
    });
    let disk = LazyDisk::create(
        root.path(),
        &single_block(BLOCK, BLOCK, &Value::Null),
        source.clone(),
    )
    .unwrap();
    let zeros = vec![0; frame as usize];
    // Repeated sparse discards model accumulated obsolete append history while
    // allocating only headers. Every zero write follows a nonzero byte so it
    // cannot be suppressed by the live zero-range optimization.
    let count = gib * 1024 * 1024 * 1024 / frame;
    let pairs_per_batch = (MAX_IO / (frame as usize + 1)).min(64);
    for first in (0..count).step_by(pairs_per_batch) {
        let mut writes = Vec::new();
        for index in first..(first + pairs_per_batch as u64).min(count) {
            let offset = index % (BLOCK / frame) * frame;
            writes.push(DiskWrite {
                offset,
                bytes: b"x",
            });
            writes.push(DiskWrite {
                offset,
                bytes: &zeros,
            });
        }
        disk.write_batch(&writes).unwrap();
    }
    disk.sync().unwrap();
    let logical = disk.accounting().unwrap()["journalBytes"].as_u64().unwrap();
    drop(disk);
    let physical = std::fs::read_dir(root.path())
        .unwrap()
        .map(|entry| entry.unwrap().metadata().unwrap().blocks() * 512)
        .sum::<u64>();

    for trial in 0..3 {
        let before_reads = read_bytes();
        let before_cpu = cpu_micros();
        let started = Instant::now();
        let disk = LazyDisk::open(root.path(), source.clone()).unwrap();
        let open_us = started.elapsed().as_micros();
        let open_cpu_us = cpu_micros() - before_cpu;
        let open_read_bytes = read_bytes() - before_reads;
        let mut bytes = [1; 4096];
        let started = Instant::now();
        disk.read_at(0, &mut bytes).unwrap();
        assert_eq!(bytes, [if trial == 0 { 0 } else { 7 }; 4096]);
        let cold_read_us = started.elapsed().as_micros();
        let started = Instant::now();
        for _ in 0..256 {
            disk.read_at(0, &mut bytes).unwrap();
        }
        let warm_reads_us = started.elapsed().as_micros();
        let started = Instant::now();
        for _ in 0..16 {
            disk.write_at(0, &[7; 4096]).unwrap();
        }
        disk.sync().unwrap();
        let write_us = started.elapsed().as_micros();
        println!(
            "REOPEN_PERF {}",
            serde_json::json!({
                "gib": gib,
                "frameKiB": frame / 1024,
                "trial": trial,
                "journalLogicalBytes": logical,
                "journalAllocatedBytes": physical,
                "openUs": open_us,
                "openCpuUs": open_cpu_us,
                "openReadBytes": open_read_bytes,
                "coldReadUs": cold_read_us,
                "warmReads256Us": warm_reads_us,
                "writes16AndSyncUs": write_us,
                "remoteEncodedBytes": 0
            })
        );
    }
}

struct Source {
    reads: AtomicUsize,
}

#[test]
fn published_writes_resume_from_bounded_disk_cache_after_controller_restart() {
    struct Missing;
    impl BlockSource for Missing {
        fn fetch(&self, _: &str) -> io::Result<Vec<u8>> {
            Err(io::Error::other("Remote storage is unavailable"))
        }
    }
    let root = tempfile::tempdir().unwrap();
    let directory = root.path().join("disks/conversation/lazy");
    let policy = super::super::policy::Policy {
        reserve_mi_b: 64,
        ..Default::default()
    };
    std::fs::write(
        root.path().join("storage-policy.json"),
        serde_json::to_vec(&policy).unwrap(),
    )
    .unwrap();
    let source = Arc::new(Missing);
    let disk = LazyDisk::create(
        &directory,
        &single_block(BLOCK, BLOCK, &Value::Null),
        source.clone(),
    )
    .unwrap();
    // Newly created Codex databases were written, but never read during this turn.
    disk.write_at(17, b"new conversation state").unwrap();
    let generation = disk.seal().unwrap();
    let manifest = disk.capture(generation).unwrap();
    let hash = manifest["blocks"][0]["hash"].as_str().unwrap();
    assert_eq!(disk.performance()["blockCache"]["bytes"], 0);
    disk.commit_published(generation, "published").unwrap();
    drop(disk);
    let disk = LazyDisk::open(&directory, source).unwrap();
    let mut bytes = [0; 22];
    disk.read_at(17, &mut bytes).unwrap();
    assert_eq!(&bytes, b"new conversation state");
    assert!(directory.join("cache").join(hash).exists());
    assert_eq!(disk.performance()["remoteFetch"]["count"], 0);
    assert_eq!(disk.performance()["diskHits"], 1);
}

/// Write `blocks` as the local base files of a disk created at `root`. A `None`
/// block is an empty (zero) extent.
fn local_base(root: &Path, blocks: &[Option<Vec<u8>>]) -> serde_json::Value {
    std::fs::create_dir_all(root.join("cache")).unwrap();
    let blocks = blocks
        .iter()
        .enumerate()
        .map(|(index, bytes)| {
            let hash = bytes.as_ref().map(|bytes| {
                let hash = block_digest(bytes);
                std::fs::write(root.join("cache").join(&hash), bytes).unwrap();
                hash
            });
            serde_json::json!({ "offset": index as u64 * BLOCK, "size": BLOCK, "hash": hash })
        })
        .collect::<Vec<_>>();
    serde_json::json!({
        "version": 1,
        "size": blocks.len() as u64 * BLOCK,
        "blockSize": BLOCK,
        "blocks": blocks
    })
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
    let before = disk.accounting().unwrap()["journalBytes"].as_u64().unwrap();
    let generation = disk.seal().unwrap();
    disk.capture(generation).unwrap();
    disk.commit_published(generation, "published").unwrap();
    let after = disk.accounting().unwrap()["journalBytes"].as_u64().unwrap();
    assert!(
        after < before / 4,
        "published journal retained {after} of {before} bytes"
    );
    let mut bytes = [0; 4];
    disk.read_at(0, &mut bytes).unwrap();
    assert_eq!(bytes, [7; 4]);
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
        .env("CAIRN_STORAGE_CRASH_TEST_DIR", root.path())
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
    let Some(root) = std::env::var_os("CAIRN_STORAGE_CRASH_TEST_DIR") else {
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
fn reopening_indexes_headers_and_verifies_payload_only_when_used() {
    use std::os::unix::fs::FileExt;

    let root = tempfile::tempdir().unwrap();
    let source = Arc::new(Source {
        reads: AtomicUsize::new(0),
    });
    let disk = LazyDisk::create(
        root.path(),
        &single_block(4096, BLOCK, &Value::Null),
        source.clone(),
    )
    .unwrap();
    disk.write_at(0, b"retained").unwrap();
    disk.sync().unwrap();
    drop(disk);

    let segment = std::fs::OpenOptions::new()
        .write(true)
        .open(root.path().join("payload-1-1.segment"))
        .unwrap();
    segment.write_all_at(b"damaged!", 112).unwrap();
    segment.sync_all().unwrap();

    let disk = LazyDisk::open(root.path(), source).expect("opening must not read frame payloads");
    let mut output = [0; 8];
    assert!(disk.read_at(0, &mut output).is_err());
    let generation = disk.seal().unwrap();
    assert!(disk.capture(generation).is_err());
}

#[test]
fn recovery_does_not_read_obsolete_overwritten_payloads() {
    use std::os::unix::fs::FileExt;

    let root = tempfile::tempdir().unwrap();
    let source = Arc::new(Source {
        reads: AtomicUsize::new(0),
    });
    let disk = LazyDisk::create(
        root.path(),
        &single_block(4096, BLOCK, &Value::Null),
        source.clone(),
    )
    .unwrap();
    disk.write_at(0, b"obsolete").unwrap();
    disk.write_at(0, b"current!").unwrap();
    disk.sync().unwrap();
    drop(disk);

    let segment = std::fs::OpenOptions::new()
        .write(true)
        .open(root.path().join("payload-1-1.segment"))
        .unwrap();
    segment.write_all_at(b"damaged!", 112).unwrap();
    segment.sync_all().unwrap();

    let disk = LazyDisk::open(root.path(), source).unwrap();
    let mut output = [0; 8];
    disk.read_at(0, &mut output).unwrap();
    assert_eq!(&output, b"current!");
    assert_eq!(disk.accounting().unwrap()["dirtyBytes"], 16);
}

#[test]
fn journal_integrity_covers_the_write_location() {
    use std::os::unix::fs::FileExt;
    let root = tempfile::tempdir().unwrap();
    let source = Arc::new(Source {
        reads: AtomicUsize::new(0),
    });
    let manifest = single_block(4096, 4194304, &serde_json::Value::Null);
    let disk = LazyDisk::create(root.path(), &manifest, source.clone()).unwrap();
    disk.write_at(3, b"retained").unwrap();
    drop(disk);
    let segment = std::fs::OpenOptions::new()
        .write(true)
        .open(root.path().join("payload-1-1.segment"))
        .unwrap();
    segment.write_all_at(&4_i64.to_le_bytes(), 24).unwrap();
    segment.sync_all().unwrap();
    assert!(
        LazyDisk::open(root.path(), source).is_err(),
        "corrupt journal metadata must be rejected before any read"
    );
}

#[test]
fn interrupted_tail_is_trimmed_and_missing_acknowledged_segments_are_rejected() {
    use std::io::Write;
    let root = tempfile::tempdir().unwrap();
    let source = Arc::new(Source {
        reads: AtomicUsize::new(0),
    });
    let disk = LazyDisk::create(
        root.path(),
        &single_block(4096, BLOCK, &Value::Null),
        source.clone(),
    )
    .unwrap();
    disk.write_at(0, b"retained").unwrap();
    drop(disk);
    let path = root.path().join("payload-1-1.segment");
    let length = std::fs::metadata(&path).unwrap().len();
    let mut tail = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap();
    tail.write_all(b"CAIRNJ03partial").unwrap();
    tail.sync_all().unwrap();
    let disk = LazyDisk::open(root.path(), source.clone()).unwrap();
    assert_eq!(std::fs::metadata(&path).unwrap().len(), length);
    drop(disk);
    // A complete, valid header followed by an incomplete payload is also an
    // unacknowledged append; corrupt headers must not take this recovery path.
    let mut header = std::fs::read(&path).unwrap()[..112].to_vec();
    header[8..16].copy_from_slice(&2_i64.to_le_bytes());
    let checksum = journal::digest(&header[..48], &[]);
    header[48..80].copy_from_slice(&checksum);
    tail.write_all(&header).unwrap();
    tail.write_all(b"par").unwrap();
    tail.sync_all().unwrap();
    let disk = LazyDisk::open(root.path(), source.clone()).unwrap();
    assert_eq!(std::fs::metadata(&path).unwrap().len(), length);
    disk.write_at(8, b"continued").unwrap();
    let generation = disk.seal().unwrap();
    disk.write_at(20, b"new generation").unwrap();
    drop(disk);
    let disk = LazyDisk::open(root.path(), source.clone()).unwrap();
    assert_eq!(disk.accounting().unwrap()["generation"], generation + 1);
    let mut bytes = [0; 17];
    disk.read_at(0, &mut bytes).unwrap();
    assert_eq!(&bytes, b"retainedcontinued");
    drop(disk);
    std::fs::remove_file(path).unwrap();
    assert!(
        LazyDisk::open(root.path(), source).is_err(),
        "missing durable descriptor must fail closed"
    );
}

struct PublishedSource(PathBuf);

impl BlockSource for PublishedSource {
    fn fetch(&self, hash: &str) -> io::Result<Vec<u8>> {
        std::fs::read(self.0.join(format!("remote-{hash}")))
    }
}

#[test]
fn publication_and_partial_reclamation_survive_process_crashes() {
    use std::io::BufRead;
    for phase in [
        "publication_transaction",
        "publication_committed",
        "reclamation",
    ] {
        let root = tempfile::tempdir().unwrap();
        let source = Arc::new(PublishedSource(root.path().to_owned()));
        drop(
            LazyDisk::create(
                root.path(),
                &single_block(BLOCK, BLOCK, &Value::Null),
                source.clone(),
            )
            .unwrap(),
        );
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["crash_publication_writer", "--nocapture"])
            .env("CAIRN_JOURNAL_CRASH_DIR", root.path())
            .env("CAIRN_JOURNAL_CRASH_PHASE", phase)
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let output = std::io::BufReader::new(child.stdout.take().unwrap());
        let reached = output
            .lines()
            .any(|line| line.unwrap().contains("JOURNAL_CRASH_READY"));
        child.kill().unwrap();
        child.wait().unwrap();
        assert!(reached, "missing injection: {phase}");
        let disk = LazyDisk::open(root.path(), source).unwrap();
        let mut bytes = [0; 6];
        disk.read_at(0, &mut bytes).unwrap();
        assert_eq!(&bytes, b"before");
        disk.read_at(8, &mut bytes).unwrap();
        assert_eq!(&bytes, b"after!");
        let published = disk.accounting().unwrap()["published"].clone();
        assert_eq!(published.is_null(), phase == "publication_transaction");
        disk.write_at(32, b"continued").unwrap();
        disk.sync().unwrap();
    }
}

#[test]
fn crash_publication_writer() {
    let Some(root) = std::env::var_os("CAIRN_JOURNAL_CRASH_DIR") else {
        return;
    };
    let root = Path::new(&root);
    let disk = LazyDisk::open(root, Arc::new(PublishedSource(root.to_owned()))).unwrap();
    // More than one obsolete segment makes the unlink injection meaningful.
    for _ in 0..17 {
        disk.write_at(0, &vec![7; BLOCK as usize]).unwrap();
    }
    disk.write_at(0, b"before").unwrap();
    let generation = disk.seal().unwrap();
    let manifest = disk.capture(generation).unwrap();
    let hash = manifest["blocks"][0]["hash"].as_str().unwrap();
    let bytes = disk.captured_block(generation, hash).unwrap();
    let file = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(root.join(format!("remote-{hash}")))
        .unwrap();
    std::io::Write::write_all(&mut &file, &bytes).unwrap();
    file.sync_all().unwrap();
    File::open(root).unwrap().sync_all().unwrap();
    disk.write_at(8, b"after!").unwrap();
    disk.commit_published(generation, "published").unwrap();
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
    let generation = disk.seal_completed().unwrap();
    disk.write_at(7, b"after!").unwrap();
    let completed = disk.completed().unwrap().unwrap();
    assert_eq!(completed["generation"], generation);
    drop(disk);
    let disk = LazyDisk::open(root.path(), source.clone()).unwrap();
    assert_eq!(disk.completed().unwrap().unwrap(), completed);
    assert!(disk.has_sealed(generation).unwrap());
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
    assert_eq!(disk.completed().unwrap().unwrap(), completed);
    assert!(!disk.has_sealed(generation).unwrap());
    let mut bytes = [0; 6];
    disk.read_at(7, &mut bytes).unwrap();
    assert_eq!(&bytes, b"after!");
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
