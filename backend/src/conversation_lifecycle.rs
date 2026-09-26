//! Conversation retention is independent from execution status and task archival.
use crate::{
    config::now,
    error::{Error, Result, required},
    service::Service,
    validation::text,
};
use serde_json::{Value, json};

pub type StorageLock = crate::file_lock::Guard;

pub fn storage_lock(directory: &std::path::Path) -> Result<StorageLock> {
    crate::file_lock::exclusive(
        &directory.join("conversation-storage.lock"),
        "A conversation storage operation is in progress. Retry shortly.",
    )
}

pub const DAY: i64 = 86_400_000;

pub fn state(chat: &Value) -> &str {
    chat["lifecycle"].as_str().unwrap_or("active")
}

pub fn require_active(chat: &Value) -> Result<()> {
    if state(chat) != "active" {
        return Err(Error::new(
            409,
            "Restore this conversation before continuing.",
        ));
    }
    Ok(())
}

pub fn require_active_run(db: &crate::store::Db<'_>, run: &str) -> Result<()> {
    for chat in db.json_rows(
        "SELECT data FROM records WHERE kind='chats' AND json_extract(data,'$.runId')=?",
        [run],
    )? {
        require_active(&chat)?;
    }
    Ok(())
}

pub fn in_view(chat: &Value, view: &str) -> bool {
    match view {
        "active" => state(chat) == "active",
        "archives" => matches!(state(chat), "archiving" | "archived" | "restoring"),
        "trash" => matches!(state(chat), "trash" | "purging"),
        _ => false,
    }
}

impl Service {
    pub async fn chat_list_view(&self, view: &str) -> Result<Vec<Value>> {
        if !["active", "archives", "trash"].contains(&view) {
            return Err(Error::bad("Choose active, archives or trash."));
        }
        let view = view.to_owned();
        self.store
            .read(move |db| {
                Ok(crate::chats::list_all(db)?
                    .into_iter()
                    .filter(|chat| in_view(chat, &view))
                    .collect())
            })
            .await
    }

    pub async fn chat_trash(&self, id: &str, confirmed: bool) -> Result<Value> {
        crate::validation::uuid(id)?;
        let id = id.to_owned();
        let result = self.store.transaction(move |db| {
            let mut chat = required(db.get("chats", &id)?, "Chat not found")?;
            if matches!(state(&chat), "trash" | "purging") { return Ok(chat); }
            let run = db.run(text(&chat, "runId"))?;
            let messages = db.messages(&id)?;
            let questions = db.keys(&format!("chat-question:{id}:"))?;
            let busy = run.as_ref().is_some_and(|r| ["queued", "running"].contains(&text(r, "status")))
                || messages.iter().any(|m| ["queued", "sending"].contains(&text(m, "status")))
                || questions.iter().any(|(_, q)| ["pending", "answering"].contains(&text(q, "status")));
            if busy && !confirmed {
                return Err(Error::new(409, "Confirm deletion to stop the agent and cancel pending messages and questions."));
            }
            for mut message in messages {
                if ["queued", "sending"].contains(&text(&message, "status")) {
                    message["status"] = "cancelled".into();
                    db.put_message(&message)?;
                }
            }
            for (key, mut question) in questions {
                if ["pending", "answering"].contains(&text(&question, "status")) {
                    question["status"] = "cancelled".into();
                    question["blocking"] = false.into();
                    db.set(&key, &question, None)?;
                }
            }
            for (key, mut artifact) in db.keys(&format!("artifact:{}:", text(&chat, "runId")))? {
                if let Some(token) = artifact["publicToken"].as_str() { db.delete(&format!("artifact-share:{token}"))?; }
                artifact["visibility"] = "private".into();
                artifact["publicToken"] = Value::Null;
                artifact["publicUrl"] = Value::Null;
                db.set(&key, &artifact, None)?;
            }
            db.delete(&format!("chat-title-pending:{id}"))?;
            if let Some(run) = run && ["queued", "running"].contains(&text(&run, "status")) {
                db.patch_run(text(&run, "id"), &json!({"cancelRequestedAt":now()}))?;
            }
            chat["cancelledByDeletion"] = busy.into();
            for prefix in ["push-outbox:", "mcp-grant:"] {
                for (key, value) in db.keys(prefix)? {
                    if value["chatId"] == id || (chat["runId"].is_string() && value["runId"] == chat["runId"]) { db.delete(&key)?; }
                }
            }
            chat["previousLifecycle"] = state(&chat).into();
            chat["lifecycle"] = "trash".into();
            chat["trashedAt"] = now().into();
            chat["purgeAt"] = (now() + 30 * DAY).into();
            chat["paused"] = true.into();
            db.set("conversation-cache-revision", &crate::config::id().into(), None)?;
            db.audit("chat.trashed", &json!({"id":id}))?;
            db.put("chats", &chat)
        }).await?;
        if let Some(run) = result["runId"].as_str() {
            match self.worker.cancel(self, run).await {
                Ok(()) => {}
                Err(error) if error.status == 409 => {}
                Err(error) => return Err(error),
            }
        }
        Ok(result)
    }

