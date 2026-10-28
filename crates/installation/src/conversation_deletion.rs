//! Retryable conversation deletion and disk cleanup. No archival or data export.
use crate::{
    error::{Error, Result},
    service::Service,
    store::Db,
    validation::text,
};
use serde_json::{Value, json};
use std::path::Path;

async fn remove(path: &Path) -> Result<()> {
    match tokio::fs::symlink_metadata(path).await {
        Ok(meta) if meta.is_dir() => tokio::fs::remove_dir_all(path).await?,
        Ok(_) => tokio::fs::remove_file(path).await?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    Ok(())
}

/// Forgets disk grants a node reported retired, or every grant when the disk is gone.
async fn forget_disk_grants(
    s: &Service,
    run: &str,
    node: &str,
    all: bool,
    retired: Vec<Value>,
) -> Result<()> {
    let (run, node) = (run.to_owned(), node.to_owned());
    s.store
        .transaction(move |db| {
            for grant in db.list("node-disk-grants")? {
                let owned = grant["runId"] == run && grant["nodeId"] == node;
                if owned && (all || retired.contains(&grant["id"])) {
                    db.remove("node-disk-grants", text(&grant, "id"))?;
                }
            }
            Ok(())
        })
        .await
}

async fn forget_deleted_disk(s: &Service, run: &str, node: &str) -> Result<()> {
    forget_disk_grants(s, run, node, true, Vec::new()).await?;
    let id = format!("{run}:{node}");
    s.store
        .write(move |db| db.remove("node-volumes", &id))
        .await
}

async fn disk(s: &Service, run: &str, action: &str, destination: Option<&str>) -> Result<()> {
    if run.is_empty() {
        return Ok(());
    }
    let base = match destination {
        Some(node) if node != crate::nodes::LOCAL_NODE_ID => format!(
            "{}/internal/execution/{node}",
            s.config.public_url.trim_end_matches('/')
        ),
        Some(_) => s.config.runner_url.clone(),
        None => crate::nodes::transport::url(s, run).await?,
    };
    if base.is_empty() {
        if s.store.run(run).await?["isolated"] == true {
            return Err(Error::unavailable("The VM node is unavailable."));
        }
        return Ok(());
    }
    let credential = crate::execution::secret(&s.config.data_dir, "runner-secret").await?;
    let checkpoint = s.store.checkpoint_boundary(run).await?.unwrap_or_default();
    let node = destination
        .or(checkpoint["nodeId"].as_str())
        .unwrap_or(crate::nodes::LOCAL_NODE_ID);
    let response = s
        .http
        .post(format!("{base}/disks/{run}/{action}"))
        .bearer_auth(&credential)
        .json(&json!({}))
        .timeout(std::time::Duration::from_secs(7200))
        .send()
        .await
        .map_err(|_| Error::unavailable("Workspace transfer interrupted."))?;
    if !response.status().is_success() {
        return Err(Error::unavailable(
            "Workspace transfer failed or the agent has not stopped yet.",
        ));
    }
    let value: Value = response.json().await.map_err(Error::internal)?;
    let deleted = action == "delete";
    if deleted {
        forget_deleted_disk(s, run, node).await?;
    } else if action == "prune" {
        let retired = value["retiredGrants"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        forget_disk_grants(s, run, node, false, retired).await?;
    }
    Ok(())
}

/// Frees an old disk left on a node: the whole disk of a conversation that now runs
/// elsewhere, or only the older copies kept beside a conversation's current disk.
pub async fn discard_stale_disk(s: &Service, run: &str, node: &str, whole: bool) -> Result<()> {
    let action = if whole { "delete" } else { "prune" };
    disk(s, run, action, Some(node)).await?;
    if !whole {
        let id = format!("{run}:{node}");
        s.store
            .transaction(move |db| {
                if let Some(mut volume) = db.get("node-volumes", &id)?
                    && let Some(active) = volume["activeDiskMiB"].as_u64()
                {
                    volume["diskMiB"] = active.into();
                    db.put("node-volumes", &volume)?;
                }
                Ok(())
            })
            .await?;
    }
    Ok(())
}

/// Irreversible conversation deletion must also release its published data when
/// a permanently revoked machine can never acknowledge local disk erasure.
async fn delete_disk_for_purge(s: &Service, run: &str, destination: Option<&str>) -> Result<()> {
    if run.is_empty() {
        return Ok(());
    }
    let checkpoint = crate::nodes::checkpoint(s, run).await?;
    let node = destination
        .or(checkpoint["nodeId"].as_str())
        .unwrap_or(crate::nodes::LOCAL_NODE_ID);
    let revoked = node != crate::nodes::LOCAL_NODE_ID
        && s.store
            .node_boundary(node)
            .await?
            .is_some_and(|record| record["revoked"] == true);
    if revoked {
        forget_deleted_disk(s, run, node).await?;
        tracing::info!(
            run,
            node,
            remote_erasure_confirmed = false,
            "Released deleted conversation references on a permanently revoked node"
        );
        return Ok(());
    }
    disk(s, run, "delete", destination).await
}

async fn delete_disks(s: &Service, run: &str) -> Result<()> {
    if run.is_empty() {
        return Ok(());
    }
    let volumes = s
        .store
        .list("node-volumes")
        .await?
        .into_iter()
        .filter(|v| v["runId"] == run)
        .collect::<Vec<_>>();
    if volumes.is_empty() {
        if s.store.run(run).await?["environmentState"] == "expired" {
            return Ok(());
        }
        return delete_disk_for_purge(s, run, None).await;
    }
    for volume in volumes {
        delete_disk_for_purge(s, run, Some(text(&volume, "nodeId"))).await?;
    }
    Ok(())
}

async fn remove_worktree(project: &Path, target: &Path) -> Result<()> {
    let status = tokio::process::Command::new("git")
        .arg("-C")
        .arg(project)
        .args(["worktree", "remove", "--force", "--"])
        .arg(target)
        .env_clear()
        .envs(std::env::vars().filter(|(key, _)| !crate::process::storage_key(key)))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .status()
        .await?;
    if !status.success() {
        return Err(Error::unavailable(
            "Unable to detach a conversation worktree; cleanup will retry.",
        ));
    }
    Ok(())
}

fn array(value: &Value) -> Vec<Value> {
    value.as_array().cloned().unwrap_or_default()
}

async fn detach_worktrees(s: &Service, chat: &Value) -> Result<()> {
    let run_id = text(chat, "runId");
    let run = if run_id.is_empty() {
        Value::Null
    } else {
        s.store.run(run_id).await?
    };
    let root = s.config.data_dir.join("runs").join(run_id);
    let workspaces = array(&run["workspaces"]);
    let projects = crate::service::run_projects(&run);
    for workspace in workspaces.iter().filter(|w| w["kind"] == "worktree") {
        let target = Path::new(text(workspace, "path"));
        if !target.is_dir() {
            continue;
        }
        if !target.starts_with(&root) || tokio::fs::canonicalize(target).await? != target {
            return Err(Error::bad("Invalid conversation worktree path."));
        }
        let Some(project) = projects.iter().find(|p| p["id"] == workspace["projectId"]) else {
            continue;
        };
        let project = Path::new(text(project, "path"));
        if project.is_dir() {
            remove_worktree(project, target).await?;
        }
    }
    Ok(())
}

async fn files(s: &Service, chat: &Value) -> Result<()> {
    let run = text(chat, "runId");
    detach_worktrees(s, chat).await?;
    if !run.is_empty() {
        remove(&s.config.data_dir.join("runs").join(run)).await?;
    }
    remove(
        &s.config
            .data_dir
            .join("chat-attachments")
            .join(text(chat, "id")),
    )
    .await?;
    for (_, artifact) in s.store.keys(&format!("artifact:{run}:")).await? {
        let id = text(&artifact, "id");
        crate::validation::uuid(id)?;
        for name in [id.to_owned(), format!("{id}.jpg")] {
            remove(&s.config.data_dir.join("artifacts").join(name)).await?;
        }
    }
    Ok(())
}

/// Deletes every record of a conversation and its run, including share tokens.
fn forget(db: &Db<'_>, cid: &str, run: &str) -> Result<()> {
    for prefix in [
        format!("artifact:{run}:"),
        crate::chats::question_prefix(cid),
        format!("chat-attachment:{cid}:"),
    ] {
        for (key, value) in db.keys(&prefix)? {
            if let Some(token) = value["publicToken"].as_str() {
                db.delete(&format!("artifact-share:{token}"))?;
            }
            db.delete(&key)?;
        }
    }
    db.delete_checkpoint(run)?;
    for key in [
        format!("turn-speed:{run}"),
        format!("chat-error:{cid}"),
        format!("chat-title-pending:{cid}"),
        format!("chat-title-checked:{cid}"),
    ] {
        db.delete(&key)?;
    }
    for prefix in ["push-outbox:"] {
        for (key, value) in db.keys(prefix)? {
            if value["runId"] == run || value["chatId"] == cid {
                db.delete(&key)?;
            }
        }
    }
    db.revoke_mcp_run(run)?;
    db.0.execute("DELETE FROM chat_messages WHERE chat_id=?", [cid])?;
    db.0.execute("DELETE FROM runs WHERE id=?", [run])?;
    db.remove_conversation(cid)?;
    db.audit("chat.purged", &json!({ "id": cid }))
}

pub async fn purge(s: &Service, chat: Value) -> Result<()> {
    let _previews = crate::artifacts::preview::JOBS
        .acquire()
        .await
        .map_err(Error::internal)?;
    let cid = text(&chat, "id").to_owned();
    let run = text(&chat, "runId").to_owned();
    if !run.is_empty() {
        s.accounts.recover_run(s, &s.store.run(&run).await?).await?;
    }
    delete_disks(s, &run).await?;
    crate::nodes::publication::purge(s, &run).await?;
    files(s, &chat).await?;
    s.store.transaction(move |db| forget(db, &cid, &run)).await
}

/// Retires only the disposable environment. Messages, uploads, artifacts and
/// public artifact URLs belong to the conversation and remain available.
pub async fn expire_environment(s: &Service, chat: Value) -> Result<()> {
    let cid = text(&chat, "id").to_owned();
    let run = text(&chat, "runId").to_owned();
    if !run.is_empty() {
        s.accounts.recover_run(s, &s.store.run(&run).await?).await?;
        delete_disks(s, &run).await?;
        crate::nodes::publication::expire_environment(s, &run).await?;
        detach_worktrees(s, &chat).await?;
        remove(&s.config.data_dir.join("runs").join(&run)).await?;
    }
    s.store.transaction(move |db| {
        if !run.is_empty() {
            let boundary = json!({
                "environmentState": "expired",
                "environmentExpiredAt": chat["environmentExpiredAt"],
                // Disk deletion already fenced the old environment. Recovery
                // must not contact its retired node before fresh placement.
                "isolated": false,
                "backup": null,
                "storage": null,
                "nodeId": null,
                "targetNodeId": null,
                "preferredNodeId": null,
                "pinnedNodeId": null,
                "requiredRuntimeId": null,
                "requiredRuntime": null,
                "runtimeId": null,
                "nodeState": null,
                "workspaces": [],
                "restoredAt": null,
                "contextUsage": null,
                "movementError": null,
            });
            db.change_run(&run, crate::runs::RunChanges {
                session_id: Some(None),
                resume_available: Some(false),
                workspace: Some(None),
                workspace_cleaned_at: Some(None),
                recovery_pending: Some(false),
                boundary: Some(boundary),
                ..Default::default()
            })?;
            crate::worker::checkpoint::RunCheckpoint {
                completed: Some(true),
                ..Default::default()
            }.store_in(db, &run)?;
            db.event(&run, &cairn_contracts::EventKind::Status,
                "Environment removed after inactivity. Conversation history, uploads and published artifacts are retained. The next message starts a fresh environment.", None)?;
        }
        if let Some(mut current) = db.conversation(&cid)? {
            current.environment_state = Some(cairn_contracts::ConversationEnvironmentState::Expired);
            current.environment_error = None;
            current.environment_retry_after = None;
            db.save_conversation(&current)?;
        }
        Ok(())
    }).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn forgetting_a_conversation_removes_its_run_records() {
        let root = tempfile::tempdir().unwrap();
        let store = crate::store::Store::open(root.path()).unwrap();
        store
            .transaction(|db| {
                db.save_conversation_boundary(&json!({ "id": "chat", "runId": "run" }))?;
                for key in ["turn-speed:run", "chat-error:chat"] {
                    db.set(key, &json!({ "messageId": "message" }), None)?;
                }
                forget(db, "chat", "run")?;
                for key in ["turn-speed:run", "chat-error:chat"] {
                    assert!(db.kv(key)?.is_none(), "{key}");
                }
                Ok(())
            })
            .await
            .unwrap();
    }
}
