//! Node-side attempt staging and scoped refresh/inbox lifetimes.
use super::workspace::{fetch, seed};
use crate::{
    error::{Error, Result},
    validation::text,
};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
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
}

fn plan_path(data: &Path, attempt: &str) -> PathBuf {
    data.join("runner-plans").join(format!("{attempt}.json"))
}

#[derive(Default)]
pub struct Executor {
    attempts: Mutex<HashMap<String, CancellationToken>>,
    preparation: Mutex<()>,
}

impl Executor {
    pub async fn prepare(
        self: &Arc<Self>,
        client: &reqwest::Client,
        master: &url::Url,
        token: &str,
        attempt: &str,
    ) -> Result<()> {
        let _guard = self.preparation.lock().await;
        if self.attempts.lock().await.contains_key(attempt) {
            return Ok(());
        }
        let session = Session {
            client: client.clone(),
            master: master.clone(),
            token: token.into(),
            attempt: attempt.into(),
        };
        let data = super::node_data_dir();
        let plan = stage_plan(&session, &data).await?;
        let root = data.join("runs").join(text(&plan, "runId"));
        grant_lease(&session, &data).await?;
        let stop = CancellationToken::new();
        self.attempts
            .lock()
            .await
            .insert(attempt.into(), stop.clone());
        let home = root
            .join("home")
            .join(super::workspace::provider_home(&plan));
        crate::skills::private_dir(&home).await?;
        let auth = super::workspace::auth_listener(
            home.join("cairn-auth.sock"),
            client.clone(),
            master.clone(),
            token.into(),
            attempt.into(),
            stop.clone(),
        );
        tokio::spawn(auth);
        let mcp = crate::mcps::channel::forward(
            root.join("home").join(crate::microvm::mcp::SOCKET),
            session.clone(),
            stop.clone(),
        );
        tokio::spawn(mcp);
        tokio::spawn(refresh_inbox(session, root, stop));
        Ok(())
    }

    pub async fn stop(&self, attempt: &str) {
        if let Some(stop) = self.attempts.lock().await.remove(attempt) {
            stop.cancel();
        }
    }

    pub async fn close(&self) -> Vec<String> {
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
    ) -> Result<()> {
        let parts = path.trim_start_matches('/').split('/').collect::<Vec<_>>();
        if let ["runs", attempt, "projects", project] = parts.as_slice() {
            let value: Value = serde_json::from_slice(body)?;
            let run = text(&value, "runId");
            crate::validation::uuid(run)?;
            let target = super::node_data_dir()
                .join("runs")
                .join(run)
                .join("workspace")
                .join(project);
            if value["source"] != target.to_string_lossy().as_ref() {
                return Err(Error::bad("Invalid project destination."));
            }
            let selection = json!({ "kind": "project", "projectId": project });
            seed(client, master, token, attempt, selection, &target).await?;
        }
        if method == "DELETE"
            && let ["runs", attempt] = parts.as_slice()
        {
            self.stop(attempt).await;
        }
        Ok(())
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

/// Fetches the attempt's plan and initial files, and stores the plan for the local runner.
async fn stage_plan(session: &Session, data: &Path) -> Result<Value> {
    let descriptor = session.json("plan", &json!({})).await?;
    let mut plan = descriptor["plan"].clone();
    if descriptor["dataRoot"] != data.to_string_lossy().as_ref() {
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
    let root = data.join("runs").join(text(&plan, "runId"));
    session.seed(json!({ "kind": "initial" }), &root).await?;
    crate::skills::private_dir(&data.join("runner-plans")).await?;
    // A disconnected connector cannot leave a newly prepared VM runnable forever.
    plan["nodeLeaseRequired"] = true.into();

    // The manager's own origin can be loopback. Disk reads use the address this
    // node authenticated against, including a private LAN or VPN route.
    if plan["storage"].is_object() {
        plan["storage"]["master"] = session.master.as_str().into();
    }

    crate::skills::atomic_write(
        &plan_path(data, &session.attempt),
        &serde_json::to_vec(&plan)?,
    )
    .await?;
    Ok(plan)
}

/// Hands the master's first lease to the local controller, minus the time already spent.
async fn grant_lease(session: &Session, data: &Path) -> Result<()> {
    let started = super::boot_ms();
    let grant = session.json("lease", &json!({})).await?;
    let remaining = grant["remainingMs"]
        .as_u64()
        .unwrap_or(0)
        .saturating_sub(super::boot_ms().saturating_sub(started));
    let controller =
        std::env::var("RUNNER_URL").map_err(|_| Error::bad("Missing local runner."))?;
    let credential = crate::execution::secret(data, "runner-secret").await?;
    let lease = session
        .client
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
