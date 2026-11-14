//! Retained native directories. Only legacy conversion opens a journal backend.
use super::{atomic_write, block, quota, workspace};
use crate::{
    error::{Error, Result},
    skills::private_dir,
    storage::{self, environment},
};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering},
    },
};
use tokio_util::sync::CancellationToken;

pub(crate) use block::{Completion, DISK_BUSY, free_bytes, initialize, recover_host_mounts, view};

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
}

impl Handle {
    pub fn new(mount: Mounted) -> Self {
        observe(Arc::downgrade(&mount.volume));
        Self {
            volume: mount.volume.clone(),
            filesystem: mount.filesystem.clone(),
            mount: tokio::sync::Mutex::new(mount),
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
    Ok(directory.join("directory.json").exists() || storage::runtime::exists(&directory))
}

pub async fn mount(
    state: &Path,
    run: &str,
    plan: &Value,
    stop: &CancellationToken,
) -> Result<Mounted> {
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
        // A durable directory marker wins over a partially removed legacy
        // journal. Never reopen or copy that old format after the switch.
        let _replacement = storage::runtime::replacement(&owner.directory).await?;
        tokio::fs::remove_dir_all(owner.directory.join("lazy")).await?;
        tokio::fs::File::open(&owner.directory)
            .await?
            .sync_all()
            .await?;
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
    if !marker.exists() {
        if staging.exists() {
            tokio::fs::remove_dir_all(&staging).await?;
        }
        let namespace = storage::ublk::BackendNamespace::create(state)?;
        let mut legacy = block::mount(state, run, plan, stop, &namespace, None).await?;
        let copied = async {
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
            copy_verified(&root.join("data"), &staging).await?;
            // Verify the final allocated total as well as the kernel-enforced
            // copy. Keep the old journal intact if the grant is insufficient.
            let allowance = plan["resources"]["diskMiB"].as_u64().unwrap() * super::disk::MIB;
            if quota::usage(&workload)? > allowance {
                return Err(Error::conflict("Legacy native data exceeds its migration allowance; increase the grant before conversion."));
            }
            Ok(())
        }
        .await;
        legacy.close().await?;
        drop(legacy);
        copied?;
        atomic_write(&marker, b"1").await?;
    }
    let owner = environment::lock(state, run, DISK_BUSY).await?;
    if staging.exists() {
        let data = workload.join("data");
        if data.exists() {
            tokio::fs::remove_dir(&data).await?;
        }
        tokio::fs::rename(&staging, &data).await?;
        tokio::fs::File::open(&workload).await?.sync_all().await?;
        tokio::fs::File::open(root).await?.sync_all().await?;
    }
    let _replacement = storage::runtime::replacement(&owner.directory).await?;
    atomic_write(&owner.directory.join("directory.json"), &serde_json::to_vec(&json!({
        "backend": "native", "storage": STORAGE, "os": "linux", "arch": "x86_64", "runtimeId": plan["runtimeId"]
    }))?).await?;
    tokio::fs::remove_dir_all(owner.directory.join("lazy")).await?;
    tokio::fs::File::open(&owner.directory)
        .await?
        .sync_all()
        .await?;
    tokio::fs::remove_file(marker).await?;
    cleanup_legacy_directories(state, run).await?;
    Ok(())
}

pub(super) async fn copy_verified(source: &Path, target: &Path) -> Result<()> {
    let source = format!("{}/", source.display());
    let mut copy = tokio::process::Command::new("rsync");
    // Root's CAP_SYS_RESOURCE bypasses project quotas, including inode limits.
    // The child retains the ownership/xattr privileges needed for preservation,
    // but its exec cannot regain the quota exemption from UID zero.
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
    let result = copy
        .args([
            "-aHAX",
            "--numeric-ids",
            "--sparse",
            "--fsync",
            "--",
            &source,
        ])
        .arg(target)
        .kill_on_drop(true)
        .output()
        .await?;
    if !result.status.success() {
        return Err(Error::unavailable("Native copy failed; source retained."));
    }
    verify_copy(Path::new(source.trim_end_matches('/')), target).await?;
    block::sync(target).await
}

pub(super) async fn verify_copy(source: &Path, target: &Path) -> Result<()> {
    let source = format!("{}/", source.display());
    let verify = tokio::process::Command::new("rsync")
        .args([
            "-aHAXnc",
            "--numeric-ids",
            "--delete",
            "--itemize-changes",
            "--",
            &source,
        ])
        .arg(target)
        .kill_on_drop(true)
        .output()
        .await?;
    if !verify.status.success() || !verify.stdout.is_empty() {
        return Err(Error::conflict(
            "Native copy verification failed; source retained.",
        ));
    }
    Ok(())
}

pub async fn status(state: &Path, run: &str) -> Result<Value> {
    let directory = environment::directory(state, run)?;
    if !directory.join("directory.json").exists() {
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
    remove_temporary(&control.join("docker-runtime")).await?;
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
