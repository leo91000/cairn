//! Master-approved node releases and bounded preparation for maintenance.
use crate::{
    error::{Error, Result},
    http::App,
    service::Service,
    validation::text,
};
use axum::{
    body::Body,
    extract::{Request, State},
    response::Response,
};
use serde_json::{Value, json};
use std::time::Duration;

pub fn release() -> Result<Value> {
    let image = std::env::var("LEO_NODE_IMAGE").unwrap_or_default();
    let (repository, digest) = image.split_once("@sha256:").ok_or_else(|| {
        Error::new(
            503,
            "The master must configure LEO_NODE_IMAGE with its immutable deployed image digest.",
        )
    })?;
    if repository.is_empty()
        || repository.len() > 400
        || !repository.starts_with(|c: char| c.is_ascii_alphanumeric())
        || !repository
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || b"/:._-".contains(&c))
        || !super::snapshots::valid_hash(digest)
    {
        return Err(Error::new(503, "Invalid master node image digest."));
    }
    Ok(
        json!({"image":image,"commit":std::env::var("APP_COMMIT").unwrap_or_else(|_|"development".into()),"protocol":2,"shutdownTimeoutSeconds":300}),
    )
}
pub async fn downloads(State(app): State<App>, request: Request) -> Result<Response> {
    if request.method() != "GET" {
        return Err(Error::new(405, "Method not allowed."));
    }
    let (kind, body) = match request.uri().path() {
        "/internal/nodes/release" => {
            let mut value = release()?;
            let runtime = std::env::var("APP_RUNTIME_ID").unwrap_or_else(|_| "development".into());
            if super::valid_runtime(&runtime) {
                app.service
                    .store
                    .set(
                        &format!("node-runtime:{runtime}"),
                        json!({"runtimeId":runtime,"image":value["image"]}),
                        None,
                    )
                    .await?;
            }
            value["shutdownTimeoutSeconds"] =
                super::backups::settings(&app.service).await?["shutdownTimeoutSeconds"].clone();
            ("application/json", value.to_string())
        }
        "/internal/nodes/host.py" => (
            "text/x-python",
            include_str!("../../../deploy/nodes/host.py").into(),
        ),
        "/internal/nodes/install.sh" => {
            release()?;
            let origin = super::connector::master(&app.service.config.public_url)?.to_string();
            let quoted = format!("'{}'", origin.replace('\'', "'\\''"));
            (
                "text/x-shellscript",
                include_str!("../../../deploy/nodes/install.sh")
                    .replace("__LEO_MASTER_ORIGIN__", &quoted),
            )
        }
        _ => return Err(Error::new(404, "Unknown node download.")),
    };
    Response::builder()
        .header("content-type", kind)
        .header("cache-control", "no-store")
        .body(Body::from(body))
        .map_err(Error::internal)
}
pub async fn request(s: &Service, node: &str, input: &Value) -> Result<Value> {
    match text(input, "action") {
        "status" => s.get("nodes", node).await,
        "runtimes" => Ok(
            json!({"runtimes":s.store.keys("node-runtime:").await?.into_iter().map(|(_,value)|value).collect::<Vec<_>>()}),
        ),
        "complete" => {
            let node = node.to_owned();
            let image = text(input, "image").to_owned();
            let error = input["error"]
                .as_str()
                .map(|v| v.chars().take(500).collect::<String>());
            s.store
                .transaction(move |db| {
                    let mut record = db
                        .get("nodes", &node)?
                        .ok_or_else(|| Error::new(404, "Node removed."))?;
                    record["maintenance"] = Value::Null;
                    record["maintenanceGeneration"] = Value::Null;
                    record["imageDigest"] = image.into();
                    record["updateError"] = json!(error);
                    record["updatedAt"] = crate::config::now().into();
                    db.put("nodes", &record)?;
                    Ok(record)
                })
                .await
        }
        "drain" => {
            let node = node.to_owned();
            if !s.node_maintenance_tasks.lock().await.insert(node.clone()) {
                return s.get("nodes", &node).await;
            }
            let timeout = super::backups::settings(s).await?["shutdownTimeoutSeconds"]
                .as_u64()
                .unwrap_or(300)
                .saturating_sub(20)
                .max(10);
            let generation = crate::config::id();
            let started_generation = generation.clone();
            let id = node.clone();
            let started = s
                .store
                .transaction(move |db| {
                    let mut record = db
                        .get("nodes", &id)?
                        .ok_or_else(|| Error::new(404, "Node removed."))?;
                    record["maintenance"] = "draining".into();
                    record["maintenanceGeneration"] = started_generation.into();
                    record["maintenanceStartedAt"] = crate::config::now().into();
                    db.put("nodes", &record)?;
                    Ok(())
                })
                .await;
            if let Err(error) = started {
                s.node_maintenance_tasks.lock().await.remove(&node);
                return Err(error);
            }
            {
                let service = s.clone();
                let id = node.clone();
                tokio::spawn(async move {
                    let result =
                        tokio::time::timeout(Duration::from_secs(timeout), drain(&service, &id))
                            .await;
                    let error=match result {Ok(Ok(()))=>Value::Null,Ok(Err(error))=>error.message.into(),Err(_)=>"Preparation deadline reached; the node must stop its controller before updating.".into()};
                    let key = id.clone();
                    let _ = service
                        .store
                        .transaction(move |db| {
                            if let Some(mut node) = db.get("nodes", &key)?
                                && node["maintenanceGeneration"] == generation
                            {
                                node["maintenance"] = "ready-to-update".into();
                                node["maintenanceError"] = error;
                                db.put("nodes", &node)?;
                            }
                            Ok(())
                        })
                        .await;
                    service.node_maintenance_tasks.lock().await.remove(&id);
                });
            }
            Ok(json!({"maintenance":"draining"}))
        }
        _ => Err(Error::bad("Invalid maintenance action.")),
    }
}
async fn drain(s: &Service, node: &str) -> Result<()> {
    let mut failure = None;
    for attempt in s
        .store
        .list("node-attempts")
        .await?
        .into_iter()
        .filter(|a| a["nodeId"] == node && a["released"] != true && a["role"] == "execution")
    {
        let run = s.store.run(text(&attempt, "runId")).await?;
        let checkpoint = s
            .store
            .kv(&format!("run-checkpoint:{}", text(&run, "id")))
            .await?
            .unwrap_or_default();
        if checkpoint["nodeId"] != node || checkpoint["runnerId"] != attempt["id"] {
            continue;
        }
        s.store
            .patch_run(text(&run, "id"), json!({"nodeState":"updating"}))
            .await?;
        s.store
            .event(
                text(&run, "id"),
                "status",
                "Node maintenance: pausing and saving the environment before restart.",
                None,
            )
            .await?;
        let stopped = s
            .http
            .delete(format!(
                "{}/internal/execution/{node}/runs/{}",
                s.config.public_url.trim_end_matches('/'),
                text(&attempt, "id")
            ))
            .bearer_auth(crate::execution::secret(&s.config.data_dir, "runner-secret").await?)
            .timeout(Duration::from_secs(20))
            .send()
            .await;
        if stopped.is_err() || stopped.is_ok_and(|r| !r.status().is_success()) {
            failure = Some(Error::new(503, "Node did not confirm maintenance pause."));
            continue;
        }
        super::placement::release(s, text(&attempt, "id")).await?;
        if run["sessionId"].is_string()
            && let Err(error) = super::backups::capture(s, &run).await
        {
            failure = Some(error);
        }
    }
    if let Some(error) = failure {
        return Err(error);
    }
    Ok(())
}
