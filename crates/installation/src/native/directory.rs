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
        atomic::{AtomicBool, AtomicU64, Ordering},
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
}

impl Volume {
    pub fn activate_attempt(&self, attempt: &str) -> Result<()> {
        crate::validation::uuid(attempt)
    }

    pub fn set_paused(&self, value: bool) {
        self.paused.store(value, Ordering::Release);
    }

    pub fn cpu_pause_reason(&self) -> Result<Option<&'static str>> {
        let policy = match std::fs::read(self.state.join("storage-policy.json")) {
            Ok(bytes) => storage::policy::from_persisted(&bytes)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                storage::policy::Policy::default()
            }
            Err(error) => return Err(error.into()),
        };
        let (total, free) = storage::policy::space(&self.root)?;
        Ok((free <= policy.reserve(total)).then_some("disk-space"))
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
        Self {
            volume: mount.volume.clone(),
            filesystem: mount.filesystem.clone(),
            mount: tokio::sync::Mutex::new(mount),
        }
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
            remove_temporary(&self.volume.root.join("tmp")).await?;
            remove_temporary(&self.volume.root.parent().unwrap().join("docker-runtime")).await?;
        }
        let record = async {
            let attempt =
                tokio::fs::read_to_string(self.volume.root.parent().unwrap().join("attempt"))
                    .await?;
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
    let workload = root.join("workload");
    workspace::require_no_mounts(&workload)?;
    private_dir(&workload).await?;
    let saved = environment::directory(state, run)?;
    if !saved.join("directory.json").exists() && storage::runtime::exists(&saved) {
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
    let policy: storage::policy::Policy =
        serde_json::from_value(plan["storage"]["policy"].clone())?;
    policy.validate().map_err(Error::bad)?;
    let saved_limit = owner.directory.join("directory-limit");
    if let Ok(value) = tokio::fs::read_to_string(&saved_limit).await
        && value.parse::<u64>().map_err(Error::internal)? > bytes
    {
        return Err(Error::conflict(
            "Native reservation is smaller than its retained allowance; reconcile the grant before resuming.",
        ));
    }
    quota::prepare(state, &workload, bytes).await?;
    let runtime = json!({ "backend": "native", "storage": STORAGE, "os": "linux", "arch": "x86_64", "runtimeId": plan["runtimeId"] });
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
    let data = workload.join("data");
    private_dir(&data).await?;
    let view = block::View {
        home: data.join("home"),
        workspace: data.join("workspaces"),
        output: data.join("output"),
        filesystems: vec![data],
    };
    atomic_write(&root.join("view.json"), &serde_json::to_vec(&view)?).await?;
    atomic_write(&marker, &serde_json::to_vec(&runtime)?).await?;
    atomic_write(&root.join("directory.json"), &serde_json::to_vec(&runtime)?).await?;
    atomic_write(
        &owner.directory.join("runtime.json"),
        &serde_json::to_vec(&runtime)?,
    )
    .await?;
    atomic_write(
        &owner.directory.join("directory-limit"),
        bytes.to_string().as_bytes(),
    )
    .await?;
    if !root.join("block-initialized").exists() {
        atomic_write(&root.join("block-seeding"), b"1").await?;
    }
    Ok(Mounted {
        volume: Arc::new(Volume {
            root: workload,
            state: state.to_owned(),
            stop: CancellationToken::new(),
            paused: AtomicBool::new(false),
        }),
        filesystem: Arc::new(AtomicU64::new(bytes)),
        owner,
        _physical: physical,
        aborted: false,
    })
}

async fn migrate(
    state: &Path,
    run: &str,
    plan: &Value,
    stop: &CancellationToken,
    root: &Path,
) -> Result<()> {
    let staging = root.join("workload/directory-conversion");
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
                &root.join("workload"),
                plan["resources"]["diskMiB"]
                    .as_u64()
                    .ok_or_else(|| Error::bad("Missing native allowance."))?
                    * super::disk::MIB,
            )
            .await?;
            private_dir(&staging).await?;
            copy_verified(&root.join("data"), &staging).await
        }
        .await;
        legacy.close().await?;
        drop(legacy);
        copied?;
        atomic_write(&marker, b"1").await?;
    }
    let owner = environment::lock(state, run, DISK_BUSY).await?;
    if staging.exists() {
        let data = root.join("workload/data");
        if data.exists() {
            tokio::fs::remove_dir(&data).await?;
        }
        tokio::fs::rename(&staging, &data).await?;
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
    Ok(())
}

pub(super) async fn copy_verified(source: &Path, target: &Path) -> Result<()> {
    let source = format!("{}/", source.display());
    let result = tokio::process::Command::new("rsync")
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
    block::sync(target).await
}

pub async fn status(state: &Path, run: &str) -> Result<Value> {
    let directory = environment::directory(state, run)?;
    if !directory.join("directory.json").exists() {
        return block::status(state, run).await;
    }
    let used = quota::usage(&workspace::directory(state, run)?.join("workload"))?;
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
        if root.join("workload").exists() && !root.join("quota-retirement.json").exists() {
            super::systemd::directory_consumers::stop(&root.join("workload")).await?;
        }
        block::sync(&root).await?;
        atomic_write(&root.join("attempt"), attempt.as_bytes()).await?;
        return Ok(());
    }
    block::recover_completed(state, run, attempt).await
}

pub async fn action(state: &Path, run: &str, delete: bool) -> Result<Vec<String>> {
    let lease = environment::lock(state, run, DISK_BUSY).await?;
    if delete {
        let root = workspace::directory(state, run)?;
        if root.join("workload").exists()
            && root.join("quota-owner.json").exists()
            && !root.join("quota-retirement.json").exists()
        {
            super::systemd::directory_consumers::stop(&root.join("workload")).await?;
        }
        quota::retire(&root).await?;
    }
    block::action_locked(&lease.directory, delete).await
}

pub fn used_bytes(path: &Path) -> Result<u64> {
    workspace::measure(path, true)
}

/// The workload quota includes managed temporary files, while control metadata
/// remains writable after ENOSPC/EDQUOT. Legacy fixtures retain their old path.
pub async fn temporary(state: &Path, run: &str) -> Result<PathBuf> {
    let root = workspace::directory(state, run)?;
    Ok(if root.join("workload").is_dir() {
        root.join("workload/tmp")
    } else {
        root.join("tmp")
    })
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
