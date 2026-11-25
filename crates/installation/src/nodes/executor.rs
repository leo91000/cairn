//! Node-side attempt staging and scoped refresh/inbox lifetimes.
use super::workspace::{await_preparation, check_preparing, fetch, seed, seed_with_stop};
use crate::{
    error::{Error, Result},
    performance::Operation,
    validation::text,
};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, OnceLock},
    time::Duration,
};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

/// The master connection an attempt's node-side work runs under.
#[derive(Clone)]
pub struct Session {
    pub client: reqwest::Client,
    pub master: url::Url,
    pub token: String,
    pub attempt: String,
}

impl Session {
    async fn json(&self, operation: &str, body: &Value) -> Result<Value> {
        fetch(
            &self.client,
            &self.master,
            &self.token,
            &self.attempt,
            operation,
            body,
        )
        .await?
        .json()
        .await
        .map_err(Error::internal)
    }

    async fn seed(&self, selection: Value, target: &Path) -> Result<()> {
        seed(
            &self.client,
            &self.master,
            &self.token,
            &self.attempt,
            selection,
            target,
        )
        .await
    }

    async fn seed_preparation(
        &self,
        selection: Value,
        target: &Path,
        stop: &CancellationToken,
        publication: Option<&Mutex<()>>,
    ) -> Result<()> {
        seed_with_stop(
            &self.client,
            &self.master,
            &self.token,
            &self.attempt,
            selection,
            target,
            stop,
            publication,
        )
        .await
    }
}

fn plan_path(data: &Path, attempt: &str) -> PathBuf {
    data.join("runner-plans").join(format!("{attempt}.json"))
}

pub struct Executor {
    native: bool,
    data: PathBuf,
    attempts: Mutex<HashMap<String, CancellationToken>>,
    preparation: super::coordination::Coordination,
    pending: Mutex<HashMap<String, Arc<PendingPreparation>>>,
    attempt_gate: super::coordination::Coordination,
}

#[derive(Default)]
struct PendingPreparation {
    stop: CancellationToken,
    done: CancellationToken,
    publication: Mutex<()>,
    result: OnceLock<Result<()>>,
}

impl PendingPreparation {
    async fn cancel_and_drain(&self) {
        {
            let _publish = self.publication.lock().await;
            self.stop.cancel();
        }
        self.done.cancelled().await;
    }
}

impl Default for Executor {
    fn default() -> Self {
        Self {
            native: crate::native::enabled(),
            data: super::node_data_dir(),
            attempts: Mutex::default(),
            preparation: super::coordination::Coordination::default(),
            pending: Mutex::default(),
            attempt_gate: super::coordination::Coordination::default(),
        }
    }
}

impl Executor {
    pub(super) fn native(&self) -> bool {
        self.native
    }

    #[cfg(test)]
    pub(super) fn for_project_import(data: PathBuf) -> Self {
        Self {
            native: true,
            data,
            ..Self::default()
        }
    }

    pub async fn prepare(
        self: &Arc<Self>,
        client: &reqwest::Client,
        master: &url::Url,
        token: &str,
        attempt: &str,
    ) -> Result<()> {
        let session = Session {
            client: client.clone(),
            master: master.clone(),
            token: token.into(),
            attempt: attempt.into(),
        };
        if self.native {
            return self.prepare_native(session).await;
        }
        self.prepare_attempt(session, CancellationToken::new(), None)
            .await
    }

    async fn prepare_native(self: &Arc<Self>, session: Session) -> Result<()> {
        let attempt = session.attempt.clone();
        crate::validation::uuid(&attempt)?;
        let (worker, first) = {
            let _gate = self.attempt_gate.lock(&attempt).await;
            let state = crate::native::state_dir();
            if state.join(format!("{attempt}.stopped")).try_exists()? {
                return Err(Error::conflict("Native attempt has already stopped."));
            }
            if self.attempts.lock().await.contains_key(&attempt) {
                return Ok(());
            }
            let mut pending = self.pending.lock().await;
            if let Some(worker) = pending
                .get(&attempt)
                .filter(|worker| !worker.done.is_cancelled())
            {
                (worker.clone(), false)
            } else {
                let worker = Arc::new(PendingPreparation::default());
                pending.insert(attempt.clone(), worker.clone());
                (worker, true)
            }
        };
        if !first {
            worker.done.cancelled().await;
            return worker.result.get().cloned().unwrap_or_else(|| {
                Err(Error::unavailable(
                    "Native preparation worker did not publish a result.",
                ))
            });
        }

        let executor = self.clone();
        // A dropped relay waiter requests cancellation. Only the worker
        // releases staging ownership after its accepted I/O has drained.
        let _cancel = worker.stop.clone().drop_guard();
        tokio::spawn(async move {
            let _complete = worker.done.clone().drop_guard();
            let result = executor
                .prepare_attempt(session, worker.stop.clone(), Some(&worker.publication))
                .await;
            let _ = worker.result.set(result.clone());
            executor.pending.lock().await.remove(&attempt);
            result
        })
        .await
        .map_err(Error::internal)?
    }

