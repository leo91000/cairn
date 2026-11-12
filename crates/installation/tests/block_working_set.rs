use cairn_installation::storage::{Disk, LazyDisk};
use serde_json::json;
use std::{collections::HashMap, io};

fn local_block(directory: &std::path::Path, hash: &str, bytes: &[u8]) {
    std::fs::create_dir_all(directory.join("cache")).unwrap();
    std::fs::write(directory.join("cache").join(hash), bytes).unwrap();
}

#[test]
fn consolidating_a_foreground_block_keeps_its_verified_new_identity_local() {
    const BLOCK: u64 = 4 * 1024 * 1024;
    let root = tempfile::tempdir().unwrap();
    let directory = root.path().join("disks/conversation/lazy");
    let manifest = json!({
        "version": 1, "size": BLOCK, "blockSize": BLOCK,
        "blocks": [{ "offset": 0, "size": BLOCK, "hash": null }]
    });
    let disk = LazyDisk::create(&directory, &manifest).unwrap();
    disk.write_at(0, &[37; 4096]).unwrap();
    let mut bytes = [0; 4096];
    disk.read_at(0, &mut bytes).unwrap();
    assert_eq!(bytes, [37; 4096]);

    // Final saves can reconstruct after the VM closes its mounted disk.
    let owner = LazyDisk::create(&root.path().join("disks/cache-owner/lazy"), &manifest).unwrap();
    drop(disk);
    let disk = LazyDisk::open(&directory).unwrap();

    let generation = disk.seal().unwrap();
    assert_eq!(disk.consolidate().unwrap(), Some(generation));
    disk.read_at(0, &mut bytes)
        .expect("Consolidation preserves the local foreground bytes");
    assert_eq!(bytes, [37; 4096]);

    // Reopening the disk models the next VM while the same controller is alive.
    drop(disk);
    let resumed = LazyDisk::open(&directory).unwrap();
    resumed.read_at(0, &mut bytes).unwrap();
    assert_eq!(bytes, [37; 4096]);

    drop(owner);
}

#[test]
fn repeated_consolidations_keep_hot_bytes_and_later_writes_without_admitting_cold_scans() {
    const BLOCK: u64 = 4 * 1024 * 1024;
    let root = tempfile::tempdir().unwrap();
    let manifest = json!({
        "version": 1, "size": 2 * BLOCK, "blockSize": BLOCK,
        "blocks": [
            { "offset": 0, "size": BLOCK, "hash": null },
            { "offset": BLOCK, "size": BLOCK, "hash": null }
        ]
    });
    let disk = LazyDisk::create(root.path(), &manifest).unwrap();
    let mut bytes = [0; 4096];
    disk.write_at(BLOCK, &[91; 4096]).unwrap();

    for value in 1..=3 {
        disk.write_at(0, &[value; 4096]).unwrap();
        disk.read_at(0, &mut bytes).unwrap();
        assert_eq!(bytes, [value; 4096]);
        let generation = disk.seal().unwrap();
        disk.write_at(2048, &[113; 1024]).unwrap();
        assert_eq!(disk.consolidate().unwrap(), Some(generation));
        let point = local_manifest(root.path());
        let hot = point["blocks"][0]["hash"].as_str().unwrap();
        let cold = point["blocks"][1]["hash"].as_str().unwrap();
        assert_eq!(
            &std::fs::read(root.path().join("cache").join(hot)).unwrap()[..4096],
            &[value; 4096]
        );
        // Upload scans must not populate the foreground cache.
        if value == 1 {
            assert_eq!(
                &std::fs::read(root.path().join("cache").join(cold)).unwrap()[..4096],
                &[91; 4096]
            );
        }
        assert_eq!(disk.performance()["blockCache"]["entries"], value);

        disk.read_at(0, &mut bytes).unwrap();
        assert_eq!(&bytes[..2048], &[value; 2048]);
        assert_eq!(&bytes[2048..3072], &[113; 1024]);
        assert_eq!(&bytes[3072..], &[value; 1024]);
    }

    // Optional published disk bytes may survive; evict them to isolate the RAM
    // admission guarantee exercised by this test.
    std::fs::remove_dir_all(root.path().join("cache")).unwrap();
    assert!(
        disk.read_at(BLOCK, &mut bytes).is_err(),
        "The never-read cold block must remain absent from the RAM cache"
    );
}

