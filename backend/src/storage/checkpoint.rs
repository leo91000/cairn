//! A generation is sealed before resuming the guest; its blocks can be sent later.
use crate::{
    error::{Error, Result},
    microvm::host,
    skills::atomic_write,
};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

/// A retained guest stays CPU-paused throughout capture; it must not be thawed
/// or have its mounted volume's read cancellation token closed.
pub(crate) enum Target {
    Stopped,
    Running { socket: PathBuf, attempt: String },
    Paused,
}

pub async fn capture(
    state: &Path,
    run: &str,
    socket: Option<PathBuf>,
    control: Arc<Mutex<()>>,
    stop: CancellationToken,
    attempt: &str,
) -> Result<Value> {
    let target = match socket {
        Some(socket) => Target::Running {
            socket,
            attempt: attempt.to_owned(),
        },
        None => Target::Stopped,
    };
    capture_target(state, run, target, control, stop).await
}

pub(crate) async fn capture_target(
    state: &Path,
    run: &str,
    target: Target,
    control: Arc<Mutex<()>>,
    stop: CancellationToken,
) -> Result<Value> {
    seal_target(state, run, target, control, stop)
        .await?
        .finish(state, run)
        .await
}

/// The immutable journal prefix is ready before block reconstruction starts.
/// Physical disk ownership must outlive this value; guest execution need not.
pub(crate) struct SealedCapture {
    volume: Arc<super::runtime::Volume>,
    directory: PathBuf,
    generation: i64,
    captured_at: i64,
    pause_ms: Value,
    crash_consistent: bool,
    stop: CancellationToken,
    timing: crate::performance::Operation,
    _stopped_reads: Option<tokio_util::sync::DropGuard>,
}

/// Reconstruct a completed turn without touching guest control or new writes.
pub(crate) async fn capture_completed(
    state: &Path,
    run: &str,
    stop: CancellationToken,
) -> Result<Value> {
    let directory = super::environment::directory(state, run)?;
    let volume = super::runtime::load(&directory).await?;
    let disk = volume.disk.clone();
    let completed = tokio::task::spawn_blocking(move || disk.completed())
        .await
        .map_err(Error::internal)??
        .ok_or_else(|| Error::new(412, "No completed turn generation is available."))?;
    let generation = completed["generation"]
        .as_i64()
        .ok_or_else(|| Error::conflict("Invalid completed generation."))?;
    let disk = volume.disk.clone();
    if !tokio::task::spawn_blocking(move || disk.has_sealed(generation))
        .await
        .map_err(Error::internal)??
    {
        let status = volume.inspect().await?;
        if status["published"]["generation"]
            .as_i64()
            .is_some_and(|published| published >= generation)
        {
            return Ok(
                json!({ "alreadyPublished": true, "published": status["published"], "grantId": volume.source.grant_id()? }),
            );
        }
        return Err(Error::conflict(
            "Completed journal generation is unavailable.",
        ));
    }
    SealedCapture {
        volume,
        directory,
        generation,
        captured_at: completed["capturedAt"].as_i64().unwrap_or(0),
        pause_ms: json!(0),
        crash_consistent: true,
        stop,
        timing: crate::performance::Operation::new("disk_snapshot", run, "completed_generation"),
        _stopped_reads: None,
    }
    .finish(state, run)
    .await
}

