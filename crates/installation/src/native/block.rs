//! Host-mounted ext4 over the same local journal and base blocks as VMs.
mod closing;

use super::{atomic_write, workspace};
use crate::{
    error::{Error, Result},
    skills::private_dir,
    storage::{self, Disk, environment, runtime::Volume},
    validation::text,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    future::Future,
    path::{Component, Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};
use tokio_util::sync::CancellationToken;

pub(crate) const KIND: &str = "native-ext4-v1";
pub(crate) const STORAGE: &str = "ublk-rootless-v1";

/// Whether a native controller mounts this checkpoint as a block journal: a
/// disk created natively, or a VM disk that native nodes mount in place.
pub(crate) fn mounts_block(manifest: &Value) -> bool {
    manifest["kind"] == KIND || vm_disk(&manifest["runtime"]) && manifest["kind"].is_null()
}

/// A VM disk mounted natively records its VM image for a later move back.
fn vm_disk(runtime: &Value) -> bool {
    runtime["backend"] == "native" && runtime["disk"] == "vm"
}

/// Where a conversation's files live in its mounted filesystems. Native disks
/// keep them at the filesystem root; VM disks keep the guest's layout, with
/// owners translated by an idmapped mount.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct View {
    pub home: PathBuf,
    pub workspace: PathBuf,
    pub output: PathBuf,
    /// Mounted filesystem roots, in mount order.
    pub filesystems: Vec<PathBuf>,
    /// Whether file owners are translated from the VM agent identity.
    pub mapped: bool,
}

impl View {
    fn native(root: &Path) -> Self {
        let data = root.join("data");
        Self {
            home: data.join("home"),
            workspace: data.join("workspaces"),
            output: data.join("output"),
            filesystems: vec![data],
            mapped: false,
        }
    }

    /// Current VM disks keep their writable guest overlay in `upper`.
    fn vm(root: &Path, plan: &Value) -> Result<Self> {
        let data = root.join("data");
        let locate = |guest: &Path| -> Result<PathBuf> {
            if !guest.is_absolute()
                || guest
                    .components()
                    .any(|part| !matches!(part, Component::RootDir | Component::Normal(_)))
            {
                return Err(Error::bad("Invalid saved guest path."));
            }
            Ok(data.join("upper").join(guest.strip_prefix("/").unwrap()))
        };
        let output = PathBuf::from(
            plan["guestDataRoot"]
                .as_str()
                .ok_or_else(|| Error::bad("Missing native guest data root."))?,
        )
        .join("runs")
        .join(text(plan, "runId"))
        .join("output");
        Ok(Self {
            home: locate(Path::new("/home/node"))?,
            workspace: locate(Path::new(text(plan, "projectRoot")))?,
            output: locate(&output)?,
            filesystems: vec![data],
            mapped: true,
        })
    }

    /// Rootful VM and rootless native daemons cannot share their metadata.
    /// Keep both stores on the durable disk, without rewriting either format.
    pub(super) fn docker_data(&self) -> Result<PathBuf> {
        let name = if self.mapped {
            ".cairn-native-docker"
        } else {
            "docker"
        };
        Ok(self.filesystem_of(&self.home)?.join(name))
    }

    /// The mounted filesystem holding `path`, for same-filesystem staging.
    pub(crate) fn filesystem_of(&self, path: &Path) -> Result<&Path> {
        self.filesystems
            .iter()
            .rev()
            .find(|root| path.starts_with(root))
            .map(PathBuf::as_path)
            .ok_or_else(|| Error::bad("Path is outside the conversation's filesystems."))
    }

    /// Validate every guest-written bind source before the controller writes it.
    pub(super) async fn prepare(&self, uid: u32, gid: u32) -> Result<()> {
        for path in [
            &self.home,
            &self.workspace,
            &self.output,
            &self.home.join(".local"),
        ] {
            directory_beneath(self.filesystem_of(path)?, path, Some((uid, gid))).await?;
        }
        Ok(())
    }
}

