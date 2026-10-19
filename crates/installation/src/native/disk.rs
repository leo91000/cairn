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
    let mut used = bytes_if_present(data).await?;
    let mut entries = tokio::fs::read_dir(state).await?;
    while let Some(entry) = entries.next_entry().await? {
        // Sandbox mount points refer to public host libraries in the workload's
        // private mount namespace. They are not retained Cairn payloads.
        if entry.file_name() == "roots" {
            let mut plans = tokio::fs::read_dir(entry.path()).await?;
            while let Some(plan) = plans.next_entry().await? {
                let metadata = plan.metadata().await?;
                if metadata.is_file() {
                    used = used.saturating_add(metadata.len());
                }
            }
            continue;
        }
        let metadata = entry.metadata().await?;
        if metadata.is_dir() && !entry.file_type().await?.is_symlink() {
            used = used.saturating_add(bytes_if_present(&entry.path()).await?);
        } else if metadata.is_file() {
            used = used.saturating_add(metadata.len());
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

pub(crate) async fn lock(state: &Path) -> Result<crate::file_lock::Guard> {
    tokio::time::timeout(Duration::from_secs(300), async {
        loop {
            match crate::file_lock::exclusive(
                &state.join("storage.lock"),
                "Another native storage operation is in progress.",
            ) {
                Ok(owner) => return Ok(owner),
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
