//! Controller capture. Freeze the guest filesystem, pause CPUs, copy, then resume.
//!
//! When the master names a baseline this node still tracks (see [`super::tracking`]),
//! only the 4 MiB blocks written since then are copied: the running guest lists them,
//! or a stopped disk's sealed metadata does. Every other capture copies the whole disk.
use super::{snapshots::BLOCK, tracking};
use crate::{
    error::{Error, Result},
    microvm::host,
    skills::{atomic_write, private_dir},
};
use serde_json::{Value, json};
use std::{
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
    baseline: Option<&str>,
) -> Result<Value> {
    crate::validation::uuid(run)?;
    let disk = state.join("disks").join(run);
    let _capture_lock = crate::file_lock::exclusive(
        &disk.join("snapshot.lock"),
        "A snapshot is already in progress.",
    )?;
    let captured_at = crate::config::now();
    let _lock = if socket.is_none() {
        Some(crate::file_lock::exclusive(
            &disk.join("lock"),
            "VM disk is still active.",
        )?)
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
    let known = tracking::baseline(&disk, baseline);
    let started = tokio::time::Instant::now();
    let mut frozen = false;
    let mut paused = false;
    let mut plan = Plan::default();
    let mut read = 0u64;
    let operation=async {
        if let Some(socket)=&socket {
            let status = guest(socket,json!({"op":"status"})).await?;
            if status["filesystemSnapshots"] != true {
                return Err(Error::new(412,ACTIVE_CAPTURE_UNSUPPORTED));
            }
            frozen=true;
            let result=guest(socket,json!({"op":"freeze"})).await?;
            if result["ok"]!=true {return Err(Error::new(503,"Guest filesystem freeze failed."));}
            frozen=true;
            // Ask before pausing: paused vCPUs cannot answer, and the frozen filesystem
            // already keeps the list exact until the copy ends.
            if status["writeTracking"] == true {
                plan = Plan::from_guest(socket, known.as_ref()).await;
            }
            let _guard=control.lock().await;
            if stop.is_cancelled() {return Err(Error::new(409,"VM stopped during capture."));}
            paused=true;host::pause_attempt(state,attempt).await?;
        } else {
            plan = Plan::from_seal(&disk, known.as_ref()).await;
        }
        if let Some(blocks) = &plan.written {
            read = copy_blocks(&disk.join("data.ext4"), &directory.join("disk"), blocks).await?;
            let present = blocks.iter().map(|block| block * BLOCK).collect::<Vec<_>>();
            atomic_write(&directory.join("present.json"), &serde_json::to_vec(&present)?).await?;
            return Ok(());
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
            let result = guest(socket, json!({"op":"thaw"})).await?;
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
        let mut manifest = match (&known, &plan.written) {
            (Some(known), Some(blocks)) => {
                let mut manifest =
                    super::snapshots::update(&directory.join("disk"), &known.manifest, blocks)
                        .await?;
                manifest["localBytesRead"] = read.into();
                manifest
            }
            _ => super::snapshots::index(&directory.join("disk")).await?,
        };
        manifest["incremental"] = plan.written.is_some().into();
        manifest["runtime"] =
            serde_json::from_slice(&tokio::fs::read(disk.join("runtime.json")).await?)?;
        manifest["capturedAt"] = captured_at.into();
        manifest["pauseMs"] = pause_ms.into();
        manifest["indexMs"] = (indexed.elapsed().as_millis() as u64).into();
        atomic_write(
            &directory.join("manifest.json"),
            &serde_json::to_vec(&manifest)?,
        )
        .await?;
        if let Some(era) = plan.era {
            let full_at = match (&known, &plan.written) {
                (Some(known), Some(_)) => known.full_at,
                _ => captured_at,
            };
            tracking::remember(&disk, &id, era, &manifest, full_at).await?;
        }
        Ok(json!({"id":id,"manifest":manifest}))
    }
    .await;
    if result.is_err() {
        let _ = tokio::fs::remove_dir_all(&directory).await;
    }
    result
}

/// What a capture copies, and the era it records so the next one can continue.
#[derive(Default)]
struct Plan {
    era: Option<u64>,
    /// Blocks written since the baseline; `None` copies the whole disk.
    written: Option<Vec<u64>>,
}
impl Plan {
    /// Asks the frozen guest. Tracking is an optimization: any failure means a full copy.
    async fn from_guest(socket: &Path, known: Option<&tracking::Baseline>) -> Self {
        let Ok(reply) = guest(
            socket,
            json!({"op":"written","since":known.map(|known| known.era)}),
        )
        .await
        else {
            return Self::default();
        };
        let Some(era) = reply["era"].as_u64() else {
            return Self::default();
        };
        if reply["ok"] != true || reply["blockSize"] != BLOCK {
            return Self::default();
        }
        let ranges = serde_json::from_value::<Vec<[u64; 2]>>(reply["blocks"].clone()).ok();
        Self {
            era: Some(era),
            written: known
                .zip(ranges)
                .and_then(|(known, ranges)| tracking::blocks(&ranges, &known.manifest)),
        }
    }
    /// A stopped disk whose guest sealed its last era lists its writes offline.
    async fn from_seal(disk: &Path, known: Option<&tracking::Baseline>) -> Self {
        let Some(era) = tracking::sealed(disk) else {
            return Self::default();
        };
        let written = match known {
            Some(known) => tracking::offline(disk, known.era)
                .await
                .and_then(|ranges| tracking::blocks(&ranges, &known.manifest)),
            None => None,
        };
        Self {
            era: Some(era),
            written,
        }
    }
}
/// Copies the listed blocks of the paused disk to a sparse file of the same size.
async fn copy_blocks(source: &Path, target: &Path, blocks: &[u64]) -> Result<u64> {
    let (source, target, blocks) = (source.to_owned(), target.to_owned(), blocks.to_owned());
    tokio::task::spawn_blocking(move || -> Result<u64> {
        use std::os::unix::fs::FileExt;
        let input = std::fs::File::open(&source)?;
        let size = input.metadata()?.len();
        let output = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&target)?;
        output.set_len(size)?;
        let (mut buffer, mut read) = (vec![0; BLOCK as usize], 0);
        for block in blocks {
            let offset = block * BLOCK;
            let length = size.saturating_sub(offset).min(BLOCK) as usize;
            input.read_exact_at(&mut buffer[..length], offset)?;
            output.write_all_at(&buffer[..length], offset)?;
            read += length as u64;
        }
        output.sync_all()?;
        Ok(read)
    })
    .await
    .map_err(Error::internal)?
}
async fn guest(socket: &Path, request: Value) -> Result<Value> {
    tokio::time::timeout(
        Duration::from_secs(10),
        host::guest_request(socket, &request),
    )
    .await
    .map_err(|_| Error::new(503, "Guest filesystem operation timed out."))?
}
