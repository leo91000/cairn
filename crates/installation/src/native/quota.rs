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

pub(super) mod migration;

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

const EXT4_MAGIC: i64 = 0xef53;
const XFS_MAGIC: i64 = 0x5846_5342;
// XFS_IOC_FSGEOMETRY_V1 and Q_XGETQSTATV.
const XFS_GEOMETRY: libc::c_ulong = 0x8070_5864;
const QUOTA_STATE: u32 = 0x5808;
const XFS_PROJECT_ID_32: u32 = 1 << 11;
const XFS_REFLINK: u32 = 1 << 20;
const PROJECT_ACCOUNTING: u16 = 0x10;
const PROJECT_ENFORCEMENT: u16 = 0x20;

/// The two filesystems whose project quotas the controller enforces.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Pool {
    Ext4,
    Xfs,
}

impl Pool {
    fn of(file: &File) -> Result<Option<Self>> {
        Ok(match filesystem(file)?.f_type as i64 {
            EXT4_MAGIC => Some(Self::Ext4),
            XFS_MAGIC => Some(Self::Xfs),
            _ => None,
        })
    }

    pub(super) fn name(self) -> &'static str {
        match self {
            Self::Ext4 => "ext4",
            Self::Xfs => "xfs",
        }
    }
}

#[repr(C)]
#[derive(Default)]
struct XfsGeometry {
    block_size: u32,
    realtime_extent_size: u32,
    group_blocks: u32,
    group_count: u32,
    log_blocks: u32,
    sector_size: u32,
    inode_size: u32,
    inode_max_percent: u32,
    data_blocks: u64,
    realtime_blocks: u64,
    realtime_extents: u64,
    log_start: u64,
    uuid: [u8; 16],
    stripe_unit: u32,
    stripe_width: u32,
    version: i32,
    flags: u32,
    log_sector_size: u32,
    realtime_sector_size: u32,
    directory_block_size: u32,
}

