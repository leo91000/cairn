//! Frozen version-14 → 15 adapter, never used by the current runtime.
use crate::{
    config::{id, now},
    error::{Error, Result},
    store::Db,
};
use cairn_contracts::{
    ChatMessage, ConversationEnvironmentState, ConversationLifecycle, ConversationRequester,
};
use rusqlite::params;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationV15 {
    pub id: String,
    #[serde(default)]
    pub slug: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub agent_id: String,
    #[serde(default)]
    pub project_id: Option<String>,
    #[serde(default)]
    pub run_id: Option<String>,
    #[serde(default)]
    pub paused: bool,
    #[serde(default)]
    pub lifecycle: ConversationLifecycle,
    #[serde(default)]
    pub created_at: i64,
    #[serde(default)]
    pub updated_at: i64,
    #[serde(default)]
    pub last_activity_at: Option<i64>,
    #[serde(default)]
    pub revision: String,
    #[serde(default)]
    pub trashed_at: Option<i64>,
    #[serde(default)]
    pub purge_at: Option<i64>,
    #[serde(default)]
    pub restored_at: Option<i64>,
    #[serde(default)]
    pub retry_after: Option<i64>,
    #[serde(default)]
    pub lifecycle_error: Option<String>,
    #[serde(default)]
    pub previous_lifecycle: Option<ConversationLifecycle>,
    #[serde(default)]
    pub session_restart_requested: bool,
    #[serde(default)]
    pub cancelled_by_deletion: bool,
    #[serde(default)]
    pub agent_name: Option<String>,
    #[serde(default)]
    pub project_name: Option<String>,
    #[serde(default)]
    pub requested_by: Option<ConversationRequester>,
    #[serde(default)]
    pub wakeup: Option<Value>,
    #[serde(default)]
    pub native_principals: Option<Vec<String>>,
    #[serde(default)]
    pub native_authority_nodes: Option<Vec<String>>,
    #[serde(default)]
    pub native_parent_run_id: Option<String>,
    #[serde(default)]
    pub native_parent_access: Option<String>,
    #[serde(default)]
    pub native_provenance_unknown: Option<bool>,
    #[serde(default)]
    pub native_plugin_grants: Option<Vec<crate::plugins::connections::NativePluginGrant>>,
    #[serde(default)]
    pub environment_state: Option<ConversationEnvironmentState>,
    #[serde(default)]
    pub environment_expired_at: Option<i64>,
    #[serde(default)]
    pub environment_error: Option<String>,
    #[serde(default)]
    pub environment_retry_after: Option<i64>,
    /// Retained historical account reference; it does not select an account.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_id: Option<String>,
}

/// A8 still owns questionnaire delivery and answer validation. Its temporary
/// payload is local to this boundary, never an open message decision document.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageV15 {
    #[serde(flatten)]
    pub content: ChatMessage,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answers: Option<std::collections::BTreeMap<String, MessageAnswer>>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MessageAnswer {
    Legacy(Vec<String>),
    Current {
        #[serde(default)]
        selected: Vec<String>,
        #[serde(default)]
        comment: String,
    },
}

