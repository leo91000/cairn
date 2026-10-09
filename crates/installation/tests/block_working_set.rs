use cairn_installation::storage::{BlockSource, Disk, LazyDisk};
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
fn publishing_a_foreground_block_keeps_its_verified_new_identity_local() {
    const BLOCK: u64 = 4 * 1024 * 1024;
    let root = tempfile::tempdir().unwrap();
    let directory = root.path().join("disks/conversation/lazy");
    let manifest = json!({
        "version": 1, "size": BLOCK, "blockSize": BLOCK,
        "blocks": [{ "offset": 0, "size": BLOCK, "hash": null }]
    });
    let disk = LazyDisk::create(&directory, &manifest, Arc::new(Offline)).unwrap();
    disk.write_at(0, &[37; 4096]).unwrap();
    let mut bytes = [0; 4096];
    disk.read_at(0, &mut bytes).unwrap();
    assert_eq!(bytes, [37; 4096]);

    // Final saves can reconstruct after the VM closes its mounted disk.
    let owner = LazyDisk::create(
        &root.path().join("disks/cache-owner/lazy"),
        &manifest,
        Arc::new(Offline),
    )
    .unwrap();
    drop(disk);
    let disk = LazyDisk::open(&directory, Arc::new(Offline)).unwrap();

    let generation = disk.seal().unwrap();
    disk.capture(generation).unwrap();
    disk.commit_published(generation, "published-backup")
        .unwrap();
    disk.read_at(0, &mut bytes)
        .expect("Publication must not download a block just read and uploaded on this node");
    assert_eq!(bytes, [37; 4096]);

    // Reopening the disk models the next VM while the same controller is alive.
    drop(disk);
    let resumed = LazyDisk::open(&directory, Arc::new(Offline)).unwrap();
    resumed.read_at(0, &mut bytes).unwrap();
    assert_eq!(bytes, [37; 4096]);
    assert_eq!(resumed.performance()["remoteFetch"]["count"], 0);
    drop(owner);
}

#[test]
fn repeated_publications_keep_hot_bytes_and_later_writes_without_admitting_cold_scans() {
    const BLOCK: u64 = 4 * 1024 * 1024;
    let root = tempfile::tempdir().unwrap();
    let manifest = json!({
        "version": 1, "size": 2 * BLOCK, "blockSize": BLOCK,
        "blocks": [
            { "offset": 0, "size": BLOCK, "hash": null },
            { "offset": BLOCK, "size": BLOCK, "hash": null }
        ]
    });
    let disk = LazyDisk::create(root.path(), &manifest, Arc::new(Offline)).unwrap();
    let mut bytes = [0; 4096];
    disk.write_at(BLOCK, &[91; 4096]).unwrap();

    for value in 1..=3 {
        disk.write_at(0, &[value; 4096]).unwrap();
        disk.read_at(0, &mut bytes).unwrap();
        assert_eq!(bytes, [value; 4096]);
        let generation = disk.seal().unwrap();
        let point = disk.capture(generation).unwrap();
        let hot = point["blocks"][0]["hash"].as_str().unwrap();
        let cold = point["blocks"][1]["hash"].as_str().unwrap();
        assert_eq!(
            &disk.captured_block(generation, hot).unwrap()[..4096],
            &[value; 4096]
        );
        // Upload scans must not populate the foreground cache.
        if value == 1 {
            assert_eq!(
                &disk.captured_block(generation, cold).unwrap()[..4096],
                &[91; 4096]
            );
        }
        assert_eq!(disk.performance()["blockCache"]["entries"], value);

        disk.write_at(2048, &[113; 1024]).unwrap();
        disk.commit_published(generation, &format!("backup-{generation}"))
            .unwrap();
        disk.read_at(0, &mut bytes).unwrap();
        assert_eq!(&bytes[..2048], &[value; 2048]);
        assert_eq!(&bytes[2048..3072], &[113; 1024]);
        assert_eq!(&bytes[3072..], &[value; 1024]);
    }
    assert_eq!(disk.performance()["remoteFetch"]["count"], 0);
    // Optional published disk bytes may survive; evict them to isolate the RAM
    // admission guarantee exercised by this test.
    std::fs::remove_dir_all(root.path().join("cache")).unwrap();
    assert!(
        disk.read_at(BLOCK, &mut bytes).is_err(),
        "The never-read cold block must remain absent from the RAM cache"
    );
}