    async fn prepare_attempt(
        &self,
        session: Session,
        stop: CancellationToken,
        publication: Option<&Mutex<()>>,
    ) -> Result<()> {
        let attempt = session.attempt.as_str();
        crate::validation::uuid(attempt)?;
        let mut timing = Operation::new("node_prepare", attempt, "plan");
        let data = self.data.clone();
        let descriptor = await_preparation(&stop, session.json("plan", &json!({}))).await?;
        let run = text(&descriptor["plan"], "runId");
        crate::validation::uuid(run)?;
        timing.next("queue");
        let _guard =
            await_preparation(&stop, async { Ok(self.preparation.lock(run).await) }).await?;
        if self.attempts.lock().await.contains_key(attempt) {
            timing.finish();
            return Ok(());
        }
        let _staging = if self.native {
            let state = crate::native::state_dir();
            let guard = crate::native::disk::lock_staging_after_release(&state, run, &stop).await?;
            for terminal in ["stopped", "exit"] {
                if state.join(format!("{attempt}.{terminal}")).try_exists()? {
                    return Err(Error::conflict("Native attempt has already stopped."));
                }
            }
            Some(guard)
        } else {
            None
        };
        if stop.is_cancelled() {
            return Err(Error::unavailable("Node workspace preparation stopped."));
        }
        timing.next("seed");
        let mut pin = crate::native::bases::PendingPin::new(&data.join("runs").join(run), attempt)?;
        let plan = stage_plan(&session, &data, descriptor, &stop, publication).await?;
        if stop.is_cancelled() {
            return Err(Error::unavailable("Node workspace preparation stopped."));
        }
        let root = data.join("runs").join(text(&plan, "runId"));
        timing.next("lease");
        grant_lease(&session, &data, &stop).await?;
        let home = root
            .join("home")
            .join(super::workspace::provider_home(&plan));
        crate::skills::private_dir(&home).await?;
        let active = CancellationToken::new();
        {
            let mut attempts = self.attempts.lock().await;
            check_preparing(&stop)?;
            attempts.insert(attempt.into(), active.clone());
        }
        let auth = super::workspace::auth_listener(
            home.join("cairn-auth.sock"),
            session.client.clone(),
            session.master.clone(),
            session.token.clone(),
            attempt.into(),
            active.clone(),
        );
        tokio::spawn(auth);
        let mcp = crate::mcps::channel::forward(
            root.join("home").join(crate::microvm::mcp::SOCKET),
            session.clone(),
            active.clone(),
        );
        tokio::spawn(mcp);
        tokio::spawn(refresh_inbox(session, root, active));
        pin.keep();
        timing.finish();
        Ok(())
    }

    async fn stop(&self, attempt: &str) -> Option<super::coordination::Operation> {
        let gate = if self.native {
            let gate = self.attempt_gate.lock(attempt).await;
            let worker = self.pending.lock().await.get(attempt).cloned();
            if let Some(worker) = worker {
                worker.cancel_and_drain().await;
                self.pending.lock().await.remove(attempt);
            }
            Some(gate)
        } else {
            None
        };
        if let Some(stop) = self.attempts.lock().await.remove(attempt) {
            stop.cancel();
        }
        let (data, attempt) = (self.data.clone(), attempt.to_owned());
        tokio::spawn(async move {
            if let Err(error) = crate::native::bases::release_attempt(&data, &attempt).await {
                tracing::debug!(
                    status = error.status,
                    "Cancelled native preparation pin cleanup deferred"
                );
            }
        });
        gate
    }

    pub async fn close(&self) -> Vec<String> {
        let pending: Vec<_> = self.pending.lock().await.values().cloned().collect();
        for worker in pending {
            worker.cancel_and_drain().await;
        }
        let mut attempts = self.attempts.lock().await;
        let ids = attempts.keys().cloned().collect();
        for (_, stop) in attempts.drain() {
            stop.cancel();
        }
        ids
    }

