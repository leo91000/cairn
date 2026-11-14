//! Verified local directory templates. Each environment receives private files;
//! a durable initialization pin retains the source through crash recovery.
use super::{atomic_write, disk, workspace};
use crate::{
    error::{Error, Result},
    project_archives::Archive,
    skills::private_dir,
    storage::{self, environment},
    validation::text,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};
use tokio_util::sync::CancellationToken;

const CLONE_RESERVE: u64 = 16 * disk::MIB;
const MAX_BASE_BYTES: u64 = 64 * 1024 * disk::MIB;

pub(crate) fn cache_limit(limit: u64) -> u64 {
    (limit / 8).min(MAX_BASE_BYTES)
}

#[derive(Clone)]
pub(crate) struct Bases {
    pub data: PathBuf,
    pub state: PathBuf,
    pub identity: (u32, u32),
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct Inputs {
    format: u8,
    projects: Vec<(String, String)>,
    disk_mib: u64,
    uid: u32,
    gid: u32,
}

impl Inputs {
    fn new(archives: &[Archive], disk_mib: u64, identity: (u32, u32)) -> Result<Self> {
        if archives.is_empty() || archives.len() > 64 || !(128..=1_048_576).contains(&disk_mib) {
            return Err(Error::bad("Invalid native base inputs."));
        }
        let mut projects = Vec::new();
        for archive in archives {
            archive.validate()?;
            projects.push((archive.project_id.clone(), archive.hash.clone()));
        }
        projects.sort();
        if projects.windows(2).any(|pair| pair[0].0 == pair[1].0) {
            return Err(Error::bad("Duplicate native base project."));
        }
        Ok(Self {
            format: 2,
            projects,
            disk_mib,
            uid: identity.0,
            gid: identity.1,
        })
    }

    fn key(&self) -> Result<String> {
        Ok(blake3::hash(&serde_json::to_vec(self)?)
            .to_hex()
            .to_string())
    }
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Ready {
    pub key: String,
    pub metadata_bytes: u64,
    inputs: Inputs,
}

impl Bases {
    pub fn eligible(archives: &[Archive], disk_mib: u64) -> bool {
        !archives.is_empty() && archives.len() <= 64 && (128..=1_048_576).contains(&disk_mib)
    }

    pub fn key(&self, archives: &[Archive], disk_mib: u64) -> Result<String> {
        Inputs::new(archives, disk_mib, self.identity)?.key()
    }

    fn root(&self) -> PathBuf {
        self.state.join("native-bases")
    }

