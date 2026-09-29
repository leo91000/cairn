//! Retryable conversation deletion and disk cleanup. No archival or data export.
use crate::{
    error::{Error, Result},
    service::Service,
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
    let checkpoint = s
        .store
        .kv(&format!("run-checkpoint:{run}"))
        .await?
        .unwrap_or_default();
    let node = destination
        .or(checkpoint["nodeId"].as_str())
        .unwrap_or(crate::nodes::LOCAL_NODE_ID);
    async {
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
        if action == "delete" || action == "prune" {
            let (run, node, all, retired) = (
                run.to_owned(),
                node.to_owned(),
                action == "delete",
                value["retiredGrants"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default(),
            );
            s.store
                .transaction(move |db| {
                    for grant in db.list("node-disk-grants")? {
                        if grant["runId"] == run
                            && grant["nodeId"] == node
                            && (all || retired.contains(&grant["id"]))
                        {
                            db.remove("node-disk-grants", text(&grant, "id"))?;
                        }
                    }
                    Ok(())
                })
                .await?;
        }
        if action == "delete" {
            let id = format!("{run}:{}", node);
            s.store
                .write(move |db| db.remove("node-volumes", &id))
                .await?;
        }
        Ok(())
    }
    .await
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

async fn delete_disks(s: &Service, run: &str) -> Result<()> {
    let volumes = s
        .store
        .list("node-volumes")
        .await?
        .into_iter()
        .filter(|v| v["runId"] == run)
        .collect::<Vec<_>>();
    if volumes.is_empty() {
        return disk(s, run, "delete", None).await;
    }
    for volume in volumes {
        disk(s, run, "delete", Some(text(&volume, "nodeId"))).await?;
    }
    Ok(())
}

async fn detach_worktrees(s: &Service, chat: &Value) -> Result<()> {
    let run = if text(chat, "runId").is_empty() {
        Value::Null
    } else {
        s.store.run(text(chat, "runId")).await?
    };
    let root = s.config.data_dir.join("runs").join(text(chat, "runId"));
    let mut workspaces = chat["legacyWorkspaces"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    workspaces.extend(run["workspaces"].as_array().into_iter().flatten().cloned());
    let mut projects = chat["legacyProjects"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    projects.extend(crate::service::run_projects(&run));
    for workspace in workspaces.iter().filter(|w| w["kind"] == "worktree") {
        let target = Path::new(text(workspace, "path"));
        if !target.is_dir() {
            continue;
        }
        if !target.starts_with(&root) || tokio::fs::canonicalize(target).await? != target {
            return Err(Error::bad("Invalid conversation worktree path."));
        }
        if let Some(project) = projects.iter().find(|p| p["id"] == workspace["projectId"]) {
            let project_path = Path::new(text(project, "path"));
            if project_path.is_dir() {
                let status = tokio::process::Command::new("git")
                    .arg("-C")
                    .arg(project_path)
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
            }
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
        {
            let id = text(&artifact, "id");
            crate::validation::uuid(id)?;
            for name in [id.to_owned(), format!("{id}.jpg")] {
                remove(&s.config.data_dir.join("artifacts").join(name)).await?;
            }
        }
    }
    Ok(())
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
    s.store
        .transaction(move |db| {
            for prefix in [
                format!("artifact:{run}:"),
                format!("chat-question:{cid}:"),
                format!("chat-attachment:{cid}:"),
            ] {
                for (key, value) in db.keys(&prefix)? {
                    if let Some(token) = value["publicToken"].as_str() {
                        db.delete(&format!("artifact-share:{token}"))?;
                    }
                    db.delete(&key)?;
                }
            }
            for key in [
                format!("run-checkpoint:{run}"),
                format!("chat-error:{cid}"),
                format!("chat-title-pending:{cid}"),
                format!("chat-title-checked:{cid}"),
            ] {
                db.delete(&key)?;
            }
            for prefix in ["push-outbox:", "mcp-grant:"] {
                for (key, value) in db.keys(prefix)? {
                    if value["runId"] == run || value["chatId"] == cid {
                        db.delete(&key)?;
                    }
                }
            }
            db.0.execute("DELETE FROM chat_messages WHERE chat_id=?", [&cid])?;
            db.0.execute("DELETE FROM runs WHERE id=?", [&run])?;
            db.remove("chats", &cid)?;
            db.audit("chat.purged", &json!({"id": cid}))?;
            Ok(())
        })
        .await
}
