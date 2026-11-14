//! Kernel project quotas for native directories, including their managed tmp and Docker data.
use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};
use std::{
    fs::File,
    os::{
        fd::AsRawFd,
        unix::fs::{MetadataExt, OpenOptionsExt},
    },
    path::Path,
};

#[repr(C)]
#[derive(Default)]
struct Attributes {
    flags: u32,
    extent_size: u32,
    extents: u32,
    project: u32,
    cow_extent_size: u32,
    padding: [u8; 8],
}

#[repr(C)]
#[derive(Default)]
struct Quota {
    block_hard: u64,
    block_soft: u64,
    space: u64,
    inode_hard: u64,
    inode_soft: u64,
    inodes: u64,
    block_time: u64,
    inode_time: u64,
    valid: u32,
    id: u32,
}

#[repr(C)]
#[derive(Default)]
struct Information {
    block_grace: u64,
    inode_grace: u64,
    flags: u32,
    valid: u32,
}

fn filesystem(file: &File) -> Result<libc::statfs> {
    let mut status = std::mem::MaybeUninit::<libc::statfs>::zeroed();
    if unsafe { libc::fstatfs(file.as_raw_fd(), status.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(unsafe { status.assume_init() })
}

fn filesystem_identity(file: &File) -> Result<Vec<u8>> {
    crate::storage::policy::filesystem_id(file).map_err(Into::into)
}

/// This node owns project IDs on its dedicated state filesystem. No automatic
/// remount, quota activation or filesystem conversion is performed here.
pub(super) fn require(state: &Path) -> Result<()> {
    let pool = super::workspace::data_root(state);
    let file = open(&pool)?;
    let metadata = file.metadata()?;
    if metadata.uid() != 0 || metadata.mode() & 0o022 != 0 {
        return Err(Error::unavailable(
            "Native data pool must be root-owned and not writable by other accounts.",
        ));
    }
    if metadata.dev() == open(state)?.metadata()?.dev() {
        return Err(Error::unavailable(
            "Native data and controller state require separate filesystems so exhausted data inodes cannot block recovery.",
        ));
    }
    if filesystem(&file)?.f_type != libc::EXT4_SUPER_MAGIC {
        return Err(Error::unavailable(
            "Native directories require a dedicated ext4 native-data filesystem with enforced project quotas.",
        ));
    }
    let mounted = std::fs::read_to_string("/proc/self/mountinfo")?
        .lines()
        .any(|line| {
            let Some((mount, filesystem)) = line.split_once(" - ") else {
                return false;
            };
            let fields: Vec<_> = mount.split_whitespace().collect();
            fields.get(4).is_some_and(|path| Path::new(path) == pool)
                && filesystem
                    .split_whitespace()
                    .nth(2)
                    .is_some_and(|options| options.split(',').any(|value| value == "prjquota"))
        });
    let mut information = Information::default();
    let enabled = unsafe {
        libc::syscall(
            libc::SYS_quotactl_fd,
            file.as_raw_fd(),
            (0x800005_u32 << 8) | 2,
            0,
            &mut information,
        )
    } == 0;
    if !mounted || !enabled {
        return Err(Error::unavailable(
            "Mount the native-data filesystem with enforced ext4 project quotas before starting this node; see docs/NATIVE-NODES.md.",
        ));
    }
    Ok(())
}

fn open(path: &Path) -> Result<File> {
    Ok(std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?)
}

fn attributes_io(file: &File) -> std::io::Result<Attributes> {
    let mut attributes = Attributes::default();
    if unsafe { libc::ioctl(file.as_raw_fd(), 0x801c581f, &mut attributes) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(attributes)
}

fn attributes(file: &File) -> Result<Attributes> {
    attributes_io(file).map_err(Into::into)
}

pub(in crate::native) fn project_io(file: &File) -> std::io::Result<u32> {
    Ok(attributes_io(file)?.project)
}

pub(in crate::native) fn owned_project(root: &Path, file: &File) -> Result<u32> {
    let parent = super::workspace::control_for_workload(root)?;
    let owner: Ownership =
        serde_json::from_slice(&std::fs::read(parent.join("quota-owner.json"))?)?;
    owner.validate(&parent, file)?;
    if project_io(file)? != owner.project {
        return Err(Error::conflict("Native workload quota ownership changed."));
    }
    Ok(owner.project)
}

fn set_project(file: &File, project: u32) -> Result<()> {
    let mut attributes = attributes(file)?;
    attributes.project = project;
    if file.metadata()?.is_dir() {
        attributes.flags |= 0x200; // FS_XFLAG_PROJINHERIT
    }
    if unsafe { libc::ioctl(file.as_raw_fd(), 0x401c5820, &attributes) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    file.sync_all()?;
    Ok(())
}

fn control(file: &File, command: u32, project: u32, quota: &mut Quota) -> std::io::Result<()> {
    // quotactl_fd binds the operation to the already-open filesystem, never a
    // device number that another mount could have reused after a restart.
    let result = unsafe {
        libc::syscall(
            libc::SYS_quotactl_fd,
            file.as_raw_fd(),
            (command << 8) | 2,
            project,
            quota,
        )
    };
    if result != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

fn operation(file: &File, command: u32, project: u32, quota: &mut Quota) -> Result<()> {
    control(file, command, project, quota).map_err(|error| {
        Error::unavailable(format!("Native project quota operation failed: {error}"))
    })
}

fn unused_project(file: &File, mut candidate: u32) -> Result<u32> {
    loop {
        if candidate == 0 {
            return Err(Error::conflict("Invalid native project quota counter."));
        }
        let mut quota = Quota::default();
        match control(file, 0x800009, candidate, &mut quota) {
            Err(error) if error.raw_os_error() == Some(libc::ENOENT) => return Ok(candidate),
            Err(error) => return Err(error.into()),
            Ok(()) if quota.id > candidate => return Ok(candidate),
            Ok(()) => {
                candidate = quota
                    .id
                    .checked_add(1)
                    .ok_or_else(|| Error::unavailable("Native project quota IDs exhausted."))?;
            }
        }
    }
}

pub(super) fn limit(root_path: &Path, bytes: u64) -> Result<()> {
    let root = open(root_path)?;
    let project = owned_project(root_path, &root)?;
    if project == 0 || bytes < 1024 {
        return Err(Error::conflict("Native directory has no project quota."));
    }
    let status = filesystem(&root)?;
    let total_bytes = status.f_blocks.saturating_mul(status.f_bsize as u64).max(1);
    let data_inodes = status
        .f_files
        .saturating_sub((status.f_files / 100).max(64));
    let inode_limit =
        ((u128::from(bytes) * u128::from(data_inodes)) / u128::from(total_bytes)).max(1) as u64;
    let mut current = Quota::default();
    operation(&root, 0x800007, project, &mut current)?;
    let inode_limit = inode_limit.max(current.inodes);
    let mut quota = Quota {
        block_hard: bytes / 1024,
        block_soft: bytes / 1024,
        // Bound zero-length files as well as allocated data. This limit grows
        // with the same grant and cannot consume the filesystem's inode pool.
        inode_hard: inode_limit,
        inode_soft: inode_limit,
        valid: 1 | 4,
        ..Default::default()
    };
    operation(&root, 0x800008, project, &mut quota)
}

pub(super) fn usage(root_path: &Path) -> Result<u64> {
    let root = open(root_path)?;
    let project = owned_project(root_path, &root)?;
    if project == 0 {
        return Err(Error::conflict("Native directory has no project quota."));
    }
    let mut quota = Quota::default();
    operation(&root, 0x800007, project, &mut quota)?;
    Ok(quota.space)
}

pub(super) async fn prepare(state: &Path, root: &Path, bytes: u64) -> Result<()> {
    let counter = state.join("native-project-next");
    // The controller's process-safe storage gate serializes this allocator.
    // Consumed IDs are never reused, including after interrupted creation.
    let file = open(root)?;
    if filesystem(&file)?.f_type != libc::EXT4_SUPER_MAGIC {
        return Err(Error::unavailable(
            "Native directories require a dedicated ext4 filesystem with project quota enforcement.",
        ));
    }
    let parent = super::workspace::control_for_workload(root)?;
    if parent.join("quota-retirement.json").exists() {
        return Err(Error::conflict(
            "Native quota retirement must finish before admission.",
        ));
    }
    if file.metadata()?.dev() != open(&super::workspace::data_root(state))?.metadata()?.dev()
        || file.metadata()?.dev() == open(state)?.metadata()?.dev()
    {
        return Err(Error::conflict(
            "Native workload must belong to the separate native data filesystem.",
        ));
    }
    if !file.metadata()?.is_dir() {
        return Err(Error::conflict("Native workload must be a directory."));
    }
    tokio::fs::File::open(root.parent().unwrap())
        .await?
        .sync_all()
        .await?;
    let marker = parent.join("quota-owner.json");
    let owner: Ownership = match tokio::fs::read(&marker).await {
        Ok(bytes) => serde_json::from_slice(&bytes)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if attributes(&file)?.project != 0 || std::fs::read_dir(root)?.next().is_some() {
                return Err(Error::conflict(
                    "Unrecognized native quota tree; retained files were preserved.",
                ));
            }
            let candidate = match tokio::fs::read_to_string(&counter).await {
                Ok(value) => value.parse::<u32>().map_err(Error::internal)?,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => 1,
                Err(error) => return Err(error.into()),
            };
            let project = unused_project(&file, candidate)?;
            let next = project
                .checked_add(1)
                .ok_or_else(|| Error::unavailable("Native project quota IDs exhausted."))?;
            crate::skills::atomic_write(&counter, next.to_string().as_bytes()).await?;
            tokio::fs::File::open(state).await?.sync_all().await?;
            let control_file = open(&parent)?;
            let control = control_file.metadata()?;
            let workload = file.metadata()?;
            let owner = Ownership {
                project,
                filesystem: filesystem_identity(&file)?,
                control_device: control.dev(),
                control_inode: control.ino(),
                control_filesystem: filesystem_identity(&control_file)?,
                workload_device: workload.dev(),
                workload_inode: workload.ino(),
            };
            super::atomic_write(&marker, &serde_json::to_vec(&owner)?).await?;
            owner
        }
        Err(error) => return Err(error.into()),
    };
    owner.validate(&parent, &file)?;
    match attributes(&file)?.project {
        0 if parent.join("directory.json").exists() => {
            return Err(Error::conflict(
                "Retained native quota identity is missing; recover it before resuming.",
            ));
        }
        0 => set_project(&file, owner.project)?,
        project if project == owner.project => {}
        _ => return Err(Error::conflict("Native workload project was replaced.")),
    }
    limit(root, bytes)
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Ownership {
    project: u32,
    filesystem: Vec<u8>,
    control_device: u64,
    control_inode: u64,
    control_filesystem: Vec<u8>,
    workload_device: u64,
    workload_inode: u64,
}

impl Ownership {
    fn validate(&self, parent: &Path, workload: &File) -> Result<()> {
        let control_file = open(parent)?;
        let control = control_file.metadata()?;
        let metadata = workload.metadata()?;
        if self.project == 0
            || !metadata.is_dir()
            || !control.is_dir()
            || control.ino() != self.control_inode
            || filesystem_identity(&control_file)? != self.control_filesystem
            || metadata.ino() != self.workload_inode
            || filesystem_identity(workload)? != self.filesystem
        {
            return Err(Error::conflict(
                "Native quota identity belongs to a replaced environment.",
            ));
        }
        Ok(())
    }
}

/// Caller owns the conversation and environment locks and has fenced all
/// consumers. The control receipt survives every unlink and a controller crash.
pub(super) async fn retire(control_root: &Path) -> Result<()> {
    let marker = control_root.join("quota-retirement.json");
    let workload = super::workspace::workload_for_control(control_root)?;
    super::workspace::require_no_mounts(&workload)?;
    if !control_root.join("quota-owner.json").exists()
        && !marker.exists()
        && !tokio::fs::try_exists(&workload).await?
    {
        return Ok(());
    }
    let pool = open(workload.parent().unwrap())?;
    let control = open(control_root)?;
    let metadata = control.metadata()?;
    let retirement: Ownership = match tokio::fs::read(&marker).await {
        Ok(bytes) => serde_json::from_slice(&bytes)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let present = match tokio::fs::symlink_metadata(&workload).await {
                Ok(metadata) if !metadata.is_dir() || metadata.file_type().is_symlink() => {
                    return Err(Error::conflict(
                        "Native workload is not an owned directory.",
                    ));
                }
                Ok(_) => true,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
                Err(error) => return Err(error.into()),
            };
            if !present {
                let owner: Ownership = serde_json::from_slice(
                    &tokio::fs::read(control_root.join("quota-owner.json")).await?,
                )?;
                super::atomic_write(&marker, &serde_json::to_vec(&owner)?).await?;
                owner
            } else {
                let file = open(&workload)?;
                let retirement: Ownership =
                    match tokio::fs::read(control_root.join("quota-owner.json")).await {
                        Ok(bytes) => serde_json::from_slice(&bytes)?,
                        Err(error)
                            if error.kind() == std::io::ErrorKind::NotFound
                                && attributes(&file)?.project == 0 =>
                        {
                            // A failed, unassigned preparation has no kernel quota to
                            // release. Never alter the filesystem's project-zero quota.
                            tokio::fs::remove_dir_all(&workload).await?;
                            control.sync_all()?;
                            return Ok(());
                        }
                        Err(error) => return Err(error.into()),
                    };
                retirement.validate(control_root, &file)?;
                super::atomic_write(&marker, &serde_json::to_vec(&retirement)?).await?;
                retirement
            }
        }
        Err(error) => return Err(error.into()),
    };
    match tokio::fs::read(control_root.join("quota-owner.json")).await {
        Ok(bytes) if serde_json::from_slice::<Ownership>(&bytes)? != retirement => {
            return Err(Error::conflict(
                "An old quota retirement cannot remove a replacement environment's ownership.",
            ));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    if metadata.ino() != retirement.control_inode
        || filesystem_identity(&control)? != retirement.control_filesystem
        || filesystem_identity(&pool)? != retirement.filesystem
    {
        return Err(Error::conflict(
            "Native quota retirement no longer belongs to this environment.",
        ));
    }
    if tokio::fs::try_exists(&workload).await? {
        let file = open(&workload)?;
        let metadata = file.metadata()?;
        if metadata.dev() != pool.metadata()?.dev() || metadata.ino() != retirement.workload_inode {
            return Err(Error::conflict(
                "A replacement native workload cannot be retired by an old quota receipt.",
            ));
        }
        let project = attributes(&file)?.project;
        if project != retirement.project && project != 0 {
            return Err(Error::conflict(
                "Native workload project changed during retirement.",
            ));
        }
        let mut entries = tokio::fs::read_dir(&workload).await?;
        while let Some(entry) = entries.next_entry().await? {
            if entry.file_type().await?.is_dir() {
                tokio::fs::remove_dir_all(entry.path()).await?;
            } else {
                tokio::fs::remove_file(entry.path()).await?;
            }
        }
        set_project(&file, 0)?;
    }
    let mut usage = Quota::default();
    operation(&pool, 0x800007, retirement.project, &mut usage)?;
    if usage.space != 0 || usage.inodes != 0 {
        return Err(Error::conflict(
            "Native quota still has open or retained consumers; retirement will retry.",
        ));
    }
    let mut cleared = Quota {
        valid: 1 | 4,
        ..Default::default()
    };
    operation(&pool, 0x800008, retirement.project, &mut cleared)?;
    match tokio::fs::remove_dir(&workload).await {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    match tokio::fs::remove_file(control_root.join("quota-owner.json")).await {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    tokio::fs::remove_file(marker).await?;
    control.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs::OpenOptions, io::Write};
    use tokio::io::{AsyncBufReadExt, BufReader};

    #[test]
    fn filesystem_identity_survives_device_renumbering_but_rejects_a_replaced_tree() {
        let root = tempfile::tempdir().unwrap();
        let control_path = root.path().join("control");
        let data_path = root.path().join("data");
        std::fs::create_dir(&control_path).unwrap();
        std::fs::create_dir(&data_path).unwrap();
        let control = open(&control_path).unwrap();
        let data = open(&data_path).unwrap();
        let mut owner = Ownership {
            project: 1,
            filesystem: filesystem_identity(&data).unwrap(),
            control_device: u64::MAX,
            control_inode: control.metadata().unwrap().ino(),
            control_filesystem: filesystem_identity(&control).unwrap(),
            workload_device: u64::MAX,
            workload_inode: data.metadata().unwrap().ino(),
        };
        owner.validate(&control_path, &data).unwrap();
        owner.filesystem[0] ^= 0xff;
        assert!(owner.validate(&control_path, &data).is_err());
    }

    #[test]
    #[ignore = "owned subprocess helper, invoked only by kernel_quota_lifecycle_qualification"]
    fn directory_consumer_child() {
        assert_eq!(
            std::env::var("CAIRN_NATIVE_DISPOSABLE_HOST").as_deref(),
            Ok("1")
        );
        let root = std::path::PathBuf::from(
            std::env::var_os("CAIRN_QUOTA_CONSUMER_ROOT").expect("owned fixture root"),
        );
        match std::env::var("CAIRN_QUOTA_CONSUMER_MODE").unwrap().as_str() {
            "cwd" => std::env::set_current_dir(root).unwrap(),
            "mmap" => {
                let file = OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(root.join("mapped"))
                    .unwrap();
                let memory = unsafe {
                    libc::mmap(
                        std::ptr::null_mut(),
                        4096,
                        libc::PROT_READ | libc::PROT_WRITE,
                        libc::MAP_SHARED,
                        file.as_raw_fd(),
                        0,
                    )
                };
                assert_ne!(memory, libc::MAP_FAILED);
                drop(file);
            }
            _ => panic!("unsupported fixture consumer"),
        }
        println!("fixture-consumer-ready");
        std::io::stdout().flush().unwrap();
        loop {
            std::thread::park();
        }
    }

    async fn consumer(root: &Path, mode: &str) -> tokio::process::Child {
        let mut child = tokio::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "native::quota::tests::directory_consumer_child",
                "--ignored",
                "--nocapture",
                "--test-threads=1",
            ])
            .env("CAIRN_QUOTA_CONSUMER_ROOT", root)
            .env("CAIRN_QUOTA_CONSUMER_MODE", mode)
            .stdout(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let mut reader = BufReader::new(stdout);
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                let mut line = String::new();
                assert!(reader.read_line(&mut line).await.unwrap() > 0);
                if line.contains("fixture-consumer-ready") {
                    break;
                }
            }
        })
        .await
        .unwrap();
        child
    }

    #[tokio::test]
    #[ignore = "requires the dedicated disposable ext4 quota fixture; scripts/qualify-native-quota.mjs"]
    async fn kernel_quota_lifecycle_qualification() {
        assert_eq!(
            std::env::var("CAIRN_NATIVE_DISPOSABLE_HOST").as_deref(),
            Ok("1")
        );
        assert_eq!(unsafe { libc::geteuid() }, 0);
        let state = std::path::PathBuf::from(
            std::env::var_os("CAIRN_NATIVE_QUOTA_TEST_STATE").expect("dedicated fixture state"),
        );
        require(&state).unwrap();
        let root = state.join("native").join(crate::config::id());
        crate::skills::private_dir(&root).await.unwrap();
        let foreign = state.join("native").join(crate::config::id());
        crate::skills::private_dir(&foreign).await.unwrap();
        std::fs::write(foreign.join("retained"), b"another environment").unwrap();
        let foreign_data = super::super::workspace::workload_for_control(&foreign).unwrap();
        crate::skills::private_dir(&foreign_data).await.unwrap();
        prepare(&state, &foreign_data, 4 * 1024 * 1024)
            .await
            .unwrap();
        std::fs::write(
            foreign_data.join("retained"),
            b"another environment on the same data filesystem",
        )
        .unwrap();
        let mut peer = consumer(&foreign_data, "cwd").await;
        let mut projects = Vec::new();
        let mut peak = 0;
        for cycle in 0..5 {
            let workload = super::super::workspace::workload_for_control(&root).unwrap();
            crate::skills::private_dir(&workload).await.unwrap();
            prepare(&state, &workload, 4 * 1024 * 1024).await.unwrap();
            let project = attributes(&open(&workload).unwrap()).unwrap().project;
            assert!(projects.last().is_none_or(|old| project > *old));
            projects.push(project);
            let mut file = std::fs::File::create(workload.join("build-output")).unwrap();
            let buffer = [42; 65536];
            let failure = loop {
                match file.write_all(&buffer) {
                    Ok(()) => {}
                    Err(error) => break error,
                }
                assert!(file.metadata().unwrap().len() <= 4 * 1024 * 1024);
            };
            assert!(
                matches!(failure.raw_os_error(), Some(libc::EDQUOT | libc::ENOSPC)),
                "{failure}"
            );
            // Controller metadata remains writable at byte/inode exhaustion.
            super::super::atomic_write(&root.join("attempt"), cycle.to_string().as_bytes())
                .await
                .unwrap();
            drop(file);
            prepare(&state, &workload, 8 * 1024 * 1024).await.unwrap();
            let mut file = std::fs::OpenOptions::new()
                .append(true)
                .open(workload.join("build-output"))
                .unwrap();
            file.write_all(&buffer).unwrap();
            file.sync_all().unwrap();
            peak = peak.max(usage(&workload).unwrap());
            // An unlinked but open file retains quota. Persisted retirement
            // cannot acknowledge success until that last consumer is gone.
            assert!(retire(&root).await.is_err());
            assert!(root.join("quota-retirement.json").exists());
            assert!(!workload.join("build-output").exists());
            drop(file);
            retire(&root).await.unwrap();
            retire(&root).await.unwrap();
            assert!(!workload.exists());
            assert!(!root.join("quota-retirement.json").exists());
            let mut released = Quota::default();
            match control(
                &open(&super::super::workspace::data_root(&state)).unwrap(),
                0x800007,
                project,
                &mut released,
            ) {
                Ok(()) => assert_eq!(
                    (
                        released.space,
                        released.inodes,
                        released.block_hard,
                        released.inode_hard
                    ),
                    (0, 0, 0, 0)
                ),
                Err(error) => assert!(matches!(
                    error.raw_os_error(),
                    Some(libc::ENOENT | libc::ESRCH)
                )),
            }
            assert_eq!(
                std::fs::read(foreign.join("retained")).unwrap(),
                b"another environment"
            );
        }
        let workload = super::super::workspace::workload_for_control(&root).unwrap();
        crate::skills::private_dir(&workload).await.unwrap();
        prepare(&state, &workload, 4 * 1024 * 1024).await.unwrap();
        let initial_inodes = super::super::workspace::filesystem(&workload)
            .unwrap()
            .f_favail;
        let mut inode_error = None;
        for index in 0..initial_inodes + 1 {
            match std::fs::File::create(workload.join(format!("inode-{index}"))) {
                Ok(file) => drop(file),
                Err(error) => {
                    inode_error = Some(error);
                    break;
                }
            }
        }
        assert!(matches!(
            inode_error.and_then(|error| error.raw_os_error()),
            Some(libc::EDQUOT | libc::ENOSPC)
        ));
        assert!(
            super::super::workspace::filesystem(&workload)
                .unwrap()
                .f_favail
                > 0,
            "per-environment inode quota must leave pool headroom"
        );
        super::super::atomic_write(&root.join("attempt"), b"inode-exhaustion-recovery")
            .await
            .unwrap();
        retire(&root).await.unwrap();
        crate::skills::private_dir(&workload).await.unwrap();
        prepare(&state, &workload, 4 * 1024 * 1024).await.unwrap();
        std::fs::write(workload.join("mapped"), vec![0; 4096]).unwrap();
        for mode in ["cwd", "mmap"] {
            let mut child = consumer(&workload, mode).await;
            assert!(
                super::super::systemd::directory_consumers::stop(&workload)
                    .await
                    .is_err(),
                "missed external {mode} consumer"
            );
            child.kill().await.unwrap();
            child.wait().await.unwrap();
            super::super::systemd::directory_consumers::stop(&workload)
                .await
                .unwrap();
            assert!(
                peer.try_wait().unwrap().is_none(),
                "another conversation's consumer was stopped"
            );
        }
        let mut held = std::fs::File::create(workload.join("held")).unwrap();
        held.write_all(b"old open data").unwrap();
        held.sync_all().unwrap();
        assert!(retire(&root).await.is_err());
        let stale = tokio::fs::read(root.join("quota-retirement.json"))
            .await
            .unwrap();
        drop(held);
        retire(&root).await.unwrap();
        assert_eq!(
            std::fs::read(foreign_data.join("retained")).unwrap(),
            b"another environment on the same data filesystem"
        );
        peer.kill().await.unwrap();
        peer.wait().await.unwrap();
        retire(&foreign).await.unwrap();
        crate::skills::private_dir(&workload).await.unwrap();
        prepare(&state, &workload, 4 * 1024 * 1024).await.unwrap();
        std::fs::write(workload.join("new-environment"), b"must survive").unwrap();
        super::super::atomic_write(&root.join("quota-retirement.json"), &stale)
            .await
            .unwrap();
        assert!(retire(&root).await.is_err());
        assert_eq!(
            std::fs::read(workload.join("new-environment")).unwrap(),
            b"must survive"
        );
        tokio::fs::remove_file(root.join("quota-retirement.json"))
            .await
            .unwrap();
        retire(&root).await.unwrap();
        assert_eq!(
            std::fs::read_dir(&root).unwrap().count(),
            1,
            "only the retained control record remains"
        );
        println!(
            "{}",
            serde_json::json!({ "cycles": 5, "peakAllocatedBytes": peak, "remainingWorkloads": 0, "remainingRetirementIntents": 0, "retained": ["fixture control record", "node project counter", "unrelated environment"] })
        );
    }
}
