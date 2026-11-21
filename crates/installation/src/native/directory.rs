//! Retained native directories. Only legacy conversion opens a journal backend.
use super::{atomic_write, block, quota, workspace};
use crate::{
    error::{Error, Result},
    skills::private_dir,
    storage::{self, environment},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering},
    },
};
use tokio_util::sync::CancellationToken;

#[cfg(test)]
pub(crate) use block::initialize;
pub(crate) use block::{Completion, DISK_BUSY, free_bytes, view};

pub(crate) const STORAGE: &str = "local-directory-v1";

pub struct Volume {
    root: PathBuf,
    state: PathBuf,
    pub stop: CancellationToken,
    paused: AtomicBool,
    pressure: AtomicU8,
    observed: AtomicU64,
}

impl Volume {
    pub fn activate_attempt(&self, attempt: &str) -> Result<()> {
        crate::validation::uuid(attempt)
    }

    pub fn set_paused(&self, value: bool) {
        self.paused.store(value, Ordering::Release);
    }

    pub fn cpu_pause_reason(&self) -> Result<Option<&'static str>> {
        if crate::nodes::boot_ms().saturating_sub(self.observed.load(Ordering::Acquire)) > 15_000 {
            return Ok(Some("storage-observation"));
        }
        Ok(match self.pressure.load(Ordering::Acquire) {
            0 => None,
            1 => Some("disk-space"),
            _ => Some("storage-observation"),
        })
    }

    pub fn paused(&self) -> bool {
        self.paused.load(Ordering::Acquire)
    }

    pub fn limit_backing(&self, bytes: u64) -> Result<()> {
        quota::limit(&self.root, bytes)
    }
}

pub struct Handle {
    pub volume: Arc<Volume>,
    pub filesystem: Arc<AtomicU64>,
    pub mount: tokio::sync::Mutex<Mounted>,
    pub(super) ingress: Arc<tokio::sync::Mutex<()>>,
}

impl Handle {
    pub fn new(mount: Mounted) -> Self {
        observe(Arc::downgrade(&mount.volume));
        Self {
            volume: mount.volume.clone(),
            filesystem: mount.filesystem.clone(),
            mount: tokio::sync::Mutex::new(mount),
            ingress: Arc::new(tokio::sync::Mutex::new(())),
        }
    }

    pub async fn fence_consumers(&self) -> Result<()> {
        super::systemd::directory_consumers::stop(&self.volume.root).await
    }

    pub async fn abort_io(&self) -> Result<()> {
        self.volume.stop.cancel();
        self.mount.lock().await.aborted = true;
        Ok(())
    }
}

pub struct Mounted {
    volume: Arc<Volume>,
    filesystem: Arc<AtomicU64>,
    owner: environment::OwnerLease,
    _physical: crate::file_lock::Guard,
    aborted: bool,
}

impl Mounted {
    pub async fn grow(&mut self, bytes: u64) -> Result<bool> {
        if self.aborted || self.volume.stop.is_cancelled() {
            return Err(Error::unavailable("Native directory is closing."));
        }
        if bytes < self.filesystem.load(Ordering::Acquire) {
            return Err(Error::conflict(
                "Native quota cannot shrink retained capacity.",
            ));
        }
        quota::limit(&self.volume.root, bytes)?;
        atomic_write(
            &self.owner.directory.join("directory-limit"),
            bytes.to_string().as_bytes(),
        )
        .await?;
        self.filesystem.store(bytes, Ordering::Release);
        Ok(true)
    }

    pub async fn close(&mut self) -> Result<Completion> {
        super::systemd::directory_consumers::stop(&self.volume.root).await?;
        let completion = if self.aborted {
            Completion::Aborted
        } else {
            match block::sync_io(&self.volume.root).await {
                Ok(()) => Completion::Flushed,
                Err(error) if matches!(error.raw_os_error(), Some(libc::EROFS | libc::EIO)) => {
                    tracing::error!(%error, "Fenced native directory flush failed; retaining the filesystem crash prefix");
                    Completion::FailedFlush
                }
                Err(error) => return Err(error.into()),
            }
        };
        if completion != Completion::FailedFlush {
            retire_temporary(
                &self.volume.root,
                &workspace::control_for_workload(&self.volume.root)?,
            )
            .await?;
        }
        let record = async {
            let control = workspace::control_for_workload(&self.volume.root)?;
            let attempt = tokio::fs::read_to_string(control.join("attempt")).await?;
            super::capacity::record_retained(
                &self.owner.directory,
                &attempt,
                quota::usage(&self.volume.root)?,
            )
            .await
        }
        .await;
        if let Err(error) = record {
            tracing::warn!(message = %error.message, "Native retained capacity credit unavailable");
        }
        self.volume.stop.cancel();
        Ok(completion)
    }
}

pub async fn exists(state: &Path, run: &str) -> Result<bool> {
    let directory = environment::directory(state, run)?;
    Ok(directory.join("directory.json").exists()
        || storage::runtime::exists(&directory)
        || storage::bootstrap::resize_source(&directory)?.is_some())
}

pub async fn mount(
    state: &Path,
    run: &str,
    plan: &Value,
    stop: &CancellationToken,
) -> Result<Mounted> {
    super::cleanup::check(state, run)?;
    if stop.is_cancelled() {
        return Err(Error::unavailable("Native preparation stopped."));
    }
    let root = workspace::directory(state, run)?;
    private_dir(&root).await?;
    let bytes = plan["resources"]["diskMiB"]
        .as_u64()
        .and_then(|size| size.checked_mul(super::disk::MIB))
        .ok_or_else(|| Error::bad("Missing native directory allowance."))?;
    let workload = workspace::workload(state, run)?;
    workspace::require_no_mounts(&workload)?;
    let saved = environment::directory(state, run)?;
    if !saved.join("directory.json").exists()
        && storage::bootstrap::resize_source(&saved)?.is_some()
    {
        let lease = environment::lock(state, run, DISK_BUSY).await?;
        storage::bootstrap::recover_resize_source(&lease.directory, &plan["storage"], "native")
            .await?;
    }
    if saved.join("directory.json").exists() && root.join("block-initialized").exists() {
        require_retained_tree(&workload)?;
    }
    if !saved.join("directory.json").exists() && storage::runtime::exists(&saved) {
        private_dir(&workload).await?;
        migrate(state, run, plan, stop, &root).await?;
    }
    let owner = environment::ownership(state, run, DISK_BUSY).await?;
    let physical = crate::file_lock::exclusive(&owner.directory.join("lock"), DISK_BUSY)?;
    if owner.directory.join("directory.json").exists() && owner.directory.join("lazy").exists() {
        // The new format wins over partial source cleanup only while its
        // independently verified replacement still exists and belongs to us.
        quota::usage(&workload)?;
        let proof = read_conversion_proof(&root).await?;
        verify_conversion_target(&workload.join("data"), &proof).await?;
        let _replacement = storage::runtime::replacement(&owner.directory).await?;
        tokio::fs::remove_dir_all(owner.directory.join("lazy")).await?;
        tokio::fs::File::open(&owner.directory)
            .await?
            .sync_all()
            .await?;
        remove_conversion_proof(&root).await?;
    }
    if root.join("block-initialized").exists() && !owner.directory.join("directory.json").exists() {
        return Err(Error::conflict(
            "Native journal is missing; recover or expire the environment explicitly before starting a new one.",
        ));
    }
    let runtime = format(plan);
    let marker = owner.directory.join("directory.json");
    if marker.exists() {
        let previous: Value = serde_json::from_slice(&tokio::fs::read(&marker).await?)?;
        if ["backend", "storage", "os", "arch"]
            .iter()
            .any(|key| previous[*key] != runtime[*key])
        {
            return Err(Error::conflict("Native directory platform changed."));
        }
    }
    let policy: storage::policy::Policy =
        serde_json::from_value(plan["storage"]["policy"].clone())?;
    policy.validate().map_err(Error::bad)?;
    if !owner.directory.join("directory.json").exists() {
        // Acquire both ownership fences before classifying retained bytes. An
        // active/preparing owner must keep the existing busy/stop semantics.
        require_fresh(&owner.directory, &root, &workload)?;
    }
    private_dir(&workload).await?;
    let saved_limit = owner.directory.join("directory-limit");
    if let Ok(value) = tokio::fs::read_to_string(&saved_limit).await
        && value.parse::<u64>().map_err(Error::internal)? > bytes
    {
        return Err(Error::conflict(
            "Native reservation is smaller than its retained allowance; reconcile the grant before resuming.",
        ));
    }
    quota::prepare(state, &workload, bytes).await?;
    let data = workload.join("data");
    private_dir(&data).await?;
    let view = block::View {
        home: data.join("home"),
        workspace: data.join("workspaces"),
        output: data.join("output"),
        filesystems: vec![data],
    };
    atomic_write(&root.join("view.json"), &serde_json::to_vec(&view)?).await?;
    publish_format(&owner.directory, &root, plan, bytes).await?;
    if !root.join("block-initialized").exists() {
        atomic_write(&root.join("block-seeding"), b"1").await?;
    }
    Ok(Mounted {
        volume: Arc::new(Volume {
            root: workload,
            state: state.to_owned(),
            stop: CancellationToken::new(),
            paused: AtomicBool::new(false),
            pressure: AtomicU8::new(2),
            observed: AtomicU64::new(crate::nodes::boot_ms()),
        }),
        filesystem: Arc::new(AtomicU64::new(bytes)),
        owner,
        _physical: physical,
        aborted: false,
    })
}

