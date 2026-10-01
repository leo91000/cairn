use super::*;
use std::{
    sync::mpsc,
    time::{Duration, Instant},
};

#[test]
fn one_caller_batches_ordered_writes_behind_one_durable_barrier() {
    let root = tempfile::tempdir().unwrap();
    let disk = disk(root.path());
    let writes = [
        DiskWrite {
            offset: 4,
            bytes: b"aaaaaaaa",
        },
        DiskWrite {
            offset: 8,
            bytes: b"bbbbbbbb",
        },
        DiskWrite {
            offset: 10,
            bytes: b"cc",
        },
    ];
    disk.write_batch(&writes).unwrap();
    assert_eq!(disk.performance()["journalCommit"]["count"], 1);
    assert_eq!(disk.performance()["committedFrames"], 3);
    assert_eq!(disk.performance()["write"]["count"], 3);
    drop(disk);
    let reopened = LazyDisk::open(
        root.path(),
        Arc::new(Source {
            reads: AtomicUsize::new(0),
        }),
    )
    .unwrap();
    let mut bytes = [0; 20];
    reopened.read_at(0, &mut bytes).unwrap();
    assert_eq!(&bytes, b"\0\0\0\0aaaabbccbbbb\0\0\0\0");
}

#[test]
fn invalid_later_write_rejects_the_batch_before_any_append() {
    let root = tempfile::tempdir().unwrap();
    let disk = disk(root.path());
    assert!(
        disk.write_batch(&[
            DiskWrite {
                offset: 0,
                bytes: b"must not appear"
            },
            DiskWrite {
                offset: BLOCK,
                bytes: b"outside"
            },
        ])
        .is_err()
    );
    assert_eq!(disk.journal.lock().unwrap().next, 1);
    assert_eq!(disk.performance()["journalCommit"]["count"], 0);
    disk.write_at(0, b"valid after rejected batch").unwrap();
}

fn disk(root: &Path) -> Arc<LazyDisk> {
    Arc::new(
        LazyDisk::create(
            root,
            &single_block(BLOCK, BLOCK, &Value::Null),
            Arc::new(Source {
                reads: AtomicUsize::new(0),
            }),
        )
        .unwrap(),
    )
}

fn wait_for_staged(disk: &LazyDisk, next: i64) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while disk.journal.lock().unwrap().next != next {
        assert!(
            Instant::now() < deadline,
            "writers failed to stage their frames"
        );
        std::thread::yield_now();
    }
}

#[test]
fn concurrent_writes_share_one_durable_barrier_before_acknowledgement() {
    let root = tempfile::tempdir().unwrap();
    let disk = disk(root.path());
    let commit = disk.commit.lock().unwrap();
    let (send, receive) = mpsc::channel();
    std::thread::scope(|scope| {
        for index in 0..4 {
            let disk = disk.clone();
            let send = send.clone();
            scope.spawn(move || {
                let result = disk.write_at(index * 4096, &[index as u8 + 1; 4096]);
                send.send(result).unwrap();
            });
        }
        wait_for_staged(&disk, 5);
        assert!(disk.write_gate.try_write().is_err());
        assert!(disk.publication.try_write().is_err());
        assert!(
            receive.try_recv().is_err(),
            "staging must not acknowledge writes"
        );
        assert!(disk.journal.lock().unwrap().sync().is_err());
        drop(commit);
        for _ in 0..4 {
            receive
                .recv_timeout(Duration::from_secs(5))
                .unwrap()
                .unwrap();
        }
    });
    let metrics = disk.performance();
    assert_eq!(metrics["journalCommit"]["count"], 1);
    assert_eq!(metrics["committedFrames"], 4);
    assert_eq!(metrics["maxCommitFrames"], 4);
    disk.sync().unwrap();
    let generation = disk.seal().unwrap();
    assert_eq!(generation, 1);
    drop(disk);
    let reopened = LazyDisk::open(
        root.path(),
        Arc::new(Source {
            reads: AtomicUsize::new(0),
        }),
    )
    .unwrap();
    for index in 0..4 {
        let mut bytes = [0; 4096];
        reopened.read_at(index * 4096, &mut bytes).unwrap();
        assert_eq!(bytes, [index as u8 + 1; 4096]);
    }
}

