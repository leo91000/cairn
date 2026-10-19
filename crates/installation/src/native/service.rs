//! Native implementation of the existing authenticated node controller protocol.
use super::{atomic_write, systemd, workspace};
use crate::{
    error::{Error, Result},
    microvm::{plan::Plan, protocol::Event, wire},
    nodes::snapshots,
    skills::private_dir,
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
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    os::unix::fs::{FileTypeExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncSeekExt, AsyncWriteExt},
    sync::{Mutex, Notify},
};
use tokio_util::sync::CancellationToken;

const MAX_PLAN: u64 = 8_000_000;
const MAX_LOG: u64 = 64 * 1024 * 1024;

struct Attempt {
    run: String,
    stop: CancellationToken,
}

#[derive(Clone)]
struct Controller {
    data: PathBuf,
    state: PathBuf,
    active: Arc<Mutex<HashMap<String, Attempt>>>,
    leases: Arc<Mutex<HashMap<String, u64>>>,
    admission: Arc<Mutex<()>>,
    budget: Arc<Mutex<Value>>,
    changed: Arc<Notify>,
    log_lock: Arc<Mutex<()>>,
    stop: CancellationToken,
    identity: Arc<systemd::Identity>,
}

impl Controller {
    fn file(&self, id: &str, extension: &str) -> PathBuf {
        self.state.join(format!("{id}.{extension}"))
    }

    async fn plan(&self, id: &str) -> Result<Value> {
        uuid(id)?;
        let file = tokio::fs::File::open(self.data.join("runner-plans").join(format!("{id}.json")))
            .await?;
        let mut bytes = Vec::new();
        file.take(MAX_PLAN + 1).read_to_end(&mut bytes).await?;
        if bytes.len() as u64 > MAX_PLAN {
            return Err(Error::bad("Native plan exceeds limit."));
        }
        let plan = Plan::new(serde_json::from_slice(&bytes)?);
        let guest_data = guest_data(plan.as_value(), &self.data);
        let mut checked = plan.as_value().clone();
        for import in checked["imports"]
            .as_array_mut()
            .ok_or_else(|| Error::bad("Missing native imports."))?
        {
            let source = PathBuf::from(text(import, "source"));
            let relative = source
                .strip_prefix(&self.data)
                .map_err(|_| Error::bad("Native seed outside node storage."))?;
            import["source"] = guest_data
                .join(relative)
                .to_string_lossy()
                .into_owned()
                .into();
        }
        crate::runner::validate_plan(&Plan::new(checked), id, &guest_data)?;
        if plan.as_value()["backend"] != "native"
            || !plan.node_lease_required()
            || plan.as_value().get("command").is_some()
        {
            return Err(Error::bad(
                "Native execution requires an explicit managed plan and lease.",
            ));
        }
        let root = PathBuf::from(text(plan.as_value(), "projectRoot"));
        let run_root = guest_data.join("runs").join(plan.run_id());
        if !root.starts_with(&run_root) || root == run_root || !plan.cwd().starts_with(&root) {
            return Err(Error::bad("Invalid native project root."));
        }
        for import in plan.imports() {
            if tokio::fs::canonicalize(import.source).await? != import.source {
                return Err(Error::bad("Native import changed location."));
            }
        }
        Ok(plan.as_value().clone())
    }

    async fn leased(&self, id: &str) -> bool {
        self.leases
            .lock()
            .await
            .get(id)
            .is_some_and(|until| *until > crate::nodes::boot_ms())
    }

