use super::*;

#[tokio::test]
async fn controller_refuses_incomplete_or_incorrectly_mounted_migrations() {
    let root = tempfile::tempdir().unwrap();
    let state = root.path().join("state");
    let migration = root.path().join("native-pool-migration");
    std::fs::create_dir(&state).unwrap();
    require_settled(&state).await.unwrap();
    std::fs::create_dir(&migration).unwrap();
    let record = migration.join("state.json");
    std::fs::write(&record, br#"{"phase":"switched"}"#).unwrap();
    assert!(require_settled(&state).await.unwrap_err().is_conflict());

    std::fs::create_dir(workspace::data_root(&state)).unwrap();
    let expected = json!({"phase": "complete", "uuid": crate::config::id()});
    std::fs::write(&record, serde_json::to_vec(&expected).unwrap()).unwrap();
    let error = require_settled(&state).await.unwrap_err();
    assert!(error.is_conflict() && error.message.contains("activate-pool"));
    let receipt: Value = serde_json::from_slice(&std::fs::read(&record).unwrap()).unwrap();
    assert_eq!(receipt, expected);

    let mut authorized = expected;
    authorized["activationRequested"] = true.into();
    std::fs::write(&record, serde_json::to_vec(&authorized).unwrap()).unwrap();
    assert!(require_settled(&state).await.unwrap_err().is_conflict());
}

#[test]
fn retirement_receipts_outlive_an_unlinked_workload() {
    let state = tempfile::tempdir().unwrap();
    let run = crate::config::id();
    let root = workspace::directory(state.path(), &run).unwrap();
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join(RETIREMENT), b"preserved retirement receipt").unwrap();
    assert_eq!(orphaned_controls(state.path()).unwrap().len(), 1);
    std::fs::remove_file(root.join(RETIREMENT)).unwrap();
    assert!(orphaned_controls(state.path()).unwrap().is_empty());
}

#[test]
fn base_initialization_pin_must_settle_even_before_allocating_a_workload() {
    let state = tempfile::tempdir().unwrap();
    let run = crate::config::id();
    let attempt = crate::config::id();
    let directory = crate::storage::environment::directory(state.path(), &run).unwrap();
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(
        directory.join(format!(
            "native-base-initializing-v1.{}.{attempt}.pin",
            "a".repeat(64)
        )),
        b"",
    )
    .unwrap();
    assert_eq!(unfinished_bases(state.path()).unwrap().len(), 1);
    std::fs::write(directory.join("directory.json"), b"{}").unwrap();
    assert!(unfinished_bases(state.path()).unwrap().is_empty());
}

use std::{collections::BTreeMap, io::Write};

fn ownership(control: &Path, workload: &Path, project: u32) -> Ownership {
    let control_file = open(control).unwrap();
    let workload_file = open(workload).unwrap();
    Ownership {
        project,
        filesystem: pool_identity(&workload_file).unwrap(),
        control_device: control_file.metadata().unwrap().dev(),
        control_inode: control_file.metadata().unwrap().ino(),
        control_filesystem: super::super::filesystem_identity(&control_file).unwrap(),
        workload_device: workload_file.metadata().unwrap().dev(),
        workload_inode: workload_file.metadata().unwrap().ino(),
        migrated_limits: None,
    }
}