pub(super) fn format(plan: &Value) -> Value {
    json!({
        "backend": "native",
        "storage": STORAGE,
        "os": "linux",
        "arch": "x86_64",
        "runtimeId": plan["runtimeId"]
    })
}

/// Caller owns the environment lease, and data publication is already durable.
/// The authoritative marker is synced before secondary records, so interrupted
/// publication is recognized as this format and can reconstruct those records.
pub(super) async fn publish_format(
    directory: &Path,
    root: &Path,
    plan: &Value,
    bytes: u64,
) -> Result<()> {
    let encoded = serde_json::to_vec(&format(plan))?;
    tokio::fs::File::open(root).await?.sync_all().await?;
    tokio::fs::File::open(root.parent().unwrap())
        .await?
        .sync_all()
        .await?;
    atomic_write(&directory.join("directory.json"), &encoded).await?;
    tokio::fs::File::open(directory).await?.sync_all().await?;
    tokio::fs::File::open(directory.parent().unwrap())
        .await?
        .sync_all()
        .await?;
    atomic_write(&directory.join("runtime.json"), &encoded).await?;
    atomic_write(
        &directory.join("directory-limit"),
        bytes.to_string().as_bytes(),
    )
    .await?;
    atomic_write(&root.join("directory.json"), &encoded).await?;
    tokio::fs::File::open(directory).await?.sync_all().await?;
    tokio::fs::File::open(root).await?.sync_all().await?;
    Ok(())
}