/// Walks from `root` to `path` without following links. Missing components
/// are created when `owner` is given; the final one is owned by it. Returns
/// whether `path` exists as a directory afterwards.
pub(super) async fn directory_beneath(
    root: &Path,
    path: &Path,
    owner: Option<(u32, u32)>,
) -> Result<bool> {
    let relative = path
        .strip_prefix(root)
        .map_err(|_| Error::bad("Path is outside its filesystem."))?;
    let mut current = root.to_owned();
    let count = relative.components().count();
    for (index, part) in relative.components().enumerate() {
        let Component::Normal(part) = part else {
            return Err(Error::bad("Invalid conversation path."));
        };
        current.push(part);
        match tokio::fs::symlink_metadata(&current).await {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
            Ok(_) => {
                return Err(Error::conflict(
                    "A conversation directory on this disk is a link or a file.",
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let Some((uid, gid)) = owner else {
                    return Ok(false);
                };
                tokio::fs::create_dir(&current).await?;
                if index + 1 == count {
                    std::os::unix::fs::lchown(&current, Some(uid), Some(gid))?;
                }
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(true)
}

/// The view recorded when the conversation was last mounted here.
pub(crate) async fn view(state: &Path, run: &str) -> Result<View> {
    let root = workspace::directory(state, run)?;
    match tokio::fs::read(root.join("view.json")).await {
        Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(View::native(&root)),
        Err(error) => Err(error.into()),
    }
}

pub(crate) const DISK_BUSY: &str = "Native disk is active.";

pub async fn exists(state: &Path, run: &str) -> Result<bool> {
    let path = environment::directory(state, run)?.join("lazy/journal-v2.sqlite");
    exists_from_metadata(tokio::fs::metadata(path)).await
}

pub(super) async fn exists_from_metadata(
    metadata: impl Future<Output = std::io::Result<std::fs::Metadata>>,
) -> Result<bool> {
    match metadata.await {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

/// Resume the retained journal as-is even if a manager grant arrived before a
/// stop. The supervision path applies any outstanding allowance lazily after
/// mount, without bootstrap's legacy whole-image materialization.
fn preparation_size(directory: &Path, requested: u64) -> Result<u64> {
    if storage::runtime::exists(directory) {
        return Ok(storage::LazyDisk::geometry(&directory.join("lazy"))?);
    }
    Ok(requested)
}

async fn record_runtime(directory: &Path, plan: &Value) -> Result<()> {
    let mut runtime = json!({
        "backend": "native",
        "storage": STORAGE,
        "os": "linux",
        "arch": "x86_64",
        "runtimeId": plan["runtimeId"],
    });
    if directory.join("runtime.json").exists() {
        let previous: Value =
            serde_json::from_slice(&tokio::fs::read(directory.join("runtime.json")).await?)?;
        for field in ["disk", "vmRuntimeId"] {
            if let Some(value) = previous.get(field) {
                runtime[field] = value.clone();
            }
        }
        if ["backend", "os", "arch"]
            .iter()
            .any(|field| previous[*field] != runtime[*field])
            || !matches!(previous["storage"].as_str(), Some("ublk") | Some(STORAGE))
        {
            return Err(Error::conflict("Native disk platform changed."));
        }
    }
    // Older controllers require storage="ublk" before mounting/reconciling.
    // Commit the isolation fence while preserving current tool-bundle updates.
    atomic_write(
        &directory.join("runtime.json"),
        &serde_json::to_vec(&runtime)?,
    )
    .await?;
    Ok(())
}

fn ext4_bytes(superblock: &[u8; 1024]) -> Result<u64> {
    if u16::from_le_bytes(superblock[56..58].try_into().unwrap()) != 0xef53 {
        return Err(Error::conflict("Invalid native ext4 geometry."));
    }
    let low = u64::from(u32::from_le_bytes(superblock[4..8].try_into().unwrap()));
    let features = u32::from_le_bytes(superblock[96..100].try_into().unwrap());
    let high = if features & 0x80 != 0 {
        u64::from(u32::from_le_bytes(
            superblock[0x150..0x154].try_into().unwrap(),
        ))
    } else {
        0
    };
    let shift = u32::from_le_bytes(superblock[24..28].try_into().unwrap());
    let size = 1024u64
        .checked_shl(shift)
        .and_then(|block| ((high << 32) | low).checked_mul(block))
        .filter(|bytes| *bytes > 0 && *bytes <= 1 << 40)
        .ok_or_else(|| Error::conflict("Invalid native ext4 size."))?;
    Ok(size)
}

struct HostMount {
    path: PathBuf,
    run: String,
    device: String,
}

fn host_mounts(state: &Path, mountinfo: &str) -> Result<Vec<HostMount>> {
    let native = state.join("native");
    let mut mounts = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for line in mountinfo.lines() {
        let Some((mount, filesystem)) = line.split_once(" - ") else {
            continue;
        };
        let fields = mount.split_whitespace().collect::<Vec<_>>();
        let Some(path) = fields.get(4) else { continue };
        let path = PathBuf::from(
            path.replace("\\040", " ")
                .replace("\\011", "\t")
                .replace("\\012", "\n")
                .replace("\\134", "\\"),
        );
        let Ok(relative) = path.strip_prefix(&native) else {
            continue;
        };
        let components = relative.components().collect::<Vec<_>>();
        let [
            std::path::Component::Normal(run),
            std::path::Component::Normal(data),
        ] = components.as_slice()
        else {
            continue;
        };
        if *data != "data" {
            continue;
        }
        let run = run
            .to_str()
            .ok_or_else(|| Error::conflict("Invalid native mount identity."))?
            .to_owned();
        crate::validation::uuid(&run)?;
        if fields[3] != "/"
            || filesystem.split_whitespace().next() != Some("ext4")
            || !seen.insert(path.clone())
        {
            return Err(Error::conflict(
                "Unexpected or stacked native filesystem mount.",
            ));
        }
        mounts.push(HostMount {
            path,
            run,
            device: fields[2].into(),
        });
    }
    Ok(mounts)
}

fn verify_host_mount(state: &Path, mounted: &HostMount) -> Result<()> {
    use std::os::unix::fs::{FileTypeExt, MetadataExt};

    let root = workspace::directory(state, &mounted.run)?;
    if !root.join("block-initialized").exists() && !root.join("block-seeding").exists() {
        return Err(Error::conflict("Unrecognized native filesystem mount."));
    }
    let attachment = state
        .join("native-block")
        .join(&mounted.run)
        .join("root/disk.blk");
    let device = std::fs::symlink_metadata(attachment)?;
    let identity = format!(
        "{}:{}",
        libc::major(device.rdev()),
        libc::minor(device.rdev())
    );
    if !device.file_type().is_block_device() || identity != mounted.device {
        return Err(Error::conflict(
            "Native mount does not match its owned block attachment.",
        ));
    }
    Ok(())
}

/// The controller and system manager share a namespace; host mounts survive a
/// controller crash. Fence every attributed workload and non-lazily unmount only
/// verified private attachments before the existing nonce-safe device cleanup.
pub async fn recover_host_mounts(state: &Path) -> Result<()> {
    let mounts = host_mounts(state, &std::fs::read_to_string("/proc/self/mountinfo")?)?;
    for mounted in mounts {
        let root = workspace::directory(state, &mounted.run)?;
        let _workspace = crate::file_lock::exclusive(
            &root.join("owner.lock"),
            "Native workspace is still active.",
        )?;
        let owner = environment::lock(state, &mounted.run, "Native disk is still owned.").await?;
        if !storage::runtime::exists(&owner.directory) {
            return Err(Error::conflict("Mounted native journal is missing."));
        }
        verify_host_mount(state, &mounted)?;
        let identity: Value =
            serde_json::from_slice(&tokio::fs::read(owner.directory.join("runtime.json")).await?)?;
        if identity["backend"] != "native"
            || !matches!(identity["storage"].as_str(), Some("ublk") | Some(STORAGE))
        {
            return Err(Error::conflict("Mounted disk is not a native journal."));
        }
        let attempt = tokio::fs::read_to_string(root.join("attempt")).await?;
        crate::validation::uuid(&attempt)?;
        super::systemd::request_stop(&attempt).await?;
        let state = state.to_owned();
        let run = mounted.run.clone();
        let ownership = tokio::task::spawn_blocking(move || -> Result<_> {
            // Keep both leases with kernel operations if startup is cancelled.
            let ownership = (_workspace, owner);

            storage::ublk::quiesce_stale_host_disk(&state, &run)?;
            Ok(ownership)
        })
        .await
        .map_err(Error::internal)??;
        // A dead backend can leave blocked D-state consumers. Abort its I/O
        // before waiting for their cgroup fence, but never delete it yet.
        super::systemd::stop(&attempt).await?;
        tokio::task::spawn_blocking(move || -> Result<()> {
            let _ownership = ownership;
            unmount_sync(&mounted.path)?;
            Ok(())
        })
        .await
        .map_err(Error::internal)??;
    }
    Ok(())
}

fn unmount_sync(path: &Path) -> std::io::Result<()> {
    use std::os::unix::ffi::OsStrExt;

    let path = std::ffi::CString::new(path.as_os_str().as_bytes())?;
    if unsafe { libc::umount2(path.as_ptr(), 0) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

async fn unmount(path: &Path) -> Result<()> {
    let path = path.to_owned();
    tokio::task::spawn_blocking(move || unmount_sync(&path))
        .await
        .map_err(Error::internal)??;
    Ok(())
}

/// Keep the volume available to crash-consistent publication even while a
/// filesystem flush holds its lifecycle lock waiting for journal headroom.
pub struct Handle {
    pub volume: Arc<Volume>,
    pub filesystem: Arc<AtomicU64>,
    pub mount: tokio::sync::Mutex<Mounted>,
}

impl Handle {
    pub fn new(mount: Mounted) -> Self {
        Self {
            volume: mount.volume.clone(),
            filesystem: mount.filesystem.clone(),
            mount: tokio::sync::Mutex::new(mount),
        }
    }

    pub async fn abort_io(&self) -> Result<()> {
        // Wake a blocked flush before waiting for its lifecycle lock.
        self.volume.stop.cancel();
        self.mount.lock().await.abort_io().await
    }
}

pub struct Mounted {
    pub volume: Arc<Volume>,
    filesystem: Arc<AtomicU64>,
    quota_directory: PathBuf,
    target: PathBuf,
    jail: PathBuf,
    closing: closing::State,

    device: Arc<std::sync::Mutex<storage::ublk::MountedDisk>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Completion {
    Flushed,
    Aborted,
    FailedFlush,
}

impl Completion {
    pub fn exit_code(self, previous: i32) -> i32 {
        if self == Self::FailedFlush {
            return crate::runner::CONTROLLER_FAILED;
        }
        previous
    }
}

async fn open_volume(directory: &Path, allowance: u64) -> Result<Arc<Volume>> {
    let volume = storage::runtime::load(directory).await?;
    volume.limit_backing(allowance)?;
    Ok(volume)
}

pub(super) async fn mount(
    state: &Path,
    run: &str,
    plan: &Value,
    stop: &CancellationToken,
    namespace: &storage::ublk::BackendNamespace,
    identity: Option<&super::identity::Identity>,
) -> Result<Mounted> {
    {
        let owner = environment::ownership(state, run, DISK_BUSY).await?;
        let directory = &owner.directory;
        let root = workspace::directory(state, run)?;
        private_dir(&root).await?;
        if root.join("block-initialized").exists() && !storage::runtime::exists(directory) {
            return Err(Error::conflict(
                "Native journal is missing; restore its S3 checkpoint before execution.",
            ));
        }
        if directory.join("restore.pending").exists() {
            return Err(Error::conflict(
                "Native journal restore must finish before execution resumes.",
            ));
        }
        // A later execution invalidates restore idempotence.
        remove(&directory.join("recovery.json")).await?;
        tokio::fs::File::open(directory).await?.sync_all().await?;
        let requested = plan["resources"]["diskMiB"]
            .as_u64()
            .ok_or_else(|| Error::bad("Missing native disk size."))?
            .saturating_mul(super::disk::MIB);
        storage::bootstrap::prepare_native(
            directory,
            preparation_size(directory, requested)?,
            &plan["storage"],
            stop,
        )
        .await
        .map_err(|error| {
            if error.is_conflict() && error.message == storage::bootstrap::DISK_BUSY {
                Error::conflict(DISK_BUSY)
            } else {
                error
            }
        })?;
        let physical = crate::file_lock::exclusive(&directory.join("lock"), DISK_BUSY)?;
        record_runtime(directory, plan).await?;
        let volume = open_volume(directory, requested).await?;
        let runtime: Value =
            serde_json::from_slice(&tokio::fs::read(directory.join("runtime.json")).await?)?;
        let vm = vm_disk(&runtime);
        if vm && !root.join("block-initialized").exists() {
            return Err(Error::conflict(
                "A VM disk must be restored before it runs natively.",
            ));
        }
        let view = if vm {
            View::vm(&root, plan)?
        } else {
            View::native(&root)
        };
        atomic_write(&root.join("view.json"), &serde_json::to_vec(&view)?).await?;
        let target = root.join("data");
        if !root.join("block-initialized").exists() && !root.join("block-seeding").exists() {
            atomic_write(&root.join("block-seeding"), b"1").await?;
        }
        private_dir(&target).await?;
        let jail = state.join("native-block").join(run).join("root");
        private_dir(&jail).await?;
        retire_attachment(&jail).await?;
        let (backend_volume, backend_state, backend_jail) =
            (volume.clone(), state.to_owned(), jail.clone());
        let namespace = namespace.clone();
        let backend_directory = directory.to_owned();
        let device = tokio::task::spawn_blocking(move || {
            storage::ublk::mount_host_disk(
                backend_volume,
                &backend_state,
                &backend_jail,
                &namespace,
                &backend_directory,
            )
        })
        .await
        .map_err(Error::internal)??;
        let device = Arc::new(std::sync::Mutex::new(device));
        let source = jail.join("disk.blk");
        let destination = target.clone();
        let identity = identity.cloned();
        let mut mounting = tokio::task::spawn_blocking(move || -> std::io::Result<()> {
            use std::os::unix::ffi::OsStrExt;
            if vm {
                return super::idmap::mount_ext4(
                    &source,
                    &destination,
                    identity
                        .as_ref()
                        .ok_or_else(|| std::io::Error::other("Missing VM mount identity"))?,
                );
            }
            let source = std::ffi::CString::new(source.as_os_str().as_bytes())?;
            let target = std::ffi::CString::new(destination.as_os_str().as_bytes())?;
            if unsafe {
                libc::mount(
                    source.as_ptr(),
                    target.as_ptr(),
                    c"ext4".as_ptr(),
                    libc::MS_NODEV | libc::MS_NOSUID | libc::MS_NOATIME,
                    std::ptr::null(),
                )
            } != 0
            {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
        let mut aborted = false;
        let mounted = tokio::select! {
            biased;
            result = &mut mounting => result.map_err(Error::internal)?,
            () = stop.cancelled() => {
                aborted = true;
                volume.stop.cancel();
                let device = device.clone();
                tokio::task::spawn_blocking(move || device.lock().map_err(|_| std::io::Error::other("Native device lock poisoned"))?.abort_io()).await.map_err(Error::internal)??;
                mounting.await.map_err(Error::internal)?
            }
        };
        if let Err(error) = mounted {
            let _ = tokio::task::spawn_blocking(move || {
                device
                    .lock()
                    .map_err(|_| std::io::Error::other("Native device lock poisoned"))?
                    .close_in_place()
            })
            .await;
            return Err(error.into());
        }
        let mut result = Mounted {
            volume,
            target,
            jail,
            filesystem: Arc::new(AtomicU64::new(0)),
            quota_directory: directory.join("lazy"),
            device,
            closing: closing::State::new(owner, physical, aborted),
        };
        // The durable journal size is also the resize intent. A crash after
        // growing the journal/device, or a restored older filesystem image,
        // resumes resize2fs on the next owned mount without a separate marker.
        let resized = async {
            let filesystem = result.filesystem_bytes().await?;
            result.filesystem.store(filesystem, Ordering::Release);
            if filesystem > result.volume.disk.size() {
                return Err(Error::conflict(
                    "Native filesystem exceeds its journal geometry.",
                ));
            }
            if filesystem < result.volume.disk.size() {
                result.finish_growth().await?;
            }
            Ok(())
        }
        .await;
        if let Err(error) = resized {
            result.close().await?;
            return Err(error);
        }
        Ok(result)
    }
}

impl Mounted {
    /// Journal -> kernel geometry -> ext4. A crash after either geometry change
    /// leaves the journal geometry as intent for the next owned mount.
    pub(crate) async fn grow(&mut self, bytes: u64) -> Result<bool> {
        if self.closing.aborted || !self.closing.mounted || !storage::ublk::supports_growth() {
            return Err(Error::unavailable(
                "Online native disk growth is unavailable.",
            ));
        }
        if bytes < self.volume.disk.size() {
            return Err(Error::conflict(
                "Native disk allowance cannot shrink retained geometry.",
            ));
        }
        let filesystem = self.filesystem_bytes().await?;
        if filesystem > bytes {
            return Err(Error::conflict(
                "Native filesystem exceeds its journal geometry.",
            ));
        }
        if bytes == self.volume.disk.size() && filesystem == bytes {
            return Ok(true);
        }
        if bytes > self.volume.disk.size() {
            // Keep the mount and its leases alive through an uncancellable
            // metadata commit; the caller owns the native lifecycle lock.
            let disk = self.volume.disk.clone();
            let result = tokio::task::spawn_blocking(move || {
                // Fold sealed generations into the base first: none may later
                // reinstall the previous geometry. A concurrent consolidation
                // leaves them pending, and growth retries on the next tick.
                if disk.sealed_pending() {
                    disk.consolidate()?;
                }
                disk.grow(bytes)
            })
            .await
            .map_err(Error::internal)?;
            match result {
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return Ok(false),
                result => result?,
            }
        }
        let device = self.device.clone();
        tokio::task::spawn_blocking(move || {
            device
                .lock()
                .map_err(|_| std::io::Error::other("Native device lock poisoned"))?
                .grow(bytes)
        })
        .await
        .map_err(Error::internal)??;
        self.finish_growth().await?;
        Ok(true)
    }

    async fn filesystem_bytes(&self) -> Result<u64> {
        let disk = self.volume.disk.clone();
        tokio::task::spawn_blocking(move || {
            let mut bytes = [0; 1024];
            storage::Disk::read_at(disk.as_ref(), 1024, &mut bytes)?;
            ext4_bytes(&bytes)
        })
        .await
        .map_err(Error::internal)?
    }

    async fn finish_growth(&mut self) -> Result<()> {
        let mut command = tokio::process::Command::new("resize2fs");
        command.arg(self.jail.join("disk.blk")).kill_on_drop(true);
        let output = tokio::time::timeout(std::time::Duration::from_secs(120), command.output())
            .await
            .map_err(|_| {
                Error::unavailable("Native filesystem growth timed out; it will retry.")
            })??;
        if !output.status.success() {
            return Err(Error::unavailable(
                "Native filesystem growth failed; the journal and growth intent are retained.",
            ));
        }
        sync(&self.target).await?;
        let bytes = self.filesystem_bytes().await?;
        self.filesystem.store(bytes, Ordering::Release);
        if bytes != self.volume.disk.size() {
            return Err(Error::unavailable(
                "Native filesystem growth is incomplete; it will retry.",
            ));
        }
        Ok(())
    }

    /// Keep this object owned on every error: an uncertain flush/unmount cannot
    /// release its physical lease or terminate a backend still serving a mount.
    pub async fn abort_io(&mut self) -> Result<()> {
        // Abort after cancellation during preparation, a failed systemd fence,
        // or a fenced filesystem's terminal read-only flush failure. A later
        // non-lazy unmount remains mandatory. The journal is a crash prefix.
        self.closing.aborted = true;
        self.volume.stop.cancel();

        {
            let device = self.device.clone();
            tokio::task::spawn_blocking(move || {
                device
                    .lock()
                    .map_err(|_| std::io::Error::other("Native device lock poisoned"))?
                    .abort_io()
            })
            .await
            .map_err(Error::internal)??;
        }
        Ok(())
    }

    pub async fn close(&mut self) -> Result<Completion> {
        let operations = CloseOperations {
            target: &self.target,
            quota_directory: &self.quota_directory,
            jail: &self.jail,
            volume: &self.volume,
            device: &self.device,
        };
        self.closing.close(&operations).await
    }
}

struct CloseOperations<'a> {
    target: &'a Path,
    quota_directory: &'a Path,
    jail: &'a Path,
    volume: &'a Arc<Volume>,
    device: &'a Arc<std::sync::Mutex<storage::ublk::MountedDisk>>,
}

#[async_trait::async_trait]
impl closing::Operations for CloseOperations<'_> {
    async fn flush(&self) -> std::io::Result<Option<u64>> {
        self.volume.finish_fenced(sync_io(self.target)).await?;
        Ok(used_bytes(self.target).ok())
    }

    async fn abort(&self) -> Result<()> {
        self.volume.stop.cancel();
        let device = self.device.clone();
        tokio::task::spawn_blocking(move || {
            device
                .lock()
                .map_err(|_| std::io::Error::other("Native device lock poisoned"))?
                .abort_io()
        })
        .await
        .map_err(Error::internal)??;
        Ok(())
    }

    async fn unmount(&self) -> Result<()> {
        unmount(self.target).await
    }

    async fn record_usage(&self, used: u64) -> Result<()> {
        let attempt =
            tokio::fs::read_to_string(self.target.parent().unwrap().join("attempt")).await?;
        super::capacity::record_retained(self.quota_directory, &attempt, used).await
    }

    fn initialized(&self) -> bool {
        self.target
            .parent()
            .unwrap()
            .join("block-initialized")
            .exists()
    }

    async fn seal(&self) -> Result<()> {
        self.volume.seal_completed().await?;
        Ok(())
    }

    async fn retire(&self) -> Result<()> {
        self.volume.stop.cancel();
        let device = self.device.clone();
        tokio::task::spawn_blocking(move || {
            device
                .lock()
                .map_err(|_| std::io::Error::other("Native device lock poisoned"))?
                .close_in_place()
        })
        .await
        .map_err(Error::internal)??;
        retire_attachment(self.jail).await
    }
}

/// Removing the private path never retires a kernel device by its numeric ID.
/// Caller owns both conversation leases and has no mounted consumer here.
async fn retire_attachment(jail: &Path) -> Result<()> {
    use std::os::unix::fs::FileTypeExt;
    let path = jail.join("disk.blk");
    match tokio::fs::symlink_metadata(&path).await {
        Ok(metadata) if metadata.file_type().is_block_device() => {
            tokio::fs::remove_file(&path).await?;
            tokio::fs::File::open(jail).await?.sync_all().await?;
        }
        Ok(_) => return Err(Error::conflict("Native block attachment path is occupied.")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

pub async fn sync(path: &Path) -> Result<()> {
    sync_io(path).await.map_err(Into::into)
}

async fn sync_io(path: &Path) -> std::io::Result<()> {
    let path = path.to_owned();
    tokio::task::spawn_blocking(move || -> std::io::Result<()> {
        use std::os::fd::AsRawFd;
        let directory = std::fs::File::open(path)?;
        require_writable(&directory)?;
        if unsafe { libc::syncfs(directory.as_raw_fd()) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        // An error can remount ext4 during this very flush, while syncfs still
        // returns success. Check both sides before claiming completion.
        require_writable(&directory)
    })
    .await
    .map_err(std::io::Error::other)?
}

fn require_writable(directory: &std::fs::File) -> std::io::Result<()> {
    use std::os::fd::AsRawFd;

    // Native workspaces are mounted writable. ext4 can return success from
    // syncfs after shutdown, so a read-only mount is also a terminal failure.
    let mut status = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    if unsafe { libc::fstatvfs(directory.as_raw_fd(), status.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    if unsafe { status.assume_init() }.f_flag & libc::ST_RDONLY != 0 {
        return Err(std::io::Error::from_raw_os_error(libc::EROFS));
    }
    Ok(())
}

pub async fn initialize(state: &Path, run: &str, plan: &Value, budget: u64) -> Result<()> {
    let root = workspace::directory(state, run)?;
    let data = root.join("data");
    if root.join("block-initialized").exists() {
        return Ok(());
    }
    if !data.join(".cairn-seed-complete").exists() {
        // No writer has been admitted while block-seeding is present. Build the
        // initial contents off to one side and install only a complete copy.
        let staged = data.join(".cairn-initial-seed");
        if staged.exists() {
            tokio::fs::remove_dir_all(&staged).await?;
        }
        {
            private_dir(&staged).await?;
            let seed = crate::microvm::plan::Plan::new(plan.clone());
            let source = seed
                .imports()
                .find(|import| import.target == text(plan, "projectRoot"))
                .ok_or_else(|| Error::bad("Missing native workspace seed."))?;
            if !crate::project_archives::initialize(
                plan,
                source.source,
                &staged.join("workspaces"),
                budget,
            )
            .await?
            {
                workspace::copy(source.source, &staged.join("workspaces"), budget).await?;
            }
        }
        for name in ["home", "workspaces", "output"] {
            let target = data.join(name);
            if staged.join(name).exists() {
                if target.exists() {
                    tokio::fs::remove_dir_all(&target).await?;
                }
                tokio::fs::rename(staged.join(name), &target).await?;
            } else {
                private_dir(&target).await?;
            }
        }
        workspace::sync_directories(&data).await?;
        atomic_write(&data.join(".cairn-seed-complete"), run.as_bytes()).await?;
        sync(&data).await?;
        tokio::fs::remove_dir_all(staged).await?;
    }
    atomic_write(&root.join("block-initialized"), b"1").await?;
    atomic_write(&root.join("initialized"), b"1").await?;
    let runtime = tokio::fs::read(environment::directory(state, run)?.join("runtime.json")).await?;
    atomic_write(&root.join("runtime.json"), &runtime).await?;
    tokio::fs::remove_file(root.join("block-seeding")).await?;
    // Pins protect an archive only until the private disk is durably complete.
    // Retaining them for every conversation would prevent cache reclamation.
    if plan["nativeProjectArchives"].is_array() {
        let seed = crate::microvm::plan::Plan::new(plan.clone());
        if let Some(source) = seed
            .imports()
            .find(|import| import.target == text(plan, "projectRoot"))
            && let Some(parent) = source.source.parent()
        {
            match tokio::fs::remove_dir_all(parent.join("project-archives")).await {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
    }
    Ok(())
}

impl Drop for Mounted {
    fn drop(&mut self) {
        if !self.closing.retired {
            // Neither unmount nor sealing proves backend retirement. Preserve
            // the journal, backend and leases through any uncertain close.
            tracing::error!(target = %self.target.display(), "Native mount abandoned; ownership retained until controller recovery");

            std::mem::forget(self.device.clone());
            std::mem::forget(self.volume.clone());
        }
    }
}

pub async fn status(state: &Path, run: &str) -> Result<Value> {
    let directory = environment::directory(state, run)?;
    match tokio::fs::metadata(directory.join("lazy/journal-v2.sqlite")).await {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(json!({
                "mode": "native",
                "availability": "unavailable",
                "reason": crate::native::MISSING_NATIVE_JOURNAL_CODE
            }));
        }
        Err(error) => return Err(error.into()),
    }

    let volume = storage::runtime::load_for_inspection(&directory).await?;
    let mut status = volume.inspect().await?;
    status["paused"] = volume.paused().into();
    Ok(status)
}

pub async fn published(state: &Path, run: &str, receipt: &Value) -> Result<Value> {
    let volume = storage::runtime::load(&environment::directory(state, run)?).await?;
    let generation = receipt["generation"]
        .as_i64()
        .ok_or_else(|| Error::bad("Missing generation."))?;
    if receipt["grantId"] != volume.source.grant_id()? {
        return Err(Error::conflict(
            "Publication belongs to a replaced native disk.",
        ));
    }
    let backup = text(receipt, "backupId").to_owned();
    crate::validation::uuid(&backup)?;
    let disk = volume.disk.clone();
    let state = state.to_owned();
    tokio::task::spawn_blocking(move || {
        volume
            .source
            .acknowledge(&state, &disk, generation, &backup)
    })
    .await
    .map_err(Error::internal)??;
    Ok(json!({ "committed": true }))
}

/// Filesystem consistency uses a short freeze only through the durable seal;
/// hashing and upload proceed after thaw, like the guest capture path.
pub async fn capture_active(
    state: &Path,
    run: &str,
    mounted: Arc<Handle>,
    filesystem: bool,
    stop: CancellationToken,
) -> Result<Value> {
    let _capture = crate::nodes::checkpoint::prepare_capture(state, run).await?;
    let mount = if filesystem {
        Some(mounted.mount.lock().await)
    } else {
        None
    };
    if mount.as_ref().is_some_and(|mount| !mount.closing.mounted)
        || !workspace::directory(state, run)?
            .join("block-initialized")
            .exists()
    {
        return Err(Error::conflict(
            "Native filesystem is not ready for capture.",
        ));
    }
    let volume = mounted.volume.clone();
    let waiting = volume.inspect().await?["waitingFor"].is_string();
    let freeze = if filesystem && !waiting {
        freeze(
            &workspace::directory(state, run)?.join("data"),
            stop.clone(),
        )
        .await?
    } else {
        None
    };
    let crash_consistent = freeze.is_none();
    let sealed = storage::checkpoint::seal_host(state, run, volume, crash_consistent, stop).await?;
    drop(freeze);
    drop(mount);
    sealed.finish(state, run).await
}

struct Frozen {
    file: std::fs::File,
    stop: CancellationToken,
}

impl Drop for Frozen {
    fn drop(&mut self) {
        use std::os::fd::AsRawFd;
        // Linux x86-64 FIFREEZE/FITHAW, matching the supported native ABI.
        if unsafe { libc::ioctl(self.file.as_raw_fd(), 0xc0045878_u64, 0) } != 0 {
            tracing::error!(error = %std::io::Error::last_os_error(), "Native filesystem thaw failed; stopping its writer");
            self.stop.cancel();
        }
    }
}

async fn freeze(target: &Path, stop: CancellationToken) -> Result<Option<Frozen>> {
    let (send, receive) = tokio::sync::oneshot::channel();
    // Pin the mounted filesystem before handing work to the blocking pool. A
    // late freeze must never reopen an empty mountpoint on the host filesystem.
    let file = tokio::fs::File::open(target).await?.into_std().await;
    tokio::task::spawn_blocking(move || {
        use std::os::fd::AsRawFd;
        let frozen = (|| -> Result<Frozen> {
            if unsafe { libc::ioctl(file.as_raw_fd(), 0xc0045877_u64, 0) } != 0 {
                return Err(std::io::Error::last_os_error().into());
            }
            Ok(Frozen { file, stop })
        })();
        // If the freeze completed after its deadline, dropping the unsent guard
        // thaws it here. Cancellation can never abandon a later successful freeze.
        let _ = send.send(frozen);
    });
    match tokio::time::timeout(std::time::Duration::from_secs(30), receive).await {
        Ok(Ok(Ok(guard))) => Ok(Some(guard)),
        Ok(Ok(Err(error))) => Err(error),
        Ok(Err(_)) => Err(Error::unavailable("Native freeze worker stopped.")),
        Err(_) => Ok(None),
    }
}

pub async fn restore(state: &Path, run: &str, request: Value) -> Result<Value> {
    let manifest = &request["manifest"];
    if !mounts_block(manifest)
        || manifest["runtime"]["backend"] != "native"
        || !matches!(
            manifest["runtime"]["storage"].as_str(),
            Some("ublk") | Some(STORAGE)
        )
        || manifest["runtime"]["os"] != "linux"
        || manifest["runtime"]["arch"] != "x86_64"
    {
        return Err(Error::conflict(
            "Native block checkpoints require native Linux x86-64 and flat ext4.",
        ));
    }
    crate::nodes::snapshots::validate(manifest)?;
    super::systemd::tools(&crate::nodes::component_runtime())?;
    crate::validation::uuid(text(&request, "backupId"))?;
    let root = workspace::directory(state, run)?;
    private_dir(&root).await?;
    let _owner =
        crate::file_lock::exclusive(&root.join("owner.lock"), "Native workspace is active.")?;
    let pending = root.join("block-restore.pending.json");
    if pending.exists() {
        let current: Value = serde_json::from_slice(&tokio::fs::read(&pending).await?)?;
        if current["backupId"] != request["backupId"] {
            return Err(Error::conflict(
                "Another native block restore must finish first.",
            ));
        }
    } else {
        atomic_write(
            &pending,
            &serde_json::to_vec(&json!({ "backupId": request["backupId"] }))?,
        )
        .await?;
    }
    let runtime = manifest["runtime"].clone();
    let result = crate::nodes::restore::restore_journal(state, run, request).await?;
    let data = root.join("data");
    if data.exists() {
        let mut entries = tokio::fs::read_dir(&data).await?;
        if entries.next_entry().await?.is_some() {
            tokio::fs::rename(
                &data,
                root.join(format!("stale-native-{}", crate::config::id())),
            )
            .await?;
        }
    }
    private_dir(&data).await?;
    atomic_write(&root.join("runtime.json"), &serde_json::to_vec(&runtime)?).await?;
    remove(&root.join("file-owner.json")).await?;
    atomic_write(&root.join("block-initialized"), b"1").await?;
    atomic_write(&root.join("initialized"), b"1").await?;
    tokio::fs::remove_file(pending).await?;
    tokio::fs::File::open(&root).await?.sync_all().await?;
    Ok(result)
}

pub fn free_bytes(path: &Path) -> Result<u64> {
    use std::os::unix::ffi::OsStrExt;
    let path = std::ffi::CString::new(path.as_os_str().as_bytes()).map_err(Error::internal)?;
    let mut stat = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    if unsafe { libc::statvfs(path.as_ptr(), stat.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let stat = unsafe { stat.assume_init() };
    Ok(stat.f_bavail.saturating_mul(stat.f_frsize))
}

async fn remove(path: &Path) -> Result<()> {
    match tokio::fs::symlink_metadata(path).await {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
            tokio::fs::remove_dir_all(path).await?;
        }
        Ok(_) => tokio::fs::remove_file(path).await?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

pub async fn action(state: &Path, run: &str, delete: bool) -> Result<Vec<String>> {
    let lease = environment::lock(state, run, DISK_BUSY).await?;
    let directory = &lease.directory;
    let replacement = storage::runtime::replacement(directory).await?;
    let mut grants = Vec::new();
    if !delete && directory.join("restore.pending").exists() {
        return Err(Error::conflict(
            "Finish native restore before pruning local data.",
        ));
    }
    let mut entries = tokio::fs::read_dir(directory).await?;
    while let Some(entry) = entries.next_entry().await? {
        let name = entry.file_name();
        if !delete && !name.to_string_lossy().starts_with("stale-") {
            continue;
        }
        if environment::retained(&name) {
            continue;
        }
        if entry.file_type().await?.is_dir()
            && name.to_string_lossy().starts_with("stale-")
            && entry.path().join("journal-v2.sqlite").exists()
        {
            let context = storage::LazyDisk::context(&entry.path())?;
            grants.push(crate::auth::digest(text(&context, "grant")));
        }
        remove(&entry.path()).await?;
    }
    // A durable retirement receipt must never precede the journal unlinks.
    // Keep physical ownership through cache invalidation and directory sync.
    drop(replacement);
    tokio::fs::File::open(directory).await?.sync_all().await?;
    Ok(grants)
}

/// A controller crash loses kernel page cache just as killing a guest does.
/// After all owned processes are fenced, seal the recovered durable journal
/// prefix so a final request cannot accidentally acknowledge an older turn.
pub async fn recover_completed(state: &Path, run: &str, attempt: &str) -> Result<()> {
    let root = workspace::directory(state, run)?;
    if !root.join("block-initialized").exists() || !exists(state, run).await? {
        return Ok(());
    }
    let lease = environment::lock(state, run, "Native journal recovery is still owned.").await?;
    let volume = storage::runtime::load(&lease.directory).await?;
    volume.seal_completed().await?;
    atomic_write(&root.join("attempt"), attempt.as_bytes()).await?;
    Ok(())
}

pub fn used_bytes(path: &Path) -> Result<u64> {
    use std::os::unix::ffi::OsStrExt;
    let path = std::ffi::CString::new(path.as_os_str().as_bytes()).map_err(Error::internal)?;
    let mut stat = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    if unsafe { libc::statvfs(path.as_ptr(), stat.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let stat = unsafe { stat.assume_init() };
    Ok(stat
        .f_blocks
        .saturating_sub(stat.f_bfree)
        .saturating_mul(stat.f_frsize))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn native_volume_admission_consolidates_overwrites_before_the_task_allowance_is_exhausted()
     {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("disks/native");
        let block = 4 * 1024 * 1024u64;
        let disk = storage::runtime::create(
            &directory.join("lazy"),
            &json!({
                "version": 1, "size": 4 * block, "blockSize": block,
                "blocks": (0..4).map(|index| json!({ "offset": index * block, "size": block, "hash": null })).collect::<Vec<_>>()
            }),
            &json!({
                "master": "http://127.0.0.1:1/", "grant": "native-allowance",
                "policy": { "reserveMiB": 64, "reservePercent": 1 }
            }),
        )
        .await
        .unwrap();
        for value in 1..=3 {
            disk.write_at(0, &vec![value; block as usize]).unwrap();
        }
        drop(disk);
        // Use the constructor invoked before the real native ublk mount, with
        // its issued allowance, without configuring Volume::limit_backing here.
        let volume = open_volume(&directory, 2 * block).await.unwrap();
        let writing = volume.clone();
        let mut writer = tokio::task::spawn_blocking(move || writing.write_at(0, b"native"));
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let blocked = !writer.is_finished();
        let result = tokio::time::timeout(std::time::Duration::from_secs(15), &mut writer).await;
        if result.is_err() {
            volume.stop.cancel();
            let _ = writer.await;
        }
        assert!(
            blocked,
            "native admission must wait for retained journal history"
        );
        result.unwrap().unwrap().unwrap();
        assert_eq!(
            volume.disk.accounting().unwrap()["published"]["local"],
            true
        );
        assert!(storage::runtime::retained_backing_bytes(&directory).unwrap() < 2 * block);
        let mut bytes = [0; 6];
        volume.read_at(0, &mut bytes).unwrap();
        assert_eq!(&bytes, b"native");
    }

    #[tokio::test]
    async fn vm_views_reject_redirected_bind_and_runtime_directories() {
        let root = tempfile::tempdir().unwrap();
        let run = crate::config::id();
        let plan =
            json!({ "runId": run, "guestDataRoot": "/data", "projectRoot": "/data/workspace" });
        let view = View::vm(root.path(), &plan).unwrap();
        assert_eq!(
            view.workspace,
            root.path().join("data/upper/data/workspace")
        );
        assert_eq!(view.home, root.path().join("data/upper/home/node"));
        std::fs::create_dir_all(root.path().join("data/upper/home")).unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), root.path().join("data/upper/home/node"))
            .unwrap();
        assert!(view.prepare(1000, 1000).await.is_err());
        std::os::unix::fs::symlink(outside.path(), view.docker_data().unwrap()).unwrap();
        assert!(
            directory_beneath(
                &root.path().join("data"),
                &view.docker_data().unwrap(),
                Some((1000, 1000))
            )
            .await
            .is_err()
        );
        assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 0);
    }

    #[tokio::test]
    async fn vm_native_docker_preparation_preserves_the_rootful_store() {
        use std::os::unix::fs::MetadataExt;

        let root = tempfile::tempdir().unwrap();
        let data = root.path().join("data");
        let rootful = data.join("docker/containers/private");
        std::fs::create_dir_all(&rootful).unwrap();
        std::fs::write(rootful.join("config.v2.json"), b"VM daemon metadata").unwrap();
        let view = View::vm(
            root.path(),
            &json!({
                "runId": crate::config::id(), "guestDataRoot": "/data",
                "projectRoot": "/data/workspace"
            }),
        )
        .unwrap();
        let docker = view.docker_data().unwrap();
        assert_eq!(docker, data.join(".cairn-native-docker"));
        let owner = std::fs::metadata(&data).unwrap();
        directory_beneath(&data, &docker, Some((owner.uid(), owner.gid())))
            .await
            .unwrap();
        std::fs::write(docker.join("native-state"), b"rootless metadata").unwrap();
        assert_eq!(
            std::fs::read(rootful.join("config.v2.json")).unwrap(),
            b"VM daemon metadata"
        );
        assert!(!data.join("docker/native-state").exists());
        assert_eq!(
            View::native(root.path()).docker_data().unwrap(),
            data.join("docker")
        );
    }

    #[test]
    fn completed_close_preserves_cancellation_without_a_flush_failure() {
        for (completion, previous, expected) in [
            (Completion::Flushed, 0, 0),
            (Completion::Flushed, 143, 143),
            (Completion::Aborted, 143, 143),
            (Completion::Aborted, 70, 70),
            (Completion::FailedFlush, 0, 70),
            (Completion::FailedFlush, 143, 70),
        ] {
            assert_eq!(completion.exit_code(previous), expected);
        }
    }

    #[tokio::test]
    async fn missing_native_journal_status_is_typed_without_opening_sqlite() {
        let state = tempfile::tempdir().unwrap();
        let run = crate::config::id();
        let journal = environment::directory(state.path(), &run)
            .unwrap()
            .join("lazy/journal-v2.sqlite");

        let first_status = status(state.path(), &run).await.unwrap();
        let repeated = status(state.path(), &run).await.unwrap();

        assert_eq!(first_status["mode"], "native");
        assert_eq!(first_status["availability"], "unavailable");
        assert_eq!(first_status["reason"], "missing_native_journal");
        assert_eq!(repeated, first_status);
        assert!(!journal.exists());
    }

    #[tokio::test]
    async fn malformed_native_journal_is_not_reported_as_missing() {
        let state = tempfile::tempdir().unwrap();
        let run = crate::config::id();
        let directory = environment::directory(state.path(), &run).unwrap();
        tokio::fs::create_dir_all(directory.join("lazy"))
            .await
            .unwrap();
        tokio::fs::write(directory.join("lazy/journal-v2.sqlite"), b"not sqlite")
            .await
            .unwrap();

        let error = status(state.path(), &run).await.unwrap_err();

        assert_eq!(error.status, 500);
    }

    #[tokio::test]
    async fn native_disks_resume_with_current_tools_and_keep_the_platform_boundary() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("runtime.json");
        let previous = json!({
            "backend": "native", "storage": "ublk", "os": "linux", "arch": "x86_64",
            "runtimeId": "previous-release",
        });
        atomic_write(&path, &serde_json::to_vec(&previous).unwrap())
            .await
            .unwrap();
        tokio::fs::write(root.path().join("workspace"), b"retained")
            .await
            .unwrap();
        record_runtime(root.path(), &json!({ "runtimeId": "current-release" }))
            .await
            .unwrap();
        let current: Value =
            serde_json::from_slice(&tokio::fs::read(&path).await.unwrap()).unwrap();
        assert_eq!(current["runtimeId"], "current-release");
        assert_eq!(current["storage"], STORAGE);
        assert_eq!(
            tokio::fs::read(root.path().join("workspace"))
                .await
                .unwrap(),
            b"retained"
        );
        // A future isolation/storage implementation must never be downgraded
        // by rewriting its marker or touching the retained payload.
        let mut future = current.clone();
        future["storage"] = "ublk-rootless-v2".into();
        atomic_write(&path, &serde_json::to_vec(&future).unwrap())
            .await
            .unwrap();
        assert!(
            record_runtime(root.path(), &json!({ "runtimeId": "next-release" }))
                .await
                .is_err()
        );
        assert_eq!(
            serde_json::from_slice::<Value>(&tokio::fs::read(&path).await.unwrap()).unwrap(),
            future
        );
        let mut incompatible = current.clone();
        incompatible["arch"] = "aarch64".into();
        atomic_write(&path, &serde_json::to_vec(&incompatible).unwrap())
            .await
            .unwrap();
        assert!(
            record_runtime(root.path(), &json!({ "runtimeId": "next-release" }))
                .await
                .is_err()
        );
        assert_eq!(
            serde_json::from_slice::<Value>(&tokio::fs::read(path).await.unwrap()).unwrap(),
            incompatible
        );
    }

    #[tokio::test]
    async fn unapplied_grant_resumes_retained_journal_without_materializing_it() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join(crate::config::id());
        let size = 4 * 1024 * 1024;
        let manifest = json!({ "version": 1, "size": size, "blockSize": size,
            "blocks": [{ "offset": 0, "size": size, "hash": null }] });
        let context = json!({ "master": "http://127.0.0.1:1/", "grant": "synthetic", "policy": storage::policy::Policy::default() });
        let disk = storage::runtime::create(&directory.join("lazy"), &manifest, &context)
            .await
            .unwrap();
        disk.write_at(0, b"retained").unwrap();
        disk.sync().unwrap();
        drop(disk);
        let requested = 2 * size;
        let prepare = preparation_size(&directory, requested).unwrap();
        assert_eq!(prepare, size);
        storage::bootstrap::prepare_native(
            &directory,
            prepare,
            &context,
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(
            storage::LazyDisk::geometry(&directory.join("lazy")).unwrap(),
            size
        );
        assert!(!directory.join("data.ext4").exists());
        assert!(!directory.join("bootstrap.pending").exists());
    }

    #[test]
    fn host_recovery_selects_only_conversation_mounts_and_rejects_stacked_or_foreign_filesystems() {
        let root = tempfile::tempdir().unwrap();
        let state = root.path();
        let run = crate::config::id();
        let target = state.join("native").join(&run).join("data");
        let owned = format!(
            "42 1 259:3 / {} rw - ext4 /owned/disk.blk rw\n",
            target.display()
        );
        let unrelated = format!(
            "43 1 259:4 / {} rw - ext4 /other/disk rw\n",
            state.join("unrelated").display()
        );
        let child = format!(
            "44 1 259:3 / {} rw - ext4 /owned/disk.blk rw\n",
            target.join("home").display()
        );
        let mounts = host_mounts(state, &format!("{owned}{unrelated}{child}")).unwrap();
        assert_eq!(mounts.len(), 1);
        assert_eq!(mounts[0].path, target);
        assert_eq!(mounts[0].run, run);
        assert_eq!(mounts[0].device, "259:3");
        assert!(host_mounts(state, &format!("{owned}{owned}")).is_err());
        assert!(host_mounts(state, &owned.replace("- ext4", "- tmpfs")).is_err());
        assert!(host_mounts(state, &owned.replace("259:3 / ", "259:3 /subdirectory ")).is_err());
    }

    #[tokio::test]
    async fn host_recovery_never_unmounts_an_unrecognized_attachment() {
        let root = tempfile::tempdir().unwrap();
        let state = root.path();
        let run = crate::config::id();
        let workspace = workspace::directory(state, &run).unwrap();
        private_dir(&workspace).await.unwrap();
        let mounted = HostMount {
            path: workspace.join("data"),
            run: run.clone(),
            device: "0:0".into(),
        };
        assert!(verify_host_mount(state, &mounted).is_err());
        atomic_write(&workspace.join("block-initialized"), b"1")
            .await
            .unwrap();
        let jail = state.join("native-block").join(run).join("root");
        private_dir(&jail).await.unwrap();
        tokio::fs::write(jail.join("disk.blk"), b"unrelated ordinary file")
            .await
            .unwrap();
        assert!(verify_host_mount(state, &mounted).is_err());
        assert_eq!(
            tokio::fs::read(jail.join("disk.blk")).await.unwrap(),
            b"unrelated ordinary file"
        );
    }

    #[tokio::test]
    async fn attachment_cleanup_refuses_unrelated_files_and_redirects() {
        let root = tempfile::tempdir().unwrap();
        let jail = root.path().join("jail");
        private_dir(&jail).await.unwrap();
        retire_attachment(&jail).await.unwrap();
        let node = jail.join("disk.blk");
        tokio::fs::write(&node, b"unrelated owner file")
            .await
            .unwrap();
        assert!(retire_attachment(&jail).await.is_err());
        assert_eq!(
            tokio::fs::read(&node).await.unwrap(),
            b"unrelated owner file"
        );
        tokio::fs::remove_file(&node).await.unwrap();
        let outside = root.path().join("outside");
        tokio::fs::write(&outside, b"owner file").await.unwrap();
        std::os::unix::fs::symlink(&outside, &node).unwrap();
        assert!(retire_attachment(&jail).await.is_err());
        assert_eq!(tokio::fs::read(&outside).await.unwrap(), b"owner file");
    }
}
