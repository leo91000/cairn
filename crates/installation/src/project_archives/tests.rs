use super::*;
use rand::{RngCore, SeedableRng};
use serde_json::json;
use std::{io::Write, time::Instant};

fn cpu_seconds() -> f64 {
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::uninit();
    assert_eq!(
        unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) },
        0
    );
    let usage = unsafe { usage.assume_init() };
    (usage.ru_utime.tv_sec + usage.ru_stime.tv_sec) as f64
        + (usage.ru_utime.tv_usec + usage.ru_stime.tv_usec) as f64 / 1_000_000.0
}

fn descriptor(bytes: &[u8]) -> Archive {
    Archive {
        project_id: crate::config::id(),
        revision: "a".repeat(40),
        origin_hash: None,
        key: "b".repeat(64),
        hash: blake3::hash(bytes).to_hex().to_string(),
        bytes: bytes.len() as u64,
        logical_bytes: 4096,
    }
}

#[tokio::test]
async fn native_initialization_extracts_pinned_archives_without_an_expanded_staging_tree() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source");
    std::fs::create_dir(&source).unwrap();
    std::fs::write(source.join("file"), b"project").unwrap();
    let archive = tempfile::NamedTempFile::new().unwrap();
    archive::pack(&source, archive.reopen().unwrap()).unwrap();
    let descriptor = descriptor(&std::fs::read(archive.path()).unwrap());
    let stage = root.path().join("stage");
    std::fs::create_dir_all(stage.join("project-archives")).unwrap();
    std::fs::create_dir(stage.join("workspace")).unwrap();
    std::fs::copy(
        archive.path(),
        stage
            .join("project-archives")
            .join(format!("{}.zip", descriptor.project_id)),
    )
    .unwrap();
    let plan = json!({ "nativeProjectArchives": [descriptor] });
    let output = root.path().join("workspaces");
    assert!(
        initialize(&plan, &stage.join("workspace"), &output, 4096)
            .await
            .unwrap()
    );
    assert_eq!(
        std::fs::read(output.join(&descriptor.project_id).join("file")).unwrap(),
        b"project"
    );
    assert!(
        !stage
            .join("workspace")
            .join(&descriptor.project_id)
            .join("file")
            .exists()
    );
}

#[tokio::test]
async fn cache_eviction_preserves_open_archives_and_pinned_conversation_seeds() {
    let root = tempfile::tempdir().unwrap();
    let data = root.path();
    std::fs::create_dir(data.join("project-archives")).unwrap();
    let bytes = b"immutable archive";
    let descriptor = descriptor(bytes);
    std::fs::write(descriptor.path(data), bytes).unwrap();
    let mut file = cached(data, &descriptor).await.unwrap().unwrap();
    let pinned = data.join("pinned.zip");
    std::fs::hard_link(descriptor.path(data), &pinned).unwrap();
    prune(data, CACHE_BYTES).await.unwrap();
    assert!(!descriptor.path(data).exists());
    let mut saved = Vec::new();
    std::io::Read::read_to_end(&mut file, &mut saved).unwrap();
    assert_eq!(saved, bytes);
    assert_eq!(std::fs::read(pinned).unwrap(), bytes);
}

#[tokio::test]
async fn pool_pressure_reclaims_only_unused_archives_even_below_the_cache_cap() {
    let root = tempfile::tempdir().unwrap();
    let cache = root.path().join("project-archives");
    std::fs::create_dir(&cache).unwrap();
    let old = cache.join("old.zip");
    let pinned = cache.join("pinned.zip");
    std::fs::write(&old, vec![1; 600]).unwrap();
    std::fs::write(&pinned, vec![2; 500]).unwrap();
    std::fs::hard_link(&pinned, root.path().join("run-pin.zip")).unwrap();
    // Scale the reviewed 150/600/200 MiB scenario down to bytes. Reclamation
    // is driven by the actual preparation deficit, despite a non-full cache.
    assert_eq!(reclaim(root.path(), 400 - 150).await.unwrap(), 600);
    assert!(!old.exists());
    assert!(pinned.exists());
    assert_eq!(reclaim(root.path(), 1).await.unwrap(), 0);
}

#[tokio::test]
async fn initial_configuration_and_legacy_transfers_reclaim_cache_only_when_needed() {
    let root = tempfile::tempdir().unwrap();
    let cache = root.path().join("project-archives");
    std::fs::create_dir(&cache).unwrap();
    std::fs::write(cache.join("unused.zip"), vec![1; 600]).unwrap();
    std::fs::write(cache.join("needed.zip"), vec![2; 100]).unwrap();
    let source = root.path().join("source");
    std::fs::create_dir_all(source.join("home")).unwrap();
    std::fs::write(source.join("home/config"), vec![3; 128]).unwrap();
    let mut frames = Vec::new();
    crate::nodes::files::send(&source, &mut frames)
        .await
        .unwrap();
    let keep = vec!["needed".to_owned()];
    let destination = root.path().join("received");
    crate::nodes::files::receive_with_reclaim(
        &mut frames.as_slice(),
        &destination,
        0,
        1024,
        |needed| {
            let (data, keep) = (root.path().to_owned(), keep.clone());
            async move { reclaim_for_foreground(&data, needed, &keep).await }
        },
    )
    .await
    .unwrap();
    assert_eq!(
        std::fs::read(destination.join("home/config")).unwrap(),
        vec![3; 128]
    );
    assert!(!cache.join("unused.zip").exists());
    assert!(cache.join("needed.zip").exists());
}