/// Simulates the mount switch with directory renames on one filesystem: the
/// staged record must describe the tree at the pool path before it replaces
/// the original, and rollback restores the original from the other side.
#[tokio::test]
async fn records_switch_only_onto_the_verified_copy_and_roll_back() {
    let root = tempfile::tempdir().unwrap();
    let state = root.path().join("state");
    let work = root.path().join("work");
    let run = crate::config::id();
    let control = state.join("native").join(&run);
    let pool = workspace::data_root(&state);
    let copy_pool = root.path().join("copy");
    for directory in [&control, &pool.join(&run), &copy_pool.join(&run)] {
        std::fs::create_dir_all(directory).unwrap();
    }
    let original = serde_json::to_vec(&ownership(&control, &pool.join(&run), 7)).unwrap();
    std::fs::write(control.join(OWNER), &original).unwrap();
    let migrated = serde_json::to_vec(&ownership(&control, &copy_pool.join(&run), 7)).unwrap();
    stage(
        &record_directory(&work, &run, "original").join(OWNER),
        &original,
    )
    .await
    .unwrap();
    stage(
        &record_directory(&work, &run, "migrated").join(OWNER),
        &migrated,
    )
    .await
    .unwrap();

    // The copy is not mounted at the pool path yet: nothing is replaced.
    assert!(switch(&state, &work).await.is_err());
    assert_eq!(std::fs::read(control.join(OWNER)).unwrap(), original);

    let original_pool = root.path().join("original");
    std::fs::rename(&pool, &original_pool).unwrap();
    std::fs::rename(&copy_pool, &pool).unwrap();
    let expected =
        ExpectedRecords::from([(run.clone(), vec![OWNER.to_owned(), RETIREMENT.to_owned()])]);
    seal_records(&work, &expected).await.unwrap();
    assert!(
        switch(&state, &work).await.is_err(),
        "missing sealed receipts must refuse cutover"
    );
    assert_eq!(std::fs::read(control.join(OWNER)).unwrap(), original);
    let expected = ExpectedRecords::from([(run.clone(), vec![OWNER.to_owned()])]);
    seal_records(&work, &expected).await.unwrap();
    assert_eq!(switch(&state, &work).await.unwrap()["switchedRecords"], 1);
    assert_eq!(switch(&state, &work).await.unwrap()["switchedRecords"], 0);
    assert_eq!(std::fs::read(control.join(OWNER)).unwrap(), migrated);

    // Rollback refuses while the copy is still mounted, then restores.
    assert!(rollback(&state, &work).await.is_err());
    assert_eq!(std::fs::read(control.join(OWNER)).unwrap(), migrated);
    std::fs::rename(&pool, &copy_pool).unwrap();
    std::fs::rename(&original_pool, &pool).unwrap();
    assert_eq!(rollback(&state, &work).await.unwrap()["restoredRecords"], 1);
    assert_eq!(std::fs::read(control.join(OWNER)).unwrap(), original);

    // A record the controller replaced since the copy is never overwritten.
    let replacement = serde_json::to_vec(&ownership(&control, &pool.join(&run), 8)).unwrap();
    std::fs::write(control.join(OWNER), &replacement).unwrap();
    std::fs::rename(&pool, &original_pool).unwrap();
    std::fs::rename(&copy_pool, &pool).unwrap();
    assert!(switch(&state, &work).await.is_err());
    assert_eq!(std::fs::read(control.join(OWNER)).unwrap(), replacement);
}

#[test]
fn command_paths_must_be_absolute() {
    let args = |path: &str| vec!["plan".to_owned(), path.to_owned()];
    assert!(argument(&args("state"), 1).is_err());
    assert!(argument(&args("./state"), 1).is_err());
    assert!(argument(&args("/state"), 1).is_ok());
    assert!(argument(&args("/state"), 2).is_err());
}

fn fixture_state() -> PathBuf {
    assert_eq!(
        std::env::var("CAIRN_NATIVE_DISPOSABLE_HOST").as_deref(),
        Ok("1")
    );
    assert_eq!(unsafe { libc::geteuid() }, 0);
    PathBuf::from(std::env::var_os("CAIRN_NATIVE_QUOTA_TEST_STATE").expect("fixture state"))
}

fn expected_path() -> PathBuf {
    PathBuf::from(std::env::var_os("CAIRN_NATIVE_MIGRATION_EXPECTED").expect("fixture record"))
}

