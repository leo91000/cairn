//! Master-approved node releases and bounded preparation for maintenance.
use super::NodeState;
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
use cairn_contracts::EventKind;
use serde::Serialize;
use serde_json::{Value, json};
use std::time::Duration;

mod diagnostic;

use diagnostic::{Drain, Outcome, Phase};

#[cfg(test)]
mod diagnostic_tests;

/// Maintenance progress of a node, persisted in `node.maintenance`.
#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "kebab-case")]
enum Maintenance {
    Draining,
    ReadyToUpdate,
}

/// The node image and protocol the master currently deploys.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Release {
    pub image: String,
    commit: String,
    protocol: u32,
    shutdown_timeout_seconds: Value,
}

pub(crate) fn release() -> Result<Release> {
    let image = std::env::var("CAIRN_NODE_IMAGE").unwrap_or_default();
    let (repository, digest) = image.split_once("@sha256:").ok_or_else(|| {
        Error::unavailable(
            "The master must configure CAIRN_NODE_IMAGE with its immutable deployed image digest.",
        )
    })?;
    if repository.is_empty()
        || repository.len() > 400
        || !repository.starts_with(|c: char| c.is_ascii_alphanumeric())
        || !repository
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || b"/:._-".contains(&c))
        || digest.len() != 64
        || !digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(Error::unavailable("Invalid master node image digest."));
    }
    Ok(Release {
        commit: std::env::var("APP_COMMIT").unwrap_or_else(|_| "development".into()),
        image,
        protocol: 2,
        shutdown_timeout_seconds: 300.into(),
    })
}

pub async fn downloads(State(app): State<App>, request: Request) -> Result<Response> {
    if request.method() != "GET" {
        return Err(Error::method_not_allowed("Method not allowed."));
    }
    let (kind, body) = match request.uri().path() {
        "/internal/nodes/native-host.py" => (
            "text/x-python",
            include_str!("../../../../deploy/nodes/native-host.py").into(),
        ),
        "/internal/nodes/native-install.sh" => (
            "text/x-shellscript",
            include_str!("../../../../deploy/nodes/native-install.sh").into(),
        ),
        "/internal/nodes/release" => ("application/json", advertise(&app.service).await?),
        "/internal/nodes/host.py" => (
            "text/x-python",
            include_str!("../../../../deploy/nodes/host.py").into(),
        ),
        "/internal/nodes/install.sh" => {
            if request.uri().query().is_some() {
                return Err(Error::bad(
                    "Pass the direct manager address as an installer argument, not in its URL.",
                ));
            }
            release()?;
            // The manager's Docker-only origin need not be reachable or HTTPS.
            // The generated command supplies the validated direct address.
            let origin = super::connector::master(&app.service.config.public_url)
                .map(|origin| origin.to_string())
                .unwrap_or_default();
            let quoted = format!("'{}'", origin.replace('\'', "'\\''"));
            (
                "text/x-shellscript",
                include_str!("../../../../deploy/nodes/install.sh")
                    .replace("__CAIRN_MASTER_ORIGIN__", &quoted),
            )
        }
        _ => return Err(Error::not_found("Unknown node download.")),
    };
    Response::builder()
        .header("content-type", kind)
        .header("cache-control", "no-store")
        .body(Body::from(body))
        .map_err(Error::internal)
}

/// The current release and configured shutdown deadline.
async fn advertise(s: &Service) -> Result<String> {
    let mut release = release()?;
    release.shutdown_timeout_seconds =
        super::publication::settings(s).await?["shutdownTimeoutSeconds"].clone();
    Ok(serde_json::to_value(release)?.to_string())
}

pub async fn request(s: &Service, node: &str, input: &Value) -> Result<Value> {
    match text(input, "action") {
        "status" => s.node_boundary(node).await,
        "complete" => complete(s, node, input).await,
        "drain" => start_drain(s, node).await,
        _ => Err(Error::bad("Invalid maintenance action.")),
    }
}

async fn complete(s: &Service, node: &str, input: &Value) -> Result<Value> {
    let node = node.to_owned();
    let image = text(input, "image").to_owned();
    let error = input["error"]
        .as_str()
        .map(|v| v.chars().take(500).collect::<String>());
    s.store
        .transaction(move |db| {
            let mut record = db
                .node_boundary(&node)?
                .ok_or_else(|| Error::not_found("Node removed."))?;
            record["maintenance"] = Value::Null;
            record["maintenanceGeneration"] = Value::Null;
            record["imageDigest"] = image.into();
            record["updateError"] = error.into();
            db.save_node_boundary(&record)?;
            Ok(record)
        })
        .await
}

