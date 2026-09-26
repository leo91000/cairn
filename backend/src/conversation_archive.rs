//! Durable archival jobs. The chat record is the journal; each destructive step
//! follows a verified object and can be replayed after a process restart.
use crate::{
    archive_storage::{self, Storage},
    config::{id, now},
    conversation_lifecycle::{DAY, state},
    error::{Error, Result, required},
    service::Service,
    validation::text,
};
use serde_json::{Value, json};
use std::{
    io::{BufRead, Write},
    path::{Path, PathBuf},
};

fn prefix(chat: &Value) -> String {
    format!("leo-conversations/{}/", text(chat, "id"))
}
async fn remove(path: &Path) -> Result<()> {
    match tokio::fs::symlink_metadata(path).await {
        Ok(meta) if meta.is_dir() => tokio::fs::remove_dir_all(path).await?,
        Ok(_) => tokio::fs::remove_file(path).await?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    Ok(())
}
pub(crate) async fn storage(s: &Service, chat: &Value) -> Result<Storage> {
    let storage = Storage::configured(s)?;
    if chat["archiveBucket"]
        .as_str()
        .is_some_and(|b| b != storage.bucket)
    {
        return Err(Error::new(
            409,
            "The archive's bucket differs from the server configuration. Restore the original configuration.",
        ));
    }
    Ok(storage)
}
async fn disk(
    s: &Service,
    run: &str,
    action: &str,
    staging: &Path,
    destination: Option<&str>,
    runtime: Option<&Value>,
) -> Result<()> {
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
            return Err(Error::new(503, "The VM node is unavailable."));
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
    let remote = node != crate::nodes::LOCAL_NODE_ID;
    let transfer = staging.file_name().and_then(|v| v.to_str()).unwrap_or("");
    let result = async {
        if remote && action == "import" {
            crate::nodes::archive::transfer(s, &base, &credential, staging, true).await?;
        }
        let response = s
            .http
            .post(format!("{base}/disks/{run}/{action}"))
            .bearer_auth(&credential)
            .json(&json!({"transfer":transfer,"runtimeId":runtime}))
            .timeout(std::time::Duration::from_secs(7200))
            .send()
            .await
            .map_err(|_| Error::new(503, "Workspace transfer interrupted."))?;
        if !response.status().is_success() {
            return Err(Error::new(
                503,
                "Workspace transfer failed or the agent has not stopped yet.",
            ));
        }
        let value: Value = response.json().await.map_err(Error::internal)?;
        if remote && action == "export" && value["archivePresent"] == true {
            crate::nodes::archive::transfer(s, &base, &credential, staging, false).await?;
        }
        if action == "delete" {
            let id = format!("{run}:{}", node);
            s.store
                .write(move |db| db.remove("node-volumes", &id))
                .await?;
        }
        Ok(())
    }
    .await;
    if remote && action != "delete" {
        let _ = s
            .http
            .delete(format!("{base}/archive-transfers/{transfer}/discard"))
            .bearer_auth(&credential)
            .timeout(std::time::Duration::from_secs(20))
            .send()
            .await;
    }
    result
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
        return disk(s, run, "delete", Path::new("unused"), None, None).await;
    }
    for volume in volumes {
        disk(
            s,
            run,
            "delete",
            Path::new("unused"),
            Some(text(&volume, "nodeId")),
            None,
        )
        .await?;
    }
    Ok(())
}
async fn restore_workspace(
    s: &Service,
    chat: &Value,
    metadata: &mut Value,
    staging: &Path,
) -> Result<()> {
    let run_id = text(chat, "runId");
    let checkpoint_key = format!("run-checkpoint:{run_id}");
    let old = metadata["keys"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|entry| entry[0] == checkpoint_key)
        .map(|v| v[1].clone())
        .unwrap_or_default();
    let mut selecting = metadata["run"].clone();
    selecting["isolated"] = true.into();
    selecting["placementTransition"] = true.into();
    selecting["preferredNodeId"] = old["nodeId"].clone();
    selecting["requiredRuntime"] = old["runtimeId"].clone();
    let reservation = id();
    let selected = crate::nodes::placement::reserve(s, &selecting, &reservation).await?;
    let node = text(&selected, "nodeId");
    let result = async {
        crate::nodes::placement::materialize(s, &reservation).await?;
        disk(
            s,
            run_id,
            "import",
            staging,
            Some(node),
            Some(&old["runtimeId"]),
        )
        .await
    }
    .await;
    crate::nodes::placement::release(s, &reservation).await?;
    result?;
    metadata["run"]["nodeId"] = node.into();
    for entry in metadata["keys"].as_array_mut().into_iter().flatten() {
        if entry[0] == checkpoint_key {
            entry[1]["nodeId"] = node.into();
            entry[1]["process"] = Value::Null;
        }
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
            return Err(Error::bad("Invalid archived worktree path."));
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
                    .envs(std::env::vars().filter(|(key, _)| !crate::process::archive_key(key)))
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .kill_on_drop(true)
                    .status()
                    .await?;
                if !status.success() {
                    return Err(Error::new(
                        503,
                        "Unable to detach an archived worktree; cleanup will retry.",
                    ));
                }
            }
        }
    }
    Ok(())
}