    pub async fn before(
        &self,
        client: &reqwest::Client,
        master: &url::Url,
        token: &str,
        method: &str,
        path: &str,
        body: &[u8],
    ) -> Result<Option<super::coordination::Operation>> {
        let parts = path.trim_start_matches('/').split('/').collect::<Vec<_>>();
        if let ["runs", attempt, "projects", project] = parts.as_slice() {
            let value: Value = serde_json::from_slice(body)?;
            let run = text(&value, "runId");
            crate::validation::uuid(run)?;
            let plan: Value =
                serde_json::from_slice(&tokio::fs::read(plan_path(&self.data, attempt)).await?)?;
            if plan["runId"] != run {
                return Err(Error::bad("Invalid project run."));
            }
            let manager = if self.native {
                PathBuf::from(text(&plan, "guestDataRoot"))
            } else {
                self.data.clone()
            };
            let expected = crate::execution::project_seed_path(
                &manager.join("runs").join(run),
                &plan,
                project,
            )?;
            let relative = expected.strip_prefix(&manager).map_err(Error::internal)?;
            let target = self.data.join(relative);
            if value["source"] != expected.to_string_lossy().as_ref() {
                return Err(Error::bad("Invalid project destination."));
            }
            let selection = json!({ "kind": "project", "projectId": project });
            seed(client, master, token, attempt, selection, &target).await?;
        }
        if method == "POST"
            && let ["runs", attempt, "environment"] = parts.as_slice()
        {
            let request: crate::background_jobs::protocol::Request = serde_json::from_slice(body)?;
            if matches!(
                request,
                crate::background_jobs::protocol::Request::ResumeTurn { .. }
            ) {
                let plan: Value = serde_json::from_slice(
                    &tokio::fs::read(plan_path(&self.data, attempt)).await?,
                )?;
                if plan["environmentProtocol"] != 1 {
                    return Err(Error::conflict(
                        "Environment does not support continuation.",
                    ));
                }
                let _guard = self.preparation.lock(text(&plan, "runId")).await;
                let home = self
                    .data
                    .join("runs")
                    .join(text(&plan, "runId"))
                    .join("home");
                seed(
                    client,
                    master,
                    token,
                    attempt,
                    json!({"kind":"runtime"}),
                    &home,
                )
                .await?;
                let descriptor = fetch(client, master, token, attempt, "plan", &json!({}))
                    .await?
                    .json::<Value>()
                    .await
                    .map_err(Error::internal)?;
                let credentials = descriptor["credentialFiles"].clone();
                if self.native {
                    let data = self.data.clone();
                    let attempt = (*attempt).to_owned();
                    let stop = CancellationToken::new();
                    let _cancel = stop.clone().drop_guard();
                    tokio::spawn(async move {
                        let _staging = await_preparation(
                            &stop,
                            super::workspace::lock_live_staging(&data, &home, &attempt, &stop),
                        )
                        .await?;
                        check_preparing(&stop)?;
                        refresh_credential_manifest(&home, &credentials).await
                    })
                    .await
                    .map_err(Error::internal)??;
                } else {
                    refresh_credential_manifest(&home, &credentials).await?;
                }
            }
        }
        if method == "DELETE"
            && let ["runs", attempt] = parts.as_slice()
        {
            return Ok(self.stop(attempt).await);
        }
        Ok(None)
    }

    pub async fn result(
        client: &reqwest::Client,
        master: &url::Url,
        token: &str,
        attempt: &str,
    ) -> Result<()> {
        let data = super::node_data_dir();
        let plan: Value =
            serde_json::from_slice(&tokio::fs::read(plan_path(&data, attempt)).await?)?;
        let path = data
            .join("runs")
            .join(text(&plan, "runId"))
            .join("output/result.md");
        if let Ok(result) = tokio::fs::read_to_string(path).await {
            let body = json!({ "result": result });
            fetch(client, master, token, attempt, "result", &body).await?;
        }
        Ok(())
    }
}

async fn refresh_credential_manifest(home: &Path, value: &Value) -> Result<()> {
    let files: Vec<String> = serde_json::from_value(value.clone())?;
    if files.len() > 4096 || files.iter().any(|path| !cairn_plugin_api::home_path(path)) {
        return Err(Error::bad("Invalid runtime credential manifest."));
    }
    let run = home
        .parent()
        .ok_or_else(|| Error::bad("Missing runtime directory."))?;
    crate::skills::atomic_write(
        &run.join("credential-files.json"),
        &serde_json::to_vec(&files)?,
    )
    .await
}

#[cfg(test)]
mod runtime_manifest_tests {
    use super::*;

    #[tokio::test]
    async fn runtime_refresh_publishes_new_credential_directories() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        crate::skills::private_dir(&home).await.unwrap();
        refresh_credential_manifest(&home, &json!([".config/first/token"]))
            .await
            .unwrap();
        refresh_credential_manifest(
            &home,
            &json!([".config/first/token", ".config/second/token"]),
        )
        .await
        .unwrap();
        assert_eq!(
            crate::execution::credential_files(root.path()).await,
            vec![".config/first/token", ".config/second/token"]
        );
        assert!(
            refresh_credential_manifest(&home, &json!(["../secret"]))
                .await
                .is_err()
        );
        assert_eq!(
            crate::execution::credential_files(root.path()).await.len(),
            2
        );
    }
}