fn xfs_geometry(file: &File) -> Result<XfsGeometry> {
    let mut geometry = XfsGeometry::default();
    if unsafe { libc::ioctl(file.as_raw_fd(), XFS_GEOMETRY, &mut geometry) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(geometry)
}

#[repr(C)]
struct QuotaState {
    version: i8,
    padding: u8,
    flags: u16,
    remainder: [u8; 156],
}

/// Accounting alone is not enough: XFS can account project usage without
/// enforcing it (`pqnoenforce`), which ext4 mounts never report as enforced.
fn project_enforced(file: &File) -> bool {
    let mut state = QuotaState {
        version: 1,
        padding: 0,
        flags: 0,
        remainder: [0; 156],
    };
    let queried = unsafe {
        libc::syscall(
            libc::SYS_quotactl_fd,
            file.as_raw_fd(),
            (QUOTA_STATE << 8) | 2,
            0,
            &mut state,
        )
    } == 0;
    queried
        && state.flags & (PROJECT_ACCOUNTING | PROJECT_ENFORCEMENT)
            == PROJECT_ACCOUNTING | PROJECT_ENFORCEMENT
}

/// XFS reports its block device number as `f_fsid`, which a loop or device
/// mapper device can change across reboots. Bind its records to the UUID
/// instead; ext4 derives `f_fsid` from its UUID, so its records are unchanged.
pub(in crate::native) fn pool_identity(file: &File) -> Result<Vec<u8>> {
    if Pool::of(file)? != Some(Pool::Xfs) {
        return filesystem_identity(file);
    }
    let uuid = xfs_geometry(file)?.uuid;
    if uuid == [0; 16] {
        return Err(Error::unavailable(
            "Native data pool has no stable filesystem UUID.",
        ));
    }
    Ok(uuid.to_vec())
}

/// Whether the pool shares blocks between clones. ext4 always copies.
pub(super) fn reflink(file: &File) -> Result<bool> {
    Ok(Pool::of(file)? == Some(Pool::Xfs) && xfs_geometry(file)?.flags & XFS_REFLINK != 0)
}

/// Inodes the pool can hold for workloads. ext4 fixes this at mkfs time. XFS
/// allocates inodes dynamically and shrinks `f_files` as data fills the pool,
/// so use its static ceiling: a grant's inode limit must not drop as the pool
/// fills, or a later resize would reject an environment's existing inodes.
fn inode_capacity(pool: &File, status: &libc::statfs) -> Result<u64> {
    if Pool::of(pool)? != Some(Pool::Xfs) {
        return Ok(status.f_files);
    }
    let geometry = xfs_geometry(pool)?;
    let ceiling = u128::from(geometry.data_blocks)
        * u128::from(geometry.block_size)
        * u128::from(geometry.inode_max_percent)
        / 100
        / u128::from(geometry.inode_size.max(1));
    Ok(u64::try_from(ceiling)
        .unwrap_or(u64::MAX)
        .max(status.f_files))
}

pub(super) fn describe(state: &Path) -> Result<serde_json::Value> {
    let pool = open(&super::workspace::data_root(state))?;
    let kind = Pool::of(&pool)?
        .ok_or_else(|| Error::unavailable("Unsupported native data pool filesystem."))?;
    Ok(serde_json::json!({
        "filesystem": kind.name(),
        "reflink": reflink(&pool)?,
        "projectQuotaEnforced": project_enforced(&pool),
    }))
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
    let Some(kind) = Pool::of(&file)? else {
        return Err(Error::unavailable(
            "Native directories require a dedicated XFS or ext4 native-data filesystem with enforced project quotas.",
        ));
    };
    if kind == Pool::Xfs && xfs_geometry(&file)?.flags & XFS_PROJECT_ID_32 == 0 {
        return Err(Error::unavailable(
            "The XFS native-data filesystem must support 32-bit project IDs (mkfs.xfs -i projid32bit=1).",
        ));
    }
    // Both filesystems list `prjquota` among their super options only when
    // project quotas are enforced; XFS shows `pqnoenforce` for accounting only.
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
    if !mounted || !enabled || !project_enforced(&file) {
        return Err(Error::unavailable(
            "Mount the native-data filesystem with enforced project quotas (-o prjquota) before starting this node; see docs/NATIVE-NODES.md.",
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

/// XFS frees a project's quota record once it has no usage and no limits,
/// where ext4 keeps an empty one. Both mean nothing is charged or limited.
fn current(file: &File, project: u32) -> Result<Quota> {
    let mut quota = Quota::default();
    match control(file, 0x800007, project, &mut quota) {
        Ok(()) => Ok(quota),
        Err(error) if matches!(error.raw_os_error(), Some(libc::ENOENT | libc::ESRCH)) => {
            Ok(Quota::default())
        }
        Err(error) => Err(Error::unavailable(format!(
            "Native project quota operation failed: {error}"
        ))),
    }
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
    // ext4 and XFS statfs on a project-inheriting directory report its
    // quota-limited geometry. Derive new limits from the project-zero pool,
    // never the old grant.
    let pool = open(
        root_path
            .parent()
            .ok_or_else(|| Error::bad("Missing native data pool."))?,
    )?;
    let status = filesystem(&pool)?;
    let total_bytes = status.f_blocks.saturating_mul(status.f_bsize as u64).max(1);
    let inodes = inode_capacity(&pool, &status)?;
    let data_inodes = inodes.saturating_sub((inodes / 100).max(64));
    let inode_limit =
        ((u128::from(bytes) * u128::from(data_inodes)) / u128::from(total_bytes)).max(1) as u64;
    let current = current(&root, project)?;
    if current.inodes > inode_limit {
        return Err(Error::conflict(
            "Native inode use exceeds the granted quota; retained data must be reconciled.",
        ));
    }
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
    Ok(current(&root, project)?.space)
}

async fn allocator_lock(state: &Path) -> Result<crate::file_lock::Guard> {
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        loop {
            match crate::file_lock::exclusive(
                &state.join("native-project-allocation.lock"),
                "Native project allocation is busy.",
            ) {
                Ok(owner) => return Ok(owner),
                Err(error) if error.is_conflict() => {
                    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                }
                Err(error) => return Err(error),
            }
        }
    })
    .await
    .map_err(|_| Error::unavailable("Native project allocation timed out; ownership retained."))?
}

pub(super) async fn prepare(state: &Path, root: &Path, bytes: u64) -> Result<()> {
    let counter = state.join("native-project-next");
    // Mount workers do not hold the global storage gate. Serialize identity
    // allocation here across runtimes/processes, including durable publication.
    let _allocator = allocator_lock(state).await?;
    // Consumed IDs are never reused, including after interrupted creation.
    let file = open(root)?;
    if Pool::of(&file)?.is_none() {
        return Err(Error::unavailable(
            "Native directories require a dedicated XFS or ext4 filesystem with project quota enforcement.",
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
            let owner = Ownership::bind(&parent, &file, project)?;
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
    fn bind(parent: &Path, workload: &File, project: u32) -> Result<Self> {
        let control_file = open(parent)?;
        let control = control_file.metadata()?;
        let data = workload.metadata()?;
        Ok(Self {
            project,
            filesystem: pool_identity(workload)?,
            control_device: control.dev(),
            control_inode: control.ino(),
            control_filesystem: filesystem_identity(&control_file)?,
            workload_device: data.dev(),
            workload_inode: data.ino(),
        })
    }

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
            || pool_identity(workload)? != self.filesystem
        {
            return Err(Error::conflict(
                "Native quota identity belongs to a replaced environment.",
            ));
        }
        Ok(())
    }
}

/// Transfer the record after an anonymous workload root is renamed. The
/// directory, project and limits stay attached to the original inode.
pub(super) async fn adopt_prepared(source: &Path, target: &Path, workload: &Path) -> Result<()> {
    let file = open(workload)?;
    let marker = target.join("quota-owner.json");
    let project = project_io(&file)?;
    if let Ok(bytes) = tokio::fs::read(&marker).await {
        let owner: Ownership = serde_json::from_slice(&bytes)?;
        owner.validate(target, &file)?;
        if owner.project == project {
            return Ok(());
        }
        return Err(Error::conflict("Claimed native quota project changed."));
    }
    let original: Ownership =
        serde_json::from_slice(&tokio::fs::read(source.join("quota-owner.json")).await?)?;
    original.validate(source, &file)?;
    if original.project != project {
        return Err(Error::conflict("Prepared native quota project changed."));
    }
    let owner = Ownership::bind(target, &file, project)?;
    super::atomic_write(&marker, &serde_json::to_vec(&owner)?).await
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
        || pool_identity(&pool)? != retirement.filesystem
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
    let mut usage = current(&pool, retirement.project)?;
    if (usage.space != 0 || usage.inodes != 0) && Pool::of(&pool)? == Some(Pool::Xfs) {
        // XFS releases unlinked inodes through its asynchronous inode GC.
        // A sync submits pending work. The affected project may report the
        // old counters for a short time after the syscall returns.
        if unsafe { libc::syncfs(pool.as_raw_fd()) } != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            usage = current(&pool, retirement.project)?;
            if (usage.space == 0 && usage.inodes == 0) || tokio::time::Instant::now() >= deadline {
                break;
            }
            // An open-unlinked consumer can retain quota indefinitely. Leave
            // its durable retirement receipt in place if it remains charged.
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    }
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
    // Persist removal on the data filesystem before discarding the recovery
    // intent on the separate control filesystem.
    pool.sync_all()?;
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

    #[tokio::test]
    async fn project_allocator_is_exclusive_across_independent_waiters() {
        let state = tempfile::tempdir().unwrap();
        let held = allocator_lock(state.path()).await.unwrap();
        let waiting = allocator_lock(state.path());
        tokio::pin!(waiting);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(20), &mut waiting)
                .await
                .is_err()
        );
        drop(held);
        let _next = tokio::time::timeout(std::time::Duration::from_secs(1), waiting)
            .await
            .unwrap()
            .unwrap();
    }

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
            filesystem: pool_identity(&data).unwrap(),
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
        let mut concurrent = Vec::new();
        for _ in 0..8 {
            let state = state.clone();
            concurrent.push(tokio::spawn(async move {
                let control = state.join("native").join(crate::config::id());
                crate::skills::private_dir(&control).await.unwrap();
                let workload = super::super::workspace::workload_for_control(&control).unwrap();
                crate::skills::private_dir(&workload).await.unwrap();
                prepare(&state, &workload, 4 * 1024 * 1024).await.unwrap();
                let project = attributes(&open(&workload).unwrap()).unwrap().project;
                (control, project)
            }));
        }
        let mut allocated = std::collections::HashSet::new();
        let mut controls = Vec::new();
        for task in concurrent {
            let (control, project) = task.await.unwrap();
            assert!(
                allocated.insert(project),
                "Concurrent environments shared a project quota"
            );
            controls.push(control);
        }
        for control in controls {
            retire(&control).await.unwrap();
        }
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
        let pool = super::super::workspace::data_root(&state);
        let initial_inodes = super::super::workspace::filesystem(&pool).unwrap().f_favail;
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
            super::super::workspace::filesystem(&pool).unwrap().f_favail > 0,
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