#[tokio::test]
async fn interrupted_downloads_release_their_budget_without_evicting_valid_objects() {
    let root = tempfile::tempdir().unwrap();
    let cache = root.path().join("project-archives");
    std::fs::create_dir(&cache).unwrap();
    std::fs::write(cache.join(".tmp-killed-download"), vec![1; 600]).unwrap();
    std::fs::write(cache.join("valid.zip"), vec![2; 100]).unwrap();
    assert_eq!(
        reclaim_for_foreground(root.path(), 128, &[]).await.unwrap(),
        600
    );
    assert!(!cache.join(".tmp-killed-download").exists());
    assert!(cache.join("valid.zip").exists());
}

#[tokio::test]
async fn checksum_verification_covers_zip_permissions_outside_the_payload_crc() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source");
    std::fs::create_dir(&source).unwrap();
    std::fs::write(source.join("script"), "#!/bin/sh\n").unwrap();
    std::fs::set_permissions(
        source.join("script"),
        std::fs::Permissions::from_mode(0o751),
    )
    .unwrap();
    let original = tempfile::NamedTempFile::new().unwrap();
    archive::pack(&source, original.reopen().unwrap()).unwrap();
    let mut bytes = std::fs::read(original.path()).unwrap();
    let descriptor = descriptor(&bytes);
    let central = bytes
        .windows(4)
        .position(|bytes| bytes == b"PK\x01\x02")
        .unwrap();
    bytes[central + 40] ^= 0o100; // Unix owner execute bit in external attributes.
    let damaged = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(damaged.path(), &bytes).unwrap();
    let unverified = root.path().join("unverified");
    archive::extract(damaged.reopen().unwrap(), &unverified, 4096).unwrap();
    assert_ne!(
        std::fs::metadata(unverified.join("script"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o751
    );
    assert!(
        extract_verified(
            damaged.reopen().unwrap(),
            &descriptor,
            &root.path().join("verified"),
            4096
        )
        .await
        .is_err()
    );
    assert!(!root.path().join("verified").exists());
}

#[tokio::test]
#[ignore = "reproducible local archive versus file-frame benchmark"]
async fn measure_project_archive_transfer() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source");
    std::fs::create_dir(&source).unwrap();
    for index in 0..4096 {
        std::fs::write(source.join(format!("file-{index:04}")), vec![b'a'; 4096]).unwrap();
    }
    let mut random = vec![0; 8 * 1024 * 1024];
    rand::rngs::StdRng::seed_from_u64(42).fill_bytes(&mut random);
    std::fs::File::create(source.join("binary"))
        .unwrap()
        .write_all(&random)
        .unwrap();
    let mut frames = Vec::new();
    let before = Instant::now();
    crate::nodes::files::send(&source, &mut frames)
        .await
        .unwrap();
    let legacy_encode = before.elapsed();
    let file = tempfile::NamedTempFile::new().unwrap();
    let before = Instant::now();
    archive::pack(&source, file.reopen().unwrap()).unwrap();
    let archive_encode = before.elapsed();
    for sample in 0..3 {
        let old_cpu = cpu_seconds();
        let before = Instant::now();
        crate::nodes::files::receive(
            &mut tokio::io::BufReader::new(frames.as_slice()),
            &root.path().join(format!("old-{sample}")),
            LOGICAL_BYTES,
        )
        .await
        .unwrap();
        let old = before.elapsed();
        let old_cpu = cpu_seconds() - old_cpu;
        let output = root.path().join(format!("new-{sample}"));
        let new_cpu = cpu_seconds();
        let before = Instant::now();
        extract(file.reopen().unwrap(), &output, LOGICAL_BYTES)
            .await
            .unwrap();
        crate::native::block::sync(&output).await.unwrap();
        let new = before.elapsed();
        let new_cpu = cpu_seconds() - new_cpu;
        println!(
            "{}",
            json!({"sample": sample, "legacyWriteMs": old.as_secs_f64()*1000.0, "archiveWriteMs": new.as_secs_f64()*1000.0, "legacyCpuSeconds": old_cpu, "archiveCpuSeconds": new_cpu})
        );
    }
    let before = Instant::now();
    let first_read = std::fs::read(file.path()).unwrap();
    let first_read_ms = before.elapsed().as_secs_f64() * 1000.0;
    let before = Instant::now();
    let warm_read = std::fs::read(file.path()).unwrap();
    let warm_read_ms = before.elapsed().as_secs_f64() * 1000.0;
    assert_eq!(first_read, warm_read);
    println!(
        "archive read first/warm ms: {first_read_ms:.3}/{warm_read_ms:.3}; both page-cache eligible"
    );
    println!(
        "{}",
        json!({"logicalBytes": 24*1024*1024, "legacyEncodedBytes": frames.len(), "archiveEncodedBytes": file.as_file().metadata().unwrap().len(), "legacyEncodeMs": legacy_encode.as_secs_f64()*1000.0, "archiveEncodeMs": archive_encode.as_secs_f64()*1000.0, "hostLoad": std::fs::read_to_string("/proc/loadavg").unwrap(), "workload": "4096 x 4KiB text + 8MiB seeded random (42); local page cache; no network"})
    );
}
