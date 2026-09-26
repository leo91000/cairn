use crate::{
    accounts::{Lease, broker},
    config::now,
    error::{Error, Result, required},
    execution,
    process::Environment,
    provider::Provider,
    recovery, run_output,
    service::{Service, policy, run_projects},
    skills::{atomic_write, private_dir},
    store::{Store, merge},
    supervisor::Supervised,
    validation::text,
};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicI64, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt},
    sync::{Mutex, Notify, OnceCell, mpsc},
};
use tokio_util::{sync::CancellationToken, task::TaskTracker};
#[derive(Default)]
pub struct Worker {
    pub active: Mutex<HashMap<String, CancellationToken>>,
    tick_lock: Mutex<()>,
    wake: Notify,
    initialized: OnceCell<()>,
    maintenance: AtomicI64,
    tasks: TaskTracker,
}
struct Checkpoint {
    store: Store,
    id: String,
    value: Mutex<Value>,
    deadline: Option<i64>,
}
impl Checkpoint {
    async fn value(&self) -> Value {
        self.value.lock().await.clone()
    }
    async fn save(&self, patch: Value) -> Result<()> {
        let mut value = self.value.lock().await;
        merge(&mut value, &patch);
        value["remainingMs"] = self
            .deadline
            .map(|deadline| (deadline - now()).max(0))
            .into();
        self.store
            .set(&format!("run-checkpoint:{}", self.id), value.clone(), None)
            .await
    }
    async fn memory(&self, key: &str, value: Value) {
        self.value.lock().await[key] = value;
    }
}
impl Worker {
    pub async fn initialize(&self, s: &Service) -> Result<()> {
        self.initialized
            .get_or_try_init(|| {
                s.store.transaction(|db| {
                    db.restore_chat_summaries()?;
                    for run in db.active()? {
                        let id = text(&run, "id");
                        if run["status"] != "running" {
                            if !run["cancelRequestedAt"].is_null() {
                                db.patch_run(
                                    id,
                                    &json!({
                                    "recoveryPending":true}
                                    ),
                                )?;
                            }
                            continue;
                        }
                        if db.kv(&format!("run-checkpoint:{id}"))?.is_none() && !run["startedAt"].is_null() {
                            db.patch_run(
                                id,
                                &json!({
                                "status":"interrupted","finishedAt":now(),"summary":"This older run has no restart checkpoint. Review its working files before retrying."}
                                ),
                            )?;
                            continue;
                        }
                        db.patch_run(
                            id,
                            &json!({
                            "status":"queued","recoveryPending":true,"finishedAt":null,"accountWaitReason":"Recovering after worker restart."}
                            ),
                        )?;
                        db.event(id, "status", "Recovering after worker restart", None)?;
                    }
                    Ok(())
                })
            })
            .await
            .map(|_| ())
    }
    pub async fn start(self: &Arc<Self>, s: Arc<Service>) -> Result<()> {
        self.initialize(&s).await?;
        self.tasks.spawn(crate::chat_titles::run(s.clone()));
        self.tasks.spawn(crate::nodes::backups::maintain(s.clone()));
        let retention = s.clone();
        self.tasks.spawn(async move {
            let mut timer = tokio::time::interval(Duration::from_secs(15));
            timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tokio::select! { _ = retention.shutdown.cancelled() => break, _ = timer.tick() => {} }
                tokio::select! {
                    _ = retention.shutdown.cancelled() => break,
                    result = retention.retention_tick() => {
                        if let Err(error) = result {
                            let _ = retention.store.audit("conversation.retention_failed", json!({"message":error.message})).await;
                        }
                    }
                }
            }
        });
        let worker = self.clone();
        self.tasks.spawn(async move {
            let mut timer = tokio::time::interval(Duration::from_secs(1));
            timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tokio::select! {
                    _ = s.shutdown.cancelled() => break,
                    _ = timer.tick() => {},
                    _ = worker.wake.notified() => {},
                }
                if let Err(error) = worker.tick(&s).await {
                    let _ = s
                        .store
                        .audit("worker.error", json!({"message": error.message}))
                        .await;
                }
            }
        });
        Ok(())
    }
    /// Coalesce wakeups without losing work queued during an active scheduling pass.
    pub fn notify(&self) {
        self.wake.notify_one();
    }
    pub async fn deployment_lease(
        &self,
        s: &Service,
        owner: String,
        release: bool,
    ) -> Result<Value> {
        // Finish an in-flight scheduling pass before reporting the worker idle.
        // Otherwise account selection could launch a run after the lease returns.
        let _tick = self.tick_lock.lock().await;
        s.store
            .transaction(move |db| {
                if db
                    .kv("deployment-lease")?
                    .is_some_and(|value| value != owner)
                {
                    return Err(Error::new(
                        409,
                        "Another deployment holds the worker lease.",
                    ));
                }
                if release {
                    db.delete("deployment-lease")?;
                } else {
                    db.set("deployment-lease", &owner.into(), Some(now() + 20 * 60000))?;
                }
                Ok(())
            })
            .await?;
        let active_runs = self.active.lock().await.len();
        drop(_tick);
        if release {
            self.notify();
        }
        Ok(json!({"paused": !release, "activeRuns": active_runs}))
    }
    pub async fn tick(self: &Arc<Self>, s: &Arc<Service>) -> Result<()> {
        let Ok(_tick) = self.tick_lock.try_lock() else {
            return Ok(());
        };
        if s.shutdown.is_cancelled() {
            return Ok(());
        }
        self.initialize(s).await?;
        if now() - self.maintenance.load(Ordering::Relaxed) > 3600000 {
            s.store
                .write(|db| {
                    db.0.execute("DELETE FROM kv WHERE expires IS NOT NULL AND expires<=?", [now()])?;
                    db.0.execute("DELETE FROM audit WHERE created_at<?", [now() - 90 * 86400000_i64])?;
                    db.0.execute("DELETE FROM events WHERE created_at<? AND run_id IN (SELECT id FROM runs WHERE status NOT IN ('queued','running') AND json_extract(data,'$.trigger')!='chat')", [now() - 30 * 86400000_i64])?;
                    Ok(())
                })
                .await?;
            self.maintenance.store(now(), Ordering::Relaxed);
        }
        let active = self
            .active
            .lock()
            .await
            .keys()
            .cloned()
            .collect::<HashSet<_>>();
        s.chat_tick(&active).await?;
        s.schedule().await?;
        if s.shutdown.is_cancelled() || s.store.kv("deployment-lease").await?.is_some() {
            return Ok(());
        }
        let locking_projects = |run: &Value| {
            if !execution::uses_vm(run, &s.config) {
                run_projects(run)
            } else {
                Vec::new()
            }
        };
        let mut projects = HashSet::new();
        for id in &active {
            for p in locking_projects(&s.store.run(id).await?) {
                projects.insert(text(&p, "id").to_owned());
            }
        }
        for run in s.store.read(|db| db.active()).await? {
            if self.active.lock().await.len() >= s.config.concurrency {
                break;
            }
            let run_id = text(&run, "id");
            if run["status"] != "queued"
                || locking_projects(&run)
                    .iter()
                    .any(|p| projects.contains(text(p, "id")))
            {
                continue;
            }
            let checkpoint = s.store.kv(&format!("run-checkpoint:{run_id}")).await?;
            if run["recoveryPending"] == true
                || checkpoint.as_ref().is_some_and(|c| c["launched"] == true)
            {
                for project in locking_projects(&run) {
                    projects.insert(text(&project, "id").to_owned());
                }
            }
            if run["recoveryPending"] == true {
                let result = async {
                    if !crate::nodes::moves::advance(s,&run).await? {return Ok(false);}
                    recovery::fence(s, &run).await?;
                    s.mcps.revoke_run(s, run_id).await?;
                    s.accounts.recover_run(s, &run).await?;
                    if !s.store.run(run_id).await?["cancelRequestedAt"].is_null() {
                        s.store
                            .patch_run(
                                run_id,
                                json!({
                                "status":"cancelled","finishedAt":now(),"accountWaitReason":null,"recoveryPending":false}
                                ),
                            )
                            .await?;
                        return Ok(false);
                    }
                    if let Some(checkpoint) = s.store.kv(&format!("run-checkpoint:{run_id}")).await? {
                        if checkpoint["settled"].is_object() {
                            let mut patch = checkpoint["settled"].clone();
                            merge(
                                &mut patch,
                                &json!({
                                "accountWaitReason":null,"recoveryPending":false}
                                ),
                            );
                            s.store.patch_run(run_id, patch).await?;
                            return Ok(false);
                        }
                        if checkpoint["completed"] == true {
                            s.store
                                .patch_run(
                                    run_id,
                                    json!({
                                    "status":"succeeded","finishedAt":now(),"resumeAvailable":false,"accountWaitReason":null,"recoveryPending":false,"summary":if text(&checkpoint,"lastMessage").is_empty(){
                                    "Conversation completed before worker restart. See Activity for the recorded result."}
                                    else{
                                    text(&checkpoint,"lastMessage")}
                                    }
                                    ),
                                )
                                .await?;
                            return Ok(false);
                        }
                    }
                    s.store
                        .patch_run(
                            run_id,
                            json!({
                            "recoveryPending":false}
                            ),
                        )
                        .await?;
                    Ok::<_, Error>(true)
                }
                .await;
                match result {
                    Ok(true) => {}
                    Ok(false) => continue,
                    Err(_) => {
                        let message = "Waiting for the previous execution to stop before recovery.";
                        if run["accountWaitReason"] != message {
                            s.store
                                .patch_run(
                                    run_id,
                                    json!({
                                    "accountWaitReason":message}
                                    ),
                                )
                                .await?;
                        }
                        continue;
                    }
                }
            }
            let provider = Provider::of_run(&run);
            let acquired = s
                .accounts
                .acquire(
                    s,
                    run_id,
                    provider,
                    text(&run["snapshot"]["agent"], "model"),
                )
                .await;
            let account = match acquired {
                Ok(account) => account,
                Err(error) => {
                    if run["accountWaitReason"] != error.message {
                        // Clients offer Connections when the wait needs the user.
                        let required = s
                            .accounts
                            .needs_attention(s, provider)
                            .await?
                            .then_some(provider);
                        s.store
                            .patch_run(
                                run_id,
                                json!({"accountWaitReason":error.message,"accountRequired":required}),
                            )
                            .await?;
                        s.store
                            .event(run_id, "status", &error.message, None)
                            .await?;
                    }
                    continue;
                }
            };
            if s.shutdown.is_cancelled() || s.store.run(run_id).await?["status"] != "queued" {
                if let Some(account) = account {
                    s.accounts.release(&account).await?;
                }
                continue;
            }
            for project in locking_projects(&run) {
                projects.insert(text(&project, "id").to_owned());
            }
            let cancel = CancellationToken::new();
            self.active
                .lock()
                .await
                .insert(run_id.into(), cancel.clone());
            let s = s.clone();
            let worker = self.clone();
            self.tasks.spawn(async move {
                let run_id = text(&run, "id").to_owned();
                if let Err(error) = worker.execute(&s, run, account, cancel).await {
                    // Keep the slot occupied until recovery is durable. A transient
                    // disk/database failure must not strand a running record or
                    // launch a second process before the previous one is fenced.
                    loop {
                        let id = run_id.clone();
                        let lease = s.accounts.lease(&id).await;
                        let saved = s.store.transaction(move |db| {
                            let Some(current) = db.run(&id)? else { return Ok(()); };
                            let key = format!("run-checkpoint:{id}");
                            let mut checkpoint = db.kv(&key)?.unwrap_or_else(|| json!({"remainingMs":0}));
                            if !["queued", "running"].contains(&text(&current, "status")) {
                                checkpoint["settled"] = json!({"status":current["status"],"summary":current["summary"],"finishedAt":current["finishedAt"]});
                            }
                            db.set(&key, &checkpoint, None)?;
                            let mut patch = json!({"status":"queued","recoveryPending":true,"finishedAt":null,"accountWaitReason":"Recovering after a worker error."});
                            if let Some(lease) = lease { patch["accountId"] = lease.account_id.into(); }
                            db.patch_run(&id, &patch)?;
                            Ok(())
                        }).await;
                        if saved.is_ok() { break; }
                        tokio::select! { _=s.shutdown.cancelled()=>break, _=tokio::time::sleep(Duration::from_secs(1))=>{} }
                    }
                    let _ = s
                        .store
                        .audit(
                            "worker.execution_failed",
                            json!({
                            "runId":run_id,"message":error.message}
                            ),
                        )
                        .await;
                }
                worker.active.lock().await.remove(&run_id);
                worker.notify();
            });
        }
        Ok(())
    }
    async fn execute(
        &self,
        s: &Arc<Service>,
        mut run: Value,
        mut account: Option<Lease>,
        cancel: CancellationToken,
    ) -> Result<()> {
        let run_id = text(&run, "id").to_owned();
        let saved = s.store.kv(&format!("run-checkpoint:{run_id}")).await?;
        let existing = saved.is_some();
        let value = saved.unwrap_or_else(|| {
            json!({
            "launched":false,"remainingMs":crate::run_limits::budget_ms(&run["snapshot"]["agent"])}
            )
        });
        let checkpoint = Arc::new(Checkpoint {
            deadline: if value.get("remainingMs") == Some(&Value::Null) {
                None
            } else {
                Some(now() + value["remainingMs"].as_i64().unwrap_or(0))
            },
            value: Mutex::new(value),
            store: s.store.clone(),
            id: run_id.clone(),
        });
        let stop_heartbeat = CancellationToken::new();
        let _heartbeat_guard = stop_heartbeat.clone().drop_guard();
        let heartbeat = {
            let c = checkpoint.clone();
            let stop = stop_heartbeat.clone();
            tokio::spawn(async move {
                let mut timer = tokio::time::interval(Duration::from_secs(5));
                loop {
                    tokio::select! {
                    _=stop.cancelled()=>break,_=timer.tick()=>{
                    let _=c.save(json!({
                    }
                    )).await;
                    }
                    }
                }
            })
        };
        let mut sensitive = Vec::new();
        let result = Self::execute_inner(
            s,
            &mut run,
            &mut account,
            &cancel,
            &checkpoint,
            existing,
            &mut sensitive,
        )
        .await;
        if let Err(error) = result {
            let saved = checkpoint.value().await;
            let recover_controller = error.status == 503
                && saved["prepared"]["backend"] == "firecracker"
                && (saved["controllerRecoveries"].as_u64().unwrap_or(0) < 3
                    || s.store.run(&run_id).await?["moveRequest"].is_object()
                    || saved["nodeId"]
                        .as_str()
                        .is_some_and(|id| id != crate::nodes::LOCAL_NODE_ID))
                && s.store.run(&run_id).await?["sessionId"].is_string()
                && checkpoint.deadline.is_none_or(|deadline| now() < deadline)
                && !cancel.is_cancelled()
                && !s.shutdown.is_cancelled();
            if recover_controller {
                checkpoint
                    .save(json!({
                        "controllerRecoveries":saved["controllerRecoveries"].as_u64().unwrap_or(0)+1
                    }))
                    .await?;
            }
            let status = if cancel.is_cancelled() {
                "cancelled"
            } else if s.shutdown.is_cancelled() || recover_controller {
                "queued"
            } else {
                "failed"
            };
            let needs_fence =
                saved["prepared"]["isolated"] == true && saved["runnerId"].is_string();
            let summary = run_output::redact(&error.message, &sensitive);
            if needs_fence && status != "queued" {
                checkpoint
                    .save(json!({
                    "settled":{
                    "status":status,"summary":summary,"error":summary,"finishedAt":now()}
                    }
                    ))
                    .await?;
            }
            s.store
                .patch_run(
                    &run_id,
                    json!({
                    "status":if needs_fence{
                    "queued"}
                    else{
                    status}
                    ,"recoveryPending":needs_fence||status=="queued","finishedAt":if needs_fence||status=="queued"{
                    Value::Null}
                    else{
                    now().into()}
                    ,"summary":summary,"error":if status=="failed"{json!(summary)}else{Value::Null},"accountWaitReason":if needs_fence{
                    json!("Waiting for the previous VM to stop.")}
                    else if status=="queued"{
                    json!("Paused for worker restart. This run will resume automatically.")}
                    else{
                    Value::Null}
                    }
                    ),
                )
                .await?;
            s.store
                .event(
                    &run_id,
                    if s.shutdown.is_cancelled() {
                        "status"
                    } else {
                        "error"
                    },
                    &summary,
                    None,
                )
                .await?;
        }
        let saved = checkpoint.value().await;
        let mut fenced = true;
        if saved["prepared"]["isolated"] == true
            && saved["runnerId"].is_string()
            && recovery::fence(s, &s.store.run(&run_id).await?)
                .await
                .is_err()
        {
            fenced = false;
            let current = s.store.run(&run_id).await?;
            if current["status"] != "queued" {
                checkpoint
                    .save(json!({
                    "settled":{
                    "status":current["status"],"summary":current["summary"],"finishedAt":current["finishedAt"]}
                    }
                    ))
                    .await?;
            }
            s.store
                .patch_run(
                    &run_id,
                    json!({
                    "status":"queued","recoveryPending":true,"finishedAt":null,"accountWaitReason":"Waiting for the previous VM to stop."}
                    ),
                )
                .await?;
        }
        stop_heartbeat.cancel();
        let _ = heartbeat.await;
        checkpoint.save(json!({})).await?;
        let saved = checkpoint.value().await;
        if fenced && saved["settled"].is_object() {
            let mut patch = saved["settled"].clone();
            merge(
                &mut patch,
                &json!({
                "recoveryPending":false,"accountWaitReason":null}
                ),
            );
            s.store.patch_run(&run_id, patch).await?;
        }
        if fenced
            && let Some(account) = &account
            && s.accounts.release(account).await.is_err()
        {
            s.store
                .audit(
                    "account.release_failed",
                    json!({
                    "id":account.account_id,"runId":run_id}
                    ),
                )
                .await?;
        }
        s.mcps.revoke_run(s, &run_id).await?;
        let pending = s.store.run(&run_id).await?;
        if pending["moveRequest"].is_object() && pending["cancelRequestedAt"].is_null() {
            checkpoint
                .save(json!({"completed":false,"settled":null}))
                .await?;
            s.store
                .patch_run(
                    &run_id,
                    json!({"status":"queued","recoveryPending":true,"finishedAt":null}),
                )
                .await?;
        }
        if fenced
            && saved["prepared"]["backend"] == "firecracker"
            && s.store.run(&run_id).await?["status"] == "succeeded"
        {
            crate::nodes::backups::attempt(s, &s.store.run(&run_id).await?).await;
        }
        if fenced {
            for provider in Provider::ALL {
                let _ = provider.driver().recover(s, &run_id).await;
            }
            let _ = tokio::fs::remove_dir_all(
                s.config
                    .data_dir
                    .join("runs")
                    .join(&run_id)
                    .join("home/.config/gh"),
            )
            .await;
        }
        if let Some(runner) = saved["runnerId"].as_str() {
            let _ = tokio::fs::remove_file(
                s.config
                    .data_dir
                    .join("runner-plans")
                    .join(format!("{runner}.json")),
            )
            .await;
        }
        Ok(())
    }
    async fn execute_inner(
        s: &Arc<Service>,
        run: &mut Value,
        account: &mut Option<Lease>,
        cancel: &CancellationToken,
        checkpoint: &Arc<Checkpoint>,
        existing: bool,
        sensitive: &mut Vec<String>,
    ) -> Result<()> {
        let id = text(run, "id").to_owned();
        let provider = Provider::of_run(run);
        let claude = provider == Provider::Claude;
        checkpoint.save(json!({})).await?;
        let account_name = if let Some(account) = account {
            s.accounts.get(s, &account.account_id).await?["name"].clone()
        } else {
            Value::Null
        };
        s.store
            .patch_run(
                &id,
                json!({"status":"running","startedAt":run["startedAt"].as_i64().unwrap_or_else(now),"finishedAt":null,"accountWaitReason":null,"accountRequired":null,"accountId":account.as_ref().map(|a|&a.account_id),"accountName":account_name}),
            )
            .await?;
        if let Some(name) = account_name.as_str() {
            s.store
                .event(
                    &id,
                    "status",
                    &format!("Using {} account: {name}", provider.label()),
                    None,
                )
                .await?;
        }
        s.store
            .event(&id, "status", "Preparing workspace", None)
            .await?;
        let directory = s.config.data_dir.join("runs").join(&id);
        private_dir(&directory).await?;
        let current = s
            .get("agents", text(&run["snapshot"]["agent"], "id"))
            .await?;
        crate::nodes::require_node(&current)?;
        if policy(&current) != policy(&run["snapshot"]["agent"]) {
            return Err(Error::new(
                409,
                "Agent access changed after this run was queued. Run the task again with the current policy.",
            ));
        }
        let saved = checkpoint.value().await;
        if existing && !saved["prepared"].is_object() {
            checkpoint
                .save(json!({
                "generation":crate::config::id()[..8].to_owned()}
                ))
                .await?;
        }
        let saved = checkpoint.value().await;
        let github = s
            .store
            .kv(&format!(
                "agent-github:{}",
                text(&run["snapshot"]["agent"], "id")
            ))
            .await?;
        let prepared = if saved["prepared"].is_object() {
            execution::restore(
                run,
                saved["prepared"].clone(),
                &s.config,
                github.as_ref().and_then(Value::as_str),
            )
            .await?
        } else {
            execution::prepare(
                run,
                &s.config,
                github.as_ref().and_then(Value::as_str),
                account.as_ref().map(|a| a.home.as_path()),
                saved["generation"].as_str(),
            )
            .await?
        };
        checkpoint
            .save(json!({
            "prepared":prepared}
            ))
            .await?;
        let codex_home = directory.join(if prepared["isolated"] == true {
            "home/.codex"
        } else {
            "codex"
        });
        private_dir(&codex_home).await?;
        if !claude && prepared["isolated"] != true {
            execution::codex_home(&s.config, &codex_home).await?;
        }
        if let Some(account) = account
            && prepared["isolated"] == true
            && !claude
        {
            s.accounts.relocate(s, account, &codex_home).await?;
        }
        let patch = json!({
        "workspace":prepared["cwd"],"workspaces":prepared["workspaces"],"isolated":prepared["isolated"]}
        );
        merge(run, &patch);
        s.store.patch_run(&id, patch).await?;
        let mut env = if prepared["isolated"] == true {
            std::env::vars().collect::<Environment>()
        } else {
            crate::toolkit::environment(&s.config.home, std::env::vars().collect()).await?
        };
        crate::process::remove_archive_environment(&mut env);
        env.insert("HOME".into(), s.config.home.to_string_lossy().into_owned());
        env.insert(
            "CODEX_HOME".into(),
            codex_home.to_string_lossy().into_owned(),
        );
        for key in ["OPENAI_API_KEY", "CODEX_API_KEY", "CODEX_THREAD_ID"] {
            env.remove(key);
        }
        if claude {
            // The session belongs to the run, so it can continue on another Claude account.
            let home = account
                .as_ref()
                .map(|a| a.home.clone())
                .unwrap_or_else(|| directory.join("home/.claude"));
            crate::claude::configure(&mut env, &home);
        }
        let mcp = s.mcps.run_configuration(s, run).await?;
        sensitive.extend(
            mcp["redactions"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(str::to_owned),
        );
        if let Some(account) = account {
            sensitive.extend(s.accounts.redactions(s, account).await?);
        }
        for (key, value) in mcp["env"].as_object().unwrap() {
            env.insert(key.clone(), value.as_str().unwrap_or("").into());
        }
        let mut resume = if checkpoint.value().await["launched"] == true {
            Some(recovery::session(s, run, &codex_home, Path::new(text(&prepared, "cwd"))).await?)
        } else {
            None
        };
        if let Some(session) = &resume {
            s.store
                .patch_run(
                    &id,
                    json!({
                    "sessionId":session,"resumeCount":run["resumeCount"].as_u64().unwrap_or(0)+1,"resumeAvailable":true}
                    ),
                )
                .await?;
            s.store
                .event(
                    &id,
                    "status",
                    "Resuming saved conversation and workspace",
                    None,
                )
                .await?;
        }
        let mut total = 0;
        loop {
            if cancel.is_cancelled() || s.shutdown.is_cancelled() {
                return Err(Error::new(409, "Execution stopped."));
            }
            if checkpoint
                .deadline
                .is_some_and(|deadline| now() >= deadline)
            {
                return Err(Error::new(409, "Run exceeded its time limit."));
            }
            let output = text(&prepared, "output");
            match tokio::fs::remove_file(output).await {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
            if !claude {
                env.insert(
                    "CODEX_HOME".into(),
                    account
                        .as_ref()
                        .map(|a| a.home.clone())
                        .unwrap_or_else(|| codex_home.clone())
                        .to_string_lossy()
                        .into_owned(),
                );
            }
            let mut prompt = if resume.is_some() {
                "Continue the same task from the saved conversation and current workspace. Execution was interrupted. Resume the original task from its last completed step. Preserve completed work and verify external effects before repeating any action.".into()
            } else {
                run_output::prompt(run, false)
            };
            if resume.is_some()
                && let Some(restored) = run["restoredAt"].as_i64()
            {
                prompt.push_str(&format!("\nThe VM files and native provider session were restored from recovery point at Unix time {restored} ms. Master chat may contain more recent messages and actions than this workspace. Review the master conversation history with Leo tools, reconcile file state, and verify external effects (commits, messages, deployments) before repeating them. Do not assume later chat messages prove those changes exist on this disk."));
            }
            if resume.is_some()
                && let Some(restored) = run["restoredAt"].as_i64()
            {
                let run_id = id.clone();
                let (events, _) = s
                    .store
                    .read(move |db| db.events_before(&run_id, i64::MAX))
                    .await?;
                let newer=events.into_iter().filter_map(|event|serde_json::to_value(event).ok()).filter(|event|event["createdAt"].as_i64().is_some_and(|at|at>restored)).map(|event|json!({"at":event["createdAt"],"type":event["type"],"text":event["text"]})).collect::<Vec<_>>();
                if !newer.is_empty() {
                    prompt.push_str("\nRecent master history after this recovery point (messages are history, not proof that disk changes survived):\n");
                    prompt.extend(serde_json::to_string(&newer)?.chars().take(16000));
                }
            }
            let mut binary = s.config.codex_bin.clone();
            let mut args = mcp["args"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect::<Vec<_>>();
            args.extend(run_output::args(run, output, resume.as_deref()));
            let _auth_broker = if let Some(account) = account.as_ref() {
                Some(broker::serve(s, account).await?)
            } else {
                None
            };
            let chat = if claude || run["chatExecution"].is_object() || account.is_some() {
                private_dir(&directory.join("chat-input")).await?;
                s.prepare_chat_files(text(run, "id"), &run["chatExecution"]["attachments"])
                    .await?;
                let mut chat =
                    run_output::chat_plan(run, &prepared, &directory, &mcp, resume.as_deref());
                if claude {
                    chat["claudeManagedAuth"] = account.is_some().into();
                }
                if !run["chatExecution"].is_object() {
                    // Scheduled work uses the same authenticated protocol as
                    // chats, while retaining its own task brief and session.
                    chat["execution"] = json!({"messageId":id,"text":prompt,"recovery":resume.is_some(),"attachments":[]});
                }
                if !claude && text(&chat, "reasoning").is_empty() {
                    let source = account
                        .as_ref()
                        .map(|a| a.account_id.as_str())
                        .unwrap_or("");
                    if let Some((model, effort)) =
                        crate::models::cached_defaults(s, source, text(&chat, "model")).await?
                    {
                        chat["model"] = model.into();
                        chat["reasoning"] = effort.into();
                    }
                }
                binary = std::env::current_exe()?.to_string_lossy().into_owned();
                args = vec![
                    "chat".into(),
                    if claude {
                        s.config.claude_bin.clone()
                    } else {
                        s.config.codex_bin.clone()
                    },
                ];
                prompt = chat.to_string();
                Some(chat)
            } else {
                None
            };
            if prepared["isolated"] == true {
                let runner = id::new();
                let placement =
                    crate::nodes::placement::reserve(s, &s.store.run(&id).await?, &runner).await?;
                crate::nodes::placement::materialize(s, &runner).await?;
                checkpoint
                    .save(json!({
                    "runnerId":runner,"nodeId":placement["nodeId"],"runtimeId":placement["runtimeId"]}
                    ))
                    .await?;
                let plans = s.config.data_dir.join("runner-plans");
                private_dir(&plans).await?;
                let mut mounts = prepared["mounts"].as_array().unwrap().clone();
                if chat.is_some() {
                    mounts.push(json!({
                    "source":directory.join("chat-input"),"target":"/run/leo-chat","readOnly":true}
                    ));
                }
                let mut context = run.clone();
                context["snapshot"]["skills"] = prepared["skills"].clone();
                let mut plan = json!({
                "id":runner,"runId":id,"args":args,"cwd":prepared["cwd"],"prompt":if resume.is_some(){
                prompt.clone()}
                else{
                run_output::prompt(&context,false)}
                ,"imports":mounts,"expires":checkpoint.deadline,"sandbox":policy(&run["snapshot"]["agent"])["sandbox"],"mcpEnv":mcp["env"]}
                );
                plan["resources"] = placement["resources"].clone();
                if let Some(chat) = chat {
                    plan["chat"] = chat;
                }
                atomic_write(
                    &plans.join(format!("{runner}.json")),
                    &serde_json::to_vec(&plan)?,
                )
                .await?;
                let runner_url = crate::nodes::transport::url(s, &id).await?;
                if placement["nodeId"] != crate::nodes::LOCAL_NODE_ID {
                    let prepared = s
                        .http
                        .post(format!("{runner_url}/prepare/{runner}"))
                        .bearer_auth(execution::secret(&s.config.data_dir, "runner-secret").await?)
                        .timeout(Duration::from_secs(300))
                        .send()
                        .await
                        .map_err(|_| {
                            Error::new(503, "Remote workspace preparation interrupted.")
                        })?;
                    if !prepared.status().is_success() {
                        return Err(Error::new(
                            503,
                            "Remote workspace preparation failed; source files are preserved.",
                        ));
                    }
                    s.store
                        .event(
                            &id,
                            "status",
                            &format!("Executing on node {}", text(&placement, "nodeId")),
                            None,
                        )
                        .await?;
                }
                s.store
                    .patch_run(
                        &id,
                        json!({"nodeId":placement["nodeId"],"resources":placement["resources"],"nodeState":null,"movementError":null}),
                    )
                    .await?;
                env.insert("RUNNER_URL".into(), runner_url);
                env.insert(
                    "RUNNER_TOKEN".into(),
                    execution::secret(&s.config.data_dir, "runner-secret").await?,
                );
                binary = std::env::current_exe()?.to_string_lossy().into_owned();
                args = vec!["runner-client".into(), runner];
            }
            let mut child =
                Supervised::spawn(&binary, &args, &env, Path::new(text(&prepared, "cwd"))).await?;
            let identity = recovery::process_identity(child.child.id().unwrap()).await?;
            if run["chatExecution"].is_object() {
                run["chatExecution"]["recovery"] = true.into();
                s.store
                    .patch_run(
                        &id,
                        json!({
                        "chatExecution":run["chatExecution"]}
                        ),
                    )
                    .await?;
            }
            checkpoint
                .save(json!({
                "process":identity,"launched":true,"completed":false,"lastError":null}
                ))
                .await?;
            if cancel.is_cancelled() || s.shutdown.is_cancelled() {
                child.stop().await;
                return Err(Error::new(409, "Execution stopped before launch."));
            }
            let (stdout, stderr) = (
                child.child.stdout.take().unwrap(),
                child.child.stderr.take().unwrap(),
            );
            let (tx, mut events) = mpsc::channel(32);
            let out = tokio::spawn(read_output(stdout, false, tx.clone()));
            let err = tokio::spawn(read_output(stderr, true, tx));
            if let Err(error) = child.start(prompt).await {
                child.stop().await;
                out.abort();
                err.abort();
                return Err(error);
            }
            let mut exhausted = false;
            let mut timed_out = false;
            let mut status = None;
            let mut open = true;
            let timeout = crate::run_limits::wait_until(checkpoint.deadline);
            tokio::pin!(timeout);
            while status.is_none() || open {
                tokio::select! {
                                    _=cancel.cancelled(),if status.is_none()=>{
                child.stop().await;
                status=Some(143);
                }
                ,
                                    _=s.shutdown.cancelled(),if status.is_none()=>{
                child.stop().await;
                status=Some(143);
                }
                ,
                                    _=&mut timeout,if status.is_none()=>{
                timed_out=true;
                child.stop().await;
                status=Some(143);
                }
                ,
                                    code=child.child.wait(),if status.is_none()=>status=Some(code?.code().unwrap_or(143)),
                                    event=events.recv(),if open=>match event{
                Some((diagnostic,raw))=>{
                if let Some(account) = account.as_ref() {
                    for secret in s.accounts.redactions(s, account).await? {
                        if !sensitive.contains(&secret) { sensitive.push(secret); }
                    }
                }
                exhausted|=record(s,&id,&raw,diagnostic,checkpoint,sensitive,&mut total).await?;
                }
                ,None=>open=false}
                                }
            }
            let _ = out.await;
            let _ = err.await;
            drop(_auth_broker);
            checkpoint
                .save(json!({
                "process":null}
                ))
                .await?;
            if prepared["isolated"] == true {
                recovery::fence(s, &s.store.run(&id).await?).await?;
            }
            let saved = checkpoint.value().await;
            if s.shutdown.is_cancelled() && !cancel.is_cancelled() && saved["completed"] != true {
                return Err(Error::new(409, "Worker is restarting"));
            }
            if prepared["backend"] == "firecracker"
                && status == Some(crate::runner::CONTROLLER_INTERRUPTED)
                && !cancel.is_cancelled()
                && !timed_out
                && !exhausted
            {
                return Err(Error::new(
                    503,
                    "VM controller interrupted execution. The saved conversation and workspace have been preserved.",
                ));
            }
            let session = s.store.run(&id).await?["sessionId"]
                .as_str()
                .map(str::to_owned);
            if exhausted
                && status != Some(0)
                && account.is_some()
                && !cancel.is_cancelled()
                && !s.shutdown.is_cancelled()
                && !timed_out
                && session.is_some()
            {
                let previous = account.take().unwrap();
                let model = text(&run["snapshot"]["agent"], "model");
                s.accounts.exhausted(s, &previous.account_id, model).await?;
                s.accounts.release(&previous).await?;
                s.store
                    .patch_run(
                        &id,
                        json!({"accountWaitReason":format!("Usage exhausted. Waiting for an available {} account to resume.", provider.label()),"accountId":null,"accountName":null}),
                    )
                    .await?;
                s.store.event(&id, "status", "Usage exhausted. Saving this session and restoring capacity or switching accounts.", None).await?;
                s.accounts.refresh(s, &previous.account_id).await?;
                while account.is_none()
                    && !cancel.is_cancelled()
                    && !s.shutdown.is_cancelled()
                    && checkpoint.deadline.is_none_or(|deadline| now() < deadline)
                {
                    match s.accounts.acquire(s, &id, provider, model).await {
                        Ok(value) => *account = value,
                        Err(error) if error.status == 409 => {
                            tokio::select! {
                            _=tokio::time::sleep(Duration::from_secs(1))=>{
                            }
                            ,_=cancel.cancelled()=>{
                            }
                            ,_=s.shutdown.cancelled()=>{
                            }
                            }
                        }
                        Err(error) => return Err(error),
                    }
                }
                if let Some(account) = account {
                    if prepared["isolated"] == true || claude {
                        s.accounts.relocate(s, account, &previous.home).await?;
                    } else {
                        execution::codex_home(&s.config, &account.home).await?;
                    }
                    sensitive.extend(s.accounts.redactions(s, account).await?);
                    let name = s.accounts.get(s, &account.account_id).await?["name"].clone();
                    s.store
                        .patch_run(
                            &id,
                            json!({"accountWaitReason":null,"accountId":account.account_id,"accountName":name}),
                        )
                        .await?;
                    s.store
                        .event(
                            &id,
                            "status",
                            &format!(
                                "Resuming saved session with {} account: {}",
                                provider.label(),
                                name.as_str().unwrap_or("")
                            ),
                            None,
                        )
                        .await?;
                }
                resume = session;
                continue;
            }
            let output_summary = crate::skills::small_file(Path::new(output))
                .await
                .unwrap_or_default();
            let exit_status = status;
            let status = if cancel.is_cancelled() {
                "cancelled"
            } else if timed_out {
                "failed"
            } else if status == Some(0) || (s.shutdown.is_cancelled() && saved["completed"] == true)
            {
                "succeeded"
            } else {
                "failed"
            };
            let error = if cancel.is_cancelled() {
                "Run stopped. You can resume this conversation.".into()
            } else if timed_out {
                "Run exceeded its time limit.".into()
            } else if status == "succeeded" {
                String::new()
            } else if !text(&saved, "lastError").is_empty() {
                text(&saved, "lastError").to_owned()
            } else {
                match exit_status {
                    Some(code) => format!(
                        "Process exited with status {code}. Resume the conversation to continue."
                    ),
                    None => {
                        "Process stopped before finishing. Resume the conversation to continue."
                            .into()
                    }
                }
            };
            let summary = if status != "succeeded" {
                &error
            } else if !output_summary.is_empty() {
                &output_summary
            } else {
                text(&saved, "lastMessage")
            };
            let summary = run_output::redact(summary, sensitive)
                .chars()
                .take(100000)
                .collect::<String>();
            s.store
                .patch_run(
                    &id,
                    json!({
                    "status":status,"finishedAt":now(),"resumeAvailable":(status!="succeeded"||run["chatExecution"].is_object())&&session.is_some(),"summary":summary,"error":if error.is_empty(){Value::Null}else{json!(run_output::redact(&error,sensitive))}}
                    ),
                )
                .await?;
            s.store.event(&id, "status", status, None).await?;
            break;
        }
        Ok(())
    }
    pub async fn cancel(&self, s: &Service, id: &str) -> Result<()> {
        let active = self.active.lock().await;
        let is_active = active.contains_key(id);
        let id_owned = id.to_owned();
        s.store
            .transaction(move |db| {
                let run = required(db.run(&id_owned)?, "Run not found")?;
                if !["queued", "running"].contains(&text(&run, "status")) {
                    return Err(Error::new(409, "This run has already finished."));
                }
                db.patch_run(
                    &id_owned,
                    &json!({
                    "cancelRequestedAt":now()}
                    ),
                )?;
                if !is_active && run["recoveryPending"] != true {
                    db.patch_run(
                        &id_owned,
                        &json!({
                        "status":"cancelled","finishedAt":now(),"summary":"Cancelled before execution."}
                        ),
                    )?;
                }
                db.audit(
                    "run.cancelled",
                    &json!({
                    "id":id_owned}
                    ),
                )
            })
            .await?;
        if let Some(cancel) = active.get(id) {
            cancel.cancel();
        }
        self.notify();
        Ok(())
    }
    pub async fn resume(&self, s: &Service, id: &str) -> Result<Value> {
        let active = self.active.lock().await;
        if active.contains_key(id) {
            return Err(Error::new(
                409,
                "Wait for this run to finish stopping before resuming.",
            ));
        }
        let id = id.to_owned();
        let result = s.store
            .transaction(move |db| {
                let run = required(db.run(&id)?, "Run not found")?;
                crate::conversation_lifecycle::require_active_run(db, &id)?;
                if db.list("chats")?.iter().any(|c| c["runId"] == id && (c["cancelledByDeletion"] == true || c["sessionRestartRequested"] == true)) {
                    return Err(Error::new(409,"Send a new message and resume the conversation queue to continue; cancelled work will not replay."));
                }
                let key = format!("run-checkpoint:{id}");
                let checkpoint = db.kv(&key)?;
                let before_launch = run["chatExecution"].is_object() && ["failed", "cancelled"].contains(&text(&run, "status")) && checkpoint.as_ref().is_none_or(|c| c["launched"] != true);
                if !before_launch && (!["failed", "interrupted", "cancelled"].contains(&text(&run, "status")) || run["resumeAvailable"] != true || checkpoint.as_ref().is_none_or(|c| !c["prepared"].is_object()) || !run["workspaceCleanedAt"].is_null()) {
                    return Err(Error::new(409, "This run has no saved conversation available to resume."));
                }
                if db.active()?.iter().any(|a| a["taskId"] == run["taskId"]) {
                    return Err(Error::new(409, "This task already has an active run."));
                }
                if let Some(mut checkpoint) = checkpoint {
                    checkpoint["remainingMs"] = crate::run_limits::budget_ms(&run["snapshot"]["agent"]).into();
                    if !before_launch {
                        checkpoint["completed"] = false.into();
                    }
                    checkpoint.as_object_mut().unwrap().remove("settled");
                    db.set(&key, &checkpoint, None)?;
                }
                let result = db.patch_run(
                    &id,
                    &json!({
                    "status":"queued","error":null,"outcome":null,"recoveryPending":true,"cancelRequestedAt":null,"finishedAt":null,"accountWaitReason":if before_launch{
                    Value::Null}
                    else{
                    "Resuming saved conversation.".into()}
                    }
                    ),
                )?;
                db.event(&id, "status", "Resume requested", None)?;
                Ok(result)
            })
            .await?;
        self.notify();
        Ok(result)
    }
    pub async fn close(&self) {
        let _guard = self.tick_lock.lock().await;
        self.tasks.close();
        self.tasks.wait().await;
    }
}
// Separate pipe readers keep stderr draining even while stdout applies database backpressure.
async fn read_output(
    mut reader: impl AsyncRead + Unpin,
    diagnostic: bool,
    events: mpsc::Sender<(bool, String)>,
) {
    let mut bytes = [0; 8192];
    let mut line = Vec::new();
    loop {
        let n = match reader.read(&mut bytes).await {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        if diagnostic {
            if events
                .send((true, String::from_utf8_lossy(&bytes[..n]).into_owned()))
                .await
                .is_err()
            {
                return;
            }
            continue;
        }
        for byte in &bytes[..n] {
            if *byte == b'\n' {
                if events
                    .send((false, String::from_utf8_lossy(&line).into_owned()))
                    .await
                    .is_err()
                {
                    return;
                }
                line.clear();
            } else if line.len() < 2_000_000 {
                line.push(*byte);
            }
        }
    }
    if !line.is_empty() {
        let _ = events
            .send((diagnostic, String::from_utf8_lossy(&line).into_owned()))
            .await;
    }
}
async fn record(
    s: &Service,
    id: &str,
    raw: &str,
    diagnostic: bool,
    checkpoint: &Checkpoint,
    secrets: &[String],
    total: &mut usize,
) -> Result<bool> {
    if diagnostic {
        if *total < 5_000_000 {
            *total += raw.len();
            s.store
                .event(id, "diagnostic", &run_output::redact(raw, secrets), None)
                .await?;
        }
        return Ok(false);
    }
    let event = serde_json::from_str::<Value>(raw).ok();
    if let Some(event) = event {
        match text(&event, "type") {
            "chat.question" => {
                s.question_receive(id, event["question"].clone()).await?;
                return Ok(false);
            }
            "chat.question.closed" => {
                s.question_release(id, event["questionId"].as_str()).await?;
                return Ok(false);
            }
            "chat.delivered" => {
                if let Some(message) = event["messageId"].as_str() {
                    s.chat_acknowledge(id, message).await?;
                }
                return Ok(false);
            }
            _ => {}
        }
        let exhausted = run_output::exhausted(&event);
        if event["type"] == "turn.failed" && !text(&event["error"], "message").is_empty() {
            let saved = checkpoint.value().await;
            if text(&saved, "lastError").is_empty() {
                checkpoint.save(json!({"lastError":run_output::redact(text(&event["error"], "message"), secrets).chars().take(10000).collect::<String>()})).await?;
            }
        }
        if event["type"] == "thread.started" && event["thread_id"].is_string() {
            s.store
                .patch_run(
                    id,
                    json!({
                    "sessionId":event["thread_id"],"resumeAvailable":true}
                    ),
                )
                .await?;
        }
        if event["item"]["type"] == "agent_message" && event["item"]["text"].is_string() {
            checkpoint
                .memory(
                    "lastMessage",
                    run_output::redact(text(&event["item"], "text"), secrets)
                        .chars()
                        .take(100000)
                        .collect::<String>()
                        .into(),
                )
                .await;
        }
        if event["type"] == "turn.completed" {
            checkpoint
                .save(json!({
                "completed":true}
                ))
                .await?;
            if !event["usage"].is_null() {
                s.store
                    .patch_run(
                        id,
                        json!({
                        "usage":event["usage"]}
                        ),
                    )
                    .await?;
            }
        }
        // Tool/diagnostic volume must never hide the conversation or its outcome.
        // Keep turn markers as well: clients use them to scope reused message IDs.
        let conversation = event["item"]["type"] == "agent_message"
            || [
                "thread.started",
                "turn.started",
                "turn.completed",
                "turn.failed",
                "error",
            ]
            .contains(&text(&event, "type"));
        if conversation || *total < 5_000_000 {
            if !conversation {
                *total += raw.len();
            }
            let value = event["item"]
                .get("text")
                .or_else(|| event["item"].get("aggregated_output"))
                .or_else(|| event["error"].get("message"));
            let value = value
                .map(|v| {
                    v.as_str()
                        .map(str::to_owned)
                        .unwrap_or_else(|| v.to_string())
                })
                .unwrap_or_else(|| raw.into());
            s.store
                .event(
                    id,
                    if text(&event, "type").is_empty() {
                        "output"
                    } else {
                        text(&event, "type")
                    },
                    &run_output::redact(&value, secrets),
                    Some(run_output::payload(&event, secrets)),
                )
                .await?;
        }
        if event["type"] == "turn.completed" {
            // Title failures cannot change the outcome of the user's turn.
            let _ = crate::chat_titles::enqueue(s, id).await;
        }
        return Ok(exhausted);
    }
    if *total < 5_000_000 {
        *total += raw.len();
        s.store
            .event(id, "output", &run_output::redact(raw, secrets), None)
            .await?;
    }
    Ok(false)
}
mod id {
    pub fn new() -> String {
        crate::config::id()
    }
}
impl Worker {
    pub async fn cleanup(&self, s: &Service, id: &str) -> Result<Value> {
        let _active = self.active.lock().await;
        let run = s.store.run(id).await?;
        if ["queued", "running"].contains(&text(&run, "status")) || _active.contains_key(id) {
            return Err(Error::new(
                409,
                "Wait for this run to finish before cleaning up.",
            ));
        }
        if run["isolated"] == true {
            return Err(Error::new(
                409,
                "This workspace is retained on a private VM disk. Resume the run to review and preserve its work.",
            ));
        }
        let workspaces = run["workspaces"].as_array().cloned().unwrap_or_else(|| {
            if run["workspace"].is_string() && run["snapshot"]["project"].is_object() {
                vec![json!({
                "projectId":run["snapshot"]["project"]["id"],"path":run["workspace"],"kind":"worktree"}
                )]
            } else {
                vec![]
            }
        });
        let managed = workspaces
            .iter()
            .filter(|w| w["kind"] != "direct")
            .collect::<Vec<_>>();
        if run["snapshot"]["task"]["worktree"] != true
            || !run["workspace"].is_string()
            || managed.is_empty()
        {
            return Err(Error::new(
                409,
                "This run has no managed worktree to clean up.",
            ));
        }
        let checkpoint = s
            .store
            .kv(&format!("run-checkpoint:{id}"))
            .await?
            .unwrap_or(Value::Null);
        let root = s.config.data_dir.join("runs").join(id).join(
            checkpoint["generation"]
                .as_str()
                .map(|g| format!("workspace-{g}"))
                .unwrap_or_else(|| "workspace".into()),
        );
        let env = std::env::vars().collect();
        for workspace in &managed {
            let target = Path::new(text(workspace, "path"));
            if !target.starts_with(&root)
                || crate::skills::workspace(target, std::slice::from_ref(&root)).await? != target
            {
                return Err(Error::new(
                    409,
                    "Workspace is outside this run’s managed directory.",
                ));
            }
            let args = vec![
                "-C".into(),
                target.to_string_lossy().into_owned(),
                "status".into(),
                "--porcelain".into(),
                "--ignored".into(),
            ];
            let output = crate::process::bounded_output(
                crate::process::command("git", &args, &env, None),
                Duration::from_secs(10),
                100000,
            )
            .await?;
            if !output.success || !output.stdout.trim().is_empty() {
                return Err(Error::new(
                    409,
                    "This worktree contains changes or untracked files. Commit or move them before cleanup.",
                ));
            }
        }
        for workspace in managed {
            if workspace["kind"] == "clone" {
                tokio::fs::remove_dir_all(text(workspace, "path")).await?;
                continue;
            }
            let project = run_projects(&run)
                .into_iter()
                .find(|p| p["id"] == workspace["projectId"])
                .ok_or_else(|| Error::bad("Project not found"))?;
            let args = vec![
                "-C".into(),
                text(&project, "path").into(),
                "worktree".into(),
                "remove".into(),
                text(workspace, "path").into(),
            ];
            let output = crate::process::bounded_output(
                crate::process::command("git", &args, &env, None),
                Duration::from_secs(10),
                100000,
            )
            .await?;
            if !output.success {
                return Err(Error::new(409, "Git could not remove this worktree."));
            }
        }
        s.store
            .patch_run(
                id,
                json!({
                "workspace":null,"resumeAvailable":false,"workspaceCleanedAt":now()}
                ),
            )
            .await?;
        s.store
            .audit(
                "run.workspace.cleaned",
                json!({
                "id":id}
                ),
            )
            .await?;
        Ok(json!({
        "cleaned":true}
        ))
    }
}
pub async fn routes(s: &Arc<Service>, input: &crate::http::Input) -> Option<Result<Value>> {
    if input.method != "POST" {
        return None;
    }
    let segments = input
        .path
        .trim_start_matches("/api/")
        .split('/')
        .collect::<Vec<_>>();
    let result = match segments.as_slice() {
        ["runs", id, "cancel"] => {
            async {
                s.worker.cancel(s, id).await?;
                Ok(json!({
                "ok":true}
                ))
            }
            .await
        }
        ["runs", id, "resume"] => s.worker.resume(s, id).await,
        ["runs", id, "cleanup"] => s.worker.cleanup(s, id).await,
        ["chats", id, "pause"] => {
            async {
                let paused = input.boolean("paused")?;
                let chat = s.get("chats", id).await?;
                crate::conversation_lifecycle::require_active(&chat)?;
                if !paused && let Some(id) = chat["runId"].as_str() {
                    let run = s.store.run(id).await?;
                    if ["failed", "interrupted", "cancelled"].contains(&text(&run, "status"))
                        && chat["cancelledByDeletion"] != true
                        && chat["sessionRestartRequested"] != true
                    {
                        s.worker.resume(s, id).await?;
                    }
                }
                s.store.delete(&format!("chat-error:{id}")).await?;
                s.chat_pause(id, paused).await
            }
            .await
        }
        ["chats", id, "stop"] => {
            async {
                let chat = s.get("chats", id).await?;
                s.chat_pause(id, true).await?;
                if let Some(id) = chat["runId"].as_str()
                    && ["queued", "running"].contains(&text(&s.store.run(id).await?, "status"))
                {
                    s.worker.cancel(s, id).await?;
                }
                Ok(json!({
                "stopped":true}
                ))
            }
            .await
        }
        _ => return None,
    };
    Some(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn committed_work_and_deployment_release_wake_the_scheduler() {
        let root = tempfile::TempDir::new().unwrap();
        std::fs::create_dir(root.path().join("home")).unwrap();
        let config = crate::config::Config {
            data_dir: root.path().join("data"),
            home: root.path().join("home"),
            workspace_roots: vec![root.path().to_owned()],
            public_url: "http://localhost:4310".into(),
            host: "127.0.0.1".into(),
            port: 0,
            setup_token: String::new(),
            codex_bin: "unused".into(),
            claude_bin: "claude".into(),
            gh_bin: "unused".into(),
            concurrency: 1,
            logger: false,
            worker_enabled: false,
            runner_url: String::new(),
        };
        let s = Service::new(config).await.unwrap();
        let chat = s
            .chat_create(json!({"agentId":crate::config::MAIN_AGENT_ID}))
            .await
            .unwrap();
        let chat_id = text(&chat, "id");
        s.chat_send(chat_id, json!({"id":crate::config::id(),"text":"Hello"}))
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_millis(100), s.worker.wake.notified())
            .await
            .unwrap();
        assert_eq!(
            s.chat_detail(chat_id).await.unwrap()["messages"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert!(s.chat_send(chat_id, json!({"text":""})).await.is_err());
        assert!(
            tokio::time::timeout(Duration::from_millis(10), s.worker.wake.notified())
                .await
                .is_err()
        );
        let task = s
            .task(
                json!({"name":"Wake test","prompt":"Hello","agentId":crate::config::MAIN_AGENT_ID}),
                None,
            )
            .await
            .unwrap();
        let run = s.enqueue(text(&task, "id"), "manual", None).await.unwrap();
        tokio::time::timeout(Duration::from_millis(100), s.worker.wake.notified())
            .await
            .unwrap();
        s.worker
            .deployment_lease(&s, "test".into(), false)
            .await
            .unwrap();
        s.worker.tick(&s).await.unwrap();
        assert!(s.worker.active.lock().await.is_empty());
        assert_eq!(
            s.store.run(text(&run, "id")).await.unwrap()["status"],
            "queued"
        );
        s.worker
            .deployment_lease(&s, "test".into(), true)
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_millis(100), s.worker.wake.notified())
            .await
            .unwrap();
        assert!(s.store.kv("deployment-lease").await.unwrap().is_none());
    }
    #[tokio::test]
    async fn scheduling_wakeups_survive_a_busy_worker_and_coalesce() {
        let worker = Worker::default();
        let busy = worker.tick_lock.lock().await;
        worker.notify();
        worker.notify();
        drop(busy);
        tokio::time::timeout(Duration::from_millis(100), worker.wake.notified())
            .await
            .unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(10), worker.wake.notified())
                .await
                .is_err()
        );
        worker.notify();
        tokio::time::timeout(Duration::from_millis(100), worker.wake.notified())
            .await
            .unwrap();
    }
}
