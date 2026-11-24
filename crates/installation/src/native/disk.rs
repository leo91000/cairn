//! One logical disk pool includes retained conversations, archives and transfer
//! staging. Controller and connector mutations share the same process-safe lock.
use crate::error::{Error, Result};
use std::{
    io::Write,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

pub(crate) const MIB: u64 = 1_048_576;
pub(crate) const SEED_TEMP_PREFIX: &str = ".cairn-seed-";

/// Staging and retirement share the run's lock across the connector and the
/// controller. Acquisition is nonblocking under the node-wide storage gate.
pub(crate) async fn lock_staging(state: &Path, run: &str) -> Result<crate::file_lock::Guard> {
    crate::validation::uuid(run)?;
    let locks = state.join("native-staging-locks");
    crate::skills::private_dir(&locks).await?;
    crate::file_lock::exclusive(&locks.join(run), "Native workspace staging is busy.")
}

const METADATA_RESERVE: u64 = MIB;

pub(crate) fn available(limit: u64, used: u64) -> u64 {
    limit.saturating_sub(used).saturating_sub(METADATA_RESERVE)
}

pub(super) fn data_pool_headroom(
    state: &Path,
    fallback: &crate::storage::policy::Policy,
    own_reservation: Option<&Path>,
) -> Result<u64> {
    physical_headroom(
        state,
        &super::workspace::data_root(state),
        fallback,
        own_reservation,
    )
}

pub(super) fn physical_headroom(
    state: &Path,
    filesystem: &Path,
    fallback: &crate::storage::policy::Policy,
    own_reservation: Option<&Path>,
) -> Result<u64> {
    let policy = match std::fs::read(state.join("storage-policy.json")) {
        Ok(bytes) => crate::storage::policy::from_persisted(&bytes)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => fallback.clone(),
        Err(error) => return Err(error.into()),
    };
    policy.validate().map_err(Error::bad)?;
    // Other preparations may allocate before the next physical sample.
    let peers = physical_preparations(state, filesystem, own_reservation)?;
    let (total, free) = crate::storage::policy::physical_space(filesystem)?;
    Ok(free
        .saturating_sub(policy.reserve(total))
        .saturating_sub(crate::storage::cache::pending_write_bytes())
        .saturating_sub(peers))
}

pub(crate) async fn bytes_if_present(path: &Path) -> Result<u64> {
    if !tokio::fs::try_exists(path).await? {
        return Ok(0);
    }
    super::workspace::bytes(path).await
}

pub(crate) async fn used(data: &Path, state: &Path) -> Result<u64> {
    let (data, state) = (data.to_owned(), state.to_owned());
    tokio::task::spawn_blocking(move || usage_sync(&data, &state))
        .await
        .map_err(Error::internal)?
}

pub(crate) async fn actual_used(data: &Path, state: &Path) -> Result<u64> {
    let (data, state) = (data.to_owned(), state.to_owned());
    tokio::task::spawn_blocking(move || measure_usage(&data, &state, false, None))
        .await
        .map_err(Error::internal)?
}

/// Admission includes reserved incoming bytes; supervision uses only bytes
/// already materialized, so a copy cannot be charged twice and stop a peer.
pub(crate) fn usage_sync(data: &Path, state: &Path) -> Result<u64> {
    measure_usage(data, state, true, None)
}

pub(super) fn usage_except(data: &Path, state: &Path, preparation: &Path) -> Result<u64> {
    measure_usage(data, state, true, Some(preparation))
}

static USAGE_SCANS: AtomicU64 = AtomicU64::new(0);

/// Full usage scans performed by this process, for bootstrap observations.
pub(crate) fn usage_scans() -> u64 {
    USAGE_SCANS.load(Ordering::Relaxed)
}

/// Tests share one process: count scans per controller state as well.
#[cfg(test)]
fn scans_by_state() -> &'static std::sync::Mutex<std::collections::HashMap<std::path::PathBuf, u64>>
{
    static SCANS: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<std::path::PathBuf, u64>>,
    > = std::sync::OnceLock::new();
    SCANS.get_or_init(Default::default)
}

#[cfg(test)]
pub(crate) fn usage_scans_of(state: &Path) -> u64 {
    scans_by_state()
        .lock()
        .unwrap()
        .get(state)
        .copied()
        .unwrap_or(0)
}

/// Count file-backed staging conservatively, but charge journal/cache blocks
/// by allocated bytes. Mounted views refer to those same blocks and add no charge.
/// Base contents live on the data pool and are counted there; `native-bases`
/// holds only their small control records.
fn measure_usage(
    data: &Path,
    state: &Path,
    include_reservations: bool,
    except: Option<&Path>,
) -> Result<u64> {
    fn size(
        path: &Path,
        physical: bool,
        seen: &mut std::collections::HashSet<(u64, u64)>,
    ) -> Result<u64> {
        use std::os::unix::fs::MetadataExt;
        match std::fs::symlink_metadata(path) {
            Ok(metadata) if metadata.is_dir() => {
                super::workspace::measure_unique(path, physical, seen)
            }
            Ok(metadata) if metadata.is_file() && seen.insert((metadata.dev(), metadata.ino())) => {
                Ok(if physical {
                    metadata.blocks().saturating_mul(512)
                } else {
                    metadata.len()
                })
            }
            Ok(_) => Ok(0),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(0),
            Err(error) => Err(error.into()),
        }
    }
    use std::os::unix::fs::MetadataExt;
    USAGE_SCANS.fetch_add(1, Ordering::Relaxed);
    #[cfg(test)]
    {
        *scans_by_state()
            .lock()
            .unwrap()
            .entry(state.to_owned())
            .or_default() += 1;
    }
    let mut seen = std::collections::HashSet::new();
    let mut used = 0u64;
    if include_reservations {
        for (path, reservation) in preparations(state)? {
            let materialized = match &reservation.staging {
                Some(staging) => {
                    staging.validate_location(&data.join("runs"))?;
                    size(&staging.path, false, &mut seen)?
                }
                None => 0,
            };
            // Charge the same observed inode bytes exactly once. The normal
            // inventory shares `seen` with this sample and therefore skips
            // bytes already covered by an in-flight transfer's commitment.
            let outstanding = if except == Some(path.as_path()) {
                0
            } else {
                reservation.bytes.saturating_sub(materialized)
            };
            used = used
                .saturating_add(materialized)
                .saturating_add(outstanding);
        }
    }
    used = used.saturating_add(size(data, false, &mut seen)?);
    for entry in std::fs::read_dir(state)? {
        let entry = entry?;
        let name = entry.file_name();
        if name == "native-data" && entry.metadata()?.dev() != std::fs::metadata(state)?.dev() {
            // The dedicated data filesystem accounts directories, deleted-open
            // inodes and all project payloads without rescanning every file.
            let filesystem = super::workspace::filesystem(&entry.path())?;
            used = used.saturating_add(
                filesystem
                    .f_blocks
                    .saturating_sub(filesystem.f_bfree)
                    .saturating_mul(filesystem.f_frsize),
            );
        } else if name == "roots" {
            for plan in std::fs::read_dir(entry.path())? {
                let plan = plan?;
                if plan.file_type()?.is_file() {
                    used = used.saturating_add(size(&plan.path(), false, &mut seen)?);
                }
            }
        } else if name == "native" {
            for conversation in std::fs::read_dir(entry.path())? {
                let conversation = conversation?;
                if !conversation.file_type()?.is_dir() {
                    continue;
                }
                let root = conversation.path();
                let block = !root.join("directory.json").exists()
                    && (root.join("block-initialized").exists()
                        || root.join("block-seeding").exists());
                if block {
                    for file in std::fs::read_dir(&root)? {
                        let file = file?;
                        if file.file_name() != "data" {
                            used = used.saturating_add(size(&file.path(), false, &mut seen)?);
                        }
                    }
                } else {
                    used = used.saturating_add(size(
                        &root,
                        root.join("directory.json").exists(),
                        &mut seen,
                    )?);
                }
            }
        } else if name == "native-preparations" {
            continue;
        } else {
            let physical = name == "native-bases"
                || name == "native-data"
                || super::super::storage::environment::ROOTS
                    .iter()
                    .any(|root| name == *root);
            used = used.saturating_add(size(&entry.path(), physical, &mut seen)?);
        }
    }
    Ok(used)
}

/// Incoming bytes remain charged while the copy's physical worker is alive.
pub(crate) struct Preparation {
    path: PathBuf,
    lease: PathBuf,
    _owner: crate::file_lock::Guard,
    pub bytes: u64,
    staging: Option<StagingReceipt>,
}

#[derive(serde::Deserialize, serde::Serialize)]
struct PreparationRecord {
    bytes: u64,
    physical: Option<PhysicalClaim>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    staging: Option<StagingReceipt>,
}

#[derive(serde::Deserialize, serde::Serialize)]
struct StagingReceipt {
    path: PathBuf,
    device: u64,
    inode: u64,
}

impl StagingReceipt {
    fn validate_location(&self, parent: &Path) -> Result<()> {
        if self.path.parent() != Some(parent)
            || !self
                .path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with(SEED_TEMP_PREFIX))
        {
            return Err(Error::conflict("Invalid native staging reservation path."));
        }
        Ok(())
    }

    fn remove(&self, parent: &Path) -> Result<()> {
        self.validate_location(parent)?;
        let metadata = match std::fs::symlink_metadata(&self.path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        if !metadata.is_dir()
            || metadata.dev() != self.device
            || metadata.ino() != self.inode
            || std::fs::canonicalize(parent)? != parent
        {
            return Err(Error::conflict(
                "The native staging tree no longer belongs to its reservation.",
            ));
        }
        super::workspace::require_no_mounts(&self.path)?;
        std::fs::remove_dir_all(&self.path)?;
        std::fs::File::open(parent)?.sync_all()?;
        Ok(())
    }
}

#[derive(serde::Deserialize, serde::Serialize)]
struct PhysicalClaim {
    device: u64,
    remaining: u64,
}

fn read_preparation(text: &str) -> Result<PreparationRecord> {
    if let Ok(bytes) = text.parse::<u64>() {
        return Ok(PreparationRecord {
            bytes,
            physical: None,
            staging: None,
        });
    }
    Ok(serde_json::from_str(text)?)
}

pub(super) struct PreparationProgress {
    pub(super) path: PathBuf,
    initial: u64,
}

impl PreparationProgress {
    pub(super) fn credit(&self, consumed: u64) -> Result<()> {
        let mut record = read_preparation(&std::fs::read_to_string(&self.path)?)?;
        let Some(claim) = &mut record.physical else {
            return Ok(());
        };
        let remaining = self.initial.saturating_sub(consumed);
        if remaining >= claim.remaining {
            return Ok(());
        }
        claim.remaining = remaining;
        // Progress is only needed by concurrent writers. A crash reaps the
        // owner's reservation, so an atomic replacement needs no flush here.
        let mut file = tempfile::NamedTempFile::new_in(self.path.parent().unwrap())?;
        file.write_all(&serde_json::to_vec(&record)?)?;
        file.persist(&self.path).map_err(|error| error.error)?;
        Ok(())
    }
}

impl Preparation {
    #[cfg(test)]
    pub async fn reserve(state: &Path, attempt: &str, bytes: u64) -> Result<Self> {
        Self::publish(
            state,
            attempt,
            PreparationRecord {
                bytes,
                physical: None,
                staging: None,
            },
        )
        .await
    }

    /// Caller serializes reservation publication with the storage pool gate.
    pub(crate) async fn reserve_on(
        state: &Path,
        attempt: &str,
        bytes: u64,
        filesystem: &Path,
    ) -> Result<Self> {
        Self::reserve_target(state, attempt, bytes, filesystem, None).await
    }

    pub(crate) async fn reserve_staging(
        state: &Path,
        attempt: &str,
        bytes: u64,
        data: &Path,
        staging: &Path,
    ) -> Result<Self> {
        let metadata = std::fs::symlink_metadata(staging)?;
        if !metadata.is_dir() {
            return Err(Error::bad("Invalid native staging reservation path."));
        }
        let receipt = StagingReceipt {
            path: staging.to_owned(),
            device: metadata.dev(),
            inode: metadata.ino(),
        };
        receipt.validate_location(&data.join("runs"))?;
        Self::reserve_target(state, attempt, bytes, data, Some(receipt)).await
    }

    async fn reserve_target(
        state: &Path,
        attempt: &str,
        bytes: u64,
        filesystem: &Path,
        staging: Option<StagingReceipt>,
    ) -> Result<Self> {
        let device = std::fs::metadata(filesystem)?.dev();
        Self::publish(
            state,
            attempt,
            PreparationRecord {
                bytes,
                physical: Some(PhysicalClaim {
                    device,
                    remaining: bytes,
                }),
                staging,
            },
        )
        .await
    }

    async fn publish(state: &Path, attempt: &str, record: PreparationRecord) -> Result<Self> {
        crate::validation::uuid(attempt)?;
        let root = state.join("native-preparations");
        crate::skills::private_dir(&root).await?;
        let path = root.join(attempt);
        let lease = root.join(format!("{attempt}.lease"));
        let owner = crate::file_lock::exclusive(&lease, "Native preparation is already reserved.")?;
        if path.try_exists()? {
            return Err(Error::conflict("Native preparation is already reserved."));
        }
        super::atomic_write(&path, &serde_json::to_vec(&record)?).await?;
        Ok(Self {
            path,
            lease,
            _owner: owner,
            bytes: record.bytes,
            staging: record.staging,
        })
    }

    pub(super) fn progress(&self) -> Result<PreparationProgress> {
        let record = read_preparation(&std::fs::read_to_string(&self.path)?)?;
        Ok(PreparationProgress {
            path: self.path.clone(),
            initial: record.physical.map_or(0, |claim| claim.remaining),
        })
    }
}

impl Drop for Preparation {
    fn drop(&mut self) {
        if let Some(staging) = &self.staging
            && let Err(error) = staging.remove(staging.path.parent().expect("owned staging parent"))
        {
            tracing::error!(%error, "Native staging cleanup deferred with its reservation");
            return;
        }
        if let Err(error) = std::fs::remove_file(&self.path)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            tracing::error!(%error, "Native preparation reservation remains retained");
        }
        if let Err(error) = std::fs::remove_file(&self.lease)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            tracing::error!(%error, "Native preparation reservation lease remains retained");
        }
    }
}

