//! Controller capture. Freeze the guest filesystem, pause CPUs, copy, then resume.
use crate::{
    error::{Error, Result},
    microvm::host,
    skills::{atomic_write, private_dir},
};
use serde_json::{Value, json};
use std::{
    os::fd::AsRawFd,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
pub const ACTIVE_CAPTURE_UNSUPPORTED: &str =
    "This retained VM runtime requires a paused capture; active backups are unavailable.";

pub async fn capture(
    state: &Path,
    run: &str,
    socket: Option<PathBuf>,
    control: Arc<Mutex<()>>,
    stop: CancellationToken,
    attempt: &str,
) -> Result<Value> {
    crate::validation::uuid(run)?;
    let disk = state.join("disks").join(run);
    let capture_lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(disk.join("snapshot.lock"))?;
    if unsafe { libc::flock(capture_lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err(Error::new(409, "A snapshot is already in progress."));
    }
    let captured_at = crate::config::now();
    let _lock = if socket.is_none() {
        let lock = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(disk.join("lock"))?;
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err(Error::new(409, "VM disk is still active."));
        }
        Some(lock)
    } else {
        None
    };
    // A new capture supersedes abandoned transfers for this run. The per-run
    // capture lock prevents removing a snapshot still being produced.
    let snapshots = state.join("snapshots");
    if snapshots.exists() {
        let mut entries = tokio::fs::read_dir(&snapshots).await?;
        while let Some(entry) = entries.next_entry().await? {
            if entry.file_type().await?.is_dir()
                && tokio::fs::read_to_string(entry.path().join("run"))
                    .await
                    .is_ok_and(|owner| owner == run)
            {
                tokio::fs::remove_dir_all(entry.path()).await?;
            }
        }
    }
    let id = crate::config::id();
    let directory = state.join("snapshots").join(&id);
    private_dir(&directory).await?;
    atomic_write(&directory.join("run"), run.as_bytes()).await?;
    let started = tokio::time::Instant::now();
    let mut frozen = false;
    let mut paused = false;
    let operation=async {
        if let Some(socket)=&socket {
            let status = guest(socket,"status").await?;
            if status["filesystemSnapshots"] != true {
                return Err(Error::new(412,ACTIVE_CAPTURE_UNSUPPORTED));
            }
            frozen=true;
            let result=guest(socket,"freeze").await?;
            if result["ok"]!=true {return Err(Error::new(503,"Guest filesystem freeze failed."));}
            frozen=true;
            let _guard=control.lock().await;
            if stop.is_cancelled() {return Err(Error::new(409,"VM stopped during capture."));}
            paused=true;host::pause_attempt(state,attempt).await?;
        }
        let mut command=tokio::process::Command::new("cp");
        command.args(["--reflink=auto","--sparse=always","--"]).arg(disk.join("data.ext4")).arg(directory.join("disk")).kill_on_drop(true).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
        let copy=tokio::time::timeout(Duration::from_secs(240),command.status());
        let status=tokio::select! {_=stop.cancelled()=>return Err(Error::new(409,"VM capture interrupted.")),result=copy=>result.map_err(|_|Error::new(503,"VM capture timed out."))??};
        if !status.success() {return Err(Error::new(503,"VM capture failed."));}
        std::fs::File::open(directory.join("disk"))?.sync_all()?;
        Ok(())
    }.await;
    // Cleanup is awaited even when the HTTP caller disappears: caller spawns capture.
    let resume = async {
        if paused {
            let _guard = control.lock().await;
            if !stop.is_cancelled() {
                host::resume_attempt(state, attempt).await?;
            }
        }
        if frozen
            && !stop.is_cancelled()
            && let Some(socket) = &socket
        {
            let result = guest(socket, "thaw").await?;
            if result["ok"] != true {
                return Err(Error::new(503, "Guest filesystem thaw failed."));
            }
        }
        Ok::<_, Error>(())
    }
    .await;
    if resume.is_err() {
        // A cancelled attempt is torn down by the controller, even if CPUs remain paused.
        stop.cancel();
    }
    if let Err(error) = operation.and(resume) {
        let _ = tokio::fs::remove_dir_all(directory).await;
        return Err(error);
    }
    let pause_ms = started.elapsed().as_millis() as u64;
    let result = async {
        let indexed = tokio::time::Instant::now();
        let mut manifest = super::snapshots::index(&directory.join("disk")).await?;
        manifest["runtime"] =
            serde_json::from_slice(&tokio::fs::read(disk.join("runtime.json")).await?)?;
        manifest["capturedAt"] = captured_at.into();
        manifest["pauseMs"] = pause_ms.into();
        manifest["indexMs"] = (indexed.elapsed().as_millis() as u64).into();
        manifest["localBytesRead"] = manifest["size"].clone();
        atomic_write(
            &directory.join("manifest.json"),
            &serde_json::to_vec(&manifest)?,
        )
        .await?;
        Ok(json!({"id":id,"manifest":manifest}))
    }
    .await;
    if result.is_err() {
        let _ = tokio::fs::remove_dir_all(&directory).await;
    }
    result
}

async fn guest(socket: &Path, operation: &str) -> Result<Value> {
    tokio::time::timeout(
        Duration::from_secs(10),
        host::guest_request(socket, &json!({"op":operation})),
    )
    .await
    .map_err(|_| Error::new(503, "Guest filesystem operation timed out."))?
}
