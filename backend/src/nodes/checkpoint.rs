//! Serialize captures of a journal-backed conversation disk.
use crate::{
    error::{Error, Result},
    skills::private_dir,
};
use serde_json::Value;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

pub async fn capture(
    state: &Path,
    run: &str,
    socket: Option<PathBuf>,
    control: Arc<Mutex<()>>,
    stop: CancellationToken,
    attempt: &str,
    _baseline: Option<&str>,
) -> Result<Value> {
    // A stopped disk has no physical owner. Running captures use guest control.
    let _stopped = if socket.is_none() {
        Some(crate::storage::environment::lock(state, run, "VM disk is still active.").await?)
    } else {
        None
    };
    let target = match socket {
        Some(socket) => crate::storage::checkpoint::Target::Running {
            socket,
            attempt: attempt.to_owned(),
        },
        None => crate::storage::checkpoint::Target::Stopped,
    };
    capture_target(state, run, target, control, stop).await
}

/// The pool holds its capture barrier and the paused VMM's ownership through this entire
/// call. No turn can resume, resize, move or erase this physical disk meanwhile.
pub(crate) async fn capture_paused(
    state: &Path,
    run: &str,
    owner: &crate::storage::environment::OwnerLease,
    stop: CancellationToken,
) -> Result<Value> {
    if owner.directory != crate::storage::environment::directory(state, run)? {
        return Err(Error::conflict("Paused disk ownership changed."));
    }
    capture_target(
        state,
        run,
        crate::storage::checkpoint::Target::Paused,
        Arc::default(),
        stop,
    )
    .await
}

async fn capture_target(
    state: &Path,
    run: &str,
    target: crate::storage::checkpoint::Target,
    control: Arc<Mutex<()>>,
    stop: CancellationToken,
) -> Result<Value> {
    crate::validation::uuid(run)?;
    let disk = crate::storage::environment::directory(state, run)?;
    let _capture = crate::file_lock::exclusive(
        &disk.join("snapshot.lock"),
        "A snapshot is already in progress.",
    )?;
    if !crate::storage::runtime::exists(&disk) {
        return Err(Error::conflict("Conversation has no S3-backed journal."));
    }
    let snapshots = state.join("snapshots");
    private_dir(&snapshots).await?;
    let mut entries = tokio::fs::read_dir(&snapshots).await?;
    // Earlier captures of this run are superseded by the new one.
    while let Some(entry) = entries.next_entry().await? {
        let owned = entry.file_type().await?.is_dir()
            && tokio::fs::read_to_string(entry.path().join("run"))
                .await
                .is_ok_and(|owner| owner == run);
        if owned {
            tokio::fs::remove_dir_all(entry.path()).await?;
        }
    }
    crate::storage::checkpoint::capture_target(state, run, target, control, stop).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::Disk;
    use serde_json::json;

    #[tokio::test]
    async fn retained_capture_preserves_mounted_reads_and_ownership() {
        let root = tempfile::tempdir().unwrap();
        let run = crate::config::id();
        let owner = crate::storage::environment::ownership(root.path(), &run, "busy")
            .await
            .unwrap();
        let manifest = json!({
            "version": 1,
            "size": 4096,
            "blockSize": crate::nodes::snapshots::BLOCK,
            "blocks": [{ "offset": 0, "size": 4096, "hash": null }],
        });
        let source =
            json!({ "master": "http://127.0.0.1:9/", "grant": "synthetic-retained-owner" });
        let disk =
            crate::storage::runtime::create(&owner.directory.join("lazy"), &manifest, &source)
                .await
                .unwrap();
        disk.write_at(0, b"retained durable data").unwrap();
        std::fs::write(
            owner.directory.join("runtime.json"),
            b"{\"runtimeId\":\"fixture\"}",
        )
        .unwrap();
        drop(disk);
        let volume = crate::storage::runtime::load(&owner.directory)
            .await
            .unwrap();
        let disk = volume.disk.clone();
        let cancellation = volume.stop.clone();
        for _ in 0..2 {
            let snapshot = capture_paused(root.path(), &run, &owner, CancellationToken::new())
                .await
                .unwrap();
            assert_eq!(snapshot["manifest"]["consistency"], "crash");
            assert!(snapshot["manifest"]["generation"].as_i64().unwrap() > 0);
            assert!(
                !cancellation.is_cancelled(),
                "Publishing a paused guest never cancels its mounted source"
            );
            assert!(Arc::ptr_eq(
                &volume,
                &crate::storage::runtime::load(&owner.directory)
                    .await
                    .unwrap()
            ));
            assert!(
                crate::storage::environment::ownership(root.path(), &run, "busy")
                    .await
                    .is_err()
            );
            let mut bytes = [0; 21];
            disk.read_at(0, &mut bytes).unwrap();
            assert_eq!(&bytes, b"retained durable data");
        }
    }
}
