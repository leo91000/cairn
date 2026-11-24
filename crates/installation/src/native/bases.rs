//! Verified local directory templates. Each environment receives private files;
//! a durable initialization pin retains the source through crash recovery.
//!
//! Contents live on the data pool under `native-data/.cairn-bases/<key>`, so a
//! clone shares the pool's filesystem and full usage scans count them through
//! its `statvfs`. Control records (`pin`, `contents.digest`, `ready.json`) stay
//! in `native-bases/<key>` on the controller filesystem: pins remain hard links
//! beside attempt staging and environment intents.
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
    collections::{HashMap, HashSet},
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
};
use tokio_util::sync::CancellationToken;

const CLONE_RESERVE: u64 = 16 * disk::MIB;
const MAX_BASE_BYTES: u64 = 64 * 1024 * disk::MIB;
const FORMAT: u8 = 3;

/// Root-only directory on the data pool. Nothing below it is ever bound into
/// a workload: environments receive private clones under their own run.
pub(crate) const POOL_DIRECTORY: &str = ".cairn-bases";
const BUILD_PREFIX: &str = ".build-";
const PROBE_PREFIX: &str = ".probe-";

pub(crate) fn cache_limit(limit: u64) -> u64 {
    (limit / 8).min(MAX_BASE_BYTES)
}

#[derive(Clone)]
pub(crate) struct Bases {
    pub data: PathBuf,
    pub state: PathBuf,
    pub identity: (u32, u32),
    #[cfg(test)]
    pub build_entered: Option<std::sync::Arc<tokio::sync::Notify>>,
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
            format: FORMAT,
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
    /// Physical allocation of the contents, measured once at publication.
    /// Cache budgets and trimming use it instead of walking the tree.
    pub allocated_bytes: u64,
    /// Binds this record to the published contents directory on the pool.
    contents_inode: u64,
    inputs: Inputs,
}

fn valid_key(name: &str) -> bool {
    name.len() == 64
        && name
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

/// Contents verified by this controller process, by key and directory identity.
/// Publication counts as verification; a controller restart hashes again.
fn verified() -> &'static Mutex<HashSet<(String, u64, u64)>> {
    static VERIFIED: OnceLock<Mutex<HashSet<(String, u64, u64)>>> = OnceLock::new();
    VERIFIED.get_or_init(Default::default)
}

fn remember_verified(key: &str, contents: &std::fs::Metadata) {
    verified()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert((key.to_owned(), contents.dev(), contents.ino()));
}

fn forget_verified(key: &str) {
    verified()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .retain(|(verified, _, _)| verified != key);
}

#[cfg(test)]
pub(crate) fn forget_verification(key: &str) {
    forget_verified(key);
}

/// Probe once per pool and controller process. A failed probe is never fatal:
/// the copying fallback is always correct, only slower.
pub(crate) fn reflink_supported(pool: &Path) -> bool {
    static PROBED: OnceLock<Mutex<HashMap<PathBuf, bool>>> = OnceLock::new();
    let probed = PROBED.get_or_init(Default::default);
    if let Some(supported) = probed
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(pool)
    {
        return *supported;
    }

    let supported = probe_reflink(pool).unwrap_or(false);
    probed
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(pool.to_owned(), supported);
    supported
}

