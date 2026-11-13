use super::*;
use std::collections::BTreeSet;

fn block_files(directory: &Path) -> BTreeSet<String> {
    match std::fs::read_dir(directory.join("cache")) {
        Ok(entries) => entries
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect(),
        Err(error) if error.kind() == io::ErrorKind::NotFound => BTreeSet::new(),
        Err(error) => panic!("{error}"),
    }
}

fn base_hashes(disk: &LazyDisk) -> BTreeSet<String> {
    disk.base.lock().unwrap()["blocks"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|block| block["hash"].as_str().map(str::to_owned))
        .collect()
}

fn segments(directory: &Path) -> usize {
    std::fs::read_dir(directory)
        .unwrap()
        .filter(|entry| {
            entry
                .as_ref()
                .unwrap()
                .file_name()
                .to_string_lossy()
                .ends_with(".segment")
        })
        .count()
}

fn read(disk: &LazyDisk, offset: u64, length: usize) -> Vec<u8> {
    let mut bytes = vec![0; length];
    disk.read_at(offset, &mut bytes).unwrap();
    bytes
}

#[test]
fn consolidation_folds_sealed_generations_into_a_durable_local_base() {
    use std::os::unix::fs::MetadataExt;
    let root = tempfile::tempdir().unwrap();

    let manifest = local_base(root.path(), &[None, None, None]);
    let disk = LazyDisk::create(root.path(), &manifest).unwrap();
    for value in 1..=3 {
        disk.write_at(0, &vec![value; BLOCK as usize]).unwrap();
    }
    disk.write_at(BLOCK + 17, b"partial").unwrap();
    disk.sync().unwrap();
    assert!(disk.accounting().unwrap()["journalBytes"].as_u64().unwrap() > 3 * BLOCK);
    assert_eq!(disk.consolidate().unwrap(), None, "nothing is sealed yet");

    let generation = disk.seal().unwrap();
    assert!(disk.sealed_pending());
    assert_eq!(disk.consolidate().unwrap(), Some(generation));
    let accounting = disk.accounting().unwrap();
    assert_eq!(accounting["dirtyBytes"], 0);
    assert_eq!(accounting["journalBytes"], 0);
    assert_eq!(
        accounting["published"],
        serde_json::json!({ "generation": generation, "local": true })
    );
    assert!(!disk.sealed_pending());
    assert!(!disk.has_sealed(generation).unwrap());
    assert_eq!(segments(root.path()), 0);
    assert_eq!(base_hashes(&disk).len(), 2);
    assert_eq!(block_files(root.path()), base_hashes(&disk));

    // A mostly empty block keeps its zero pages as holes.
    let mut partial = vec![0; BLOCK as usize];
    partial[17..24].copy_from_slice(b"partial");
    let file = root.path().join("cache").join(block_digest(&partial));
    assert_eq!(std::fs::metadata(&file).unwrap().len(), BLOCK);
    assert!(std::fs::metadata(&file).unwrap().blocks() * 512 < 1024 * 1024);

    drop(disk);
    let disk = LazyDisk::open(root.path()).unwrap();
    assert_eq!(read(&disk, 0, 4096), vec![3; 4096]);
    assert_eq!(read(&disk, BLOCK + 16, 9), b"\0partial\0");
    assert_eq!(read(&disk, 2 * BLOCK, 4096), vec![0; 4096]);
}

#[test]
fn consolidation_keeps_writes_after_its_seal_in_the_journal() {
    let root = tempfile::tempdir().unwrap();
    let manifest = local_base(root.path(), &[None, None]);
    let disk = LazyDisk::create(root.path(), &manifest).unwrap();
    disk.write_at(0, b"sealed").unwrap();
    let generation = disk.seal_completed().unwrap();
    disk.write_at(2, b"NEWER").unwrap();
    disk.write_at(BLOCK, b"tail").unwrap();
    assert_eq!(disk.consolidate().unwrap(), Some(generation));
    assert_eq!(disk.accounting().unwrap()["dirtyBytes"], 9);
    assert_eq!(read(&disk, 0, 7), b"seNEWER");
    assert_eq!(read(&disk, BLOCK, 4), b"tail");
    // The completed-turn boundary remains as recorded.
    assert_eq!(disk.completed().unwrap().unwrap()["generation"], generation);
    drop(disk);

    let disk = LazyDisk::open(root.path()).unwrap();
    assert_eq!(read(&disk, 0, 7), b"seNEWER");
    let next = disk.seal().unwrap();
    assert_eq!(disk.consolidate().unwrap(), Some(next));
    assert_eq!(disk.accounting().unwrap()["dirtyBytes"], 0);
    assert_eq!(read(&disk, 0, 7), b"seNEWER");
    assert_eq!(read(&disk, BLOCK, 4), b"tail");
}

