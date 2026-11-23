use super::{bases::*, disk};
use crate::{
    project_archives::Archive,
    skills::private_dir,
    storage::{self},
};
use serde_json::json;
use std::{io::Write, os::unix::fs::MetadataExt, path::Path};
use tokio_util::sync::CancellationToken;

async fn fixture() -> (tempfile::TempDir, Bases, Archive, storage::policy::Policy) {
    let root = tempfile::tempdir().unwrap();
    let bases = Bases {
        data: root.path().join("data"),
        state: root.path().join("state"),
        identity: (unsafe { libc::geteuid() }, unsafe { libc::getegid() }),
        build_entered: None,
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
async fn prepared_directory_base_is_verified_and_private_copies_survive_cache_reclamation() {
    let (_root, bases, archive, policy) = fixture().await;
    let archives = vec![archive.clone()];
    let stop = CancellationToken::new();
    bases
        .warm(&archives, 128, 16 * 1024 * 1024 * 1024, &policy, &stop)
        .await
        .unwrap();
    let ready = bases.lookup(&archives, 128).await.unwrap().unwrap();
    assert!(bases.lookup(&archives, 256).await.unwrap().is_none());
    let other = Bases {
        identity: (bases.identity.0 + 1, bases.identity.1),
        ..bases.clone()
    };
    assert!(other.lookup(&archives, 128).await.unwrap().is_none());
    let source = bases.state.join("native-bases").join(&ready.key);
    let contents = bases.contents(&ready.key);
    assert!(!source.join("lazy").exists());
    assert!(!source.join("contents").exists());
    assert_eq!(
        std::fs::metadata(contents.parent().unwrap())
            .unwrap()
            .mode()
            & 0o777,
        0o700
    );
    let private = bases.state.join("private-copy");
    private_dir(&private).await.unwrap();
    super::directory::copy_unbudgeted(&contents, &private, &CancellationToken::new(), false)
        .await
        .unwrap();
    let relative = Path::new("workspaces")
        .join(&archive.project_id)
        .join("file");
    assert_eq!(
        std::fs::read(private.join(&relative)).unwrap(),
        b"initial project"
    );
    assert_ne!(
        std::fs::metadata(contents.join(&relative)).unwrap().ino(),
        std::fs::metadata(private.join(&relative)).unwrap().ino()
    );
    bases.trim(0, u64::MAX, None).await.unwrap();
    assert!(!source.exists());
    assert!(!contents.exists());
    assert_eq!(
        std::fs::read(private.join(relative)).unwrap(),
        b"initial project"
    );
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
    // Logical text bytes fit, but 8192 separately allocated files need at
    // least 32 MiB. Admission must use that allocation before extracting.
    let occupied = tokio::fs::File::create(bases.data.join("other-retained-work"))
        .await
        .unwrap();
    occupied.set_len(limit - 16 * disk::MIB).await.unwrap();
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
    let source = bases.state.join("native-bases").join(&ready.key);
    let payload = bases
        .contents(&ready.key)
        .join("workspaces")
        .join(&archives[0].project_id)
        .join("file");
    let mut bytes = std::fs::read(&payload).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 0xff;
    std::fs::write(&payload, &bytes).unwrap();
    // Publication verified this tree for the controller's lifetime: later
    // lookups check its records and identity without hashing it again.
    assert!(bases.lookup(&archives, 128).await.unwrap().is_some());
    assert!(source.join("ready.json").exists());
    // A restarted controller hashes it before the first claim.
    forget_verification(&ready.key);
    assert!(bases.lookup(&archives, 128).await.unwrap().is_none());
    assert!(!source.join("ready.json").exists());
    assert_eq!(
        std::fs::metadata(payload).unwrap().len(),
        bytes.len() as u64
    );
}

#[test]
fn background_preparation_yields_to_foreground_intent_and_then_finishes() {
    crate::test_process::run_ignored_with_timeout(
        "native::bases_tests::background_preparation_yields_to_foreground_intent_and_then_finishes_child",
        std::time::Duration::from_secs(90),
    );
}

#[tokio::test]
#[ignore = "executed by the isolated background preparation wrapper"]
async fn background_preparation_yields_to_foreground_intent_and_then_finishes_child() {
    use std::{sync::Arc, time::Duration};

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
    // Wait for the actual worker lifecycle, not filesystem metadata: an
    // in-flight sync can make no visible changes. The ceiling allows for the
    // measured 27-second slow-sync build while still bounding a stuck worker.
    tokio::time::timeout(Duration::from_secs(60), warmer.wait_idle())
        .await
        .expect("native base warmer did not finish");
    assert!(
        bases
            .lookup(std::slice::from_ref(&archive), 128)
            .await
            .unwrap()
            .is_some(),
        "native base warmer finished without publishing a ready base"
    );
    stop.cancel();
    warmer.wait_idle().await;
}

#[tokio::test]
async fn active_background_build_releases_its_temporary_files_before_foreground_admission() {
    use std::{sync::Arc, time::Duration};
    let (_root, mut bases, archive, policy) = fixture().await;
    let entered = Arc::new(tokio::sync::Notify::new());
    bases.build_entered = Some(entered.clone());
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
    tokio::time::timeout(Duration::from_secs(5), entered.notified())
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
    assert_eq!(
        std::fs::read_dir(super::workspace::data_root(&bases.state).join(POOL_DIRECTORY))
            .unwrap()
            .count(),
        0
    );
    stop.cancel();
    drop(foreground);
    warmer.wait_idle().await;
}

#[tokio::test]
#[ignore = "requires the disposable project-quota pool; scripts/qualify-native-quota.mjs"]
async fn measure_prepared_native_bases() {
    use rand::{RngCore, SeedableRng};
    use std::{io::Read, os::fd::AsRawFd, time::Instant};

    fn cpu_ms() -> f64 {
        let mut time = std::mem::MaybeUninit::<libc::timespec>::uninit();
        assert_eq!(
            unsafe { libc::clock_gettime(libc::CLOCK_PROCESS_CPUTIME_ID, time.as_mut_ptr()) },
            0
        );
        let time = unsafe { time.assume_init() };
        let mut children = std::mem::MaybeUninit::<libc::rusage>::zeroed();
        assert_eq!(
            unsafe { libc::getrusage(libc::RUSAGE_CHILDREN, children.as_mut_ptr()) },
            0
        );
        let children = unsafe { children.assume_init() };
        time.tv_sec as f64 * 1000.0
            + time.tv_nsec as f64 / 1_000_000.0
            + (children.ru_utime.tv_sec + children.ru_stime.tv_sec) as f64 * 1000.0
            + (children.ru_utime.tv_usec + children.ru_stime.tv_usec) as f64 / 1000.0
    }

    fn file_io(root: &Path, expected: &[u8]) -> serde_json::Value {
        let mut reads = Vec::new();
        for cold in [true, false] {
            let mut file = std::fs::File::open(root.join("asset")).unwrap();
            if cold {
                assert_eq!(
                    unsafe {
                        libc::posix_fadvise(file.as_raw_fd(), 0, 0, libc::POSIX_FADV_DONTNEED)
                    },
                    0
                );
            }
            let mut buffer = vec![0; 4 * 1024 * 1024];
            let begin = Instant::now();
            let cpu = cpu_ms();
            file.read_exact(&mut buffer).unwrap();
            let sample = json!({ "wallMs": begin.elapsed().as_secs_f64() * 1000.0, "cpuMs": cpu_ms() - cpu });
            assert_eq!(buffer, expected[..buffer.len()]);
            reads.push(sample);
        }
        let begin = Instant::now();
        let cpu = cpu_ms();
        {
            let mut file = std::fs::File::create(root.join("measurement-delta")).unwrap();
            file.write_all(&[0x5a; 4096]).unwrap();
            file.sync_all().unwrap();
        }
        json!({ "read4MiBColdHintThenWarm": reads, "write4KiB": {
            "wallMs": begin.elapsed().as_secs_f64() * 1000.0, "cpuMs": cpu_ms() - cpu
        } })
    }

    assert_eq!(
        std::env::var("CAIRN_NATIVE_DISPOSABLE_HOST").as_deref(),
        Ok("1")
    );
    assert_eq!(unsafe { libc::geteuid() }, 0);
    let state =
        std::path::PathBuf::from(std::env::var_os("CAIRN_NATIVE_QUOTA_TEST_STATE").unwrap());
    super::quota::require(&state).unwrap();
    let (_root, mut bases, mut archive, policy) = fixture().await;
    bases.state = state.clone();
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
    let cache = bases.contents(&ready.key);
    let mut samples = Vec::new();
    for _ in 0..3 {
        let baseline_run = crate::config::id();
        let baseline_control = super::workspace::directory(&state, &baseline_run).unwrap();
        let baseline_workload = super::workspace::workload(&state, &baseline_run).unwrap();
        private_dir(&baseline_control).await.unwrap();
        private_dir(&baseline_workload).await.unwrap();
        super::quota::prepare(&state, &baseline_workload, 256 * disk::MIB)
            .await
            .unwrap();
        let extracted = baseline_workload.join("data");
        let begin = Instant::now();
        let cpu = cpu_ms();
        crate::project_archives::extract_cached(&bases.data, &archive, &extracted, &stop)
            .await
            .unwrap();
        super::workspace::own_tree(&extracted, bases.identity.0, bases.identity.1)
            .await
            .unwrap();
        super::block::sync(&extracted).await.unwrap();
        let zip_sample =
            json!({ "wallMs": begin.elapsed().as_secs_f64() * 1000.0, "cpuMs": cpu_ms() - cpu });
        let baseline_io = file_io(&extracted, &random);
        let run = crate::config::id();
        let attempt = crate::config::id();
        let plan = json!({
            "id": attempt, "runId": run, "runtimeId": "a".repeat(40),
            "resources": { "diskMiB": 256 },
            "nativeProjectArchives": archives, "nativeProjectBase": ready.key,
            "storage": { "owner": run, "policy": policy }
        });
        let begin = Instant::now();
        let cpu = cpu_ms();
        assert!(bases.initialize(&plan, &stop).await.unwrap());
        let mut mounted = super::directory::mount(&state, &run, &plan, &stop)
            .await
            .unwrap();
        super::directory::initialize(&state, &run, &plan, 256 * disk::MIB)
            .await
            .unwrap();
        let clone_sample =
            json!({ "wallMs": begin.elapsed().as_secs_f64() * 1000.0, "cpuMs": cpu_ms() - cpu });
        let workload = super::workspace::workload(&state, &run).unwrap();
        let private_bytes = super::quota::usage(&workload).unwrap();
        let prepared_io = file_io(
            &workload.join("data/workspaces").join(&archive.project_id),
            &random,
        );
        samples.push(json!({ "cachedZipExtraction": zip_sample, "preparedDirectoryAdmission": clone_sample,
            "privateAllocatedBytesBeforeDelta": private_bytes, "baselineIo": baseline_io, "preparedIo": prepared_io }));
        let control = super::workspace::directory(&state, &run).unwrap();
        tokio::fs::write(control.join("attempt"), attempt.as_bytes())
            .await
            .unwrap();
        mounted.close().await.unwrap();
        drop(mounted);
        super::directory::action(&state, &run, true).await.unwrap();
        super::quota::retire(&baseline_control).await.unwrap();
    }
    println!(
        "{}",
        json!({
            "logicalBytes": archive.logical_bytes, "zipEncodedBytes": archive.bytes,
            "baseAllocatedBytes": super::workspace::measure(&cache, true).unwrap(),
            "basePreparation": warm, "samples": samples,
            "hostLoad": std::fs::read_to_string("/proc/loadavg").unwrap(),
            "kernel": std::fs::read_to_string("/proc/sys/kernel/osrelease").unwrap().trim(),
            "cpuScope": "test process plus reaped child CPU, including rsync/checksum children",
            "conditions": "4096x4KiB text + seeded random42 8MiB; cached ZIP reference and prepared-directory admission on same dedicated ext4 project-quota pool; 256MiB per project; no ublk or WAN. Native admission includes verified private copy, durable format and initialization. Syncfs can include other pool writeback. File reads use advisory DONTNEED, not guaranteed hardware-cold storage; writes fsync a new 4KiB file. Development profile with debug=0/incremental=0 in CI."
        })
    );
    bases.trim(0, u64::MAX, None).await.unwrap();
}

/// Uses the real quota pool supplied by the disposable qualification launcher.
#[tokio::test]
#[ignore = "requires scripts/qualify-native-quota.mjs on its dedicated disposable filesystem"]
async fn kernel_prepared_base_admission_and_recovery() {
    assert_eq!(
        std::env::var("CAIRN_NATIVE_DISPOSABLE_HOST").as_deref(),
        Ok("1")
    );
    assert_eq!(unsafe { libc::geteuid() }, 0);
    let state =
        std::path::PathBuf::from(std::env::var_os("CAIRN_NATIVE_QUOTA_TEST_STATE").unwrap());
    super::quota::require(&state).unwrap();
    let enrollment = state.join("different-enrollment/data/node");
    private_dir(&enrollment).await.unwrap();
    assert!(super::enabled());
    let capabilities = crate::nodes::connector::capabilities(&enrollment).unwrap();
    let total = capabilities["diskTotalMiB"].as_u64().unwrap();
    assert!(
        (768..=1024).contains(&total),
        "native enrollment must sample its 1GiB data pool, not controller storage: {total}"
    );
    let (_root, mut bases, archive, policy) = fixture().await;
    bases.state = state.clone();
    let stop = CancellationToken::new();
    bases
        .warm(
            std::slice::from_ref(&archive),
            128,
            16 * 1024 * 1024 * 1024,
            &policy,
            &stop,
        )
        .await
        .unwrap();
    let ready = bases
        .lookup(std::slice::from_ref(&archive), 128)
        .await
        .unwrap()
        .unwrap();
    let source = state.join("native-bases").join(&ready.key);
    for phase in ["fresh", "copying", "renamed", "committed"] {
        let run = crate::config::id();
        let attempt = crate::config::id();
        let control = super::workspace::directory(&state, &run).unwrap();
        let workload = super::workspace::workload(&state, &run).unwrap();
        let directory = storage::environment::directory(&state, &run).unwrap();
        private_dir(&control).await.unwrap();
        private_dir(&directory).await.unwrap();
        let stage = bases.data.join("runs").join(&run);
        private_dir(&stage).await.unwrap();
        let plan = json!({
            "id": attempt, "runId": run, "runtimeId": "synthetic",
            "resources": { "diskMiB": 128 },
            "storage": { "owner": run, "policy": policy },
            "nativeProjectArchives": [archive], "nativeProjectBase": ready.key
        });
        let intent = directory.join(format!(
            "native-base-initializing-v1.{}.{}.pin",
            ready.key, attempt
        ));
        if phase != "fresh" {
            tokio::fs::hard_link(source.join("pin"), &intent)
                .await
                .unwrap();
            private_dir(&workload).await.unwrap();
            super::quota::prepare(&state, &workload, 128 * disk::MIB)
                .await
                .unwrap();
            if phase == "copying" {
                private_dir(&workload.join("base.partial")).await.unwrap();
                tokio::fs::write(workload.join("base.partial/interrupted"), b"partial copy")
                    .await
                    .unwrap();
            } else {
                private_dir(&workload.join("data")).await.unwrap();
                super::directory::copy_unbudgeted(
                    &bases.contents(&ready.key),
                    &workload.join("data"),
                    &stop,
                    false,
                )
                .await
                .unwrap();
            }
            if phase == "committed" {
                super::directory::publish_format(&directory, &control, &plan, 128 * disk::MIB)
                    .await
                    .unwrap();
                tokio::fs::write(
                    workload.join("data/workspaces/new-work"),
                    b"must survive recovery",
                )
                .await
                .unwrap();
            }
            // Startup attempt-pin cleanup must preserve this environment's
            // durable initialization pin and therefore its cache source.
            recover_pins(&bases.data).await.unwrap();
            assert!(intent.exists());
        }
        if phase == "copying" {
            let path = state.join("storage-policy.json");
            let previous = std::fs::read(&path).ok();
            let (total, _) =
                crate::storage::policy::physical_space(&super::workspace::data_root(&state))
                    .unwrap();
            let reserved = json!({ "reserveMiB": total / disk::MIB + 1, "reservePercent": 1 });
            super::atomic_write(&path, &serde_json::to_vec(&reserved).unwrap())
                .await
                .unwrap();
            assert!(bases.initialize(&plan, &stop).await.is_err());
            assert!(
                intent.exists(),
                "Capacity refusal must preserve the initialization intent and source pin"
            );
            assert!(!directory.join("directory.json").exists());
            if let Some(bytes) = previous {
                super::atomic_write(&path, &bytes).await.unwrap();
            } else {
                tokio::fs::remove_file(&path).await.unwrap();
            }
        }
        let expected_clone = phase != "committed";
        assert_eq!(
            bases.initialize(&plan, &stop).await.unwrap(),
            expected_clone
        );
        assert!(!std::fs::read_dir(&directory).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with("native-base-initializing-v1.")
        }));
        let mut mounted = super::directory::mount(&state, &run, &plan, &stop)
            .await
            .unwrap();
        super::directory::initialize(&state, &run, &plan, 128 * disk::MIB)
            .await
            .unwrap();
        assert_eq!(
            std::fs::read(
                workload
                    .join("data/workspaces")
                    .join(&archive.project_id)
                    .join("file")
            )
            .unwrap(),
            b"initial project"
        );
        if phase == "committed" {
            assert_eq!(
                std::fs::read(workload.join("data/workspaces/new-work")).unwrap(),
                b"must survive recovery"
            );
        }
        tokio::fs::write(control.join("attempt"), attempt.as_bytes())
            .await
            .unwrap();
        mounted.close().await.unwrap();
        drop(mounted);
        // Model leftover temporary resources after controller death. Recovery
        // must clean them even if no conversation ever resumes afterward.
        private_dir(&workload.join("tmp")).await.unwrap();
        private_dir(&control.join("docker-runtime")).await.unwrap();
        private_dir(&control.join("tmp")).await.unwrap();
        tokio::fs::write(
            control.join("tmp/legacy-interrupted-output"),
            b"old temporary",
        )
        .await
        .unwrap();
        tokio::fs::write(workload.join("tmp/interrupted-output"), b"temporary")
            .await
            .unwrap();
        tokio::fs::write(
            control.join("docker-runtime/interrupted-runtime"),
            b"temporary",
        )
        .await
        .unwrap();
        let peer_temporary = state.join("foreign-legacy-tmp");
        private_dir(&peer_temporary).await.unwrap();
        tokio::fs::write(peer_temporary.join("peer-work"), b"foreign temporary")
            .await
            .unwrap();
        let mut peer_consumer = tokio::process::Command::new("sleep")
            .arg("30")
            .current_dir(&peer_temporary)
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let mut consumer = tokio::process::Command::new("sleep")
            .arg("30")
            .current_dir(control.join("tmp"))
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        assert!(
            super::directory::recover_completed(&state, &run, &attempt)
                .await
                .is_err()
        );
        assert!(control.join("tmp/legacy-interrupted-output").is_file());
        consumer.kill().await.unwrap();
        consumer.wait().await.unwrap();
        super::directory::recover_completed(&state, &run, &attempt)
            .await
            .unwrap();
        super::directory::recover_completed(&state, &run, &attempt)
            .await
            .unwrap();
        assert!(!workload.join("tmp").exists());
        assert!(!control.join("docker-runtime").exists());
        assert!(!control.join("tmp").exists());
        assert_eq!(
            tokio::fs::read(peer_temporary.join("peer-work"))
                .await
                .unwrap(),
            b"foreign temporary"
        );
        assert!(peer_consumer.try_wait().unwrap().is_none());
        peer_consumer.kill().await.unwrap();
        peer_consumer.wait().await.unwrap();
        tokio::fs::remove_dir_all(&peer_temporary).await.unwrap();
        assert_eq!(
            std::fs::read(
                workload
                    .join("data/workspaces")
                    .join(&archive.project_id)
                    .join("file")
            )
            .unwrap(),
            b"initial project"
        );
        super::directory::action(&state, &run, true).await.unwrap();
        assert!(!workload.exists());
    }
    assert_eq!(std::fs::metadata(source.join("pin")).unwrap().nlink(), 1);
    bases.trim(0, u64::MAX, None).await.unwrap();
    println!(
        "Prepared native initialization: fresh, interrupted copy, durable rename and committed work all admitted and retired."
    );
}