#[test]
fn consolidation_after_a_smaller_cache_policy_retains_the_most_recent_extent() {
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
    let disk = LazyDisk::create(root.path(), &manifest).unwrap();
    for index in 0..3 {
        disk.write_at(index * BLOCK, &[index as u8 + 1; 4096])
            .unwrap();
        disk.read_at(index * BLOCK, &mut [0; 4096]).unwrap();
    }
    disk.read_at(0, &mut [0; 4096]).unwrap();
    disk.set_context(&json!({"policy": {"memoryCacheMiB": 4}}))
        .unwrap();
    let generation = disk.seal().unwrap();
    assert_eq!(disk.consolidate().unwrap(), Some(generation));
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
    let hash = cairn_installation::storage::digest::block(&bytes);
    let manifest = json!({
        "version": 1,
        "size": bytes.len(),
        "blockSize": bytes.len(),
        "blocks": [{ "offset": 0, "size": bytes.len(), "hash": hash }]
    });
    let first_directory = root.path().join("disks/first/lazy");
    local_block(&first_directory, &hash, &bytes);
    let first = LazyDisk::create(&first_directory, &manifest).unwrap();
    let file = first_directory.join("cache").join(&hash);
    let mut buffer = [0; 4096];
    first.read_at(0, &mut buffer).unwrap();
    assert_eq!(buffer, [37; 4096]);
    std::fs::remove_file(file).unwrap();
    drop(first);

    let second_directory = root.path().join("disks/second/lazy");
    local_block(&second_directory, &hash, &bytes);
    let second = LazyDisk::create(&second_directory, &manifest).unwrap();
    std::fs::remove_file(second_directory.join("cache").join(&hash)).unwrap();
    second.read_at(0, &mut buffer).unwrap();
    assert_eq!(buffer, [37; 4096]);
    assert_eq!(second.performance()["memoryHits"], 1);
    drop(second);
    drop(controller);

    let after_directory = root.path().join("disks/after-shutdown/lazy");
    local_block(&after_directory, &hash, &bytes);
    let after_shutdown = LazyDisk::create(&after_directory, &manifest).unwrap();
    std::fs::remove_file(after_directory.join("cache").join(&hash)).unwrap();
    assert_eq!(
        after_shutdown.read_at(0, &mut buffer).unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
}

#[test]
fn small_interleaved_reads_reuse_verified_blocks_across_conversation_disks() {
    const BLOCK: u64 = 4 * 1024 * 1024;
    let root = tempfile::tempdir().unwrap();
    let mut blocks = Vec::new();
    let mut data = HashMap::new();
    for index in 0..9 {
        let bytes = vec![index as u8 + 1; BLOCK as usize];
        let hash = cairn_installation::storage::digest::block(&bytes);
        blocks.push(json!({"offset": index * BLOCK, "size": BLOCK, "hash": hash}));
        data.insert(hash, bytes);
    }
    let manifest = json!({
        "version": 1, "size": 9 * BLOCK, "blockSize": BLOCK, "blocks": blocks
    });
    let directory = root.path().join("disks/first/lazy");
    for (hash, bytes) in &data {
        local_block(&directory, hash, bytes);
    }
    let first = LazyDisk::create(&directory, &manifest).unwrap();
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

    // The second disk owns its local base, and may reuse verified node RAM.
    let second_directory = root.path().join("disks/second/lazy");
    for (hash, bytes) in &data {
        local_block(&second_directory, hash, bytes);
    }
    let second = LazyDisk::create(&second_directory, &manifest).unwrap();
    for index in 0..9 {
        second.read_at(index * BLOCK, &mut buffer).unwrap();
        assert!(buffer.iter().all(|&byte| byte == index as u8 + 1));
    }
    assert_eq!(second.performance()["memoryHits"], 9);

    let unrelated = tempfile::tempdir().unwrap();
    assert!(
        LazyDisk::create(&unrelated.path().join("disks/third/lazy"), &manifest,).is_err(),
        "Another node cannot mount a base it does not hold"
    );

    let mut wrong_extent = manifest;
    wrong_extent["size"] = 4096.into();
    wrong_extent["blocks"].as_array_mut().unwrap().truncate(1);
    wrong_extent["blocks"][0]["size"] = 4096.into();
    let malformed_directory = root.path().join("disks/malformed/lazy");
    let hash = wrong_extent["blocks"][0]["hash"].as_str().unwrap();
    local_block(&malformed_directory, hash, &data[hash]);
    assert!(
        LazyDisk::create(&malformed_directory, &wrong_extent).is_err(),
        "A memory cache hit cannot bypass owned-base extent verification"
    );
}

#[test]
fn a_missing_local_base_is_refused_without_blocking_another_owned_disk() {
    let root = tempfile::tempdir().unwrap();
    let bytes = vec![7; 4096];
    let hash = cairn_installation::storage::digest::block(&bytes);
    let manifest = json!({
        "version": 1, "size": 4096, "blockSize": 4 * 1024 * 1024,
        "blocks": [{ "offset": 0, "size": 4096, "hash": hash }]
    });
    let missing = LazyDisk::create(&root.path().join("disks/missing/lazy"), &manifest);
    assert_eq!(missing.err().unwrap().kind(), io::ErrorKind::InvalidData);
    let directory = root.path().join("disks/available/lazy");
    local_block(&directory, &hash, &bytes);
    let owned = LazyDisk::create(&directory, &manifest).unwrap();
    let mut buffer = [0; 4096];
    owned.read_at(0, &mut buffer).unwrap();
    assert_eq!(buffer, [7; 4096]);
}

fn local_manifest(directory: &std::path::Path) -> serde_json::Value {
    let db = rusqlite::Connection::open_with_flags(
        directory.join("journal-v2.sqlite"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let encoded: String = db
        .query_row("SELECT manifest FROM state WHERE id=1", [], |row| {
            row.get(0)
        })
        .unwrap();
    serde_json::from_str(&encoded).unwrap()
}
