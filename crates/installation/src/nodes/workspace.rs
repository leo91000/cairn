//! Master-owned run scope for remote seed files, inboxes, results, access-token
//! relay and run-scoped MCP.
use crate::{
    error::{Error, Result},
    http::{App, Input},
    run_status::RunStatus,
    service::Service,
    validation::text,
};
use axum::{
    Json,
    body::Body,
    extract::{Request, State},
    response::{IntoResponse, Response},
};
use serde_json::{Value, json};
use std::{
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::io::{AsyncWriteExt, BufReader};

const MAX_RESULT_BYTES: usize = 1_000_000;
const MAX_AUTH_REQUEST_BYTES: usize = 32768;
const SEED_BUDGET_BYTES: u64 = 32 * 1024 * 1024 * 1024;

/// The provider's configuration directory inside the conversation home.
pub(crate) fn provider_home(plan: &Value) -> &'static str {
    if plan["chat"]["provider"] == "claude" {
        ".claude"
    } else {
        ".codex"
    }
}

async fn authorize(s: &Service, node: &str, attempt: &str) -> Result<(Value, Value)> {
    crate::validation::uuid(attempt)?;
    let bytes = tokio::fs::read(
        s.config
            .data_dir
            .join("runner-plans")
            .join(format!("{attempt}.json")),
    )
    .await?;
    let plan: Value = serde_json::from_slice(&bytes)?;
    let run = s.store.run(text(&plan, "runId")).await?;
    let checkpoint = super::checkpoint(s, text(&plan, "runId")).await?;
    let agent = s.get("agents", super::run_agent(&run)).await?;
    let ownership = s
        .store
        .get("node-attempts", attempt)
        .await?
        .unwrap_or_default();
    if ownership["released"] == true
        || ownership["nodeId"] != node
        || checkpoint["nodeId"] != node
        || checkpoint["runnerId"] != attempt
        || run["status"] != RunStatus::Running
        || !run["cancelRequestedAt"].is_null()
        || !super::agent_allows(&agent, node)
    {
        return Err(Error::forbidden("Remote execution scope expired."));
    }
    Ok((run, plan))
}

pub async fn handle(State(app): State<App>, request: Request) -> Result<Response> {
    let input = Input::read(request).await?;
    let s = &app.service;
    let node = super::transport::authenticate(s, &input.headers).await?;
    let parts = input
        .path
        .trim_start_matches("/internal/node-workspace/")
        .split('/')
        .collect::<Vec<_>>();
    let [attempt, operation] = parts.as_slice() else {
        return Err(Error::bad("Invalid workspace operation."));
    };
    if input.method != "POST" {
        return Err(Error::method_not_allowed("Method not allowed."));
    }
    let (run, plan) = authorize(s, &node, attempt).await?;
    let root = s.config.data_dir.join("runs").join(text(&run, "id"));
    match *operation {
        "lease" => {
            let lease_ms = renew_lease(s, attempt).await?;
            Ok(Json(json!({ "remainingMs": lease_ms })).into_response())
        }
        "plan" => {
            let descriptor = json!({ "plan": plan, "dataRoot": s.config.data_dir });
            Ok(Json(descriptor).into_response())
        }
        "inbox" => {
            let path = root.join("chat-input/messages.json");
            let bytes = tokio::fs::read(path)
                .await
                .unwrap_or_else(|_| b"[]".to_vec());
            let messages: Value = serde_json::from_slice(&bytes)?;
            Ok(Json(json!({ "messages": messages })).into_response())
        }
        "seed" => seed_source(&run, &root, &input.body).await,
        "auth" => {
            let socket = root
                .join("home")
                .join(provider_home(&plan))
                .join("cairn-auth.sock");
            let value = relay_auth(&socket, &input.body).await?;
            Ok(Json(value).into_response())
        }
        "mcp" => crate::mcps::channel::relayed(s, text(&run, "id"), &input.body).await,
        "result" => {
            let result = text(&input.body, "result");
            if result.len() > MAX_RESULT_BYTES {
                return Err(Error::bad("Result exceeds limit."));
            }
            crate::skills::atomic_write(&root.join("output/result.md"), result.as_bytes()).await?;
            Ok(Json(json!({ "ok": true })).into_response())
        }
        _ => Err(Error::not_found("Unknown workspace operation.")),
    }
}

/// Extends the attempt's lease and returns its duration.
async fn renew_lease(s: &Service, attempt: &str) -> Result<u64> {
    let id = attempt.to_owned();
    let lease_ms = super::publication::lease_ms(s).await?;
    s.store
        .transaction(move |db| {
            let mut record = db
                .get("node-attempts", &id)?
                .ok_or_else(|| Error::conflict("Attempt removed."))?;
            if record["released"] == true {
                return Err(Error::conflict("Attempt released."));
            }
            record["leaseExpiresAt"] = (crate::config::now() + lease_ms as i64).into();
            record["leaseDurationMs"] = record["leaseDurationMs"]
                .as_u64()
                .unwrap_or(0)
                .max(lease_ms)
                .into();
            db.put("node-attempts", &record)?;
            Ok(())
        })
        .await?;
    super::record_lease(s, attempt, lease_ms).await;
    Ok(lease_ms)
}

