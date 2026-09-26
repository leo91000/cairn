//! Authenticated execution interface shared by chats, scheduled tasks and recovery.
use crate::{
    auth::safe_equal,
    config::now,
    error::{Error, Result},
    microvm::{host, wire},
    skills::{atomic_write, private_dir},
    validation::{text, uuid},
};
use axum::{
    Json, Router,
    body::Body,
    extract::{Request, State},
    response::{IntoResponse, Response},
    routing::any,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use bytes::Bytes;
use futures_util::StreamExt;
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt, BufReader},
    sync::{Mutex, watch},
};
use tokio_util::sync::CancellationToken;

pub const CONTROLLER_INTERRUPTED: i32 = 75;

struct Attempt {
    stop: CancellationToken,
    done: watch::Receiver<bool>,
    socket: Arc<tokio::sync::OnceCell<PathBuf>>,
    plan: Value,
    imports: Arc<Mutex<()>>,
    control: Arc<Mutex<()>>,
}
#[derive(Clone)]
struct Broker {
    data: PathBuf,
    state: PathBuf,
    pool: Arc<crate::microvm::pool::Pool>,
    active: Arc<Mutex<HashMap<String, Attempt>>>,
    stop: CancellationToken,
    leases: Arc<Mutex<HashMap<String, u64>>>,
}

