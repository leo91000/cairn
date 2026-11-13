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
    let isolated = s.store.run(run).await?["isolated"] == true;
    disk_on(s, run, action, destination, isolated, None).await
}

async fn disk_on(
    s: &Service,
    run: &str,
    action: &str,
    destination: Option<&str>,
    isolated: bool,
    retirement: Option<&str>,
) -> Result<()> {
    let base = match destination {
        Some(node) if node != crate::nodes::LOCAL_NODE_ID => format!(
            "{}/internal/execution/{node}",
            s.config.public_url.trim_end_matches('/')
        ),
        Some(_) => s.config.runner_url.clone(),
        None => crate::nodes::transport::url(s, run).await?,
    };
    if base.is_empty() {
        if isolated || retirement.is_some() {
            return Err(Error::unavailable("The VM node is unavailable."));
        }
        return Ok(());
    }
    let credential = crate::execution::secret(&s.config.data_dir, "runner-secret").await?;
    let checkpoint = s.store.checkpoint_boundary(run).await?.unwrap_or_default();
    let node = destination
        .or(checkpoint["nodeId"].as_str())
        .unwrap_or(crate::nodes::LOCAL_NODE_ID);
    let body = match retirement {
        Some(id) => json!({ "retirementId": id }),
        None => json!({}),
    };
    let response = s
        .http
        .post(format!("{base}/disks/{run}/{action}"))
        .bearer_auth(&credential)
        .json(&body)
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
    if let Some(retirement) = retirement
        && (value["retirementId"].as_str() != Some(retirement) || value["deleted"] != true)
    {
        return Err(Error::unavailable(
            "The node did not acknowledge durable disk retirement. Upgrade the node before reusing this conversation's disk.",
        ));
    }
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
    if whole {
        return s.retire_stale_disk(run, node).await;
    }
    disk(s, run, "prune", Some(node)).await?;
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
    Ok(())
}

/// A permanently revoked machine can never acknowledge local disk erasure: its disk
/// references are released instead.
async fn forget_revoked_disk(s: &Service, run: &str, node: &str) -> Result<bool> {
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
            "Released retired disk references on a permanently revoked node"
        );
    }
    Ok(revoked)
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
    if forget_revoked_disk(s, run, node).await? {
        return Ok(());
    }
    disk(s, run, "delete", destination).await
}

/// Deletes the disk an expired environment left on `node`, once its execution is fenced.
pub(crate) async fn delete_retired_disk(
    s: &Service,
    run: &str,
    node: &str,
    isolated: bool,
    retirement: &str,
) -> Result<()> {
    if forget_revoked_disk(s, run, node).await? {
        return Ok(());
    }
    tokio::time::timeout(
        std::time::Duration::from_secs(10),
        disk_on(s, run, "delete", Some(node), isolated, Some(retirement)),
    )
    .await
    .map_err(|_| {
        Error::unavailable("The retired disk deletion will retry when its node responds.")
    })?
}