fn probe_reflink(pool: &Path) -> std::io::Result<bool> {
    use std::{io::Write, os::fd::AsRawFd};
    let name = format!("{PROBE_PREFIX}{}", crate::config::id());
    let (source, target) = (
        pool.join(format!("{name}.source")),
        pool.join(format!("{name}.target")),
    );
    let result = (|| {
        // FICLONE needs a readable source and a writable target.
        let mut original = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&source)?;
        original.write_all(&[0x5a; 4096])?;
        let clone = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&target)?;
        Ok(unsafe { libc::ioctl(clone.as_raw_fd(), libc::FICLONE, original.as_raw_fd()) } == 0)
    })();
    for path in [&source, &target] {
        if let Err(error) = std::fs::remove_file(path)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            return Err(error);
        }
    }
    result
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

    fn pool(&self) -> PathBuf {
        workspace::data_root(&self.state).join(POOL_DIRECTORY)
    }

    pub(crate) fn contents(&self, key: &str) -> PathBuf {
        self.pool().join(key)
    }

    async fn stored(
        &self,
        archives: &[Archive],
        disk_mib: u64,
    ) -> Result<Option<(PathBuf, std::fs::Metadata, Ready)>> {
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
        // A published record and its contents are separate renames. Either
        // half alone, or contents from another build, is never a base.
        let contents = match tokio::fs::symlink_metadata(self.contents(&key)).await {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => metadata,
            Ok(_) => return Ok(None),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        if ready.key != key
            || ready.inputs != inputs
            || ready.metadata_bytes < CLONE_RESERVE
            || ready.metadata_bytes > MAX_BASE_BYTES
            || ready.contents_inode != contents.ino()
            || !root.join("pin").is_file()
            || !root.join("contents.digest").is_file()
        {
            return Ok(None);
        }
        Ok(Some((root, contents, ready)))
    }

    /// Hashes a base once per controller process. Later lookups trust the
    /// sealed root-only tree after checking its records and directory identity.
    pub async fn lookup(&self, archives: &[Archive], disk_mib: u64) -> Result<Option<Ready>> {
        let Some((root, contents, ready)) = self.stored(archives, disk_mib).await? else {
            return Ok(None);
        };
        let identity = (ready.key.clone(), contents.dev(), contents.ino());
        if verified()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains(&identity)
        {
            return Ok(Some(ready));
        }

        let (record, source) = (root.clone(), self.contents(&ready.key));
        let verified = tokio::task::spawn_blocking(move || verify_cached_base(&record, &source))
            .await
            .map_err(Error::internal)?;
        match verified {
            Ok(()) => remember_verified(&ready.key, &contents),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return Ok(None),
            Err(_) => {
                let _ = tokio::fs::remove_file(root.join("ready.json")).await;
                return Ok(None);
            }
        }
        Ok(Some(ready))
    }

    /// Recorded allocation of one cache entry. Legacy or unsealed entries have
    /// no trusted record and are measured; they are removed at startup or trim.
    fn entry_bytes(&self, name: &str, control: &Path) -> Result<(u64, u64)> {
        let recorded = std::fs::read(control.join("ready.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Ready>(&bytes).ok())
            .filter(|ready| ready.inputs.format == FORMAT && ready.key == name);
        if let Some(ready) = recorded {
            return Ok((ready.allocated_bytes, ready.allocated_bytes));
        }

        let mut total = workspace::measure(control, true)?;
        let mut reclaimable = reclaimable_bytes(control)?;
        let contents = self.contents(name);
        if valid_key(name) && contents.is_dir() {
            total = total.saturating_add(workspace::measure(&contents, true)?);
            reclaimable = reclaimable.saturating_add(reclaimable_bytes(&contents)?);
        }
        Ok((total, reclaimable))
    }

    /// Sum of recorded cache allocations; it never walks published contents.
    fn cached_bytes(&self) -> Result<u64> {
        let entries = match std::fs::read_dir(self.root()) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(error) => return Err(error.into()),
        };
        let mut total = 0u64;
        for entry in entries {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            total = total.saturating_add(self.entry_bytes(&name, &entry.path())?.0);
        }
        Ok(total)
    }

    /// Unpublish the control record first: a crash then leaves orphan pool
    /// contents for startup cleanup, never a record naming partial contents.
    async fn remove_entry(&self, name: &str) -> Result<()> {
        forget_verified(name);
        match tokio::fs::remove_dir_all(self.root().join(name)).await {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        tokio::fs::File::open(self.root()).await?.sync_all().await?;
        if valid_key(name) {
            remove_tree(&self.contents(name)).await?;
        }
        Ok(())
    }

    /// Caller owns storage.lock. Pinned staging attempts make cache admission
    /// and execution atomic with respect to eviction, including across processes.
    pub async fn trim(&self, maximum: u64, needed: u64, keep: Option<&str>) -> Result<u64> {
        private_dir(&self.root()).await?;
        let mut entries = tokio::fs::read_dir(self.root()).await?;
        let mut candidates = Vec::new();
        let mut total = 0u64;
        while let Some(entry) = entries.next_entry().await? {
            if !entry.file_type().await?.is_dir() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            let (bytes, reclaimable) = self.entry_bytes(&name, &entry.path())?;
            total = total.saturating_add(bytes);
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
            candidates.push((recency, name, entry.path(), bytes, reclaimable));
        }
        candidates.sort();
        let mut freed = 0u64;
        for (_, name, path, bytes, reclaimable) in candidates {
            if freed >= needed && total <= maximum {
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
            self.remove_entry(&name).await?;
            total = total.saturating_sub(bytes);
            // Reflinked clones may still share these extents; callers sample
            // the pool again before relying on the space.
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

        // Contents are extracted on the data pool they will be cloned within;
        // its geometry, inodes and physical reserve bound the build.
        let pool = self.pool();
        private_dir(&pool).await?;
        let filesystem = workspace::filesystem(&pool)?;
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
        // Shared extracted caches may not exhaust the data pool's inodes.
        // Allocation rounded per entry is a conservative upper bound on
        // required inodes, including zero-length archive entries.
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
        let mut used = disk::used(&self.data, &self.state).await?;
        let freed = self
            .trim(
                budget.saturating_sub(estimated_base),
                required.saturating_sub(disk::available(limit, used)),
                None,
            )
            .await?;
        if freed > 0 {
            used = disk::used(&self.data, &self.state).await?;
        }
        let capacity = workspace::filesystem(&pool)?;
        let host_free = capacity.f_bavail.saturating_mul(capacity.f_frsize);
        if required > disk::available(limit, used)
            || required
                .saturating_add(policy.reserve(capacity.f_blocks.saturating_mul(capacity.f_frsize)))
                > host_free
            || self.cached_bytes()?.saturating_add(estimated_base) > budget
        {
            return Ok(());
        }
        private_dir(&self.state.join("environments")).await?;
        // Interrupted, unclaimed control records use the existing environment
        // recovery namespace; build contents use a private pool prefix that
        // startup removes. Both contain only disposable cache material.
        let id = crate::config::id();
        let control = self.state.join("environments").join(&id);
        let contents = pool.join(format!("{BUILD_PREFIX}{id}"));
        private_dir(&control).await?;
        private_dir(&contents).await?;
        let result = async {
            self.build(&contents, archives, &inputs, limit, policy, stop)
                .await?;
            if stop.is_cancelled() {
                return Ok(None);
            }
            let measured = contents.clone();
            let (allocated_bytes, footprint) = tokio::task::spawn_blocking(move || {
                Ok::<_, Error>((
                    workspace::measure(&measured, true)?,
                    clone_footprint(&measured)?,
                ))
            })
            .await
            .map_err(Error::internal)??;
            let metadata_bytes = footprint.saturating_add(CLONE_RESERVE);
            if self.cached_bytes()?.saturating_add(allocated_bytes) > budget
                || disk::used(&self.data, &self.state).await? > limit
                || metadata_bytes > budget
            {
                return Ok(None);
            }
            Ok(Some(Ready {
                key: key.clone(),
                metadata_bytes,
                allocated_bytes,
                contents_inode: tokio::fs::symlink_metadata(&contents).await?.ino(),
                inputs,
            }))
        }
        .await;
        let published = match result {
            Ok(Some(ready)) => self.publish(&control, &contents, &ready).await,
            Ok(None) => Ok(()),
            Err(error) => Err(error),
        };
        remove_tree(&control).await?;
        remove_tree(&contents).await?;
        published
    }

    /// Seal every lookup input while still private. A lookup may run without
    /// storage.lock and must never invalidate a half-published candidate.
    async fn publish(&self, control: &Path, contents: &Path, ready: &Ready) -> Result<()> {
        atomic_write(&control.join("pin"), b"").await?;
        let measured = contents.to_owned();
        let digest = tokio::task::spawn_blocking(move || base_signature(&measured))
            .await
            .map_err(Error::internal)??;
        atomic_write(&control.join("contents.digest"), digest.as_bytes()).await?;
        atomic_write(&control.join("ready.json"), &serde_json::to_vec(ready)?).await?;
        private_dir(&self.root()).await?;
        if self.root().join(&ready.key).exists() || self.contents(&ready.key).exists() {
            self.remove_entry(&ready.key).await?;
        }

        // Contents first: until the record is renamed they are an orphan that
        // startup removes. The record then publishes both in one rename.
        let target = self.contents(&ready.key);
        tokio::fs::rename(contents, &target).await?;
        tokio::fs::File::open(self.pool()).await?.sync_all().await?;
        if let Err(error) = tokio::fs::rename(control, self.root().join(&ready.key)).await {
            remove_tree(&target).await?;
            return Err(error.into());
        }
        tokio::fs::File::open(self.root()).await?.sync_all().await?;
        remember_verified(&ready.key, &tokio::fs::symlink_metadata(&target).await?);
        Ok(())
    }

    async fn build(
        &self,
        contents: &Path,
        archives: &[Archive],
        inputs: &Inputs,
        limit: u64,
        policy: &storage::policy::Policy,
        stop: &CancellationToken,
    ) -> Result<()> {
        #[cfg(test)]
        if let Some(entered) = &self.build_entered {
            entered.notify_one();
            stop.cancelled().await;
            return Err(Error::unavailable("Native base preparation stopped."));
        }
        let pool = workspace::data_root(&self.state);
        let filesystem = workspace::filesystem(&pool)?;
        let unit = filesystem.f_frsize.max(filesystem.f_bsize).max(4096);
        let admission = std::sync::Arc::new(std::sync::Mutex::new(super::ingress::Admission::new(
            &self.data,
            &self.state,
            contents,
            &pool,
            limit,
            policy,
        )?));
        admission
            .lock()
            .map_err(Error::internal)?
            .admit(unit.saturating_add(1024))?;
        for name in ["home", "output", "workspaces"] {
            admission
                .lock()
                .map_err(Error::internal)?
                .admit(unit.saturating_add(1024))?;
            private_dir(&contents.join(name)).await?;
        }
        for archive in archives {
            let admission = admission.clone();
            crate::project_archives::extract_cached_checked(
                &self.data,
                archive,
                &contents.join("workspaces").join(&archive.project_id),
                stop,
                unit,
                move |bytes| admission.lock().map_err(Error::internal)?.admit(bytes),
            )
            .await?;
        }
        admission
            .lock()
            .map_err(Error::internal)?
            .admit(unit.saturating_add(1024))?;
        atomic_write(&contents.join(".cairn-seed-complete"), b"native-base-v1").await?;
        workspace::own_tree_with_stop(contents, inputs.uid, inputs.gid, stop).await?;
        admission.lock().map_err(Error::internal)?.admit(0)?;
        super::block::sync(contents).await?;

        Ok(())
    }

    #[cfg(test)]
    pub async fn initialize(&self, plan: &Value, stop: &CancellationToken) -> Result<bool> {
        self.initialize_reserved(plan, stop, 0, None).await
    }

    pub async fn initialize_reserved(
        &self,
        plan: &Value,
        stop: &CancellationToken,
        reserved: u64,
        pool_owner: Option<disk::StorageGuard>,
    ) -> Result<bool> {
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
                super::spares::finish(&root).await?;
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
        let claimed = super::spares::claim(&self.state, run, &ready.key).await?;
        // Cache eviction is serialized with publishing the durable hard-link
        // pin. From here on, the pin keeps the source alive and the environment
        // lease and byte reservation cover the clone and its capacity checks.
        drop(pool_owner);
        private_dir(&workload).await?;
        workspace::require_no_mounts(&workload)?;
        super::quota::prepare(&self.state, &workload, size * disk::MIB).await?;
        let data = workload.join("data");
        let staging = workload.join("base.partial");
        let source = self.contents(&ready.key);
        // Both monitored copy and verification check capacity before starting.
        let capacity = super::directory::CopyCapacity::new(
            &self.state,
            &workload,
            ready.metadata_bytes,
            crate::storage::policy::for_node(&plan["storage"]["policy"])?,
        );
        let capacity = capacity.with_reservation(reserved);
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
        if claimed {
            if !existing_data || !data.join(".cairn-seed-complete").is_file() {
                return Err(Error::conflict(
                    "Claimed native spare is missing its verified seed.",
                ));
            }
        } else if existing_data && std::fs::read_dir(&data)?.next().is_some() {
            // Rename may have completed before the format marker. Verify only;
            // never overwrite retained files while replaying initialization.
            super::directory::verify_copy(&source, &data, stop, &capacity).await?;
        } else {
            if staging.exists() {
                tokio::fs::remove_dir_all(&staging).await?;
            }
            private_dir(&staging).await?;
            let pool = self.pool();
            let reflink = tokio::task::spawn_blocking(move || reflink_supported(&pool))
                .await
                .map_err(Error::internal)?;
            if let Err(error) =
                super::directory::copy_verified(&source, &staging, stop, &capacity, reflink).await
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
        super::spares::finish(&root).await?;
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

fn verify_cached_base(control: &Path, contents: &Path) -> std::io::Result<()> {
    if std::fs::read_to_string(control.join("contents.digest"))? != base_signature(contents)? {
        return Err(std::io::Error::other("Native base contents changed."));
    }
    Ok(())
}

async fn remove_tree(path: &Path) -> Result<()> {
    match tokio::fs::remove_dir_all(path).await {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

/// Startup calls this after `recover_pins`, before admitting any plan, so a
/// pin's link count reflects only durable environment intents. No connector
/// can pin a base it has not looked up from this controller, and the warmer is
/// not running yet. Pinned legacy entries remain for a later trim.
pub(crate) async fn recover_cache(state: &Path) -> Result<()> {
    let bases = Bases {
        data: PathBuf::new(),
        state: state.to_owned(),
        identity: (0, 0),
        #[cfg(test)]
        build_entered: None,
    };
    let mut records = HashSet::new();
    let mut entries = match tokio::fs::read_dir(bases.root()).await {
        Ok(entries) => Some(entries),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    while let Some(entry) = match &mut entries {
        Some(entries) => entries.next_entry().await?,
        None => None,
    } {
        if !entry.file_type().await?.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let control = entry.path();
        let pinned = tokio::fs::metadata(control.join("pin"))
            .await
            .is_ok_and(|metadata| metadata.nlink() > 1);
        if pinned {
            records.insert(name);
            continue;
        }
        // Legacy records carry inline contents or an older format; current
        // records need contents bound to their pool directory.
        let ready = tokio::fs::read(control.join("ready.json"))
            .await
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Ready>(&bytes).ok());
        let contents = tokio::fs::symlink_metadata(bases.contents(&name))
            .await
            .ok();
        let current = valid_key(&name)
            && !control.join("contents").exists()
            && ready.is_some_and(|ready| {
                ready.inputs.format == FORMAT
                    && ready.key == name
                    && contents.as_ref().is_some_and(|contents| {
                        contents.is_dir()
                            && !contents.file_type().is_symlink()
                            && contents.ino() == ready.contents_inode
                    })
            });
        if current {
            records.insert(name);
        } else {
            bases.remove_entry(&name).await?;
        }
    }

    let mut entries = match tokio::fs::read_dir(bases.pool()).await {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    let mut removed = false;
    while let Some(entry) = entries.next_entry().await? {
        let name = entry.file_name().to_string_lossy().into_owned();
        let interrupted = name.starts_with(BUILD_PREFIX) || name.starts_with(PROBE_PREFIX);
        let orphan = valid_key(&name) && !records.contains(&name);
        if !interrupted && !orphan {
            continue;
        }
        if entry.file_type().await?.is_dir() {
            tokio::fs::remove_dir_all(entry.path()).await?;
        } else {
            tokio::fs::remove_file(entry.path()).await?;
        }
        removed = true;
    }
    if removed {
        tokio::fs::File::open(bases.pool())
            .await?
            .sync_all()
            .await?;
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
    async fn cache_publication_exposes_only_fully_sealed_contents() {
        let fixture = tempfile::tempdir().unwrap();
        let bases = Bases {
            data: fixture.path().join("data"),
            state: fixture.path().join("state"),
            identity: (unsafe { libc::geteuid() }, unsafe { libc::getegid() }),
            build_entered: None,
        };
        let archive = Archive {
            project_id: crate::config::id(),
            revision: "a".repeat(40),
            origin_hash: None,
            key: "b".repeat(64),
            hash: "c".repeat(64),
            bytes: 1,
            logical_bytes: 5,
        };
        let inputs = Inputs::new(std::slice::from_ref(&archive), 128, bases.identity).unwrap();
        let id = crate::config::id();
        let control = bases.state.join("environments").join(&id);
        let contents = bases.pool().join(format!("{BUILD_PREFIX}{id}"));
        private_dir(&control).await.unwrap();
        private_dir(&contents).await.unwrap();
        let ready = Ready {
            key: inputs.key().unwrap(),
            metadata_bytes: CLONE_RESERVE + 16384,
            allocated_bytes: 16384,
            contents_inode: std::fs::metadata(&contents).unwrap().ino(),
            inputs,
        };
        let fifo =
            std::ffi::CString::new(contents.join("unsupported").as_os_str().as_encoded_bytes())
                .unwrap();
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
        assert!(bases.publish(&control, &contents, &ready).await.is_err());
        assert!(
            !bases.root().join(&ready.key).exists() && !bases.contents(&ready.key).exists(),
            "An unsealed candidate must never become visible"
        );
        assert!(
            bases
                .lookup(std::slice::from_ref(&archive), 128)
                .await
                .unwrap()
                .is_none()
        );
        std::fs::remove_file(contents.join("unsupported")).unwrap();
        for name in ["home", "workspaces", "output"] {
            private_dir(&contents.join(name)).await.unwrap();
        }
        std::fs::write(contents.join("workspaces/file"), b"hello").unwrap();
        bases.publish(&control, &contents, &ready).await.unwrap();
        assert!(!control.exists() && !contents.exists());
        let source = bases.root().join(&ready.key);
        for _ in 0..3 {
            assert!(source.join("contents.digest").is_file());
            assert!(!source.join("contents").exists());
            assert_eq!(
                std::fs::read(bases.contents(&ready.key).join("workspaces/file")).unwrap(),
                b"hello"
            );
            let found = bases
                .lookup(std::slice::from_ref(&archive), 128)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(found.allocated_bytes, 16384);
            assert!(
                source.join("ready.json").is_file(),
                "Lookup must not invalidate a complete publication"
            );
        }

        // A record without its contents, or with contents from another
        // build, is never a base.
        let published = bases.contents(&ready.key);
        let moved = bases.pool().join("moved");
        std::fs::rename(&published, &moved).unwrap();
        assert!(
            bases
                .lookup(std::slice::from_ref(&archive), 128)
                .await
                .unwrap()
                .is_none()
        );
        private_dir(&published).await.unwrap();
        assert!(
            bases
                .lookup(std::slice::from_ref(&archive), 128)
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn large_verified_directory_payloads_remain_discoverable_and_do_not_rebuild() {
        let fixture = tempfile::tempdir().unwrap();
        let bases = Bases {
            data: fixture.path().join("data"),
            state: fixture.path().join("state"),
            identity: (unsafe { libc::geteuid() }, unsafe { libc::getegid() }),
            build_entered: None,
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
        let contents = bases.contents(&key);
        private_dir(&root).await.unwrap();
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
            allocated_bytes: workspace::measure(&contents, true).unwrap(),
            contents_inode: std::fs::metadata(&contents).unwrap().ino(),
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