async fn files(s: &Service, chat: &Value, purge: bool) -> Result<()> {
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
        if purge || artifact["visibility"] != "public" {
            let id = text(&artifact, "id");
            crate::validation::uuid(id)?;
            for name in [id.to_owned(), format!("{id}.jpg")] {
                remove(&s.config.data_dir.join("artifacts").join(name)).await?;
            }
        }
    }
    Ok(())
}
async fn normalize_legacy_workspace(s: &Service, chat: &Value) -> Result<()> {
    let run_id = text(chat, "runId");
    if run_id.is_empty() {
        return Ok(());
    }
    let run = s.store.run(run_id).await?;
    s.accounts.recover_run(s, &run).await?;
    let key = format!("run-checkpoint:{run_id}");
    let Some(mut checkpoint) = s.store.kv(&key).await? else {
        return Ok(());
    };
    if !checkpoint["prepared"].is_object() || checkpoint["prepared"]["backend"] == "firecracker" {
        return Ok(());
    }
    if s.config.runner_url.is_empty() {
        if checkpoint["prepared"]["workspaces"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|w| ["direct", "worktree"].contains(&text(w, "kind")))
        {
            return Err(Error::new(
                409,
                "Configure the Firecracker runner to archive this legacy shared workspace without losing its files.",
            ));
        }
        return Ok(());
    }
    // Reuse the existing migration: it preserves dirty files, commits and native
    // sessions in independent run-owned clones, without changing shared projects.
    let github = s
        .store
        .kv(&format!(
            "agent-github:{}",
            text(&run["snapshot"]["agent"], "id")
        ))
        .await?;
    let prepared = crate::execution::restore(
        &run,
        checkpoint["prepared"].clone(),
        &s.config,
        github.as_ref().and_then(Value::as_str),
    )
    .await?;
    checkpoint["prepared"] = prepared.clone();
    let cid = text(chat, "id").to_owned();
    let rid = run_id.to_owned();
    s.store.transaction(move |db| {
        let mut current = required(db.get("chats",&cid)?,"Chat not found")?;
        current["legacyWorkspaces"] = run["workspaces"].clone();
        current["legacyProjects"] = crate::service::run_projects(&run).into();
        db.put("chats",&current)?;
        db.set(&key,&checkpoint,None)?;
        db.patch_run(&rid,&json!({"workspace":prepared["cwd"],"workspaces":prepared["workspaces"],"isolated":prepared["isolated"]}))?;
        Ok(())
    }).await
}