/// Marks the node draining and pauses its conversations in the background.
async fn start_drain(s: &Service, node: &str) -> Result<Value> {
    let node = node.to_owned();
    if !s.node_maintenance_tasks.lock().await.insert(node.clone()) {
        return s.node_boundary(&node).await;
    }
    let timeout = super::publication::settings(s).await?["shutdownTimeoutSeconds"]
        .as_u64()
        .unwrap_or(300)
        .saturating_sub(20)
        .max(10);
    let generation = crate::config::id();
    let started = {
        let (id, generation) = (node.clone(), generation.clone());
        s.store
            .transaction(move |db| {
                let mut record = db
                    .node_boundary(&id)?
                    .ok_or_else(|| Error::not_found("Node removed."))?;
                record["maintenance"] = serde_json::to_value(Maintenance::Draining)?;
                record["maintenanceGeneration"] = generation.into();
                record["maintenanceStartedAt"] = crate::config::now().into();
                db.save_node_boundary(&record)?;
                Ok(())
            })
            .await
    };
    if let Err(error) = started {
        s.node_maintenance_tasks.lock().await.remove(&node);
        return Err(error);
    }
    let service = s.clone();
    tokio::spawn(async move {
        let error = prepare(&service, &node, Duration::from_secs(timeout)).await;
        if let Err(error) = finish_drain(&service, &node, generation, error).await {
            tracing::warn!(error = %error, "failed to record node maintenance readiness");
        }
        service.node_maintenance_tasks.lock().await.remove(&node);
    });
    Ok(json!({ "maintenance": Maintenance::Draining }))
}

/// Runs the existing bounded drain before readiness is persisted. Keeping this
/// future separate also makes cancellation observable when its owner drops it.
async fn prepare(s: &Service, node: &str, timeout: Duration) -> Value {
    let mut diagnostic = Drain::new();
    let result = tokio::time::timeout(timeout, drain(s, node, &mut diagnostic)).await;
    let (outcome, error) = match result {
        Ok(Ok(())) => (Outcome::Completed, Value::Null),
        Ok(Err(error)) => (Outcome::Error, error.message.into()),
        Err(_) => (
            Outcome::Deadline,
            "Preparation deadline reached; the node must stop its controller before updating."
                .into(),
        ),
    };
    diagnostic.finish(outcome);
    error
}

/// Reports readiness only for the drain that is still current.
async fn finish_drain(s: &Service, node: &str, generation: String, error: Value) -> Result<()> {
    let node = node.to_owned();
    s.store
        .transaction(move |db| {
            if let Some(mut record) = db.node_boundary(&node)?
                && record["maintenanceGeneration"] == generation
            {
                record["maintenance"] = serde_json::to_value(Maintenance::ReadyToUpdate)?;
                record["maintenanceError"] = error;
                db.save_node_boundary(&record)?;
            }
            Ok(())
        })
        .await
}

async fn drain(s: &Service, node: &str, diagnostic: &mut Drain) -> Result<()> {
    let mut failure = None;
    for attempt in s
        .store
        .list("node-attempts")
        .await?
        .into_iter()
        .filter(|a| {
            a["nodeId"] == node
                && super::is_active_attempt(a)
                && a["role"] == super::placement::AttemptRole::Execution.as_str()
        })
    {
        diagnostic.attempts += 1;
        diagnostic.next(Phase::Preparation);
        let run = s.store.run(text(&attempt, "runId")).await?;
        let native = crate::native::policy::native(&run["snapshot"]["agent"]);
        let run_id = text(&run, "id");
        let checkpoint = super::checkpoint(s, run_id).await?;
        if checkpoint["nodeId"] != node || checkpoint["runnerId"] != attempt["id"] {
            diagnostic.stale_attempts += 1;
            continue;
        }
        s.store
            .update_run_boundary(run_id, json!({ "nodeState": NodeState::Updating }))
            .await?;
        s.store
            .event(
                run_id,
                &EventKind::Status,
                "Node maintenance: pausing and saving the environment before restart.",
                None,
            )
            .await?;
        diagnostic.next(Phase::PauseRequest);
        diagnostic.pause_requests += 1;
        let stopped = s
            .http
            .delete(format!(
                "{}/internal/execution/{node}/runs/{}",
                s.config.public_url.trim_end_matches('/'),
                text(&attempt, "id")
            ))
            .bearer_auth(super::runner_secret(s).await?)
            .timeout(Duration::from_secs(if native { 300 } else { 20 }))
            .send()
            .await;
        if !stopped.is_ok_and(|r| r.status().is_success()) {
            diagnostic.pause_failures += 1;
            failure = Some(Error::unavailable(
                "Node did not confirm maintenance pause.",
            ));
            continue;
        }
        if !native {
            release_placement(s, text(&attempt, "id"), diagnostic).await?;
        }
        let mut synchronized = true;
        if run["sessionId"].is_string() {
            loop {
                diagnostic.next(Phase::Capture);
                diagnostic.capture_calls += 1;
                match super::publication::capture_for_update(s, &run, node).await {
                    Ok(_) => break,
                    Err(error) if native && error.status == 425 => {
                        // The enclosing drain deadline bounds this wait. Keep
                        // placement until the node's stopped archive is ready.
                        diagnostic.capture_waits += 1;
                        diagnostic.next(Phase::CaptureWait);
                        tokio::time::sleep(Duration::from_millis(100)).await;
                    }
                    Err(error) => {
                        diagnostic.capture_failures += 1;
                        failure = Some(error);
                        synchronized = false;
                        break;
                    }
                }
            }
        }
        if native && synchronized {
            release_placement(s, text(&attempt, "id"), diagnostic).await?;
        }
    }
    failure.map_or(Ok(()), Err)
}

async fn release_placement(s: &Service, attempt: &str, diagnostic: &mut Drain) -> Result<()> {
    diagnostic.next(Phase::PlacementRelease);
    diagnostic.placement_releases += 1;
    let result = super::placement::release(s, attempt).await;
    if result.is_err() {
        diagnostic.placement_failures += 1;
    }
    result
}