#[test]
fn consolidation_retires_only_blocks_no_base_references() {
    let root = tempfile::tempdir().unwrap();
    let first = root.path().join("environments/first/lazy");
    let second = root.path().join("environments/second/lazy");
    let original = vec![1; BLOCK as usize];
    let unchanged = vec![2; BLOCK as usize];
    let disk = LazyDisk::create(
        &first,
        &local_base(&first, &[Some(original.clone()), Some(unchanged.clone())]),
    )
    .unwrap();
    let neighbour =
        LazyDisk::create(&second, &local_base(&second, &[Some(original.clone())])).unwrap();
    let obsolete = format!("b3-{}", "a".repeat(64));
    std::fs::write(first.join("cache").join(&obsolete), b"orphan").unwrap();
    std::fs::write(first.join("cache").join(".block-interrupted"), b"temp").unwrap();

    disk.write_at(0, &vec![5; BLOCK as usize]).unwrap();
    let generation = disk.seal().unwrap();
    disk.write_at(BLOCK + 3, b"unsealed").unwrap();
    assert_eq!(disk.consolidate().unwrap(), Some(generation));

    let expected = BTreeSet::from([
        block_digest(&vec![5; BLOCK as usize]),
        block_digest(&unchanged),
    ]);
    assert_eq!(base_hashes(&disk), expected);
    assert_eq!(block_files(&first), expected);
    // Another conversation's base, even with identical content, is untouched.
    assert_eq!(
        block_files(&second),
        BTreeSet::from([block_digest(&original)])
    );
    assert_eq!(read(&neighbour, 0, 4096), vec![1; 4096]);
    assert_eq!(read(&disk, BLOCK, 11), b"\x02\x02\x02unsealed");
}

#[test]
fn missing_or_corrupt_base_block_is_an_integrity_error() {
    let root = tempfile::tempdir().unwrap();

    let directory = root.path().join("disk");
    let bytes = vec![8; BLOCK as usize];
    let manifest = local_base(&directory, &[Some(bytes.clone())]);
    let file = directory.join("cache").join(block_digest(&bytes));

    // A base never refers to a block the node does not hold.
    std::fs::remove_file(&file).unwrap();
    let error = LazyDisk::create(&directory, &manifest).err().unwrap();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    std::fs::write(&file, &bytes).unwrap();
    let disk = LazyDisk::create(&directory, &manifest).unwrap();
    disk.write_at(4, b"journal").unwrap();
    drop(disk);
    std::fs::remove_file(&file).unwrap();
    let error = LazyDisk::open(&directory).err().unwrap();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);

    // Same size, different content: reads fail rather than return other bytes.
    std::fs::write(&file, vec![9; BLOCK as usize]).unwrap();
    let disk = LazyDisk::open(&directory).unwrap();
    assert_eq!(read(&disk, 4, 7), b"journal");
    let mut neighbour = [0; 8];
    let error = disk.read_at(64, &mut neighbour).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert!(file.exists(), "a damaged base block must not be discarded");
    let generation = disk.seal().unwrap();
    assert!(disk.consolidate().is_err());
    assert!(disk.has_sealed(generation).unwrap());
}

#[test]
fn concurrent_cold_reads_verify_one_local_block_once() {
    let root = tempfile::tempdir().unwrap();
    let manifest = local_base(root.path(), &[Some(vec![7; BLOCK as usize])]);
    let disk = Arc::new(LazyDisk::create(root.path(), &manifest).unwrap());
    let gate = Arc::new(std::sync::Barrier::new(8));
    std::thread::scope(|scope| {
        for _ in 0..8 {
            let (disk, gate) = (disk.clone(), gate.clone());
            scope.spawn(move || {
                gate.wait();
                assert_eq!(read(&disk, 0, 4096), vec![7; 4096]);
            });
        }
    });
    assert_eq!(disk.performance()["diskHits"], 1);
}