fn tree(workload: &Path) -> BTreeMap<String, String> {
    let mut files = BTreeMap::new();
    let mut pending = vec![workload.to_owned()];
    while let Some(path) = pending.pop() {
        let metadata = std::fs::symlink_metadata(&path).unwrap();
        let relative = path.strip_prefix(workload).unwrap().display().to_string();
        if relative == "data/.cairn-identity.pending.json" {
            // Rebound to the copy's inodes by the migration; checked separately.
            continue;
        }
        let mut description = format!(
            "{:o} {}:{} {}",
            metadata.mode(),
            metadata.uid(),
            metadata.gid(),
            metadata.nlink()
        );
        if metadata.is_dir() {
            for entry in std::fs::read_dir(&path).unwrap() {
                pending.push(entry.unwrap().path());
            }
        } else if metadata.is_file() {
            description += &format!(
                " {} {} {}",
                metadata.len(),
                blake3::hash(&std::fs::read(&path).unwrap()).to_hex(),
                metadata.blocks() * 512 < metadata.len()
            );
        } else if metadata.file_type().is_symlink() {
            description += &format!(" -> {}", std::fs::read_link(&path).unwrap().display());
        }
        let name = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
        let mut value = [0u8; 64];
        let size = unsafe {
            libc::lgetxattr(
                name.as_ptr(),
                c"user.cairn-fixture".as_ptr(),
                value.as_mut_ptr().cast(),
                value.len(),
            )
        };
        if size >= 0 {
            description += &format!(
                " xattr={}",
                String::from_utf8_lossy(&value[..size as usize])
            );
        }
        files.insert(relative, description);
    }
    files
}

