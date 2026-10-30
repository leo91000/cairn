use super::{bases::*, disk};
use crate::{
    project_archives::Archive,
    skills::private_dir,
    storage::{self, Disk},
};
use serde_json::json;
use std::{io::Write, os::unix::fs::MetadataExt};
use tokio_util::sync::CancellationToken;

async fn fixture() -> (tempfile::TempDir, Bases, Archive, storage::policy::Policy) {
    let root = tempfile::tempdir().unwrap();
    let bases = Bases {
        data: root.path().join("data"),
        state: root.path().join("state"),
        identity: (unsafe { libc::geteuid() }, unsafe { libc::getegid() }),
    };
    private_dir(&bases.data.join("project-archives"))
        .await
        .unwrap();
    private_dir(&bases.state).await.unwrap();
    let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    zip.start_file("file", zip::write::SimpleFileOptions::default())
        .unwrap();
    zip.write_all(b"initial project").unwrap();
    let bytes = zip.finish().unwrap().into_inner();
    let hash = blake3::hash(&bytes).to_hex().to_string();
    std::fs::write(
        bases
            .data
            .join("project-archives")
            .join(format!("{hash}.zip")),
        &bytes,
    )
    .unwrap();
    let archive = Archive {
        project_id: crate::config::id(),
        revision: "a".repeat(40),
        origin_hash: None,
        key: "b".repeat(64),
        hash,
        bytes: bytes.len() as u64,
        logical_bytes: 15,
    };
    let policy = serde_json::from_value(json!({ "reserveMiB": 64, "reservePercent": 1 })).unwrap();
    (root, bases, archive, policy)
}

#[tokio::test]
async fn preparation_pins_are_attempt_scoped_and_recovered_after_fencing() {
    let root = tempfile::tempdir().unwrap();
    let data = root.path();
    let run = crate::config::id();
    let stage = data.join("runs").join(&run);
    private_dir(&stage).await.unwrap();
    private_dir(&data.join("runner-plans")).await.unwrap();
    let first = crate::config::id();
    let second = crate::config::id();
    let source = data.join("pin");
    tokio::fs::write(&source, b"").await.unwrap();
    for attempt in [&first, &second] {
        tokio::fs::hard_link(&source, pin_path(&stage, attempt).unwrap())
            .await
            .unwrap();
        tokio::fs::write(
            data.join("runner-plans").join(format!("{attempt}.json")),
            serde_json::to_vec(&json!({ "runId": run })).unwrap(),
        )
        .await
        .unwrap();
    }
    release_attempt(data, &first).await.unwrap();
    assert!(!pin_path(&stage, &first).unwrap().exists());
    assert!(pin_path(&stage, &second).unwrap().exists());
    {
        let _cancelled = PendingPin::new(&stage, &second).unwrap();
    }
    assert!(!pin_path(&stage, &second).unwrap().exists());
    tokio::fs::hard_link(&source, pin_path(&stage, &second).unwrap())
        .await
        .unwrap();
    let mut prepared = PendingPin::new(&stage, &second).unwrap();
    prepared.keep();
    drop(prepared);
    assert!(pin_path(&stage, &second).unwrap().exists());
    tokio::fs::write(stage.join("native-base-invalid.pin"), b"unrelated")
        .await
        .unwrap();
    recover_pins(data).await.unwrap();
    assert!(!pin_path(&stage, &second).unwrap().exists());
    assert!(stage.join("native-base-invalid.pin").exists());
    assert_eq!(std::fs::metadata(source).unwrap().nlink(), 1);
}

