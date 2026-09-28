//! Conversation deletion is independent from execution status and task archival.
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
        "trash" => matches!(state(chat), "trash" | "purging"),
        _ => false,
    }
}

impl Service {
    pub async fn chat_list_view(&self, view: &str) -> Result<Vec<Value>> {
        if !["active", "trash"].contains(&view) {
            return Err(Error::bad("Choose active or trash."));
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
            .conversation_storage_lock
            .try_lock()
            .map_err(|_| Error::new(409, "A storage operation is in progress. Retry shortly."))?;
        let id = id.to_owned();
        self.store
            .transaction(move |db| {
                let mut chat = required(db.get("chats", &id)?, "Chat not found")?;
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
                chat["lifecycle"] = "active".into();
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

impl Service {
    pub async fn cleanup_conversations(&self) -> Result<()> {
        let _process_lock = match storage_lock(&self.config.data_dir) {
            Ok(lock) => lock,
            Err(error) if error.status == 409 => return Ok(()),
            Err(error) => return Err(error),
        };
        let Ok(_guard) = self.conversation_storage_lock.try_lock() else {
            return Ok(());
        };
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
                        crate::conversation_deletion::purge(self, chat).await
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
