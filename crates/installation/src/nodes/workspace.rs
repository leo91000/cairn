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
    match plan["chat"]["provider"].as_str() {
        Some("claude") => ".claude",
        Some("opencode") => ".opencode",
        _ => ".codex",
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
    let record = s.get("nodes", node).await?;
    let native_allowed = if crate::native::policy::node_native(&record) {
        let (run, record) = (run.clone(), record.clone());
        s.store
            .read(move |db| crate::native::policy::allows(db, &run, &record))
            .await?
    } else {
        true
    };
    if !native_allowed
        || ownership["released"] == true
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
            let bytes = crate::chat_inbox::read(
                &root.join("chat-input"),
                crate::chat_inbox::TRANSPORT_LIMIT,
            )
            .await;
            let bytes = match bytes {
                Ok(bytes) => bytes.unwrap_or_else(|| b"[]".to_vec()),
                // This response is a change detector, not the inbox transfer.
                // Keep refresh_inbox seeding the file so the controller's bounded
                // reader reports overflow to the run instead of retaining stale input.
                Err(error) if error.status == 400 => {
                    return Ok(Json(json!({ "oversized": true })).into_response());
                }
                Err(error) => return Err(error),
            };
            let messages: Value = serde_json::from_slice(&bytes)?;
            Ok(Json(json!({ "messages": messages })).into_response())
        }
        "seed" => seed_source(&run, &plan, &root, &input.body).await,
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
async fn seed_source(
    run: &Value,
    plan: &Value,
    root: &Path,
    selection: &Value,
) -> Result<Response> {
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
    // Older nodes omit `workspace` and keep receiving a complete seed. Never
    // let a node omit bootstrap files for a fresh VM or a native environment.
    let resumed_vm = plan["backend"] == "firecracker" && plan["resumeWorkspace"] == true;
    let skip_projects =
        text(selection, "kind") == "initial" && selection["workspace"] == false && resumed_vm;
    let project_root = if skip_projects {
        plan["projectRoot"]
            .as_str()
            .and_then(|path| Path::new(path).strip_prefix(root).ok())
            .filter(|path| !path.as_os_str().is_empty())
            .map(Path::to_path_buf)
    } else {
        None
    };
    let (mut writer, reader) = tokio::io::duplex(131072);
    tokio::spawn(async move {
        // A failed transfer ends the stream early, which the receiver rejects.
        let _ = super::files::send_filtered(&source, &mut writer, |path| {
            project_root.as_deref() != Some(path)
        })
        .await;
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
    let _disk = if crate::native::enabled() {
        Some(crate::native::disk::lock(&crate::native::state_dir()).await?)
    } else {
        None
    };
    let budget = if crate::native::enabled() {
        let available = crate::native::disk::staging_budget().await?;
        // Inbox refresh copies into an inode already mounted by the workload;
        // its received temporary tree and the replacement can coexist briefly.
        let available = if selection["kind"] == "inbox" {
            available / 2
        } else {
            available
        };
        SEED_BUDGET_BYTES.min(available)
    } else {
        SEED_BUDGET_BYTES
    };
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
    super::files::receive(&mut reader, temp.path(), budget).await?;
    // Preserve the directory inode mounted into a native workload. Replacing a
    // bind-mounted directory would leave it reading the previous inbox forever.
    if crate::native::enabled() && selection["kind"] == "inbox" {
        return install_native_inbox(temp.path(), target).await;
    }
    // Only host staging files are replaced. Guest disks remain authoritative.
    if target.exists() {
        tokio::fs::remove_dir_all(target).await?;
    }
    tokio::fs::rename(temp.path(), target).await?;
    Ok(())
}

async fn install_native_inbox(source: &Path, target: &Path) -> Result<()> {
    crate::skills::private_dir(target).await?;
    let bytes = tokio::fs::read(source.join("messages.json")).await?;
    crate::skills::atomic_write(&target.join("messages.json"), &bytes).await?;
    tokio::fs::set_permissions(
        target.join("messages.json"),
        std::fs::Permissions::from_mode(0o444),
    )
    .await?;
    // This root-owned inode is bind-mounted read-only into the workload. It
    // must stay traversable after every refresh, including under UMask=0077.
    tokio::fs::set_permissions(target, std::fs::Permissions::from_mode(0o755)).await?;
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

#[cfg(test)]
mod native_inbox_tests {
    use super::*;
    use std::os::unix::fs::MetadataExt;

    #[tokio::test]
    async fn native_refresh_preserves_the_bound_directory_and_unprivileged_read_access() {
        let root = tempfile::tempdir().unwrap();
        let (source, target) = (root.path().join("source"), root.path().join("inbox"));
        crate::skills::private_dir(&source).await.unwrap();
        crate::skills::private_dir(&target).await.unwrap();
        let inode = tokio::fs::metadata(&target).await.unwrap().ino();
        for message in [b"first".as_slice(), b"follow-up".as_slice()] {
            tokio::fs::write(source.join("messages.json"), message)
                .await
                .unwrap();
            install_native_inbox(&source, &target).await.unwrap();
            let metadata = tokio::fs::metadata(&target).await.unwrap();
            assert_eq!(metadata.ino(), inode);
            assert_eq!(metadata.permissions().mode() & 0o777, 0o755);
            assert_eq!(
                tokio::fs::metadata(target.join("messages.json"))
                    .await
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o444
            );
            assert_eq!(
                tokio::fs::read(target.join("messages.json")).await.unwrap(),
                message
            );
        }
    }
}