#[test]
fn failed_reconstruction_never_commits_a_base_and_can_retry() {
    let root = tempfile::tempdir().unwrap();
    let base = vec![1; BLOCK as usize];
    let manifest = local_base(root.path(), &[Some(base.clone()), Some(base.clone())]);
    let file = root.path().join("cache").join(block_digest(&base));
    let disk = LazyDisk::create(root.path(), &manifest).unwrap();
    for index in 0..2u64 {
        disk.write_at(index * BLOCK + 17, &[index as u8 + 2])
            .unwrap();
    }
    let generation = disk.seal().unwrap();
    std::fs::write(&file, vec![0; BLOCK as usize]).unwrap();
    assert!(disk.consolidate().is_err());
    assert!(disk.sealed_pending());
    assert!(disk.has_sealed(generation).unwrap());
    assert_eq!(base_hashes(&disk), BTreeSet::from([block_digest(&base)]));

    std::fs::write(&file, &base).unwrap();
    assert_eq!(disk.consolidate().unwrap(), Some(generation));
    for index in 0..2u64 {
        let mut expected = base.clone();
        expected[17] = index as u8 + 2;
        assert_eq!(read(&disk, index * BLOCK, BLOCK as usize), expected);
    }
    assert_eq!(block_files(root.path()), base_hashes(&disk));
}

#[test]
fn a_new_owner_interrupts_consolidation_before_its_commit() {
    let root = tempfile::tempdir().unwrap();
    let manifest = local_base(root.path(), &[None, None]);
    let disk = LazyDisk::create(root.path(), &manifest).unwrap();
    disk.write_at(0, b"work").unwrap();
    disk.write_at(BLOCK, b"more").unwrap();
    let generation = disk.seal().unwrap();
    disk.yield_consolidation();
    let error = disk.consolidate().unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::Interrupted);
    assert!(disk.has_sealed(generation).unwrap());
    assert!(base_hashes(&disk).is_empty());
    drop(disk);

    let disk = LazyDisk::open(root.path()).unwrap();
    assert!(disk.sealed_pending());
    assert_eq!(disk.consolidate().unwrap(), Some(generation));
    assert_eq!(read(&disk, 0, 4), b"work");
    assert_eq!(read(&disk, BLOCK, 4), b"more");
}

const CRASH_PHASES: [&str; 10] = [
    "consolidation_block_written",
    "consolidation_block_synced",
    "consolidation_block_renamed",
    "consolidation_blocks_written",
    "consolidation_blocks_durable",
    "consolidation_transaction",
    "consolidation_committed",
    "reclamation",
    "consolidation_reclaimed",
    "consolidation_blocks_retired",
];

#[test]
fn consolidation_survives_process_crashes_at_every_step() {
    use std::io::BufRead;
    let old = vec![1; BLOCK as usize];
    let unchanged = vec![2; BLOCK as usize];
    let replaced = vec![7; BLOCK as usize];
    for phase in CRASH_PHASES {
        let root = tempfile::tempdir().unwrap();
        let disk =
            LazyDisk::create(root.path(), &local_base(root.path(), &[None, None, None])).unwrap();
        disk.write_at(0, &old).unwrap();
        disk.write_at(BLOCK, &unchanged).unwrap();
        let first = disk.seal().unwrap();
        assert_eq!(disk.consolidate().unwrap(), Some(first));
        let previous = base_hashes(&disk);
        drop(disk);

        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["crash_consolidation_writer", "--nocapture"])
            .env("CAIRN_JOURNAL_CRASH_DIR", root.path())
            .env("CAIRN_JOURNAL_CRASH_PHASE", phase)
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let reached = std::io::BufReader::new(child.stdout.take().unwrap())
            .lines()
            .any(|line| line.unwrap().contains("JOURNAL_CRASH_READY"));
        child.kill().unwrap();
        child.wait().unwrap();
        assert!(reached, "missing injection: {phase}");

        let disk = LazyDisk::open(root.path()).unwrap();
        let committed = !matches!(
            phase,
            "consolidation_block_written"
                | "consolidation_block_synced"
                | "consolidation_block_renamed"
                | "consolidation_blocks_written"
                | "consolidation_blocks_durable"
                | "consolidation_transaction"
        );
        assert_eq!(base_hashes(&disk) == previous, !committed, "{phase}");
        assert_eq!(disk.sealed_pending(), !committed, "{phase}");
        let check = |disk: &LazyDisk| {
            assert_eq!(read(disk, 0, BLOCK as usize), replaced, "{phase}");
            assert_eq!(
                read(disk, BLOCK, 14),
                b"\x02\x02\x02\x02\x02\x02\x02\x02after!",
                "{phase}"
            );
            assert_eq!(read(disk, 2 * BLOCK + 4, 7), b"\0third\0", "{phase}");
        };
        check(&disk);

        // The next consolidation completes the interrupted one and removes
        // every unreferenced segment and block file.
        let next = disk.seal().unwrap();
        assert_eq!(disk.consolidate().unwrap(), Some(next), "{phase}");
        assert_eq!(disk.accounting().unwrap()["dirtyBytes"], 0, "{phase}");
        assert_eq!(segments(root.path()), 0, "{phase}");
        assert_eq!(block_files(root.path()), base_hashes(&disk), "{phase}");
        assert!(
            base_hashes(&disk).contains(&block_digest(&replaced)),
            "{phase}"
        );
        check(&disk);
        drop(disk);
        check(&LazyDisk::open(root.path()).unwrap());
    }
}