/// Fetches the attempt's plan and initial files, and stores the plan for the local runner.
async fn stage_plan(
    session: &Session,
    data: &Path,
    descriptor: Value,
    stop: &CancellationToken,
    publication: Option<&Mutex<()>>,
) -> Result<Value> {
    let mut plan = descriptor["plan"].clone();
    if !crate::native::enabled() && descriptor["dataRoot"] != data.to_string_lossy().as_ref() {
        return Err(Error::conflict(
            "Master and node DATA_DIR must match for portable VM paths.",
        ));
    }
    crate::validation::uuid(text(&plan, "runId"))?;
    if plan["id"] != session.attempt.as_str() || plan.get("claudeState").is_some() {
        return Err(Error::conflict(
            "Remote execution requires managed provider authentication.",
        ));
    }
    if crate::native::enabled() {
        let manager = PathBuf::from(text(&descriptor, "dataRoot"));
        if !manager.is_absolute() {
            return Err(Error::bad("Invalid manager data root."));
        }
        plan["guestDataRoot"] = descriptor["dataRoot"].clone();
        for import in plan["imports"]
            .as_array_mut()
            .ok_or_else(|| Error::bad("Missing native imports."))?
        {
            let source = PathBuf::from(text(import, "source"));
            let relative = source
                .strip_prefix(&manager)
                .map_err(|_| Error::bad("Native import outside manager storage."))?;
            import["source"] = data.join(relative).to_string_lossy().into_owned().into();
        }
    }
    let root = data.join("runs").join(text(&plan, "runId"));
    let archives: Option<Vec<crate::project_archives::Archive>> = if crate::native::enabled() {
        descriptor
            .get("projectArchives")
            .cloned()
            .map(serde_json::from_value)
            .transpose()?
    } else {
        None
    };
    let mut seed_workspace =
        (crate::native::enabled() || plan["resumeWorkspace"] != true) && archives.is_none();
    let mut timing = super::workspace::seed_observation::Seed::new(
        crate::native::enabled(),
        "node_stage_plan",
        &session.attempt,
        "initial_seed",
        std::time::Instant::now(),
    );
    session
        .seed_preparation(
            json!({ "kind": "initial", "workspace": seed_workspace, "projectArchives": archives.is_some(),
                "keepArchives": archives.as_ref().map(|archives| archives.iter().map(|archive| &archive.hash).collect::<Vec<_>>()) }),
            &root,
            stop,
            publication,
        )
        .await?;
    timing.next("project_archives");
    plan["workspaceSeeded"] = seed_workspace.into();
    if let Some(archives) = archives {
        let base_size = plan["resources"]["diskMiB"].as_u64();
        match crate::project_archives::stage(session, &root, &archives, base_size, stop).await {
            Ok(base) => {
                if let Some(key) = base {
                    plan["nativeProjectBase"] = key.into();
                }
                plan["nativeProjectArchives"] = serde_json::to_value(archives)?;
            }
            Err(error) => {
                fallback_seed(session, &root, error, stop, publication).await?;
                seed_workspace = true;
                plan["workspaceSeeded"] = true.into();
            }
        }
    }
    timing.next("publish_plan");
    let publish = match publication {
        Some(fence) => Some(fence.lock().await),
        None => None,
    };
    check_preparing(stop)?;
    if !seed_workspace {
        resume_import_directories(&plan, &root).await?;
    }
    crate::skills::private_dir(&data.join("runner-plans")).await?;
    // A disconnected connector cannot leave a newly prepared VM runnable forever.
    plan["nodeLeaseRequired"] = true.into();

    check_preparing(stop)?;
    crate::skills::atomic_write(
        &plan_path(data, &session.attempt),
        &serde_json::to_vec(&plan)?,
    )
    .await?;
    drop(publish);
    timing.finish();
    Ok(plan)
}

async fn fallback_seed(
    session: &Session,
    root: &Path,
    error: Error,
    stop: &CancellationToken,
    publication: Option<&Mutex<()>>,
) -> Result<()> {
    check_preparing(stop)?;
    if matches!(error.status, 401 | 403) {
        return Err(error);
    }
    tracing::warn!(
        status = error.status,
        "Archive staging unavailable; requesting the complete seed"
    );
    crate::project_archives::abandon_stage(root, error.status == 507, &session.attempt).await?;
    session
        .seed_preparation(
            json!({ "kind": "initial", "workspace": true }),
            root,
            stop,
            publication,
        )
        .await
}

/// Keep mount-source validation intact without copying the projects held by the
/// guest disk. These empty host directories must never initialize a fresh guest.
async fn resume_import_directories(plan: &Value, root: &Path) -> Result<()> {
    let Some(project_root) = plan["projectRoot"].as_str().map(Path::new) else {
        return Ok(());
    };
    for import in plan["imports"].as_array().into_iter().flatten() {
        // Native sources have already been remapped to node storage; targets
        // and projectRoot still use the execution's paths.
        let target = Path::new(text(import, "target"));
        if !target.starts_with(project_root) {
            continue;
        }
        let source = Path::new(text(import, "source"));
        let relative = source.strip_prefix(root).map_err(Error::internal)?;
        let mut directory = root.to_owned();
        for part in relative.components() {
            if !matches!(part, std::path::Component::Normal(_)) {
                return Err(Error::bad("Invalid workspace import directory."));
            }
            directory.push(part);
            match tokio::fs::symlink_metadata(&directory).await {
                Ok(metadata) if metadata.is_dir() => {}
                Ok(_) => return Err(Error::bad("Workspace import is not a directory.")),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    crate::skills::private_dir(&directory).await?;
                }
                Err(error) => return Err(error.into()),
            }
        }
    }
    Ok(())
}