async fn migrate(
    state: &Path,
    run: &str,
    plan: &Value,
    stop: &CancellationToken,
    root: &Path,
) -> Result<()> {
    let workload = workspace::workload(state, run)?;
    let staging = workload.join("directory-conversion");
    let marker = root.join("directory-conversion-verified");
    if marker.exists() {
        quota::usage(&workload)?;
        let proof = read_conversion_proof(root).await?;
        let target = if staging.exists() {
            staging.clone()
        } else {
            workload.join("data")
        };
        verify_conversion_target(&target, &proof).await?;
    }
    if !marker.exists() {
        if staging.exists() {
            tokio::fs::remove_dir_all(&staging).await?;
        }
        let namespace = storage::ublk::BackendNamespace::create(state)?;
        let mut legacy = block::mount(state, run, plan, stop, &namespace, None).await?;
        let copied = async {
            let requested = plan["resources"]["diskMiB"].as_u64()
                .ok_or_else(|| Error::bad("Missing native allowance."))? * super::disk::MIB;
            let original = storage::LazyDisk::geometry(&environment::directory(state, run)?.join("lazy"))?;
            if requested < original {
                return Err(Error::conflict("Native conversion cannot shrink its retained allowance."));
            }
            let source = root.join("data");
            let measured = source.clone();
            let required = tokio::task::spawn_blocking(move || super::bases::clone_footprint(&measured))
                .await.map_err(Error::internal)??;
            let fallback = storage::policy::for_node(&plan["storage"]["policy"])?;
            check_copy_headroom(state, required, &fallback, 0).await?;
            quota::prepare(
                state,
                &workload,
                plan["resources"]["diskMiB"]
                    .as_u64()
                    .ok_or_else(|| Error::bad("Missing native allowance."))?
                    * super::disk::MIB,
            )
            .await?;
            private_dir(&staging).await?;
            let capacity = CopyCapacity {
                state: state.to_owned(), workload: workload.clone(), required, fallback, reserved: 0,
            };
            copy_verified_with_capacity(&source, &staging, stop, Some(&capacity)).await?;
            // Verify the final allocated total as well as the kernel-enforced
            // copy. Keep the old journal intact if the grant is insufficient.
            let allowance = plan["resources"]["diskMiB"].as_u64().unwrap() * super::disk::MIB;
            if quota::usage(&workload)? > allowance {
                return Err(Error::conflict("Legacy native data exceeds its migration allowance; increase the grant before conversion."));
            }
            capture_conversion_proof(&staging).await
        }
        .await;
        legacy.close().await?;
        drop(legacy);
        let proof = match copied {
            Ok(proof) => proof,
            Err(error) => {
                // No writer was admitted to this uncommitted private tree.
                // The monitored rsync group has settled before returning here.
                workspace::require_no_mounts(&staging)?;
                if staging.exists() {
                    tokio::fs::remove_dir_all(&staging).await?;
                }
                tokio::fs::File::open(&workload).await?.sync_all().await?;
                return Err(error);
            }
        };
        atomic_write(&marker, &serde_json::to_vec(&proof)?).await?;
    }
    let owner = environment::lock(state, run, DISK_BUSY).await?;
    quota::usage(&workload)?;
    let proof = read_conversion_proof(root).await?;
    let target = if staging.exists() {
        staging.clone()
    } else {
        workload.join("data")
    };
    verify_conversion_target(&target, &proof).await?;
    if staging.exists() {
        let data = workload.join("data");
        if data.exists() {
            tokio::fs::remove_dir(&data).await?;
        }
        tokio::fs::rename(&staging, &data).await?;
        tokio::fs::File::open(&workload).await?.sync_all().await?;
        tokio::fs::File::open(root).await?.sync_all().await?;
    }
    retire_legacy_temporary(root).await?;
    let _replacement = storage::runtime::replacement(&owner.directory).await?;
    atomic_write(&owner.directory.join("directory.json"), &serde_json::to_vec(&json!({
        "backend": "native", "storage": STORAGE, "os": "linux", "arch": "x86_64", "runtimeId": plan["runtimeId"]
    }))?).await?;
    tokio::fs::remove_dir_all(owner.directory.join("lazy")).await?;
    tokio::fs::File::open(&owner.directory)
        .await?
        .sync_all()
        .await?;
    remove_conversion_proof(root).await?;
    cleanup_legacy_directories(state, run).await?;
    Ok(())
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
struct ConversionProof {
    version: u8,
    filesystem: Vec<u8>,
    inode: u64,
    digest: String,
}

async fn read_conversion_proof(root: &Path) -> Result<ConversionProof> {
    let bytes = tokio::fs::read(root.join("directory-conversion-verified")).await?;
    let proof: ConversionProof = serde_json::from_slice(&bytes).map_err(|_| {
        Error::conflict(
            "Native conversion verification metadata is unsupported; both sources were retained.",
        )
    })?;
    if proof.version != 1 || proof.digest.len() != 64 {
        return Err(Error::conflict(
            "Native conversion verification metadata is unsupported.",
        ));
    }
    Ok(proof)
}

async fn remove_conversion_proof(root: &Path) -> Result<()> {
    tokio::fs::remove_file(root.join("directory-conversion-verified")).await?;
    tokio::fs::File::open(root).await?.sync_all().await?;
    Ok(())
}

async fn verify_conversion_target(target: &Path, expected: &ConversionProof) -> Result<()> {
    let current = capture_conversion_proof(target).await?;
    if current != *expected {
        return Err(Error::conflict(
            "Native conversion replacement changed or is missing; legacy source retained.",
        ));
    }
    Ok(())
}

async fn capture_conversion_proof(target: &Path) -> Result<ConversionProof> {
    let target = target.to_owned();
    tokio::task::spawn_blocking(move || -> Result<ConversionProof> {
        use std::os::{
            fd::AsRawFd,
            unix::fs::{MetadataExt, OpenOptionsExt},
        };
        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
            .open(&target)?;
        for name in ["home", "workspaces", "output"] {
            let metadata = std::fs::symlink_metadata(target.join(name))?;
            if !metadata.is_dir() || metadata.file_type().is_symlink() {
                return Err(Error::conflict(
                    "Native conversion replacement is incomplete; legacy source retained.",
                ));
            }
        }
        let pinned = PathBuf::from(format!("/proc/self/fd/{}/.", file.as_raw_fd()));
        let digest = conversion_digest(&pinned)?;
        let metadata = file.metadata()?;
        Ok(ConversionProof {
            version: 1,
            filesystem: storage::policy::filesystem_id(&file)?,
            inode: metadata.ino(),
            digest,
        })
    })
    .await
    .map_err(Error::internal)?
}

fn conversion_digest(root: &Path) -> std::io::Result<String> {
    use std::{
        io::Read,
        os::unix::fs::{MetadataExt, OpenOptionsExt},
    };
    let mut pending = vec![root.to_owned()];
    let mut hash = blake3::Hasher::new();
    while let Some(path) = pending.pop() {
        let metadata = if path == root {
            std::fs::metadata(&path)?
        } else {
            std::fs::symlink_metadata(&path)?
        };
        let relative = path.strip_prefix(root).map_err(std::io::Error::other)?;
        let name = relative.as_os_str().as_encoded_bytes();
        hash.update(&(name.len() as u64).to_le_bytes());
        hash.update(name);
        for value in [
            metadata.mode() as u64,
            metadata.uid() as u64,
            metadata.gid() as u64,
            metadata.nlink(),
            metadata.rdev(),
            metadata.len(),
            metadata.mtime() as u64,
            metadata.mtime_nsec() as u64,
        ] {
            hash.update(&value.to_le_bytes());
        }
        if metadata.is_dir() {
            let mut entries = std::fs::read_dir(&path)?
                .map(|entry| entry.map(|entry| entry.path()))
                .collect::<std::io::Result<Vec<_>>>()?;
            entries.sort();
            pending.extend(entries.into_iter().rev());
        } else if metadata.is_file() {
            let mut file = std::fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
                .open(&path)?;
            let current = file.metadata()?;
            if !current.is_file()
                || (current.dev(), current.ino()) != (metadata.dev(), metadata.ino())
            {
                return Err(std::io::Error::other(
                    "Native conversion entry changed during verification",
                ));
            }
            let mut bytes = [0; 131072];
            loop {
                let count = file.read(&mut bytes)?;
                if count == 0 {
                    break;
                }
                hash.update(&bytes[..count]);
            }
        } else if metadata.file_type().is_symlink() {
            hash.update(std::fs::read_link(&path)?.as_os_str().as_encoded_bytes());
        }
        hash_conversion_attributes(&path, &mut hash)?;
    }
    Ok(hash.finalize().to_hex().to_string())
}

fn hash_conversion_attributes(path: &Path, hash: &mut blake3::Hasher) -> std::io::Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let path = std::ffi::CString::new(path.as_os_str().as_bytes())?;
    let length = unsafe { libc::llistxattr(path.as_ptr(), std::ptr::null_mut(), 0) };
    if length < 0 {
        return Err(std::io::Error::last_os_error());
    }
    if length > 1024 * 1024 {
        return Err(std::io::Error::other(
            "Native conversion attribute list exceeds bound",
        ));
    }
    let mut names = vec![0_u8; length as usize];
    if length != 0
        && unsafe { libc::llistxattr(path.as_ptr(), names.as_mut_ptr().cast(), names.len()) }
            != length
    {
        return Err(std::io::Error::other(
            "Native conversion attributes changed during verification",
        ));
    }
    let mut names = names
        .split(|byte| *byte == 0)
        .filter(|name| !name.is_empty())
        .collect::<Vec<_>>();
    names.sort();
    for name in names {
        let attribute = std::ffi::CString::new(name)?;
        let length =
            unsafe { libc::lgetxattr(path.as_ptr(), attribute.as_ptr(), std::ptr::null_mut(), 0) };
        if length < 0 {
            return Err(std::io::Error::last_os_error());
        }
        if length > 1024 * 1024 {
            return Err(std::io::Error::other(
                "Native conversion attribute exceeds bound",
            ));
        }
        let mut value = vec![0_u8; length as usize];
        if unsafe {
            libc::lgetxattr(
                path.as_ptr(),
                attribute.as_ptr(),
                value.as_mut_ptr().cast(),
                value.len(),
            )
        } != length
        {
            return Err(std::io::Error::other(
                "Native conversion attribute changed during verification",
            ));
        }
        hash.update(&(name.len() as u64).to_le_bytes());
        hash.update(name);
        hash.update(&(value.len() as u64).to_le_bytes());
        hash.update(&value);
    }
    Ok(())
}

pub(super) struct CopyCapacity {
    state: PathBuf,
    workload: PathBuf,
    required: u64,
    fallback: storage::policy::Policy,
    reserved: u64,
}

