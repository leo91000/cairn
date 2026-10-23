//! Native implementation of the existing authenticated node controller protocol.
use super::{atomic_write, block, disk, systemd, workspace};
use crate::{
    error::{Error, Result},
    microvm::{plan::Plan, protocol::Event, wire},
    nodes::snapshots,
    skills::private_dir,
    storage,
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

fn native_budget(value: &Value) -> Result<crate::microvm::budget::Budget> {
    let budget: crate::microvm::budget::Budget = serde_json::from_value(value.clone())?;
    budget.limits.validate()?;
    if !(1..=4096).contains(&budget.slots) {
        return Err(Error::bad("Use between 1 and 4096 execution slots."));
    }
    Ok(budget)
}

struct Attempt {
    run: String,
    stop: CancellationToken,
    started: bool,
    protection: Option<tokio::task::JoinHandle<()>>,
}

#[derive(Clone)]
struct Controller {
    data: PathBuf,
    state: PathBuf,
    active: Arc<Mutex<HashMap<String, Attempt>>>,
    mounted: Arc<Mutex<HashMap<String, Arc<block::Handle>>>>,
    leases: Arc<Mutex<HashMap<String, u64>>>,
    admission: Arc<Mutex<()>>,
    base_warmer: Arc<super::bases_warmer::Warmer>,
    budget: Arc<Mutex<Value>>,
    changed: Arc<Notify>,
    log_lock: Arc<Mutex<()>>,
    stop: CancellationToken,
    identity: Arc<systemd::Identity>,
}

impl Controller {
    fn bases(&self) -> super::bases::Bases {
        super::bases::Bases {
            data: self.data.clone(),
            state: self.state.clone(),
            identity: (self.identity.uid, self.identity.gid),
        }
    }

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
        if self.active.lock().await.len() as u64
            >= self.budget.lock().await["slots"].as_u64().unwrap_or(0)
        {
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
        let _disk = disk::lock(&self.state).await?;
        self.check_preparation_space(&plan).await?;
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
                started: false,
                protection: None,
            },
        );
        let (controller, id) = (self.clone(), id.to_owned());
        let (lease_controller, lease_id, lease_stop) =
            (controller.clone(), id.clone(), stop.clone());
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    () = lease_stop.cancelled() => {},
                    () = tokio::time::sleep(Duration::from_millis(100)) => {
                        if lease_controller.leased(&lease_id).await { continue; }
                        lease_stop.cancel();
                    }
                }
                let preparation = lease_controller
                    .active
                    .lock()
                    .await
                    .get(&lease_id)
                    .is_some_and(|attempt| !attempt.started);
                if preparation {
                    let _ = lease_controller.abort_disk(&lease_id).await;
                }
                break;
            }
        });
        tokio::spawn(async move {
            let result = controller.execute(&id, &plan, stop.clone()).await;
            // Reject in-flight file mutations before flushing/unmounting. Any
            // already admitted copy retains the global mutation lock.
            stop.cancel();
            // The physical owner lock and active marker survive any uncertain stop.
            while let Err(error) = systemd::stop(&id).await {
                let _ = controller.abort_disk(&id).await;
                tracing::error!(attempt = id, message = %error.message, "Native workload fencing must be retried");
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
            let protection = controller
                .active
                .lock()
                .await
                .get_mut(&id)
                .and_then(|attempt| attempt.protection.take());
            if let Some(protection) = protection {
                // A late freeze/thaw must finish before this mount's leases or
                // volume can be released to a following attempt.
                let _ = protection.await;
            }
            let code = match result {
                Ok(code) => code,
                Err(error) => {
                    let message = format!("Native execution failed: {}\n", error.message);
                    let _ = controller.append(&id, true, message.as_bytes()).await;
                    crate::runner::CONTROLLER_FAILED
                }
            };
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
            let native_block =
                block::exists(&controller.state, text(&plan, "runId")).unwrap_or(true);
            if native_block {
                let mounted = {
                    controller
                        .mounted
                        .lock()
                        .await
                        .get(text(&plan, "runId"))
                        .cloned()
                };
                let had_mount = mounted.is_some();
                if let Some(mounted) = mounted {
                    loop {
                        let result = async {
                            let _disk = disk::lock(&controller.state).await?;
                            mounted.mount.lock().await.close().await
                        }
                        .await;
                        match result {
                            Ok(()) => break,
                            Err(error) => {
                                tracing::error!(attempt = id, %error, "Native filesystem must finish flushing and unmounting before ownership release");
                                tokio::time::sleep(Duration::from_secs(2)).await;
                            }
                        }
                    }
                }
                controller.mounted.lock().await.remove(text(&plan, "runId"));
                while !had_mount
                    && let Err(error) =
                        block::recover_completed(&controller.state, text(&plan, "runId"), &id).await
                {
                    tracing::error!(attempt = id, %error, "Native completed generation must be recovered before ownership release");
                    tokio::time::sleep(Duration::from_secs(2)).await;
                }
            } else {
                let budget = plan["resources"]["diskMiB"]
                    .as_u64()
                    .unwrap_or(32768)
                    .saturating_mul(1_048_576);
                let captured = controller
                    .capture_checkpoint(text(&plan, "runId"), &id, budget)
                    .await;
                if let Err(error) = captured {
                    tracing::error!(attempt = id, message = %error.message, "Native checkpoint failed; local work retained");
                    let _ = atomic_write(&root.join("checkpoint-error"), error.message.as_bytes())
                        .await;
                } else {
                    let _ = tokio::fs::remove_file(root.join("checkpoint-error")).await;
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
        let disk_owner = disk::lock(&self.state).await?;
        self.check_preparation_space(plan).await?;
        let root = workspace::directory(&self.state, text(plan, "runId"))?;
        atomic_write(&root.join("attempt"), id.as_bytes()).await?;
        atomic_write(
            &root.join("resources.json"),
            &serde_json::to_vec(&plan["resources"])?,
        )
        .await?;
        let data = root.join("data");
        let runtime = plan["runtimeId"]
            .as_str()
            .ok_or_else(|| Error::bad("Missing pinned native runtime."))?;
        super::systemd::tools(runtime)?;
        let from_base = self.bases().initialize(plan, &stop).await?;
        let mounted = block::mount(&self.state, text(plan, "runId"), plan, &stop).await?;
        self.mounted.lock().await.insert(
            text(plan, "runId").to_owned(),
            Arc::new(block::Handle::new(mounted)),
        );
        if stop.is_cancelled() {
            return Ok(143);
        }
        block::initialize(
            &self.state,
            text(plan, "runId"),
            plan,
            self.disk_budget(plan).min(self.disk_growth().await?),
        )
        .await?;
        super::bases::release_pin(
            &self.data.join("runs").join(text(plan, "runId")),
            text(plan, "id"),
        )
        .await?;
        if from_base || plan.get("nativeProjectBase").is_some() {
            self.bases()
                .trim(super::bases::cache_limit(self.disk_limit().await?), 0, None)
                .await?;
        }
        let temporary = root.join("tmp");
        if temporary.exists() {
            tokio::fs::remove_dir_all(&temporary).await?;
        }
        private_dir(&temporary).await?;
        workspace::own_tree(&temporary, self.identity.uid, self.identity.gid).await?;
        let stage = self.data.join("runs").join(text(plan, "runId"));
        let bindings = super::home::prepare(
            &stage.join("home"),
            &data.join("home"),
            self.identity.uid,
            self.identity.gid,
        )
        .await?;
        workspace::own_tree(&stage.join("home"), self.identity.uid, self.identity.gid).await?;
        let ownership = json!({ "uid": self.identity.uid, "gid": self.identity.gid });
        let saved_owner: Value = match tokio::fs::read(root.join("file-owner.json")).await {
            Ok(bytes) => serde_json::from_slice(&bytes)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Value::Null,
            Err(error) => return Err(error.into()),
        };
        if saved_owner != ownership {
            workspace::own_tree(&data, self.identity.uid, self.identity.gid).await?;
            block::sync(&data).await?;
            atomic_write(
                &root.join("file-owner.json"),
                &serde_json::to_vec(&ownership)?,
            )
            .await?;
        }
        if disk::used(&self.data, &self.state).await? > self.disk_limit().await? {
            return Err(Error::conflict(
                "Native preparation exceeds the shared node disk budget; local work is retained.",
            ));
        }
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
        plan["nativeHomeBindings"] = serde_json::to_value(bindings)?;
        if plan["chat"]["provider"] == "claude" {
            plan["chat"]["claudeCredentialsDirectory"] = super::home::CLAUDE_AUTH_DIRECTORY.into();
        }
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
        let encoded_plan = serde_json::to_vec(&plan)?;
        if encoded_plan.len() as u64 > self.disk_growth().await? {
            return Err(Error::conflict(
                "Insufficient shared native disk budget for the execution plan.",
            ));
        }
        atomic_write(&plan_file, &encoded_plan).await?;
        tokio::fs::set_permissions(&plan_file, std::fs::Permissions::from_mode(0o444)).await?;
        // The staging inbox is readonly in the workload's mount namespace.
        private_dir(&stage.join("chat-input")).await?;
        crate::nodes::workspace::prepare_native_inbox(&stage.join("chat-input")).await?;
        let launch = systemd::Launch {
            id,
            plan: &plan,
            data: &self.data,
            root: &container,
            home: &data.join("home"),
            workspace: &data.join("workspaces"),
            output: &data.join("output"),
            identity: &self.identity,
            temporary: &temporary,
            auth: auth.as_deref(),
        };
        drop(disk_owner);
        if !self.leased(id).await || stop.is_cancelled() {
            return Ok(143);
        }
        if let Some(attempt) = self.active.lock().await.get_mut(id) {
            attempt.started = true;
        }
        systemd::start_guardian(id, &self.file(id, "lease")).await?;
        let mut child = systemd::spawn(&launch)?;
        let storage_stop = stop.child_token();
        let _storage_guard = storage_stop.clone().drop_guard();
        let mounted = self
            .mounted
            .lock()
            .await
            .get(text(&plan, "runId"))
            .cloned()
            .ok_or_else(|| Error::internal("Native disk ownership missing."))?;
        let (protection_id, protection_stop) = (id.to_owned(), stop.clone());
        let protection = tokio::spawn(async move {
            if let Err(error) =
                super::protection::monitor(mounted.volume.clone(), &protection_id, storage_stop)
                    .await
            {
                tracing::error!(attempt = protection_id, %error, "Native storage CPU protection failed; fencing workload");
                protection_stop.cancel();
                if systemd::stop(&protection_id).await.is_err() {
                    let _ = mounted.abort_io().await;
                }
            }
        });
        if let Some(attempt) = self.active.lock().await.get_mut(id) {
            attempt.protection = Some(protection);
        }
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
                    let conversation_bytes = block::used_bytes(&data)?.saturating_add(disk::bytes_if_present(&temporary).await?);
                    let oversized = conversation_bytes > self.disk_budget(&plan)
                        || disk::used(&self.data, &self.state).await? > self.disk_limit().await?;
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

    async fn disk_limit(&self) -> Result<u64> {
        self.budget.lock().await["limits"]["diskMiB"]
            .as_u64()
            .map(|limit| limit.saturating_mul(disk::MIB))
            .ok_or_else(|| Error::unavailable("Native node disk budget is unavailable."))
    }

    async fn disk_growth(&self) -> Result<u64> {
        Ok(disk::available(
            self.disk_limit().await?,
            disk::used(&self.data, &self.state).await?,
        ))
    }

    async fn usage(&self) -> Result<Value> {
        let used = disk::used(&self.data, &self.state).await?;
        Ok(json!({
            "memoryMiB": 0,
            "diskMiB": used.div_ceil(disk::MIB),
            "diskBytes": used,
        }))
    }

    /// Ceilings are not preallocated reservations. Charge actual retained
    /// payloads, then bound every additional copy by the remaining shared pool.
    async fn check_preparation_space(&self, plan: &Value) -> Result<()> {
        let run = text(plan, "runId");
        let root = workspace::directory(&self.state, run)?;
        let current = if block::exists(&self.state, run)? {
            0
        } else {
            disk::bytes_if_present(&root.join("data")).await?
        };
        let assigned_base =
            block::exists(&self.state, run)? && root.join("file-owner.json").exists();
        let seed = if assigned_base {
            0
        } else if !root.join("initialized").exists()
            && !root.join("data/.cairn-seed-complete").exists()
        {
            let source = Plan::new(plan.clone())
                .imports()
                .find(|import| import.target == text(plan, "projectRoot"))
                .map(|import| import.source.to_owned())
                .ok_or_else(|| Error::bad("Missing native workspace seed."))?;
            if let Some(archives) = plan["nativeProjectArchives"].as_array() {
                let descriptors: Vec<crate::project_archives::Archive> = archives
                    .iter()
                    .cloned()
                    .map(serde_json::from_value)
                    .collect::<std::result::Result<_, _>>()?;
                let size = plan["resources"]["diskMiB"]
                    .as_u64()
                    .ok_or_else(|| Error::bad("Missing native disk size."))?;
                if let Some(base) = self.bases().lookup(&descriptors, size).await? {
                    base.metadata_bytes
                } else {
                    archives.iter().try_fold(0u64, |bytes, archive| {
                        bytes
                            .checked_add(
                                archive["logicalBytes"]
                                    .as_u64()
                                    .ok_or_else(|| Error::bad("Invalid project archive size."))?,
                            )
                            .ok_or_else(|| Error::too_large("Project archives exceed disk budget."))
                    })?
                }
            } else {
                workspace::bytes(&source).await?
            }
        } else {
            0
        };
        let home = disk::bytes_if_present(&self.data.join("runs").join(run).join("home")).await?;
        let incoming = seed.saturating_add(home);
        let growth = self.disk_growth().await?;
        if incoming > growth {
            crate::project_archives::reclaim_for_foreground(&self.data, incoming - growth, &[])
                .await?;
        }
        if current.saturating_add(incoming) > self.disk_budget(plan)
            || incoming > self.disk_growth().await?
            || disk::used(&self.data, &self.state).await? > self.disk_limit().await?
        {
            return Err(Error::conflict(
                "Retained native work leaves insufficient shared disk budget for this preparation.",
            ));
        }
        Ok(())
    }

    async fn capture_checkpoint(&self, run: &str, attempt: &str, budget: u64) -> Result<Value> {
        let _disk = disk::lock(&self.state).await?;
        workspace::capture(&self.state, run, attempt, budget, self.disk_growth().await?).await
    }

    async fn stopped_snapshot(&self, run: &str, force: bool) -> Result<Value> {
        let _disk = disk::lock(&self.state).await?;
        if block::exists(&self.state, run)? {
            let root = workspace::directory(&self.state, run)?;
            if !root.join("block-initialized").exists() {
                return Err(Error::conflict(
                    "Native block initialization must finish before publication.",
                ));
            }
            if !force {
                return crate::nodes::checkpoint::capture_completed(
                    &self.state,
                    run,
                    CancellationToken::new(),
                )
                .await;
            }
            return crate::nodes::checkpoint::capture(
                &self.state,
                run,
                None,
                Arc::default(),
                CancellationToken::new(),
                "",
                storage::checkpoint::Consistency::Filesystem,
            )
            .await;
        }
        if self
            .active
            .lock()
            .await
            .values()
            .any(|attempt| attempt.run == run)
        {
            return Err(Error::conflict(
                "Waiting for the stopped native checkpoint to finish.",
            ));
        }
        let root = workspace::directory(&self.state, run)?;
        let _owner =
            crate::file_lock::exclusive(&root.join("owner.lock"), "Native workspace is active.")?;
        let attempt = tokio::fs::read_to_string(root.join("attempt")).await?;
        if !force
            && let Ok(snapshot) = workspace::completed(&self.state, run).await
            && snapshot["manifest"]["attempt"] == attempt
        {
            return Ok(snapshot);
        }
        let limit = match tokio::fs::read(root.join("resources.json")).await {
            Ok(bytes) => {
                let resources: crate::nodes::Resources = serde_json::from_slice(&bytes)?;
                resources
                    .disk_mi_b
                    .saturating_mul(disk::MIB)
                    .min(self.disk_limit().await?)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => self.disk_limit().await?,
            Err(error) => return Err(error.into()),
        };
        workspace::capture(&self.state, run, &attempt, limit, self.disk_growth().await?).await
    }

    async fn abort_disk(&self, id: &str) -> Result<()> {
        let run = self
            .active
            .lock()
            .await
            .get(id)
            .map(|attempt| attempt.run.clone());
        if let Some(run) = run {
            let mounted = self.mounted.lock().await.get(&run).cloned();
            if let Some(mounted) = mounted {
                mounted.abort_io().await?;
            }
        }
        Ok(())
    }

    async fn stop_attempt(&self, id: &str) -> Result<()> {
        uuid(id)?;
        let _admission = self.admission.lock().await;
        atomic_write(&self.file(id, "stopped"), b"1").await?;
        if let Some(attempt) = self.active.lock().await.get(id) {
            attempt.stop.cancel();
        }
        let preparing = self
            .active
            .lock()
            .await
            .get(id)
            .is_some_and(|attempt| !attempt.started);
        if preparing {
            self.abort_disk(id).await?;
        }
        if let Err(error) = systemd::stop(id).await {
            self.abort_disk(id).await?;
            return Err(error);
        }
        self.wait_stopped(id).await?;
        if let Err(error) = super::bases::release_attempt(&self.data, id).await {
            tracing::debug!(
                status = error.status,
                "Stopped native preparation pin cleanup deferred"
            );
        }
        Ok(())
    }

    async fn wait_stopped(&self, id: &str) -> Result<()> {
        tokio::time::timeout(Duration::from_secs(300), async {
            while self.active.lock().await.contains_key(id) {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .map_err(|_| {
            Error::unavailable("Stopped native checkpoint did not finish before its deadline.")
        })
    }

    /// Match the VM wait protocol: headers and spaces keep relayed waits alive.
    fn wait(&self, id: &str) -> Response {
        let stream = futures_util::stream::unfold(
            (self.clone(), id.to_owned(), false),
            |(controller, id, done)| async move {
                if done {
                    return None;
                }
                // An exit marker alone does not release physical ownership.
                let released = !controller.active.lock().await.contains_key(&id);
                if released
                    && let Ok(code) = tokio::fs::read_to_string(controller.file(&id, "exit")).await
                {
                    let result = code
                        .parse::<i32>()
                        .map(|code| Bytes::from(json!({ "StatusCode": code }).to_string()))
                        .map_err(std::io::Error::other);
                    return Some((result, (controller, id, true)));
                }
                tokio::time::sleep(Duration::from_secs(1)).await;
                let stopped = controller.stop.is_cancelled();
                Some((Ok(Bytes::from_static(b" ")), (controller, id, stopped)))
            },
        );
        (
            [("content-type", "application/json")],
            Body::from_stream(stream),
        )
            .into_response()
    }
}

fn guest_data(plan: &Value, data: &Path) -> PathBuf {
    plan["guestDataRoot"]
        .as_str()
        .map_or_else(|| data.to_owned(), PathBuf::from)
}

#[cfg(test)]
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
        let usage = controller.usage().await?;
        let budget = controller.budget.lock().await.clone();
        return Ok(Json(json!({
            "status": "ok", "backend": "native", "nodeProtocol": 2, "projectArchives": 1, "nativeProjectBases": 1,
            "runtimeId": std::env::var("APP_RUNTIME_ID").unwrap_or_else(|_| "development".into()),
            "dataRoot": controller.data, "capabilities": capabilities,
            "runtimes": systemd::runtimes()?,
            "pool": { "capacity": budget["slots"] }, "activeRuns": controller.active.lock().await.len(),
            "sharedResources": true, "exclusiveReservations": false, "budget": budget,
            "usage": usage, "pressure": null
        })).into_response());
    }
    let secret = tokio::fs::read_to_string(controller.data.join("runner-secret")).await?;
    let bearer = crate::nodes::bearer(request.headers()).unwrap_or("");
    if !crate::auth::safe_equal(bearer, secret.trim()) {
        return Err(Error::unauthorized("Invalid native controller credential."));
    }
    let parts = path.trim_start_matches('/').split('/').collect::<Vec<_>>();
    let value = match (method.as_str(), parts.as_slice()) {
        ("POST", ["native-bases", "lookup"]) => {
            let value = input(request, 8_000_000).await?;
            let archives: Vec<crate::project_archives::Archive> =
                serde_json::from_value(value["archives"].clone())?;
            let size = value["diskMiB"]
                .as_u64()
                .ok_or_else(|| Error::bad("Missing native base size."))?;
            json!({ "base": controller.bases().lookup(&archives, size).await? })
        }
        ("POST", ["native-bases", "warm"]) => {
            let value = input(request, 8_000_000).await?;
            controller
                .base_warmer
                .enqueue(
                    controller.bases(),
                    value,
                    controller.budget.clone(),
                    controller.stop.clone(),
                )
                .await?;
            json!({ "queued": true })
        }
        ("POST", ["node-budget"]) => {
            let value = input(request, 16384).await?;
            let budget = native_budget(&value)?;
            let _admission = controller.admission.lock().await;
            let previous = controller.budget.lock().await.clone();
            if value != previous {
                // Do not hold the budget mutex while waiting for storage:
                // foreground transfers query health with their disk lease held.
                let _disk = disk::lock(&controller.state).await?;
                let limit = budget.limits.disk_mi_b.saturating_mul(disk::MIB);
                let used = disk::used(&controller.data, &controller.state).await?;
                if let Err(error) = controller
                    .bases()
                    .trim(
                        super::bases::cache_limit(limit),
                        used.saturating_sub(limit),
                        None,
                    )
                    .await
                {
                    tracing::debug!(
                        status = error.status,
                        "Native cache reclamation deferred during budget update"
                    );
                }
                if value["limits"] != previous["limits"] {
                    systemd::apply_budget(&budget).await?;
                }
                atomic_write(
                    &controller.state.join(crate::microvm::budget::FILE),
                    &serde_json::to_vec(&value)?,
                )
                .await?;
                *controller.budget.lock().await = value;
            }
            json!({ "ready": true })
        }
        ("POST", ["storage-policy"]) => {
            let policy: crate::storage::policy::Policy =
                serde_json::from_value(input(request, 16384).await?)?;
            policy.validate()?;
            let path = controller.state.join("storage-policy.json");
            let encoded = serde_json::to_vec(&policy)?;
            if tokio::fs::read(&path).await.ok().as_ref() != Some(&encoded) {
                let _disk = disk::lock(&controller.state).await?;
                atomic_write(&path, &encoded).await?;
                crate::storage::runtime::configure(&controller.state, &policy)?;
            }
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
            return Ok(controller.wait(id));
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
                let mut at_line_boundary = true;
                let mut last_progress = tokio::time::Instant::now();
                loop {
                    if file.seek(std::io::SeekFrom::Start(offset)).await.is_err() {
                        break;
                    }
                    match file.read(&mut buffer).await {
                        Ok(0) => {
                            if !controller.active.lock().await.contains_key(&id) {
                                break;
                            }
                            // Idle transport frames must not split a log record
                            // whose remaining bytes have not been appended yet.
                            if at_line_boundary && last_progress.elapsed() >= Duration::from_secs(1)
                            {
                                if writer
                                    .write_all(b"{\"type\":\"heartbeat\"}\n")
                                    .await
                                    .is_err()
                                {
                                    break;
                                }
                                last_progress = tokio::time::Instant::now();
                            }
                            tokio::time::sleep(Duration::from_millis(100)).await;
                        }
                        Ok(count) => {
                            if writer.write_all(&buffer[..count]).await.is_err() {
                                break;
                            }
                            at_line_boundary = buffer[count - 1] == b'\n';
                            last_progress = tokio::time::Instant::now();
                            offset += count as u64;
                        }
                        Err(_) => break,
                    }
                }
            });
            return Ok((
                [("content-type", "application/x-ndjson")],
                Body::from_stream(tokio_util::io::ReaderStream::new(reader)),
            )
                .into_response());
        }
        ("POST", ["disks", run, "storage-status"]) => {
            uuid(run)?;
            if block::exists(&controller.state, run)? {
                return Ok(Json(block::status(&controller.state, run).await?).into_response());
            }
            let root = workspace::directory(&controller.state, run)?;
            let active_local = disk::bytes_if_present(&root.join("data")).await?;
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
            if block::exists(&controller.state, run)? {
                let receipt = input(request, 16384).await?;
                return Ok(
                    Json(block::published(&controller.state, run, &receipt).await?).into_response(),
                );
            }
            let _disk = disk::lock(&controller.state).await?;
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
            controller.stopped_snapshot(run, false).await?
        }
        ("POST", ["disks", run, "snapshot"]) => {
            uuid(run)?;
            controller.stopped_snapshot(run, true).await?
        }
        ("POST", ["runs", id, "snapshot"]) => {
            uuid(id)?;
            let active = {
                controller
                    .active
                    .lock()
                    .await
                    .get(*id)
                    .map(|attempt| (attempt.run.clone(), attempt.stop.clone()))
            };
            if let Some((run, stop)) = active
                && block::exists(&controller.state, &run)?
            {
                let value = input(request, 16384).await?;
                let mounted = controller
                    .mounted
                    .lock()
                    .await
                    .get(&run)
                    .cloned()
                    .ok_or_else(|| Error::conflict("Native filesystem is not mounted."))?;
                return Ok(Json(
                    block::capture_active(
                        &controller.state,
                        &run,
                        mounted,
                        value["consistency"] != "crash",
                        if value["consistency"] == "crash" {
                            CancellationToken::new()
                        } else {
                            stop
                        },
                    )
                    .await?,
                )
                .into_response());
            }
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
        ("POST", ["disks", run, "evict"]) => {
            let _disk = disk::lock(&controller.state).await?;
            uuid(run)?;
            if controller
                .active
                .lock()
                .await
                .values()
                .any(|attempt| attempt.run == *run)
            {
                return Err(Error::conflict(
                    "Native execution must finish stopping before cache eviction.",
                ));
            }
            let expected = input(request, 16384).await?;
            block::evict(&controller.data, &controller.state, run, &expected).await?
        }
        ("POST", ["disks", run, "restore"]) => {
            let _disk = disk::lock(&controller.state).await?;
            uuid(run)?;
            let request = input(request, snapshots::MAX_MANIFEST_BYTES).await?;
            if request["manifest"]["kind"] == block::KIND {
                return Ok(
                    Json(block::restore(&controller.state, run, request).await?).into_response()
                );
            }
            let limit = controller.budget.lock().await["limits"]["diskMiB"]
                .as_u64()
                .unwrap_or(32768)
                * 1_048_576;
            block::restore_files(
                &controller.state,
                run,
                &request,
                limit,
                controller.disk_growth().await?,
            )
            .await?
        }
        ("POST", ["disks", run, action @ ("delete" | "prune")]) => {
            let _disk = disk::lock(&controller.state).await?;
            uuid(run)?;
            let root = workspace::directory(&controller.state, run)?;
            private_dir(&root).await?;
            let _owner = crate::file_lock::exclusive(
                &root.join("owner.lock"),
                "Native workspace is active.",
            )?;
            if (root.join("restore.pending.json").exists()
                || root.join("block-restore.pending.json").exists()
                || root.join("legacy-block-restore.pending.json").exists())
                && *action == "prune"
            {
                return Err(Error::conflict(
                    "Finish native restore before pruning retained local work.",
                ));
            }
            let retired_grants = block::action(&controller.state, run, *action == "delete").await?;
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
                    "resources.json",
                    "attempt",
                    "restored.json",
                    "checkpoint-error",
                    "published.json",
                    "runtime.json",
                    "restore.pending.json",
                    "block-initialized",
                    "file-owner.json",
                    "block-seeding",
                    "block-restore.pending.json",
                    "legacy-block-restore.pending.json",
                    "legacy-data",
                    "tmp",
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
            json!({ "deleted": *action == "delete", "pruned": *action == "prune", "retiredGrants": retired_grants })
        }
        ("POST", ["runs", id, "artifact"]) => {
            let disk_owner = disk::lock(&controller.state).await?;
            uuid(id)?;
            let value = input(request, 16384).await?;
            let active = controller.active.lock().await;
            let attempt = active
                .get(*id)
                .ok_or_else(|| Error::conflict("Native attempt is not active."))?;
            if value["runId"] != attempt.run || attempt.stop.is_cancelled() {
                return Err(Error::forbidden("Wrong native artifact scope."));
            }
            let plan = controller.plan(id).await?;
            let environment = workspace::directory(&controller.state, &attempt.run)?;
            let root = environment.join("data");
            let path = Path::new(text(&value, "path"));
            let project_root = Path::new(text(&plan, "projectRoot"));
            let (source, allowed) = if let Ok(relative) = path.strip_prefix(project_root) {
                (root.join("workspaces").join(relative), root.clone())
            } else if let Ok(relative) = path.strip_prefix(
                guest_data(&plan, &controller.data)
                    .join("runs")
                    .join(&attempt.run)
                    .join("output"),
            ) {
                (root.join("output").join(relative), root.clone())
            } else if let Ok(relative) = path.strip_prefix("/tmp") {
                let temporary = environment.join("tmp");
                (temporary.join(relative), temporary)
            } else {
                return Err(Error::forbidden(
                    "Native artifacts must be in this conversation's workspace, output directory or /tmp.",
                ));
            };
            let exports = controller.state.join("exports");
            private_dir(&exports).await?;
            let (snapshot, size) = crate::artifacts::file::snapshot_in(
                &source,
                &allowed,
                &exports,
                controller.disk_growth().await?,
            )
            .await?;
            drop(active);
            drop(disk_owner);
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
            let _disk = disk::lock(&controller.state).await?;
            uuid(id)?;
            uuid(project)?;
            let value = input(request, 16384).await?;
            let active = controller.active.lock().await;
            let attempt = active
                .get(*id)
                .ok_or_else(|| Error::conflict("Native attempt is not active."))?;
            if value["runId"] != attempt.run || attempt.stop.is_cancelled() {
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
                let data = workspace::directory(&controller.state, &attempt.run)?.join("data");
                // Stage on the mounted filesystem so installation is an atomic
                // rename, and do not walk all lazy files to estimate free space.
                let staged = tempfile::tempdir_in(&data)?;
                let received = staged.path().join("project");
                let remaining = block::free_bytes(&data)?;
                workspace::copy(
                    &source,
                    &received,
                    remaining.min(controller.disk_growth().await?),
                )
                .await?;
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

// Called only after old workloads and block devices are fenced. Temporary
// anonymous bases/clones have no authority; conventional committed disks remain.
async fn recover_preparations(data: &Path, state: &Path) -> Result<()> {
    storage::environment::recover(state).await?;
    super::bases::recover_pins(data).await
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
    if !state.is_absolute() || tokio::fs::canonicalize(&state).await? != state {
        return Err(Error::bad(
            "Native state must use a canonical absolute directory.",
        ));
    }
    let _lock = crate::file_lock::exclusive(
        &state.join("controller.lock"),
        "Another native controller owns this storage.",
    )?;
    disk::recover_exports(&state).await?;
    atomic_write(
        &state.join("native-accounting.json"),
        &serde_json::to_vec(&json!({ "data": data }))?,
    )
    .await?;
    let identity = Arc::new(systemd::identity().await?);
    private_dir(&state.join("roots")).await?;
    let mut interrupted = Vec::new();
    let mut entries = tokio::fs::read_dir(&state).await?;
    while let Some(entry) = entries.next_entry().await? {
        if entry
            .path()
            .extension()
            .is_some_and(|extension| extension == "active")
        {
            let attempt = entry
                .path()
                .file_stem()
                .unwrap()
                .to_string_lossy()
                .into_owned();
            uuid(&attempt)?;
            let run = tokio::fs::read_to_string(entry.path()).await?;
            uuid(&run)?;
            systemd::request_stop(&attempt).await?;
            interrupted.push((attempt, run, entry.path()));
        }
    }
    // Host mounts survive controller death. Recovery aborts only verified dead
    // backends to release blocked I/O, confirms fencing, then unmounts. Complete
    // all other attributed fences before retiring any kernel device.
    block::recover_host_mounts(&state).await?;
    for (attempt, _, _) in &interrupted {
        systemd::stop(attempt).await?;
    }

    {
        let state = state.clone();
        tokio::task::spawn_blocking(move || storage::ublk::cleanup_stale(&state))
            .await
            .map_err(Error::internal)??;
    }
    for (attempt, run, marker) in interrupted {
        block::recover_completed(&state, &run, &attempt).await?;
        atomic_write(
            &marker.with_extension("exit"),
            crate::runner::CONTROLLER_INTERRUPTED.to_string().as_bytes(),
        )
        .await?;
        tokio::fs::remove_file(marker).await?;
    }
    recover_preparations(&data, &state).await?;
    let budget_path = state.join(crate::microvm::budget::FILE);
    let budget: Value = match tokio::fs::read(&budget_path).await {
        Ok(bytes) => serde_json::from_slice(&bytes)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            json!({ "slots": 1, "limits": crate::nodes::placement::defaults() })
        }
        Err(error) => return Err(error.into()),
    };
    let checked = native_budget(&budget)?;
    systemd::apply_budget(&checked).await?;
    atomic_write(&budget_path, &serde_json::to_vec(&budget)?).await?;
    let controller = Controller {
        data,
        state,
        active: Arc::default(),
        mounted: Arc::default(),
        leases: Arc::default(),
        admission: Arc::default(),
        base_warmer: Arc::default(),
        budget: Arc::new(Mutex::new(budget)),
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
    controller.base_warmer.wait_idle().await;
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

    #[tokio::test]
    async fn native_startup_reclaims_interrupted_preparations_and_preserves_committed_disks() {
        let (_fixture, controller) = fixture().await;
        let run = crate::config::id();
        let attempt = crate::config::id();
        let disk = controller.state.join("disks").join(&run);
        let abandoned = controller
            .state
            .join("environments")
            .join(crate::config::id());
        private_dir(&disk.join("lazy")).await.unwrap();
        atomic_write(&disk.join("lazy/preserved"), b"committed payload")
            .await
            .unwrap();
        private_dir(&abandoned.join("lazy")).await.unwrap();
        atomic_write(&abandoned.join("lazy/partial"), b"unclaimed")
            .await
            .unwrap();
        let stage = controller.data.join("runs").join(&run);
        private_dir(&stage).await.unwrap();
        let pin = super::super::bases::pin_path(&stage, &attempt).unwrap();
        atomic_write(&pin, b"").await.unwrap();
        recover_preparations(&controller.data, &controller.state)
            .await
            .unwrap();
        assert!(!abandoned.exists());
        assert!(!pin.exists());
        assert_eq!(
            storage::environment::directory(&controller.state, &run).unwrap(),
            disk
        );
        assert_eq!(
            tokio::fs::read(disk.join("lazy/preserved")).await.unwrap(),
            b"committed payload"
        );
    }

    #[tokio::test]
    async fn optional_base_limits_do_not_reject_valid_zip_preparation() {
        let (_fixture, controller) = fixture().await;
        let run = crate::config::id();
        let project_root = controller.data.join("runs").join(&run).join("workspace");
        let archive = json!({
            "projectId": crate::config::id(),
            "revision": "a".repeat(40),
            "key": "b".repeat(64),
            "hash": "c".repeat(64),
            "bytes": 100,
            "logicalBytes": 15
        });
        for (size, count) in [(2_097_152, 1), (128, 65)] {
            let mut archives = Vec::new();
            for _ in 0..count {
                let mut value = archive.clone();
                value["projectId"] = crate::config::id().into();
                archives.push(value);
            }
            let plan = json!({
                "id": crate::config::id(),
                "runId": run,
                "projectRoot": project_root,
                "resources": { "cpu": 1, "memoryMiB": 512, "diskMiB": size },
                "imports": [{ "source": project_root, "target": project_root }],
                "nativeProjectArchives": archives
            });
            controller.check_preparation_space(&plan).await.unwrap();
            assert!(
                !controller
                    .bases()
                    .initialize(&plan, &CancellationToken::new())
                    .await
                    .unwrap()
            );
        }
    }

    #[tokio::test]
    async fn budget_update_does_not_block_budget_reads_while_waiting_for_storage() {
        let (_fixture, controller) = fixture().await;
        let owner =
            crate::file_lock::exclusive(&controller.state.join("storage.lock"), "fixture owner")
                .unwrap();
        let mut value = controller.budget.lock().await.clone();
        value["slots"] = 2.into();
        let updating = controller.clone();
        let pending =
            tokio::spawn(async move { route(updating, request("/node-budget", &value)).await });
        let waiting = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if crate::file_lock::exclusive(
                    &controller.state.join("storage-foreground.lock"),
                    "priority probe",
                )
                .is_err()
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .is_ok();
        let readable = controller.budget.try_lock().is_ok();
        // Abort this direct route before releasing the fixture disk; no real
        // systemd budget operation is admitted by this concurrency test.
        pending.abort();
        let _ = pending.await;
        drop(owner);
        assert!(waiting);
        assert!(readable);
    }

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
            mounted: Arc::default(),
            leases: Arc::default(),
            admission: Arc::default(),
            base_warmer: Arc::default(),
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
    async fn native_wait_streams_heartbeats_until_physical_ownership_is_released() {
        use futures_util::StreamExt;

        let (_fixture, controller) = fixture().await;
        let attempt = crate::config::id();
        controller.active.lock().await.insert(
            attempt.clone(),
            Attempt {
                run: crate::config::id(),
                stop: CancellationToken::new(),
                started: true,
                protection: None,
            },
        );
        atomic_write(&controller.file(&attempt, "exit"), b"17")
            .await
            .unwrap();

        let response = tokio::time::timeout(
            Duration::from_millis(250),
            handler(
                State(controller.clone()),
                request(&format!("/runs/{attempt}/wait"), &json!({})),
            ),
        )
        .await
        .expect("Native wait must send response headers before the attempt finishes")
        .unwrap();
        assert_eq!(response.headers()["content-type"], "application/json");
        let mut body = response.into_body().into_data_stream();
        let mut received = Vec::new();
        for _ in 0..2 {
            let bytes = tokio::time::timeout(Duration::from_secs(2), body.next())
                .await
                .expect("An unfinished native attempt must keep its wait connection alive")
                .unwrap()
                .unwrap();
            assert!(bytes.iter().all(u8::is_ascii_whitespace));
            received.extend_from_slice(&bytes);
        }

        controller.active.lock().await.remove(&attempt);
        while let Some(bytes) = tokio::time::timeout(Duration::from_secs(2), body.next())
            .await
            .unwrap()
        {
            received.extend_from_slice(&bytes.unwrap());
        }
        let result: Value = serde_json::from_slice(&received).unwrap();
        assert_eq!(result, json!({ "StatusCode": 17 }));
    }

    #[tokio::test]
    async fn native_logs_keep_quiet_attempts_alive_without_splitting_json_records() {
        use futures_util::StreamExt;

        let (_fixture, controller) = fixture().await;
        let attempt = crate::config::id();
        controller.active.lock().await.insert(
            attempt.clone(),
            Attempt {
                run: crate::config::id(),
                stop: CancellationToken::new(),
                started: true,
                protection: None,
            },
        );
        let log = controller.file(&attempt, "log");
        atomic_write(&log, b"").await.unwrap();
        let request = Request::builder()
            .method("GET")
            .uri(format!("/runs/{attempt}/logs"))
            .header("authorization", "Bearer synthetic-private-controller-token")
            .body(Body::empty())
            .unwrap();
        let response = handler(State(controller.clone()), request).await.unwrap();
        let mut body = response.into_body().into_data_stream();
        let heartbeat = tokio::time::timeout(Duration::from_secs(2), body.next())
            .await
            .expect("A quiet native attempt must keep its logs connection alive")
            .unwrap()
            .unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&heartbeat).unwrap(),
            json!({ "type": "heartbeat" }),
        );

        let mut file = tokio::fs::OpenOptions::new()
            .append(true)
            .open(&log)
            .await
            .unwrap();
        file.write_all(b"{\"type\":\"agent_").await.unwrap();
        file.flush().await.unwrap();
        let prefix = tokio::time::timeout(Duration::from_secs(2), body.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(&prefix[..], b"{\"type\":\"agent_");
        assert!(
            tokio::time::timeout(Duration::from_millis(1200), body.next())
                .await
                .is_err(),
            "Heartbeats must not be inserted inside a partially written JSON record",
        );
        file.write_all(b"message\",\"text\":\"ok\"}\n")
            .await
            .unwrap();
        file.flush().await.unwrap();
        controller.active.lock().await.remove(&attempt);
        let mut record = prefix.to_vec();
        while let Some(bytes) = tokio::time::timeout(Duration::from_secs(2), body.next())
            .await
            .unwrap()
        {
            record.extend_from_slice(&bytes.unwrap());
        }
        assert_eq!(
            serde_json::from_slice::<Value>(&record).unwrap(),
            json!({ "type": "agent_message", "text": "ok" }),
        );
    }

    async fn sparse(path: &Path, size: u64) {
        private_dir(path.parent().unwrap()).await.unwrap();
        tokio::fs::File::create(path)
            .await
            .unwrap()
            .set_len(size)
            .await
            .unwrap();
    }

    async fn stopped_data(controller: &Controller, run: &str, attempt: &str) -> PathBuf {
        let root = workspace::directory(&controller.state, run).unwrap();
        private_dir(&root.join("data/workspaces")).await.unwrap();
        atomic_write(&root.join("attempt"), attempt.as_bytes())
            .await
            .unwrap();
        atomic_write(&root.join("initialized"), b"1").await.unwrap();
        atomic_write(&root.join("data/workspaces/source"), b"original")
            .await
            .unwrap();
        root
    }

    #[tokio::test]
    async fn tmp_artifact_uses_the_ephemeral_path_and_rejects_redirects() {
        let (_fixture, controller) = fixture().await;
        let run = crate::config::id();
        let attempt = crate::config::id();
        let stage = controller.data.join("runs").join(&run);
        let environment = workspace::directory(&controller.state, &run).unwrap();
        for directory in [
            stage.join("workspace"),
            stage.join("home"),
            environment.join("tmp"),
            environment.join("data/tmp"),
            controller.data.join("runner-plans"),
        ] {
            private_dir(&directory).await.unwrap();
        }
        let plan = json!({
            "id": attempt, "runId": run, "backend": "native", "nodeLeaseRequired": true, "expires": null,
            "cwd": stage.join("workspace"), "projectRoot": stage.join("workspace"),
            "sandbox": "workspace-write", "args": [], "prompt": "fixture",
            "resources": { "cpu": 1, "memoryMiB": 128, "diskMiB": 128 },
            "imports": [
                { "source": stage.join("workspace"), "target": stage.join("workspace") },
                { "source": stage.join("home"), "target": "/home/node" }
            ]
        });
        atomic_write(
            &controller
                .data
                .join("runner-plans")
                .join(format!("{attempt}.json")),
            &serde_json::to_vec(&plan).unwrap(),
        )
        .await
        .unwrap();
        controller.active.lock().await.insert(
            attempt.clone(),
            Attempt {
                run: run.clone(),
                stop: CancellationToken::new(),
                started: true,
                protection: None,
            },
        );
        tokio::fs::write(environment.join("tmp/result"), b"actual temporary artifact")
            .await
            .unwrap();
        tokio::fs::write(environment.join("data/tmp/result"), b"wrong stale bytes")
            .await
            .unwrap();
        let response = route(
            controller.clone(),
            request(
                &format!("/runs/{attempt}/artifact"),
                &json!({ "runId": run, "path": "/tmp/result" }),
            ),
        )
        .await
        .unwrap();
        let bytes = axum::body::to_bytes(response.into_body(), 1024)
            .await
            .unwrap();
        assert_eq!(bytes.as_ref(), b"actual temporary artifact");
        std::os::unix::fs::symlink(
            environment.join("data/tmp/result"),
            environment.join("tmp/redirect"),
        )
        .unwrap();
        assert!(
            route(
                controller,
                request(
                    &format!("/runs/{attempt}/artifact"),
                    &json!({ "runId": run, "path": "/tmp/redirect" })
                )
            )
            .await
            .is_err()
        );
    }

    #[tokio::test]
    async fn native_disk_pool_charges_retained_conversations_archives_and_staging() {
        let (fixture, controller) = fixture().await;
        controller.budget.lock().await["limits"]["diskMiB"] = 32768.into();
        let first = crate::config::id();
        let second = crate::config::id();
        let gib = 1024 * disk::MIB;
        let retained = workspace::directory(&controller.state, &first)
            .unwrap()
            .join("data/workspaces/model");
        sparse(&retained, 20 * gib).await;
        let archive = controller.state.join("snapshots").join(crate::config::id());
        sparse(&archive.join("disk"), gib).await;
        atomic_write(&archive.join("run"), first.as_bytes())
            .await
            .unwrap();
        sparse(
            &controller.data.join("runs").join(&second).join("staging"),
            2 * gib,
        )
        .await;
        let usage = controller.usage().await.unwrap();
        assert!(usage["diskBytes"].as_u64().unwrap() > 23 * gib);
        assert_eq!(usage["diskMiB"], 23 * 1024 + 1);

        // A second 20 GiB conversation cannot consume the remaining shared pool.
        // The transfer checks the size frame before reading/writing its contents.
        let incoming = fixture.path().join("remote-source");
        sparse(&incoming.join("model"), 20 * gib).await;
        let receiving = tempfile::tempdir_in(controller.data.join("runs")).unwrap();
        let _owner = disk::lock(&controller.state).await.unwrap();
        let error = workspace::copy(
            &incoming,
            receiving.path(),
            controller.disk_growth().await.unwrap(),
        )
        .await
        .unwrap_err();
        assert!(error.message.contains("disk budget"), "{}", error.message);
        assert!(!receiving.path().join("model").exists());
        assert_eq!(tokio::fs::metadata(retained).await.unwrap().len(), 20 * gib);
        assert_eq!(controller.usage().await.unwrap(), usage);
    }

    #[tokio::test]
    async fn native_stop_waits_for_checkpoint_completion_and_physical_owner_release() {
        let (_fixture, controller) = fixture().await;
        let run = crate::config::id();
        let attempt = crate::config::id();
        let root = stopped_data(&controller, &run, &attempt).await;
        let owner = workspace::execution_ownership(&controller.state, &run)
            .await
            .unwrap();
        controller.active.lock().await.insert(
            attempt.clone(),
            Attempt {
                run: run.clone(),
                stop: CancellationToken::new(),
                started: true,
                protection: None,
            },
        );
        let (waiting_controller, waiting_attempt) = (controller.clone(), attempt.clone());
        let mut waiting =
            tokio::spawn(async move { waiting_controller.wait_stopped(&waiting_attempt).await });
        assert!(
            tokio::time::timeout(Duration::from_millis(30), &mut waiting)
                .await
                .is_err()
        );
        let error = controller.stopped_snapshot(&run, false).await.unwrap_err();
        assert_eq!(error.status, 409);
        let snapshot = controller
            .capture_checkpoint(&run, &attempt, disk::MIB)
            .await
            .unwrap();
        assert!(!waiting.is_finished());
        drop(owner);
        controller.active.lock().await.remove(&attempt);
        tokio::time::timeout(Duration::from_secs(2), waiting)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let _replacement = crate::file_lock::exclusive(&root.join("owner.lock"), "busy").unwrap();
        assert_eq!(
            workspace::completed(&controller.state, &run).await.unwrap()["id"],
            snapshot["id"]
        );
    }

    #[tokio::test]
    async fn native_completed_snapshot_recovers_after_space_is_freed_without_serving_old_data() {
        let (_fixture, controller) = fixture().await;
        let run = crate::config::id();
        let attempt = crate::config::id();
        let root = stopped_data(&controller, &run, &attempt).await;
        let original = controller.stopped_snapshot(&run, false).await.unwrap();
        atomic_write(&root.join("data/workspaces/source"), b"latest local work")
            .await
            .unwrap();
        let other = controller
            .state
            .join("native")
            .join(crate::config::id())
            .join("data/stale");
        sparse(&other, 2048 * disk::MIB).await;
        assert!(controller.stopped_snapshot(&run, true).await.is_err());
        assert!(workspace::completed(&controller.state, &run).await.is_err());
        assert!(controller.stopped_snapshot(&run, false).await.is_err());
        assert!(
            controller
                .state
                .join("snapshots")
                .join(text(&original, "id"))
                .exists()
        );

        tokio::fs::remove_file(other).await.unwrap();
        let response = route(
            controller.clone(),
            request(&format!("/disks/{run}/snapshot-completed"), &json!({})),
        )
        .await
        .unwrap();
        let recovered: Value = serde_json::from_slice(
            &axum::body::to_bytes(response.into_body(), 1_000_000)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_ne!(recovered["id"], original["id"]);
        assert!(!root.join("checkpoint-error").exists());
        let archive = tokio::fs::read(
            controller
                .state
                .join("snapshots")
                .join(text(&recovered, "id"))
                .join("disk"),
        )
        .await
        .unwrap();
        assert!(
            String::from_utf8(archive)
                .unwrap()
                .contains(&STANDARD.encode(b"latest local work"))
        );
        assert_eq!(
            controller.stopped_snapshot(&run, false).await.unwrap()["id"],
            recovered["id"]
        );

        atomic_write(
            &root.join("checkpoint-error"),
            b"transient explicit capture failure",
        )
        .await
        .unwrap();
        let replacement = controller.stopped_snapshot(&run, true).await.unwrap();
        assert!(!root.join("checkpoint-error").exists());
        assert_eq!(
            workspace::completed(&controller.state, &run).await.unwrap()["id"],
            replacement["id"]
        );
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
    async fn native_budget_supports_six_slots_and_rejects_invalid_capacities() {
        let (_root, controller) = fixture().await;
        route(
            controller.clone(),
            request(
                "/node-budget",
                &json!({ "slots": 6, "limits": { "cpu": 1, "memoryMiB": 1024, "diskMiB": 1024 } }),
            ),
        )
        .await
        .unwrap();
        assert_eq!(controller.budget.lock().await["slots"], 6);
        for slots in [json!(0), json!(4097), json!(1.5), json!(null)] {
            assert!(route(controller.clone(), request("/node-budget", &json!({
                "slots": slots, "limits": { "cpu": 1, "memoryMiB": 1024, "diskMiB": 1024 }
            }))).await.is_err());
        }
        assert_eq!(controller.budget.lock().await["slots"], 6);
    }

    #[tokio::test]
    async fn controller_enforces_all_configured_slots_before_preparing_an_attempt() {
        let (_root, controller) = fixture().await;
        controller.budget.lock().await["slots"] = 6.into();
        let attempt = crate::config::id();
        controller
            .leases
            .lock()
            .await
            .insert(attempt.clone(), crate::nodes::boot_ms() + 60000);
        for _ in 0..5 {
            controller.active.lock().await.insert(
                crate::config::id(),
                Attempt {
                    run: crate::config::id(),
                    stop: CancellationToken::new(),
                    started: true,
                    protection: None,
                },
            );
        }
        // Five occupied slots still admit a sixth attempt as far as plan loading.
        assert_ne!(controller.start(&attempt).await.unwrap_err().status, 503);
        let existing = crate::config::id();
        controller.active.lock().await.insert(
            existing.clone(),
            Attempt {
                run: crate::config::id(),
                stop: CancellationToken::new(),
                started: true,
                protection: None,
            },
        );
        assert_eq!(controller.start(&attempt).await.unwrap_err().status, 503);
        // Retrying an existing attempt never consumes a second slot.
        controller.start(&existing).await.unwrap();
        controller.active.lock().await.remove(&existing);
        assert_ne!(controller.start(&attempt).await.unwrap_err().status, 503);
    }
}