pub(super) fn physical_preparations(
    state: &Path,
    filesystem: &Path,
    except: Option<&Path>,
) -> Result<u64> {
    let device = std::fs::metadata(filesystem)?.dev();
    sum_preparations(state, except, |record| {
        record
            .physical
            .filter(|claim| claim.device == device)
            .map_or(0, |claim| claim.remaining)
    })
}

/// A bounded seed may have materialized data since its last receipt update.
/// Use only exclusive, already allocated bytes from the recorded inode.
pub(super) fn reconciled_physical_preparations(
    state: &Path,
    filesystem: &Path,
    except: Option<&Path>,
) -> Result<u64> {
    let device = std::fs::metadata(filesystem)?.dev();
    let mut bytes = 0_u64;
    for (path, record) in preparations(state)? {
        if except == Some(path.as_path()) {
            continue;
        }
        let Some(claim) = record.physical.filter(|claim| claim.device == device) else {
            continue;
        };
        let remaining = if let Some(staged) = record.staging {
            let owned = std::fs::symlink_metadata(&staged.path).is_ok_and(|metadata| {
                metadata.is_dir()
                    && metadata.dev() == staged.device
                    && metadata.ino() == staged.inode
            });
            if owned {
                claim.remaining.min(
                    record
                        .bytes
                        .saturating_sub(super::workspace::credit_bytes(&staged.path)?),
                )
            } else {
                claim.remaining
            }
        } else {
            claim.remaining
        };
        bytes = bytes.saturating_add(remaining);
    }
    Ok(bytes)
}