impl CopyCapacity {
    pub(super) fn new(
        state: &Path,
        workload: &Path,
        required: u64,
        fallback: storage::policy::Policy,
    ) -> Self {
        Self {
            state: state.to_owned(),
            workload: workload.to_owned(),
            required,
            fallback,
            reserved: 0,
        }
    }

    pub(super) fn with_reservation(mut self, reserved: u64) -> Self {
        self.reserved = reserved;
        self
    }

    pub(super) async fn check(&self) -> Result<()> {
        let workload = self.workload.clone();
        let allocated = tokio::task::spawn_blocking(move || quota::usage(&workload))
            .await
            .map_err(Error::internal)??;
        check_copy_headroom(
            &self.state,
            self.required.saturating_sub(allocated),
            &self.fallback,
            self.reserved,
        )
        .await
    }
}

async fn check_copy_headroom(
    state: &Path,
    remaining: u64,
    fallback: &storage::policy::Policy,
    reserved: u64,
) -> Result<()> {
    let state = state.to_owned();
    let fallback = fallback.clone();
    let sample = tokio::task::spawn_blocking(move || -> Result<()> {
        let policy = match std::fs::read(state.join("storage-policy.json")) {
            Ok(bytes) => storage::policy::from_persisted(&bytes)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => fallback,
            Err(error) => return Err(error.into()),
        };
        let (total, free) = storage::policy::physical_space(&workspace::data_root(&state))?;
        let pending = storage::cache::pending_write_bytes();
        let physical = free
            .saturating_sub(policy.reserve(total))
            .saturating_sub(pending);
        let shared = match std::fs::read(state.join(crate::microvm::budget::FILE)) {
            Ok(bytes) => {
                let budget = super::native_budget(&serde_json::from_slice(&bytes)?)?;
                crate::microvm::budget::invalidate_disk_usage(&state)?;
                let used =
                    crate::microvm::budget::committed_disk_bytes(&state)?.saturating_sub(reserved);
                (budget.limits.disk_mi_b * super::disk::MIB)
                    .saturating_sub(used)
                    .saturating_sub(pending)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => u64::MAX,
            Err(error) => return Err(error.into()),
        };
        if remaining.saturating_add(super::disk::MIB) > physical.min(shared) {
            return Err(Error::new(
                507,
                "Native conversion needs replacement headroom within the shared budget and data-pool reserve; source retained.",
            ));
        }
        Ok(())
    });
    tokio::time::timeout(std::time::Duration::from_secs(5), sample)
        .await
        .map_err(|_| {
            Error::unavailable(
                "Native conversion capacity observations are stale; source retained.",
            )
        })?
        .map_err(Error::internal)?
}

pub(super) async fn copy_verified(
    source: &Path,
    target: &Path,
    stop: &CancellationToken,
    capacity: &CopyCapacity,
) -> Result<()> {
    copy_verified_with_capacity(source, target, stop, Some(capacity)).await
}

#[cfg(test)]
pub(super) async fn copy_unbudgeted(
    source: &Path,
    target: &Path,
    stop: &CancellationToken,
) -> Result<()> {
    copy_verified_with_capacity(source, target, stop, None).await
}

async fn copy_verified_with_capacity(
    source: &Path,
    target: &Path,
    stop: &CancellationToken,
    capacity: Option<&CopyCapacity>,
) -> Result<()> {
    let mut copy = tokio::process::Command::new("rsync");
    // The exec child retains ownership/xattr privileges but cannot bypass quotas.
    unsafe {
        copy.pre_exec(|| {
            if libc::geteuid() == 0
                && (libc::prctl(libc::PR_CAPBSET_DROP, 24, 0, 0, 0) != 0
                    || libc::prctl(libc::PR_CAP_AMBIENT, libc::PR_CAP_AMBIENT_LOWER, 24, 0, 0) != 0)
            {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    // No per-file fsync: on a loop-backed pool each one costs a host journal
    // commit. One syncfs flushes the copy, and every caller publishes it only
    // after this returns, through writes that fail if ext4 shut down meanwhile.
    // Its descriptor is opened before the copy so writeback errors during the
    // copy are reported to this barrier.
    let barrier = tokio::fs::File::open(target).await?.into_std().await;
    copy.args(["-aHAX", "--numeric-ids", "--sparse", "--"])
        .arg(format!("{}/", source.display()))
        .arg(target);
    rsync_status(copy, stop, capacity).await?;
    verify_copy_with_capacity(source, target, stop, capacity).await?;
    block::sync_opened(barrier).await.map_err(Into::into)
}

pub(super) async fn verify_copy(
    source: &Path,
    target: &Path,
    stop: &CancellationToken,
    capacity: &CopyCapacity,
) -> Result<()> {
    verify_copy_with_capacity(source, target, stop, Some(capacity)).await
}

async fn verify_copy_with_capacity(
    source: &Path,
    target: &Path,
    stop: &CancellationToken,
    capacity: Option<&CopyCapacity>,
) -> Result<()> {
    let mut verify = tokio::process::Command::new("rsync");
    verify
        .args([
            "-aHAXnc",
            "--numeric-ids",
            "--delete",
            "--itemize-changes",
            "--",
        ])
        .arg(format!("{}/", source.display()))
        .arg(target);
    rsync_status(verify, stop, capacity).await
}

/// rsync is a local trusted program; both commands are quiet on success. Keep
/// its leader unreaped until stdout closes, so cancellation can signal only its
/// original process group while that leader's PID cannot be recycled.
async fn rsync_status(
    mut command: tokio::process::Command,
    stop: &CancellationToken,
    capacity: Option<&CopyCapacity>,
) -> Result<()> {
    use tokio::io::AsyncReadExt;
    if let Some(capacity) = capacity {
        capacity.check().await?;
    }
    if stop.is_cancelled() {
        return Err(Error::unavailable("Native copy stopped; source retained."));
    }
    let mut child = command
        .process_group(0)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()?;
    let mut output = child
        .stdout
        .take()
        .ok_or_else(|| Error::unavailable("Native copy output was not acquired."))?;
    let mut one = [0_u8; 1];
    let mut closed = false;
    let mut interval = tokio::time::interval(std::time::Duration::from_millis(250));
    let result = loop {
        tokio::select! {
            () = stop.cancelled() => {
                break Err(Error::unavailable("Native copy stopped; source retained."));
            },
            result = output.read(&mut one), if !closed => {
                match result {
                    Ok(0) => closed = true,
                    Ok(_) => {
                        break Err(Error::conflict("Native copy verification reported differences; source retained."));
                    },
                    Err(error) => break Err(error.into()),
                }
            },
            status = child.wait(), if closed => {
                break status.map_err(Into::into);
            },
            _ = interval.tick(), if capacity.is_some() => {
                if let Some(capacity) = capacity
                    && let Err(error) = capacity.check().await
                {
                    break Err(error);
                }
            }
        }
    };
    let status = match result {
        Ok(status) => status,
        Err(error) => {
            // Child has not been reaped on these branches. Its numeric PID
            // still pins the process-group identity established before exec.
            if let Some(pid) = child.id()
                && unsafe { libc::kill(-(pid as i32), libc::SIGKILL) } != 0
                && std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
            {
                tracing::warn!(
                    "Native copy group signalling failed; ownership retained while waiting"
                );
            }
            // Keep the leader unreaped while its group identity is needed to
            // inventory children, including a receiver blocked in filesystem IO.
            if let Some(pid) = child.id() {
                let mut warning = tokio::time::Instant::now();
                loop {
                    let observation = copy_group_remaining(pid);
                    if matches!(&observation, Ok(0)) {
                        break;
                    }
                    if tokio::time::Instant::now() >= warning {
                        match observation {
                            Ok(remaining) => tracing::warn!(
                                remaining,
                                "Native copy descendants remain unsettled; ownership retained"
                            ),
                            Err(error) => {
                                tracing::warn!(%error, "Native copy descendant inventory is uncertain; ownership retained");
                            }
                        }
                        warning = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                }
            }
            loop {
                match tokio::time::timeout(std::time::Duration::from_secs(5), child.wait()).await {
                    Ok(result) => {
                        result?;
                        break;
                    }
                    Err(_) => tracing::warn!(
                        "Native copy retirement is pending; source and ownership retained"
                    ),
                }
            }
            return Err(error);
        }
    };
    if !status.success() {
        return Err(Error::unavailable(format!(
            "Native copy failed ({status}); source retained."
        )));
    }
    if let Some(capacity) = capacity {
        capacity.check().await?;
    }
    Ok(())
}

fn copy_group_remaining(leader: u32) -> std::io::Result<usize> {
    let mut remaining = 0;
    for entry in std::fs::read_dir("/proc")? {
        let entry = entry?;
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|value| value.parse::<u32>().ok())
        else {
            continue;
        };
        if pid == leader {
            continue;
        }
        let group = unsafe { libc::getpgid(pid as i32) };
        if group < 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::ESRCH) {
                continue;
            }
            return Err(error);
        }
        if group != leader as i32 {
            continue;
        }
        let stat = match std::fs::read_to_string(entry.path().join("stat")) {
            Ok(value) => value,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        let tail = stat
            .rsplit_once(") ")
            .ok_or_else(|| std::io::Error::other("Invalid copy process inventory"))?
            .1;
        let mut fields = tail.split_whitespace();
        let state = fields.next();
        let _parent = fields.next();
        let group = fields.next().and_then(|value| value.parse::<u32>().ok());
        if group == Some(leader) && !matches!(state, Some("Z" | "X")) {
            remaining += 1;
        }
    }
    Ok(remaining)
}

pub async fn status(state: &Path, run: &str) -> Result<Value> {
    let directory = environment::directory(state, run)?;
    if !directory.join("directory.json").exists() {
        if storage::bootstrap::resize_source(&directory)?.is_some() {
            return Ok(json!({ "mode": "native", "availability": "available",
                "storage": "legacy-resize-source", "paused": false,
                "localBytes": crate::microvm::budget::allocated(&directory)? }));
        }
        return block::status(state, run).await;
    }
    let root = workspace::directory(state, run)?;
    let workload = workspace::workload(state, run)?;
    if root.join("block-initialized").exists() && require_retained_tree(&workload).is_err() {
        // Keep the established availability code for older clients. It now
        // identifies missing retained native data, not a transport requirement.
        return Ok(
            json!({ "mode": "native", "storage": STORAGE, "availability": "unavailable", "reason": super::MISSING_NATIVE_JOURNAL_CODE }),
        );
    }
    let used = quota::usage(&workload)?;
    Ok(
        json!({ "mode": "native", "storage": STORAGE, "availability": "available", "localBytes": used, "activeLocalBytes": used, "paused": false }),
    )
}

pub async fn recover_completed(state: &Path, run: &str, attempt: &str) -> Result<()> {
    if environment::directory(state, run)?
        .join("directory.json")
        .exists()
    {
        let root = workspace::directory(state, run)?;
        let workload = workspace::workload(state, run)?;
        if workload.exists() && !root.join("quota-retirement.json").exists() {
            super::systemd::directory_consumers::stop(&workload).await?;
        }
        if workload.exists() {
            block::sync(&workload).await?;
        }
        // Recovery must settle the same temporary resources as normal close
        // before startup acknowledges this execution and removes its retry flag.
        retire_temporary(&workload, &root).await?;
        atomic_write(&root.join("attempt"), attempt.as_bytes()).await?;
        tokio::fs::File::open(&root).await?.sync_all().await?;
        return Ok(());
    }
    block::recover_completed(state, run, attempt).await
}

pub async fn action(state: &Path, run: &str, delete: bool) -> Result<Vec<String>> {
    super::cleanup::check(state, run)?;
    let lease = environment::lock(state, run, DISK_BUSY).await?;
    if delete {
        let root = workspace::directory(state, run)?;
        let workload = workspace::workload(state, run)?;
        if workload.exists()
            && root.join("quota-owner.json").exists()
            && !root.join("quota-retirement.json").exists()
        {
            super::systemd::directory_consumers::stop(&workload).await?;
        }
        quota::retire(&root).await?;
    }
    let grants = block::action_locked(&lease.directory, delete).await?;
    if delete {
        cleanup_legacy_directories(state, run).await?;
    }
    Ok(grants)
}

pub async fn usage(state: &Path, run: &str) -> Result<(u64, u64)> {
    let workload = workspace::workload(state, run)?;
    tokio::task::spawn_blocking(move || -> Result<_> {
        let temporary = workload.join("tmp");
        let temporary = if temporary.exists() {
            workspace::measure(&temporary, true)?
        } else {
            0
        };
        let total = quota::usage(&workload)?;
        Ok((total.saturating_sub(temporary), temporary))
    })
    .await
    .map_err(Error::internal)?
}

/// The workload quota includes managed temporary files, while control metadata
/// remains writable after ENOSPC/EDQUOT. Legacy fixtures retain their old path.
pub async fn temporary(state: &Path, run: &str) -> Result<PathBuf> {
    let root = workspace::directory(state, run)?;
    let workload = workspace::workload(state, run)?;
    Ok(if workload.is_dir() {
        workload.join("tmp")
    } else {
        root.join("tmp")
    })
}

async fn cleanup_legacy_directories(state: &Path, run: &str) -> Result<()> {
    let base = state.join("native-block");
    match tokio::fs::symlink_metadata(&base).await {
        Ok(metadata) if !metadata.is_dir() || metadata.file_type().is_symlink() => {
            return Err(Error::conflict(
                "Legacy attachment namespace is not an owned directory.",
            ));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    }
    let directory = base.join(run);
    let metadata = match tokio::fs::symlink_metadata(&directory).await {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(Error::conflict(
            "Legacy attachment directory is not an owned directory.",
        ));
    }
    for path in [directory.join("root"), directory] {
        match tokio::fs::symlink_metadata(&path).await {
            Ok(metadata) if !metadata.is_dir() || metadata.file_type().is_symlink() => {
                return Err(Error::conflict(
                    "Unrecognized legacy attachment alias; cleanup retained for recovery.",
                ));
            }
            Ok(_) => tokio::fs::remove_dir(path).await?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

async fn retire_temporary(workload: &Path, control: &Path) -> Result<()> {
    remove_temporary(&workload.join("tmp")).await?;
    // Make the data-filesystem retirement durable before a control-filesystem
    // exit record can suppress subsequent recovery after another crash.
    if workload.exists() {
        tokio::fs::File::open(workload).await?.sync_all().await?;
    }
    retire_legacy_temporary(control).await?;
    remove_temporary(&control.join("docker-runtime")).await?;
    tokio::fs::File::open(control).await?.sync_all().await?;
    Ok(())
}

async fn retire_legacy_temporary(control: &Path) -> Result<()> {
    let temporary = control.join("tmp");
    match tokio::fs::symlink_metadata(&temporary).await {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
            super::systemd::directory_consumers::require_unused_legacy_temporary(&temporary)
                .await?;
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    remove_temporary(&temporary).await?;
    tokio::fs::File::open(control).await?.sync_all().await?;
    Ok(())
}

async fn remove_temporary(path: &Path) -> Result<()> {
    workspace::require_no_mounts(path)?;
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

pub(super) fn require_fresh(saved: &Path, control: &Path, workload: &Path) -> Result<()> {
    let entries = match std::fs::read_dir(saved) {
        Ok(entries) => Some(entries),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    for entry in entries.into_iter().flatten() {
        let entry = entry?;
        let name = entry.file_name();
        let temporary = name
            .to_str()
            .and_then(|name| name.strip_prefix('.'))
            .and_then(|name| name.strip_suffix(".tmp"))
            .is_some_and(|name| crate::validation::uuid(name).is_ok())
            && entry.file_type()?.is_file();
        if !environment::retained(&name) && !temporary {
            return Err(Error::conflict(
                "Unrecognized retained native storage; recover its source before creating a directory environment.",
            ));
        }
    }
    for name in [
        "data",
        "runtime.json",
        "initialized",
        "block-initialized",
        "block-seeding",
        "block-restore.pending.json",
    ] {
        match control.join(name).symlink_metadata() {
            Ok(_) => {
                return Err(Error::conflict(
                    "Native retained data has no recognized source format; recover or expire it explicitly.",
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    // A crash before the first format marker can leave an assigned quota and
    // the newly created empty data directory. It cannot have admitted a writer.
    let present = match std::fs::symlink_metadata(workload) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => true,
        Ok(_) => {
            return Err(Error::conflict(
                "Native workload is not an owned directory.",
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => return Err(error.into()),
    };
    if present {
        for entry in std::fs::read_dir(workload)? {
            let entry = entry?;
            if entry.file_name() != "data"
                || !entry.file_type()?.is_dir()
                || std::fs::read_dir(entry.path())?.next().is_some()
            {
                return Err(Error::conflict(
                    "Unrecognized native directory contents; retained files were preserved.",
                ));
            }
        }
    }
    Ok(())
}

pub(super) fn require_retained_tree(workload: &Path) -> Result<()> {
    for name in ["", "data", "data/home", "data/workspaces", "data/output"] {
        let path = workload.join(name);
        match std::fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
            Ok(_) => {
                return Err(Error::conflict(
                    "Retained native data was replaced by an unsupported file or alias; recover it before resuming.",
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(Error::conflict(
                    "Retained native data is missing; recover it or explicitly expire the environment.",
                ));
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn observe(volume: std::sync::Weak<Volume>) {
    tokio::spawn(async move {
        loop {
            let Some(current) = volume.upgrade() else {
                return;
            };
            let state = current.state.clone();
            let stop = current.stop.clone();
            drop(current);
            let sample = tokio::task::spawn_blocking(move || -> Result<bool> {
                let policy = match std::fs::read(state.join("storage-policy.json")) {
                    Ok(bytes) => storage::policy::from_persisted(&bytes)?,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        storage::policy::Policy::default()
                    }
                    Err(error) => return Err(error.into()),
                };
                // A project directory's statfs is quota-limited by ext4.
                // Node reserve pressure must use the unrestricted data pool;
                // the independent capacity monitor handles task pressure.
                let (total, free) = storage::policy::space(&workspace::data_root(&state))?;
                Ok(free <= policy.reserve(total))
            });
            let result = tokio::select! {
                () = stop.cancelled() => return,
                result = sample => result,
            };
            if stop.is_cancelled() {
                return;
            }
            if let Some(current) = volume.upgrade() {
                let value = match result {
                    Ok(Ok(false)) => 0,
                    Ok(Ok(true)) => 1,
                    _ => 2,
                };
                current.pressure.store(value, Ordering::Release);
                current
                    .observed
                    .store(crate::nodes::boot_ms(), Ordering::Release);
            }
            tokio::select! {
                () = stop.cancelled() => return,
                () = tokio::time::sleep(std::time::Duration::from_secs(1)) => {}
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    async fn copy_budget(state: &Path, disk_mib: u64) {
        atomic_write(
            &state.join(crate::microvm::budget::FILE),
            &serde_json::to_vec(&json!({
                "nativeModeProtocol": 1,
                "slots": 1,
                "limits": {
                    "cpu": 1,
                    "memoryMiB": 128,
                    "diskMiB": disk_mib
                }
            }))
            .unwrap(),
        )
        .await
        .unwrap();
    }

    fn copy_space_fixture(state: &Path, data: &Path) -> storage::policy::Policy {
        use std::io::Write;
        std::fs::create_dir_all(workspace::data_root(state)).unwrap();
        std::fs::write(
            state.join("native-accounting.json"),
            serde_json::to_vec(&json!({ "data": data })).unwrap(),
        )
        .unwrap();
        let policy =
            storage::policy::for_node(&json!({ "reserveMiB": 64, "reservePercent": 1 })).unwrap();
        std::fs::write(
            state.join("storage-policy.json"),
            serde_json::to_vec(&policy).unwrap(),
        )
        .unwrap();
        let mut source = std::fs::File::create(data.join("retained-source")).unwrap();
        source.write_all(b"retained source canary").unwrap();
        source.set_len(96 * super::super::disk::MIB).unwrap();
        std::fs::File::create(data.join("other-work"))
            .unwrap()
            .set_len(24 * super::super::disk::MIB)
            .unwrap();
        policy
    }

    #[tokio::test]
    async fn replacement_admission_charges_retained_work_and_refreshes_budget_and_physical_reserve()
    {
        const CHILD: &str = "CAIRN_TEST_NATIVE_COPY_HEADROOM";
        if std::env::var_os(CHILD).is_none() {
            let status = tokio::process::Command::new(std::env::current_exe().unwrap())
                .env_clear().env(CHILD, "1").env("TMPDIR", std::env::temp_dir())
                .args(["--exact", "native::directory::tests::replacement_admission_charges_retained_work_and_refreshes_budget_and_physical_reserve", "--nocapture"])
                .status().await.unwrap();
            assert!(status.success());
            return;
        }
        let state = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        let policy = copy_space_fixture(state.path(), data.path());
        let required = 96 * super::super::disk::MIB;
        copy_budget(state.path(), 128).await;
        assert_eq!(
            check_copy_headroom(state.path(), required, &policy, 0)
                .await
                .unwrap_err()
                .status,
            507
        );
        copy_budget(state.path(), 256).await;
        check_copy_headroom(state.path(), required, &policy, 0)
            .await
            .unwrap();
        std::fs::OpenOptions::new()
            .write(true)
            .open(data.path().join("other-work"))
            .unwrap()
            .set_len(128 * super::super::disk::MIB)
            .unwrap();
        assert!(
            check_copy_headroom(state.path(), required, &policy, 0)
                .await
                .is_err()
        );
        std::fs::OpenOptions::new()
            .write(true)
            .open(data.path().join("other-work"))
            .unwrap()
            .set_len(24 * super::super::disk::MIB)
            .unwrap();
        let (total, _) =
            storage::policy::physical_space(&workspace::data_root(state.path())).unwrap();
        let mut reserve = policy.clone();
        reserve.reserve_mi_b = total / super::super::disk::MIB + 1;
        std::fs::write(
            state.path().join("storage-policy.json"),
            serde_json::to_vec(&reserve).unwrap(),
        )
        .unwrap();
        assert!(
            check_copy_headroom(state.path(), 0, &policy, 0)
                .await
                .is_err()
        );
        assert_eq!(
            std::fs::metadata(data.path().join("retained-source"))
                .unwrap()
                .len(),
            required
        );
        let mut source = std::fs::File::open(data.path().join("retained-source")).unwrap();
        let mut canary = [0; 22];
        source.read_exact(&mut canary).unwrap();
        assert_eq!(&canary, b"retained source canary");
    }

    fn sleeping_copy(marker: &Path) -> tokio::process::Command {
        let mut command = tokio::process::Command::new("/bin/sh");
        command
            .args([
                "-c",
                r#"sleep 30 & printf '%s %s' "$$" "$!" > "$1.partial"; mv "$1.partial" "$1"; wait"#,
                "native-copy-test",
            ])
            .arg(marker);
        command
    }

    async fn copy_process_handles(marker: &Path) -> Vec<std::fs::File> {
        use std::os::fd::FromRawFd;
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if let Ok(value) = tokio::fs::read_to_string(marker).await {
                    let pids: Vec<u32> = value
                        .split_whitespace()
                        .map(|value| value.parse().unwrap())
                        .collect();
                    if pids.len() == 2 {
                        return pids
                            .into_iter()
                            .map(|pid| {
                                let fd =
                                    unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) } as i32;
                                assert!(fd >= 0);
                                unsafe { std::fs::File::from_raw_fd(fd) }
                            })
                            .collect();
                    }
                }
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap()
    }

    fn copy_processes_exited(handles: &[std::fs::File]) {
        use std::os::fd::AsRawFd;
        for file in handles {
            let mut poll = libc::pollfd {
                fd: file.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            assert_eq!(unsafe { libc::poll(&mut poll, 1, 2000) }, 1);
        }
    }

    #[tokio::test]
    async fn cancellation_settles_only_the_owned_copy_process_group() {
        let fixture = tempfile::tempdir().unwrap();
        let marker = fixture.path().join("copy-processes");
        let stop = CancellationToken::new();
        let operation = tokio::spawn({
            let stop = stop.clone();
            let command = sleeping_copy(&marker);
            async move { rsync_status(command, &stop, None).await }
        });
        let handles = copy_process_handles(&marker).await;
        stop.cancel();
        assert!(
            tokio::time::timeout(std::time::Duration::from_secs(5), operation)
                .await
                .unwrap()
                .unwrap()
                .is_err()
        );
        copy_processes_exited(&handles);
    }

    #[tokio::test]
    #[ignore = "requires scripts/qualify-native-quota.mjs on its dedicated disposable filesystem"]
    async fn kernel_migration_copy_headroom() {
        assert_eq!(
            std::env::var("CAIRN_NATIVE_DISPOSABLE_HOST").as_deref(),
            Ok("1")
        );
        let state = PathBuf::from(std::env::var_os("CAIRN_NATIVE_QUOTA_TEST_STATE").unwrap());
        quota::require(&state).unwrap();
        let names = [
            crate::microvm::budget::FILE,
            "native-accounting.json",
            "storage-policy.json",
        ];
        let previous: Vec<_> = names
            .iter()
            .map(|name| std::fs::read(state.join(name)).ok())
            .collect();
        let data = tempfile::tempdir().unwrap();
        let policy = copy_space_fixture(&state, data.path());
        copy_budget(&state, 512).await;
        let run = crate::config::id();
        let root = workspace::directory(&state, &run).unwrap();
        private_dir(&root).await.unwrap();
        let workload = workspace::workload(&state, &run).unwrap();
        private_dir(&workload).await.unwrap();
        quota::prepare(&state, &workload, 128 * super::super::disk::MIB)
            .await
            .unwrap();
        let capacity = CopyCapacity {
            state: state.clone(),
            workload: workload.clone(),
            required: 96 * super::super::disk::MIB,
            fallback: policy,
            reserved: 0,
        };
        capacity.check().await.unwrap();
        let marker = data.path().join("copy-processes");
        let operation = tokio::spawn({
            let command = sleeping_copy(&marker);
            async move { rsync_status(command, &CancellationToken::new(), Some(&capacity)).await }
        });
        let handles = copy_process_handles(&marker).await;
        copy_budget(&state, 128).await;
        assert_eq!(
            tokio::time::timeout(std::time::Duration::from_secs(5), operation)
                .await
                .unwrap()
                .unwrap()
                .unwrap_err()
                .status,
            507
        );
        copy_processes_exited(&handles);
        assert!(data.path().join("retained-source").is_file());
        quota::retire(&root).await.unwrap();
        for (name, value) in names.into_iter().zip(previous) {
            if let Some(bytes) = value {
                std::fs::write(state.join(name), bytes).unwrap();
            } else {
                std::fs::remove_file(state.join(name)).unwrap();
            }
        }
        crate::microvm::budget::invalidate_disk_usage(&state).unwrap();
    }

    #[tokio::test]
    async fn conversion_proof_survives_rename_and_refuses_changed_or_missing_replacements() {
        let fixture = tempfile::tempdir().unwrap();
        let staging = fixture.path().join("staging");
        for name in ["home", "workspaces", "output"] {
            private_dir(&staging.join(name)).await.unwrap();
        }
        tokio::fs::write(staging.join("workspaces/saved"), b"original user work")
            .await
            .unwrap();
        std::os::unix::fs::symlink("saved", staging.join("workspaces/alias")).unwrap();
        let fifo = std::ffi::CString::new(
            staging
                .join("workspaces/pipe")
                .as_os_str()
                .as_encoded_bytes(),
        )
        .unwrap();
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
        let proof = capture_conversion_proof(&staging).await.unwrap();
        verify_conversion_target(&staging, &proof).await.unwrap();
        let data = fixture.path().join("data");
        tokio::fs::rename(&staging, &data).await.unwrap();
        verify_conversion_target(&data, &proof).await.unwrap();
        let file =
            std::ffi::CString::new(data.join("workspaces/saved").as_os_str().as_encoded_bytes())
                .unwrap();
        let attribute = c"user.cairn-conversion-test";
        assert_eq!(
            unsafe {
                libc::lsetxattr(
                    file.as_ptr(),
                    attribute.as_ptr(),
                    b"changed".as_ptr().cast(),
                    7,
                    0,
                )
            },
            0
        );
        assert!(verify_conversion_target(&data, &proof).await.is_err());
        assert_eq!(
            unsafe { libc::lremovexattr(file.as_ptr(), attribute.as_ptr()) },
            0
        );
        verify_conversion_target(&data, &proof).await.unwrap();
        tokio::fs::write(data.join("workspaces/saved"), b"changed user work")
            .await
            .unwrap();
        assert!(verify_conversion_target(&data, &proof).await.is_err());
        tokio::fs::remove_dir_all(&data).await.unwrap();
        assert!(verify_conversion_target(&data, &proof).await.is_err());
    }

    #[tokio::test]
    async fn missing_verified_conversion_output_never_erases_its_legacy_source() {
        let state = tempfile::tempdir().unwrap();
        let run = crate::config::id();
        let root = workspace::directory(state.path(), &run).unwrap();
        let source = environment::directory(state.path(), &run).unwrap();
        private_dir(&root).await.unwrap();
        private_dir(&source.join("lazy")).await.unwrap();
        let retained = source.join("lazy/journal-v2.sqlite");
        tokio::fs::write(&retained, b"retained legacy source bytes")
            .await
            .unwrap();
        atomic_write(&root.join("directory-conversion-verified"), b"1")
            .await
            .unwrap();
        let plan = json!({ "runtimeId": "fixture", "resources": { "diskMiB": 128 },
            "storage": { "owner": run, "policy": { "reserveMiB": 64, "reservePercent": 1 } } });
        for _ in 0..2 {
            assert!(
                mount(state.path(), &run, &plan, &CancellationToken::new())
                    .await
                    .is_err()
            );
            assert_eq!(
                tokio::fs::read(&retained).await.unwrap(),
                b"retained legacy source bytes"
            );
            assert!(!source.join("directory.json").exists());
        }
    }

    #[tokio::test]
    #[ignore = "requires scripts/qualify-native-quota.mjs on its dedicated disposable filesystem"]
    async fn kernel_missing_conversion_replacement_preserves_original_journal() {
        assert_eq!(
            std::env::var("CAIRN_NATIVE_DISPOSABLE_HOST").as_deref(),
            Ok("1")
        );
        let state = PathBuf::from(std::env::var_os("CAIRN_NATIVE_QUOTA_TEST_STATE").unwrap());
        quota::require(&state).unwrap();
        let run = crate::config::id();
        let root = workspace::directory(&state, &run).unwrap();
        let workload = workspace::workload(&state, &run).unwrap();
        let source = environment::directory(&state, &run).unwrap();
        private_dir(&root).await.unwrap();
        private_dir(&workload).await.unwrap();
        let context = json!({ "owner": run, "policy": { "memoryCacheMiB": 4, "reserveMiB": 64, "reservePercent": 1 } });
        let stop = CancellationToken::new();
        storage::bootstrap::prepare_native(&source, 128 * super::super::disk::MIB, &context, &stop)
            .await
            .unwrap();
        quota::prepare(&state, &workload, 128 * super::super::disk::MIB)
            .await
            .unwrap();
        let staging = workload.join("directory-conversion");
        for name in ["home", "workspaces", "output"] {
            private_dir(&staging.join(name)).await.unwrap();
        }
        tokio::fs::write(staging.join("workspaces/saved"), b"user work")
            .await
            .unwrap();
        let proof = capture_conversion_proof(&staging).await.unwrap();
        atomic_write(
            &root.join("directory-conversion-verified"),
            &serde_json::to_vec(&proof).unwrap(),
        )
        .await
        .unwrap();
        tokio::fs::remove_dir_all(staging).await.unwrap();
        let plan =
            json!({ "runtimeId": "fixture", "resources": { "diskMiB": 128 }, "storage": context });
        assert!(migrate(&state, &run, &plan, &stop, &root).await.is_err());
        assert!(source.join("lazy/journal-v2.sqlite").exists());
        assert!(!source.join("directory.json").exists());
        quota::retire(&root).await.unwrap();
        let lease = environment::lock(&state, &run, DISK_BUSY).await.unwrap();
        block::action_locked(&lease.directory, true).await.unwrap();
    }

    #[tokio::test]
    async fn unsupported_native_sources_without_initialization_hints_are_preserved() {
        for name in ["lazy/payload-1", "lazy/journal-v1.sqlite", "runtime.json"] {
            let state = tempfile::tempdir().unwrap();
            let run = crate::config::id();
            let saved = environment::directory(state.path(), &run).unwrap();
            let source = saved.join(name);
            private_dir(source.parent().unwrap()).await.unwrap();
            tokio::fs::write(&source, b"retained work from an unsupported format")
                .await
                .unwrap();
            let plan = json!({ "runtimeId": "fixture", "resources": { "diskMiB": 64 },
                "storage": { "owner": run, "policy": { "memoryCacheMiB": 4, "reserveMiB": 64, "reservePercent": 1 } } });
            for _ in 0..2 {
                assert!(
                    mount(state.path(), &run, &plan, &CancellationToken::new())
                        .await
                        .is_err()
                );
                assert_eq!(
                    tokio::fs::read(&source).await.unwrap(),
                    b"retained work from an unsupported format"
                );
                assert!(!saved.join("directory.json").exists());
                assert!(!workspace::workload(state.path(), &run).unwrap().exists());
            }
        }
    }

    #[test]
    fn unrecognized_workload_contents_and_aliases_are_not_fresh() {
        let state = tempfile::tempdir().unwrap();
        let run = crate::config::id();
        let saved = environment::directory(state.path(), &run).unwrap();
        let control = workspace::directory(state.path(), &run).unwrap();
        let workload = workspace::workload(state.path(), &run).unwrap();
        std::fs::create_dir_all(workload.join("data/workspaces")).unwrap();
        std::fs::write(workload.join("data/workspaces/saved"), b"retained work").unwrap();
        assert!(require_fresh(&saved, &control, &workload).is_err());
        assert_eq!(
            std::fs::read(workload.join("data/workspaces/saved")).unwrap(),
            b"retained work"
        );
        let alias = workload.with_file_name(crate::config::id());
        std::os::unix::fs::symlink(&workload, &alias).unwrap();
        assert!(require_fresh(&saved, &control, &alias).is_err());
    }

    #[tokio::test]
    async fn missing_retained_data_reports_unavailable_without_recreating_files() {
        let state = tempfile::tempdir().unwrap();
        let run = crate::config::id();
        let root = workspace::directory(state.path(), &run).unwrap();
        private_dir(&root).await.unwrap();
        atomic_write(&root.join("block-initialized"), b"1")
            .await
            .unwrap();
        let owner = environment::ownership(state.path(), &run, "busy")
            .await
            .unwrap();
        atomic_write(&owner.directory.join("directory.json"), b"{}")
            .await
            .unwrap();
        drop(owner);
        let status = status(state.path(), &run).await.unwrap();
        assert_eq!(status["availability"], "unavailable");
        assert!(!workspace::workload(state.path(), &run).unwrap().exists());
    }

    #[test]
    fn missing_or_aliased_retained_tree_is_refused_without_recreation() {
        let root = tempfile::tempdir().unwrap();
        let workload = root.path().join("workload");
        std::fs::create_dir_all(workload.join("data/home")).unwrap();
        std::fs::create_dir_all(workload.join("data/workspaces")).unwrap();
        assert!(require_retained_tree(&workload).is_err());
        assert!(!workload.join("data/output").exists());
        std::os::unix::fs::symlink(workload.join("data/home"), workload.join("data/output"))
            .unwrap();
        assert!(require_retained_tree(&workload).is_err());
        std::fs::remove_file(workload.join("data/output")).unwrap();
        std::fs::create_dir(workload.join("data/output")).unwrap();
        require_retained_tree(&workload).unwrap();
    }
}