    pub async fn chat_restore(&self, id: &str) -> Result<Value> {
        let _process_lock = storage_lock(&self.config.data_dir)?;
        crate::validation::uuid(id)?;
        let _guard = self
            .retention_lock
            .try_lock()
            .map_err(|_| Error::new(409, "A storage operation is in progress. Retry shortly."))?;
        let id = id.to_owned();
        self.store
            .transaction(move |db| {
                let mut chat = required(db.get("chats", &id)?, "Chat not found")?;
                if ["archived", "restoring"].contains(&state(&chat)) {
                    chat["lifecycle"] = "restoring".into();
                    chat["lifecycleError"] = Value::Null;
                    chat["retryAfter"] = Value::Null;
                    return db.put("chats", &chat);
                }
                if state(&chat) != "trash" {
                    return Err(Error::new(409, "This conversation is not in the trash."));
                }
                if chat["purgeAt"].as_i64().is_none_or(|at| at <= now()) {
                    return Err(Error::new(410, "This conversation has expired."));
                }
                if db
                    .run(text(&chat, "runId"))?
                    .is_some_and(|r| ["queued", "running"].contains(&text(&r, "status")))
                {
                    return Err(Error::new(409, "Wait for the agent to finish stopping."));
                }
                chat["lifecycle"] = if text(&chat, "previousLifecycle") == "archived"
                    || (matches!(text(&chat, "previousLifecycle"), "archiving" | "restoring")
                        && chat["archivePhase"] == "verified")
                {
                    "archived"
                } else {
                    "active"
                }
                .into();
                if state(&chat) == "active" {
                    chat["archiveNotBefore"] =
                        (now() + policy(db)?["inactivityDays"].as_i64().unwrap_or(30) * DAY).into();
                }
                chat["trashedAt"] = Value::Null;
                chat["purgeAt"] = Value::Null;
                chat["previousLifecycle"] = Value::Null;
                chat["retryAfter"] = Value::Null;
                chat["lifecycleError"] = Value::Null;
                db.audit("chat.recovered", &json!({"id":id}))?;
                db.put("chats", &chat)
            })
            .await
    }
}

fn policy(db: &crate::store::Db<'_>) -> Result<Value> {
    Ok(db
        .kv("conversation-retention")?
        .unwrap_or_else(|| json!({"enabled":false,"inactivityDays":30,"coldAfterDays":90})))
}

fn eligible(
    db: &crate::store::Db<'_>,
    chat: &Value,
    days: i64,
    at: i64,
    runner: bool,
) -> Result<bool> {
    if state(chat) != "active" || chat["archiveNotBefore"].as_i64().unwrap_or(0) > at {
        return Ok(false);
    }
    let run = db.run(text(chat, "runId"))?.unwrap_or_default();
    if ["queued", "running"].contains(&text(&run, "status")) {
        return Ok(false);
    }
    if !runner
        && run["workspaces"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|w| ["direct", "worktree"].contains(&text(w, "kind")))
    {
        return Ok(false);
    }
    let last = chat["lastActivityAt"]
        .as_i64()
        .or_else(|| chat["updatedAt"].as_i64())
        .unwrap_or(at)
        .max(run["finishedAt"].as_i64().unwrap_or(0));
    if last > at - days * DAY {
        return Ok(false);
    }
    if db
        .messages(text(chat, "id"))?
        .iter()
        .any(|m| ["queued", "sending"].contains(&text(m, "status")))
    {
        return Ok(false);
    }
    Ok(!db
        .keys(&format!("chat-question:{}:", text(chat, "id")))?
        .iter()
        .any(|(_, q)| ["pending", "answering"].contains(&text(q, "status"))))
}