#[test]
fn crash_consolidation_writer() {
    let Some(root) = std::env::var_os("CAIRN_JOURNAL_CRASH_DIR") else {
        return;
    };
    if !std::env::var("CAIRN_JOURNAL_CRASH_PHASE")
        .is_ok_and(|phase| CRASH_PHASES.contains(&phase.as_str()))
    {
        return;
    }
    let disk = LazyDisk::open(Path::new(&root)).unwrap();
    // Several full segments make the unlink injection meaningful.
    for _ in 0..17 {
        disk.write_at(0, &vec![7; BLOCK as usize]).unwrap();
    }
    disk.write_at(2 * BLOCK + 5, b"third").unwrap();
    disk.seal().unwrap();
    disk.write_at(BLOCK + 8, b"after!").unwrap();
    disk.consolidate().unwrap();
}

#[test]
fn local_bases_larger_than_former_cache_budget_are_never_evicted() {
    let root = tempfile::tempdir().unwrap();
    let manifest = local_base(root.path(), &vec![None; 12]);
    let disk = LazyDisk::create(root.path(), &manifest).unwrap();
    for index in 0..12 {
        disk.write_at(index * BLOCK, &vec![index as u8 + 1; BLOCK as usize])
            .unwrap();
        disk.seal().unwrap();
        disk.consolidate().unwrap();
    }
    assert_eq!(block_files(root.path()).len(), 12);
    drop(disk);
    let disk = LazyDisk::open(root.path()).unwrap();
    for index in 0..12 {
        assert_eq!(
            read(&disk, index * BLOCK, 4096),
            vec![index as u8 + 1; 4096]
        );
    }
    assert_eq!(block_files(root.path()).len(), 12);
}

#[test]
fn consolidation_counts_journal_and_base_together_against_the_node_budget() {
    let root = tempfile::tempdir().unwrap();
    let directory = root.path().join("disks/owned/lazy");
    let manifest = local_base(&directory, &[None]);
    let disk = LazyDisk::create(&directory, &manifest).unwrap();
    disk.write_at(0, &vec![7; BLOCK as usize]).unwrap();
    disk.seal().unwrap();
    let policy = super::super::super::policy::Policy {
        reserve_mi_b: 64,
        reserve_percent: 1,
        ..Default::default()
    };
    std::fs::write(
        root.path().join("storage-policy.json"),
        serde_json::to_vec(&policy).unwrap(),
    )
    .unwrap();
    let budget = crate::microvm::budget::Budget {
        slots: 1,
        limits: crate::nodes::Resources {
            cpu: 1,
            memory_mi_b: 2048,
            disk_mi_b: 4,
        },
    };
    std::fs::write(
        root.path().join(crate::microvm::budget::FILE),
        serde_json::to_vec(&budget).unwrap(),
    )
    .unwrap();
    assert_eq!(
        disk.consolidate().unwrap_err().kind(),
        io::ErrorKind::StorageFull
    );
    assert!(disk.sealed_pending());
    assert_eq!(read(&disk, 0, 4096), vec![7; 4096]);
    assert!(base_hashes(&disk).is_empty());
    assert!(disk.accounting().unwrap()["journalBytes"].as_u64().unwrap() > 0);
}

#[test]
fn consolidation_preserves_the_full_configured_node_reserve() {
    let root = tempfile::tempdir().unwrap();
    let directory = root.path().join("disks/owned/lazy");
    let manifest = local_base(&directory, &[None]);
    let disk = LazyDisk::create(&directory, &manifest).unwrap();
    disk.write_at(0, b"durable").unwrap();
    disk.seal().unwrap();
    let (_, free) = super::super::super::policy::space(&directory).unwrap();
    let policy = super::super::super::policy::Policy {
        reserve_mi_b: free.div_ceil(1024 * 1024) * 3 / 2,
        reserve_percent: 1,
        ..Default::default()
    };
    std::fs::write(
        root.path().join("storage-policy.json"),
        serde_json::to_vec(&policy).unwrap(),
    )
    .unwrap();
    assert_eq!(
        disk.consolidate().unwrap_err().kind(),
        io::ErrorKind::StorageFull
    );
    assert!(disk.sealed_pending());
    assert_eq!(read(&disk, 0, 7), b"durable");
    assert!(base_hashes(&disk).is_empty());
}