#[tokio::test]
async fn committed_prepared_intent_cleanup_preserves_later_user_work_without_cache_access() {
    let (_root, bases, _archive, _policy) = fixture().await;
    let run = crate::config::id();
    let old = crate::config::id();
    let next = crate::config::id();
    let directory = storage::environment::directory(&bases.state, &run).unwrap();
    let workload = super::workspace::workload(&bases.state, &run).unwrap();
    private_dir(&directory).await.unwrap();
    for name in ["home", "workspaces", "output"] {
        private_dir(&workload.join("data").join(name))
            .await
            .unwrap();
    }
    let retained = workload.join("data/workspaces/saved");
    tokio::fs::write(&retained, b"user work after format publication")
        .await
        .unwrap();
    let plan = json!({ "id": next, "runId": run, "runtimeId": "current" });
    super::atomic_write(
        &directory.join("directory.json"),
        &serde_json::to_vec(&super::directory::format(&plan)).unwrap(),
    )
    .await
    .unwrap();
    let intent = directory.join(format!(
        "native-base-initializing-v1.{}.{}.pin",
        "a".repeat(64),
        old
    ));
    tokio::fs::write(&intent, b"").await.unwrap();
    let stage = bases.data.join("runs").join(&run);
    let blocked_pin = pin_path(&stage, &old).unwrap();
    private_dir(&blocked_pin).await.unwrap();
    assert!(
        bases
            .initialize(&plan, &CancellationToken::new())
            .await
            .is_err()
    );
    assert!(
        intent.exists(),
        "failed staging cleanup must preserve its retry intent"
    );
    assert_eq!(
        std::fs::read(&retained).unwrap(),
        b"user work after format publication"
    );
    tokio::fs::remove_dir(blocked_pin).await.unwrap();
    assert!(
        !bases
            .initialize(&plan, &CancellationToken::new())
            .await
            .unwrap()
    );
    assert!(!intent.exists());
    assert_eq!(
        std::fs::read(retained).unwrap(),
        b"user work after format publication"
    );
}

