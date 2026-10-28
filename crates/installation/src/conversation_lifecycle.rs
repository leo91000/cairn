//! Conversation deletion is independent from execution status and task archival.
use crate::chats::storage::{Conversation, Message};
use crate::{
    chats::{MessageStatus, Question, QuestionStatus},
    config::now,
    error::{Error, Result, required},
    run_status::RunStatus,
    service::Service,
    store::Db,
    validation::{string_enum, text},
};
use cairn_contracts::ConversationEnvironmentState as EnvironmentState;
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

pub use cairn_contracts::ConversationRetention as Retention;

fn retention(db: &Db<'_>) -> Result<Retention> {
    db.conversation_retention()
}

fn cutoff(policy: &Retention) -> Option<i64> {
    (policy.inactivity_days > 0).then(|| now() - i64::from(policy.inactivity_days) * DAY)
}

/// Only work recorded by the current writer advances inactivity; reads do not count.
fn inactive_due(chat: &Conversation, cutoff: Option<i64>) -> bool {
    chat.lifecycle == Lifecycle::Active
        && chat.environment_state.is_none()
        && cutoff
            .zip(chat.last_activity_at)
            .is_some_and(|(cutoff, activity)| activity <= cutoff)
}

pub(crate) fn require_environment_state_ready(state: Option<EnvironmentState>) -> Result<()> {
    if state == Some(EnvironmentState::Cleaning) {
        return Err(Error::conflict(
            "The expired environment is being removed. Retry after cleanup finishes.",
        ));
    }
    Ok(())
}

/// A11's backend migration still receives the named conversation boundary.
pub(crate) fn require_environment_ready(chat: &Value) -> Result<()> {
    require_environment_state_ready(serde_json::from_value(chat["environmentState"].clone())?)
}

