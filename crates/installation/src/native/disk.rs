//! One logical disk pool includes retained conversations, archives and transfer
//! staging. Controller and connector mutations share the same process-safe lock.
use crate::error::{Error, Result};
use std::{
    path::Path,
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

pub(crate) const MIB: u64 = 1_048_576;

const METADATA_RESERVE: u64 = MIB;

pub(crate) fn available(limit: u64, used: u64) -> u64 {
    limit.saturating_sub(used).saturating_sub(METADATA_RESERVE)
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
    tokio::task::spawn_blocking(move || measure_usage(&data, &state, false))
        .await
        .map_err(Error::internal)?
}

/// Admission includes reserved incoming bytes; supervision uses only bytes
/// already materialized, so a copy cannot be charged twice and stop a peer.
pub(crate) fn usage_sync(data: &Path, state: &Path) -> Result<u64> {
    measure_usage(data, state, true)
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
fn measure_usage(data: &Path, state: &Path, include_reservations: bool) -> Result<u64> {
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
    let mut used = size(data, false, &mut seen)?;
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
            if !include_reservations {
                continue;
            }
            for reservation in std::fs::read_dir(entry.path())? {
                let reservation = reservation?;
                if reservation
                    .file_name()
                    .to_str()
                    .is_none_or(|name| crate::validation::uuid(name).is_err())
                {
                    continue;
                }
                let kind = match reservation.file_type() {
                    Ok(kind) => kind,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                    Err(error) => return Err(error.into()),
                };
                if !kind.is_file() {
                    return Err(Error::conflict("Invalid native preparation reservation."));
                }
                let text = match std::fs::read_to_string(reservation.path()) {
                    Ok(text) => text,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                    Err(error) => return Err(error.into()),
                };
                let bytes = text.parse::<u64>().map_err(Error::internal)?;
                used = used.saturating_add(bytes);
            }
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
    path: std::path::PathBuf,
    lease: std::path::PathBuf,
    _owner: crate::file_lock::Guard,
    pub bytes: u64,
}

impl Preparation {
    /// Caller serializes this publication with the native storage pool gate.
    pub async fn reserve(state: &Path, attempt: &str, bytes: u64) -> Result<Self> {
        crate::validation::uuid(attempt)?;
        let root = state.join("native-preparations");
        crate::skills::private_dir(&root).await?;
        let path = root.join(attempt);
        let lease = root.join(format!("{attempt}.lease"));
        let owner = crate::file_lock::exclusive(&lease, "Native preparation is already reserved.")?;
        if path.try_exists()? {
            return Err(Error::conflict("Native preparation is already reserved."));
        }
        super::atomic_write(&path, bytes.to_string().as_bytes()).await?;
        Ok(Self {
            path,
            lease,
            _owner: owner,
            bytes,
        })
    }
}

impl Drop for Preparation {
    fn drop(&mut self) {
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

pub(crate) fn preparation_bytes(state: &Path) -> Result<u64> {
    let entries = match std::fs::read_dir(state.join("native-preparations")) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error.into()),
    };
    let mut bytes = 0u64;
    for entry in entries {
        let entry = entry?;
        if !entry
            .file_name()
            .to_str()
            .is_some_and(|name| crate::validation::uuid(name).is_ok())
        {
            continue;
        }
        match std::fs::read_to_string(entry.path()) {
            Ok(amount) => {
                bytes = bytes.saturating_add(amount.parse::<u64>().map_err(Error::internal)?);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(bytes)
}

pub(super) fn recover_preparations(
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
        for path in [root.join(attempt), lease] {
            match std::fs::remove_file(path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
    }
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
    staging_budget_from(&crate::nodes::local_client()?, "http://localhost").await
}

pub(super) async fn staging_budget_from(client: &reqwest::Client, origin: &str) -> Result<u64> {
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
    let budget: serde_json::Value = response.json().await.map_err(Error::internal)?;
    budget["availableBytes"]
        .as_u64()
        .ok_or_else(|| Error::unavailable("Native disk budget is unavailable."))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn recovery_keeps_another_process_staging_reservation_until_its_lease_settles() {
        let state = tempfile::tempdir().unwrap();
        let attempt = crate::config::id();
        let protected = Preparation::reserve(state.path(), &attempt, 42)
            .await
            .unwrap();
        let stale = crate::config::id();
        std::fs::write(state.path().join("native-preparations").join(&stale), b"10").unwrap();

        recover_preparations(state.path(), &std::collections::HashMap::new()).unwrap();
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