async fn delete_disks(s: &Service, run: &str) -> Result<()> {
    if run.is_empty() {
        return Ok(());
    }
    let run_id = run.to_owned();
    let mut nodes = s
        .store
        .read(move |db| crate::environment_expiration::retiring_nodes(db, &run_id))
        .await?;
    for volume in s
        .store
        .list("node-volumes")
        .await?
        .into_iter()
        .filter(|v| v["runId"] == run)
    {
        let node = text(&volume, "nodeId").to_owned();
        if !nodes.contains(&node) {
            nodes.push(node);
        }
    }
    if nodes.is_empty() {
        if s.store.run(run).await?["environmentState"] == "expired" {
            return Ok(());
        }
        return delete_disk_for_purge(s, run, None).await;
    }
    for node in nodes {
        delete_disk_for_purge(s, run, Some(&node)).await?;
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
    let workspaces = array(&run["workspaces"]);
    let projects = crate::service::run_projects(&run);
    detach_retired_worktrees(s, run_id, &workspaces, &projects).await
}

/// Detaches the worktrees an environment used, from the identities captured with it.
pub(crate) async fn detach_retired_worktrees(
    s: &Service,
    run_id: &str,
    workspaces: &[Value],
    projects: &[Value],
) -> Result<()> {
    let root = s.config.data_dir.join("runs").join(run_id);
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

/// Removes the manager's working files of a run.
pub(crate) async fn remove_run_files(s: &Service, run: &str) -> Result<()> {
    if run.is_empty() {
        return Ok(());
    }
    remove(&s.config.data_dir.join("runs").join(run)).await
}

async fn files(s: &Service, chat: &Value) -> Result<()> {
    let run = text(chat, "runId");
    detach_worktrees(s, chat).await?;
    let run_id = run.to_owned();
    for (workspaces, projects) in s
        .store
        .read(move |db| crate::environment_expiration::detached_worktrees(db, &run_id))
        .await?
    {
        detach_retired_worktrees(s, run, &workspaces, &projects).await?;
    }
    remove_run_files(s, run).await?;
    s.attachments
        .purge(
            &s.store,
            &s.config.data_dir.join("chat-attachments"),
            text(chat, "id"),
        )
        .await?;
    for artifact in s.store.artifacts(Some(run)).await? {
        let id = &artifact.id;
        crate::validation::uuid(id)?;
        for name in [id.to_owned(), format!("{id}.jpg")] {
            remove(&s.config.data_dir.join("artifacts").join(name)).await?;
        }
    }
    Ok(())
}

/// Deletes every record of a conversation and its run, including share tokens.
fn forget(db: &Db<'_>, cid: &str, run: &str) -> Result<()> {
    let [side_questions, open_side_questions, used_side_questions] =
        crate::chats::side_question_prefixes(cid);
    for prefix in [side_questions, open_side_questions] {
        for (key, value) in db.keys(&prefix)? {
            if let Some(token) = value["publicToken"].as_str() {
                db.delete(&format!("artifact-share:{token}"))?;
            }
            db.delete(&key)?;
        }
    }
    // The lifetime identity ledger has only opaque membership markers, never
    // public tokens. Erase its indexed range without loading/parsing every row
    // into a Rust collection. SQLite still performs work proportional to rows.
    db.0.execute(
        "DELETE FROM kv WHERE key>=?1 AND key<?2",
        rusqlite::params![
            used_side_questions,
            format!("{used_side_questions}\u{10ffff}")
        ],
    )?;
    // Physical purge already fenced execution and deleted/forgot every disk.
    // Retire admission that never reached checkpoint publication as well.
    for attempt in db
        .attempts()?
        .into_iter()
        .filter(|attempt| attempt.run_id == run && !attempt.released)
    {
        crate::nodes::placement::release_in(db, &attempt.id)?;
    }
    db.delete_checkpoint(run)?;
    crate::environment_expiration::forget_in(db, run)?;
    for key in [format!("turn-speed:{run}"), format!("chat-error:{cid}")]
        .into_iter()
        .chain(crate::chat_titles::keys(cid))
    {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn completed_purge_releases_admission_missing_from_the_checkpoint() {
        use crate::{attempt_fixture::DbAttemptFixture, run_fixture::DbRunFixture};
        let root = tempfile::tempdir().unwrap();
        let store = crate::store::Store::open(root.path()).unwrap();
        store.transaction(|db| {
            db.add_run_fixture(&json!({ "id": "run", "taskId": "chat", "status": "succeeded", "createdAt": 0 }), None)?;
            db.save_conversation_boundary(&json!({ "id": "chat", "runId": "run" }))?;
            db.put_attempt_fixture(&json!({ "id": "unpublished", "runId": "run", "diskMaterialized": true, "released": false }))?;
            forget(db, "chat", "run")?;
            assert!(db.run("run")?.is_none());
            assert_eq!(db.attempt_fixture("unpublished")?.unwrap()["released"], true);
            Ok(())
        }).await.unwrap();
    }

    #[tokio::test]
    async fn forgetting_retired_identities_does_not_decode_the_lifetime_ledger() {
        use crate::run_fixture::DbRunFixture;
        let root = tempfile::tempdir().unwrap();
        let store = crate::store::Store::open(root.path()).unwrap();
        store
            .transaction(|db| {
                db.add_run_fixture(
                    &json!({
                        "id": "run",
                        "taskId": "chat",
                        "status": "succeeded",
                        "createdAt": 0,
                    }),
                    None,
                )?;
                db.save_conversation_boundary(&json!({ "id": "chat", "runId": "run" }))?;
                let [_, _, used] = crate::chats::side_question_prefixes("chat");
                for index in 0..512 {
                    db.set(&format!("{used}{index}"), &json!(true), None)?;
                }
                // Erasure needs only this namespace's keys, not its boolean payloads.
                // A malformed opaque sentinel catches implementations that load/decode
                // the entire lifetime ledger before removing its first record.
                db.0.execute(
                    "INSERT INTO kv VALUES(?, 'not-json', NULL)",
                    [format!("{used}opaque")],
                )?;
                db.set(&format!("{used}expired"), &json!(true), Some(0))?;
                let neighbor = "chat-side-question-used:chat-other:retired";
                db.set(neighbor, &json!(true), None)?;
                forget(db, "chat", "run")?;
                let remaining: i64 = db.0.query_row(
                    "SELECT COUNT(*) FROM kv WHERE key>=?1 AND key<?2",
                    rusqlite::params![used, format!("{used}\u{10ffff}")],
                    |row| row.get(0),
                )?;
                assert_eq!(remaining, 0);
                assert_eq!(db.kv(neighbor)?, Some(json!(true)));
                Ok(())
            })
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn forgetting_a_conversation_removes_its_run_records() {
        use crate::run_fixture::DbRunFixture;
        let root = tempfile::tempdir().unwrap();
        let store = crate::store::Store::open(root.path()).unwrap();
        store
            .transaction(|db| {
                db.add_run_fixture(&json!({ "id": "run", "taskId": "chat", "status": "succeeded", "createdAt": 0 }), None)?;
                db.save_conversation_boundary(&json!({ "id": "chat", "runId": "run" }))?;
                let keys = [
                    "turn-speed:run",
                    "chat-error:chat",
                    "chat-title-pending:chat",
                    "chat-title-checked:chat",
                    "chat-title-manual:chat",
                ];
                for key in keys {
                    db.set(key, &json!({ "messageId": "message" }), None)?;
                }
                forget(db, "chat", "run")?;
                for key in keys {
                    assert!(db.kv(key)?.is_none(), "{key}");
                }
                Ok(())
            })
            .await
            .unwrap();
    }
}

#[cfg(test)]
mod retirement_tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    #[tokio::test]
    async fn retired_disk_requires_the_exact_durable_acknowledgement() {
        let requests = Arc::new(tokio::sync::Mutex::new(Vec::new()));
        let count = Arc::new(AtomicUsize::new(0));
        let observed = requests.clone();
        let app = axum::Router::new().fallback(move |axum::Json(body): axum::Json<Value>| {
            let (count, observed) = (count.clone(), observed.clone());
            async move {
                observed.lock().await.push(body.clone());
                let reply = match count.fetch_add(1, Ordering::SeqCst) {
                    0 => json!({ "deleted": true }),
                    1 => json!({ "deleted": true, "retirementId": crate::config::id() }),
                    2 => json!({ "deleted": false, "retirementId": body["retirementId"] }),
                    _ => json!({ "deleted": true, "retirementId": body["retirementId"] }),
                };
                axum::Json(reply)
            }
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let root = tempfile::tempdir().unwrap();
        let mut config = crate::common::config(root.path());
        config.runner_url = origin;
        let service = Service::new(config).await.unwrap();
        let run = crate::config::id();
        let retirement = crate::config::id();
        let node = crate::nodes::LOCAL_NODE_ID;
        let volume = json!({ "id": format!("{run}:{node}"), "runId": run, "nodeId": node });
        let volume_id = text(&volume, "id").to_owned();
        service
            .store
            .write(move |db| db.put("node-volumes", &volume))
            .await
            .unwrap();
        for _ in 0..3 {
            assert!(
                delete_retired_disk(&service, &run, node, false, &retirement)
                    .await
                    .is_err()
            );
            assert!(
                service
                    .store
                    .get("node-volumes", &volume_id)
                    .await
                    .unwrap()
                    .is_some()
            );
        }
        delete_retired_disk(&service, &run, node, false, &retirement)
            .await
            .unwrap();
        assert!(
            service
                .store
                .get("node-volumes", &volume_id)
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(
            *requests.lock().await,
            vec![json!({ "retirementId": retirement }); 4]
        );
        server.abort();
        let _ = server.await;
    }
}