#[tokio::test]
async fn prepared_initialization_intent_does_not_authorize_unrecognized_old_payloads() {
    let (_root, bases, archive, policy) = fixture().await;
    let stop = CancellationToken::new();
    bases
        .warm(
            std::slice::from_ref(&archive),
            128,
            16 * 1024 * 1024 * 1024,
            &policy,
            &stop,
        )
        .await
        .unwrap();
    let ready = bases
        .lookup(std::slice::from_ref(&archive), 128)
        .await
        .unwrap()
        .unwrap();
    let run = crate::config::id();
    let attempt = crate::config::id();
    let directory = storage::environment::directory(&bases.state, &run).unwrap();
    private_dir(&directory.join("lazy")).await.unwrap();
    let payload = directory.join("lazy/retained-payload");
    tokio::fs::write(&payload, b"old work").await.unwrap();
    tokio::fs::hard_link(
        bases
            .state
            .join("native-bases")
            .join(&ready.key)
            .join("pin"),
        directory.join(format!(
            "native-base-initializing-v1.{}.{}.pin",
            ready.key, attempt
        )),
    )
    .await
    .unwrap();
    let plan = json!({ "id": attempt, "runId": run, "resources": { "diskMiB": 128 },
        "nativeProjectArchives": [archive], "nativeProjectBase": ready.key });
    assert!(bases.initialize(&plan, &stop).await.is_err());
    assert_eq!(std::fs::read(payload).unwrap(), b"old work");
    assert!(!directory.join("directory.json").exists());
    assert!(
        !super::workspace::workload(&bases.state, &run)
            .unwrap()
            .exists()
    );
}