#[test]
fn consolidation_rechecks_a_reserve_increase_before_admitting_the_next_block() {
    let root = tempfile::tempdir().unwrap();
    let directory = root.path().join("disks/owned/lazy");
    let manifest = local_base(&directory, &[None, None]);
    let disk = LazyDisk::create(&directory, &manifest).unwrap();
    disk.write_at(0, b"first").unwrap();
    disk.write_at(BLOCK, b"second").unwrap();
    let generation = disk.seal().unwrap();
    let low = super::super::super::policy::Policy {
        reserve_mi_b: 64,
        reserve_percent: 1,
        ..Default::default()
    };
    let policy_path = root.path().join("storage-policy.json");
    std::fs::write(&policy_path, serde_json::to_vec(&low).unwrap()).unwrap();
    let (_, free) = super::super::super::policy::space(&directory).unwrap();
    let raised_path = policy_path.clone();
    disk.on_consolidation_block_admitted(Box::new(move || {
        let raised = super::super::super::policy::Policy {
            reserve_mi_b: free.div_ceil(1024 * 1024) * 2,
            reserve_percent: 1,
            ..Default::default()
        };
        // An older controller file must still apply its current local reserve.
        let mut legacy_policy = serde_json::to_value(&raised).unwrap();
        legacy_policy["cacheMiB"] = serde_json::json!(20480);
        legacy_policy["backupSeconds"] = serde_json::json!(30);
        legacy_policy["maxDirtySeconds"] = serde_json::json!(90);
        std::fs::write(raised_path, serde_json::to_vec(&legacy_policy).unwrap()).unwrap();
    }));
    assert_eq!(
        disk.consolidate().unwrap_err().kind(),
        io::ErrorKind::StorageFull
    );
    assert_eq!(block_files(&directory).len(), 1);
    assert!(disk.sealed_pending());
    assert!(base_hashes(&disk).is_empty());
    assert_eq!(read(&disk, 0, 5), b"first");
    assert_eq!(read(&disk, BLOCK, 6), b"second");

    std::fs::write(policy_path, serde_json::to_vec(&low).unwrap()).unwrap();
    assert_eq!(disk.consolidate().unwrap(), Some(generation));
    assert_eq!(disk.accounting().unwrap()["dirtyBytes"], 0);
}

#[test]
fn failed_consolidations_keep_only_one_bounded_internal_copy_on_retry() {
    let root = tempfile::tempdir().unwrap();
    let environment = root.path().join("disks/owned");
    let directory = environment.join("lazy");
    let manifest = local_base(&directory, &[None, None]);
    let disk = LazyDisk::create(&directory, &manifest).unwrap();
    let low = super::super::super::policy::Policy {
        reserve_mi_b: 64,
        reserve_percent: 1,
        ..Default::default()
    };
    let policy_path = root.path().join("storage-policy.json");
    let (total, _) = super::super::super::policy::space(&directory).unwrap();
    for value in 1..=3 {
        std::fs::write(&policy_path, serde_json::to_vec(&low).unwrap()).unwrap();
        disk.write_at(0, &vec![value; 4096]).unwrap();
        disk.write_at(BLOCK, &vec![value + 10; 4096]).unwrap();
        disk.seal().unwrap();
        let path = policy_path.clone();
        disk.on_consolidation_block_admitted(Box::new(move || {
            let raised = super::super::super::policy::Policy {
                reserve_mi_b: total.div_ceil(1024 * 1024) * 2,
                reserve_percent: 1,
                ..Default::default()
            };
            std::fs::write(path, serde_json::to_vec(&raised).unwrap()).unwrap();
        }));
        assert_eq!(
            disk.consolidate().unwrap_err().kind(),
            io::ErrorKind::StorageFull
        );
        assert_eq!(
            block_files(&directory).len(),
            1,
            "a retry left an older candidate copy"
        );
        let physical = crate::microvm::budget::allocated(&environment).unwrap();
        let (retained, _) = disk.retained_backing(&environment).unwrap();
        assert!(physical > retained);
        assert!(physical - retained <= disk.size());
        assert!(base_hashes(&disk).is_empty());
    }
    std::fs::write(policy_path, serde_json::to_vec(&low).unwrap()).unwrap();
    disk.consolidate().unwrap().unwrap();
    assert_eq!(disk.accounting().unwrap()["dirtyBytes"], 0);
    assert_eq!(
        disk.retained_backing(&environment).unwrap().0,
        crate::microvm::budget::allocated(&environment).unwrap()
    );
    assert_eq!(read(&disk, 0, 4096), vec![3; 4096]);
    assert_eq!(read(&disk, BLOCK, 4096), vec![13; 4096]);
}
