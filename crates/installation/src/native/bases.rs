//! Disposable local ext4 bases. Workloads receive privately owned journals;
//! only offline, sealed payload segments share inodes with the cache.
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
            format: 1,
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

struct Unclaimed {
    path: PathBuf,
}

impl Drop for Unclaimed {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
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
            || ready.metadata_bytes > 256 * disk::MIB
            || !root.join("pin").is_file()
            || !storage::runtime::exists(&root)
        {
            return Ok(None);
        }
        Ok(Some((root, ready)))
    }

    pub async fn lookup(&self, archives: &[Archive], disk_mib: u64) -> Result<Option<Ready>> {
        let Some((root, ready)) = self.stored(archives, disk_mib).await? else {
            return Ok(None);
        };
        let source = root.join("lazy");
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
        let metadata_bytes = tokio::fs::metadata(root.join("lazy/journal-v2.sqlite"))
            .await?
            .len()
            .saturating_add(CLONE_RESERVE);
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
        policy.validate()?;
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
        let budget = cache_limit(limit);
        let metadata = disk_mib.saturating_mul(disk::MIB) / 32 + 256 * disk::MIB;
        // Extraction, raw ext4 and imported journal coexist during building.
        // Include rounded tiny-file blocks, not only sum(st_size).
        let required = expanded
            .saturating_mul(3)
            .saturating_add(metadata.saturating_mul(2));
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
        // Old controllers already recognize and reclaim unclaimed environments;
        // sparse raw images therefore retain correct accounting on interruption.
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
        let metadata_bytes = tokio::fs::metadata(temporary.join("lazy/journal-v2.sqlite"))
            .await?
            .len()
            .saturating_add(CLONE_RESERVE);
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
        remember_verified(&target.join("lazy"), base_signature(&target.join("lazy"))?)?;
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
        storage::bootstrap::prepare_native_base(
            temporary,
            inputs.disk_mib * disk::MIB,
            &contents,
            (inputs.uid, inputs.gid),
            policy,
            stop,
        )
        .await?;
        tokio::fs::remove_dir_all(contents).await?;
        Ok(())
    }

    pub async fn initialize(&self, plan: &Value, stop: &CancellationToken) -> Result<bool> {
        let run = text(plan, "runId");
        let directory = environment::directory(&self.state, run)?;
        let root = workspace::directory(&self.state, run)?;
        if storage::runtime::exists(&directory)
            || root.join("initialized").exists()
            || root.join("data").exists()
        {
            return Ok(false);
        }
        let Some(values) = plan["nativeProjectArchives"].as_array() else {
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
            if plan.get("nativeProjectBase").is_some() {
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
        for name in [
            "lazy",
            "data.ext4",
            "bootstrap.pending",
            "resize-source",
            "restore.pending",
        ] {
            if lease.directory.join(name).exists() {
                return Ok(false);
            }
        }
        let physical = self.state.join("environments").join(crate::config::id());
        touch_pin(&self.root().join(&ready.key).join("pin"));
        let (source, target, context) = (
            self.root().join(&ready.key).join("lazy"),
            physical.clone(),
            plan["storage"].clone(),
        );
        let runtime = json!({
            "backend": "native",
            "storage": "ublk",
            "os": "linux",
            "arch": "x86_64",
            "runtimeId": plan["runtimeId"]
        });
        let handle = tokio::runtime::Handle::current();
        let unclaimed = tokio::task::spawn_blocking(move || -> Result<Unclaimed> {
            use std::os::unix::fs::DirBuilderExt;
            std::fs::DirBuilder::new().mode(0o700).create(&target)?;
            let guard = Unclaimed {
                path: target.clone(),
            };
            handle.block_on(storage::runtime::clone_native_base(
                &source,
                &target.join("lazy"),
                &context,
            ))?;
            std::fs::File::open(&target)?.sync_all()?;
            std::fs::File::open(target.parent().unwrap())?.sync_all()?;
            Ok(guard)
        })
        .await
        .map_err(Error::internal)??;
        // Publish directly into the conventional native disk.
        // Runtime identity is durable before
        // the journal rename commits; before then this is an ordinary empty disk.
        let runtime_path = lease.directory.join("runtime.json");
        match tokio::fs::read(&runtime_path).await {
            Ok(bytes) if serde_json::from_slice::<Value>(&bytes)? != runtime => {
                return Err(Error::conflict("Native disk runtime changed."));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        atomic_write(&runtime_path, &serde_json::to_vec(&runtime)?).await?;
        tokio::fs::File::open(&lease.directory)
            .await?
            .sync_all()
            .await?;
        tokio::fs::File::open(lease.directory.parent().unwrap())
            .await?
            .sync_all()
            .await?;
        tokio::fs::rename(physical.join("lazy"), lease.directory.join("lazy")).await?;
        tokio::fs::File::open(&lease.directory)
            .await?
            .sync_all()
            .await?;
        drop(unclaimed);
        // Existing block initialization recognizes the populated filesystem's
        // .cairn-seed-complete marker even if interrupted before these hints.
        private_dir(&root).await?;
        atomic_write(
            &root.join("file-owner.json"),
            &serde_json::to_vec(&json!({ "uid": ready.inputs.uid, "gid": ready.inputs.gid }))?,
        )
        .await?;
        release_pin(&self.data.join("runs").join(run), text(plan, "id")).await?;
        drop(lease);
        Ok(true)
    }
}

fn verified_receipts() -> &'static std::sync::Mutex<std::collections::HashMap<PathBuf, String>> {
    static RECEIPTS: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<PathBuf, String>>,
    > = std::sync::OnceLock::new();
    RECEIPTS.get_or_init(Default::default)
}

fn base_signature(root: &Path) -> std::io::Result<String> {
    if std::fs::metadata(root.join("journal-v2.sqlite-wal"))
        .is_ok_and(|metadata| metadata.len() != 0)
    {
        return Err(std::io::Error::other(
            "Native base has an active metadata journal.",
        ));
    }
    let mut paths = Vec::new();
    for entry in std::fs::read_dir(root)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name == "journal-v2.sqlite" || name.starts_with("payload-") {
            paths.push((name, entry.path()));
        }
    }
    paths.sort();
    let mut hash = blake3::Hasher::new();
    for (name, path) in paths {
        let metadata = std::fs::symlink_metadata(path)?;
        if !metadata.is_file() {
            return Err(std::io::Error::other("Invalid native base file."));
        }
        hash.update(name.as_bytes());
        for value in [
            metadata.dev(),
            metadata.ino(),
            metadata.len(),
            metadata.mtime() as u64,
            metadata.mtime_nsec() as u64,
            metadata.mode() as u64,
        ] {
            hash.update(&value.to_le_bytes());
        }
    }
    Ok(hash.finalize().to_hex().to_string())
}

fn remember_verified(root: &Path, signature: String) -> std::io::Result<()> {
    let mut receipts = verified_receipts()
        .lock()
        .map_err(|_| std::io::Error::other("Native base verification registry is poisoned."))?;
    if receipts.len() >= 128 {
        receipts.clear();
    }
    receipts.insert(root.to_owned(), signature);
    Ok(())
}

fn verify_cached_base(root: &Path) -> std::io::Result<()> {
    let signature = base_signature(root)?;
    if verified_receipts()
        .lock()
        .map_err(|_| std::io::Error::other("Native base verification registry is poisoned."))?
        .get(root)
        == Some(&signature)
    {
        return Ok(());
    }
    storage::LazyDisk::verify_sealed(root)?;
    let checked = base_signature(root)?;
    if checked != signature {
        return Err(std::io::Error::other(
            "Native base changed during verification.",
        ));
    }
    remember_verified(root, checked)
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
