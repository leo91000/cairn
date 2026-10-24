//! Host-mounted ext4 over the same journal, remote source and clean cache as VMs.
use super::{atomic_write, workspace};
use crate::{
    error::{Error, Result},
    skills::private_dir,
    storage::{self, environment, runtime::Volume},
    validation::text,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    path::{Component, Path, PathBuf},
    sync::Arc,
};
use tokio_util::sync::CancellationToken;

pub(crate) const KIND: &str = "native-ext4-v1";

/// Mount points of a conversation's filesystems, and their block attachments.
/// A paired VM disk keeps its larger workspace view in a second filesystem.
const FILESYSTEMS: [(&str, &str); 2] = [("data", "disk.blk"), ("conversation", "workspace.blk")];

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
    /// Home directories a paired VM disk keeps on its workspace filesystem,
    /// with the guest path each one is bound over.
    pub caches: Vec<(PathBuf, PathBuf)>,
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
            caches: Vec::new(),
            mapped: false,
        }
    }

    /// Mirrors the guest mounts: the system filesystem holds the overlay's
    /// upper layer and a paired workspace filesystem is mounted at `/data`.
    fn vm(root: &Path, plan: &Value, paired: bool) -> Result<Self> {
        let data = root.join("data");
        let conversation = root.join("conversation");
        let locate = |guest: &Path| -> Result<PathBuf> {
            if !guest.is_absolute()
                || guest
                    .components()
                    .any(|part| !matches!(part, Component::RootDir | Component::Normal(_)))
            {
                return Err(Error::bad("Invalid saved guest path."));
            }
            if paired && let Ok(rest) = guest.strip_prefix("/data") {
                return Ok(conversation.join(rest));
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
        let caches = if paired {
            [
                ("home-cache", "/home/node/.cache"),
                ("home-share", "/home/node/.local/share"),
                ("android", "/home/node/.android"),
            ]
            .into_iter()
            .map(|(name, target)| (conversation.join(".cairn-system").join(name), target.into()))
            .collect()
        } else {
            Vec::new()
        };
        let home = locate(Path::new("/home/node"))?;
        let workspace = locate(Path::new(text(plan, "projectRoot")))?;
        let output = locate(&output)?;
        let mut filesystems = vec![data.clone()];
        if paired {
            filesystems.push(conversation.clone());
        }
        Ok(Self {
            home,
            workspace,
            output,
            filesystems,
            caches,
            mapped: true,
        })
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

    /// Creates missing user directories and refuses any redirected component:
    /// a VM disk was written by its guest, and these paths become host bind
    /// sources. Returns the paired cache bindings that exist.
    pub(crate) async fn prepare(&self, uid: u32, gid: u32) -> Result<Vec<super::home::Binding>> {
        for path in [&self.home, &self.workspace, &self.output] {
            let root = self.filesystem_of(path)?;
            directory_beneath(root, path, Some((uid, gid))).await?;
        }
        // A VM may have inherited .local from its read-only base image rather
        // than copied it into the upper layer. It remains a user-owned parent
        // for local installs as well as for the paired share binding.
        directory_beneath(&self.home, &self.home.join(".local"), Some((uid, gid))).await?;
        let mut bindings = Vec::new();
        for (source, target) in &self.caches {
            let root = self.filesystem_of(source)?;
            if !directory_beneath(root, source, None).await? {
                continue;
            }
            let relative = target
                .strip_prefix("/home/node")
                .map_err(|_| Error::bad("Invalid cache binding."))?;
            directory_beneath(&self.home, &self.home.join(relative), Some((uid, gid))).await?;
            bindings.push(super::home::Binding {
                source: source.clone(),
                target: target.clone(),
                read_only: false,
            });
        }
        Ok(bindings)
    }
}

/// Walks from `root` to `path` without following links. Missing components
/// are created when `owner` is given; the final one is owned by it. Returns
/// whether `path` exists as a directory afterwards.
async fn directory_beneath(root: &Path, path: &Path, owner: Option<(u32, u32)>) -> Result<bool> {
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

pub fn exists(state: &Path, run: &str) -> Result<bool> {
    Ok(storage::runtime::exists(&environment::directory(
        state, run,
    )?))
}

struct HostMount {
    path: PathBuf,
    run: String,
    device: String,
    /// The block attachment this filesystem must be mounted from.
    attachment: &'static str,
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
            std::path::Component::Normal(name),
        ] = components.as_slice()
        else {
            continue;
        };
        let Some((_, attachment)) = FILESYSTEMS.iter().find(|(mounted, _)| *name == *mounted)
        else {
            continue;
        };
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
            attachment,
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
        .join("root")
        .join(mounted.attachment);
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
    let mut runs: Vec<(String, Vec<HostMount>)> = Vec::new();
    for mounted in mounts {
        match runs.iter_mut().find(|(run, _)| *run == mounted.run) {
            Some((_, group)) => group.push(mounted),
            None => runs.push((mounted.run.clone(), vec![mounted])),
        }
    }
    for (run, mut group) in runs {
        let root = workspace::directory(state, &run)?;
        let _workspace =
            crate::file_lock::exclusive(&root.join("owner.lock"), "Native workspace is active.")?;
        let owner = environment::lock(state, &run, "Native disk is still owned.").await?;
        if !storage::runtime::exists(&owner.directory) {
            return Err(Error::conflict("Mounted native journal is missing."));
        }
        for mounted in &group {
            verify_host_mount(state, mounted)?;
        }
        let identity: Value =
            serde_json::from_slice(&tokio::fs::read(owner.directory.join("runtime.json")).await?)?;
        if identity["backend"] != "native" || identity["storage"] != "ublk" {
            return Err(Error::conflict("Mounted disk is not a native journal."));
        }
        let attempt = tokio::fs::read_to_string(root.join("attempt")).await?;
        crate::validation::uuid(&attempt)?;
        super::systemd::request_stop(&attempt).await?;
        let state = state.to_owned();
        let quiesced = run.clone();
        let ownership = tokio::task::spawn_blocking(move || -> Result<_> {
            // Keep both leases with kernel operations if startup is cancelled.
            let ownership = (_workspace, owner);

            storage::ublk::quiesce_stale_host_disk(&state, &quiesced)?;
            Ok(ownership)
        })
        .await
        .map_err(Error::internal)??;
        // A dead backend can leave blocked D-state consumers. Abort its I/O
        // before waiting for their cgroup fence, but never delete it yet.
        super::systemd::stop(&attempt).await?;
        // Unmount the workspace filesystem before the system filesystem.
        group.reverse();
        tokio::task::spawn_blocking(move || -> Result<()> {
            let _ownership = ownership;
            for mounted in group {
                unmount_sync(&mounted.path)?;
            }
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
    pub mount: tokio::sync::Mutex<Mounted>,
}

impl Handle {
    pub fn new(mount: Mounted) -> Self {
        Self {
            volume: mount.volume.clone(),
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
    /// Filesystems still mounted, removed in reverse order after each success.
    targets: Vec<PathBuf>,
    root: PathBuf,
    jail: PathBuf,
    mounted: bool,
    completed: bool,
    aborted: bool,

    devices: Vec<Arc<std::sync::Mutex<storage::ublk::MountedDisk>>>,
    _owner: Option<environment::OwnerLease>,
    _physical: Option<crate::file_lock::Guard>,
}

/// The block views a disk exposes: one flat filesystem, or a paired VM
/// disk's system and workspace filesystems over the same journal.
fn block_views(
    volume: &Arc<Volume>,
    layout: storage::layout::Layout,
) -> Result<Vec<(&'static str, Arc<dyn storage::Disk>)>> {
    let storage::layout::Layout::Paired { system_bytes } = layout else {
        return Ok(vec![(
            FILESYSTEMS[0].1,
            volume.clone() as Arc<dyn storage::Disk>,
        )]);
    };
    let total = storage::Disk::size(volume.as_ref());
    let system = storage::DiskView::new(volume.clone(), 0, system_bytes)?;
    let workspace = storage::DiskView::new(volume.clone(), system_bytes, total - system_bytes)?;
    Ok(vec![
        (FILESYSTEMS[0].1, Arc::new(system) as Arc<dyn storage::Disk>),
        (
            FILESYSTEMS[1].1,
            Arc::new(workspace) as Arc<dyn storage::Disk>,
        ),
    ])
}

async fn close_devices(devices: Vec<Arc<std::sync::Mutex<storage::ublk::MountedDisk>>>) {
    for device in devices {
        let _ = tokio::task::spawn_blocking(move || {
            device
                .lock()
                .map_err(|_| std::io::Error::other("Native device lock poisoned"))?
                .close_in_place()
        })
        .await;
    }
}

async fn abort_devices(
    devices: &[Arc<std::sync::Mutex<storage::ublk::MountedDisk>>],
) -> Result<()> {
    for device in devices {
        let device = device.clone();
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

/// Mounts the conversation's disk for `workload`. A disk created natively is
/// mounted as is; a VM disk is mounted in place through idmapped mounts and
/// keeps its VM runtime identity for a later move back.
pub async fn mount(
    state: &Path,
    run: &str,
    plan: &Value,
    workload: (u32, u32),
    stop: &CancellationToken,
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
        // A later execution invalidates restore idempotence, just as the native
        // file backend invalidates restored.json when it acquires its writer.
        remove(&directory.join("recovery.json")).await?;
        tokio::fs::File::open(directory).await?.sync_all().await?;
        storage::bootstrap::prepare_native(
            directory,
            plan["resources"]["diskMiB"]
                .as_u64()
                .ok_or_else(|| Error::bad("Missing native disk size."))?
                .saturating_mul(super::disk::MIB),
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
        let runtime = json!({
            "backend": "native", "storage": "ublk", "os": "linux", "arch": "x86_64",
            "runtimeId": plan["runtimeId"],
        });
        let mut vm = false;
        if directory.join("runtime.json").exists() {
            let previous: Value =
                serde_json::from_slice(&tokio::fs::read(directory.join("runtime.json")).await?)?;
            // A VM disk also records its VM image; only the native fields must match.
            let current = runtime.as_object().unwrap();
            if current.iter().any(|(key, value)| previous[key] != *value) {
                return Err(Error::conflict("Native disk runtime changed."));
            }
            vm = vm_disk(&previous);
        } else {
            atomic_write(
                &directory.join("runtime.json"),
                &serde_json::to_vec(&runtime)?,
            )
            .await?;
        }
        let volume = storage::runtime::load(directory).await?;
        let layout = volume.disk.layout()?;
        if !vm && layout != storage::layout::Layout::Flat {
            return Err(Error::conflict(
                "Native workspaces require a flat ext4 layout.",
            ));
        }
        if vm && !root.join("block-initialized").exists() {
            return Err(Error::conflict(
                "A VM disk must be restored before it runs natively.",
            ));
        }
        let view = if vm {
            View::vm(&root, plan, layout != storage::layout::Layout::Flat)?
        } else {
            View::native(&root)
        };
        let target = root.join("data");
        if !root.join("block-initialized").exists() && !root.join("block-seeding").exists() {
            if target.exists() && !root.join("initialized").exists() {
                let mut entries = tokio::fs::read_dir(&target).await?;
                if entries.next_entry().await?.is_some() {
                    return Err(Error::conflict(
                        "Incomplete legacy native preparation requires review before migration.",
                    ));
                }
            }
            // Existing stopped file workspaces are retained until the new disk
            // has been published. A failed migration never erases its source.
            if target.exists() {
                if root.join("legacy-data").exists() {
                    return Err(Error::conflict("Ambiguous native migration sources."));
                }
                tokio::fs::rename(&target, root.join("legacy-data")).await?;
            }
            atomic_write(&root.join("block-seeding"), b"1").await?;
        }
        for filesystem in &view.filesystems {
            private_dir(filesystem).await?;
        }
        atomic_write(&root.join("view.json"), &serde_json::to_vec(&view)?).await?;
        let jail = state.join("native-block").join(run).join("root");
        private_dir(&jail).await?;
        retire_attachment(&jail).await?;
        // The controller's global disk lock keeps this inventory stable until
        // every backend has cloned its namespace. Verify all inherited native
        // filesystems before handing their paths to the storage transport.
        let inherited = host_mounts(
            state,
            &std::fs::read_to_string("/proc/thread-self/mountinfo")?,
        )?;
        for mounted in &inherited {
            verify_host_mount(state, mounted)?;
        }
        let inherited: Vec<PathBuf> = inherited.into_iter().map(|mounted| mounted.path).collect();
        let mut devices = Vec::new();
        for (name, disk) in block_views(&volume, layout)? {
            let (backend_state, backend_jail, inherited) =
                (state.to_owned(), jail.clone(), inherited.clone());
            let created = tokio::task::spawn_blocking(move || {
                storage::ublk::mount_host_named(
                    disk,
                    &backend_state,
                    &backend_jail,
                    inherited,
                    name,
                )
            })
            .await
            .map_err(Error::internal)?;
            match created {
                Ok(device) => devices.push(Arc::new(std::sync::Mutex::new(device))),
                Err(error) => {
                    close_devices(devices).await;
                    return Err(error.into());
                }
            }
        }
        let mut aborted = false;
        let mut targets: Vec<PathBuf> = Vec::new();
        for (filesystem, (_, attachment)) in view.filesystems.iter().zip(FILESYSTEMS) {
            let source = jail.join(attachment);
            let destination = filesystem.clone();
            let mut mounting = tokio::task::spawn_blocking(move || -> std::io::Result<()> {
                use std::os::unix::ffi::OsStrExt;
                if vm {
                    return super::idmap::mount_ext4(&source, &destination, workload);
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
            let mounted = tokio::select! {
                biased;
                result = &mut mounting => result.map_err(Error::internal)?,
                () = stop.cancelled() => {
                    aborted = true;
                    volume.stop.cancel();
                    abort_devices(&devices).await?;
                    mounting.await.map_err(Error::internal)?
                }
            };
            if let Err(error) = mounted {
                for target in targets.iter().rev() {
                    let _ = unmount(target).await;
                }
                close_devices(devices).await;
                return Err(error.into());
            }
            targets.push(filesystem.clone());
        }
        Ok(Mounted {
            volume,
            targets,
            root,
            jail,
            mounted: true,
            completed: false,
            aborted,
            devices,
            _owner: Some(owner),
            _physical: Some(physical),
        })
    }
}

impl Mounted {
    /// Keep this object owned on every error: an uncertain flush/unmount cannot
    /// release its physical lease or terminate a backend still serving a mount.
    pub async fn abort_io(&mut self) -> Result<()> {
        // Abort only after cancellation during preparation or a failed systemd
        // fence. A later non-lazy unmount remains mandatory. The retained journal
        // then represents a crash prefix, like a forcibly stopped guest.
        self.aborted = true;
        self.volume.stop.cancel();
        abort_devices(&self.devices).await
    }

    pub async fn close(&mut self) -> Result<()> {
        if self.mounted {
            unmount_remaining(&mut self.targets, self.aborted).await?;
            self.mounted = false;
        }
        if !self.completed && self.root.join("block-initialized").exists() {
            self.volume.seal_completed().await?;
            self.completed = true;
        }

        {
            self.volume.stop.cancel();
            for device in &self.devices {
                let device = device.clone();
                tokio::task::spawn_blocking(move || {
                    device
                        .lock()
                        .map_err(|_| std::io::Error::other("Native device lock poisoned"))?
                        .close_in_place()
                })
                .await
                .map_err(Error::internal)??;
            }
        }
        // No mount or backend can still use this controller-private attachment.
        retire_attachment(&self.jail).await?;
        Ok(())
    }
}

/// Remember partial progress so a later flush/unmount failure can be retried
/// without attempting to detach an already unmounted filesystem.
async fn unmount_remaining(targets: &mut Vec<PathBuf>, aborted: bool) -> Result<()> {
    while let Some(target) = targets.last() {
        if !aborted {
            sync(target).await?;
        }
        unmount(target).await?;
        targets.pop();
    }
    Ok(())
}

/// Removing the private path never retires a kernel device by its numeric ID.
/// Caller owns both conversation leases and has no mounted consumer here.
async fn retire_attachment(jail: &Path) -> Result<()> {
    use std::os::unix::fs::FileTypeExt;
    for (_, name) in FILESYSTEMS {
        let path = jail.join(name);
        match tokio::fs::symlink_metadata(&path).await {
            Ok(metadata) if metadata.file_type().is_block_device() => {
                tokio::fs::remove_file(&path).await?;
                tokio::fs::File::open(jail).await?.sync_all().await?;
            }
            Ok(_) => return Err(Error::conflict("Native block attachment path is occupied.")),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

pub async fn sync(path: &Path) -> Result<()> {
    let path = path.to_owned();
    tokio::task::spawn_blocking(move || -> std::io::Result<()> {
        use std::os::fd::AsRawFd;
        let directory = std::fs::File::open(path)?;
        if unsafe { libc::syncfs(directory.as_raw_fd()) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    })
    .await
    .map_err(Error::internal)?
    .map_err(Into::into)
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
        if root.join("legacy-data").exists() {
            workspace::copy_durable(&root.join("legacy-data"), &staged, budget).await?;
        } else {
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
        if self.mounted {
            // An unexpected unwind must not release an open filesystem to a
            // second writer. Keep its leases/backend until controller restart.
            tracing::error!(target = ?self.targets, "Native mount abandoned; ownership retained until controller recovery");
            if let Some(owner) = self._owner.take() {
                std::mem::forget(owner);
            }
            if let Some(physical) = self._physical.take() {
                std::mem::forget(physical);
            }

            for device in &self.devices {
                std::mem::forget(device.clone());
            }
            std::mem::forget(self.volume.clone());
        }
    }
}

pub async fn status(state: &Path, run: &str) -> Result<Value> {
    let volume = storage::runtime::load(&environment::directory(state, run)?).await?;
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
    tokio::task::spawn_blocking(move || disk.commit_published(generation, &backup))
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
    if mount.as_ref().is_some_and(|mount| !mount.mounted)
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
    // Every filesystem of a paired disk shares the journal: freeze them all
    // before sealing, and thaw in reverse order once the seal is durable.
    let mut frozen = Vec::new();
    if let Some(mount) = mount.as_ref().filter(|_| !waiting) {
        for target in &mount.targets {
            match freeze(target, stop.clone()).await? {
                Some(guard) => frozen.push(guard),
                None => {
                    frozen.clear();
                    break;
                }
            }
        }
    }
    let crash_consistent = frozen.is_empty();
    let sealed = storage::checkpoint::seal_host(state, run, volume, crash_consistent, stop).await?;
    while let Some(guard) = frozen.pop() {
        drop(guard);
    }
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
    // VM disks keep their flat or paired guest layout and are mounted in place.
    let vm = vm_disk(&manifest["runtime"]);
    let flat = storage::layout::Layout::from_manifest(manifest)? == storage::layout::Layout::Flat;
    if !mounts_block(manifest)
        || manifest["runtime"]["backend"] != "native"
        || manifest["runtime"]["storage"] != "ublk"
        || manifest["runtime"]["os"] != "linux"
        || manifest["runtime"]["arch"] != "x86_64"
        || !(vm || flat)
        || vm
            && !manifest["runtime"]["vmRuntimeId"]
                .as_str()
                .is_some_and(crate::nodes::valid_runtime)
    {
        return Err(Error::conflict(
            "Native block checkpoints require native Linux x86-64 and a flat native or VM ext4 disk.",
        ));
    }
    crate::nodes::snapshots::validate(manifest)?;
    super::systemd::tools(text(&manifest["runtime"], "runtimeId"))?;
    crate::validation::uuid(text(&request, "backupId"))?;
    let root = workspace::directory(state, run)?;
    private_dir(&root).await?;
    let _owner =
        crate::file_lock::exclusive(&root.join("owner.lock"), "Native workspace is active.")?;
    if root.join("legacy-block-restore.pending.json").exists()
        || root.join("restore.pending.json").exists()
    {
        return Err(Error::conflict(
            "Finish the pending native file restore first.",
        ));
    }
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
    let result = crate::nodes::restore::restore_journal(state, run, request, false).await?;
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
    let conversation = root.join("conversation");
    if conversation.exists() {
        let mut entries = tokio::fs::read_dir(&conversation).await?;
        if entries.next_entry().await?.is_some() {
            tokio::fs::rename(
                &conversation,
                root.join(format!("stale-native-{}", crate::config::id())),
            )
            .await?;
        }
    }
    atomic_write(&root.join("runtime.json"), &serde_json::to_vec(&runtime)?).await?;
    remove(&root.join("file-owner.json")).await?;
    remove(&root.join("view.json")).await?;
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

pub async fn evict(data: &Path, state: &Path, run: &str, expected: &Value) -> Result<Value> {
    let root = workspace::directory(state, run)?;
    let _owner =
        crate::file_lock::exclusive(&root.join("owner.lock"), "Native workspace is active.")?;
    let lease = environment::lock(state, run, DISK_BUSY).await?;
    if root.join("block-restore.pending.json").exists()
        || root.join("restore.pending.json").exists()
        || root.join("legacy-block-restore.pending.json").exists()
        || lease.directory.join("restore.pending").exists()
        || !root.join("block-initialized").exists()
    {
        return Err(Error::conflict(
            "Native recovery must finish before cache eviction.",
        ));
    }
    let attempt = tokio::fs::read_to_string(root.join("attempt")).await?;
    let volume = storage::runtime::load(&lease.directory).await?;
    let status = volume.inspect().await?;
    if attempt != text(expected, "attempt")
        || status["dirtyBytes"] != 0
        || status["published"]["backupId"] != expected["backupId"]
        || status["published"]["generation"] != expected["generation"]
        || status["grantId"] != expected["grantId"]
        || expected["backupId"].as_str().is_none()
        || expected["generation"].as_i64().is_none()
    {
        return Err(Error::conflict(
            "Native cache has no matching acknowledged S3 generation.",
        ));
    }
    let (state, directory) = (state.to_owned(), lease.directory.clone());
    let evicted =
        tokio::task::spawn_blocking(move || storage::cache::evict_clean(&state, &directory))
            .await
            .map_err(Error::internal)??;
    // Legacy migration is complete and its replacement is acknowledged. Other
    // stale recovery trees still need explicit prune; no S3 reference is removed.
    remove(&root.join("legacy-data")).await?;
    remove(&root.join("tmp")).await?;
    cleanup_attempt_files(data, state_path(&lease.directory)?, run).await?;
    workspace::collect_archives(state_path(&lease.directory)?, run, None).await?;
    let mut status = volume.inspect().await?;
    status["evictedBytes"] = evicted.into();
    Ok(status)
}

fn state_path(directory: &Path) -> Result<&Path> {
    directory
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| Error::bad("Invalid native disk directory."))
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

async fn cleanup_attempt_files(data: &Path, state: &Path, run: &str) -> Result<()> {
    remove(&data.join("runs").join(run)).await?;
    let mut entries = tokio::fs::read_dir(state).await?;
    while let Some(entry) = entries.next_entry().await? {
        if entry
            .path()
            .extension()
            .is_some_and(|extension| extension == "run")
            && tokio::fs::read_to_string(entry.path())
                .await
                .is_ok_and(|owner| owner == run)
        {
            let attempt = entry
                .path()
                .file_stem()
                .unwrap()
                .to_string_lossy()
                .into_owned();
            crate::validation::uuid(&attempt)?;
            for extension in ["log", "lease"] {
                remove(&state.join(&attempt).with_extension(extension)).await?;
            }
            remove(
                &state
                    .join("roots")
                    .join(&attempt)
                    .with_extension("plan.json"),
            )
            .await?;
            remove(&state.join("roots").join(&attempt)).await?;
            remove(&data.join("runner-plans").join(format!("{attempt}.json"))).await?;
        }
    }
    Ok(())
}

pub async fn action(state: &Path, run: &str, delete: bool) -> Result<Vec<String>> {
    let lease = environment::lock(state, run, DISK_BUSY).await?;
    let directory = &lease.directory;
    let _replacement = storage::runtime::replacement(directory).await?;
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
            && (entry.path().join("journal-v2.sqlite").exists()
                || entry.path().join("journal.sqlite").exists())
        {
            let context = storage::LazyDisk::context(&entry.path())?;
            grants.push(crate::auth::digest(text(&context, "grant")));
        }
        remove(&entry.path()).await?;
    }
    Ok(grants)
}

/// A controller crash loses kernel page cache just as killing a guest does.
/// After all owned processes are fenced, seal the recovered durable journal
/// prefix so a final request cannot accidentally acknowledge an older turn.
pub async fn recover_completed(state: &Path, run: &str, attempt: &str) -> Result<()> {
    let root = workspace::directory(state, run)?;
    if !root.join("block-initialized").exists() || !exists(state, run)? {
        return Ok(());
    }
    let lease = environment::lock(state, run, "Native journal recovery is still owned.").await?;
    let volume = storage::runtime::load(&lease.directory).await?;
    volume.seal_completed().await?;
    atomic_write(&root.join("attempt"), attempt.as_bytes()).await?;
    Ok(())
}

pub async fn restore_files(
    state: &Path,
    run: &str,
    request: &Value,
    budget: u64,
    growth: u64,
) -> Result<Value> {
    let root = workspace::directory(state, run)?;
    private_dir(&root).await?;
    let pending = root.join("legacy-block-restore.pending.json");
    crate::validation::uuid(text(request, "backupId"))?;
    {
        let _owner =
            crate::file_lock::exclusive(&root.join("owner.lock"), "Native workspace is active.")?;
        if root.join("block-restore.pending.json").exists() {
            return Err(Error::conflict(
                "Finish the pending native block restore first.",
            ));
        }
        if pending.exists() {
            let current: Value = serde_json::from_slice(&tokio::fs::read(&pending).await?)?;
            if current["backupId"] != request["backupId"] {
                return Err(Error::conflict(
                    "Another native file restore must finish first.",
                ));
            }
        } else {
            atomic_write(
                &pending,
                &serde_json::to_vec(&json!({ "backupId": request["backupId"] }))?,
            )
            .await?;
        }
        let lease = environment::lock(state, run, DISK_BUSY).await?;
        let _replacement = storage::runtime::replacement(&lease.directory).await?;
        if lease.directory.join("restore.pending").exists() {
            return Err(Error::conflict(
                "Finish the pending native journal restore first.",
            ));
        }
        if lease.directory.join("lazy").exists() {
            tokio::fs::rename(
                lease.directory.join("lazy"),
                lease
                    .directory
                    .join(format!("stale-native-{}", crate::config::id())),
            )
            .await?;
        }
        for name in ["runtime.json", "recovery.json", "restore.pending"] {
            let source = lease.directory.join(name);
            if source.exists() {
                tokio::fs::rename(
                    &source,
                    lease
                        .directory
                        .join(format!("stale-native-{}-{name}", crate::config::id())),
                )
                .await?;
            }
        }
        if root.join("legacy-data").exists() {
            tokio::fs::rename(
                root.join("legacy-data"),
                root.join(format!("stale-native-{}", crate::config::id())),
            )
            .await?;
        }
        tokio::fs::File::open(&lease.directory)
            .await?
            .sync_all()
            .await?;
        tokio::fs::File::open(&root).await?.sync_all().await?;
    }
    let result = workspace::restore(state, run, request, budget, growth).await?;
    for name in ["block-initialized", "block-seeding", "file-owner.json"] {
        remove(&root.join(name)).await?;
    }
    remove(&pending).await?;
    tokio::fs::File::open(&root).await?.sync_all().await?;
    Ok(result)
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

    /// Run inside `unshare -Urm`: a nested mount holds the system filesystem
    /// busy after the paired workspace has already been detached.
    #[tokio::test]
    #[ignore = "requires a private user and mount namespace"]
    async fn paired_unmount_retries_only_filesystems_still_attached() {
        use std::os::unix::ffi::OsStrExt;

        fn tmpfs(path: &Path) {
            std::fs::create_dir(path).unwrap();
            let path = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
            assert_eq!(
                unsafe {
                    libc::mount(
                        c"tmpfs".as_ptr(),
                        path.as_ptr(),
                        c"tmpfs".as_ptr(),
                        libc::MS_NODEV | libc::MS_NOSUID,
                        std::ptr::null(),
                    )
                },
                0,
                "{}",
                std::io::Error::last_os_error(),
            );
        }

        let root = tempfile::tempdir().unwrap();
        let system = root.path().join("data");
        let workspace = root.path().join("conversation");
        let busy = system.join("busy");
        tmpfs(&system);
        tmpfs(&workspace);
        tmpfs(&busy);
        let mut targets = vec![system.clone(), workspace];

        assert!(unmount_remaining(&mut targets, false).await.is_err());
        assert_eq!(targets, vec![system]);
        unmount(&busy).await.unwrap();
        unmount_remaining(&mut targets, false).await.unwrap();
        assert!(targets.is_empty());
        unmount_remaining(&mut targets, false).await.unwrap();
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
        // A paired VM disk mounts its workspace view from a second attachment.
        let paired = format!(
            "45 1 259:5 / {} rw - ext4 /owned/workspace.blk rw\n",
            state
                .join("native")
                .join(&run)
                .join("conversation")
                .display()
        );
        let mounts = host_mounts(state, &format!("{owned}{paired}")).unwrap();
        assert_eq!(mounts.len(), 2);
        assert_eq!(mounts[1].attachment, "workspace.blk");
        assert_eq!(mounts[0].attachment, "disk.blk");
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
            attachment: "disk.blk",
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

    fn plan(run: &str) -> Value {
        json!({
            "runId": run,
            "projectRoot": format!("/data/runs/{run}/workspace"),
            "guestDataRoot": "/data",
        })
    }

    #[test]
    fn vm_disks_keep_the_guest_layout_of_both_filesystems() {
        let root = Path::new("/state/native/run");
        let flat = View::vm(root, &plan("run"), false).unwrap();
        assert_eq!(flat.home, root.join("data/upper/home/node"));
        assert_eq!(
            flat.workspace,
            root.join("data/upper/data/runs/run/workspace")
        );
        assert_eq!(flat.output, root.join("data/upper/data/runs/run/output"));
        assert_eq!(flat.filesystems, vec![root.join("data")]);
        assert!(flat.caches.is_empty() && flat.mapped);

        let paired = View::vm(root, &plan("run"), true).unwrap();
        assert_eq!(paired.home, root.join("data/upper/home/node"));
        assert_eq!(
            paired.workspace,
            root.join("conversation/runs/run/workspace")
        );
        assert_eq!(paired.output, root.join("conversation/runs/run/output"));
        assert_eq!(
            paired.filesystem_of(&paired.workspace).unwrap(),
            root.join("conversation")
        );
        assert_eq!(
            paired.filesystem_of(&paired.home).unwrap(),
            root.join("data")
        );
        assert_eq!(
            paired.caches[1],
            (
                root.join("conversation/.cairn-system/home-share"),
                PathBuf::from("/home/node/.local/share")
            )
        );

        let mut escaping = plan("run");
        escaping["projectRoot"] = "/data/runs/../../etc".into();
        assert!(View::vm(root, &escaping, true).is_err());
        assert_eq!(View::native(root).workspace, root.join("data/workspaces"));
    }

    #[tokio::test]
    async fn guest_written_directories_never_redirect_host_bind_sources() {
        use std::os::unix::fs::MetadataExt;

        let root = tempfile::tempdir().unwrap();
        let data = root.path().join("data");
        let owner = std::fs::metadata(root.path()).unwrap();
        let view = View::vm(root.path(), &plan("run"), true).unwrap();
        std::fs::create_dir_all(&data).unwrap();
        std::fs::create_dir_all(root.path().join("conversation/.cairn-system/home-cache")).unwrap();

        let bindings = view.prepare(owner.uid(), owner.gid()).await.unwrap();
        for path in [
            &view.home,
            &view.workspace,
            &view.output,
            &view.home.join(".cache"),
        ] {
            assert!(std::fs::symlink_metadata(path).unwrap().is_dir());
        }
        assert_eq!(bindings.len(), 1);
        assert_eq!(bindings[0].target, Path::new("/home/node/.cache"));

        let outside = tempfile::tempdir().unwrap();
        std::fs::remove_dir(&view.output).unwrap();
        std::os::unix::fs::symlink(outside.path(), &view.output).unwrap();
        assert!(view.prepare(owner.uid(), owner.gid()).await.is_err());
        std::fs::remove_file(&view.output).unwrap();
        std::fs::remove_dir_all(data.join("upper/home")).unwrap();
        std::fs::create_dir_all(data.join("upper")).unwrap();
        std::os::unix::fs::symlink(outside.path(), data.join("upper/home")).unwrap();
        assert!(view.prepare(owner.uid(), owner.gid()).await.is_err());
        assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 0);
    }
}