#[tokio::test]
async fn trimming_uses_recorded_sizes_and_unpublishes_before_removing_contents() {
    let (_root, bases, archive, policy) = fixture().await;
    let archives = vec![archive];
    bases
        .warm(
            &archives,
            128,
            16 * 1024 * disk::MIB,
            &policy,
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    let ready = bases.lookup(&archives, 128).await.unwrap().unwrap();
    let control = bases.state.join("native-bases").join(&ready.key);
    let contents = bases.contents(&ready.key);
    assert_eq!(
        ready.allocated_bytes,
        super::workspace::measure(&contents, true).unwrap()
    );

    // The recorded size, not a walk of the tree, decides the cache budget.
    let mut record: serde_json::Value =
        serde_json::from_slice(&std::fs::read(control.join("ready.json")).unwrap()).unwrap();
    record["allocatedBytes"] = (64 * disk::MIB).into();
    super::atomic_write(
        &control.join("ready.json"),
        &serde_json::to_vec(&record).unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(bases.trim(65 * disk::MIB, 0, None).await.unwrap(), 0);
    assert!(contents.is_dir());
    assert_eq!(
        bases.trim(32 * disk::MIB, 0, None).await.unwrap(),
        64 * disk::MIB
    );
    assert!(!control.exists());
    assert!(!contents.exists());
}

#[tokio::test]
async fn startup_cleanup_removes_orphans_and_unpinned_legacy_entries() {
    let (_root, bases, archive, policy) = fixture().await;
    let archives = vec![archive];
    bases
        .warm(
            &archives,
            128,
            16 * 1024 * disk::MIB,
            &policy,
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    let ready = bases.lookup(&archives, 128).await.unwrap().unwrap();
    let controls = bases.state.join("native-bases");
    let pool = super::workspace::data_root(&bases.state).join(POOL_DIRECTORY);
    let key = |index: u8| format!("{index:02x}").repeat(32);

    // Interrupted builds and probes, and contents nothing publishes.
    private_dir(
        &pool
            .join(format!(".build-{}", crate::config::id()))
            .join("home"),
    )
    .await
    .unwrap();
    tokio::fs::write(
        pool.join(format!(".probe-{}.source", crate::config::id())),
        b"x",
    )
    .await
    .unwrap();
    private_dir(&pool.join(key(1)).join("workspaces"))
        .await
        .unwrap();
    // A current record whose contents were never renamed into place.
    private_dir(&controls.join(key(2))).await.unwrap();
    let mut record: serde_json::Value = serde_json::from_slice(
        &std::fs::read(controls.join(&ready.key).join("ready.json")).unwrap(),
    )
    .unwrap();
    record["key"] = key(2).into();
    tokio::fs::write(
        controls.join(key(2)).join("ready.json"),
        serde_json::to_vec(&record).unwrap(),
    )
    .await
    .unwrap();
    tokio::fs::write(controls.join(key(2)).join("pin"), b"")
        .await
        .unwrap();
    // Legacy entries: inline contents, or an older ready format.
    let mut legacy = record.clone();
    legacy["inputs"]["format"] = 2.into();
    for index in [3, 4, 5] {
        let control = controls.join(key(index));
        private_dir(&control.join("contents/workspaces"))
            .await
            .unwrap();
        tokio::fs::write(control.join("pin"), b"").await.unwrap();
        legacy["key"] = key(index).into();
        tokio::fs::write(
            control.join("ready.json"),
            serde_json::to_vec(&legacy).unwrap(),
        )
        .await
        .unwrap();
    }
    std::fs::remove_dir_all(controls.join(key(4)).join("contents")).unwrap();
    // An environment's durable intent still pins this legacy entry.
    let intent = bases.state.join("legacy-intent.pin");
    std::fs::hard_link(controls.join(key(5)).join("pin"), &intent).unwrap();

    recover_cache(&bases.state).await.unwrap();

    let mut remaining = std::fs::read_dir(&pool)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    remaining.sort();
    assert_eq!(remaining, vec![ready.key.clone()]);
    let mut records = std::fs::read_dir(&controls)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    records.sort();
    let mut expected = vec![ready.key.clone(), key(5)];
    expected.sort();
    assert_eq!(records, expected);
    assert!(controls.join(key(5)).join("contents").is_dir());
    assert!(bases.lookup(&archives, 128).await.unwrap().is_some());

    // Once released, the legacy entry is an ordinary trim candidate.
    std::fs::remove_file(intent).unwrap();
    bases
        .trim(u64::MAX, u64::MAX, Some(&ready.key))
        .await
        .unwrap();
    assert!(!controls.join(key(5)).exists());
    assert!(bases.lookup(&archives, 128).await.unwrap().is_some());
}