    async fn start(&self, id: &str) -> Result<()> {
        let _admission = self.admission.lock().await;
        if self.active.lock().await.contains_key(id) {
            return Ok(());
        }
        if self.file(id, "exit").exists() || self.file(id, "stopped").exists() {
            return Err(Error::conflict("Attempt has already stopped."));
        }
        if !self.leased(id).await {
            return Err(Error::conflict(
                "Native execution lease is missing or expired.",
            ));
        }
        if !self.active.lock().await.is_empty() {
            return Err(Error::unavailable("The native execution slot is occupied."));
        }
        let plan = self.plan(id).await?;
        let resources: crate::nodes::Resources = serde_json::from_value(plan["resources"].clone())?;
        resources.validate()?;
        let budget: crate::nodes::Resources =
            serde_json::from_value(self.budget.lock().await["limits"].clone())?;
        if resources.cpu > budget.cpu
            || resources.memory_mi_b > budget.memory_mi_b
            || resources.disk_mi_b > budget.disk_mi_b
        {
            return Err(Error::conflict(
                "Native plan exceeds the node owner's configured resource budget.",
            ));
        }
        let root = workspace::directory(&self.state, text(&plan, "runId"))?;
        let owner = workspace::execution_ownership(&self.state, text(&plan, "runId")).await?;
        let stop = self.stop.child_token();
        atomic_write(&self.file(id, "run"), text(&plan, "runId").as_bytes()).await?;
        atomic_write(&self.file(id, "active"), text(&plan, "runId").as_bytes()).await?;
        atomic_write(&self.file(id, "log"), b"").await?;
        self.active.lock().await.insert(
            id.into(),
            Attempt {
                run: text(&plan, "runId").into(),
                stop: stop.clone(),
            },
        );
        let (controller, id) = (self.clone(), id.to_owned());
        tokio::spawn(async move {
            let result = controller.execute(&id, &plan, stop).await;
            // The physical owner lock and active marker survive any uncertain stop.
            while let Err(error) = systemd::stop(&id).await {
                tracing::error!(attempt = id, message = %error.message, "Native workload fencing must be retried");
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
            let code = match result {
                Ok(code) => code,
                Err(error) => {
                    let message = format!("Native execution failed: {}\n", error.message);
                    let _ = controller.append(&id, true, message.as_bytes()).await;
                    crate::runner::CONTROLLER_FAILED
                }
            };
            let budget = plan["resources"]["diskMiB"]
                .as_u64()
                .unwrap_or(32768)
                .saturating_mul(1_048_576);
            let captured =
                workspace::capture(&controller.state, text(&plan, "runId"), &id, budget).await;
            if let Err(error) = captured {
                tracing::error!(attempt = id, message = %error.message, "Native checkpoint failed; local work retained");
                let _ =
                    atomic_write(&root.join("checkpoint-error"), error.message.as_bytes()).await;
            } else {
                let _ = tokio::fs::remove_file(root.join("checkpoint-error")).await;
            }
            // Deliver the legacy result path expected by the node connector.
            let result = root.join("data/output/result.md");
            if let Ok(file) = crate::artifacts::file::open_export(&result, &root.join("data")) {
                let file = tokio::fs::File::from_std(file);
                let mut bytes = Vec::new();
                if file.take(1_000_001).read_to_end(&mut bytes).await.is_ok()
                    && bytes.len() <= 1_000_000
                {
                    let target = controller
                        .data
                        .join("runs")
                        .join(text(&plan, "runId"))
                        .join("output/result.md");
                    let _ = atomic_write(&target, &bytes).await;
                }
            }
            // Release physical ownership before reporting completion to a replacement.
            drop(owner);
            let _ = atomic_write(&controller.file(&id, "exit"), code.to_string().as_bytes()).await;
            let _ = tokio::fs::remove_file(controller.file(&id, "active")).await;
            controller.active.lock().await.remove(&id);
            controller.leases.lock().await.remove(&id);
            controller.changed.notify_waiters();
        });
        Ok(())
    }

    async fn append(&self, id: &str, stderr: bool, bytes: &[u8]) -> Result<()> {
        let _log = self.log_lock.lock().await;
        let mut file = tokio::fs::OpenOptions::new()
            .append(true)
            .open(self.file(id, "log"))
            .await?;
        if file.metadata().await?.len() > MAX_LOG {
            return Err(Error::bad("Native output exceeded its bounded log budget."));
        }
        wire::write(
            &mut file,
            &serde_json::to_value(Event::Output {
                stderr,
                data: STANDARD.encode(bytes),
            })?,
        )
        .await?;
        file.flush().await?;
        self.changed.notify_waiters();
        Ok(())
    }

    async fn output(
        &self,
        id: &str,
        stderr: bool,
        mut reader: impl AsyncRead + Unpin,
    ) -> Result<()> {
        let mut buffer = vec![0u8; 16384];
        loop {
            let count = reader.read(&mut buffer).await?;
            if count == 0 {
                return Ok(());
            }
            self.append(id, stderr, &buffer[..count]).await?;
        }
    }

    async fn execute(&self, id: &str, plan: &Value, stop: CancellationToken) -> Result<i32> {
        let root = workspace::directory(&self.state, text(plan, "runId"))?;
        atomic_write(&root.join("attempt"), id.as_bytes()).await?;
        let data = root.join("data");
        let runtime = plan["runtimeId"]
            .as_str()
            .ok_or_else(|| Error::bad("Missing pinned native runtime."))?;
        super::systemd::tools(runtime)?;
        if !root.join("initialized").exists() {
            if !data.join(".cairn-seed-complete").exists() {
                if data.exists() {
                    return Err(Error::conflict(
                        "Incomplete native preparation retained local data; inspect it before replacing it.",
                    ));
                }
                let seed = tempfile::tempdir_in(&root)?;
                let staged = seed.path().join("data");
                private_dir(&staged).await?;
                for directory in ["home", "output", "tmp"] {
                    private_dir(&staged.join(directory)).await?;
                }
                let source = Plan::new(plan.clone())
                    .imports()
                    .find(|import| import.target == text(plan, "projectRoot"))
                    .map(|import| import.source.to_owned())
                    .ok_or_else(|| Error::bad("Missing native workspace seed."))?;
                workspace::copy(&source, &staged.join("workspaces"), self.disk_budget(plan))
                    .await?;
                workspace::sync_directories(&staged).await?;
                atomic_write(&staged.join(".cairn-seed-complete"), b"1").await?;
                tokio::fs::rename(staged, &data).await?;
                tokio::fs::File::open(&root).await?.sync_all().await?;
            }
            let runtime_record = json!({
                "backend": "native",
                "os": "linux",
                "arch": "x86_64",
                "runtimeId": runtime,
            });
            atomic_write(
                &root.join("runtime.json"),
                &serde_json::to_vec(&runtime_record)?,
            )
            .await?;
            atomic_write(&root.join("initialized"), b"1").await?;
        }
        let temporary = data.join("tmp");
        if temporary.exists() {
            tokio::fs::remove_dir_all(&temporary).await?;
        }
        for directory in ["home", "workspaces", "output", "tmp"] {
            private_dir(&data.join(directory)).await?;
        }
        let recorded: Value =
            serde_json::from_slice(&tokio::fs::read(root.join("runtime.json")).await?)?;
        if recorded["runtimeId"] != runtime {
            return Err(Error::conflict(
                "Native environment runtime changed. Start a new conversation.",
            ));
        }
        let stage = self.data.join("runs").join(text(plan, "runId"));
        install_home(&stage.join("home"), &data.join("home")).await?;
        workspace::own_tree(&data, self.identity.uid, self.identity.gid).await?;
        let container = self.state.join("roots").join(id);
        tokio::fs::create_dir_all(&container).await?;
        tokio::fs::set_permissions(&container, std::fs::Permissions::from_mode(0o755)).await?;
        for dir in [
            "etc",
            "dev",
            "proc",
            "sys",
            "run",
            "tmp",
            "var/tmp",
            "home/node",
        ] {
            tokio::fs::create_dir_all(container.join(dir)).await?;
        }
        let mut plan = plan.clone();
        let mcp_stop = stop.child_token();
        let _mcp_guard = mcp_stop.clone().drop_guard();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let origin = format!("http://127.0.0.1:{}", listener.local_addr()?.port());
        for field in ["codexConfig", "claudeMcps", "opencodeConfig", "args"] {
            if let Some(value) = plan["chat"].get_mut(field) {
                replace_origin(value, &origin);
            }
        }
        if let Some(value) = plan.get_mut("args") {
            replace_origin(value, &origin);
        }
        let channel = stage.join("home").join(crate::microvm::mcp::SOCKET);
        tokio::spawn(relay(listener, channel, mcp_stop));
        let auth = stage
            .join("home")
            .join(crate::nodes::workspace::provider_home(&plan))
            .join("cairn-auth.sock");
        if plan["chat"].is_object() {
            let _ = tokio::time::timeout(Duration::from_secs(3), async {
                while !auth.exists() {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            })
            .await;
        }
        let auth = if auth.exists() {
            std::os::unix::fs::lchown(&auth, Some(self.identity.uid), Some(self.identity.gid))?;
            Some(auth)
        } else {
            None
        };
        let plan_file = container.with_extension("plan.json");
        atomic_write(&plan_file, &serde_json::to_vec(&plan)?).await?;
        tokio::fs::set_permissions(&plan_file, std::fs::Permissions::from_mode(0o444)).await?;
        // The staging inbox is readonly in the workload's mount namespace.
        private_dir(&stage.join("chat-input")).await?;
        tokio::fs::set_permissions(
            stage.join("chat-input"),
            std::fs::Permissions::from_mode(0o755),
        )
        .await?;
        if stage.join("chat-input/messages.json").exists() {
            tokio::fs::set_permissions(
                stage.join("chat-input/messages.json"),
                std::fs::Permissions::from_mode(0o444),
            )
            .await?;
        }
        let launch = systemd::Launch {
            id,
            plan: &plan,
            data: &self.data,
            root: &container,
            home: &data.join("home"),
            workspace: &data.join("workspaces"),
            output: &data.join("output"),
            identity: &self.identity,
            auth: auth.as_deref(),
        };
        if !self.leased(id).await || stop.is_cancelled() {
            return Ok(143);
        }
        systemd::start_guardian(id, &self.file(id, "lease")).await?;
        let mut child = systemd::spawn(&launch)?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| Error::internal("Missing native output."))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| Error::internal("Missing native diagnostics."))?;
        let (out_self, err_self, out_id, err_id) =
            (self.clone(), self.clone(), id.to_owned(), id.to_owned());
        let mut out = tokio::spawn(async move { out_self.output(&out_id, false, stdout).await });
        let mut err = tokio::spawn(async move { err_self.output(&err_id, true, stderr).await });
        let mut timer = tokio::time::interval(Duration::from_secs(1));
        let mut readers = 0;
        let code = loop {
            tokio::select! {
                result = child.wait() => break result?.code().unwrap_or(143),
                () = stop.cancelled() => {
                    systemd::stop(id).await?;
                    break 143;
                }
                _ = timer.tick() => {
                    let expired = plan["expires"].as_i64().is_some_and(|until| crate::config::now() >= until);
                    let oversized = workspace::bytes(&data).await? > self.disk_budget(&plan);
                    if !self.leased(id).await || expired || oversized {
                        systemd::stop(id).await?;
                        break 143;
                    }
                }
                result = &mut out, if readers & 1 == 0 => {
                    result.map_err(Error::internal)??;
                    readers |= 1;
                }
                result = &mut err, if readers & 2 == 0 => {
                    result.map_err(Error::internal)??;
                    readers |= 2;
                }
            }
        };
        systemd::stop(id).await?;
        let _ = child.wait().await;
        if readers & 1 == 0 {
            out.await.map_err(Error::internal)??;
        }
        if readers & 2 == 0 {
            err.await.map_err(Error::internal)??;
        }
        let _ = tokio::fs::remove_file(plan_file).await;
        let _ = tokio::fs::remove_dir_all(container).await;
        Ok(code)
    }

