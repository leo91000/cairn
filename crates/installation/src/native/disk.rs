//! One logical disk pool includes retained conversations, archives and transfer
//! staging. Controller and connector mutations share the same process-safe lock.
use crate::error::{Error, Result};
use std::{path::Path, time::Duration};

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

/// Count file-backed legacy work conservatively, but charge journal/cache blocks
/// by allocated bytes. Mounted views refer to those same blocks and add no charge.
pub(crate) fn usage_sync(data: &Path, state: &Path) -> Result<u64> {
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
    let mut seen = std::collections::HashSet::new();
    let mut used = size(data, false, &mut seen)?;
    for entry in std::fs::read_dir(state)? {
        let entry = entry?;
        let name = entry.file_name();
        if name == "roots" {
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
                let block =
                    root.join("block-initialized").exists() || root.join("block-seeding").exists();
                if block {
                    for file in std::fs::read_dir(&root)? {
                        let file = file?;
                        if file.file_name() != "data" {
                            used = used.saturating_add(size(&file.path(), false, &mut seen)?);
                        }
                    }
                } else {
                    used = used.saturating_add(size(&root, false, &mut seen)?);
                }
            }
        } else {
            let physical = super::super::storage::environment::ROOTS
                .iter()
                .any(|root| name == *root);
            used = used.saturating_add(size(&entry.path(), physical, &mut seen)?);
        }
    }
    Ok(used)
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
    let response = crate::nodes::local_client()?
        .get("http://localhost/health")
        .timeout(Duration::from_secs(30))
        .send()
        .await
        .map_err(Error::internal)?;
    if !response.status().is_success() {
        return Err(Error::unavailable("Native disk budget is unavailable."));
    }
    let health: serde_json::Value = response.json().await.map_err(Error::internal)?;
    let limit = health["budget"]["limits"]["diskMiB"]
        .as_u64()
        .ok_or_else(|| Error::unavailable("Native disk budget is unavailable."))?
        .saturating_mul(MIB);
    let used = health["usage"]["diskBytes"]
        .as_u64()
        .ok_or_else(|| Error::unavailable("Native disk usage is unavailable."))?;
    Ok(available(limit, used))
}

#[cfg(test)]
mod tests {
    use super::*;

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