pub(crate) async fn seal_target(
    state: &Path,
    run: &str,
    target: Target,
    control: Arc<Mutex<()>>,
    stop: CancellationToken,
) -> Result<SealedCapture> {
    let stopped = matches!(target, Target::Stopped);
    // Paused CPUs fence new writes but do not flush dirty guest pages. A prior
    // turn sync is insufficient when guest background processes can write later.
    let crash_consistent = matches!(target, Target::Paused);
    let (socket, attempt) = match target {
        Target::Running { socket, attempt } => (Some(socket), attempt),
        Target::Stopped | Target::Paused => (None, String::new()),
    };
    let mut timing = crate::performance::Operation::new("disk_snapshot", run, "open_journal");
    let directory = super::environment::directory(state, run)?;
    let volume = super::runtime::load(&directory).await?;
    let _stopped_reads = stopped.then(|| volume.stop.clone().drop_guard());
    timing.next("control_lock");
    let guard = control.lock().await;
    if stop.is_cancelled() {
        return Err(Error::conflict("VM stopped during capture."));
    }
    let waiting = volume.inspect().await?["waitingFor"]
        .as_str()
        .map(str::to_owned);
    if waiting.as_deref() == Some("storage-unavailable") {
        return Err(Error::unavailable(
            "Waiting for storage before capturing the disk.",
        ));
    }
    let mut emergency = waiting.is_some();
    let mut frozen = false;
    let paused_at = std::time::Instant::now();
    timing.next("freeze_and_pause");
    if let Some(socket) = &socket {
        // A healthy VM may still be paused after its previous protection cycle.
        // Resume it before asking the guest to freeze; paused CPUs cannot reply.
        if !emergency && (volume.paused() || volume.transition_pending()) {
            if let Err(error) = host::settle_attempt(state, &attempt, false, &stop).await {
                stop.cancel();
                return Err(error);
            }
            volume.set_paused(false);
        }
        if !emergency {
            let reply = tokio::time::timeout(
                Duration::from_secs(30),
                host::guest_request(socket, &json!({ "op": "freeze" })),
            )
            .await;
            frozen = true;
            if !matches!(&reply, Ok(Ok(value)) if value["ok"] == true) {
                // A freeze can itself wait on a disk write blocked by reserve.
                // Seal a crash-consistent prefix so publication can free the
                // journal, then the ordered guest thaw can complete afterwards.
                emergency = true;
            }
        }
        // A failed response does not establish that the pause was rejected.
        // Keep the monitor aware of the possible pause until resume succeeds.
        volume.set_paused(true);
        if let Err(error) = host::settle_attempt(state, &attempt, true, &stop).await {
            if !emergency && !stop.is_cancelled() {
                if host::settle_attempt(state, &attempt, false, &stop)
                    .await
                    .is_ok()
                {
                    volume.set_paused(false);
                } else {
                    stop.cancel();
                }
            } else {
                // Under pressure, running is unsafe and a lost response leaves
                // the actual CPU state unknown. Tear down the attempt instead.
                stop.cancel();
            }
            drop(guard);
            if frozen {
                if emergency {
                    thaw_later(socket, &stop);
                } else {
                    let _ = thaw(socket, &stop).await;
                }
            }
            return Err(error);
        }
    }
    let captured_at = crate::config::now();
    timing.next("seal_and_resume");
    let generation = volume.seal().await;
    if socket.is_some() && !stop.is_cancelled() && !emergency {
        if let Err(error) = host::settle_attempt(state, &attempt, false, &stop).await {
            // The controller tears down an attempt whose CPUs cannot be resumed.
            stop.cancel();
            return Err(error);
        }
        volume.set_paused(false);
    }
    // Guest thaw can itself wait on remote I/O. Leave lease enforcement and
    // storage pause/resume free to operate while the ordered thaw is pending.
    drop(guard);
    timing.next("thaw");
    if frozen {
        if emergency {
            thaw_later(socket.as_ref().unwrap(), &stop);
        } else {
            thaw(socket.as_ref().unwrap(), &stop).await?;
        }
    }
    // An emergency capture leaves the guest paused until protection recovers;
    // its final pause duration is not known when this manifest is constructed.
    let pause_ms = if socket.is_none() {
        json!(0)
    } else if emergency {
        Value::Null
    } else {
        json!(paused_at.elapsed().as_millis() as u64)
    };
    Ok(SealedCapture {
        volume,
        directory,
        generation: generation?,
        captured_at,
        pause_ms,
        crash_consistent: emergency || crash_consistent,
        stop,
        timing,
        _stopped_reads,
    })
}

impl SealedCapture {
    pub(crate) async fn finish(self, state: &Path, run: &str) -> Result<Value> {
        let Self {
            volume,
            directory,
            generation,
            captured_at,
            pause_ms,
            crash_consistent,
            stop,
            mut timing,
            _stopped_reads,
        } = self;
        timing.next("reconstruct_manifest");
        let indexed_at = std::time::Instant::now();
        let disk = volume.disk.clone();
        let mut manifest = tokio::select! {
            () = stop.cancelled() => return Err(Error::conflict("Disk capture stopped.")),
            result = tokio::task::spawn_blocking(move || disk.capture(generation)) => {
                result.map_err(Error::internal)??
            }
        };
        manifest["indexMs"] = (indexed_at.elapsed().as_millis() as u64).into();
        manifest["pauseMs"] = pause_ms;
        timing.next("persist_snapshot");
        manifest["runtime"] =
            serde_json::from_slice(&tokio::fs::read(directory.join("runtime.json")).await?)?;
        manifest["capturedAt"] = captured_at.into();
        manifest["consistency"] = if crash_consistent {
            "crash"
        } else {
            "filesystem"
        }
        .into();
        manifest["generation"] = generation.into();
        manifest["onDemand"] = true.into();

        let id = write_snapshot(state, run, &manifest).await?;
        timing.finish();
        Ok(json!({
            "id": id,
            "manifest": manifest,
            "grantId": volume.source.grant_id()?
        }))
    }
}

async fn write_snapshot(state: &Path, run: &str, manifest: &Value) -> Result<String> {
    let id = crate::config::id();
    let snapshot = state.join("snapshots").join(&id);
    crate::skills::private_dir(&snapshot).await?;
    atomic_write(&snapshot.join("run"), run.as_bytes()).await?;
    atomic_write(
        &snapshot.join("manifest.json"),
        &serde_json::to_vec(manifest)?,
    )
    .await?;
    Ok(id)
}

/// Thaws without blocking the capture; it only fails once the VM is stopped.
fn thaw_later(socket: &Path, stop: &CancellationToken) {
    let socket = socket.to_owned();
    let stop = stop.clone();
    tokio::spawn(async move {
        let _ = thaw(&socket, &stop).await;
    });
}

async fn thaw(socket: &Path, stop: &CancellationToken) -> Result<()> {
    let request = json!({ "op": "thaw" });
    loop {
        let reply = tokio::time::timeout(
            Duration::from_secs(5),
            host::guest_request(socket, &request),
        );
        tokio::select! {
            () = stop.cancelled() => return Err(Error::conflict("VM stopped during capture.")),
            result = reply => {
                if matches!(result, Ok(Ok(value)) if value["ok"] == true) {
                    return Ok(());
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}