#[tokio::test]
async fn prepared_ext4_base_adopts_private_authority_and_preserves_existing_conversations() {
    let (_root, bases, archive, policy) = fixture().await;
    let archives = vec![archive.clone()];
    let stop = CancellationToken::new();
    bases
        .warm(&archives, 128, 16 * 1024 * 1024 * 1024, &policy, &stop)
        .await
        .unwrap();
    let ready = bases.lookup(&archives, 128).await.unwrap().unwrap();
    assert!(bases.lookup(&archives, 256).await.unwrap().is_none());
    let other_identity = Bases {
        identity: (bases.identity.0 + 1, bases.identity.1),
        ..bases.clone()
    };
    assert!(
        other_identity
            .lookup(&archives, 128)
            .await
            .unwrap()
            .is_none()
    );
    let source = bases.state.join("native-bases").join(&ready.key);
    assert_eq!(
        storage::LazyDisk::context(&source.join("lazy")).unwrap()["unassigned"],
        true
    );
    let run = crate::config::id();
    let attempt = crate::config::id();
    let plan = json!({
        "id": attempt,
        "runId": run,
        "runtimeId": "a".repeat(40),
        "resources": { "diskMiB": 128 },
        "nativeProjectArchives": archives,
        "nativeProjectBase": ready.key,
        "storage": {
            "master": "http://127.0.0.1:1/",
            "grant": "first-owner",
            "policy": policy
        }
    });
    private_dir(&bases.data.join("runs").join(&run))
        .await
        .unwrap();
    tokio::fs::hard_link(
        source.join("pin"),
        pin_path(&bases.data.join("runs").join(&run), &attempt).unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(bases.trim(0, u64::MAX, None).await.unwrap(), 0);
    assert!(bases.initialize(&plan, &stop).await.unwrap());
    let directory = storage::environment::directory(&bases.state, &run).unwrap();
    assert_eq!(directory, bases.state.join("disks").join(&run));
    assert!(!directory.join("environment").exists());
    assert!(!directory.join("owner").exists());
    assert!(!directory.join("restore.pending").exists());
    assert!(
        !bases
            .data
            .join("runs")
            .join(&run)
            .join(format!("native-base-{attempt}.pin"))
            .exists()
    );
    let volume = storage::runtime::load(&directory).await.unwrap();
    assert_eq!(
        volume.source.grant_id().unwrap(),
        crate::auth::digest("first-owner")
    );
    // Read the real ext4 image through the production journal, without a mount.
    let raw = directory.join("verify.ext4");
    storage::export(volume.disk.as_ref(), &raw).unwrap();
    let result = tokio::process::Command::new("debugfs")
        .arg("-R")
        .arg(format!("cat /workspaces/{}/file", archive.project_id))
        .arg(&raw)
        .output()
        .await
        .unwrap();
    assert!(result.status.success());
    assert_eq!(result.stdout, b"initial project");
    tokio::fs::remove_file(raw).await.unwrap();
    let segment = std::fs::read_dir(source.join("lazy"))
        .unwrap()
        .map(Result::unwrap)
        .find(|entry| entry.file_name().to_string_lossy().starts_with("payload-"))
        .unwrap()
        .file_name();
    assert_eq!(
        std::fs::metadata(source.join("lazy").join(&segment))
            .unwrap()
            .ino(),
        std::fs::metadata(directory.join("lazy").join(&segment))
            .unwrap()
            .ino()
    );
    assert!(!bases.initialize(&plan, &stop).await.unwrap());
    // A conventional committed disk is readable without running environment
    // claim recovery, including when interrupted before the ownership hint.
    tokio::fs::remove_file(
        bases
            .state
            .join("native")
            .join(&run)
            .join("file-owner.json"),
    )
    .await
    .unwrap();
    bases.trim(0, u64::MAX, None).await.unwrap();
    let mut legacy_plan = plan.clone();
    legacy_plan
        .as_object_mut()
        .unwrap()
        .remove("nativeProjectArchives");
    legacy_plan
        .as_object_mut()
        .unwrap()
        .remove("nativeProjectBase");
    assert!(!bases.initialize(&legacy_plan, &stop).await.unwrap());
    assert!(!directory.join("restore.pending").exists());
    assert!(!source.exists());
    let mut magic = [0; 2];
    volume.disk.read_at(1080, &mut magic).unwrap();
    assert_eq!(magic, [0x53, 0xef]);
}

#[tokio::test]
async fn tiny_file_archives_reserve_allocated_blocks_before_extraction() {
    let (_root, bases, mut archive, policy) = fixture().await;
    let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    for index in 0..8192 {
        zip.start_file(
            format!("tiny-{index}"),
            zip::write::SimpleFileOptions::default(),
        )
        .unwrap();
        zip.write_all(b"x").unwrap();
    }
    let bytes = zip.finish().unwrap().into_inner();
    tokio::fs::remove_file(
        bases
            .data
            .join("project-archives")
            .join(format!("{}.zip", archive.hash)),
    )
    .await
    .unwrap();
    archive.hash = blake3::hash(&bytes).to_hex().to_string();
    archive.bytes = bytes.len() as u64;
    archive.logical_bytes = 8192;
    tokio::fs::write(
        bases
            .data
            .join("project-archives")
            .join(format!("{}.zip", archive.hash)),
        bytes,
    )
    .await
    .unwrap();
    let limit = 4 * 1024 * disk::MIB;
    // Logical bytes alone would admit a 520 MiB temporary reservation. Rounded
    // entry allocation requires more than the remaining 600 MiB, before mkfs.
    let occupied = tokio::fs::File::create(bases.data.join("other-retained-work"))
        .await
        .unwrap();
    occupied.set_len(limit - 600 * disk::MIB).await.unwrap();
    let stop = CancellationToken::new();
    bases
        .warm(&[archive.clone()], 128, limit, &policy, &stop)
        .await
        .unwrap();
    assert!(bases.lookup(&[archive], 128).await.unwrap().is_none());
    assert!(!bases.state.join("environments").exists());
    assert!(disk::used(&bases.data, &bases.state).await.unwrap() < limit);
}

#[tokio::test]
async fn changed_cache_payload_is_rejected_before_a_conversation_can_claim_it() {
    let (_root, bases, archive, policy) = fixture().await;
    let archives = vec![archive];
    bases
        .warm(
            &archives,
            128,
            16 * 1024 * 1024 * 1024,
            &policy,
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    let ready = bases.lookup(&archives, 128).await.unwrap().unwrap();
    let source = bases.state.join("native-bases").join(ready.key);
    let payload = std::fs::read_dir(source.join("lazy"))
        .unwrap()
        .map(Result::unwrap)
        .find(|entry| entry.file_name().to_string_lossy().starts_with("payload-"))
        .unwrap()
        .path();
    let mut bytes = std::fs::read(&payload).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 0xff;
    std::fs::write(&payload, &bytes).unwrap();
    assert!(bases.lookup(&archives, 128).await.unwrap().is_none());
    assert!(!source.join("ready.json").exists());
    assert_eq!(
        std::fs::metadata(payload).unwrap().len(),
        bytes.len() as u64
    );
}

#[test]
fn background_preparation_yields_to_foreground_intent_and_then_finishes() {
    crate::test_process::run_ignored(
        "native::bases_tests::background_preparation_yields_to_foreground_intent_and_then_finishes_child",
    );
}

#[tokio::test]
#[ignore = "executed by the isolated background preparation wrapper"]
async fn background_preparation_yields_to_foreground_intent_and_then_finishes_child() {
    use std::{sync::Arc, time::Duration};

    // The isolated fixture is the only writer in this directory. Track actual
    // build progress so slow fsyncs do not consume one deadline for the entire
    // image and journal, while a stalled or abandoned job still fails promptly.
    fn activity(
        path: &std::path::Path,
        entries: &mut Vec<(std::path::PathBuf, u64, std::time::SystemTime)>,
    ) {
        let Ok(children) = std::fs::read_dir(path) else {
            return;
        };
        for child in children.flatten() {
            let path = child.path();
            let Ok(metadata) = child.metadata() else {
                continue;
            };
            if let Ok(modified) = metadata.modified() {
                entries.push((path.clone(), metadata.len(), modified));
            }
            if metadata.is_dir() {
                activity(&path, entries);
            }
        }
    }
    let (_root, bases, archive, policy) = fixture().await;
    super::atomic_write(
        &bases.state.join("storage-policy.json"),
        &serde_json::to_vec(&policy).unwrap(),
    )
    .await
    .unwrap();
    let foreground = crate::file_lock::shared(
        &bases.state.join("storage-foreground.lock"),
        "test foreground",
    )
    .unwrap();
    let warmer = Arc::new(super::bases_warmer::Warmer::default());
    let budget = Arc::new(tokio::sync::Mutex::new(
        json!({ "limits": { "diskMiB": 16384 } }),
    ));
    let stop = CancellationToken::new();
    warmer
        .enqueue(
            bases.clone(),
            json!({ "archives": [archive.clone()], "diskMiB": 128 }),
            budget,
            stop.clone(),
        )
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(120)).await;
    assert!(!bases.state.join("environments").exists());
    assert!(
        crate::file_lock::exclusive(
            &bases.state.join("storage.lock"),
            "foreground must remain available"
        )
        .is_ok()
    );
    drop(foreground);
    // A separate overall guard prevents a job that writes forever from passing.
    // The 60 s ceiling also accommodates the measured 27 s slow-sync build.
    tokio::time::timeout(Duration::from_secs(60), async {
        let mut last_activity = Vec::new();
        let mut progress_deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            if bases
                .lookup(std::slice::from_ref(&archive), 128)
                .await
                .unwrap()
                .is_some()
            {
                break;
            }
            let mut current_activity = Vec::new();
            activity(&bases.state.join("environments"), &mut current_activity);
            current_activity.sort();
            if current_activity != last_activity {
                last_activity = current_activity;
                progress_deadline = tokio::time::Instant::now() + Duration::from_secs(10);
            }
            assert!(
                tokio::time::Instant::now() < progress_deadline,
                "native base warming made no filesystem progress for 10 seconds"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    stop.cancel();
    warmer.wait_idle().await;
}

#[tokio::test]
async fn active_background_build_releases_its_temporary_files_before_foreground_admission() {
    use std::{sync::Arc, time::Duration};
    let (_root, bases, archive, policy) = fixture().await;
    super::atomic_write(
        &bases.state.join("storage-policy.json"),
        &serde_json::to_vec(&policy).unwrap(),
    )
    .await
    .unwrap();
    let warmer = Arc::new(super::bases_warmer::Warmer::default());
    let budget = Arc::new(tokio::sync::Mutex::new(
        json!({ "limits": { "diskMiB": 16384 } }),
    ));
    let stop = CancellationToken::new();
    warmer
        .enqueue(
            bases.clone(),
            json!({ "archives": [archive.clone()], "diskMiB": 128 }),
            budget,
            stop.clone(),
        )
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if std::fs::read_dir(bases.state.join("environments"))
                .is_ok_and(|mut entries| entries.next().is_some())
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .unwrap();
    let foreground = tokio::time::timeout(Duration::from_secs(5), super::disk::lock(&bases.state))
        .await
        .unwrap()
        .unwrap();
    assert!(
        bases
            .lookup(std::slice::from_ref(&archive), 128)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        std::fs::read_dir(bases.state.join("environments"))
            .unwrap()
            .count(),
        0
    );
    stop.cancel();
    drop(foreground);
    warmer.wait_idle().await;
}

#[tokio::test]
#[ignore = "reproducible local ZIP extraction versus prepared native base benchmark"]
async fn measure_prepared_native_bases() {
    use rand::{RngCore, SeedableRng};
    use std::{os::fd::AsRawFd, time::Instant};
    fn cpu_ms() -> f64 {
        let mut time = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        assert_eq!(
            unsafe { libc::clock_gettime(libc::CLOCK_PROCESS_CPUTIME_ID, &mut time) },
            0
        );
        time.tv_sec as f64 * 1000.0 + time.tv_nsec as f64 / 1_000_000.0
    }
    let (root, bases, mut archive, policy) = fixture().await;
    let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .compression_level(Some(1));
    zip.add_directory("text/", options).unwrap();
    let text = b"cairn prepared native base measurement\n".repeat(120);
    for index in 0..4096 {
        zip.start_file(format!("text/{index}"), options).unwrap();
        zip.write_all(&text[..4096]).unwrap();
    }
    let mut random = vec![0u8; 8 * 1024 * 1024];
    rand::rngs::StdRng::seed_from_u64(42).fill_bytes(&mut random);
    zip.start_file("asset", options).unwrap();
    zip.write_all(&random).unwrap();
    let bytes = zip.finish().unwrap().into_inner();
    archive.hash = blake3::hash(&bytes).to_hex().to_string();
    archive.bytes = bytes.len() as u64;
    archive.logical_bytes = 24 * 1024 * 1024;
    std::fs::write(
        bases
            .data
            .join("project-archives")
            .join(format!("{}.zip", archive.hash)),
        bytes,
    )
    .unwrap();
    let archives = vec![archive.clone()];
    let stop = CancellationToken::new();
    let begin = Instant::now();
    let cpu = cpu_ms();
    bases
        .warm(&archives, 256, 32 * 1024 * 1024 * 1024, &policy, &stop)
        .await
        .unwrap();
    let warm = json!({
        "wallMs": begin.elapsed().as_secs_f64() * 1000.0,
        "cpuMs": cpu_ms() - cpu
    });
    let ready = bases.lookup(&archives, 256).await.unwrap().unwrap();
    let cache = bases.state.join("native-bases").join(&ready.key);
    let mut samples = Vec::new();
    for index in 0..3 {
        let extracted = root.path().join(format!("zip-{index}"));
        let begin = Instant::now();
        let cpu = cpu_ms();
        crate::project_archives::extract_cached(&bases.data, &archive, &extracted, &stop)
            .await
            .unwrap();
        super::workspace::own_tree(&extracted, bases.identity.0, bases.identity.1)
            .await
            .unwrap();
        super::block::sync(&extracted).await.unwrap();
        let zip_sample = json!({
            "wallMs": begin.elapsed().as_secs_f64() * 1000.0,
            "cpuMs": cpu_ms() - cpu
        });
        let run = crate::config::id();
        let plan = json!({
            "id": crate::config::id(),
            "runId": run,
            "runtimeId": "a".repeat(40),
            "resources": { "diskMiB": 256 },
            "nativeProjectArchives": archives,
            "nativeProjectBase": ready.key,
            "storage": {
                "master": "http://127.0.0.1:1/",
                "grant": format!("probe-{index}"),
                "policy": policy
            }
        });
        let begin = Instant::now();
        let cpu = cpu_ms();
        assert!(bases.initialize(&plan, &stop).await.unwrap());
        let physical = storage::environment::directory(&bases.state, &run).unwrap();
        storage::bootstrap::prepare_native(&physical, 256 * 1024 * 1024, &plan["storage"], &stop)
            .await
            .unwrap();
        let volume = storage::runtime::load(&physical).await.unwrap();
        let clone_sample = json!({
            "wallMs": begin.elapsed().as_secs_f64() * 1000.0,
            "cpuMs": cpu_ms() - cpu
        });
        let private_bytes = super::bases::reclaimable_bytes(&physical).unwrap()
            + super::workspace::measure(&bases.state.join("native").join(&run), true).unwrap();
        for entry in std::fs::read_dir(physical.join("lazy")).unwrap() {
            let entry = entry.unwrap();
            if entry.file_name().to_string_lossy().starts_with("payload-") {
                let file = std::fs::File::open(entry.path()).unwrap();
                assert_eq!(
                    unsafe {
                        libc::posix_fadvise(file.as_raw_fd(), 0, 0, libc::POSIX_FADV_DONTNEED)
                    },
                    0
                );
            }
        }
        let mut reads = Vec::new();
        let mut buffer = vec![0; 4 * 1024 * 1024];
        for _ in 0..2 {
            let begin = Instant::now();
            let cpu = cpu_ms();
            volume.disk.read_at(0, &mut buffer).unwrap();
            reads.push(json!({
                "wallMs": begin.elapsed().as_secs_f64() * 1000.0,
                "cpuMs": cpu_ms() - cpu
            }));
        }
        let begin = Instant::now();
        let cpu = cpu_ms();
        volume
            .disk
            .write_at(64 * 1024 * 1024, &[0x5a; 4096])
            .unwrap();
        volume.disk.sync().unwrap();
        let write = json!({
            "wallMs": begin.elapsed().as_secs_f64() * 1000.0,
            "cpuMs": cpu_ms() - cpu
        });
        samples.push(json!({
            "zip": zip_sample,
            "clone": clone_sample,
            "privateBytesBeforeDelta": private_bytes,
            "read4MiBColdHintThenWarm": reads,
            "write4KiBDelta": write
        }));
        drop(volume);
        tokio::fs::remove_dir_all(extracted).await.unwrap();
    }
    println!(
        "{}",
        json!({
            "logicalBytes": archive.logical_bytes, "zipEncodedBytes": archive.bytes,
            "baseAllocatedBytes": super::workspace::measure(&cache, true).unwrap(),
            "basePreparation": warm, "samples": samples,
            "hostLoad": std::fs::read_to_string("/proc/loadavg").unwrap(),
            "conditions": "4096x4KiB text + seeded random42 8MiB; local host filesystem; cached ZIP/base; no ublk mount or WAN. ZIP final syncfs can include unrelated host writes. Clone includes durable metadata, atomic journal publication, geometry and full journal open. Reads use advisory DONTNEED; writes probe a disposable raw journal block.",
        })
    );
}
