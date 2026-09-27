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
pub async fn capture(
    state: &Path,
    run: &str,
    socket: Option<PathBuf>,
    control: Arc<Mutex<()>>,
    stop: CancellationToken,
    attempt: &str,
) -> Result<Value> {
    let directory = state.join("disks").join(run);
    let volume = super::runtime::open(&directory)?;
    let guard = control.lock().await;
    if stop.is_cancelled() {
        return Err(Error::new(409, "VM stopped during capture."));
    }
    let waiting = volume.status()?["waitingFor"].as_str().map(str::to_owned);
    if waiting.as_deref() == Some("storage-unavailable") {
        return Err(Error::new(
            503,
            "Waiting for storage before capturing the disk.",
        ));
    }
    let mut emergency = waiting.is_some();
    let mut frozen = false;
    if let Some(socket) = &socket {
        if !emergency {
            let reply = tokio::time::timeout(
                Duration::from_secs(30),
                host::guest_request(socket, &json!({"op":"freeze"})),
            )
            .await;
            frozen = true;
            if !matches!(&reply,Ok(Ok(value)) if value["ok"]==true) {
                // A freeze can itself wait on a disk write blocked by reserve.
                // Seal a crash-consistent prefix so publication can free the
                // journal, then the ordered guest thaw can complete afterwards.
                emergency = true;
            }
        }
        if let Err(error) = host::pause_attempt(state, attempt).await {
            if frozen {
                let socket = socket.clone();
                let stop = stop.clone();
                tokio::spawn(async move {
                    let _ = thaw(&socket, &stop).await;
                });
            }
            return Err(error);
        }
    }
    let generation = volume.disk.seal();
    if socket.is_some() && !stop.is_cancelled() && !emergency {
        host::resume_attempt(state, attempt).await?;
    }
    if frozen {
        if emergency {
            let socket = socket.as_ref().unwrap().clone();
            let stop = stop.clone();
            tokio::spawn(async move {
                let _ = thaw(&socket, &stop).await;
            });
        } else {
            thaw(socket.as_ref().unwrap(), &stop).await?;
        }
    }
    drop(guard);
    let generation = generation?;
    let disk = volume.disk.clone();
    let mut manifest = tokio::task::spawn_blocking(move || disk.capture(generation))
        .await
        .map_err(Error::internal)??;
    manifest["runtime"] =
        serde_json::from_slice(&tokio::fs::read(directory.join("runtime.json")).await?)?;
    manifest["capturedAt"] = crate::config::now().into();
    manifest["consistency"] = if emergency { "crash" } else { "filesystem" }.into();
    manifest["generation"] = generation.into();
    manifest["onDemand"] = true.into();
    let id = crate::config::id();
    let snapshot = state.join("snapshots").join(&id);
    crate::skills::private_dir(&snapshot).await?;
    atomic_write(&snapshot.join("run"), run.as_bytes()).await?;
    atomic_write(
        &snapshot.join("manifest.json"),
        &serde_json::to_vec(&manifest)?,
    )
    .await?;
    Ok(json!({"id":id,"manifest":manifest,"grantId":volume.source.grant_id()}))
}
async fn thaw(socket: &Path, stop: &CancellationToken) -> Result<()> {
    let request = json!({"op":"thaw"});
    loop {
        tokio::select! {
            _=stop.cancelled()=>return Err(Error::new(409,"VM stopped during capture.")),
            result=tokio::time::timeout(Duration::from_secs(5),host::guest_request(socket,&request))=>if matches!(result,Ok(Ok(value)) if value["ok"]==true){return Ok(());}
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}
