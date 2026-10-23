//! Node-side attempt staging and scoped refresh/inbox lifetimes.
use super::workspace::{fetch, seed};
use crate::{
    error::{Error, Result},
    performance::Operation,
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
    preparation: super::coordination::Coordination,
}

impl Executor {
    pub async fn prepare(
        self: &Arc<Self>,
        client: &reqwest::Client,
        master: &url::Url,
        token: &str,
        attempt: &str,
    ) -> Result<()> {
        let mut timing = Operation::new("node_prepare", attempt, "plan");
        let session = Session {
            client: client.clone(),
            master: master.clone(),
            token: token.into(),
            attempt: attempt.into(),
        };
        let data = super::node_data_dir();
        let descriptor = session.json("plan", &json!({})).await?;
        let run = text(&descriptor["plan"], "runId");
        crate::validation::uuid(run)?;
        timing.next("queue");
        let _guard = self.preparation.lock(run).await;
        if self.attempts.lock().await.contains_key(attempt) {
            timing.finish();
            return Ok(());
        }
        timing.next("seed");
        let plan = stage_plan(&session, &data, descriptor).await?;
        let root = data.join("runs").join(text(&plan, "runId"));
        timing.next("lease");
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
        timing.finish();
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
            let expected = if crate::native::enabled() {
                let plan: Value = serde_json::from_slice(
                    &tokio::fs::read(plan_path(&super::node_data_dir(), attempt)).await?,
                )?;
                PathBuf::from(text(&plan, "guestDataRoot"))
                    .join("runs")
                    .join(run)
                    .join("workspace")
                    .join(project)
            } else {
                target.clone()
            };
            if value["source"] != expected.to_string_lossy().as_ref() {
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
async fn stage_plan(session: &Session, data: &Path, descriptor: Value) -> Result<Value> {
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
    let seed_workspace = crate::native::enabled() || plan["resumeWorkspace"] != true;
    session
        .seed(
            json!({ "kind": "initial", "workspace": seed_workspace }),
            &root,
        )
        .await?;
    plan["workspaceSeeded"] = seed_workspace.into();
    if !seed_workspace {
        resume_import_directories(&plan, &root).await?;
    }
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

/// Keep mount-source validation intact without copying the projects held by the
/// guest disk. These empty host directories must never initialize a fresh guest.
async fn resume_import_directories(plan: &Value, root: &Path) -> Result<()> {
    let Some(project_root) = plan["projectRoot"].as_str().map(Path::new) else {
        return Ok(());
    };
    for import in plan["imports"].as_array().into_iter().flatten() {
        let source = Path::new(text(import, "source"));
        if !source.starts_with(project_root) {
            continue;
        }
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

    #[tokio::test]
    async fn resume_import_directories_reject_symlinks_and_parent_traversal() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let project = root.path().join("workspace");
        tokio::fs::create_dir(&project).await.unwrap();
        tokio::fs::symlink(outside.path(), project.join("link"))
            .await
            .unwrap();
        for source in [project.join("link/child"), project.join("../escape")] {
            let plan = json!({"projectRoot":project,"imports":[{"source":source}]});
            let error = resume_import_directories(&plan, root.path())
                .await
                .unwrap_err();
            assert_eq!(error.status, 400);
            assert!(!outside.path().join("child").exists());
            assert!(!root.path().join("escape").exists());
        }
        let source = project.join("safe/child");
        let plan = json!({"projectRoot":project,"imports":[{"source":source}]});
        resume_import_directories(&plan, root.path()).await.unwrap();
        assert_eq!(tokio::fs::canonicalize(&source).await.unwrap(), source);
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