fn normalized(path: &Path) -> bool {
    path.is_absolute()
        && path.components().all(|p| {
            !matches!(
                p,
                std::path::Component::ParentDir | std::path::Component::CurDir
            )
        })
        && !path.to_string_lossy().contains("//")
}
fn validate(plan: &Value, id: &str, data: &Path) -> Result<()> {
    uuid(text(plan, "runId"))?;
    if plan["id"] != id
        || !match plan.get("expires") {
            Some(Value::Null) => true,
            Some(value) => value
                .as_i64()
                .is_some_and(|n| n > now() && n <= now() + 13 * 3600000),
            None => false,
        }
    {
        return Err(Error::bad("Invalid or expired execution plan."));
    }
    let run_root = data.join("runs").join(text(plan, "runId"));
    for import in plan["imports"]
        .as_array()
        .ok_or_else(|| Error::bad("Missing execution imports."))?
    {
        let source = Path::new(text(import, "source"));
        let target = Path::new(text(import, "target"));
        if !normalized(source)
            || !source.starts_with(&run_root)
            || !normalized(target)
            || !(target.starts_with(&run_root)
                || target == Path::new("/home/node")
                || target == Path::new("/run/leo-chat"))
        {
            return Err(Error::bad(
                "Execution import is outside its private workspace.",
            ));
        }
    }
    if !Path::new(text(plan, "cwd")).starts_with(&run_root) {
        return Err(Error::bad("Invalid guest workspace."));
    }
    if let Some(output) = plan["chat"]["output"].as_str()
        && Path::new(output) != run_root.join("output/result.md")
    {
        return Err(Error::bad("Invalid result destination."));
    }
    Ok(())
}
impl Broker {
    async fn start(&self, id: &str) -> Result<()> {
        if self.active.lock().await.contains_key(id) {
            return Ok(());
        }
        if self.state.join(format!("{id}.stopped")).exists()
            || self.state.join(format!("{id}.exit")).exists()
        {
            return Err(Error::new(
                409,
                "This execution attempt has already stopped.",
            ));
        }
        let file = tokio::fs::File::open(self.data.join("runner-plans").join(format!("{id}.json")))
            .await?;
        let mut bytes = Vec::new();
        file.take(8_000_001).read_to_end(&mut bytes).await?;
        if bytes.len() > 8_000_000 {
            return Err(Error::bad("Execution plan exceeds limit."));
        }
        let plan: Value = serde_json::from_slice(&bytes)?;
        validate(&plan, id, &self.data)?;
        if plan["nodeLeaseRequired"] == true
            && self
                .leases
                .lock()
                .await
                .get(id)
                .is_none_or(|until| *until <= crate::nodes::boot_ms())
        {
            return Err(Error::new(
                409,
                "Node execution lease is missing or expired.",
            ));
        }
        // Imports must resolve to regular directories in this run; a symlink is not a scope grant.
        for import in plan["imports"].as_array().unwrap() {
            let path = Path::new(text(import, "source"));
            if tokio::fs::canonicalize(path).await? != path {
                return Err(Error::bad("Execution import changed location."));
            }
        }
        if self
            .active
            .lock()
            .await
            .values()
            .any(|a| a.plan["runId"] == plan["runId"])
        {
            return Err(Error::new(
                409,
                "This workspace already has an active attempt.",
            ));
        }
        let reservation = self.pool.reserve(text(&plan, "runId")).await?;
        // Reservation can wait for preparation teardown. Keep health, stop and imports responsive.
        let mut active = self.active.lock().await;
        if active.contains_key(id) {
            return Ok(());
        }
        if self.stop.is_cancelled()
            || self.state.join(format!("{id}.stopped")).exists()
            || self.state.join(format!("{id}.exit")).exists()
        {
            return Err(Error::new(409, "This execution attempt has stopped."));
        }
        validate(&plan, id, &self.data)?;
        if active.values().any(|a| a.plan["runId"] == plan["runId"]) {
            return Err(Error::new(
                409,
                "This workspace already has an active attempt.",
            ));
        }
        let socket = Arc::new(tokio::sync::OnceCell::new());
        let control = Arc::new(Mutex::new(()));
        let stop = self.stop.child_token();
        let (done, receiver) = watch::channel(false);
        atomic_write(
            &self.state.join(format!("{id}.run")),
            text(&plan, "runId").as_bytes(),
        )
        .await?;
        atomic_write(&self.state.join(format!("{id}.active")), b"1").await?;
        active.insert(
            id.into(),
            Attempt {
                stop: stop.clone(),
                done: receiver,
                socket: socket.clone(),
                plan: plan.clone(),
                imports: Default::default(),
                control: control.clone(),
            },
        );
        let broker = self.clone();
        let id = id.to_owned();
        tokio::spawn(async move {
            let deadline = plan["expires"].as_i64();
            let expiry = stop.clone();
            let leased = plan["nodeLeaseRequired"] == true;
            let owner = broker.clone();
            let attempt = id.clone();
            let timer = tokio::spawn(async move {
                loop {
                    if deadline.is_some_and(|d| now() >= d)
                        || (leased
                            && owner
                                .leases
                                .lock()
                                .await
                                .get(&attempt)
                                .is_none_or(|d| *d <= crate::nodes::boot_ms()))
                    {
                        if leased {
                            let _guard = control.lock().await;
                            let _ = host::pause_attempt(&owner.state, &attempt).await;
                            expiry.cancel();
                        } else {
                            expiry.cancel();
                        }
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            });
            let result = reservation.execute(plan, socket, stop).await;
            timer.abort();
            let code = match result {
                Ok(code) => code,
                Err(error) => {
                    let event = json!({"type":"output","stderr":true,"data":STANDARD.encode(format!("{}\n",error.message))});
                    if let Ok(mut log) = tokio::fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(broker.state.join(format!("{id}.log")))
                        .await
                    {
                        let _ = wire::write(&mut log, &event).await;
                    }
                    1
                }
            };
            let _ = atomic_write(
                &broker.state.join(format!("{id}.exit")),
                code.to_string().as_bytes(),
            )
            .await;
            let _ = tokio::fs::remove_file(broker.state.join(format!("{id}.active"))).await;
            broker.active.lock().await.remove(&id);
            let _ = done.send(true);
        });
        Ok(())
    }
    async fn open_project(&self, id: &str, project_id: &str, value: Value) -> Result<Value> {
        uuid(project_id)?;
        let (plan, stop, lock, socket) = {
            let active = self.active.lock().await;
            let attempt = active
                .get(id)
                .ok_or_else(|| Error::new(409, "VM is not active."))?;
            (
                attempt.plan.clone(),
                attempt.stop.clone(),
                attempt.imports.clone(),
                attempt.socket.clone(),
            )
        };
        let _guard = lock.lock().await;
        if stop.is_cancelled() || value["runId"] != plan["runId"] {
            return Err(Error::new(409, "VM attempt changed."));
        }
        let root = self.data.join("runs").join(text(&plan, "runId"));
        let source = Path::new(text(&value, "source"));
        let target = Path::new(text(&value, "target"));
        if !normalized(source)
            || !source.starts_with(&root)
            || source != target
            || source.file_name().and_then(|v| v.to_str()) != Some(project_id)
            || tokio::fs::canonicalize(source).await? != source
        {
            return Err(Error::bad(
                "Project import is outside its private workspace.",
            ));
        }
        let socket = socket
            .get()
            .ok_or_else(|| Error::new(409, "VM is still starting."))?;
        tokio::select! {
            _ = stop.cancelled() => Err(Error::new(409,"VM stopped during project import.")),
            result = tokio::time::timeout(Duration::from_secs(290),host::import_project(socket,source,text(&value,"target"),plan["sandbox"] == "read-only")) => result.map_err(|_| Error::new(503,"Project import timed out."))?,
        }
    }
    async fn stop(&self, id: &str) -> Result<()> {
        atomic_write(&self.state.join(format!("{id}.stopped")), b"").await?;
        let attempt = {
            let active = self.active.lock().await;
            active.get(id).map(|a| {
                a.stop.cancel();
                a.done.clone()
            })
        };
        if let Some(mut done) = attempt {
            let _ = tokio::time::timeout(Duration::from_secs(15), done.wait_for(|done| *done))
                .await
                .map_err(|_| Error::new(503, "Waiting for the previous VM to stop."))?;
        }
        Ok(())
    }
}
async fn erase_attempt_content(broker: &Broker, run: &str) -> Result<()> {
    let mut attempts = std::collections::HashSet::new();
    let plans = broker.data.join("runner-plans");
    if plans.is_dir() {
        let mut entries = tokio::fs::read_dir(&plans).await?;
        while let Some(entry) = entries.next_entry().await? {
            let name = entry.file_name().to_string_lossy().into_owned();
            let Some(attempt) = name.strip_suffix(".json").filter(|v| uuid(v).is_ok()) else {
                continue;
            };
            let value: Value = serde_json::from_slice(&tokio::fs::read(entry.path()).await?)?;
            if value["runId"] == run {
                // Fence a delayed start before removing its short-lived credentials.
                atomic_write(&broker.state.join(format!("{attempt}.stopped")), b"").await?;
                tokio::fs::remove_file(entry.path()).await?;
                attempts.insert(attempt.to_owned());
            }
        }
    }
    let mut entries = tokio::fs::read_dir(&broker.state).await?;
    while let Some(entry) = entries.next_entry().await? {
        let name = entry.file_name().to_string_lossy().into_owned();
        if let Some(attempt) = name.strip_suffix(".run").filter(|v| uuid(v).is_ok()) {
            if tokio::fs::read_to_string(entry.path()).await? == run {
                atomic_write(&broker.state.join(format!("{attempt}.stopped")), b"").await?;
                attempts.insert(attempt.to_owned());
                tokio::fs::remove_file(entry.path()).await?;
            }
            continue;
        }
        let Some(attempt) = name.strip_suffix(".vm.json").filter(|v| uuid(v).is_ok()) else {
            continue;
        };
        let value: Value = serde_json::from_slice(&tokio::fs::read(entry.path()).await?)?;
        if value["runId"] != run {
            continue;
        }
        atomic_write(&broker.state.join(format!("{attempt}.stopped")), b"").await?;
        attempts.insert(attempt.to_owned());
        let vm = text(&value, "vmId");
        if uuid(vm).is_ok() {
            for suffix in ["boot.log", "vmm.log"] {
                let path = broker.state.join(format!("{vm}.{suffix}"));
                match tokio::fs::remove_file(path).await {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(e.into()),
                }
            }
        }
        tokio::fs::remove_file(entry.path()).await?;
    }
    for attempt in attempts {
        match tokio::fs::remove_file(broker.state.join(format!("{attempt}.log"))).await {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}

async fn handler(State(broker): State<Broker>, request: Request) -> Result<Response> {
    if request.uri().path() == "/health" {
        let mut runtimes = Vec::new();
        if let Ok(mut entries) = tokio::fs::read_dir(broker.state.join("images")).await {
            while let Ok(Some(entry)) = entries.next_entry().await {
                if entry.path().join("root.ext4").exists() && entry.path().join("vmlinux").exists()
                {
                    runtimes.push(entry.file_name().to_string_lossy().into_owned());
                }
            }
        }
        return Ok(Json(json!({"status":"ok","backend":"firecracker","runtimeId":std::env::var("APP_RUNTIME_ID").unwrap_or_else(|_|"development".into()),"nodeProtocol":2,"runtimes":runtimes,"dataRoot":broker.data,"activeRuns":broker.active.lock().await.len(),"capabilities":crate::nodes::connector::capabilities(&broker.state).ok(),"pool":broker.pool.health().await})).into_response());
    }
    let credential = tokio::fs::read_to_string(broker.data.join("runner-secret")).await?;
    let authorization = request
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if !safe_equal(authorization, &format!("Bearer {}", credential.trim())) {
        return Err(Error::new(401, "Invalid runner credential."));
    }
    let segments = request
        .uri()
        .path()
        .trim_start_matches('/')
        .split('/')
        .collect::<Vec<_>>();
    if segments.first() == Some(&"archive-transfers") {
        return crate::nodes::archive::controller(&broker.data, request).await;
    }
    if let ["snapshots", snapshot, hash] = segments.as_slice() {
        uuid(snapshot)?;
        let directory = broker.state.join("snapshots").join(snapshot);
        if request.method() == "DELETE" && *hash == "discard" {
            tokio::fs::remove_dir_all(directory).await?;
            return Ok(Json(json!({"ok":true})).into_response());
        }
        if request.method() != "GET" {
            return Err(Error::new(405, "Method not allowed."));
        }
        let manifest: Value =
            serde_json::from_slice(&tokio::fs::read(directory.join("manifest.json")).await?)?;
        let bytes =
            crate::nodes::snapshots::block(&directory.join("disk"), &manifest, hash).await?;
        return Ok(([("content-length", bytes.len().to_string())], bytes).into_response());
    }
    if let ["disks", run, "restore"] = segments.as_slice() {
        let run = (*run).to_owned();
        if request.method() != "POST" {
            return Err(Error::new(405, "Method not allowed."));
        }
        let bytes = axum::body::to_bytes(
            request.into_body(),
            crate::nodes::snapshots::MAX_MANIFEST_BYTES,
        )
        .await
        .map_err(|_| Error::bad("Restore manifest too large."))?;
        let value = serde_json::from_slice(&bytes)?;
        return Ok(
            Json(crate::nodes::restore::controller(&broker.state, &run, value).await?)
                .into_response(),
        );
    }
    if let ["disks", run, action] = segments.as_slice() {
        let run = (*run).to_owned();
        let action = (*action).to_owned();
        if request.method() != "POST" || !["export", "import", "delete"].contains(&action.as_str())
        {
            return Err(Error::new(405, "Invalid workspace operation."));
        }
        uuid(&run)?;
        let bytes = axum::body::to_bytes(request.into_body(), 4096)
            .await
            .map_err(|_| Error::bad("Invalid workspace request."))?;
        let body: Value = serde_json::from_slice(&bytes)?;
        let active = broker.active.lock().await;
        if active.values().any(|a| a.plan["runId"] == run) {
            return Err(Error::new(409, "The workspace still has an active agent."));
        }
        let directory = broker.state.join("disks").join(&run);
        private_dir(&directory).await?;
        use std::os::fd::AsRawFd;
        let lock = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(directory.join("lock"))?;
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err(Error::new(409, "The workspace is still in use."));
        }
        let disk = directory.join("data.ext4");
        if action == "delete" {
            match tokio::fs::remove_file(&disk).await {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
            erase_attempt_content(&broker, &run).await?;
            let mut leftovers = tokio::fs::read_dir(&directory).await?;
            while let Some(entry) = leftovers.next_entry().await? {
                if entry.file_name() == "lock" {
                    continue;
                }
                if entry.file_type().await?.is_dir() {
                    tokio::fs::remove_dir_all(entry.path()).await?;
                } else {
                    tokio::fs::remove_file(entry.path()).await?;
                }
            }
            // Preserve the lock inode: an overlapping boot must contend on it.
            return Ok(Json(json!({"deleted":true})).into_response());
        }
        drop(active);
        let transfer = text(&body, "transfer");
        uuid(transfer)?;
        let staging = broker.data.join("archive-transfers").join(transfer);
        private_dir(&staging).await?;
        if tokio::fs::canonicalize(&staging).await? != staging {
            return Err(Error::bad("Invalid workspace transfer directory."));
        }
        if action == "export" {
            if disk.exists() {
                crate::archive_storage::tar(vec![
                    "--sparse".into(),
                    "-czf".into(),
                    staging.join("workspace.tar.gz").to_string_lossy().into(),
                    "-C".into(),
                    directory.to_string_lossy().into(),
                    "data.ext4".into(),
                ])
                .await?;
                std::fs::File::open(staging.join("workspace.tar.gz"))?.sync_all()?;
            }
        } else if !disk.exists() {
            if let Some(runtime) = body["runtimeId"].as_str() {
                if runtime.is_empty()
                    || runtime.len() > 100
                    || !runtime
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-')
                {
                    return Err(Error::bad("Invalid archived runtime."));
                }
                if !broker
                    .state
                    .join("images")
                    .join(runtime)
                    .join("root.ext4")
                    .exists()
                {
                    return Err(Error::new(409, "Archived VM runtime is unavailable."));
                }
                atomic_write(
                    &directory.join("runtime.json"),
                    &serde_json::to_vec(&json!({"runtimeId":runtime}))?,
                )
                .await?;
            }
            let target = directory.join(format!("restore-{transfer}"));
            if target.exists() {
                tokio::fs::remove_dir_all(&target).await?;
            }
            private_dir(&target).await?;
            crate::archive_storage::tar(vec![
                "--no-same-owner".into(),
                "-xzf".into(),
                staging.join("workspace.tar.gz").to_string_lossy().into(),
                "-C".into(),
                target.to_string_lossy().into(),
                "data.ext4".into(),
            ])
            .await?;
            let restored = target.join("data.ext4");
            let meta = tokio::fs::symlink_metadata(&restored).await?;
            if !meta.is_file() {
                return Err(Error::bad("Invalid restored workspace."));
            }
            std::fs::File::open(&restored)?.sync_all()?;
            tokio::fs::rename(restored, &disk).await?;
            std::fs::File::open(&directory)?.sync_all()?;
            tokio::fs::remove_dir_all(target).await?;
        }
        Ok(
            Json(json!({"ready":true,"archivePresent":staging.join("workspace.tar.gz").exists()}))
                .into_response(),
        )
    } else {
        let id = segments
            .get(1)
            .filter(|_| segments.first() == Some(&"runs"))
            .ok_or_else(|| Error::new(404, "Not found"))?;
        uuid(id)?;
        match (request.method().as_str(), segments.as_slice()) {
            ("POST", ["runs", id, "snapshot"]) => {
                let id = (*id).to_owned();
                let (run, socket, control, stop) = {
                    let active = broker.active.lock().await;
                    if let Some(attempt) = active.get(&id) {
                        (
                            text(&attempt.plan, "runId").to_owned(),
                            attempt.socket.get().cloned(),
                            attempt.control.clone(),
                            attempt.stop.clone(),
                        )
                    } else {
                        (
                            tokio::fs::read_to_string(broker.state.join(format!("{id}.run")))
                                .await?,
                            None,
                            Arc::new(Mutex::new(())),
                            CancellationToken::new(),
                        )
                    }
                };
                let state = broker.state.clone();
                let task = tokio::spawn(async move {
                    crate::nodes::checkpoint::capture(&state, &run, socket, control, stop, &id)
                        .await
                });
                Ok(Json(task.await.map_err(Error::internal)??).into_response())
            }
            ("POST", ["runs", id, "lease"]) => {
                let id = (*id).to_owned();
                let bytes = axum::body::to_bytes(request.into_body(), 1024)
                    .await
                    .map_err(|_| Error::bad("Invalid lease."))?;
                let value: Value = serde_json::from_slice(&bytes)?;
                let remaining = value["remainingMs"]
                    .as_u64()
                    .filter(|v| *v > 0 && *v <= 300000)
                    .ok_or_else(|| Error::bad("Invalid lease duration."))?;
                if broker.state.join(format!("{id}.stopped")).exists()
                    || broker.state.join(format!("{id}.exit")).exists()
                {
                    return Err(Error::new(409, "Attempt stopped."));
                }
                broker
                    .leases
                    .lock()
                    .await
                    .insert((*id).to_owned(), crate::nodes::boot_ms() + remaining);
                Ok(Json(json!({"ok":true})).into_response())
            }
            ("POST", ["runs", id, "artifact"]) => {
                let id = (*id).to_owned();
                let bytes = axum::body::to_bytes(request.into_body(), 16384)
                    .await
                    .map_err(|_| Error::bad("Invalid artifact request."))?;
                let value: Value = serde_json::from_slice(&bytes)?;
                let (socket, run, stop) = {
                    let active = broker.active.lock().await;
                    let attempt = active
                        .get(&id)
                        .ok_or_else(|| Error::new(409, "VM is not active."))?;
                    if attempt.plan["runId"] != value["runId"] {
                        return Err(Error::new(403, "Wrong artifact scope."));
                    }
                    (
                        attempt
                            .socket
                            .get()
                            .cloned()
                            .ok_or_else(|| Error::new(409, "VM is not ready."))?,
                        text(&attempt.plan, "runId").to_owned(),
                        attempt.stop.clone(),
                    )
                };
                let root = broker.data.join("runs").join(run);
                let (stream, size) = tokio::select! {
                    _=stop.cancelled()=>return Err(Error::new(409,"VM stopped.")),
                    result=tokio::time::timeout(Duration::from_secs(10),host::export_artifact(&socket,text(&value,"path"),&root))=>result.map_err(|_|Error::new(408,"Artifact export timed out."))??,
                };
                let stream = tokio_util::io::ReaderStream::new(stream.take(size))
                    .take_until(async move { stop.cancelled().await });
                Ok((
                    [(axum::http::header::CONTENT_LENGTH, size.to_string())],
                    Body::from_stream(stream),
                )
                    .into_response())
            }
            ("POST", ["runs", id]) => {
                broker.start(id).await?;
                Ok(Json(json!({})).into_response())
            }
            ("POST", ["runs", id, "projects", project_id]) => {
                let (id, project_id) = ((*id).to_owned(), (*project_id).to_owned());
                let bytes = axum::body::to_bytes(request.into_body(), 16384)
                    .await
                    .map_err(|_| Error::bad("Invalid project request."))?;
                let value = serde_json::from_slice(&bytes)?;
                Ok(Json(broker.open_project(&id, &project_id, value).await?).into_response())
            }
            ("DELETE", ["runs", id]) => {
                broker.stop(id).await?;
                Ok(Json(json!({})).into_response())
            }
            ("GET", ["runs", id, "logs"]) => {
                let id = (*id).to_owned();
                let stream = futures_util::stream::try_unfold(
                    (broker, id, 0_u64, Vec::new()),
                    |(broker, id, offset, mut pending)| async move {
                        let mut offset = offset;
                        loop {
                            if let Ok(mut file) =
                                tokio::fs::File::open(broker.state.join(format!("{id}.log"))).await
                            {
                                use tokio::io::AsyncSeekExt;
                                file.seek(std::io::SeekFrom::Start(offset)).await?;
                                let mut chunk = vec![0; 65536];
                                let count = file.read(&mut chunk).await?;
                                pending.extend_from_slice(&chunk[..count]);
                                offset += count as u64;
                            }
                            if let Some(end) = pending.iter().rposition(|b| *b == b'\n') {
                                let rest = pending.split_off(end + 1);
                                return Ok::<_, std::io::Error>(Some((
                                    Bytes::from(pending),
                                    (broker, id, offset, rest),
                                )));
                            }
                            if broker.state.join(format!("{id}.exit")).exists()
                                || broker.stop.is_cancelled()
                            {
                                return Ok(None);
                            }
                            tokio::time::sleep(Duration::from_millis(100)).await;
                            if pending.is_empty() {
                                return Ok(Some((
                                    Bytes::from_static(b"{\"type\":\"heartbeat\"}\n"),
                                    (broker, id, offset, pending),
                                )));
                            }
                        }
                    },
                );
                Ok((
                    [("content-type", "application/x-ndjson")],
                    Body::from_stream(stream),
                )
                    .into_response())
            }
            ("POST", ["runs", id, "wait"]) => {
                let id = (*id).to_owned();
                let stream = futures_util::stream::unfold(
                    (broker, id, false),
                    |(broker, id, done)| async move {
                        if done {
                            return None;
                        }
                        if let Ok(code) =
                            tokio::fs::read_to_string(broker.state.join(format!("{id}.exit"))).await
                        {
                            return Some((
                                Ok::<_, std::io::Error>(Bytes::from(
                                    json!({"StatusCode":code.parse::<i32>().unwrap_or(1)})
                                        .to_string(),
                                )),
                                (broker, id, true),
                            ));
                        }
                        tokio::time::sleep(Duration::from_secs(1)).await;
                        let stop = broker.stop.is_cancelled();
                        Some((Ok(Bytes::from_static(b" ")), (broker, id, stop)))
                    },
                );
                Ok((
                    [("content-type", "application/json")],
                    Body::from_stream(stream),
                )
                    .into_response())
            }
            _ => Err(Error::new(405, "Method not allowed.")),
        }
    }
}

pub async fn serve(stop: CancellationToken) -> Result<()> {
    let concurrency = crate::config::concurrency()?;
    let data = PathBuf::from(std::env::var("DATA_DIR").unwrap_or_else(|_| "/data".into()));
    let state =
        PathBuf::from(std::env::var("RUNNER_STATE_DIR").unwrap_or_else(|_| "/runner-state".into()));
    private_dir(&state).await?;
    let controller = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(state.join("controller.lock"))?;
    if unsafe { libc::flock(controller.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err(Error::new(503, "Another VM controller owns this storage."));
    }
    let image = host::assets(&state).await?;
    // A controller container restart fences its entire PID namespace. Mark interrupted attempts
    // exited, but retain all guest disks so the manager can launch replacement attempts.
    let mut files = tokio::fs::read_dir(&state).await?;
    while let Some(file) = files.next_entry().await? {
        if file.path().extension().is_some_and(|s| s == "active") {
            atomic_write(&file.path().with_extension("exit"), b"143").await?;
            tokio::fs::remove_file(file.path()).await?;
        }
    }
    let pool =
        crate::microvm::pool::Pool::new(state.clone(), image, stop.clone(), concurrency).await?;
    let preparing = pool.clone();
    let preparation = tokio::spawn(async move { preparing.maintain().await });
    let broker = Broker {
        data,
        state,
        pool,
        active: Default::default(),
        leases: Default::default(),
        stop: stop.clone(),
    };
    let draining = broker.clone();
    let address = std::env::var("RUNNER_BIND").unwrap_or_else(|_| "0.0.0.0:4311".into());
    let listener = tokio::net::TcpListener::bind(address).await?;
    axum::serve(
        listener,
        Router::new().fallback(any(handler)).with_state(broker),
    )
    .with_graceful_shutdown(stop.cancelled_owned())
    .await?;
    let attempts = draining
        .active
        .lock()
        .await
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    for id in attempts {
        draining.stop(&id).await?;
    }
    let _ = preparation.await;
    draining.pool.drain().await;
    drop(controller);
    Ok(())
}
use std::os::fd::AsRawFd;
pub async fn client(id: &str, stop: CancellationToken) -> Result<i32> {
    uuid(id)?;
    let base = std::env::var("RUNNER_URL").map_err(|_| Error::bad("Missing runner URL."))?;
    let token =
        std::env::var("RUNNER_TOKEN").map_err(|_| Error::bad("Missing runner credential."))?;
    let url = format!("{base}/runs/{id}");
    let http = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(Error::internal)?;
    let operation = async {
        let response = http
            .post(&url)
            .bearer_auth(&token)
            .timeout(Duration::from_secs(30))
            .send()
            .await
            .map_err(|_| Error::new(503, "VM controller could not start the run."))?;
        if !response.status().is_success() {
            return Err(Error::new(503, "VM controller could not start the run."));
        }
        let logs = http
            .get(format!("{url}/logs"))
            .bearer_auth(&token)
            .send()
            .await
            .map_err(|_| Error::new(503, "Could not read VM output."))?;
        if !logs.status().is_success() {
            return Err(Error::new(503, "Could not read VM output."));
        }
        let output = async {
            let stream = logs
                .bytes_stream()
                .map(|r| r.map_err(std::io::Error::other));
            let mut reader = BufReader::new(tokio_util::io::StreamReader::new(stream));
            let mut stdout = tokio::io::stdout();
            let mut stderr = tokio::io::stderr();
            while let Some(event) = wire::read(&mut reader).await.map_err(|error| {
                if error.status == 500 {
                    Error::new(503, "VM output connection was interrupted.")
                } else {
                    error
                }
            })? {
                if event["type"] == "heartbeat" {
                    continue;
                }
                let bytes = STANDARD
                    .decode(text(&event, "data"))
                    .map_err(|_| Error::bad("Invalid VM output."))?;
                if event["stderr"] == true {
                    stderr.write_all(&bytes).await?;
                } else {
                    stdout.write_all(&bytes).await?;
                }
            }
            Ok::<_, Error>(())
        };
        let wait = async {
            let response = http
                .post(format!("{url}/wait"))
                .bearer_auth(&token)
                .send()
                .await
                .map_err(|_| Error::new(503, "Could not wait for VM."))?;
            let value: Value = response
                .json()
                .await
                .map_err(|_| Error::new(503, "VM completion connection was interrupted."))?;
            value["StatusCode"]
                .as_i64()
                .filter(|n| (0..=255).contains(n))
                .map(|n| n as i32)
                .ok_or_else(|| Error::bad("Invalid VM exit status."))
        };
        let (_, code) = tokio::try_join!(output, wait)?;
        Ok(code)
    };
    let result = tokio::select! {_=stop.cancelled()=>Ok(143),result=operation=>result};
    let _ = http
        .delete(url)
        .bearer_auth(token)
        .timeout(Duration::from_secs(17))
        .send()
        .await;
    match result {
        Err(error) if error.status == 503 => {
            eprintln!("{}", error.message);
            Ok(CONTROLLER_INTERRUPTED)
        }
        Ok(143) if !stop.is_cancelled() => Ok(CONTROLLER_INTERRUPTED),
        result => result,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn workspace_disk_round_trip_uses_authenticated_http_and_preserves_the_lock() {
        use std::io::{Read, Seek, SeekFrom, Write};
        use std::os::unix::fs::MetadataExt;
        use tower::ServiceExt;
        let root = tempfile::TempDir::new().unwrap();
        let data = root.path().join("data");
        let state = root.path().join("state");
        private_dir(&data).await.unwrap();
        private_dir(&state).await.unwrap();
        std::fs::write(data.join("runner-secret"), "synthetic-runner-secret").unwrap();
        let stop = CancellationToken::new();
        let pool = crate::microvm::pool::Pool::new(
            state.clone(),
            root.path().join("unused-image"),
            stop.clone(),
            1,
        )
        .await
        .unwrap();
        let broker = Broker {
            data: data.clone(),
            state: state.clone(),
            pool,
            active: Default::default(),
            leases: Default::default(),
            stop,
        };
        let app = Router::new().fallback(any(handler)).with_state(broker);
        let run = crate::config::id();
        let failed_attempt = crate::config::id();
        std::fs::write(state.join(format!("{failed_attempt}.run")), &run).unwrap();
        std::fs::write(
            state.join(format!("{failed_attempt}.log")),
            "private agent output",
        )
        .unwrap();
        let transfer = crate::config::id();
        let directory = state.join("disks").join(&run);
        private_dir(&directory).await.unwrap();
        let mut file = std::fs::File::create(directory.join("data.ext4")).unwrap();
        file.seek(SeekFrom::Start(8 * 1024 * 1024)).unwrap();
        file.write_all(b"native session and unpublished work")
            .unwrap();
        file.sync_all().unwrap();
        private_dir(&data.join("archive-transfers").join(&transfer))
            .await
            .unwrap();
        async fn request(
            app: &Router,
            run: &str,
            action: &str,
            transfer: &str,
            credential: &str,
        ) -> u16 {
            app.clone()
                .oneshot(
                    axum::http::Request::builder()
                        .method("POST")
                        .uri(format!("/disks/{run}/{action}"))
                        .header("authorization", format!("Bearer {credential}"))
                        .body(Body::from(json!({"transfer":transfer}).to_string()))
                        .unwrap(),
                )
                .await
                .unwrap()
                .status()
                .as_u16()
        }
        assert_eq!(request(&app, &run, "export", &transfer, "wrong").await, 401);
        assert_eq!(
            request(&app, &run, "export", &transfer, "synthetic-runner-secret").await,
            200
        );
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(directory.join("lock"))
            .unwrap();
        assert_eq!(
            unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
            0
        );
        assert_eq!(
            request(&app, &run, "delete", &transfer, "synthetic-runner-secret").await,
            409
        );
        let inode = lock.metadata().unwrap().ino();
        drop(lock);
        assert_eq!(
            request(&app, &run, "delete", &transfer, "synthetic-runner-secret").await,
            200
        );
        assert!(!directory.join("data.ext4").exists());
        assert!(!state.join(format!("{failed_attempt}.log")).exists());
        assert!(state.join(format!("{failed_attempt}.stopped")).exists());
        assert_eq!(
            std::fs::metadata(directory.join("lock")).unwrap().ino(),
            inode
        );
        assert_eq!(
            request(&app, &run, "import", &transfer, "synthetic-runner-secret").await,
            200
        );
        let mut file = std::fs::File::open(directory.join("data.ext4")).unwrap();
        file.seek(SeekFrom::Start(8 * 1024 * 1024)).unwrap();
        let mut text = String::new();
        file.read_to_string(&mut text).unwrap();
        assert_eq!(text, "native session and unpublished work");
    }
    #[test]
    fn execution_plans_accept_unlimited_but_reject_invalid_or_expired_deadlines() {
        let id = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
        let mut plan = json!({"id":id,"runId":id,"expires":null,"cwd":format!("/data/runs/{id}/workspace"),"imports":[]});
        assert!(validate(&plan, id, Path::new("/data")).is_ok());
        for expiry in [
            json!(0),
            json!(now() - 1),
            json!(now() + 14 * 3600000),
            json!("unlimited"),
        ] {
            plan["expires"] = expiry;
            assert!(validate(&plan, id, Path::new("/data")).is_err());
        }
        plan.as_object_mut().unwrap().remove("expires");
        assert!(validate(&plan, id, Path::new("/data")).is_err());
    }
    #[test]
    fn imports_cannot_grant_host_or_other_run_access() {
        let id = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
        let mut plan = json!({"id":id,"runId":id,"expires":now()+60000,"cwd":format!("/data/runs/{id}/workspace"),"imports":[]});
        assert!(validate(&plan, id, Path::new("/data")).is_ok());
        for source in [
            "/data/private",
            "/home/node",
            "/data/runs/another/workspace",
            "/data/runs/../private",
        ] {
            plan["imports"] = json!([{"source":source,"target":"/home/node"}]);
            assert!(validate(&plan, id, Path::new("/data")).is_err());
        }
    }
}