/// Hands the master's first lease to the local controller, minus the time already spent.
async fn grant_lease(session: &Session, data: &Path, stop: &CancellationToken) -> Result<()> {
    let started = super::boot_ms();
    let grant = await_preparation(stop, session.json("lease", &json!({}))).await?;
    let remaining = grant["remainingMs"]
        .as_u64()
        .unwrap_or(0)
        .saturating_sub(super::boot_ms().saturating_sub(started));
    let controller =
        std::env::var("RUNNER_URL").map_err(|_| Error::bad("Missing local runner."))?;
    let credential = crate::execution::secret(data, "runner-secret").await?;
    check_preparing(stop)?;
    let lease = super::local_client()?
        .post(format!(
            "{}/runs/{}/lease",
            controller.trim_end_matches('/'),
            session.attempt
        ))
        .bearer_auth(credential)
        .json(&json!({ "remainingMs": remaining }))
        .timeout(Duration::from_secs(3))
        .send()
        .await
        .map_err(Error::internal)?;
    if !lease.status().is_success() {
        return Err(Error::conflict(
            "Local controller rejected execution lease.",
        ));
    }
    Ok(())
}

/// Copies new chat input into the staged workspace until the attempt stops.
async fn refresh_inbox(session: Session, root: PathBuf, stop: CancellationToken) {
    let mut previous = Value::Null;
    loop {
        let operation = async {
            let value = session.json("inbox", &json!({})).await?;
            if value != previous {
                session
                    .seed(json!({ "kind": "inbox" }), &root.join("chat-input"))
                    .await?;
                previous = value;
            }
            Ok::<_, Error>(())
        };
        tokio::select! {
            () = stop.cancelled() => break,
            _ = tokio::time::timeout(Duration::from_secs(10), operation) => {}
        }
        tokio::select! {
            () = stop.cancelled() => break,
            () = tokio::time::sleep(Duration::from_secs(1)) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Json, Router, body::Body, routing::post};

    #[test]
    fn native_relay_stop_drains_a_pending_seed() {
        let fixture = tempfile::tempdir().unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "nodes::executor::tests::native_relay_stop_child",
                "--ignored",
                "--nocapture",
            ])
            .env("CAIRN_NODE_BACKEND", "native")
            .env("DATA_DIR", fixture.path().join("data"))
            .env("RUNNER_STATE_DIR", fixture.path().join("state"))
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[tokio::test]
    #[ignore = "isolated native node environment; run by its parent"]
    async fn native_relay_stop_child() {
        use futures_util::StreamExt;

        let data = crate::nodes::node_data_dir();
        let state = crate::native::state_dir();
        crate::skills::private_dir(&data).await.unwrap();
        crate::skills::private_dir(&state).await.unwrap();
        std::fs::write(
            state.join("storage-policy.json"),
            br#"{"reserveMiB":64,"reservePercent":1}"#,
        )
        .unwrap();
        std::fs::write(state.join(crate::microvm::budget::FILE), br#"{"nativeModeProtocol":1,"slots":1,"limits":{"cpu":1,"memoryMiB":1024,"diskMiB":1024}}"#).unwrap();
        let budget = Router::new().route(
            "/staging-budget",
            axum::routing::get(|| async {
                Json(json!({ "availableBytes": 16 * 1024 * 1024, "usedBytes": 0 }))
            }),
        );
        let socket = tokio::net::UnixListener::bind(state.join("controller.sock")).unwrap();
        let budget_server = tokio::spawn(async move { axum::serve(socket, budget).await.unwrap() });

        let (run, attempt) = (crate::config::id(), crate::config::id());
        let descriptor = json!({ "dataRoot": "/guest", "plan": { "id": attempt, "runId": run, "backend": "native", "imports": [] } });
        let plan_requested = Arc::new(tokio::sync::Notify::new());
        let observed_plan = plan_requested.clone();
        let seed = Router::new()
            .route("/internal/node-workspace/{attempt}/plan", post(move || {
                let descriptor = descriptor.clone();
                observed_plan.notify_one();
                async move { Json(descriptor) }
            }))
            .route("/internal/node-workspace/{attempt}/seed", post(|| async {
                let first = axum::body::Bytes::from_static(b"{\"kind\":\"file\",\"path\":\"partial\",\"size\":3,\"mode\":384}\n{\"data\":\"YWJj\"}\n");
                let stream = futures_util::stream::once(async move { Ok::<_, std::io::Error>(first) })
                    .chain(futures_util::stream::pending());
                let mut response = axum::response::Response::new(Body::from_stream(stream));
                response.headers_mut().insert("x-cairn-seed-allocation", axum::http::HeaderValue::from_static("1048576"));
                response
            }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let master: url::Url = format!("http://{}", listener.local_addr().unwrap())
            .parse()
            .unwrap();
        let manager = tokio::spawn(async move { axum::serve(listener, seed).await.unwrap() });
        let executor = Arc::new(Executor::default());
        let client = reqwest::Client::new();
        let finishing_writer = crate::native::disk::lock_staging(&state, &run)
            .await
            .unwrap();
        let mut preparing = {
            let (executor, client, master, attempt) = (
                executor.clone(),
                client.clone(),
                master.clone(),
                attempt.clone(),
            );
            tokio::spawn(async move {
                executor
                    .prepare(&client, &master, "fixture", &attempt)
                    .await
            })
        };
        tokio::time::timeout(Duration::from_secs(5), plan_requested.notified())
            .await
            .unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(50), &mut preparing)
                .await
                .is_err(),
            "the new attempt must wait while the old writer owns staging"
        );
        drop(finishing_writer);
        let staged = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Ok(entries) = std::fs::read_dir(data.join("runs")) {
                    for entry in entries.flatten() {
                        let partial = entry.path().join("partial");
                        if std::fs::read(&partial).is_ok_and(|bytes| bytes == b"abc") {
                            return entry.path();
                        }
                    }
                }
                assert!(
                    !preparing.is_finished(),
                    "seed returned before the held-open transfer"
                );
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(
            std::fs::read_dir(state.join("native-preparations"))
                .unwrap()
                .count(),
            2
        );
        let delete = format!("/runs/{attempt}");
        let fence = tokio::time::timeout(
            Duration::from_secs(5),
            executor.before(&client, &master, "fixture", "DELETE", &delete, b"{}"),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(fence.is_some());
        assert!(preparing.await.unwrap().is_err());
        assert!(!staged.exists());
        assert!(!data.join("runs").join(&run).exists());
        assert!(!plan_path(&data, &attempt).exists());
        assert_eq!(
            std::fs::read_dir(state.join("native-preparations"))
                .unwrap()
                .count(),
            0
        );
        let released = crate::native::disk::lock_staging(&state, &run)
            .await
            .unwrap();

        // A racing prepare cannot enter before the forwarded DELETE leaves
        // the local controller stopped receipt.
        assert!(
            tokio::time::timeout(
                Duration::from_millis(30),
                executor.prepare(&client, &master, "fixture", &attempt)
            )
            .await
            .is_err()
        );
        std::fs::write(state.join(format!("{attempt}.stopped")), b"1").unwrap();
        drop(released);
        drop(fence);
        assert!(
            executor
                .prepare(&client, &master, "fixture", &attempt)
                .await
                .unwrap_err()
                .is_conflict()
        );
        late_live_seed_cannot_recreate_retired_staging(&data, &state, &client).await;
        manager.abort();
        budget_server.abort();
    }

    async fn late_live_seed_cannot_recreate_retired_staging(
        data: &Path,
        state: &Path,
        client: &reqwest::Client,
    ) {
        let (run, attempt) = (crate::config::id(), crate::config::id());
        let plans = data.join("runner-plans");
        crate::skills::private_dir(&plans).await.unwrap();
        let plan = plan_path(data, &attempt);
        tokio::fs::write(&plan, json!({ "id": attempt, "runId": run }).to_string())
            .await
            .unwrap();

        let requested = Arc::new(tokio::sync::Notify::new());
        let reply = Arc::new(tokio::sync::Notify::new());
        let route = {
            let (requested, reply) = (requested.clone(), reply.clone());
            Router::new().route(
                "/internal/node-workspace/{attempt}/seed",
                post(move || {
                    let (requested, reply) = (requested.clone(), reply.clone());
                    async move {
                        requested.notify_one();
                        reply.notified().await;
                        Body::from("{\"complete\":true}\n")
                    }
                }),
            )
        };
        let server = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin: url::Url = format!("http://{}", server.local_addr().unwrap())
            .parse()
            .unwrap();
        let server = tokio::spawn(async move { axum::serve(server, route).await.unwrap() });
        let target = data.join("runs").join(&run).join("chat-input");
        let seed = seed(
            client,
            &origin,
            "fixture",
            &attempt,
            json!({ "kind": "inbox" }),
            &target,
        );
        tokio::pin!(seed);
        tokio::select! {
            request = tokio::time::timeout(Duration::from_secs(5), requested.notified()) => { request.unwrap(); },
            result = &mut seed => panic!("Seed finished before the authorized response: {result:?}"),
        }
        {
            let _retirement = crate::native::disk::lock_staging(state, &run)
                .await
                .unwrap();
            reply.notify_one();
            assert!(
                tokio::time::timeout(Duration::from_millis(100), &mut seed)
                    .await
                    .is_err()
            );
            tokio::fs::remove_file(plan).await.unwrap();
        }
        let error = tokio::time::timeout(Duration::from_secs(5), seed)
            .await
            .unwrap()
            .unwrap_err();
        assert!(error.is_conflict());
        assert!(!target.parent().unwrap().exists());
        server.abort();
    }

    #[test]
    fn native_archive_resume_stages_generated_imports_for_controller_validation() {
        let fixture = tempfile::tempdir().unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "nodes::executor::tests::native_archive_resume_child",
                "--ignored",
                "--nocapture",
            ])
            .env("CAIRN_NODE_BACKEND", "native")
            .env("DATA_DIR", fixture.path().join("data"))
            .env("RUNNER_STATE_DIR", fixture.path().join("state"))
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[tokio::test]
    #[ignore = "isolated native node environment; run by its parent"]
    async fn native_archive_resume_child() {
        let data = crate::nodes::node_data_dir();
        let state = crate::native::state_dir();
        crate::skills::private_dir(&data).await.unwrap();
        crate::skills::private_dir(&state).await.unwrap();
        tokio::fs::write(
            state.join("storage-policy.json"),
            serde_json::to_vec(&json!({
                "reserveMiB": 64, "reservePercent": 1,
            }))
            .unwrap(),
        )
        .await
        .unwrap();
        tokio::fs::write(
            state.join(crate::microvm::budget::FILE),
            serde_json::to_vec(&json!({
                "nativeModeProtocol": 1, "slots": 1,
                "limits": {"cpu": 1, "memoryMiB": 1024, "diskMiB": 1024},
            }))
            .unwrap(),
        )
        .await
        .unwrap();
        let budget = Router::new().route(
            "/staging-budget",
            axum::routing::get(|| async { Json(json!({ "availableBytes": 1_000_000 })) }),
        );
        let socket = tokio::net::UnixListener::bind(state.join("controller.sock")).unwrap();
        let budget_server = tokio::spawn(async move { axum::serve(socket, budget).await.unwrap() });

        let source = tempfile::tempdir().unwrap();
        tokio::fs::create_dir(source.path().join("home"))
            .await
            .unwrap();
        tokio::fs::write(source.path().join("home/config"), b"fresh configuration")
            .await
            .unwrap();
        let mut frames = Vec::new();
        crate::nodes::files::send(source.path(), &mut frames)
            .await
            .unwrap();
        let seed = Router::new().route(
            "/internal/node-workspace/{attempt}/seed",
            post(move |Json(selection): Json<Value>| {
                let frames = frames.clone();
                async move {
                    assert_eq!(selection["workspace"], false);
                    assert_eq!(selection["projectArchives"], true);
                    Body::from(frames)
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let master: url::Url = format!("http://{}", listener.local_addr().unwrap())
            .parse()
            .unwrap();
        let seed_server = tokio::spawn(async move { axum::serve(listener, seed).await.unwrap() });

        for workspace in ["workspace", "workspace-fdeab5b6"] {
            let (run, attempt, project) = (
                crate::config::id(),
                crate::config::id(),
                crate::config::id(),
            );
            let retained = state.join("native").join(&run);
            crate::skills::private_dir(&retained).await.unwrap();
            tokio::fs::write(retained.join("block-initialized"), b"1")
                .await
                .unwrap();
            tokio::fs::write(retained.join("preserved"), b"existing workspace")
                .await
                .unwrap();
            let guest = Path::new("/data").join("runs").join(&run).join(workspace);
            let descriptor = json!({
                "dataRoot": "/data",
                "projectArchives": [{
                    "projectId": project,
                    "revision": "a".repeat(40),
                    "key": "b".repeat(64),
                    "hash": "c".repeat(64),
                    "bytes": 100,
                    "logicalBytes": 15
                }],
                "plan": {
                    "id": attempt,
                    "runId": run,
                    "backend": "native",
                    "expires": null,
                    "cwd": guest.join(&project),
                    "projectRoot": guest,
                    "resumeWorkspace": true,
                    "imports": [
                        { "source": guest, "target": guest },
                        { "source": guest.join(&project), "target": guest.join(&project) }
                    ]
                }
            });
            let session = Session {
                client: reqwest::Client::new(),
                master: master.clone(),
                token: "fixture".into(),
                attempt: attempt.clone(),
            };
            let staged = stage_plan(&session, &data, descriptor, &CancellationToken::new(), None)
                .await
                .unwrap();
            assert_eq!(staged["workspaceSeeded"], false);
            assert!(staged["nativeProjectArchives"].is_array());
            crate::native::validate_staged_plan(&data, &attempt)
                .await
                .unwrap();
            let root = data.join("runs").join(&run);
            assert_eq!(
                tokio::fs::read(root.join("home/config")).await.unwrap(),
                b"fresh configuration"
            );
            assert_eq!(
                tokio::fs::read(retained.join("preserved")).await.unwrap(),
                b"existing workspace"
            );
            assert!(
                tokio::fs::read_dir(root.join(workspace).join(&project))
                    .await
                    .unwrap()
                    .next_entry()
                    .await
                    .unwrap()
                    .is_none()
            );
        }
        seed_server.abort();
        budget_server.abort();
    }

    #[tokio::test]
    async fn failed_archive_staging_requests_full_seed_files_but_never_bypasses_revocation() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        tokio::fs::create_dir_all(source.join("workspace/project"))
            .await
            .unwrap();
        tokio::fs::write(source.join("workspace/project/file"), "fresh project")
            .await
            .unwrap();
        let mut frames = Vec::new();
        crate::nodes::files::send(&source, &mut frames)
            .await
            .unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let observed = calls.clone();
        let app = Router::new().route(
            "/internal/node-workspace/{attempt}/seed",
            post(move |Json(selection): Json<Value>| {
                let (frames, observed) = (frames.clone(), observed.clone());
                async move {
                    assert_eq!(selection["workspace"], true);
                    observed.fetch_add(1, Ordering::SeqCst);
                    Body::from(frames)
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let master = format!("http://{}", listener.local_addr().unwrap())
            .parse()
            .unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let session = Session {
            client: reqwest::Client::new(),
            master,
            token: "fixture".into(),
            attempt: crate::config::id(),
        };
        let target = root.path().join("run");
        tokio::fs::create_dir_all(target.join("project-archives"))
            .await
            .unwrap();
        tokio::fs::write(target.join("project-archives/partial.zip"), "partial")
            .await
            .unwrap();
        assert!(
            fallback_seed(
                &session,
                &target,
                Error::forbidden("revoked"),
                &CancellationToken::new(),
                None,
            )
            .await
            .is_err()
        );
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        fallback_seed(
            &session,
            &target,
            Error::bad("archive checksum mismatch"),
            &CancellationToken::new(),
            None,
        )
        .await
        .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            tokio::fs::read_to_string(target.join("workspace/project/file"))
                .await
                .unwrap(),
            "fresh project"
        );
        assert!(!target.join("project-archives").exists());
        server.abort();
    }

    #[tokio::test]
    async fn resume_import_directories_reject_symlinks_and_parent_traversal() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        for remapped in [false, true] {
            let project = root.path().join(if remapped {
                "workspace-generation"
            } else {
                "workspace"
            });
            let guest = if remapped {
                PathBuf::from("/data/runs/fixture/workspace-generation")
            } else {
                project.clone()
            };
            tokio::fs::create_dir(&project).await.unwrap();
            tokio::fs::symlink(outside.path(), project.join("link"))
                .await
                .unwrap();
            for source in [project.join("link/child"), project.join("../escape")] {
                let plan = json!({
                    "projectRoot": guest,
                    "imports": [{ "source": source, "target": guest.join("child") }]
                });
                let error = resume_import_directories(&plan, root.path())
                    .await
                    .unwrap_err();
                assert_eq!(error.status, 400);
                assert!(!outside.path().join("child").exists());
                assert!(!root.path().join("escape").exists());
            }
            let source = project.join("safe/child");
            let plan = json!({
                "projectRoot": guest,
                "imports": [{ "source": source, "target": guest.join("safe/child") }]
            });
            resume_import_directories(&plan, root.path()).await.unwrap();
            assert_eq!(tokio::fs::canonicalize(&source).await.unwrap(), source);
        }
    }

    #[tokio::test]
    async fn resume_import_directories_selects_guest_workspace_without_leaving_staging() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let guest = Path::new("/data/runs/fixture/workspace-generation");
        let source = outside.path().join("unrelated/child");
        let plan = json!({
            "projectRoot": guest,
            "imports": [{ "source": source, "target": guest.join("project") }]
        });
        assert!(resume_import_directories(&plan, root.path()).await.is_err());
        assert!(!source.exists());

        let source = root.path().join("workspace-generation/home");
        let plan = json!({
            "projectRoot": guest,
            "imports": [{ "source": source, "target": "/home/node" }]
        });
        resume_import_directories(&plan, root.path()).await.unwrap();
        assert!(!source.exists());
    }

    #[tokio::test]
    async fn oversized_change_detection_replaces_stale_input_for_controller_validation() {
        let source = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("chat-input");
        tokio::fs::create_dir(&target).await.unwrap();
        tokio::fs::write(target.join("messages.json"), b"[]")
            .await
            .unwrap();
        let length = crate::chat_inbox::TRANSPORT_LIMIT + 1;
        tokio::fs::write(source.path().join("messages.json"), vec![b' '; length])
            .await
            .unwrap();
        let path = source.path().to_owned();
        let app = Router::new()
            .route(
                "/internal/node-workspace/attempt/inbox",
                post(|| async { Json(json!({ "oversized": true })) }),
            )
            .route(
                "/internal/node-workspace/attempt/seed",
                post(move || {
                    let path = path.clone();
                    async move {
                        let (mut writer, reader) = tokio::io::duplex(131072);
                        tokio::spawn(async move {
                            super::super::files::send(&path, &mut writer).await.unwrap();
                        });
                        Body::from_stream(tokio_util::io::ReaderStream::new(reader))
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let master = format!("http://{}/", listener.local_addr().unwrap())
            .parse()
            .unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let stop = CancellationToken::new();
        let refresh = tokio::spawn(refresh_inbox(
            Session {
                client: reqwest::Client::new(),
                master,
                token: "test-token".into(),
                attempt: "attempt".into(),
            },
            root.path().to_owned(),
            stop.clone(),
        ));
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if tokio::fs::metadata(target.join("messages.json"))
                    .await
                    .is_ok_and(|metadata| metadata.len() == length as u64)
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let error = crate::chat_inbox::read(&target, crate::chat_inbox::TRANSPORT_LIMIT)
            .await
            .unwrap_err();
        assert_eq!(error.message, "Chat inbox is too large.");
        stop.cancel();
        refresh.await.unwrap();
        server.abort();
    }
}