pub(super) fn save_conversation(
    db: &Db<'_>,
    conversation: &ConversationV15,
) -> Result<ConversationV15> {
    let mut c = conversation.clone();
    c.revision = id();
    db.0.prepare_cached("INSERT INTO conversations(id,slug,title,agent_id,project_id,run_id,paused,lifecycle,created_at,updated_at,last_activity_at,revision,trashed_at,purge_at,restored_at,retry_after,lifecycle_error,previous_lifecycle,session_restart_requested,cancelled_by_deletion,agent_name,project_name,requested_by,wakeup,native_principals,native_authority_nodes,native_parent_run_id,native_parent_access,native_provenance_unknown,native_plugin_grants,environment_state,environment_expired_at,environment_error,environment_retry_after,account_id,persisted_at,live_agent_id,live_project_id) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,(SELECT id FROM agents WHERE id=?),(SELECT id FROM projects WHERE id=?)) ON CONFLICT(id) DO UPDATE SET slug=excluded.slug,title=excluded.title,agent_id=excluded.agent_id,project_id=excluded.project_id,run_id=excluded.run_id,paused=excluded.paused,lifecycle=excluded.lifecycle,created_at=excluded.created_at,updated_at=excluded.updated_at,last_activity_at=excluded.last_activity_at,revision=excluded.revision,trashed_at=excluded.trashed_at,purge_at=excluded.purge_at,restored_at=excluded.restored_at,retry_after=excluded.retry_after,lifecycle_error=excluded.lifecycle_error,previous_lifecycle=excluded.previous_lifecycle,session_restart_requested=excluded.session_restart_requested,cancelled_by_deletion=excluded.cancelled_by_deletion,agent_name=excluded.agent_name,project_name=excluded.project_name,requested_by=excluded.requested_by,wakeup=excluded.wakeup,native_principals=excluded.native_principals,native_authority_nodes=excluded.native_authority_nodes,native_parent_run_id=excluded.native_parent_run_id,native_parent_access=excluded.native_parent_access,native_provenance_unknown=excluded.native_provenance_unknown,native_plugin_grants=excluded.native_plugin_grants,environment_state=excluded.environment_state,environment_expired_at=excluded.environment_expired_at,environment_error=excluded.environment_error,environment_retry_after=excluded.environment_retry_after,account_id=excluded.account_id,persisted_at=excluded.persisted_at,live_agent_id=excluded.live_agent_id,live_project_id=excluded.live_project_id")?.execute(params![
            c.id,
            c.slug,
            c.title,
            c.agent_id,
            c.project_id,
            c.run_id,
            c.paused,
            c.lifecycle.as_str(),
            c.created_at,
            c.updated_at,
            c.last_activity_at,
            c.revision,
            c.trashed_at,
            c.purge_at,
            c.restored_at,
            c.retry_after,
            c.lifecycle_error,
            c.previous_lifecycle.map(ConversationLifecycle::as_str),
            c.session_restart_requested,
            c.cancelled_by_deletion,
            c.agent_name,
            c.project_name,
            c.requested_by.as_ref().map(serde_json::to_string).transpose()?,
            c.wakeup.as_ref().map(serde_json::to_string).transpose()?,
            c.native_principals.as_ref().map(serde_json::to_string).transpose()?,
            c.native_authority_nodes.as_ref().map(serde_json::to_string).transpose()?,
            c.native_parent_run_id,
            c.native_parent_access,
            c.native_provenance_unknown,
            c.native_plugin_grants.as_ref().map(serde_json::to_string).transpose()?,
            c.environment_state.map(ConversationEnvironmentState::as_str),
            c.environment_expired_at,
            c.environment_error,
            c.environment_retry_after,
            c.account_id,
            now(), c.agent_id, c.project_id
        ])?;
    Ok(c)
}

pub(super) fn save_message(db: &Db<'_>, message: &MessageV15) -> Result<MessageV15> {
    let mut m = message.clone();
    m.revision = id();
    db.0.prepare_cached("INSERT INTO chat_messages(id,chat_id,revision,text,provider,model,reasoning,speed,mode,status,created_at,question_id,answers) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?) ON CONFLICT(id) DO UPDATE SET revision=excluded.revision,text=excluded.text,provider=excluded.provider,model=excluded.model,reasoning=excluded.reasoning,speed=excluded.speed,mode=excluded.mode,status=excluded.status,question_id=excluded.question_id,answers=excluded.answers")?.execute(params![m.id,m.chat_id,m.revision,m.text,m.provider,m.model,m.reasoning,m.speed,m.mode.as_str(),m.status.as_str(),m.created_at,m.question_id,m.answers.as_ref().map(serde_json::to_string).transpose()?])?;
    db.0.prepare_cached("DELETE FROM message_attachments WHERE message_id=?")?
        .execute([&m.id])?;
    let mut statement =
        db.0.prepare_cached("INSERT INTO message_attachments VALUES(?,?,?,?,?,?,?,?,?,?,?,?)")?;
    for (position, a) in m.attachments.iter().enumerate() {
        statement.execute(params![
            m.id,
            position as i64,
            a.id,
            a.chat_id,
            a.name,
            i64::try_from(a.size)
                .map_err(|_| Error::bad("Attachment size exceeds the wire range."))?,
            a.media_type,
            a.kind,
            a.width,
            a.height,
            a.digest,
            a.created_at
        ])?;
    }
    Ok(m)
}

impl std::ops::Deref for MessageV15 {
    type Target = ChatMessage;

    fn deref(&self) -> &Self::Target {
        &self.content
    }
}

impl std::ops::DerefMut for MessageV15 {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.content
    }
}