/// Streams the selected staging directory of the run to the node.
async fn seed_source(run: &Value, root: &Path, selection: &Value) -> Result<Response> {
    let source = match text(selection, "kind") {
        "initial" => root.to_path_buf(),
        "inbox" => root.join("chat-input"),
        "project" => {
            let project = text(selection, "projectId");
            crate::validation::uuid(project)?;
            if !crate::service::run_projects(run)
                .iter()
                .any(|p| p["id"] == project)
            {
                return Err(Error::forbidden("Project outside run scope."));
            }
            root.join("workspace").join(project)
        }
        _ => return Err(Error::bad("Invalid seed selection.")),
    };
    if tokio::fs::canonicalize(&source).await? != source {
        return Err(Error::bad("Workspace seed changed location."));
    }
    let (mut writer, reader) = tokio::io::duplex(131072);
    tokio::spawn(async move {
        // A failed transfer ends the stream early, which the receiver rejects.
        let _ = super::files::send(&source, &mut writer).await;
    });
    Ok(Response::new(Body::from_stream(
        tokio_util::io::ReaderStream::new(reader),
    )))
}

async fn relay_auth(path: &Path, value: &Value) -> Result<Value> {
    if serde_json::to_vec(value)?.len() > MAX_AUTH_REQUEST_BYTES {
        return Err(Error::bad("Invalid authentication request."));
    }
    tokio::time::timeout(Duration::from_secs(45), async {
        let mut stream = BufReader::new(tokio::net::UnixStream::connect(path).await?);
        crate::microvm::wire::write(stream.get_mut(), value).await?;
        crate::microvm::wire::read(&mut stream)
            .await?
            .ok_or_else(|| Error::unavailable("Authentication unavailable."))
    })
    .await
    .map_err(|_| Error::unavailable("Authentication unavailable."))?
}

pub async fn fetch(
    client: &reqwest::Client,
    master: &url::Url,
    token: &str,
    attempt: &str,
    operation: &str,
    body: &Value,
) -> Result<reqwest::Response> {
    let response = client
        .post(
            master
                .join(&format!("internal/node-workspace/{attempt}/{operation}"))
                .map_err(Error::internal)?,
        )
        .bearer_auth(token)
        .json(body)
        .send()
        .await
        .map_err(|_| Error::unavailable("Workspace connection interrupted."))?;
    if !response.status().is_success() {
        return Err(Error::new(
            response.status().as_u16(),
            "Remote workspace access rejected.",
        ));
    }
    Ok(response)
}

pub async fn seed(
    client: &reqwest::Client,
    master: &url::Url,
    token: &str,
    attempt: &str,
    selection: Value,
    target: &Path,
) -> Result<()> {
    use futures_util::StreamExt;
    let response = fetch(client, master, token, attempt, "seed", &selection).await?;
    let parent = target
        .parent()
        .ok_or_else(|| Error::bad("Invalid seed directory."))?;
    crate::skills::private_dir(parent).await?;
    let temp = tempfile::TempDir::new_in(parent)?;
    let stream = response
        .bytes_stream()
        .map(|r| r.map_err(std::io::Error::other));
    let mut reader = BufReader::new(tokio_util::io::StreamReader::new(stream));
    super::files::receive(&mut reader, temp.path(), SEED_BUDGET_BYTES).await?;
    // Only host staging files are replaced. Guest disks remain authoritative.
    if target.exists() {
        tokio::fs::remove_dir_all(target).await?;
    }
    tokio::fs::rename(temp.path(), target).await?;
    Ok(())
}

pub async fn auth_listener(
    path: PathBuf,
    client: reqwest::Client,
    master: url::Url,
    token: String,
    attempt: String,
    stop: tokio_util::sync::CancellationToken,
) -> Result<()> {
    if path.exists() {
        tokio::fs::remove_file(&path).await?;
    }
    let listener = tokio::net::UnixListener::bind(&path)?;
    tokio::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).await?;
    loop {
        let accepted = tokio::select! {
            () = stop.cancelled() => break,
            result = listener.accept() => result,
        };
        let Ok((socket, _)) = accepted else { break };
        let operation = async {
            let mut stream = BufReader::new(socket);
            let request = crate::microvm::wire::read(&mut stream)
                .await?
                .ok_or_else(|| Error::bad("Empty authentication request."))?;
            let value: Value = fetch(&client, &master, &token, &attempt, "auth", &request)
                .await?
                .json()
                .await
                .map_err(Error::internal)?;
            crate::microvm::wire::write(stream.get_mut(), &value).await?;
            stream.get_mut().shutdown().await?;
            Ok::<_, Error>(())
        };
        tokio::select! {
            () = stop.cancelled() => break,
            _ = tokio::time::timeout(Duration::from_secs(45), operation) => {}
        }
    }
    let _ = tokio::fs::remove_file(path).await;
    Ok(())
}
