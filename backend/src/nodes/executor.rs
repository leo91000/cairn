//! Node-side attempt staging and scoped refresh/inbox lifetimes.
use crate::{
    error::{Error, Result},
    validation::text,
};
use serde_json::{Value, json};
use std::{collections::HashMap, path::PathBuf, sync::Arc, time::Duration};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
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
        let descriptor: Value =
            super::workspace::fetch(client, master, token, attempt, "plan", &json!({}))
                .await?
                .json()
                .await
                .map_err(Error::internal)?;
        let mut plan = descriptor["plan"].clone();
        let data = PathBuf::from(std::env::var("DATA_DIR").unwrap_or_else(|_| "/data".into()));
        if descriptor["dataRoot"] != data.to_string_lossy().as_ref() {
            return Err(Error::new(
                409,
                "Master and node DATA_DIR must match for portable VM paths.",
            ));
        }
        crate::validation::uuid(text(&plan, "runId"))?;
        if plan["id"] != attempt || plan.get("claudeState").is_some() {
            return Err(Error::new(
                409,
                "Remote execution requires managed provider authentication.",
            ));
        }
        let root = data.join("runs").join(text(&plan, "runId"));
        super::workspace::seed(
            client,
            master,
            token,
            attempt,
            json!({"kind":"initial"}),
            &root,
        )
        .await?;
        crate::skills::private_dir(&data.join("runner-plans")).await?;
        // A disconnected connector cannot leave a newly prepared VM runnable forever.
        plan["nodeLeaseRequired"] = true.into();
        crate::skills::atomic_write(
            &data.join("runner-plans").join(format!("{attempt}.json")),
            &serde_json::to_vec(&plan)?,
        )
        .await?;
        let started = super::boot_ms();
        let grant: Value =
            super::workspace::fetch(client, master, token, attempt, "lease", &json!({}))
                .await?
                .json()
                .await
                .map_err(Error::internal)?;
        let remaining = grant["remainingMs"]
            .as_u64()
            .unwrap_or(0)
            .saturating_sub(super::boot_ms().saturating_sub(started));
        let controller =
            std::env::var("RUNNER_URL").map_err(|_| Error::bad("Missing local runner."))?;
        let credential = crate::execution::secret(&data, "runner-secret").await?;
        let lease = client
            .post(format!(
                "{}/runs/{attempt}/lease",
                controller.trim_end_matches('/')
            ))
            .bearer_auth(credential)
            .json(&json!({"remainingMs":remaining}))
            .timeout(Duration::from_secs(3))
            .send()
            .await
            .map_err(Error::internal)?;
        if !lease.status().is_success() {
            return Err(Error::new(
                409,
                "Local controller rejected execution lease.",
            ));
        }
        let stop = CancellationToken::new();
        self.attempts
            .lock()
            .await
            .insert(attempt.into(), stop.clone());
        let provider = if plan["chat"]["claudeManagedAuth"] == true {
            ".claude"
        } else {
            ".codex"
        };
        let home = root.join("home").join(provider);
        crate::skills::private_dir(&home).await?;
        let auth = super::workspace::auth_listener(
            home.join("leo-auth.sock"),
            client.clone(),
            master.clone(),
            token.into(),
            attempt.into(),
            stop.clone(),
            provider == ".claude",
        );
        tokio::spawn(auth);
        let (client, master, token, attempt) = (
            client.clone(),
            master.clone(),
            token.to_owned(),
            attempt.to_owned(),
        );
        tokio::spawn(async move {
            let mut previous = Value::Null;
            loop {
                let operation = async {
                    let value: Value = super::workspace::fetch(
                        &client,
                        &master,
                        &token,
                        &attempt,
                        "inbox",
                        &json!({}),
                    )
                    .await?
                    .json()
                    .await
                    .map_err(Error::internal)?;
                    if value != previous {
                        super::workspace::seed(
                            &client,
                            &master,
                            &token,
                            &attempt,
                            json!({"kind":"inbox"}),
                            &root.join("chat-input"),
                        )
                        .await?;
                        previous = value;
                    }
                    Ok::<_, Error>(())
                };
                tokio::select! {_=stop.cancelled()=>break,_=tokio::time::timeout(Duration::from_secs(10),operation)=>{}}
                tokio::select! {_=stop.cancelled()=>break,_=tokio::time::sleep(Duration::from_secs(1))=>{}}
            }
        });
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
            let data = PathBuf::from(std::env::var("DATA_DIR").unwrap_or_else(|_| "/data".into()));
            crate::validation::uuid(text(&value, "runId"))?;
            let target = data
                .join("runs")
                .join(text(&value, "runId"))
                .join("workspace")
                .join(project);
            if value["source"] != target.to_string_lossy().as_ref() {
                return Err(Error::bad("Invalid project destination."));
            }
            super::workspace::seed(
                client,
                master,
                token,
                attempt,
                json!({"kind":"project","projectId":project}),
                &target,
            )
            .await?;
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
        let data = PathBuf::from(std::env::var("DATA_DIR").unwrap_or_else(|_| "/data".into()));
        let plan: Value = serde_json::from_slice(
            &tokio::fs::read(data.join("runner-plans").join(format!("{attempt}.json"))).await?,
        )?;
        let path = data
            .join("runs")
            .join(text(&plan, "runId"))
            .join("output/result.md");
        if let Ok(result) = tokio::fs::read_to_string(path).await {
            super::workspace::fetch(
                client,
                master,
                token,
                attempt,
                "result",
                &json!({"result":result}),
            )
            .await?;
        }
        Ok(())
    }
}
