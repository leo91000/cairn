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
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub type StorageLock = crate::file_lock::Guard;

pub fn storage_lock(directory: &std::path::Path) -> Result<StorageLock> {
    crate::file_lock::exclusive(
        &directory.join("conversation-storage.lock"),
        "A conversation storage operation is in progress. Retry shortly.",
    )
}

pub const DAY: i64 = 86_400_000;
const PURGE_RETRY_MS: i64 = 60_000;
const RETENTION_KEY: &str = "conversation-expiration";

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Retention {
    inactivity_days: u32,
}

impl Default for Retention {
    fn default() -> Self {
        Self { inactivity_days: 7 }
    }
}

impl Retention {
    fn load(db: &Db<'_>) -> Result<Self> {
        Ok(db.kv_as(RETENTION_KEY)?.unwrap_or_default())
    }

    fn cutoff(&self) -> Option<i64> {
        (self.inactivity_days > 0).then(|| now() - i64::from(self.inactivity_days) * DAY)
    }
}

/// Legacy chats fall back to their last update or creation time. Reads do not count.
fn inactive_due(chat: &Value, cutoff: Option<i64>) -> bool {
    let activity = chat["lastActivityAt"]
        .as_i64()
        .or_else(|| chat["updatedAt"].as_i64())
        .or_else(|| chat["createdAt"].as_i64());
    is_active(chat)
        && cutoff
            .zip(activity)
            .is_some_and(|(cutoff, activity)| activity <= cutoff)
}

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
        return Err(Error::conflict("This conversation has been deleted."));
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
        || chat["wakeup"].is_object()
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
    chat["wakeup"] = Value::Null;
    revoke_chat_access(db, &chat, id)?;
    chat["previousLifecycle"] = state(&chat).into();
    chat["lifecycle"] = Lifecycle::Trash.into();
    let trashed_at = now();
    chat["trashedAt"] = trashed_at.into();
    chat["purgeAt"] = trashed_at.into();
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
    pub async fn conversation_retention(&self) -> Result<Value> {
        self.store
            .read(|db| Ok(serde_json::to_value(Retention::load(db)?)?))
            .await
    }

    pub async fn save_conversation_retention(&self, input: Value) -> Result<Value> {
        let policy: Retention = serde_json::from_value(input)
            .map_err(|_| Error::bad("Choose a whole number of days from 0 to 36500."))?;
        if policy.inactivity_days > 36500 {
            return Err(Error::bad("Choose a whole number of days from 0 to 36500."));
        }
        let result = self
            .store
            .transaction(move |db| {
                let value = serde_json::to_value(policy)?;
                db.set(RETENTION_KEY, &value, None)?;
                db.audit("conversation.retention.updated", &value)?;
                Ok(value)
            })
            .await?;
        self.worker.notify_conversation_cleanup();
        Ok(result)
    }

    /// Conversations in `view`, with those `account` has not read since their agent finished.
    pub async fn chat_list_view(&self, view: &str, account: &str) -> Result<Vec<Value>> {
        if View::parse(view).is_none() {
            return Err(Error::bad("Choose active or trash."));
        }
        crate::reads::begin(self, account).await?;
        let (view, account) = (view.to_owned(), account.to_owned());
        self.store
            .read(move |db| {
                let mut chats = crate::chats::list_all(db)?
                    .into_iter()
                    .filter(|chat| in_view(chat, &view))
                    .collect::<Vec<_>>();
                crate::reads::Reads::load(db, &account)?.chats(db, &mut chats)?;
                Ok(chats)
            })
            .await
    }

    pub async fn chat_trash(&self, id: &str, confirmed: bool) -> Result<Value> {
        self.chat_trash_authorized(id, confirmed, |_| Ok(())).await
    }

    pub(crate) async fn chat_trash_authorized<F>(
        &self,
        id: &str,
        confirmed: bool,
        authorize: F,
    ) -> Result<Value>
    where
        F: FnOnce(&Db<'_>) -> Result<()> + Send + 'static,
    {
        crate::validation::uuid(id)?;
        let id = id.to_owned();
        let result = self
            .store
            .transaction(move |db| {
                authorize(db)?;
                trash(db, &id, confirmed)
            })
            .await?;
        if let Some(run) = result["runId"].as_str() {
            match self.worker.cancel(self, run).await {
                // Cleanup may have already removed a finished run after deletion committed.
                Err(error) if error.status != 404 && !error.is_conflict() => return Err(error),
                _ => {}
            }
        }
        self.worker.notify_conversation_cleanup();
        Ok(result)
    }

    pub async fn chat_restore(&self, id: &str) -> Result<Value> {
        self.chat_restore_authorized(id, |_| Ok(())).await
    }

    pub(crate) async fn chat_restore_authorized<F>(&self, id: &str, authorize: F) -> Result<Value>
    where
        F: FnOnce(&Db<'_>) -> Result<()> + Send + 'static,
    {
        let _process_lock = storage_lock(&self.config.data_dir)?;
        crate::validation::uuid(id)?;
        let _guard = self
            .conversation_storage_lock
            .try_lock()
            .map_err(|_| Error::conflict("A storage operation is in progress. Retry shortly."))?;
        let id = id.to_owned();
        self.store
            .transaction(move |db| {
                authorize(db)?;
                restore(db, &id)
            })
            .await
    }
}

fn purge_due(chat: &Value) -> bool {
    chat["retryAfter"].as_i64().unwrap_or(0) <= now()
        && in_trash(chat)
        && chat["purgeAt"].as_i64().unwrap_or(i64::MAX) <= now()
}

/// Expires an idle conversation or claims due trash, after checking pending work.
fn mark_purging(db: &Db<'_>, id: &str) -> Result<Option<Value>> {
    let Some(mut current) = db.get("chats", id)? else {
        return Ok(None);
    };
    let run = db.run(text(&current, "runId"))?;
    if run_is_active(run.as_ref()) {
        return Ok(None);
    }
    if is_active(&current) {
        // Recheck policy, activity and pending work in the same transaction as deletion.
        let cutoff = Retention::load(db)?.cutoff();
        let recently_finished = run
            .as_ref()
            .and_then(|run| run["finishedAt"].as_i64())
            .zip(cutoff)
            .is_some_and(|(finished, cutoff)| finished > cutoff);
        if !inactive_due(&current, cutoff) || recently_finished {
            return Ok(None);
        }
        match trash(db, id, false) {
            Ok(chat) => current = chat,
            Err(error) if error.is_conflict() => return Ok(None),
            Err(error) => return Err(error),
        }
        db.audit("chat.expired", &json!({ "id": id }))?;
    }
    if !purge_due(&current) {
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
        let (mut chats, cutoff) = self
            .store
            .read(|db| Ok((db.list("chats")?, Retention::load(db)?.cutoff())))
            .await?;
        chats.sort_by_key(|chat| chat["retryAfter"].as_i64().unwrap_or(0));
        for chat in chats
            .iter()
            .filter(|chat| purge_due(chat) || inactive_due(chat, cutoff))
        {
            // Preparation can create run files before execution is registered as active.
            let preparation = self.worker.run_preparation_lock(text(chat, "runId")).await;
            let Ok(_preparation) = preparation.try_lock_owned() else {
                continue;
            };
            // Terminal status is published before checkpointing and resource release finish.
            let active = self.worker.active.lock().await;
            if active.contains_key(text(chat, "runId")) {
                continue;
            }

            let id = text(chat, "id").to_owned();
            let claim = id.clone();
            let Some(chat) = self
                .store
                .transaction(move |db| mark_purging(db, &claim))
                .await?
            else {
                continue;
            };
            drop(active);
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

/// A restoration keeps the historical offer lifetime: any later failed or
/// interrupted turn can offer a fresh session, even after an intervening success.
/// Both clients consume this availability rather than duplicating the rule.
pub(crate) fn can_request_new_session(chat: &Value, run: Option<&Value>) -> bool {
    is_active(chat)
        && chat["sessionRestartRequested"] != true
        && run.is_some_and(|run| {
            (!run["restoredAt"].is_null() || !chat["restoredAt"].is_null())
                && matches!(
                    RunStatus::of(run),
                    Some(RunStatus::Failed | RunStatus::Interrupted)
                )
        })
}

pub(crate) fn request_new_session(db: &Db<'_>, id: &str) -> Result<Value> {
    let mut chat = required(db.get("chats", id)?, "Chat not found")?;
    require_active(&chat)?;
    let run = required(db.run(text(&chat, "runId"))?, "Run not found")?;
    // A confirmation retried after a lost response keeps the already recorded intention.
    if chat["sessionRestartRequested"] == true {
        return Ok(chat);
    }

    if !can_request_new_session(&chat, Some(&run)) {
        return Err(Error::conflict(
            "A fresh session is available after a restored session fails.",
        ));
    }
    db.patch_run(
        text(&run, "id"),
        &json!({
            "sessionId": null,
            "resumeAvailable": false
        }),
    )?;
    chat["sessionRestartRequested"] = true.into();
    chat["wakeup"] = Value::Null;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn fresh_session_intent_survives_a_live_checkpoint_persist() {
        use crate::{
            store::Store,
            worker::checkpoint::{Checkpoint, RunCheckpoint},
        };

        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path()).unwrap();
        let state = RunCheckpoint::from_value(json!({
            "launched": true,
            "completed": true,
            "controllerRecoveries": 2,
            "prepared": { "workspace": "preserved" },
        }))
        .unwrap();
        let checkpoint = Checkpoint::new(store.clone(), "run".into(), state);
        checkpoint.persist().await.unwrap();
        let before = store.kv("run-checkpoint:run").await.unwrap();
        store
            .transaction(|db| {
                db.add_run(
                    &json!({
                        "id": "run",
                        "taskId": "chat",
                        "createdAt": 0,
                        "status": "failed",
                        "sessionId": "old-native",
                        "restoredAt": 10,
                    }),
                    None,
                )?;
                db.put(
                    "chats",
                    &json!({
                        "id": "chat",
                        "runId": "run",
                        "wakeup": { "wakeAt": 100 },
                    }),
                )?;
                request_new_session(db, "chat")
            })
            .await
            .unwrap();
        assert_eq!(store.kv("run-checkpoint:run").await.unwrap(), before);

        // Force the old worker's late write with the real checkpoint owner.
        // This is an isolated non-VM interleaving, not production frequency evidence.
        checkpoint.persist().await.unwrap();
        let chat = store.get("chats", "chat").await.unwrap().unwrap();
        assert_eq!(chat["sessionRestartRequested"], true);
        assert_eq!(chat["paused"], true);
        assert!(chat["wakeup"].is_null());
        let run = store.run("run").await.unwrap();
        assert!(run["sessionId"].is_null());
        assert_eq!(run["resumeAvailable"], false);
    }

    #[test]
    fn inactivity_boundary_is_inclusive_and_missing_timestamps_are_preserved() {
        let chat = json!({ "lastActivityAt": 100, "updatedAt": 1 });
        assert!(!inactive_due(&chat, Some(99)));
        assert!(inactive_due(&chat, Some(100)));
        assert!(!inactive_due(&chat, None));
        assert!(!inactive_due(&json!({}), Some(100)));
        assert!(!inactive_due(
            &json!({ "lifecycle": "trash", "updatedAt": 1 }),
            Some(100)
        ));
        assert!(inactive_due(&json!({ "createdAt": 1 }), Some(100)));
    }
}