    async fn stored(
        &self,
        archives: &[Archive],
        disk_mib: u64,
    ) -> Result<Option<(PathBuf, Ready)>> {
        if !Self::eligible(archives, disk_mib) {
            return Ok(None);
        }
        let inputs = Inputs::new(archives, disk_mib, self.identity)?;
        let key = inputs.key()?;
        let root = self.root().join(&key);
        let bytes = match tokio::fs::read(root.join("ready.json")).await {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let Ok(ready) = serde_json::from_slice::<Ready>(&bytes) else {
            return Ok(None);
        };
        if ready.key != key
            || ready.inputs != inputs
            || ready.metadata_bytes < CLONE_RESERVE
            || ready.metadata_bytes > MAX_BASE_BYTES
            || !root.join("pin").is_file()
            || !root.join("contents").is_dir()
        {
            return Ok(None);
        }
        Ok(Some((root, ready)))
    }

    pub async fn lookup(&self, archives: &[Archive], disk_mib: u64) -> Result<Option<Ready>> {
        let Some((root, ready)) = self.stored(archives, disk_mib).await? else {
            return Ok(None);
        };
        let source = root.clone();
        let verified = tokio::task::spawn_blocking(move || verify_cached_base(&source))
            .await
            .map_err(Error::internal)?;
        match verified {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return Ok(None),
            Err(_) => {
                let _ = tokio::fs::remove_file(root.join("ready.json")).await;
                return Ok(None);
            }
        }
        let metadata_bytes = clone_footprint(&root.join("contents"))?.saturating_add(CLONE_RESERVE);
        Ok(Some(Ready {
            metadata_bytes,
            ..ready
        }))
    }

    /// Caller owns storage.lock. Pinned staging attempts make cache admission
    /// and execution atomic with respect to eviction, including across processes.
    pub async fn trim(&self, maximum: u64, needed: u64, keep: Option<&str>) -> Result<u64> {
        private_dir(&self.root()).await?;
        let mut entries = tokio::fs::read_dir(self.root()).await?;
        let mut candidates = Vec::new();
        while let Some(entry) = entries.next_entry().await? {
            if !entry.file_type().await?.is_dir() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            if keep == Some(name.as_str()) {
                continue;
            }
            let pin = entry.path().join("pin");
            if tokio::fs::metadata(&pin)
                .await
                .is_ok_and(|metadata| metadata.nlink() > 1)
            {
                continue;
            }
            let recency = tokio::fs::metadata(&pin)
                .await
                .ok()
                .and_then(|metadata| metadata.modified().ok());
            candidates.push((recency, entry.path()));
        }
        candidates.sort();
        let mut freed = 0u64;
        for (_, path) in candidates {
            if freed >= needed && workspace::measure(&self.root(), true)? <= maximum {
                break;
            }
            let journal_lock = path.join("lazy/journal.lock");
            let _reader = if journal_lock.exists() {
                match crate::file_lock::exclusive(&journal_lock, "Native base is being read.") {
                    Ok(owner) => Some(owner),
                    Err(error) if error.status == 409 => continue,
                    Err(error) => return Err(error),
                }
            } else {
                None
            };
            let reclaimable = reclaimable_bytes(&path)?;
            tokio::fs::remove_dir_all(path).await?;
            freed = freed.saturating_add(reclaimable);
        }
        Ok(freed)
    }

    /// Runs inside an owning blocking worker: cancellation cannot drop its
    /// storage lease or temporary directory while nested I/O still writes.
    pub async fn warm(
        &self,
        archives: &[Archive],
        disk_mib: u64,
        limit: u64,
        policy: &storage::policy::Policy,
        stop: &CancellationToken,
    ) -> Result<()> {
        policy.validate().map_err(Error::bad)?;
        if self.stored(archives, disk_mib).await?.is_some() {
            return Ok(());
        }
        let inputs = Inputs::new(archives, disk_mib, self.identity)?;
        let key = inputs.key()?;
        if tokio::fs::metadata(self.root().join(&key).join("pin"))
            .await
            .is_ok_and(|m| m.nlink() > 1)
        {
            return Ok(());
        }

        let filesystem = workspace::filesystem(&self.state)?;
        let unit = filesystem.f_frsize.max(filesystem.f_bsize).max(4096);
        let mut expanded = 0u64;
        for archive in archives {
            if stop.is_cancelled() {
                return Ok(());
            }
            expanded = expanded.saturating_add(
                crate::project_archives::cached_allocation(&self.data, archive, unit, stop).await?,
            );
        }
        // Shared extracted caches may not exhaust the controller filesystem's
        // inode pool. Allocation rounded per entry is a conservative upper
        // bound on required inodes, including zero-length archive entries.
        if expanded.div_ceil(unit) >= filesystem.f_favail.saturating_sub(1024) {
            return Ok(());
        }
        let budget = cache_limit(limit);
        let metadata = CLONE_RESERVE;
        // Archive inputs and extracted directory templates coexist while building.
        // Include per-entry allocation, not only sum(st_size).
        let required = expanded.saturating_add(metadata);
        let estimated_base = expanded.saturating_add(metadata);
        if estimated_base > budget || stop.is_cancelled() {
            return Ok(());
        }
        let available = disk::available(limit, disk::used(&self.data, &self.state).await?);
        self.trim(
            budget.saturating_sub(estimated_base),
            required.saturating_sub(available),
            None,
        )
        .await?;
        let capacity = workspace::filesystem(&self.state)?;
        let host_free = capacity.f_bavail.saturating_mul(capacity.f_frsize);
        if required > disk::available(limit, disk::used(&self.data, &self.state).await?)
            || required.saturating_add(256 * disk::MIB) > host_free
            || workspace::measure(&self.root(), true)?.saturating_add(estimated_base) > budget
        {
            return Ok(());
        }
        private_dir(&self.state.join("environments")).await?;
        // Interrupted, unclaimed build directories use the existing environment
        // recovery namespace; they contain only disposable cache material.
        let temporary = self.state.join("environments").join(crate::config::id());
        private_dir(&temporary).await?;
        let result = self
            .build(&temporary, archives, &inputs, policy, stop)
            .await;
        if let Err(error) = result {
            let _ = tokio::fs::remove_dir_all(&temporary).await;
            return Err(error);
        }
        if stop.is_cancelled()
            || workspace::measure(&self.root(), true)?
                .saturating_add(workspace::measure(&temporary, true)?)
                > budget
            || disk::used(&self.data, &self.state).await? > limit
        {
            tokio::fs::remove_dir_all(&temporary).await?;
            return Ok(());
        }
        let metadata_bytes =
            clone_footprint(&temporary.join("contents"))?.saturating_add(CLONE_RESERVE);
        if metadata_bytes > budget {
            tokio::fs::remove_dir_all(&temporary).await?;
            return Ok(());
        }
        let ready = Ready {
            key: key.clone(),
            metadata_bytes,
            inputs,
        };
        atomic_write(&temporary.join("pin"), b"").await?;
        atomic_write(&temporary.join("ready.json"), &serde_json::to_vec(&ready)?).await?;
        private_dir(&self.root()).await?;
        let target = self.root().join(&key);
        if target.exists() {
            tokio::fs::remove_dir_all(&target).await?;
        }
        tokio::fs::rename(&temporary, &target).await?;
        tokio::fs::File::open(self.root()).await?.sync_all().await?;
        // The builder has just written and synchronized every sealed frame.
        // Later lookups invalidate this receipt on any payload/metadata change.
        atomic_write(
            &target.join("contents.digest"),
            base_signature(&target.join("contents"))?.as_bytes(),
        )
        .await?;
        Ok(())
    }

    async fn build(
        &self,
        temporary: &Path,
        archives: &[Archive],
        inputs: &Inputs,
        policy: &storage::policy::Policy,
        stop: &CancellationToken,
    ) -> Result<()> {
        let contents = temporary.join("contents");
        for name in ["home", "output", "workspaces"] {
            private_dir(&contents.join(name)).await?;
        }
        for archive in archives {
            crate::project_archives::extract_cached(
                &self.data,
                archive,
                &contents.join("workspaces").join(&archive.project_id),
                stop,
            )
            .await?;
        }
        atomic_write(&contents.join(".cairn-seed-complete"), b"native-base-v1").await?;
        workspace::own_tree_with_stop(&contents, inputs.uid, inputs.gid, stop).await?;
        policy.validate().map_err(Error::bad)?;
        super::block::sync(&contents).await?;

        Ok(())
    }

    pub async fn initialize(&self, plan: &Value, stop: &CancellationToken) -> Result<bool> {
        let run = text(plan, "runId");
        let directory = environment::directory(&self.state, run)?;
        let root = workspace::directory(&self.state, run)?;
        let pending = initializing_base(&directory)?;
        if directory.join("directory.json").exists() {
            if let Some(pending) = pending {
                let lease = environment::lock(&self.state, run, super::block::DISK_BUSY).await?;
                let record: Value = serde_json::from_slice(
                    &tokio::fs::read(lease.directory.join("directory.json")).await?,
                )?;
                let expected = super::directory::format(plan);
                if ["backend", "storage", "os", "arch"]
                    .iter()
                    .any(|key| record[*key] != expected[*key])
                {
                    return Err(Error::conflict(
                        "Prepared native directory platform changed.",
                    ));
                }
                super::directory::require_retained_tree(&workspace::workload(&self.state, run)?)?;
                finish_initializing_base(
                    &lease.directory,
                    &self.data.join("runs").join(run),
                    &pending,
                )
                .await?;
                release_pin(&self.data.join("runs").join(run), text(plan, "id")).await?;
            }
            return Ok(false);
        }
        if storage::runtime::exists(&directory)
            || directory.join("resize-source").exists()
            || root.join("initialized").exists()
            || root.join("data").exists()
        {
            if pending.is_some() {
                return Err(Error::conflict(
                    "Prepared native initialization conflicts with retained storage.",
                ));
            }
            return Ok(false);
        }
        let Some(values) = plan["nativeProjectArchives"].as_array() else {
            if pending.is_some() {
                return Err(Error::conflict(
                    "Prepared native base initialization needs its original inputs.",
                ));
            }
            return Ok(false);
        };
        let archives: Vec<Archive> = values
            .iter()
            .cloned()
            .map(serde_json::from_value)
            .collect::<std::result::Result<_, _>>()?;
        let size = plan["resources"]["diskMiB"]
            .as_u64()
            .ok_or_else(|| Error::bad("Missing native disk size."))?;
        let Some(ready) = self.lookup(&archives, size).await? else {
            if plan.get("nativeProjectBase").is_some() || pending.is_some() {
                return Err(Error::conflict("Pinned native base is unavailable."));
            }
            return Ok(false);
        };
        if plan
            .get("nativeProjectBase")
            .is_some_and(|key| key != &ready.key)
        {
            return Err(Error::conflict("Pinned native base changed."));
        }
        if stop.is_cancelled() {
            return Err(Error::unavailable("Native base clone stopped."));
        }
        let lease = environment::lock(&self.state, run, super::block::DISK_BUSY).await?;
        private_dir(&root).await?;
        let workload = workspace::workload(&self.state, run)?;
        let pending = if let Some(pending) = pending {
            for entry in std::fs::read_dir(&lease.directory)? {
                let entry = entry?;
                let name = entry.file_name();
                let atomic = name
                    .to_str()
                    .and_then(|name| name.strip_prefix('.'))
                    .and_then(|name| name.strip_suffix(".tmp"))
                    .is_some_and(|name| crate::validation::uuid(name).is_ok())
                    && entry.file_type()?.is_file();
                if !environment::retained(&name) && name != pending.name.as_str() && !atomic {
                    return Err(Error::conflict(
                        "Unrecognized retained storage beside a prepared native initialization.",
                    ));
                }
            }
            if pending.key != ready.key {
                return Err(Error::conflict(
                    "Prepared native base initialization inputs changed.",
                ));
            }
            let original = std::fs::metadata(self.root().join(&ready.key).join("pin"))?;
            let held = std::fs::symlink_metadata(lease.directory.join(&pending.name))?;
            if !held.is_file()
                || held.file_type().is_symlink()
                || (original.dev(), original.ino()) != (held.dev(), held.ino())
            {
                return Err(Error::conflict(
                    "Prepared native base initialization pin changed.",
                ));
            }
            pending
        } else {
            super::directory::require_fresh(&lease.directory, &root, &workload)?;
            let attempt = text(plan, "id");
            crate::validation::uuid(attempt)?;
            let name = format!("{INITIALIZING_BASE}{}.{}.pin", ready.key, attempt);
            // One hard link is both the durable intent and a cache pin. Startup
            // cleanup of attempt staging cannot release this environment's source.
            tokio::fs::hard_link(
                self.root().join(&ready.key).join("pin"),
                lease.directory.join(&name),
            )
            .await?;
            tokio::fs::File::open(&lease.directory)
                .await?
                .sync_all()
                .await?;
            InitializingBase {
                name,
                key: ready.key.clone(),
                attempt: attempt.into(),
            }
        };
        private_dir(&workload).await?;
        workspace::require_no_mounts(&workload)?;
        super::quota::prepare(&self.state, &workload, size * disk::MIB).await?;
        let data = workload.join("data");
        let staging = workload.join("base.partial");
        let source = self.root().join(&ready.key).join("contents");
        let capacity = super::directory::CopyCapacity::new(
            &self.state,
            &workload,
            ready.metadata_bytes,
            crate::storage::policy::for_node(&plan["storage"]["policy"])?,
        );
        capacity.check().await?;
        let existing_data = match tokio::fs::symlink_metadata(&data).await {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => true,
            Ok(_) => {
                return Err(Error::conflict(
                    "Prepared native data directory was replaced by an alias.",
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
            Err(error) => return Err(error.into()),
        };
        if existing_data && std::fs::read_dir(&data)?.next().is_some() {
            // Rename may have completed before the format marker. Verify only;
            // never overwrite retained files while replaying initialization.
            super::directory::verify_copy(&source, &data, stop, &capacity).await?;
        } else {
            if staging.exists() {
                tokio::fs::remove_dir_all(&staging).await?;
            }
            private_dir(&staging).await?;
            if let Err(error) =
                super::directory::copy_verified(&source, &staging, stop, &capacity).await
            {
                // The source pin and initialization intent remain replayable;
                // a settled, failed private copy need not consume scarce space.
                workspace::require_no_mounts(&staging)?;
                tokio::fs::remove_dir_all(&staging).await?;
                tokio::fs::File::open(&workload).await?.sync_all().await?;
                return Err(error);
            }
            tokio::fs::rename(&staging, &data).await?;
        }
        tokio::fs::File::open(&workload).await?.sync_all().await?;
        atomic_write(
            &root.join("file-owner.json"),
            &serde_json::to_vec(&json!({
                "uid": ready.inputs.uid, "gid": ready.inputs.gid
            }))?,
        )
        .await?;
        super::directory::publish_format(&lease.directory, &root, plan, size * disk::MIB).await?;
        finish_initializing_base(
            &lease.directory,
            &self.data.join("runs").join(run),
            &pending,
        )
        .await?;
        release_pin(&self.data.join("runs").join(run), text(plan, "id")).await?;
        Ok(true)
    }
}

const INITIALIZING_BASE: &str = "native-base-initializing-v1.";

struct InitializingBase {
    name: String,
    key: String,
    attempt: String,
}

fn initializing_base(directory: &Path) -> Result<Option<InitializingBase>> {
    let entries = match std::fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let mut pending = None;
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(value) = name.strip_prefix(INITIALIZING_BASE) else {
            continue;
        };
        let Some((key, attempt)) = value
            .strip_suffix(".pin")
            .and_then(|value| value.split_once('.'))
        else {
            return Err(Error::conflict(
                "Invalid prepared native initialization intent.",
            ));
        };
        if key.len() != 64
            || !key
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
            || crate::validation::uuid(attempt).is_err()
            || !entry.file_type()?.is_file()
            || pending.is_some()
        {
            return Err(Error::conflict(
                "Ambiguous prepared native initialization intent.",
            ));
        }
        pending = Some(InitializingBase {
            key: key.into(),
            attempt: attempt.into(),
            name,
        });
    }
    Ok(pending)
}

async fn finish_initializing_base(
    directory: &Path,
    stage: &Path,
    pending: &InitializingBase,
) -> Result<()> {
    // Keep the durable intent until the old staging pin has been settled too.
    // A failed cleanup must retain its retry identity after format publication.
    release_pin(stage, &pending.attempt).await?;
    match tokio::fs::File::open(stage).await {
        Ok(stage) => stage.sync_all().await?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    tokio::fs::remove_file(directory.join(&pending.name)).await?;
    tokio::fs::File::open(directory).await?.sync_all().await?;
    Ok(())
}

/// Admission concerns a private copy on ext4, not compressed blocks in the
/// controller filesystem. Keep a conservative per-entry/logical allocation bound.
pub(super) fn clone_footprint(root: &Path) -> std::io::Result<u64> {
    let mut pending = vec![root.to_owned()];
    let mut bytes = 0_u64;
    while let Some(path) = pending.pop() {
        let metadata = std::fs::symlink_metadata(&path)?;
        let required = if metadata.is_file() {
            metadata.len().div_ceil(4096).saturating_mul(4096).max(4096)
        } else {
            4096
        };
        bytes = bytes.saturating_add(required.max(metadata.blocks().saturating_mul(512)));
        if metadata.is_dir() {
            for entry in std::fs::read_dir(path)? {
                pending.push(entry?.path());
            }
        }
    }
    Ok(bytes)
}

fn base_signature(root: &Path) -> std::io::Result<String> {
    use std::io::Read;
    let mut pending = vec![root.to_owned()];
    let mut hash = blake3::Hasher::new();
    while let Some(path) = pending.pop() {
        let metadata = std::fs::symlink_metadata(&path)?;
        let relative = path.strip_prefix(root).map_err(std::io::Error::other)?;
        hash.update(relative.as_os_str().as_encoded_bytes());
        for value in [
            metadata.mode() as u64,
            metadata.uid() as u64,
            metadata.gid() as u64,
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
            let mut file = std::fs::File::open(&path)?;
            let mut buffer = vec![0; 131072];
            loop {
                let length = file.read(&mut buffer)?;
                if length == 0 {
                    break;
                }
                hash.update(&buffer[..length]);
            }
        } else if metadata.file_type().is_symlink() {
            hash.update(std::fs::read_link(&path)?.as_os_str().as_encoded_bytes());
        } else {
            return Err(std::io::Error::other("Unsupported native base entry."));
        }
    }
    Ok(hash.finalize().to_hex().to_string())
}

fn verify_cached_base(root: &Path) -> std::io::Result<()> {
    if std::fs::read_to_string(root.join("contents.digest"))?
        != base_signature(&root.join("contents"))?
    {
        return Err(std::io::Error::other("Native base contents changed."));
    }
    Ok(())
}

pub(super) fn reclaimable_bytes(root: &Path) -> Result<u64> {
    let mut pending = vec![root.to_owned()];
    let mut total = 0u64;
    let mut seen = std::collections::HashSet::new();
    while let Some(path) = pending.pop() {
        let metadata = std::fs::symlink_metadata(&path)?;
        if metadata.is_dir() {
            for entry in std::fs::read_dir(path)? {
                pending.push(entry?.path());
            }
        } else if metadata.is_file()
            && metadata.nlink() == 1
            && seen.insert((metadata.dev(), metadata.ino()))
        {
            total = total.saturating_add(metadata.blocks().saturating_mul(512));
        }
    }
    Ok(total)
}

pub(crate) fn touch_pin(path: &Path) {
    if let Ok(file) = std::fs::File::open(path) {
        let _ = file.set_modified(std::time::SystemTime::now());
    }
}

pub(crate) fn pin_path(root: &Path, attempt: &str) -> Result<PathBuf> {
    crate::validation::uuid(attempt)?;
    Ok(root.join(format!("native-base-{attempt}.pin")))
}

pub(crate) struct PendingPin {
    path: PathBuf,
    keep: bool,
}

impl PendingPin {
    pub fn new(root: &Path, attempt: &str) -> Result<Self> {
        Ok(Self {
            path: pin_path(root, attempt)?,
            keep: false,
        })
    }

    pub fn keep(&mut self) {
        self.keep = true;
    }
}

impl Drop for PendingPin {
    fn drop(&mut self) {
        if !self.keep {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

pub(crate) async fn release_pin(root: &Path, attempt: &str) -> Result<()> {
    match tokio::fs::remove_file(pin_path(root, attempt)?).await {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

/// Startup calls this after fencing old attempts and before admitting any new
/// plans. The next connector preparation regenerates the required attempt pin.
pub(crate) async fn recover_pins(data: &Path) -> Result<()> {
    let mut runs = match tokio::fs::read_dir(data.join("runs")).await {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    while let Some(run) = runs.next_entry().await? {
        if !run.file_type().await?.is_dir() {
            continue;
        }
        let mut files = tokio::fs::read_dir(run.path()).await?;
        while let Some(file) = files.next_entry().await? {
            let name = file.file_name().to_string_lossy().into_owned();
            if let Some(attempt) = name
                .strip_prefix("native-base-")
                .and_then(|name| name.strip_suffix(".pin"))
                && crate::validation::uuid(attempt).is_ok()
            {
                tokio::fs::remove_file(file.path()).await?;
            }
        }
    }
    Ok(())
}

pub(crate) async fn release_attempt(data: &Path, attempt: &str) -> Result<()> {
    crate::validation::uuid(attempt)?;
    let file = data.join("runner-plans").join(format!("{attempt}.json"));
    let plan: Value = match tokio::fs::read(file).await {
        Ok(bytes) => serde_json::from_slice(&bytes)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    let run = text(&plan, "runId");
    crate::validation::uuid(run)?;
    release_pin(&data.join("runs").join(run), attempt).await
}

pub(crate) async fn request(operation: &str, value: &Value) -> Result<Value> {
    let response = crate::nodes::local_client()?
        .post(format!("http://localhost/native-bases/{operation}"))
        .bearer_auth(
            crate::execution::secret(&crate::nodes::node_data_dir(), "runner-secret").await?,
        )
        .timeout(std::time::Duration::from_secs(5))
        .json(value)
        .send()
        .await
        .map_err(Error::internal)?;
    if !response.status().is_success() {
        return Err(Error::unavailable("Native base cache is unavailable."));
    }
    response.json().await.map_err(Error::internal)
}

#[cfg(test)]
mod payload_size_tests {
    use super::*;

    #[tokio::test]
    async fn large_verified_directory_payloads_remain_discoverable_and_do_not_rebuild() {
        let fixture = tempfile::tempdir().unwrap();
        let bases = Bases {
            data: fixture.path().join("data"),
            state: fixture.path().join("state"),
            identity: (unsafe { libc::geteuid() }, unsafe { libc::getegid() }),
        };
        let archive = Archive {
            project_id: crate::config::id(),
            revision: "a".repeat(40),
            origin_hash: None,
            key: "b".repeat(64),
            hash: "c".repeat(64),
            bytes: 1,
            logical_bytes: 300 * disk::MIB,
        };
        let inputs = Inputs::new(std::slice::from_ref(&archive), 512, bases.identity).unwrap();
        let key = inputs.key().unwrap();
        let root = bases.root().join(&key);
        let contents = root.join("contents");
        for name in ["home", "workspaces", "output"] {
            private_dir(&contents.join(name)).await.unwrap();
        }
        let file = contents.join("workspaces/large-file");
        let data = std::fs::File::create(&file).unwrap();
        data.set_len(300 * disk::MIB).unwrap();
        let inode = data.metadata().unwrap().ino();
        drop(data);
        atomic_write(&root.join("pin"), b"").await.unwrap();
        atomic_write(
            &root.join("contents.digest"),
            base_signature(&contents).unwrap().as_bytes(),
        )
        .await
        .unwrap();
        let metadata_bytes = clone_footprint(&contents).unwrap() + CLONE_RESERVE;
        assert!(metadata_bytes > 256 * disk::MIB);
        assert!(metadata_bytes > workspace::measure(&contents, true).unwrap());
        let ready = Ready {
            key: key.clone(),
            metadata_bytes,
            inputs,
        };
        atomic_write(
            &root.join("ready.json"),
            &serde_json::to_vec(&ready).unwrap(),
        )
        .await
        .unwrap();
        let found = bases
            .lookup(std::slice::from_ref(&archive), 512)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(found.metadata_bytes, metadata_bytes);
        // No archive exists: warming an already supported verified base must
        // return without attempting another extraction/build.
        bases
            .warm(
                std::slice::from_ref(&archive),
                512,
                16 * 1024 * disk::MIB,
                &storage::policy::Policy::default(),
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(std::fs::metadata(file).unwrap().ino(), inode);
    }
}
