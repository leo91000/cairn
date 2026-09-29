//! Conversation deletion is independent from execution status and task archival.
use crate::{
    chats::{MessageStatus, Question, QuestionStatus},
    config::now,
    error::{Error, Result, required},
    run_status::RunStatus,
    service::Service,
    store::Db,
    validation::{string_enum, text},
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
const TRASH_RETENTION: i64 = 30 * DAY;
const PURGE_RETRY_MS: i64 = 60_000;

string_enum! {
    /// Persisted in a chat's `lifecycle` field; a missing field means active.
    pub enum Lifecycle {
        Active => "active",
        Trash => "trash",
        Purging => "purging",
    }
}

string_enum! {
    /// Conversation list filter accepted by `GET /api/chats?view=`.
    pub enum View {
        Active => "active",
        Trash => "trash",
    }
}

pub fn state(chat: &Value) -> &str {
    chat["lifecycle"]
        .as_str()
        .unwrap_or(Lifecycle::Active.as_str())
}

pub fn is_active(chat: &Value) -> bool {
    state(chat) == Lifecycle::Active.as_str()
}

fn in_trash(chat: &Value) -> bool {
    matches!(
        Lifecycle::parse(state(chat)),
        Some(Lifecycle::Trash | Lifecycle::Purging)
    )
}

pub fn require_active(chat: &Value) -> Result<()> {
    if !is_active(chat) {
        return Err(Error::conflict(
            "Restore this conversation before continuing.",
        ));
    }
    Ok(())
}

pub fn require_active_run(db: &Db<'_>, run: &str) -> Result<()> {
    for chat in db.json_rows(
        "SELECT data FROM records WHERE kind='chats' AND json_extract(data,'$.runId')=?",
        [run],
    )? {
        require_active(&chat)?;
    }
    Ok(())
}

pub fn in_view(chat: &Value, view: &str) -> bool {
    match View::parse(view) {
        Some(View::Active) => is_active(chat),
        Some(View::Trash) => in_trash(chat),
        None => false,
    }
}

fn run_is_active(run: Option<&Value>) -> bool {
    run.and_then(RunStatus::of)
        .is_some_and(RunStatus::is_active)
}

fn message_is_pending(message: &Value) -> bool {
    message["status"] == MessageStatus::Queued || message["status"] == MessageStatus::Sending
}

/// Cancels queued messages and open questions so nothing is delivered after deletion.
fn cancel_pending_input(
    db: &Db<'_>,
    messages: Vec<Value>,
    questions: Vec<(String, Question)>,
) -> Result<()> {
    for mut message in messages {
        if message_is_pending(&message) {
            message["status"] = MessageStatus::Cancelled.into();
            db.put_message(&message)?;
        }
    }
    for (key, mut question) in questions {
        if question.is_open() {
            question.status = QuestionStatus::Cancelled;
            question.blocking = false;
            db.set_as(&key, &question, None)?;
        }
    }
    Ok(())
}

/// Trashed conversations must not stay reachable through public artifact links.
fn unshare_artifacts(db: &Db<'_>, run: &str) -> Result<()> {
    for (key, mut artifact) in db.keys(&format!("artifact:{run}:"))? {
        if let Some(token) = artifact["publicToken"].as_str() {
            db.delete(&format!("artifact-share:{token}"))?;
        }
        artifact["visibility"] = "private".into();
        artifact["publicToken"] = Value::Null;
        artifact["publicUrl"] = Value::Null;
        db.set(&key, &artifact, None)?;
    }
    Ok(())
}

/// Drops pending push notifications and MCP grants tied to the chat or its run.
fn revoke_chat_access(db: &Db<'_>, chat: &Value, id: &str) -> Result<()> {
    for prefix in ["push-outbox:", "mcp-grant:"] {
        for (key, value) in db.keys(prefix)? {
            let same_run = chat["runId"].is_string() && value["runId"] == chat["runId"];
            if value["chatId"] == id || same_run {
                db.delete(&key)?;
            }
        }
    }
    Ok(())
}

fn trash(db: &Db<'_>, id: &str, confirmed: bool) -> Result<Value> {
    let mut chat = required(db.get("chats", id)?, "Chat not found")?;
    if in_trash(&chat) {
        return Ok(chat);
    }
    let run = db.run(text(&chat, "runId"))?;
    let messages = db.messages(id)?;
    let questions = db.keys_as::<Question>(&crate::chats::question_prefix(id))?;
    let busy = run_is_active(run.as_ref())
        || messages.iter().any(message_is_pending)
        || questions.iter().any(|(_, question)| question.is_open());
    if busy && !confirmed {
        return Err(Error::conflict(
            "Confirm deletion to stop the agent and cancel pending messages and questions.",
        ));
    }
    cancel_pending_input(db, messages, questions)?;
    unshare_artifacts(db, text(&chat, "runId"))?;
    db.delete(&format!("chat-title-pending:{id}"))?;
    if let Some(run) = run.filter(|run| run_is_active(Some(run))) {
        db.patch_run(text(&run, "id"), &json!({ "cancelRequestedAt": now() }))?;
    }
    chat["cancelledByDeletion"] = busy.into();
    revoke_chat_access(db, &chat, id)?;
    chat["previousLifecycle"] = state(&chat).into();
    chat["lifecycle"] = Lifecycle::Trash.into();
    chat["trashedAt"] = now().into();
    chat["purgeAt"] = (now() + TRASH_RETENTION).into();
    chat["paused"] = true.into();
    db.set(
        "conversation-cache-revision",
        &crate::config::id().into(),
        None,
    )?;
    db.audit("chat.trashed", &json!({ "id": id }))?;
    db.put("chats", &chat)
}

fn restore(db: &Db<'_>, id: &str) -> Result<Value> {
    let mut chat = required(db.get("chats", id)?, "Chat not found")?;
    if Lifecycle::parse(state(&chat)) != Some(Lifecycle::Trash) {
        return Err(Error::conflict("This conversation is not in the trash."));
    }
    if chat["purgeAt"].as_i64().is_none_or(|at| at <= now()) {
        return Err(Error::new(410, "This conversation has expired."));
    }
    if run_is_active(db.run(text(&chat, "runId"))?.as_ref()) {
        return Err(Error::conflict("Wait for the agent to finish stopping."));
    }
    chat["lifecycle"] = Lifecycle::Active.into();
    for key in [
        "trashedAt",
        "purgeAt",
        "previousLifecycle",
        "retryAfter",
        "lifecycleError",
    ] {
        chat[key] = Value::Null;
    }
    db.audit("chat.recovered", &json!({ "id": id }))?;
    db.put("chats", &chat)
}

impl Service {
    pub async fn chat_list_view(&self, view: &str) -> Result<Vec<Value>> {
        if View::parse(view).is_none() {
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
        let result = self
            .store
            .transaction(move |db| trash(db, &id, confirmed))
            .await?;
        if let Some(run) = result["runId"].as_str() {
            match self.worker.cancel(self, run).await {
                Err(error) if !error.is_conflict() => return Err(error),
                _ => {}
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
            .map_err(|_| Error::conflict("A storage operation is in progress. Retry shortly."))?;
        let id = id.to_owned();
        self.store.transaction(move |db| restore(db, &id)).await
    }
}

fn purge_due(chat: &Value) -> bool {
    chat["retryAfter"].as_i64().unwrap_or(0) <= now()
        && in_trash(chat)
        && chat["purgeAt"].as_i64().unwrap_or(i64::MAX) <= now()
}

/// Claims a trashed conversation for purging unless its agent is still stopping.
fn mark_purging(db: &Db<'_>, id: &str) -> Result<Option<Value>> {
    let mut current = required(db.get("chats", id)?, "Chat not found")?;
    if !in_trash(&current) || run_is_active(db.run(text(&current, "runId"))?.as_ref()) {
        return Ok(None);
    }
    current["lifecycle"] = Lifecycle::Purging.into();
    db.put("chats", &current)?;
    Ok(Some(current))
}

impl Service {
    pub async fn cleanup_conversations(&self) -> Result<()> {
        let _process_lock = match storage_lock(&self.config.data_dir) {
            Ok(lock) => lock,
            Err(error) if error.is_conflict() => return Ok(()),
            Err(error) => return Err(error),
        };
        let Ok(_guard) = self.conversation_storage_lock.try_lock() else {
            return Ok(());
        };
        let mut chats = self.store.list("chats").await?;
        chats.sort_by_key(|chat| chat["retryAfter"].as_i64().unwrap_or(0));
        for chat in chats.iter().filter(|chat| purge_due(chat)) {
            let id = text(chat, "id").to_owned();
            let claim = id.clone();
            let Some(chat) = self
                .store
                .transaction(move |db| mark_purging(db, &claim))
                .await?
            else {
                continue;
            };
            if let Err(error) = crate::conversation_deletion::purge(self, chat).await {
                self.record_purge_failure(id, error).await?;
            }
            // One conversation per pass bounds load, including the existing backlog.
            break;
        }
        Ok(())
    }

    async fn record_purge_failure(&self, id: String, error: Error) -> Result<()> {
        self.store
            .transaction(move |db| {
                if let Some(mut current) = db.get("chats", &id)? {
                    current["lifecycleError"] = error.message.into();
                    current["retryAfter"] = (now() + PURGE_RETRY_MS).into();
                    db.put("chats", &current)?;
                }
                Ok(())
            })
            .await
    }
}

fn request_new_session(db: &Db<'_>, id: &str) -> Result<Value> {
    let mut chat = required(db.get("chats", id)?, "Chat not found")?;
    require_active(&chat)?;
    let run = required(db.run(text(&chat, "runId"))?, "Run not found")?;
    let restored_session_failed = !chat["restoredAt"].is_null()
        && matches!(
            RunStatus::of(&run),
            Some(RunStatus::Failed | RunStatus::Interrupted)
        );
    if !restored_session_failed {
        return Err(Error::conflict(
            "A fresh session is available after a restored session fails.",
        ));
    }
    let key = format!("run-checkpoint:{}", text(&run, "id"));
    let mut checkpoint = required(db.kv(&key)?, "Workspace checkpoint not found")?;
    checkpoint["freshSession"] = true.into();
    checkpoint["launched"] = false.into();
    if let Some(checkpoint) = checkpoint.as_object_mut() {
        checkpoint.remove("controllerRecoveries");
    }
    db.set(&key, &checkpoint, None)?;
    db.patch_run(
        text(&run, "id"),
        &json!({
            "sessionId": null,
            "resumeAvailable": false
        }),
    )?;
    chat["sessionRestartRequested"] = true.into();
    chat["paused"] = true.into();
    db.put("chats", &chat)
}

impl Service {
    pub async fn chat_new_session(&self, id: &str, confirmed: bool) -> Result<Value> {
        if !confirmed {
            return Err(Error::conflict(
                "Confirm starting a fresh agent session using the preserved history and files.",
            ));
        }
        let id = id.to_owned();
        self.store
            .transaction(move |db| request_new_session(db, &id))
            .await
    }
}