    fn disk_budget(&self, plan: &Value) -> u64 {
        plan["resources"]["diskMiB"]
            .as_u64()
            .unwrap_or(32768)
            .saturating_mul(1_048_576)
    }

    async fn stop_attempt(&self, id: &str) -> Result<()> {
        uuid(id)?;
        let _admission = self.admission.lock().await;
        atomic_write(&self.file(id, "stopped"), b"1").await?;
        if let Some(attempt) = self.active.lock().await.get(id) {
            attempt.stop.cancel();
        }
        systemd::stop(id).await?;
        Ok(())
    }

    async fn wait(&self, id: &str) -> Result<Value> {
        loop {
            if !self.active.lock().await.contains_key(id)
                && let Ok(code) = tokio::fs::read_to_string(self.file(id, "exit")).await
            {
                let code = code.parse::<i32>().map_err(Error::internal)?;
                return Ok(json!({ "StatusCode": code }));
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
}

fn guest_data(plan: &Value, data: &Path) -> PathBuf {
    plan["guestDataRoot"]
        .as_str()
        .map_or_else(|| data.to_owned(), PathBuf::from)
}

async fn install_home(source: &Path, target: &Path) -> Result<()> {
    let agents = target.join(".agents");
    if let Ok(metadata) = tokio::fs::symlink_metadata(&agents).await {
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(Error::bad("Native skill root was redirected."));
        }
        let skills = agents.join("skills");
        if let Ok(metadata) = tokio::fs::symlink_metadata(&skills).await {
            if metadata.is_dir() && !metadata.file_type().is_symlink() {
                tokio::fs::remove_dir_all(skills).await?;
            } else {
                tokio::fs::remove_file(skills).await?;
            }
        }
    }
    // The source is generated by the manager. Refuse workload-controlled ancestors.
    let mut pending = vec![(source.to_owned(), target.to_owned())];
    while let Some((source, target)) = pending.pop() {
        let metadata = tokio::fs::symlink_metadata(&source).await?;
        if metadata.file_type().is_symlink() {
            return Err(Error::bad("Native runtime seed contains a symlink."));
        }
        if metadata.is_dir() {
            if let Ok(existing) = tokio::fs::symlink_metadata(&target).await {
                if !existing.is_dir() || existing.file_type().is_symlink() {
                    return Err(Error::bad("Native runtime home was redirected."));
                }
            } else {
                tokio::fs::create_dir(&target).await?;
            }
            let mut entries = tokio::fs::read_dir(source).await?;
            while let Some(entry) = entries.next_entry().await? {
                pending.push((entry.path(), target.join(entry.file_name())));
            }
        } else if metadata.is_file() {
            let bytes = tokio::fs::read(&source).await?;
            if let Ok(existing) = tokio::fs::symlink_metadata(&target).await {
                if existing.is_dir() {
                    return Err(Error::bad(
                        "Native runtime configuration was replaced by a directory.",
                    ));
                }
                tokio::fs::remove_file(&target).await?;
            }
            atomic_write(&target, &bytes).await?;
            let mode = if metadata.permissions().mode() & 0o111 != 0 {
                0o700
            } else {
                0o600
            };
            tokio::fs::set_permissions(&target, std::fs::Permissions::from_mode(mode)).await?;
        }
    }
    Ok(())
}

fn replace_origin(value: &mut Value, origin: &str) {
    match value {
        Value::String(text) => *text = text.replace(crate::microvm::mcp::ORIGIN, origin),
        Value::Array(values) => values
            .iter_mut()
            .for_each(|value| replace_origin(value, origin)),
        Value::Object(values) => values
            .values_mut()
            .for_each(|value| replace_origin(value, origin)),
        _ => {}
    }
}

async fn relay(listener: tokio::net::TcpListener, channel: PathBuf, stop: CancellationToken) {
    let _ = crate::microvm::listener::serve(
        || listener.accept(),
        |(mut client, _)| {
            let channel = channel.clone();
            async move {
                if let Ok(mut server) = tokio::net::UnixStream::connect(channel).await {
                    let _ = tokio::io::copy_bidirectional(&mut client, &mut server).await;
                }
            }
        },
        32,
        stop,
    )
    .await;
}

async fn input(request: Request, limit: usize) -> Result<Value> {
    let bytes = axum::body::to_bytes(request.into_body(), limit)
        .await
        .map_err(|_| Error::bad("Native request exceeds limit."))?;
    if bytes.is_empty() {
        return Ok(json!({}));
    }
    serde_json::from_slice(&bytes).map_err(Error::from)
}

async fn handler(State(controller): State<Controller>, request: Request) -> Result<Response> {
    if request.method() == "GET" {
        return route(controller, request).await;
    }
    // A disconnected caller must not release physical ownership while a restore,
    // stop or filesystem mutation is still executing.
    tokio::spawn(route(controller, request))
        .await
        .map_err(Error::internal)?
}

async fn route(controller: Controller, request: Request) -> Result<Response> {
    let path = request.uri().path().to_owned();
    let method = request.method().as_str().to_owned();
    if path == "/health" && method == "GET" {
        let capabilities = crate::nodes::connector::capabilities(&controller.state)?;
        return Ok(Json(json!({
            "status": "ok", "backend": "native", "nodeProtocol": 2,
            "runtimeId": std::env::var("APP_RUNTIME_ID").unwrap_or_else(|_| "development".into()),
            "dataRoot": controller.data, "capabilities": capabilities,
            "runtimes": systemd::runtimes()?,
            "pool": { "capacity": 1 }, "activeRuns": controller.active.lock().await.len(),
            "sharedResources": true, "exclusiveReservations": false, "budget": *controller.budget.lock().await,
            "usage": { "memoryMiB": 0, "diskMiB": 0 }, "pressure": null
        })).into_response());
    }
    let secret = tokio::fs::read_to_string(controller.data.join("runner-secret")).await?;
    let bearer = crate::nodes::bearer(request.headers()).unwrap_or("");
    if !crate::auth::safe_equal(bearer, secret.trim()) {
        return Err(Error::unauthorized("Invalid native controller credential."));
    }
    let parts = path.trim_start_matches('/').split('/').collect::<Vec<_>>();
    let value = match (method.as_str(), parts.as_slice()) {
        ("POST", ["node-budget"]) => {
            let value = input(request, 16384).await?;
            let resources: crate::nodes::Resources =
                serde_json::from_value(value["limits"].clone())?;
            resources.validate()?;
            if value["slots"] != 1 {
                return Err(Error::bad("Native nodes support one active execution."));
            }
            *controller.budget.lock().await = value;
            json!({ "ready": true })
        }
        ("POST", ["storage-policy"]) => {
            let _: crate::storage::policy::Policy =
                serde_json::from_value(input(request, 16384).await?)?;
            json!({ "ready": true })
        }
        ("POST", ["runs", id, "lease"]) => {
            uuid(id)?;
            let lease = input(request, 1024).await?;
            let ms = lease["remainingMs"]
                .as_u64()
                .filter(|ms| (1..=300_000).contains(ms))
                .ok_or_else(|| Error::bad("Invalid native lease."))?;
            if controller.file(id, "exit").exists() || controller.file(id, "stopped").exists() {
                return Err(Error::conflict("Attempt stopped."));
            }
            let until = crate::nodes::boot_ms().saturating_add(ms);
            super::watchdog::grant(&controller.file(id, "lease"), until).await?;
            controller.leases.lock().await.insert((*id).into(), until);
            json!({ "ok": true })
        }
        ("POST", ["runs", id]) => {
            controller.start(id).await?;
            json!({})
        }
        ("DELETE", ["runs", id]) => {
            controller.stop_attempt(id).await?;
            json!({})
        }
        ("POST", ["runs", id, "wait"]) => {
            uuid(id)?;
            controller.wait(id).await?
        }
        ("GET", ["runs", id, "administrative-pause"]) => {
            uuid(id)?;
            json!({ "paused": false, "administrativePausedMs": 0 })
        }
        ("GET", ["runs", id, "logs"]) => {
            uuid(id)?;
            let (mut writer, reader) = tokio::io::duplex(65536);
            let (controller, id) = (controller.clone(), (*id).to_owned());
            tokio::spawn(async move {
                let Ok(mut file) = tokio::fs::File::open(controller.file(&id, "log")).await else {
                    return;
                };
                let mut offset = 0;
                let mut buffer = vec![0u8; 65536];
                loop {
                    if file.seek(std::io::SeekFrom::Start(offset)).await.is_err() {
                        break;
                    }
                    match file.read(&mut buffer).await {
                        Ok(0) => {
                            if !controller.active.lock().await.contains_key(&id) {
                                break;
                            }
                            tokio::time::sleep(Duration::from_millis(100)).await;
                        }
                        Ok(count) => {
                            if writer.write_all(&buffer[..count]).await.is_err() {
                                break;
                            }
                            offset += count as u64;
                        }
                        Err(_) => break,
                    }
                }
            });
            return Ok(Body::from_stream(tokio_util::io::ReaderStream::new(reader)).into_response());
        }
        ("POST", ["disks", run, "storage-status"]) => {
            uuid(run)?;
            let root = workspace::directory(&controller.state, run)?;
            let active_local = workspace::bytes(&root.join("data")).await.unwrap_or(0);
            let local = workspace::retained_bytes(&controller.state, run).await?;
            let active = controller
                .active
                .lock()
                .await
                .values()
                .any(|attempt| attempt.run == *run);
            let completed = workspace::completed(&controller.state, run).await.ok();
            let published: Value = match tokio::fs::read(root.join("published.json")).await {
                Ok(bytes) => serde_json::from_slice(&bytes)?,
                Err(_) => Value::Null,
            };
            let dirty = active
                || completed
                    .as_ref()
                    .is_none_or(|point| point["id"] != published["snapshotId"]);
            json!({ "mode": "native", "localBytes": local, "activeLocalBytes": active_local,
                "dirtyBytes": if dirty { local.max(1) } else { 0 }, "waitingFor": null,
                "checkpointConsistency": "stopped", "published": published })
        }
        ("POST", ["disks", run, "published"]) => {
            uuid(run)?;
            let receipt = input(request, 16384).await?;
            uuid(text(&receipt, "backupId"))?;
            uuid(text(&receipt, "snapshotId"))?;
            let root = workspace::directory(&controller.state, run)?;
            let completed = workspace::completed(&controller.state, run).await?;
            if completed["id"] != receipt["snapshotId"] {
                return Err(Error::conflict(
                    "Native checkpoint changed before publication acknowledgement.",
                ));
            }
            atomic_write(&root.join("published.json"), &serde_json::to_vec(&receipt)?).await?;
            // Acknowledging the latest durable backup makes older stopped archives
            // redundant. Never collect another conversation's or the current archive.
            workspace::collect_archives(&controller.state, run, Some(text(&completed, "id")))
                .await?;
            json!({ "committed": true })
        }
        ("POST", ["disks", run, "snapshot-completed"]) => {
            uuid(run)?;
            workspace::completed(&controller.state, run).await?
        }
        ("POST", ["disks", run, "snapshot"]) => {
            uuid(run)?;
            let root = workspace::directory(&controller.state, run)?;
            let _owner = crate::file_lock::exclusive(
                &root.join("owner.lock"),
                "Native workspace is active.",
            )?;
            let attempt = tokio::fs::read_to_string(root.join("attempt")).await?;
            let budget = controller.budget.lock().await["limits"]["diskMiB"]
                .as_u64()
                .unwrap_or(32768)
                .saturating_mul(1_048_576);
            workspace::capture(&controller.state, run, &attempt, budget).await?
        }
        ("POST", ["runs", id, "snapshot"]) => {
            uuid(id)?;
            if controller.active.lock().await.contains_key(*id) {
                return Err(Error::conflict(
                    "Native checkpoints require the workload to stop.",
                ));
            }
            let run = tokio::fs::read_to_string(controller.file(id, "run")).await?;
            workspace::completed(&controller.state, &run).await?
        }
        (_, ["snapshots", snapshot, operation]) => {
            uuid(snapshot)?;
            let directory = controller.state.join("snapshots").join(snapshot);
            match (method.as_str(), *operation) {
                ("DELETE", "discard") => {
                    tokio::fs::remove_dir_all(directory).await?;
                    json!({ "ok": true })
                }
                ("POST", "blocks" | "publication") => {
                    let value = input(request, snapshots::MAX_MANIFEST_BYTES).await?;
                    let hashes = serde_json::from_value(value["hashes"].clone())?;
                    let (length, body) = snapshots::served_publication(&directory, hashes).await?;
                    return Ok(([("content-length", length.to_string())], body).into_response());
                }
                ("GET", hash) => {
                    return Ok(snapshots::served(&directory, hash).await?.into_response());
                }
                _ => {
                    return Err(Error::method_not_allowed(
                        "Invalid native snapshot operation.",
                    ));
                }
            }
        }
        ("POST", ["disks", run, "restore"]) => {
            uuid(run)?;
            let request = input(request, snapshots::MAX_MANIFEST_BYTES).await?;
            let limit = controller.budget.lock().await["limits"]["diskMiB"]
                .as_u64()
                .unwrap_or(32768)
                * 1_048_576;
            workspace::restore(&controller.state, run, &request, limit).await?
        }
        ("POST", ["disks", run, action @ ("delete" | "prune")]) => {
            uuid(run)?;
            let root = workspace::directory(&controller.state, run)?;
            private_dir(&root).await?;
            let _owner = crate::file_lock::exclusive(
                &root.join("owner.lock"),
                "Native workspace is active.",
            )?;
            if root.join("restore.pending.json").exists() && *action == "prune" {
                return Err(Error::conflict(
                    "Finish native restore before pruning retained local work.",
                ));
            }
            let mut entries = tokio::fs::read_dir(&root).await?;
            while let Some(entry) = entries.next_entry().await? {
                if (entry.file_name().to_string_lossy().starts_with("stale-")
                    || entry.file_name().to_string_lossy().starts_with("restore-"))
                    && entry.file_type().await?.is_dir()
                {
                    tokio::fs::remove_dir_all(entry.path()).await?;
                }
            }
            if *action == "delete" {
                for entry in [
                    "data",
                    "completed.json",
                    "initialized",
                    "attempt",
                    "restored.json",
                    "checkpoint-error",
                    "published.json",
                    "runtime.json",
                    "restore.pending.json",
                ] {
                    let path = root.join(entry);
                    if let Ok(meta) = tokio::fs::symlink_metadata(&path).await {
                        if meta.is_dir() && !meta.file_type().is_symlink() {
                            tokio::fs::remove_dir_all(path).await?;
                        } else {
                            tokio::fs::remove_file(path).await?;
                        }
                    }
                }
            }
            if *action == "delete" {
                workspace::collect_archives(&controller.state, run, None).await?;
                let stage = controller.data.join("runs").join(run);
                if stage.exists() {
                    tokio::fs::remove_dir_all(stage).await?;
                }
                let mut entries = tokio::fs::read_dir(&controller.state).await?;
                while let Some(entry) = entries.next_entry().await? {
                    if entry
                        .path()
                        .extension()
                        .is_some_and(|extension| extension == "run")
                        && tokio::fs::read_to_string(entry.path())
                            .await
                            .is_ok_and(|owner| owner == *run)
                    {
                        let id = entry
                            .path()
                            .file_stem()
                            .unwrap()
                            .to_string_lossy()
                            .into_owned();
                        uuid(&id)?;
                        for extension in ["log", "lease"] {
                            let _ = tokio::fs::remove_file(controller.file(&id, extension)).await;
                        }
                        let _ = tokio::fs::remove_file(
                            controller
                                .state
                                .join("roots")
                                .join(&id)
                                .with_extension("plan.json"),
                        )
                        .await;
                        let _ = tokio::fs::remove_dir_all(controller.state.join("roots").join(&id))
                            .await;
                        let _ = tokio::fs::remove_file(
                            controller
                                .data
                                .join("runner-plans")
                                .join(format!("{id}.json")),
                        )
                        .await;
                    }
                }
            }
            json!({ "deleted": *action == "delete", "pruned": *action == "prune", "retiredGrants": [] })
        }
        ("POST", ["runs", id, "artifact"]) => {
            uuid(id)?;
            let value = input(request, 16384).await?;
            let active = controller.active.lock().await;
            let attempt = active
                .get(*id)
                .ok_or_else(|| Error::conflict("Native attempt is not active."))?;
            if value["runId"] != attempt.run {
                return Err(Error::forbidden("Wrong native artifact scope."));
            }
            let plan = controller.plan(id).await?;
            let root = workspace::directory(&controller.state, &attempt.run)?.join("data");
            let path = Path::new(text(&value, "path"));
            let project_root = Path::new(text(&plan, "projectRoot"));
            let source = if let Ok(relative) = path.strip_prefix(project_root) {
                root.join("workspaces").join(relative)
            } else if let Ok(relative) = path.strip_prefix(
                guest_data(&plan, &controller.data)
                    .join("runs")
                    .join(&attempt.run)
                    .join("output"),
            ) {
                root.join("output").join(relative)
            } else if let Ok(relative) = path.strip_prefix("/tmp") {
                root.join("tmp").join(relative)
            } else {
                return Err(Error::forbidden(
                    "Native artifacts must be in this conversation's workspace, output directory or /tmp.",
                ));
            };
            let (snapshot, size) = crate::artifacts::file::snapshot(&source, &root).await?;
            drop(active);
            let file = tokio::fs::File::from_std(snapshot.reopen()?);
            let stream = futures_util::stream::try_unfold(
                (file, snapshot),
                |(mut file, snapshot)| async move {
                    let mut bytes = vec![0; 65536];
                    let count = file.read(&mut bytes).await?;
                    bytes.truncate(count);
                    Ok::<_, std::io::Error>((count > 0).then_some((bytes, (file, snapshot))))
                },
            );
            return Ok((
                [("content-length", size.to_string())],
                Body::from_stream(stream),
            )
                .into_response());
        }
        ("POST", ["runs", id, "projects", project]) => {
            uuid(id)?;
            uuid(project)?;
            let value = input(request, 16384).await?;
            let active = controller.active.lock().await;
            let attempt = active
                .get(*id)
                .ok_or_else(|| Error::conflict("Native attempt is not active."))?;
            if value["runId"] != attempt.run {
                return Err(Error::forbidden("Wrong native project scope."));
            }
            let plan = controller.plan(id).await?;
            let source = controller
                .data
                .join("runs")
                .join(&attempt.run)
                .join("workspace")
                .join(project);
            let expected = Path::new(text(&plan, "projectRoot")).join(project);
            if value["source"]
                != guest_data(&plan, &controller.data)
                    .join("runs")
                    .join(&attempt.run)
                    .join("workspace")
                    .join(project)
                    .to_string_lossy()
                    .as_ref()
                || value["target"] != expected.to_string_lossy().as_ref()
            {
                return Err(Error::forbidden("Invalid native project path."));
            }
            let target = workspace::directory(&controller.state, &attempt.run)?
                .join("data/workspaces")
                .join(project);
            if !target.exists() {
                let root = workspace::directory(&controller.state, &attempt.run)?;
                let staged = tempfile::tempdir_in(&root)?;
                let received = staged.path().join("project");
                workspace::copy(&source, &received, controller.disk_budget(&plan)).await?;
                workspace::own_tree(&received, controller.identity.uid, controller.identity.gid)
                    .await?;
                tokio::fs::rename(received, &target).await?;
            }
            json!({ "ok": true })
        }
        _ => {
            return Err(Error::not_found(
                "Native controller operation is not supported.",
            ));
        }
    };
    Ok(Json(value).into_response())
}

async fn bind_controller(path: &Path) -> Result<tokio::net::UnixListener> {
    if let Ok(metadata) = tokio::fs::symlink_metadata(path).await {
        if !metadata.file_type().is_socket() {
            return Err(Error::bad(
                "Native controller socket path is occupied by another file.",
            ));
        }
        tokio::fs::remove_file(path).await?;
    }
    let listener = tokio::net::UnixListener::bind(path)?;
    tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).await?;
    Ok(listener)
}

pub async fn serve(stop: CancellationToken) -> Result<()> {
    let data = crate::nodes::node_data_dir();
    let state = super::state_dir();
    private_dir(&state).await?;
    let _lock = crate::file_lock::exclusive(
        &state.join("controller.lock"),
        "Another native controller owns this storage.",
    )?;
    let identity = Arc::new(systemd::identity().await?);
    private_dir(&state.join("roots")).await?;
    let mut entries = tokio::fs::read_dir(&state).await?;
    while let Some(entry) = entries.next_entry().await? {
        if entry
            .path()
            .extension()
            .is_some_and(|extension| extension == "active")
        {
            let id = entry
                .path()
                .file_stem()
                .unwrap()
                .to_string_lossy()
                .into_owned();
            uuid(&id)?;
            systemd::stop(&id).await?;
            atomic_write(
                &entry.path().with_extension("exit"),
                crate::runner::CONTROLLER_INTERRUPTED.to_string().as_bytes(),
            )
            .await?;
            tokio::fs::remove_file(entry.path()).await?;
        }
    }
    let controller = Controller {
        data,
        state,
        active: Arc::default(),
        leases: Arc::default(),
        admission: Arc::default(),
        budget: Arc::new(Mutex::new(
            json!({ "slots": 1, "limits": crate::nodes::placement::defaults() }),
        )),
        changed: Arc::default(),
        log_lock: Arc::default(),
        stop: stop.clone(),
        identity,
    };
    let socket = controller.state.join("controller.sock");
    let listener = bind_controller(&socket).await?;
    let router = Router::new()
        .fallback(any(handler))
        .with_state(controller.clone());
    let server_stop = stop.clone();
    let mut server = tokio::spawn(async move {
        axum::serve(listener, router)
            .with_graceful_shutdown(server_stop.cancelled_owned())
            .await
    });
    let result = tokio::select! {
        () = stop.cancelled() => None,
        result = &mut server => { stop.cancel(); Some(result) }
    };
    let active: Vec<_> = controller.active.lock().await.keys().cloned().collect();
    for id in active {
        controller.stop_attempt(&id).await?;
    }
    while !controller.active.lock().await.is_empty() {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    match result {
        Some(result) => result.map_err(Error::internal)??,
        None => server.await.map_err(Error::internal)??,
    }
    tokio::fs::remove_file(socket).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn fixture() -> (tempfile::TempDir, Controller) {
        let root = tempfile::tempdir().unwrap();
        let data = root.path().join("data");
        let state = root.path().join("state");
        private_dir(&data).await.unwrap();
        private_dir(&state).await.unwrap();
        atomic_write(
            &data.join("runner-secret"),
            b"synthetic-private-controller-token",
        )
        .await
        .unwrap();
        let controller = Controller {
            data,
            state,
            active: Arc::default(),
            leases: Arc::default(),
            admission: Arc::default(),
            budget: Arc::new(Mutex::new(
                json!({ "slots": 1, "limits": { "cpu": 1, "memoryMiB": 1024, "diskMiB": 1024 } }),
            )),
            changed: Arc::default(),
            log_lock: Arc::default(),
            stop: CancellationToken::new(),
            identity: Arc::new(systemd::Identity {
                user: "synthetic-workload".into(),
                uid: 65534,
                gid: 65534,
            }),
        };
        (root, controller)
    }

    fn request(path: &str, body: &Value) -> Request {
        Request::builder()
            .method("POST")
            .uri(path)
            .header("authorization", "Bearer synthetic-private-controller-token")
            .body(Body::from(body.to_string()))
            .unwrap()
    }

    #[tokio::test]
    async fn private_socket_routes_ignore_a_spoofed_tcp_controller() {
        let (_root, controller) = fixture().await;
        let socket = controller.state.join("controller.sock");
        let listener = bind_controller(&socket).await.unwrap();
        let spoof = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", spoof.local_addr().unwrap());
        let router = Router::new().fallback(any(handler)).with_state(controller);
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let response = super::super::controller_client(&socket)
            .unwrap()
            .post(format!("{origin}/node-budget"))
            .bearer_auth("synthetic-private-controller-token")
            .json(&json!({
                "slots": 1,
                "limits": { "cpu": 1, "memoryMiB": 1024, "diskMiB": 1024 }
            }))
            .send()
            .await
            .unwrap();
        assert!(response.status().is_success());
        assert_eq!(
            std::fs::metadata(&socket).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(50), spoof.accept())
                .await
                .is_err()
        );
        server.abort();
        let _ = server.await;
    }

    #[tokio::test]
    async fn controller_socket_never_replaces_an_unrelated_file() {
        let (_root, controller) = fixture().await;
        let socket = controller.state.join("controller.sock");
        atomic_write(&socket, b"preserved owner data")
            .await
            .unwrap();
        assert!(bind_controller(&socket).await.is_err());
        assert_eq!(
            tokio::fs::read(socket).await.unwrap(),
            b"preserved owner data"
        );
    }

    #[tokio::test]
    async fn frozen_skill_copy_preserves_approved_executable_bits() {
        let root = tempfile::tempdir().unwrap();
        let (source, target) = (root.path().join("seed"), root.path().join("home"));
        private_dir(&source.join(".agents/skills/game/scripts"))
            .await
            .unwrap();
        private_dir(&target).await.unwrap();
        let relative = ".agents/skills/game/scripts/build";
        atomic_write(&source.join(relative), b"#!/bin/sh\nexit 0\n")
            .await
            .unwrap();
        tokio::fs::set_permissions(
            source.join(relative),
            std::fs::Permissions::from_mode(0o700),
        )
        .await
        .unwrap();
        atomic_write(
            &source.join(".agents/skills/game/SKILL.md"),
            b"Use the build script",
        )
        .await
        .unwrap();
        install_home(&source, &target).await.unwrap();
        assert_eq!(
            tokio::fs::metadata(target.join(relative))
                .await
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            tokio::fs::metadata(target.join(".agents/skills/game/SKILL.md"))
                .await
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert!(
            tokio::process::Command::new(target.join(relative))
                .status()
                .await
                .unwrap()
                .success()
        );
    }

    #[tokio::test]
    async fn private_controller_routes_require_the_node_credential() {
        let (_root, controller) = fixture().await;
        let response = route(
            controller,
            Request::builder()
                .method("POST")
                .uri("/node-budget")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap_err();
        assert_eq!(response.status, 401);
    }

    #[tokio::test]
    async fn expired_or_stopped_attempts_cannot_start_or_regain_a_lease() {
        let (_root, controller) = fixture().await;
        let attempt = crate::config::id();
        assert_eq!(controller.start(&attempt).await.unwrap_err().status, 409);
        atomic_write(&controller.file(&attempt, "stopped"), b"1")
            .await
            .unwrap();
        let error = route(
            controller.clone(),
            request(
                &format!("/runs/{attempt}/lease"),
                &json!({ "remainingMs": 60000 }),
            ),
        )
        .await
        .unwrap_err();
        assert_eq!(error.status, 409);
        assert!(!controller.leased(&attempt).await);
    }

    #[tokio::test]
    async fn deleting_a_managed_workspace_does_not_follow_its_symlink_or_remove_unrelated_files() {
        let (_root, controller) = fixture().await;
        let outside = tempfile::tempdir().unwrap();
        tokio::fs::write(outside.path().join("keep"), b"owner files")
            .await
            .unwrap();
        let run = crate::config::id();
        let directory = workspace::directory(&controller.state, &run).unwrap();
        private_dir(&directory).await.unwrap();
        std::os::unix::fs::symlink(outside.path(), directory.join("data")).unwrap();
        tokio::fs::write(directory.join("unrelated"), b"preserved")
            .await
            .unwrap();
        route(
            controller,
            request(&format!("/disks/{run}/delete"), &json!({})),
        )
        .await
        .unwrap();
        assert_eq!(
            tokio::fs::read(outside.path().join("keep")).await.unwrap(),
            b"owner files"
        );
        assert!(!directory.join("data").exists());
        assert!(directory.join("unrelated").exists());
    }

    #[tokio::test]
    async fn native_budget_refuses_concurrent_execution_slots() {
        let (_root, controller) = fixture().await;
        let error = route(
            controller,
            request(
                "/node-budget",
                &json!({ "slots": 2, "limits": { "cpu": 1, "memoryMiB": 1024, "diskMiB": 1024 } }),
            ),
        )
        .await
        .unwrap_err();
        assert_eq!(error.status, 400);
    }
}
