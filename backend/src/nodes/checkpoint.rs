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
    crate::validation::uuid(run)?;
    let disk = state.join("disks").join(run);
    let _capture = crate::file_lock::exclusive(
        &disk.join("snapshot.lock"),
        "A snapshot is already in progress.",
    )?;
    // Without a running VM to pause, the disk must not be in use at all.
    let _stopped = socket
        .is_none()
        .then(|| crate::file_lock::exclusive(&disk.join("lock"), "VM disk is still active."))
        .transpose()?;
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
    crate::storage::checkpoint::capture(state, run, socket, control, stop, attempt).await
}