async fn metadata(s: &Service, chat: &Value, staging: PathBuf) -> Result<()> {
    let chat = chat.clone();
    s.store.read(move |db| {
        let run = text(&chat,"runId");
        let mut keys = Vec::new();
        for prefix in [format!("chat-question:{}:",text(&chat,"id")),format!("chat-attachment:{}:",text(&chat,"id"))] { keys.extend(db.keys(&prefix)?); }
        if let Some(checkpoint) = db.kv(&format!("run-checkpoint:{run}"))? { keys.push((format!("run-checkpoint:{run}"),checkpoint)); }
        let value = json!({"version":1,"chatId":chat["id"],"run":db.run(run)?,"messages":db.messages(text(&chat,"id"))?,"keys":keys});
        let mut output = std::fs::File::create(staging.join("metadata.json"))?;
        serde_json::to_writer(&mut output,&value)?; output.sync_all()?;
        let mut output = std::io::BufWriter::new(std::fs::File::create(staging.join("events.jsonl"))?);
        let mut stmt = db.0.prepare("SELECT created_at,type,text,payload FROM events WHERE run_id=? ORDER BY id")?;
        let mut rows = stmt.query([run])?;
        while let Some(row) = rows.next()? {
            let value = json!({"at":row.get::<_,i64>(0)?,"type":row.get::<_,String>(1)?,"text":row.get::<_,String>(2)?,"payload":row.get::<_,Option<String>>(3)?});
            serde_json::to_writer(&mut output,&value)?; output.write_all(b"\n")?;
        }
        output.flush()?; output.get_ref().sync_all()?;
        Ok(())
    }).await
}

pub async fn archive(s: &Service, mut chat: Value) -> Result<()> {
    let store = storage(s, &chat).await?;
    let chat_id = text(&chat, "id").to_owned();
    let _previews = crate::artifacts::preview::JOBS
        .acquire()
        .await
        .map_err(Error::internal)?;
    if chat["archivePhase"] != "verified" {
        let cid = chat_id.clone();
        chat = s
            .store
            .transaction(move |db| {
                let mut current = required(db.get("chats", &cid)?, "Chat not found")?;
                // A child upload can outlive a killed manager. Every retry writes a
                // new immutable key so that old processes cannot overwrite a verified object.
                current["archiveKey"] = format!("leo-conversations/{cid}/{}.enc", id()).into();
                db.put("chats", &current)
            })
            .await?;
    }
    if chat["archivePhase"] != "verified" {
        let transfers = s.config.data_dir.join("archive-transfers");
        crate::skills::private_dir(&transfers).await?;
        let staging = transfers.join(id());
        crate::skills::private_dir(&staging).await?;
        // Durable jobs rebuild partial uploads; temporary plaintext is restricted
        // to this private directory and cleaned before each retry/startup.
        let result = async {
            normalize_legacy_workspace(s, &chat).await?;
            metadata(s, &chat, staging.clone()).await?;
            disk(s, text(&chat, "runId"), "export", &staging, None, None).await?;
            let plain = staging.join("snapshot.tar.gz");
            let mut args = vec![
                "--sparse".into(),
                "-czf".into(),
                plain.to_string_lossy().into(),
                "-C".into(),
                staging.to_string_lossy().into(),
                "metadata.json".into(),
                "events.jsonl".into(),
            ];
            if staging.join("workspace.tar.gz").exists() {
                args.push("workspace.tar.gz".into());
            }
            let mut local_files = Vec::new();
            for relative in [
                format!("runs/{}", text(&chat, "runId")),
                format!("chat-attachments/{chat_id}"),
            ] {
                if relative != "runs/" && s.config.data_dir.join(&relative).exists() {
                    local_files.push(relative);
                }
            }
            for (_, artifact) in s
                .store
                .keys(&format!("artifact:{}:", text(&chat, "runId")))
                .await?
            {
                let id = text(&artifact, "id");
                crate::validation::uuid(id)?;
                for name in [format!("artifacts/{id}"), format!("artifacts/{id}.jpg")] {
                    if s.config.data_dir.join(&name).exists() {
                        local_files.push(name);
                    }
                }
            }
            if !local_files.is_empty() {
                args.extend(["-C".into(), s.config.data_dir.to_string_lossy().into()]);
                args.extend(local_files);
            }
            archive_storage::tar(args).await?;
            let encrypted = staging.join("snapshot.enc");
            archive_storage::crypt(
                s,
                plain,
                encrypted.clone(),
                text(&chat, "archiveKey").into(),
                false,
            )
            .await?;
            let digest = archive_storage::hash(encrypted.clone()).await?;
            store.upload(&encrypted, text(&chat, "archiveKey")).await?;
            let verified = staging.join("verify.enc");
            store.download(text(&chat, "archiveKey"), &verified).await?;
            if archive_storage::hash(verified).await? != digest {
                return Err(Error::new(
                    503,
                    "Archive verification failed. Local data has been retained.",
                ));
            }
            let cid = chat_id.clone();
            let expected_key = chat["archiveKey"].clone();
            s.store
                .transaction(move |db| {
                    let mut current = required(db.get("chats", &cid)?, "Chat not found")?;
                    if current["archiveKey"] != expected_key {
                        return Err(Error::new(409, "This archive attempt has been superseded."));
                    }
                    current["archiveDigest"] = digest.into();
                    current["archivePhase"] = "verified".into();
                    // A deletion during upload wins; the verified archive is now
                    // part of the trash, never an instruction to reactivate it.
                    db.put("chats", &current)
                })
                .await
        }
        .await;
        remove(&staging).await?;
        chat = result?;
    }
    delete_disks(s, text(&chat, "runId")).await?;
    files(s, &chat, false).await?;
    let cid = chat_id;
    s.store.transaction(move |db| {
        let mut current = required(db.get("chats",&cid)?,"Chat not found")?;
        let run = text(&current,"runId");
        db.0.execute("DELETE FROM events WHERE run_id=?",[run])?;
        db.0.execute("DELETE FROM chat_messages WHERE chat_id=?",[&cid])?;
        for prefix in [format!("chat-question:{cid}:"),format!("chat-attachment:{cid}:")] { for (key,_) in db.keys(&prefix)? { db.delete(&key)?; } }
        db.delete(&format!("run-checkpoint:{run}"))?;
        if let Some(value) = db.run(run)? {
            let retained = json!({"id":value["id"],"status":value["status"],"createdAt":value["createdAt"],"finishedAt":value["finishedAt"],"archived":true});
            db.0.execute("UPDATE runs SET data=? WHERE id=?",rusqlite::params![retained.to_string(),run])?;
        }
        if state(&current) == "trash" { current["previousLifecycle"] = "archived".into(); }
        else { current["lifecycle"] = "archived".into(); }
        current["archivedAt"] = now().into(); current["storageClass"] = "STANDARD".into(); current["lifecycleError"] = Value::Null;
        db.put("chats",&current)?;
        Ok(())
    }).await
}