pub use cairn_contracts::ConversationLifecycle as Lifecycle;

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
    if let Some(chat) = db.conversation_for_run(run)? {
        if chat.lifecycle != cairn_contracts::ConversationLifecycle::Active {
            return Err(Error::conflict("This conversation has been deleted."));
        }
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

fn message_is_pending(message: &Message) -> bool {
    message.status == MessageStatus::Queued || message.status == MessageStatus::Sending
}

/// Cancels queued messages and open questions so nothing is delivered after deletion.
fn cancel_pending_input(
    db: &Db<'_>,
    messages: Vec<Message>,
    questions: Vec<(String, Question)>,
) -> Result<()> {
    for mut message in messages {
        if message_is_pending(&message) {
            message.status = MessageStatus::Cancelled;
            db.save_message(&message)?;
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
    if let Some(run) = chat["runId"].as_str() {
        db.revoke_mcp_run(run)?;
    }
    for prefix in ["push-outbox:"] {
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
    let mut chat = required(db.conversation(id)?, "Chat not found")?;
    if chat.lifecycle != cairn_contracts::ConversationLifecycle::Active {
        return Ok(serde_json::to_value(chat)?);
    }
    let run = db.run(chat.run_id.as_deref().unwrap_or(""))?;
    let messages = db.conversation_messages(id)?;
    let questions = db.keys_as::<Question>(&crate::chats::question_prefix(id))?;
    let busy = run_is_active(run.as_ref())
        || chat.wakeup.is_some()
        || messages.iter().any(message_is_pending)
        || questions.iter().any(|(_, question)| question.is_open());
    if busy && !confirmed {
        return Err(Error::conflict(
            "Confirm deletion to stop the agent and cancel pending messages and questions.",
        ));
    }
    cancel_pending_input(db, messages, questions)?;
    unshare_artifacts(db, chat.run_id.as_deref().unwrap_or(""))?;
    db.delete(&format!("chat-title-pending:{id}"))?;
    if let Some(run) = run.filter(|run| run_is_active(Some(run))) {
        db.change_run(
            text(&run, "id"),
            crate::runs::RunChanges {
                cancel_requested_at: Some(Some(now())),
                ..Default::default()
            },
        )?;
    }
    chat.cancelled_by_deletion = busy;
    chat.wakeup = None;
    revoke_chat_access(db, &serde_json::to_value(&chat)?, id)?;
    chat.previous_lifecycle = Some(chat.lifecycle);
    chat.lifecycle = cairn_contracts::ConversationLifecycle::Trash;
    let trashed_at = now();
    chat.trashed_at = Some(trashed_at);
    chat.purge_at = Some(trashed_at);
    chat.paused = true;
    db.set(
        "conversation-cache-revision",
        &crate::config::id().into(),
        None,
    )?;
    db.audit("chat.trashed", &json!({ "id": id }))?;
    Ok(serde_json::to_value(db.save_conversation(&chat)?)?)
}

fn restore(db: &Db<'_>, id: &str) -> Result<Value> {
    let mut chat = required(db.conversation(id)?, "Chat not found")?;
    if chat.lifecycle != cairn_contracts::ConversationLifecycle::Trash {
        return Err(Error::conflict("This conversation is not in the trash."));
    }
    if chat.purge_at.is_none_or(|at| at <= now()) {
        return Err(Error::new(410, "This conversation has expired."));
    }
    if run_is_active(db.run(chat.run_id.as_deref().unwrap_or(""))?.as_ref()) {
        return Err(Error::conflict("Wait for the agent to finish stopping."));
    }
    chat.lifecycle = cairn_contracts::ConversationLifecycle::Active;
    chat.trashed_at = None;
    chat.purge_at = None;
    chat.previous_lifecycle = None;
    chat.retry_after = None;
    chat.lifecycle_error = None;
    db.audit("chat.recovered", &json!({ "id": id }))?;
    Ok(serde_json::to_value(db.save_conversation(&chat)?)?)
}

impl Service {
    pub async fn conversation_retention(&self) -> Result<Value> {
        self.conversation_retention_authorized(|_| Ok(())).await
    }

    pub(crate) async fn conversation_retention_authorized<F>(&self, authorize: F) -> Result<Value>
    where
        F: FnOnce(&Db<'_>) -> Result<()> + Send + 'static,
    {
        self.store
            .read(move |db| {
                authorize(db)?;
                Ok(serde_json::to_value(retention(db)?)?)
            })
            .await
    }

    pub async fn save_conversation_retention(&self, input: Value) -> Result<Value> {
        self.save_conversation_retention_authorized(input, |_| Ok(()))
            .await
    }

    pub(crate) async fn save_conversation_retention_authorized<F>(
        &self,
        input: Value,
        authorize: F,
    ) -> Result<Value>
    where
        F: FnOnce(&Db<'_>) -> Result<()> + Send + 'static,
    {
        let policy: Retention = serde_json::from_value(input)
            .map_err(|_| Error::bad("Choose a whole number of days from 0 to 36500."))?;
        if policy.inactivity_days > 36500 {
            return Err(Error::bad("Choose a whole number of days from 0 to 36500."));
        }
        let result = self
            .store
            .transaction(move |db| {
                let value = serde_json::to_value(&policy)?;
                authorize(db)?;
                db.save_conversation_retention(&policy)?;
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

fn purge_due(chat: &Conversation) -> bool {
    chat.retry_after.unwrap_or(0) <= now()
        && chat.lifecycle != Lifecycle::Active
        && chat.purge_at.unwrap_or(i64::MAX) <= now()
}

fn mark_purging(db: &Db<'_>, id: &str) -> Result<Option<Conversation>> {
    let Some(mut current) = db.conversation(id)? else {
        return Ok(None);
    };
    let run = db.run(current.run_id.as_deref().unwrap_or(""))?;
    if run_is_active(run.as_ref()) || !purge_due(&current) {
        return Ok(None);
    }
    current.lifecycle = Lifecycle::Purging;
    Ok(Some(db.save_conversation(&current)?))
}

fn environment_cleanup_due(chat: &Conversation) -> bool {
    chat.lifecycle == Lifecycle::Active
        && chat.environment_state == Some(EnvironmentState::Cleaning)
        && chat.environment_retry_after.unwrap_or(0) <= now()
}

fn mark_environment_expiring(db: &Db<'_>, id: &str) -> Result<Option<Conversation>> {
    let Some(mut chat) = db.conversation(id)? else {
        return Ok(None);
    };
    if environment_cleanup_due(&chat) {
        return Ok(Some(chat));
    }
    let cutoff = cutoff(&retention(db)?);
    if !inactive_due(&chat, cutoff) {
        return Ok(None);
    }
    let run_id = chat.run_id.as_deref().unwrap_or("");
    let run = db.run(run_id)?;
    let run_busy = run.as_ref().is_some_and(|run| {
        run_is_active(Some(run))
            || run["recoveryPending"] == true
            || run["backendMigration"].is_object()
            || run["moveRequest"].is_object()
            || run["finishedAt"]
                .as_i64()
                .zip(cutoff)
                .is_some_and(|(finished, cutoff)| finished > cutoff)
    });
    let pending = chat.wakeup.is_some()
        || db.conversation_messages(id)?.iter().any(message_is_pending)
        || db
            .keys_as::<Question>(&crate::chats::question_prefix(id))?
            .iter()
            .any(|(_, q)| q.is_open());
    if run_busy || pending || crate::chats::coding_agent::pending_for_run(db, run_id)? {
        return Ok(None);
    }
    chat.environment_state = Some(EnvironmentState::Cleaning);
    chat.environment_expired_at = Some(now());
    if let Some(run) = run {
        db.change_run(
            text(&run, "id"),
            crate::runs::RunChanges {
                resume_available: Some(false),
                boundary: Some(json!({ "environmentState": "cleaning" })),
                ..Default::default()
            },
        )?;
        db.revoke_mcp_run(text(&run, "id"))?;
    }
    db.audit("chat.environment.expired", &json!({ "id": id }))?;
    Ok(Some(db.save_conversation(&chat)?))
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
            .read(|db| Ok((db.conversations()?, cutoff(&retention(db)?))))
            .await?;
        chats.sort_by_key(|chat| chat.retry_after.unwrap_or(0));
        for chat in chats.iter().filter(|chat| {
            purge_due(chat) || environment_cleanup_due(chat) || inactive_due(chat, cutoff)
        }) {
            // Preparation can create run files before execution is registered as active.
            let preparation = self
                .worker
                .run_preparation_lock(chat.run_id.as_deref().unwrap_or(""))
                .await;
            let Ok(_preparation) = preparation.try_lock_owned() else {
                continue;
            };
            // Terminal status precedes final checkpoint publication and resource release.
            let active = self.worker.active.lock().await;
            if active.contains_key(chat.run_id.as_deref().unwrap_or("")) {
                continue;
            }
            let id = chat.id.clone();
            let claim = id.clone();
            let claim = self
                .store
                .transaction(move |db| {
                    if let Some(chat) = mark_purging(db, &claim)? {
                        return Ok(Some((chat, true)));
                    }
                    Ok(mark_environment_expiring(db, &claim)?.map(|chat| (chat, false)))
                })
                .await?;
            let Some((chat, deleting)) = claim else {
                continue;
            };
            drop(active);
            let result = if deleting {
                crate::conversation_deletion::purge(self, serde_json::to_value(chat)?).await
            } else {
                crate::conversation_deletion::expire_environment(self, serde_json::to_value(chat)?)
                    .await
            };
            if let Err(error) = result {
                self.record_cleanup_failure(id, error, deleting).await?;
            }
            // One conversation per pass bounds cleanup load, including backlog.
            break;
        }
        Ok(())
    }

    async fn record_cleanup_failure(&self, id: String, error: Error, deleting: bool) -> Result<()> {
        self.store
            .transaction(move |db| {
                if let Some(mut current) = db.conversation(&id)? {
                    if deleting {
                        current.lifecycle_error = Some(error.message);
                        current.retry_after = Some(now() + PURGE_RETRY_MS);
                    } else {
                        current.environment_error = Some(error.message);
                        current.environment_retry_after = Some(now() + PURGE_RETRY_MS);
                    }
                    db.save_conversation(&current)?;
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
            !run["restoredAt"].is_null()
                && matches!(
                    RunStatus::of(run),
                    Some(RunStatus::Failed | RunStatus::Interrupted)
                )
        })
}

pub(crate) fn request_new_session(db: &Db<'_>, id: &str) -> Result<Value> {
    let mut chat = required(db.conversation(id)?, "Chat not found")?;
    if chat.lifecycle != cairn_contracts::ConversationLifecycle::Active {
        return Err(Error::conflict("This conversation has been deleted."));
    }
    let run = required(
        db.run(chat.run_id.as_deref().unwrap_or(""))?,
        "Run not found",
    )?;
    // A confirmation retried after a lost response keeps the already recorded intention.
    if chat.session_restart_requested {
        return Ok(serde_json::to_value(chat)?);
    }

    if !can_request_new_session(&serde_json::to_value(&chat)?, Some(&run)) {
        return Err(Error::conflict(
            "A fresh session is available after a restored session fails.",
        ));
    }
    db.change_run(
        text(&run, "id"),
        crate::runs::RunChanges {
            session_id: Some(None),
            resume_available: Some(false),
            ..Default::default()
        },
    )?;
    chat.session_restart_requested = true;
    chat.wakeup = None;
    chat.paused = true;
    Ok(serde_json::to_value(db.save_conversation(&chat)?)?)
}

impl Service {
    pub async fn chat_new_session(&self, id: &str, confirmed: bool) -> Result<Value> {
        self.chat_new_session_authorized(id, confirmed, |_| Ok(()))
            .await
    }

    pub(crate) async fn chat_new_session_authorized<F>(
        &self,
        id: &str,
        confirmed: bool,
        authorize: F,
    ) -> Result<Value>
    where
        F: FnOnce(&Db<'_>) -> Result<()> + Send + 'static,
    {
        if !confirmed {
            return Err(Error::conflict(
                "Confirm starting a fresh agent session using the preserved history and files.",
            ));
        }
        let id = id.to_owned();
        self.store
            .transaction(move |db| {
                authorize(db)?;
                request_new_session(db, &id)
            })
            .await
    }
}

#[cfg(test)]
use crate::run_fixture::DbRunFixture;

#[cfg(test)]
mod tests {
    use super::*;

    fn inactive_due(value: &Value, cutoff: Option<i64>) -> bool {
        let mut value = value.clone();
        value["id"] = "fixture".into();
        super::inactive_due(&serde_json::from_value(value).unwrap(), cutoff)
    }

    #[tokio::test]
    async fn fresh_session_intent_survives_a_live_checkpoint_persist() {
        use crate::{
            store::Store,
            worker::checkpoint::{Checkpoint, RunCheckpoint},
        };

        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path()).unwrap();
        store
            .transaction(|db| {
                db.add_run_fixture(
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
                Ok(())
            })
            .await
            .unwrap();
        let state = RunCheckpoint::from_value(json!({
            "launched": true,
            "completed": true,
            "controllerRecoveries": 2,
            "prepared": { "workspace": "preserved" },
        }))
        .unwrap();
        let checkpoint = Checkpoint::new(store.clone(), "run".into(), state);
        checkpoint.persist().await.unwrap();
        let before = store.checkpoint_boundary("run").await.unwrap();
        store
            .transaction(|db| {
                db.save_conversation_boundary(&json!({
                    "id": "chat",
                    "runId": "run",
                    "wakeup": { "wakeAt": 100 },
                }))?;
                request_new_session(db, "chat")
            })
            .await
            .unwrap();
        assert_eq!(store.checkpoint_boundary("run").await.unwrap(), before);

        // Force the old worker's late write with the real checkpoint owner.
        // This is an isolated non-VM interleaving, not production frequency evidence.
        checkpoint.persist().await.unwrap();
        let chat = store.conversation_boundary("chat").await.unwrap().unwrap();
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
        assert!(!inactive_due(&json!({ "createdAt": 1 }), Some(100)));
    }
}