#[cfg(test)]
fn preparation_bytes(state: &Path) -> Result<u64> {
    sum_preparations(state, None, |record| record.bytes)
}

fn sum_preparations(
    state: &Path,
    except: Option<&Path>,
    bytes_for: impl Fn(PreparationRecord) -> u64,
) -> Result<u64> {
    Ok(preparations(state)?
        .into_iter()
        .filter(|(path, _)| except != Some(path.as_path()))
        .fold(0u64, |bytes, (_, record)| {
            bytes.saturating_add(bytes_for(record))
        }))
}

fn preparations(state: &Path) -> Result<Vec<(PathBuf, PreparationRecord)>> {
    let entries = match std::fs::read_dir(state.join("native-preparations")) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let mut records = Vec::new();
    for entry in entries {
        let entry = entry?;
        if !entry
            .file_name()
            .to_str()
            .is_some_and(|name| crate::validation::uuid(name).is_ok())
        {
            continue;
        }
        match entry.file_type() {
            Ok(kind) if kind.is_file() => {}
            Ok(_) => return Err(Error::conflict("Invalid native preparation reservation.")),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        }
        let path = entry.path();
        match std::fs::read_to_string(&path) {
            Ok(amount) => records.push((path, read_preparation(&amount)?)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(records)
}

pub(super) fn recover_preparations(
    data: &Path,
    state: &Path,
    pending: &std::collections::HashMap<String, String>,
) -> Result<()> {
    let root = state.join("native-preparations");
    let entries = match std::fs::read_dir(&root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        let Some(attempt) = name.to_str() else {
            continue;
        };
        let attempt = attempt.strip_suffix(".lease").unwrap_or(attempt);
        if crate::validation::uuid(attempt).is_err() {
            continue;
        }
        if pending.values().any(|id| id == attempt) {
            continue;
        }
        let lease = root.join(format!("{attempt}.lease"));
        let _owner = match crate::file_lock::exclusive(&lease, "Native preparation is active.") {
            Ok(owner) => owner,
            Err(error) if error.is_conflict() => continue,
            Err(error) => return Err(error),
        };
        let receipt = root.join(attempt);
        match std::fs::read_to_string(&receipt) {
            Ok(bytes) => {
                if let Some(staging) = read_preparation(&bytes)?.staging {
                    let parent = data.join("runs");
                    staging.remove(&parent)?;
                    match std::fs::File::open(&parent) {
                        Ok(directory) => directory.sync_all()?,
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                        Err(error) => return Err(error.into()),
                    }
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        for path in [receipt, lease] {
            match std::fs::remove_file(path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
    }
    std::fs::File::open(root)?.sync_all()?;
    Ok(())
}

/// Controller ownership is already exclusive at startup. Only this process's
/// abandoned artifact copies can be removed; unrelated entries are preserved.
pub(crate) async fn recover_exports(state: &Path) -> Result<()> {
    let exports = state.join("exports");
    crate::skills::private_dir(&exports).await?;
    let mut entries = tokio::fs::read_dir(exports).await?;
    while let Some(entry) = entries.next_entry().await? {
        if entry
            .file_name()
            .to_string_lossy()
            .starts_with("native-artifact-")
            && entry.file_type().await?.is_file()
        {
            tokio::fs::remove_file(entry.path()).await?;
        }
    }
    Ok(())
}

pub(crate) struct StorageGuard {
    _storage: crate::file_lock::Guard,
    _priority: crate::file_lock::Guard,
}

pub(crate) async fn lock(state: &Path) -> Result<StorageGuard> {
    tokio::time::timeout(Duration::from_secs(300), async {
        // Every foreground waiter holds shared intent, across processes. The
        // warmer probes this lock and cancels its network transfer promptly.
        let priority = loop {
            match crate::file_lock::shared(
                &state.join("storage-foreground.lock"),
                "Storage priority probe in progress.",
            ) {
                Ok(guard) => break guard,
                Err(error) if error.status == 409 => {
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
                Err(error) => return Err(error),
            }
        };
        loop {
            match crate::file_lock::exclusive(
                &state.join("storage.lock"),
                "Another native storage operation is in progress.",
            ) {
                Ok(owner) => {
                    return Ok(StorageGuard {
                        _storage: owner,
                        _priority: priority,
                    });
                }
                Err(error) if error.status == 409 => {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
                Err(error) => return Err(error),
            }
        }
    })
    .await
    .map_err(|_| {
        Error::unavailable("Native storage operation did not finish before its deadline.")
    })?
}

/// Called with the storage lock held, before receiving a temporary seed. A
/// missing or incompatible controller cannot authorize unbounded staging.
pub(crate) async fn staging_budget() -> Result<u64> {
    Ok(staging_sample().await?.available_bytes)
}

#[derive(serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct StagingSample {
    pub available_bytes: u64,
    pub used_bytes: Option<u64>,
}

pub(crate) async fn staging_sample() -> Result<StagingSample> {
    staging_sample_from(&crate::nodes::local_client()?, "http://localhost").await
}

#[cfg(test)]
pub(super) async fn staging_budget_from(client: &reqwest::Client, origin: &str) -> Result<u64> {
    Ok(staging_sample_from(client, origin).await?.available_bytes)
}

async fn staging_sample_from(client: &reqwest::Client, origin: &str) -> Result<StagingSample> {
    let started = std::time::Instant::now();
    let response = client
        .get(format!("{origin}/staging-budget"))
        .timeout(Duration::from_secs(30))
        .send()
        .await
        .map_err(|error| {
            tracing::warn!(
                target: "cairn_performance",
                operation = "native_staging_budget",
                elapsed_ms = started.elapsed().as_millis() as u64,
                timeout = error.is_timeout(),
                connect = error.is_connect(),
                "Native staging budget request failed"
            );
            Error::internal(error)
        })?;
    if !response.status().is_success() {
        tracing::warn!(
            target: "cairn_performance",
            operation = "native_staging_budget",
            elapsed_ms = started.elapsed().as_millis() as u64,
            status = response.status().as_u16(),
            "Native staging budget request rejected"
        );
        return Err(Error::unavailable("Native disk budget is unavailable."));
    }
    response.json().await.map_err(Error::internal)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn recovery_removes_only_the_recorded_unpublished_seed_inode() {
        let data = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let runs = data.path().join("runs");
        let receipts = state.path().join("native-preparations");
        std::fs::create_dir(&runs).unwrap();
        std::fs::create_dir(&receipts).unwrap();
        let mut published = None;
        for should_publish in [false, true] {
            let temp = tempfile::Builder::new()
                .prefix(SEED_TEMP_PREFIX)
                .tempdir_in(&runs)
                .unwrap()
                .keep();
            std::fs::write(temp.join("seed"), b"staging data").unwrap();
            let metadata = temp.metadata().unwrap();
            let record = PreparationRecord {
                bytes: MIB,
                physical: None,
                staging: Some(StagingReceipt {
                    path: temp.clone(),
                    device: metadata.dev(),
                    inode: metadata.ino(),
                }),
            };
            std::fs::write(
                receipts.join(crate::config::id()),
                serde_json::to_vec(&record).unwrap(),
            )
            .unwrap();
            if should_publish {
                let target = runs.join(crate::config::id());
                std::fs::rename(&temp, &target).unwrap();
                published = Some(target);
            }
            recover_preparations(data.path(), state.path(), &std::collections::HashMap::new())
                .unwrap();
            assert!(!temp.exists());
            assert_eq!(std::fs::read_dir(&receipts).unwrap().count(), 0);
        }
        assert_eq!(
            std::fs::read(published.unwrap().join("seed")).unwrap(),
            b"staging data"
        );
    }

    #[tokio::test]
    async fn physical_claims_charge_only_unspent_capacity_on_the_same_filesystem() {
        let state = tempfile::tempdir().unwrap();
        let path = state.path();
        let first = Preparation::reserve_on(path, &crate::config::id(), 40 * MIB, path)
            .await
            .unwrap();
        let second = Preparation::reserve_on(path, &crate::config::id(), 40 * MIB, path)
            .await
            .unwrap();
        let foreign = Preparation::publish(
            path,
            &crate::config::id(),
            PreparationRecord {
                bytes: 40 * MIB,
                physical: Some(PhysicalClaim {
                    device: std::fs::metadata(path).unwrap().dev().wrapping_add(1),
                    remaining: 40 * MIB,
                }),
                staging: None,
            },
        )
        .await
        .unwrap();

        let writer = first.progress().unwrap();
        assert_eq!(
            physical_preparations(path, path, Some(&writer.path)).unwrap(),
            40 * MIB
        );
        second.progress().unwrap().credit(25 * MIB).unwrap();
        writer.credit(25 * MIB).unwrap();
        assert_eq!(
            physical_preparations(path, path, Some(&writer.path)).unwrap(),
            15 * MIB
        );
        assert_eq!(physical_preparations(path, path, None).unwrap(), 30 * MIB);
        assert_eq!(preparation_bytes(path).unwrap(), 120 * MIB);
        drop((first, second, foreign));
    }

    #[tokio::test]
    async fn recovery_keeps_another_process_staging_reservation_until_its_lease_settles() {
        let state = tempfile::tempdir().unwrap();
        let attempt = crate::config::id();
        let protected = Preparation::reserve(state.path(), &attempt, 42)
            .await
            .unwrap();
        let stale = crate::config::id();
        std::fs::write(state.path().join("native-preparations").join(&stale), b"10").unwrap();

        recover_preparations(
            state.path(),
            state.path(),
            &std::collections::HashMap::new(),
        )
        .unwrap();
        assert_eq!(preparation_bytes(state.path()).unwrap(), 42);
        drop(protected);
        assert_eq!(preparation_bytes(state.path()).unwrap(), 0);
    }

    #[test]
    fn sparse_temporary_holes_never_reduce_future_host_space_promises() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("sparse");
        let file = std::fs::File::create(&path).unwrap();
        file.set_len(16 * MIB).unwrap();
        file.sync_all().unwrap();
        assert_eq!(
            super::super::workspace::credit_bytes(root.path()).unwrap(),
            0
        );
        std::fs::write(&path, vec![1; MIB as usize]).unwrap();
        std::fs::File::open(&path).unwrap().sync_all().unwrap();
        assert!(super::super::workspace::credit_bytes(root.path()).unwrap() > 0);
        std::fs::hard_link(&path, root.path().join("shared")).unwrap();
        assert_eq!(
            super::super::workspace::credit_bytes(root.path()).unwrap(),
            0
        );
    }

    #[test]
    fn archive_cache_and_conversation_pins_are_charged_once_per_inode() {
        let root = tempfile::tempdir().unwrap();
        let data = root.path().join("data");
        let state = root.path().join("state");
        std::fs::create_dir_all(data.join("project-archives")).unwrap();
        std::fs::create_dir_all(&state).unwrap();
        let archive = data.join("project-archives/seed.zip");
        std::fs::write(&archive, vec![1; 4096]).unwrap();
        for index in 0..4 {
            let run = data
                .join("runs")
                .join(index.to_string())
                .join("project-archives");
            std::fs::create_dir_all(&run).unwrap();
            std::fs::hard_link(&archive, run.join("seed.zip")).unwrap();
        }
        assert_eq!(usage_sync(&data, &state).unwrap(), 4096);
        std::fs::remove_file(archive).unwrap();
        assert_eq!(usage_sync(&data, &state).unwrap(), 4096);
    }

    #[tokio::test]
    async fn releasing_reservations_does_not_fail_unrelated_usage_inventory() {
        let state = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        let (started, wait) = std::sync::mpsc::channel();
        let path = state.path().to_owned();
        let worker = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(async move {
                let _ = started.send(());
                for _ in 0..500 {
                    let reservation = Preparation::reserve(&path, &crate::config::id(), 4096)
                        .await
                        .unwrap();
                    drop(reservation);
                }
            });
        });
        wait.recv().unwrap();
        for _ in 0..500 {
            usage_sync(data.path(), state.path()).unwrap();
        }
        worker.join().unwrap();
    }

    #[tokio::test]
    async fn materialized_copy_is_not_charged_against_its_incoming_reservation() {
        let state = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        let baseline = std::fs::File::create(data.path().join("existing")).unwrap();
        baseline.set_len(700 * MIB).unwrap();
        let reservation = Preparation::reserve(state.path(), &crate::config::id(), 323 * MIB)
            .await
            .unwrap();
        let copied = std::fs::File::create(data.path().join("copied")).unwrap();
        copied.set_len(10 * MIB).unwrap();
        assert_eq!(
            actual_used(data.path(), state.path()).await.unwrap(),
            710 * MIB
        );
        assert!(used(data.path(), state.path()).await.unwrap() > 1024 * MIB);
        assert_eq!(
            available(1024 * MIB, used(data.path(), state.path()).await.unwrap()),
            0
        );
        drop(reservation);
        assert_eq!(used(data.path(), state.path()).await.unwrap(), 710 * MIB);
    }

    #[tokio::test]
    async fn simultaneous_preparations_charge_incoming_bytes_until_their_copy_finishes() {
        let state = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        let first = Preparation::reserve(state.path(), &crate::config::id(), 700 * MIB)
            .await
            .unwrap();
        let charged = used(data.path(), state.path()).await.unwrap();
        assert!(charged >= 700 * MIB);
        assert!(available(1024 * MIB, charged) < 700 * MIB);
        drop(first);
        assert!(used(data.path(), state.path()).await.unwrap() < MIB);
    }

    #[tokio::test]
    async fn native_block_pool_counts_retained_journals_and_staging_without_scanning_the_mounted_view()
     {
        use std::os::unix::fs::MetadataExt;
        let fixture = tempfile::tempdir().unwrap();
        let (data, state) = (fixture.path().join("data"), fixture.path().join("state"));
        crate::skills::private_dir(&data).await.unwrap();
        crate::skills::private_dir(&state).await.unwrap();
        for _ in 0..2 {
            let run = crate::config::id();
            let journal = state.join("disks").join(&run).join("lazy");
            crate::skills::private_dir(&journal).await.unwrap();
            tokio::fs::write(journal.join("segment"), vec![1; 20 * MIB as usize])
                .await
                .unwrap();
            let root = super::super::workspace::directory(&state, &run).unwrap();
            crate::skills::private_dir(&root.join("data"))
                .await
                .unwrap();
            tokio::fs::write(root.join("block-initialized"), b"1")
                .await
                .unwrap();
            let view = root.join("data/virtual-model");
            tokio::fs::File::create(&view)
                .await
                .unwrap()
                .set_len(20 * 1024 * MIB)
                .await
                .unwrap();
            assert_eq!(std::fs::metadata(view).unwrap().blocks(), 0);
        }
        let retained = used(&data, &state).await.unwrap();
        assert!((40 * MIB..41 * MIB).contains(&retained));
        assert_eq!(
            available(32 * MIB, retained),
            0,
            "both retained conversations must be charged"
        );
        crate::skills::private_dir(&data.join("seed"))
            .await
            .unwrap();
        tokio::fs::write(data.join("seed/incoming"), vec![2; MIB as usize])
            .await
            .unwrap();
        assert!(used(&data, &state).await.unwrap() >= retained + MIB);
    }

    #[tokio::test]
    async fn concurrent_conversation_transfers_share_one_retained_disk_pool() {
        let root = tempfile::tempdir().unwrap();
        let data = root.path().join("data");
        let state = root.path().join("state");
        let source = root.path().join("remote-source");
        for directory in [&data, &state, &source] {
            crate::skills::private_dir(directory).await.unwrap();
        }
        tokio::fs::File::create(source.join("model"))
            .await
            .unwrap()
            .set_len(20 * MIB)
            .await
            .unwrap();
        let mut transfers = Vec::new();
        for index in 0..2 {
            let (data, state, source) = (data.clone(), state.clone(), source.clone());
            transfers.push(tokio::spawn(async move {
                let _owner = lock(&state).await.unwrap();
                let remaining = available(32 * MIB, used(&data, &state).await.unwrap());
                let receiving = tempfile::tempdir_in(&data).unwrap();
                let copied =
                    crate::native::workspace::copy(&source, receiving.path(), remaining).await;
                if copied.is_ok() {
                    tokio::fs::rename(receiving.path(), data.join(format!("conversation-{index}")))
                        .await
                        .unwrap();
                }
                copied
            }));
        }
        let mut completed = 0;
        for transfer in transfers {
            completed += usize::from(transfer.await.unwrap().is_ok());
        }
        assert_eq!(completed, 1);
        assert_eq!(used(&data, &state).await.unwrap(), 20 * MIB);
    }

    #[tokio::test]
    async fn export_recovery_removes_only_owned_abandoned_copies() {
        let state = tempfile::tempdir().unwrap();
        let exports = state.path().join("exports");
        crate::skills::private_dir(&exports).await.unwrap();
        tokio::fs::write(exports.join("native-artifact-abandoned"), b"temporary copy")
            .await
            .unwrap();
        tokio::fs::write(exports.join("keep"), b"unrelated owner file")
            .await
            .unwrap();
        let outside = tempfile::tempdir().unwrap();
        tokio::fs::write(outside.path().join("keep"), b"owner data")
            .await
            .unwrap();
        std::os::unix::fs::symlink(outside.path(), exports.join("native-artifact-link")).unwrap();
        recover_exports(state.path()).await.unwrap();
        assert!(!exports.join("native-artifact-abandoned").exists());
        assert!(
            exports
                .join("native-artifact-link")
                .symlink_metadata()
                .unwrap()
                .is_symlink()
        );
        assert_eq!(
            tokio::fs::read(exports.join("keep")).await.unwrap(),
            b"unrelated owner file"
        );
        assert_eq!(
            tokio::fs::read(outside.path().join("keep")).await.unwrap(),
            b"owner data"
        );
    }
}
