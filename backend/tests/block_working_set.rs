use leo_agent_manager::storage::{BlockSource, Disk, LazyDisk};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{collections::HashMap, io, sync::Arc};

struct Offline;

impl BlockSource for Offline {
    fn fetch(&self, _: &str) -> io::Result<Vec<u8>> {
        Err(io::Error::other(
            "All blocks must be served from verified local data",
        ))
    }
}

#[test]
fn small_interleaved_reads_reuse_verified_blocks_across_conversation_disks() {
    const BLOCK: u64 = 4 * 1024 * 1024;
    let root = tempfile::tempdir().unwrap();
    let mut blocks = Vec::new();
    let mut data = HashMap::new();
    for index in 0..9 {
        let bytes = vec![index as u8 + 1; BLOCK as usize];
        let hash = hex::encode(Sha256::digest(&bytes));
        blocks.push(json!({"offset": index * BLOCK, "size": BLOCK, "hash": hash}));
        data.insert(hash, bytes);
    }
    let manifest = json!({
        "version": 1, "size": 9 * BLOCK, "blockSize": BLOCK, "blocks": blocks
    });
    let directory = root.path().join("disks/first/lazy");
    let first = LazyDisk::create(&directory, &manifest, Arc::new(Offline)).unwrap();
    std::fs::create_dir(directory.join("cache")).unwrap();
    for (hash, bytes) in data {
        std::fs::write(directory.join("cache").join(hash), bytes).unwrap();
    }
    let mut buffer = [0; 4096];
    for _ in 0..64 {
        for index in 0..9 {
            first.read_at(index * BLOCK, &mut buffer).unwrap();
            assert!(buffer.iter().all(|&byte| byte == index as u8 + 1));
        }
    }
    assert_eq!(
        first.performance()["diskHits"],
        9,
        "Each block should be verified once"
    );

    // The second owned manifest authorizes the same immutable blocks. It has
    // neither a local disk cache nor a working remote source.
    let second = LazyDisk::create(
        &root.path().join("disks/second/lazy"),
        &manifest,
        Arc::new(Offline),
    )
    .unwrap();
    for index in 0..9 {
        second.read_at(index * BLOCK, &mut buffer).unwrap();
        assert!(buffer.iter().all(|&byte| byte == index as u8 + 1));
    }
    assert_eq!(second.performance()["memoryHits"], 9);

    let unrelated = tempfile::tempdir().unwrap();
    let third = LazyDisk::create(
        &unrelated.path().join("disks/third/lazy"),
        &manifest,
        Arc::new(Offline),
    )
    .unwrap();
    assert!(
        third.read_at(0, &mut buffer).is_err(),
        "Another node cannot use this node's cached plaintext"
    );

    let mut wrong_extent = manifest;
    wrong_extent["size"] = 4096.into();
    wrong_extent["blocks"].as_array_mut().unwrap().truncate(1);
    wrong_extent["blocks"][0]["size"] = 4096.into();
    let malformed = LazyDisk::create(
        &root.path().join("disks/malformed/lazy"),
        &wrong_extent,
        Arc::new(Offline),
    )
    .unwrap();
    assert!(
        malformed.read_at(0, &mut buffer).is_err(),
        "A cache hit cannot bypass extent verification"
    );
}

#[test]
fn an_unavailable_conversation_source_does_not_block_another_owned_source() {
    use std::sync::{Mutex, mpsc};
    use std::time::Duration;

    struct WaitingSource {
        started: mpsc::Sender<()>,
        release: Mutex<mpsc::Receiver<()>>,
    }

    impl BlockSource for WaitingSource {
        fn fetch(&self, _: &str) -> io::Result<Vec<u8>> {
            self.started.send(()).unwrap();
            self.release.lock().unwrap().recv().unwrap();
            Err(io::Error::other("Source remains unavailable"))
        }
    }

    struct AvailableSource;

    impl BlockSource for AvailableSource {
        fn fetch(&self, _: &str) -> io::Result<Vec<u8>> {
            Ok(vec![7; 4096])
        }
    }

    let root = tempfile::tempdir().unwrap();
    let hash = leo_agent_manager::storage::digest::block(&vec![7; 4096]);
    let manifest = json!({
        "version": 1, "size": 4096, "blockSize": 4 * 1024 * 1024,
        "blocks": [{"offset": 0, "size": 4096, "hash": hash}]
    });
    let (started, waiting) = mpsc::channel();
    let (release, released) = mpsc::channel();
    let first = LazyDisk::create(
        &root.path().join("disks/unavailable/lazy"),
        &manifest,
        Arc::new(WaitingSource {
            started,
            release: Mutex::new(released),
        }),
    )
    .unwrap();
    let second = LazyDisk::create(
        &root.path().join("disks/available/lazy"),
        &manifest,
        Arc::new(AvailableSource),
    )
    .unwrap();

    std::thread::scope(|scope| {
        let first = scope.spawn(|| first.read_at(0, &mut [0; 4096]));
        waiting.recv_timeout(Duration::from_secs(5)).unwrap();
        let (completed, result) = mpsc::channel();
        let second = scope.spawn(move || {
            let mut bytes = [0; 4096];
            let read = second.read_at(0, &mut bytes).map(|()| bytes);
            completed.send(read).unwrap();
        });
        let independent = result.recv_timeout(Duration::from_secs(1));
        // Always release and join the stalled source, including the failing
        // baseline, so this test cannot strand a worker indefinitely.
        release.send(()).unwrap();
        assert!(first.join().unwrap().is_err());
        second.join().unwrap();
        assert_eq!(
            independent
                .expect("A separate source must not inherit another conversation's wait")
                .unwrap(),
            [7; 4096]
        );
    });
}