pub async fn restore(s: &Service, chat: Value, days: i64) -> Result<()> {
    let store = storage(s, &chat).await?;
    if !store.ready(text(&chat, "archiveKey")).await? {
        let cid = text(&chat, "id").to_owned();
        s.store
            .transaction(move |db| {
                if let Some(mut current) = db.get("chats", &cid)?
                    && state(&current) == "restoring"
                {
                    current["retryAfter"] = (now() + 60_000).into();
                    db.put("chats", &current)?;
                }
                Ok(())
            })
            .await?;
        return Ok(());
    }
    let transfers = s.config.data_dir.join("archive-transfers");
    crate::skills::private_dir(&transfers).await?;
    let staging = transfers.join(id());
    crate::skills::private_dir(&staging).await?;
    let result = async {
        let encrypted = staging.join("snapshot.enc");
        store
            .download(text(&chat, "archiveKey"), &encrypted)
            .await?;
        if archive_storage::hash(encrypted.clone()).await? != text(&chat, "archiveDigest") {
            return Err(Error::bad(
                "Archive checksum mismatch; restoration stopped.",
            ));
        }
        let plain = staging.join("snapshot.tar.gz");
        archive_storage::crypt(
            s,
            encrypted,
            plain.clone(),
            text(&chat, "archiveKey").into(),
            true,
        )
        .await?;
        let extracted = staging.join("extracted");
        crate::skills::private_dir(&extracted).await?;
        archive_storage::tar(vec![
            "--no-same-owner".into(),
            "-xzf".into(),
            plain.to_string_lossy().into(),
            "-C".into(),
            extracted.to_string_lossy().into(),
        ])
        .await?;
        let mut value: Value =
            serde_json::from_slice(&tokio::fs::read(extracted.join("metadata.json")).await?)?;
        if value["version"] != 1 || value["chatId"] != chat["id"] {
            return Err(Error::bad("Incompatible archive format."));
        }
        let current = s.get("chats", text(&chat, "id")).await?;
        if state(&current) != "restoring" {
            return Ok(());
        }
        if extracted.join("workspace.tar.gz").exists() {
            tokio::fs::rename(
                extracted.join("workspace.tar.gz"),
                staging.join("workspace.tar.gz"),
            )
            .await?;
            restore_workspace(s, &chat, &mut value, &staging).await?;
        }
        // No writers can enter while restoring. Partial local copies can be
        // replaced on retry; authoritative public artifact metadata stays live.
        for relative in [
            format!("runs/{}", text(&chat, "runId")),
            format!("chat-attachments/{}", text(&chat, "id")),
        ] {
            let source = extracted.join(&relative);
            if source.exists() {
                let target = s.config.data_dir.join(&relative);
                remove(&target).await?;
                crate::skills::private_dir(target.parent().unwrap()).await?;
                tokio::fs::rename(source, target).await?;
            }
        }
        let artifacts = extracted.join("artifacts");
        if artifacts.exists() {
            crate::skills::private_dir(&s.config.data_dir.join("artifacts")).await?;
            let mut entries = tokio::fs::read_dir(artifacts).await?;
            while let Some(entry) = entries.next_entry().await? {
                let target = s.config.data_dir.join("artifacts").join(entry.file_name());
                if !target.exists() {
                    tokio::fs::rename(entry.path(), target).await?;
                }
            }
        }
        let cid = text(&chat, "id").to_owned();
        s.store
            .transaction(move |db| {
                let mut current = required(db.get("chats", &cid)?, "Chat not found")?;
                if state(&current) != "restoring" {
                    return Ok(());
                }
                let run = text(&current, "runId");
                if !value["run"].is_null() {
                    db.patch_run(run, &value["run"])?;
                }
                for message in value["messages"].as_array().into_iter().flatten() {
                    db.put_message(message)?;
                }
                for item in value["keys"].as_array().into_iter().flatten() {
                    // These keys never include grants or public links.
                    db.set(
                        item[0]
                            .as_str()
                            .ok_or_else(|| Error::bad("Invalid archive key"))?,
                        &item[1],
                        None,
                    )?;
                }
                db.0.execute("DELETE FROM events WHERE run_id=?", [run])?;
                for line in
                    std::io::BufReader::new(std::fs::File::open(extracted.join("events.jsonl"))?)
                        .lines()
                {
                    let event: Value = serde_json::from_str(&line?)?;
                    db.0.execute(
                        "INSERT INTO events(run_id,created_at,type,text,payload) VALUES(?,?,?,?,?)",
                        rusqlite::params![
                            run,
                            event["at"].as_i64(),
                            text(&event, "type"),
                            text(&event, "text"),
                            event["payload"].as_str()
                        ],
                    )?;
                }
                current["lifecycle"] = "active".into();
                current["paused"] = true.into();
                current["archiveNotBefore"] = (now() + days * DAY).into();
                current["restoredAt"] = now().into();
                current["lifecycleError"] = Value::Null;
                db.put("chats", &current)?;
                Ok(())
            })
            .await
    }
    .await;
    remove(&staging).await?;
    result
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
    if !text(&chat, "archiveKey").is_empty() {
        storage(s, &chat).await?.purge(&prefix(&chat)).await?;
    }
    delete_disks(s, &run).await?;
    files(s, &chat, true).await?;
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
            db.audit("chat.purged", &json!({"id":cid}))?;
            Ok(())
        })
        .await
}

pub async fn retire(s: &Service, chat: Value) -> Result<()> {
    storage(s, &chat).await?.purge(&prefix(&chat)).await?;
    let cid = text(&chat, "id").to_owned();
    s.store
        .transaction(move |db| {
            let mut current = required(db.get("chats", &cid)?, "Chat not found")?;
            for key in [
                "archiveKey",
                "archiveBucket",
                "archivePhase",
                "archiveDigest",
                "storageClass",
                "archivedAt",
            ] {
                current[key] = Value::Null;
            }
            db.put("chats", &current)?;
            Ok(())
        })
        .await
}