#[test]
fn failed_group_rejects_every_waiter_and_fences_until_recovery() {
    let root = tempfile::tempdir().unwrap();
    let disk = disk(root.path());
    disk.commit_failure.store(true, Ordering::Relaxed);
    let commit = disk.commit.lock().unwrap();
    let (send, receive) = mpsc::channel();
    std::thread::scope(|scope| {
        for index in 0..4 {
            let disk = disk.clone();
            let send = send.clone();
            scope.spawn(move || send.send(disk.write_at(index * 4096, &[7; 4096])).unwrap());
        }
        wait_for_staged(&disk, 5);
        assert!(receive.try_recv().is_err());
        drop(commit);
        for _ in 0..4 {
            assert!(
                receive
                    .recv_timeout(Duration::from_secs(5))
                    .unwrap()
                    .is_err()
            );
        }
    });
    assert!(disk.write_at(0, b"later").is_err());
    assert!(disk.sync().is_err());
    assert!(disk.seal().is_err());
    assert_eq!(disk.performance()["committedFrames"], 0);
    assert_eq!(disk.performance()["journalCommit"]["errors"], 1);
    drop(disk);
    let reopened = LazyDisk::open(
        root.path(),
        Arc::new(Source {
            reads: AtomicUsize::new(0),
        }),
    )
    .unwrap();
    reopened.write_at(0, b"recovered").unwrap();
    reopened.sync().unwrap();
}

#[test]
fn sync_and_seal_wait_for_staged_write_acknowledgements() {
    for seal in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let disk = disk(root.path());
        let commit = disk.commit.lock().unwrap();
        let (send, receive) = mpsc::channel();
        std::thread::scope(|scope| {
            let writer = disk.clone();
            scope.spawn(move || writer.write_at(0, b"durable").unwrap());
            wait_for_staged(&disk, 2);
            let fence = disk.clone();
            scope.spawn(move || {
                send.send(if seal {
                    fence.seal().map(|_| ())
                } else {
                    fence.sync()
                })
                .unwrap();
            });
            assert!(receive.recv_timeout(Duration::from_millis(50)).is_err());
            drop(commit);
            receive
                .recv_timeout(Duration::from_secs(5))
                .unwrap()
                .unwrap();
        });
    }
}

#[test]
fn publication_waits_for_pending_writes_and_keeps_their_newer_generation() {
    let root = tempfile::tempdir().unwrap();
    let disk = disk(root.path());
    disk.write_at(0, b"old base").unwrap();
    let generation = disk.seal().unwrap();
    disk.capture(generation).unwrap();
    let commit = disk.commit.lock().unwrap();
    let (started, running) = mpsc::channel();
    let (send, receive) = mpsc::channel();
    std::thread::scope(|scope| {
        let writer = disk.clone();
        scope.spawn(move || writer.write_at(0, b"new pending").unwrap());
        wait_for_staged(&disk, 3);
        assert!(disk.publication.try_write().is_err());
        let publisher = disk.clone();
        scope.spawn(move || {
            started.send(()).unwrap();
            send.send(publisher.commit_published(generation, "receipt"))
                .unwrap();
        });
        running.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(receive.recv_timeout(Duration::from_millis(50)).is_err());
        drop(commit);
        receive
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .unwrap();
    });
    let mut bytes = [0; 11];
    disk.read_at(0, &mut bytes).unwrap();
    assert_eq!(&bytes, b"new pending");
    disk.sync().unwrap();
    drop(disk);
    let reopened = LazyDisk::open(
        root.path(),
        Arc::new(Source {
            reads: AtomicUsize::new(0),
        }),
    )
    .unwrap();
    reopened.read_at(0, &mut bytes).unwrap();
    assert_eq!(&bytes, b"new pending");
}