async fn seed(state: &Path) {
    super::super::require(state).unwrap();
    let pool = workspace::data_root(state);
    let mut environments = Vec::new();
    for (index, mib) in [8u64, 16].into_iter().enumerate() {
        let run = crate::config::id();
        let control = state.join("native").join(&run);
        crate::skills::private_dir(&control).await.unwrap();
        let workload = workspace::workload_for_control(&control).unwrap();
        crate::skills::private_dir(&workload).await.unwrap();
        super::super::prepare(state, &workload, mib * 1024 * 1024)
            .await
            .unwrap();
        let project = super::super::attributes(&open(&workload).unwrap())
            .unwrap()
            .project;
        let files = workload.join("data/workspaces/project");
        std::fs::create_dir_all(&files).unwrap();
        std::fs::write(files.join("source"), format!("environment {index}")).unwrap();
        std::fs::hard_link(files.join("source"), files.join("linked")).unwrap();
        std::os::unix::fs::symlink("source", files.join("alias")).unwrap();
        let mut sparse = std::fs::File::create(files.join("sparse")).unwrap();
        sparse.set_len(32 * 1024 * 1024).unwrap();
        std::io::Seek::seek(&mut sparse, std::io::SeekFrom::Start(1024 * 1024)).unwrap();
        sparse.write_all(b"island").unwrap();
        sparse.sync_all().unwrap();
        let name =
            std::ffi::CString::new(files.join("source").as_os_str().as_encoded_bytes()).unwrap();
        assert_eq!(
            unsafe {
                libc::setxattr(
                    name.as_ptr(),
                    c"user.cairn-fixture".as_ptr(),
                    b"kept".as_ptr().cast(),
                    4,
                    0,
                )
            },
            0
        );
        let fifo =
            std::ffi::CString::new(files.join("fifo").as_os_str().as_encoded_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
        std::os::unix::fs::chown(&files, Some(101_000), Some(101_000)).unwrap();
        // An interrupted ownership batch binds the source inode.
        let inode = std::fs::metadata(files.join("source")).unwrap().ino();
        std::fs::write(
            workload.join("data/.cairn-identity.pending.json"),
            serde_json::to_vec(&json!([{
                "relative": b"workspaces/project/source".to_vec(),
                "inode": inode,
                "uid": 101_000,
                "gid": 101_000,
                "mode": 0o100644,
                "symlink": false,
                "attributes": [],
                "needs_apply": false,
            }]))
            .unwrap(),
        )
        .unwrap();
        let pool_file = open(&pool).unwrap();
        let mut quota = current(&pool_file, project).unwrap();
        if index == 0 {
            // An atomic rebind needs one extra inode before replacing the
            // pending journal. The migrated hard cap must still be preserved.
            quota.inode_hard = quota.inodes;
            quota.inode_soft = quota.inodes;
            set_limits(&pool_file, project, &quota).unwrap();
        }
        environments.push(json!({
            "run": run,
            "project": project,
            "blockHard": quota.block_hard,
            "inodeHard": quota.inode_hard,
            "inodes": quota.inodes,
            "tree": tree(&workload),
        }));
    }
    // Retirement can stop after clearing the project but before rmdir.
    let retiring = crate::config::id();
    let control = state.join("native").join(&retiring);
    let workload = workspace::workload_for_control(&control).unwrap();
    crate::skills::private_dir(&control).await.unwrap();
    crate::skills::private_dir(&workload).await.unwrap();
    super::super::prepare(state, &workload, 8 * 1024 * 1024)
        .await
        .unwrap();
    std::fs::copy(control.join(OWNER), control.join(RETIREMENT)).unwrap();
    set_project(&open(&workload).unwrap(), 0).unwrap();
    let inventory = plan(state).unwrap();
    let blockers = inventory["blockers"].as_array().unwrap();
    assert_eq!(blockers.len(), 1);
    let blocker = blockers[0].as_str().unwrap();
    assert!(blocker.contains(&retiring) && blocker.contains("interrupted quota retirement"));
    super::super::retire(&control).await.unwrap();
    assert!(
        plan(state).unwrap()["blockers"]
            .as_array()
            .unwrap()
            .is_empty()
    );

    // A preparation interrupted before its project allocation.
    let unassigned = crate::config::id();
    crate::skills::private_dir(&pool.join(&unassigned))
        .await
        .unwrap();
    std::fs::create_dir_all(pool.join(".cairn-bases/cache")).unwrap();
    std::fs::write(pool.join(".cairn-bases/cache/rebuildable"), b"not migrated").unwrap();
    std::fs::write(
        expected_path(),
        serde_json::to_vec(&json!({
            "environments": environments,
            "unassigned": unassigned,
            "counter": std::fs::read_to_string(state.join("native-project-next")).unwrap(),
        }))
        .unwrap(),
    )
    .unwrap();
}

/// Read-only: the migrated pool still holds every environment, its project,
/// limits and records, as the controller itself validates them.
fn verify(state: &Path, filesystem: &str) {
    super::super::require(state).unwrap();
    let description = super::super::describe(state).unwrap();
    assert_eq!(description["filesystem"], filesystem);
    assert_eq!(description["reflink"], filesystem == "xfs");
    let expected: Value = serde_json::from_slice(&std::fs::read(expected_path()).unwrap()).unwrap();
    let pool = workspace::data_root(state);
    let pool_file = open(&pool).unwrap();
    for environment in expected["environments"].as_array().unwrap() {
        let run = environment["run"].as_str().unwrap();
        let workload = pool.join(run);
        let project = environment["project"].as_u64().unwrap() as u32;
        assert_eq!(
            super::super::attributes(&open(&workload).unwrap())
                .unwrap()
                .project,
            project
        );
        assert_eq!(
            super::super::owned_project(&workload, &open(&workload).unwrap()).unwrap(),
            project
        );
        super::super::usage(&workload).unwrap();
        let quota = current(&pool_file, project).unwrap();
        assert_eq!(quota.block_hard, environment["blockHard"].as_u64().unwrap());
        assert_eq!(quota.inode_hard, environment["inodeHard"].as_u64().unwrap());
        assert_eq!(quota.inodes, environment["inodes"].as_u64().unwrap());
        assert_eq!(
            serde_json::to_value(tree(&workload)).unwrap(),
            environment["tree"],
            "{run} changed"
        );
        let journal: Value = serde_json::from_slice(
            &std::fs::read(workload.join("data/.cairn-identity.pending.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(
            journal[0]["inode"],
            std::fs::metadata(workload.join("data/workspaces/project/source"))
                .unwrap()
                .ino()
        );
    }
    let unassigned = pool.join(expected["unassigned"].as_str().unwrap());
    assert_eq!(
        super::super::attributes(&open(&unassigned).unwrap())
            .unwrap()
            .project,
        0
    );
    assert_eq!(
        std::fs::read_to_string(state.join("native-project-next")).unwrap(),
        expected["counter"].as_str().unwrap()
    );
}

/// The migrated pool is fully usable: grants grow, limits are enforced,
/// environments retire and new IDs never reuse migrated ones.
async fn exercise(state: &Path) {
    let expected: Value = serde_json::from_slice(&std::fs::read(expected_path()).unwrap()).unwrap();
    assert!(
        !workspace::data_root(state).join(".cairn-bases").exists(),
        "the rebuildable base cache was copied"
    );
    let environments = expected["environments"].as_array().unwrap();
    let first_run = environments[0]["run"].as_str().unwrap();
    let first = workspace::data_root(state).join(first_run);
    let saved = crate::storage::environment::directory(state, first_run).unwrap();
    crate::skills::private_dir(&saved).await.unwrap();
    let plan = json!({
        "resources": { "diskMiB": 8 },
        "runtimeId": "synthetic",
        "storage": { "policy": crate::storage::policy::Policy::default() },
    });
    atomic_write(
        &saved.join("directory.json"),
        &serde_json::to_vec(&crate::native::directory::format(&plan)).unwrap(),
    )
    .await
    .unwrap();
    let stop = tokio_util::sync::CancellationToken::new();
    let mounted = crate::native::directory::mount(state, first_run, &plan, &stop)
        .await
        .unwrap();
    let handle = crate::native::directory::Handle::new(mounted);
    handle.volume.limit_backing(8 * 1024 * 1024).unwrap();
    let first_project = environments[0]["project"].as_u64().unwrap() as u32;
    let limits = current(&open(&first).unwrap(), first_project).unwrap();
    assert_eq!(
        limits.inode_hard,
        environments[0]["inodeHard"].as_u64().unwrap()
    );
    handle
        .mount
        .lock()
        .await
        .grow(12 * 1024 * 1024)
        .await
        .unwrap();
    handle.mount.lock().await.close().await.unwrap();
    drop(handle);
    let mut file = std::fs::File::create(first.join("growth")).unwrap();
    let failure = loop {
        if let Err(error) = file.write_all(&[42; 65536]) {
            break error;
        }
        assert!(file.metadata().unwrap().len() <= 12 * 1024 * 1024);
    };
    assert!(matches!(
        failure.raw_os_error(),
        Some(libc::EDQUOT | libc::ENOSPC)
    ));
    drop(file);
    let second = environments[1]["run"].as_str().unwrap();
    let control = state.join("native").join(second);
    super::super::retire(&control).await.unwrap();
    assert!(!workspace::data_root(state).join(second).exists());
    let fresh = state.join("native").join(crate::config::id());
    crate::skills::private_dir(&fresh).await.unwrap();
    let workload = workspace::workload_for_control(&fresh).unwrap();
    crate::skills::private_dir(&workload).await.unwrap();
    super::super::prepare(state, &workload, 4 * 1024 * 1024)
        .await
        .unwrap();
    let project = super::super::attributes(&open(&workload).unwrap())
        .unwrap()
        .project;
    assert!(
        environments
            .iter()
            .all(|environment| u64::from(project) > environment["project"].as_u64().unwrap())
    );
    super::super::retire(&fresh).await.unwrap();
}

#[tokio::test]
#[ignore = "requires the disposable pool migration fixture; scripts/ci/qualify-native-quota.mjs"]
async fn kernel_pool_migration_qualification() {
    let state = fixture_state();
    match std::env::var("CAIRN_NATIVE_MIGRATION_PHASE")
        .unwrap()
        .as_str()
    {
        "seed" => seed(&state).await,
        "migrated" => {
            verify(&state, "xfs");
            let error = require_settled(&state).await.unwrap_err();
            assert!(error.is_conflict() && error.message.contains("activate-pool"));
        }
        "rolled-back" => verify(&state, "ext4"),
        "authorized-read-only" => {
            let error = require_settled(&state).await.unwrap_err();
            assert!(error.is_conflict() && error.message.contains("read-only"));
        }
        "exercise" => {
            verify(&state, "xfs");
            require_settled(&state).await.unwrap();
            let receipt: Value = serde_json::from_slice(
                &std::fs::read(
                    state
                        .parent()
                        .unwrap()
                        .join("native-pool-migration/state.json"),
                )
                .unwrap(),
            )
            .unwrap();
            assert_eq!(receipt["activated"], true);
            exercise(&state).await;
        }
        phase => panic!("unsupported migration fixture phase {phase}"),
    }
    println!("pool-migration-phase-ok");
}
