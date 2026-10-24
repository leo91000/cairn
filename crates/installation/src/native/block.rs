//! Host-mounted ext4 over the same journal, remote source and clean cache as VMs.
use super::{atomic_write, workspace};
use crate::{
    error::{Error, Result},
    skills::private_dir,
    storage::{self, environment, runtime::Volume},
    validation::text,
};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio_util::sync::CancellationToken;

pub(crate) const KIND: &str = "native-ext4-v1";

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
        if identity["backend"] != "native" || identity["storage"] != "ublk" {
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
    target: PathBuf,
    jail: PathBuf,
    mounted: bool,
    completed: bool,
    aborted: bool,

    device: Arc<std::sync::Mutex<storage::ublk::MountedDisk>>,
    _owner: Option<environment::OwnerLease>,
    _physical: Option<crate::file_lock::Guard>,
}

pub async fn mount(
    state: &Path,
    run: &str,
    plan: &Value,
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
            "nativeIsolation": "rootless-v1",
        });
        if directory.join("runtime.json").exists() {
            let previous: Value =
                serde_json::from_slice(&tokio::fs::read(directory.join("runtime.json")).await?)?;
            let mut upgraded = previous.clone();
            if upgraded.get("nativeIsolation").is_none() {
                upgraded["nativeIsolation"] = "rootless-v1".into();
            }
            if upgraded != runtime {
                return Err(Error::conflict("Native disk runtime changed."));
            }
        }
        // Commit the reader fence before mounting or changing any file owner.
        // Previous controllers compare the complete runtime identity and reject
        // this additional field, including after a restore on another node.
        {
            atomic_write(
                &directory.join("runtime.json"),
                &serde_json::to_vec(&runtime)?,
            )
            .await?;
        }
        let volume = storage::runtime::load(directory).await?;
        if volume.disk.layout()? != storage::layout::Layout::Flat {
            return Err(Error::conflict(
                "Native workspaces require a flat ext4 layout.",
            ));
        }
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
        private_dir(&target).await?;
        let jail = state.join("native-block").join(run).join("root");
        private_dir(&jail).await?;
        retire_attachment(&jail).await?;
        let (backend_volume, backend_state, backend_jail) =
            (volume.clone(), state.to_owned(), jail.clone());
        // The controller's global disk lock keeps this inventory stable until
        // the backend has cloned its namespace. Verify all inherited native
        // filesystems before handing their paths to the storage transport.
        let inherited = host_mounts(
            state,
            &std::fs::read_to_string("/proc/thread-self/mountinfo")?,
        )?;
        for mounted in &inherited {
            verify_host_mount(state, mounted)?;
        }
        let inherited = inherited.into_iter().map(|mounted| mounted.path).collect();
        let device = tokio::task::spawn_blocking(move || {
            storage::ublk::mount_host_disk(backend_volume, &backend_state, &backend_jail, inherited)
        })
        .await
        .map_err(Error::internal)??;
        let device = Arc::new(std::sync::Mutex::new(device));
        let source = jail.join("disk.blk");
        let destination = target.clone();
        let mut mounting = tokio::task::spawn_blocking(move || -> std::io::Result<()> {
            use std::os::unix::ffi::OsStrExt;
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
        Ok(Mounted {
            volume,
            target,
            jail,
            mounted: true,
            completed: false,
            aborted,
            device,
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

    pub async fn close(&mut self) -> Result<()> {
        if self.mounted {
            if !self.aborted {
                sync(&self.target).await?;
            }
            unmount(&self.target).await?;
            self.mounted = false;
        }
        if !self.completed
            && self
                .target
                .parent()
                .unwrap()
                .join("block-initialized")
                .exists()
        {
            self.volume.seal_completed().await?;
            self.completed = true;
        }

        {
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
        }
        // No mount or backend can still use this controller-private attachment.
        retire_attachment(&self.jail).await?;
        Ok(())
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
            tracing::error!(target = %self.target.display(), "Native mount abandoned; ownership retained until controller recovery");
            if let Some(owner) = self._owner.take() {
                std::mem::forget(owner);
            }
            if let Some(physical) = self._physical.take() {
                std::mem::forget(physical);
            }

            std::mem::forget(self.device.clone());
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
    if manifest["kind"] != KIND
        || manifest["runtime"]["backend"] != "native"
        || manifest["runtime"]["storage"] != "ublk"
        || manifest["runtime"]["os"] != "linux"
        || manifest["runtime"]["arch"] != "x86_64"
        || storage::layout::Layout::from_manifest(manifest)? != storage::layout::Layout::Flat
    {
        return Err(Error::conflict(
            "Native block checkpoints require native Linux x86-64 and flat ext4.",
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
