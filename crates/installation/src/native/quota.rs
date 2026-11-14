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
    let status = filesystem(file)?;
    // f_fsid is the kernel's persistent filesystem identity. Pair it with the
    // owned control directory inode; a device number alone is never authority.
    Ok(unsafe {
        std::slice::from_raw_parts(
            std::ptr::from_ref(&status.f_fsid).cast::<u8>(),
            std::mem::size_of_val(&status.f_fsid),
        )
        .to_vec()
    })
}

/// This node owns project IDs on its dedicated state filesystem. No automatic
/// remount, quota activation or filesystem conversion is performed here.
pub(super) fn require(state: &Path) -> Result<()> {
    let file = open(state)?;
    if filesystem(&file)?.f_type != libc::EXT4_SUPER_MAGIC {
        return Err(Error::unavailable(
            "Native directories require a dedicated ext4 state filesystem with enforced project quotas.",
        ));
    }
    let mounted = std::fs::read_to_string("/proc/self/mountinfo")?
        .lines()
        .any(|line| {
            let Some((mount, filesystem)) = line.split_once(" - ") else {
                return false;
            };
            let fields: Vec<_> = mount.split_whitespace().collect();
            fields.get(4).is_some_and(|path| Path::new(path) == state)
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
            "Mount the dedicated native state filesystem with enforced ext4 project quotas before starting this node; see docs/NATIVE-NODES.md.",
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
    let parent = root
        .parent()
        .ok_or_else(|| Error::bad("Missing native quota control directory."))?;
    let owner: Ownership =
        serde_json::from_slice(&std::fs::read(parent.join("quota-owner.json"))?)?;
    owner.validate(parent, file)?;
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
    let mut quota = Quota {
        block_hard: bytes / 1024,
        block_soft: bytes / 1024,
        // Bound zero-length files as well as allocated data. This limit grows
        // with the same grant and cannot consume the filesystem's inode pool.
        inode_hard: (bytes / 4096).max(1024),
        inode_soft: (bytes / 4096).max(1024),
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
    let parent = root
        .parent()
        .ok_or_else(|| Error::bad("Missing native quota control directory."))?;
    if parent.join("quota-retirement.json").exists() {
        return Err(Error::conflict(
            "Native quota retirement must finish before admission.",
        ));
    }
    if root.file_name().is_none_or(|name| name != "workload")
        || file.metadata()?.dev() != open(state)?.metadata()?.dev()
    {
        return Err(Error::conflict(
            "Native workload must belong to the node's state filesystem.",
        ));
    }
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
            let control = open(parent)?.metadata()?;
            let workload = file.metadata()?;
            let owner = Ownership {
                project,
                filesystem: filesystem_identity(&file)?,
                control_device: control.dev(),
                control_inode: control.ino(),
                workload_device: workload.dev(),
                workload_inode: workload.ino(),
            };
            super::atomic_write(&marker, &serde_json::to_vec(&owner)?).await?;
            owner
        }
        Err(error) => return Err(error.into()),
    };
    owner.validate(parent, &file)?;
    match attributes(&file)?.project {
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
    workload_device: u64,
    workload_inode: u64,
}

impl Ownership {
    fn validate(&self, parent: &Path, workload: &File) -> Result<()> {
        let control = open(parent)?.metadata()?;
        let metadata = workload.metadata()?;
        if self.project == 0
            || control.dev() != self.control_device
            || control.ino() != self.control_inode
            || metadata.dev() != self.workload_device
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
    super::workspace::require_no_mounts(&control_root.join("workload"))?;
    let workload = control_root.join("workload");
    let control = open(control_root)?;
    let metadata = control.metadata()?;
    let retirement: Ownership = match tokio::fs::read(&marker).await {
        Ok(bytes) => serde_json::from_slice(&bytes)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            match tokio::fs::symlink_metadata(&workload).await {
                Ok(metadata) if !metadata.is_dir() || metadata.file_type().is_symlink() => {
                    return Err(Error::conflict(
                        "Native workload is not an owned directory.",
                    ));
                }
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
                Err(error) => return Err(error.into()),
            }
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
    if metadata.dev() != retirement.control_device
        || metadata.ino() != retirement.control_inode
        || filesystem_identity(&control)? != retirement.filesystem
    {
        return Err(Error::conflict(
            "Native quota retirement no longer belongs to this environment.",
        ));
    }
    if tokio::fs::try_exists(&workload).await? {
        let file = open(&workload)?;
        let metadata = file.metadata()?;
        if metadata.dev() != retirement.workload_device
            || metadata.ino() != retirement.workload_inode
        {
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
    operation(&control, 0x800007, retirement.project, &mut usage)?;
    if usage.space != 0 || usage.inodes != 0 {
        return Err(Error::conflict(
            "Native quota still has open or retained consumers; retirement will retry.",
        ));
    }
    let mut cleared = Quota {
        valid: 1 | 4,
        ..Default::default()
    };
    operation(&control, 0x800008, retirement.project, &mut cleared)?;
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
    use std::io::Write;

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
        let mut projects = Vec::new();
        let mut peak = 0;
        for cycle in 0..5 {
            let workload = root.join("workload");
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
            match control(&open(&state).unwrap(), 0x800007, project, &mut released) {
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
        let workload = root.join("workload");
        crate::skills::private_dir(&workload).await.unwrap();
        prepare(&state, &workload, 4 * 1024 * 1024).await.unwrap();
        let mut held = std::fs::File::create(workload.join("held")).unwrap();
        held.write_all(b"old open data").unwrap();
        held.sync_all().unwrap();
        assert!(retire(&root).await.is_err());
        let stale = tokio::fs::read(root.join("quota-retirement.json"))
            .await
            .unwrap();
        drop(held);
        retire(&root).await.unwrap();
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