#[test]
fn acknowledged_prefix_survives_crash_before_and_after_group_sync() {
    use std::io::BufRead;
    for phase in [
        "before_group_sync",
        "after_group_sync",
        "group_acknowledged",
    ] {
        let root = tempfile::tempdir().unwrap();
        let disk = disk(root.path());
        disk.write_at(0, b"acknowledged").unwrap();
        drop(disk);
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "storage::lazy::tests::group_commit::crash_group_writer",
                "--ignored",
                "--nocapture",
            ])
            .env("LEO_JOURNAL_CRASH_DIR", root.path())
            .env("LEO_JOURNAL_CRASH_PHASE", phase)
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let output = child.stdout.take().unwrap();
        let (send, receive) = mpsc::channel();
        let reader = std::thread::spawn(move || {
            for line in std::io::BufReader::new(output).lines() {
                if line.unwrap().contains("JOURNAL_CRASH_READY") {
                    send.send(()).unwrap();
                    break;
                }
            }
        });
        let ready = receive.recv_timeout(Duration::from_secs(10));
        child.kill().unwrap();
        child.wait().unwrap();
        reader.join().unwrap();
        ready.unwrap();
        let reopened = LazyDisk::open(
            root.path(),
            Arc::new(Source {
                reads: AtomicUsize::new(0),
            }),
        )
        .unwrap();
        let mut bytes = [0; 12];
        reopened.read_at(0, &mut bytes).unwrap();
        assert_eq!(&bytes, b"acknowledged");
        for index in 1..5 {
            let mut bytes = [0; 4096];
            reopened.read_at(index * 4096, &mut bytes).unwrap();
            assert_eq!(bytes, [index as u8; 4096]);
        }
        reopened.write_at(0, b"after crash").unwrap();
        reopened.sync().unwrap();
    }
}

#[test]
#[ignore = "subprocess crash helper"]
fn crash_group_writer() {
    let root = std::env::var_os("LEO_JOURNAL_CRASH_DIR").unwrap();
    let disk = Arc::new(
        LazyDisk::open(
            Path::new(&root),
            Arc::new(Source {
                reads: AtomicUsize::new(0),
            }),
        )
        .unwrap(),
    );
    let commit = disk.commit.lock().unwrap();
    std::thread::scope(|scope| {
        for index in 1..5 {
            let disk = disk.clone();
            scope.spawn(move || disk.write_at(index * 4096, &[index as u8; 4096]).unwrap());
        }
        wait_for_staged(&disk, 6);
        drop(commit);
    });
    journal::crash_point(Path::new(&root), "group_acknowledged");
}

#[test]
#[ignore = "explicit matched durability benchmark; run without other tests"]
fn grouped_journal_performance() {
    const WORKERS: u64 = 4;
    const WRITES: u64 = 128;
    for grouped in [false, true, true, false] {
        let root = tempfile::tempdir_in("/var/tmp").unwrap();
        let directory = root.path();
        let disk = disk(root.path());
        let ready = std::sync::Barrier::new(WORKERS as usize + 1);
        let elapsed = std::thread::scope(|scope| {
            for worker in 0..WORKERS {
                let disk = disk.clone();
                let ready = &ready;
                scope.spawn(move || {
                    ready.wait();
                    for index in 0..WRITES {
                        let offset = (worker * WRITES + index) * 4096;
                        let bytes = [worker as u8 + 1; 4096];
                        if grouped {
                            disk.write_at(offset, &bytes).unwrap();
                            continue;
                        }
                        // Previous protocol: append and sync each frame while
                        // holding the journal lock. Same frame format, files,
                        // checksums and durable acknowledgements as the candidate.
                        let mut journal = disk.journal.lock().unwrap();
                        let sequence = journal.append(directory, &disk.db, offset, &bytes).unwrap();
                        let batch = journal.commit_batch(sequence).unwrap().unwrap();
                        batch.sync().unwrap();
                        journal.committed(&batch).unwrap();
                        disk.update_accounting(&journal).unwrap();
                    }
                });
            }
            let start = Instant::now();
            ready.wait();
            start
        })
        .elapsed();
        disk.sync().unwrap();
        let metrics = disk.performance();
        drop(disk);
        let reopened = LazyDisk::open(
            root.path(),
            Arc::new(Source {
                reads: AtomicUsize::new(0),
            }),
        )
        .unwrap();
        for worker in 0..WORKERS {
            let mut bytes = vec![0; WRITES as usize * 4096];
            reopened
                .read_at(worker * WRITES * 4096, &mut bytes)
                .unwrap();
            assert!(bytes.iter().all(|byte| *byte == worker as u8 + 1));
        }
        println!(
            "JOURNAL_GROUP_PERF {}",
            serde_json::json!({
                "grouped": grouped,
                "elapsedMs": elapsed.as_secs_f64() * 1000.0,
                "writes": WORKERS * WRITES,
                "barriers": if grouped { metrics["journalCommit"]["count"].as_u64().unwrap() } else { WORKERS * WRITES },
                "maxGroup": metrics["maxCommitFrames"],
                "integrity": true
            })
        );
    }
}