impl Service {
    pub async fn retention_policy(&self) -> Result<Value> {
        self.retention_preview(None).await
    }
    pub async fn retention_preview(&self, days: Option<i64>) -> Result<Value> {
        let configured = crate::archive_storage::Storage::configured(self).is_ok();
        let runner = !self.config.runner_url.is_empty();
        self.store
            .read(move |db| {
                let mut value = policy(db)?;
                if let Some(days) = days {
                    value["inactivityDays"] = days.into();
                }
                let mut count = 0;
                for chat in db.list("chats")? {
                    if eligible(
                        db,
                        &chat,
                        value["inactivityDays"].as_i64().unwrap_or(30),
                        now(),
                        runner,
                    )? {
                        count += 1;
                    }
                }
                value["eligible"] = count.into();
                value["configured"] = configured.into();
                Ok(value)
            })
            .await
    }
    pub async fn retention_save(&self, input: Value) -> Result<Value> {
        let enabled = input["enabled"]
            .as_bool()
            .ok_or_else(|| Error::bad("enabled must be a boolean."))?;
        let days = input["inactivityDays"]
            .as_i64()
            .filter(|n| (1..=3650).contains(n))
            .ok_or_else(|| Error::bad("Inactivity must be between 1 and 3650 days."))?;
        let cold = input["coldAfterDays"]
            .as_i64()
            .filter(|n| (1..=3650).contains(n))
            .ok_or_else(|| Error::bad("S3 retention must be between 1 and 3650 days."))?;
        if enabled {
            crate::archive_storage::Storage::configured(self)?
                .validate()
                .await?;
        }
        if enabled && input["confirmExisting"] != true {
            return Err(Error::new(
                409,
                "Review the eligible conversation count and confirm activation.",
            ));
        }
        self.store
            .set(
                "conversation-retention",
                json!({"enabled":enabled,"inactivityDays":days,"coldAfterDays":cold}),
                None,
            )
            .await?;
        self.retention_policy().await
    }
}

