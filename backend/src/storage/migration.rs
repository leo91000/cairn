//! Install a remotely published base only while the original disk remains unchanged.
use crate::error::{Error, Result};
use serde_json::Value;
use std::path::Path;
/// Callers must keep this future alive once the atomic installation starts.
/// Cancellation interrupts verification; the durable switch always finishes.
pub async fn install(
    directory: &Path,
    value: Value,
    cancel: tokio_util::sync::CancellationToken,
) -> Result<Value> {
    use serde_json::json;
    let _lock = crate::file_lock::exclusive(&directory.join("lock"), "VM disk is active.")?;
    let marker = directory.join("migration.json");
    if super::runtime::exists(directory) {
        // A retry must never replace the journal or erase later acknowledged writes.
        let volume = super::runtime::load(directory).await?;
        if marker.exists() && directory.join("data.ext4").exists() {
            tokio::fs::remove_file(directory.join("data.ext4")).await?;
            tokio::fs::File::open(directory).await?.sync_all().await?;
        }
        let mut status = volume.inspect().await?;
        status["ready"] = true.into();
        return Ok(status);
    }
    let manifest = &value["manifest"];
    crate::nodes::snapshots::validate(manifest)?;
    let original = directory.join("data.ext4");
    let current = tokio::select! {
        biased;
        _ = cancel.cancelled() => return Err(Error::new(409, "Migration deferred for conversation execution.")),
        current = crate::nodes::snapshots::index(&original) => current?,
    };
    if current["size"] != manifest["size"] || current["blocks"] != manifest["blocks"] {
        return Err(Error::new(
            409,
            "Disk changed after its recovery point; migration will retry when stopped.",
        ));
    }
    let context = json!({"master":value["master"],"grant":value["grant"],"policy":value["policy"]});
    let staging = tempfile::Builder::new()
        .prefix("migration-")
        .tempdir_in(directory)?;
    let pending = staging.path().to_owned();
    drop(super::runtime::create(&pending, manifest, &context).await?);
    crate::skills::atomic_write(
        &marker,
        &serde_json::to_vec(&json!({"backupId":value["backupId"]}))?,
    )
    .await?;
    tokio::fs::rename(&pending, directory.join("lazy")).await?;
    tokio::fs::File::open(directory).await?.sync_all().await?;
    // Only this verified original is reclaimed. Unknown stale copies are preserved.
    tokio::fs::remove_file(&original).await?;
    crate::nodes::tracking::invalidate(directory).await?;
    tokio::fs::File::open(directory).await?.sync_all().await?;
    let volume = super::runtime::load(directory).await?;
    let mut status = volume.inspect().await?;
    status["ready"] = true.into();
    Ok(status)
}
/// A migration snapshot holds the original stopped disk in place for the transfer.
/// Its hard link allocates no second image; dropping it releases both ownership locks.
pub struct Capture {
    pub value: Value,
    pub touched: tokio::time::Instant,
    directory: std::path::PathBuf,
    _disk: crate::file_lock::Guard,
    _snapshot: crate::file_lock::Guard,
}
impl Drop for Capture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}
pub async fn capture(state: &Path, run: &str) -> Result<Capture> {
    let disk = state.join("disks").join(run);
    let snapshot =
        crate::file_lock::exclusive(&disk.join("snapshot.lock"), "Capture in progress.")?;
    let owner = crate::file_lock::exclusive(&disk.join("lock"), "VM disk is active.")?;
    if super::runtime::exists(&disk) {
        return Err(Error::new(409, "Disk already migrated."));
    }
    let id = crate::config::id();
    let directory = state.join("snapshots").join(&id);
    crate::skills::private_dir(&directory).await?;
    let mut capture = Capture {
        value: Value::Null,
        touched: tokio::time::Instant::now(),
        directory,
        _disk: owner,
        _snapshot: snapshot,
    };
    tokio::fs::hard_link(disk.join("data.ext4"), capture.directory.join("disk")).await?;
    let mut manifest = crate::nodes::snapshots::index(&capture.directory.join("disk")).await?;
    manifest["runtime"] =
        serde_json::from_slice(&tokio::fs::read(disk.join("runtime.json")).await?)?;
    manifest["capturedAt"] = crate::config::now().into();
    crate::skills::atomic_write(
        &capture.directory.join("manifest.json"),
        &serde_json::to_vec(&manifest)?,
    )
    .await?;
    crate::skills::atomic_write(&capture.directory.join("run"), run.as_bytes()).await?;
    capture.value = serde_json::json!({"id":id,"manifest":manifest});
    Ok(capture)
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[tokio::test]
    async fn migration_capture_streams_original_under_lock_without_copying() {
        use std::os::unix::fs::MetadataExt;
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("disks/conversation");
        std::fs::create_dir_all(&directory).unwrap();
        let raw = directory.join("data.ext4");
        std::fs::write(&raw, vec![7; 4096]).unwrap();
        std::fs::write(directory.join("runtime.json"), b"{}").unwrap();
        let snapshot = capture(root.path(), "conversation").await.unwrap();
        assert_eq!(
            std::fs::metadata(&raw).unwrap().ino(),
            std::fs::metadata(snapshot.directory.join("disk"))
                .unwrap()
                .ino()
        );
        assert!(crate::file_lock::exclusive(&directory.join("lock"), "busy").is_err());
        let hash = snapshot.value["manifest"]["blocks"][0]["hash"]
            .as_str()
            .unwrap();
        assert_eq!(
            crate::nodes::snapshots::served(&snapshot.directory, hash)
                .await
                .unwrap(),
            vec![7; 4096]
        );
        let captured = snapshot.directory.clone();
        drop(snapshot);
        assert!(!captured.exists());
        assert_eq!(std::fs::read(&raw).unwrap(), vec![7; 4096]);
        assert!(crate::file_lock::exclusive(&directory.join("lock"), "busy").is_ok());
    }
    #[tokio::test]
    async fn migration_rejects_a_changed_original_and_installs_without_downloading() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("disks/conversation");
        std::fs::create_dir_all(&directory).unwrap();
        let raw = directory.join("data.ext4");
        std::fs::write(&raw, vec![7; 4096]).unwrap();
        let manifest = crate::nodes::snapshots::index(&raw).await.unwrap();
        let request = json!({"manifest":manifest,"backupId":"published-fixture","master":"http://127.0.0.1:1/","grant":"fixture","policy":super::super::policy::Policy::default()});
        std::fs::write(&raw, vec![9; 4096]).unwrap();
        assert!(
            install(&directory, request.clone(), Default::default())
                .await
                .is_err()
        );
        assert_eq!(std::fs::read(&raw).unwrap(), vec![9; 4096]);
        assert!(!super::super::runtime::exists(&directory));
        std::fs::write(&raw, vec![7; 4096]).unwrap();
        let cancel = tokio_util::sync::CancellationToken::new();
        // A pending future may already have entered the atomic switch: cancellation
        // there must finish installation. Cancel before verification for a deterministic
        // assertion that foreground execution leaves the original available.
        cancel.cancel();
        assert!(install(&directory, request.clone(), cancel).await.is_err());
        assert_eq!(std::fs::read(&raw).unwrap(), vec![7; 4096]);
        assert!(!super::super::runtime::exists(&directory));
        assert!(crate::file_lock::exclusive(&directory.join("lock"), "busy").is_ok());
        // The origin is deliberately offline: installation needs only verified metadata.
        let installed = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            install(&directory, request.clone(), Default::default()),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(installed["ready"], true);
        assert!(!raw.exists());
        let volume = super::super::runtime::open(&directory).unwrap();
        super::super::Disk::write_at(volume.disk.as_ref(), 3, b"new work").unwrap();
        drop(volume);
        install(&directory, request, Default::default())
            .await
            .unwrap();
        let volume = super::super::runtime::open(&directory).unwrap();
        let mut bytes = [0; 8];
        super::super::Disk::read_at(volume.disk.as_ref(), 3, &mut bytes).unwrap();
        assert_eq!(&bytes, b"new work");
    }
}