#[test]
fn publication_after_a_smaller_cache_policy_retains_the_most_recent_extent() {
    const BLOCK: u64 = 4 * 1024 * 1024;
    let root = tempfile::tempdir().unwrap();
    let manifest = json!({
        "version": 1, "size": 3 * BLOCK, "blockSize": BLOCK,
        "blocks": [
            { "offset": 0, "size": BLOCK, "hash": null },
            { "offset": BLOCK, "size": BLOCK, "hash": null },
            { "offset": 2 * BLOCK, "size": BLOCK, "hash": null }
        ]
    });
    let disk = LazyDisk::create(root.path(), &manifest, Arc::new(Offline)).unwrap();
    for index in 0..3 {
        disk.write_at(index * BLOCK, &[index as u8 + 1; 4096])
            .unwrap();
        disk.read_at(index * BLOCK, &mut [0; 4096]).unwrap();
    }
    disk.read_at(0, &mut [0; 4096]).unwrap();
    disk.set_context(&json!({"policy": {"memoryCacheMiB": 4}}))
        .unwrap();
    let generation = disk.seal().unwrap();
    disk.capture(generation).unwrap();
    disk.commit_published(generation, "smaller-budget").unwrap();
    assert_eq!(disk.performance()["blockCache"]["bytes"], BLOCK);
    std::fs::remove_dir_all(root.path().join("cache")).unwrap();
    let mut bytes = [0; 4096];
    disk.read_at(0, &mut bytes).unwrap();
    assert_eq!(bytes, [1; 4096]);
    assert!(disk.read_at(BLOCK, &mut bytes).is_err());
    assert!(disk.read_at(2 * BLOCK, &mut bytes).is_err());
}

#[tokio::test]
async fn controller_retains_verified_blocks_between_conversations_and_releases_them_on_shutdown() {
    use cairn_installation::microvm::pool::Pool;
    use tokio_util::sync::CancellationToken;

    let root = tempfile::tempdir().unwrap();
    let stop = CancellationToken::new();
    let controller = Pool::new(
        root.path().into(),
        root.path().join("fixture-image"),
        stop,
        4,
    )
    .await
    .unwrap();
    let bytes = vec![37; 4 * 1024 * 1024];
    let hash = hex::encode(Sha256::digest(&bytes));
    let manifest = json!({
        "version": 1,
        "size": bytes.len(),
        "blockSize": bytes.len(),
        "blocks": [{ "offset": 0, "size": bytes.len(), "hash": hash }]
    });
    let first_directory = root.path().join("disks/first/lazy");
    let first = LazyDisk::create(&first_directory, &manifest, Arc::new(Offline)).unwrap();
    std::fs::create_dir(first_directory.join("cache")).unwrap();
    let file = first_directory.join("cache").join(&hash);
    std::fs::write(&file, bytes).unwrap();
    let mut buffer = [0; 4096];
    first.read_at(0, &mut buffer).unwrap();
    assert_eq!(buffer, [37; 4096]);
    std::fs::remove_file(file).unwrap();
    drop(first);

    let second = LazyDisk::create(
        &root.path().join("disks/second/lazy"),
        &manifest,
        Arc::new(Offline),
    )
    .unwrap();
    second.read_at(0, &mut buffer).unwrap();
    assert_eq!(buffer, [37; 4096]);
    assert_eq!(second.performance()["memoryHits"], 1);
    drop(second);
    drop(controller);

    let after_shutdown = LazyDisk::create(
        &root.path().join("disks/after-shutdown/lazy"),
        &manifest,
        Arc::new(Offline),
    )
    .unwrap();
    assert!(after_shutdown.read_at(0, &mut buffer).is_err());
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
    let hash = cairn_installation::storage::digest::block(&vec![7; 4096]);
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