impl Service {
    pub async fn retention_tick(&self) -> Result<()> {
        let _process_lock = match storage_lock(&self.config.data_dir) {
            Ok(lock) => lock,
            Err(error) if error.status == 409 => return Ok(()),
            Err(error) => return Err(error),
        };
        let Ok(_guard) = self.retention_lock.try_lock() else {
            return Ok(());
        };
        let settings = self.retention_policy().await?;
        let days = settings["inactivityDays"].as_i64().unwrap_or(30);
        let runner = !self.config.runner_url.is_empty();
        let cold = settings["coldAfterDays"].as_i64().unwrap_or(90);
        // Clean abandoned transfer directories before retrying durable jobs.
        let transfers = self.config.data_dir.join("archive-transfers");
        if transfers.exists() {
            tokio::fs::remove_dir_all(&transfers).await?;
        }
        let mut chats = self.store.list("chats").await?;
        chats.sort_by_key(|chat| chat["retryAfter"].as_i64().unwrap_or(0));
        for chat in chats {
            if chat["retryAfter"].as_i64().unwrap_or(0) > now() {
                continue;
            }
            let id = text(&chat, "id").to_owned();
            let result = match state(&chat) {
                "trash" | "purging" if chat["purgeAt"].as_i64().unwrap_or(i64::MAX) <= now() => {
                    let cid = id.clone();
                    let purging = self
                        .store
                        .transaction(move |db| {
                            let mut current = required(db.get("chats", &cid)?, "Chat not found")?;
                            if state(&current) != "trash" && state(&current) != "purging" {
                                return Ok(None);
                            }
                            if db.run(text(&current, "runId"))?.is_some_and(|r| {
                                ["queued", "running"].contains(&text(&r, "status"))
                            }) {
                                return Ok(None);
                            }
                            current["lifecycle"] = "purging".into();
                            db.put("chats", &current)?;
                            Ok(Some(current))
                        })
                        .await?;
                    if let Some(chat) = purging {
                        crate::conversation_archive::purge(self, chat).await
                    } else {
                        continue;
                    }
                }
                "restoring" => crate::conversation_archive::restore(self, chat, days).await,
                "archiving" => crate::conversation_archive::archive(self, chat).await,
                "archived"
                    if chat["storageClass"] != "GLACIER"
                        && chat["archivedAt"].as_i64().unwrap_or(now()) + cold * DAY <= now()
                        && settings["enabled"] == true =>
                {
                    let result = async {
                        let storage = crate::conversation_archive::storage(self, &chat).await?;
                        storage.cold(text(&chat, "archiveKey")).await
                    }
                    .await;
                    if result.is_ok() {
                        let cid = id.clone();
                        self.store
                            .transaction(move |db| {
                                let mut current =
                                    required(db.get("chats", &cid)?, "Chat not found")?;
                                current["storageClass"] = "GLACIER".into();
                                db.put("chats", &current)?;
                                Ok(())
                            })
                            .await?;
                    }
                    result
                }
                "active" if !text(&chat, "archiveKey").is_empty() => {
                    crate::conversation_archive::retire(self, chat).await
                }
                "active" if settings["enabled"] == true => {
                    let cid = id.clone();
                    let bucket = crate::archive_storage::Storage::configured(self)?.bucket;
                    let selected = self
                        .store
                        .transaction(move |db| {
                            let mut current = required(db.get("chats", &cid)?, "Chat not found")?;
                            if !eligible(db, &current, days, now(), runner)? {
                                return Ok(None);
                            }
                            db.set(
                                "conversation-cache-revision",
                                &crate::config::id().into(),
                                None,
                            )?;
                            current["agentName"] = db
                                .get("agents", text(&current, "agentId"))?
                                .map(|a| a["name"].clone())
                                .unwrap_or("Deleted agent".into());
                            current["projectName"] = db
                                .get("projects", text(&current, "projectId"))?
                                .map(|p| p["name"].clone())
                                .unwrap_or(Value::Null);
                            current["lifecycle"] = "archiving".into();
                            current["archiveKey"] =
                                format!("leo-conversations/{cid}/{}.enc", crate::config::id())
                                    .into();
                            current["archiveBucket"] = bucket.into();
                            current["archivePhase"] = "upload".into();
                            db.put("chats", &current)?;
                            Ok(Some(current))
                        })
                        .await?;
                    if let Some(chat) = selected {
                        crate::conversation_archive::archive(self, chat).await
                    } else {
                        continue;
                    }
                }
                _ => continue,
            };
            if let Err(error) = result {
                self.store
                    .transaction(move |db| {
                        if let Some(mut current) = db.get("chats", &id)? {
                            current["lifecycleError"] = error.message.into();
                            current["retryAfter"] = (now() + 60_000).into();
                            db.put("chats", &current)?;
                        }
                        Ok(())
                    })
                    .await?;
            }
            // One conversation per pass bounds load, including the existing backlog.
            break;
        }
        Ok(())
    }
}

impl Service {
    pub async fn chat_new_session(&self, id: &str, confirmed: bool) -> Result<Value> {
        if !confirmed {
            return Err(Error::new(
                409,
                "Confirm starting a fresh agent session using the preserved history and files.",
            ));
        }
        let id = id.to_owned();
        self.store
            .transaction(move |db| {
                let mut chat = required(db.get("chats", &id)?, "Chat not found")?;
                require_active(&chat)?;
                let run = required(db.run(text(&chat, "runId"))?, "Run not found")?;
                if chat["restoredAt"].is_null()
                    || !["failed", "interrupted"].contains(&text(&run, "status"))
                {
                    return Err(Error::new(
                        409,
                        "A fresh session is available after a restored session fails.",
                    ));
                }
                let key = format!("run-checkpoint:{}", text(&run, "id"));
                let mut checkpoint = required(db.kv(&key)?, "Workspace checkpoint not found")?;
                checkpoint["freshSession"] = true.into();
                checkpoint["launched"] = false.into();
                checkpoint
                    .as_object_mut()
                    .unwrap()
                    .remove("controllerRecoveries");
                db.set(&key, &checkpoint, None)?;
                db.patch_run(
                    text(&run, "id"),
                    &json!({"sessionId":null,"resumeAvailable":false}),
                )?;
                chat["sessionRestartRequested"] = true.into();
                chat["paused"] = true.into();
                db.put("chats", &chat)
            })
            .await
    }
}
