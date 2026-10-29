//! Current conversation SQL. Waits/questions and native provenance have explicit owning boundaries.
use crate::{
    config::{id, now},
    error::{Error, Result},
    store::{Db, Store},
};
use cairn_contracts::{
    ChatAttachment, ChatMessage, ConversationEnvironmentState, ConversationLifecycle,
    ConversationRequester, MessageMode, MessageStatus,
};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Conversation {
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
pub struct Message {
    #[serde(flatten)]
    pub content: ChatMessage,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answers: Option<std::collections::BTreeMap<String, MessageAnswer>>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MessageAnswer {
    #[serde(default)]
    pub selected: Vec<String>,
    #[serde(default)]
    pub comment: String,
}

impl std::ops::Deref for Message {
    type Target = ChatMessage;

    fn deref(&self) -> &Self::Target {
        &self.content
    }
}

impl std::ops::DerefMut for Message {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.content
    }
}

pub(crate) const SCHEMA: &str = "
CREATE TABLE conversation_retention(singleton INTEGER PRIMARY KEY CHECK(singleton=1),inactivity_days INTEGER NOT NULL CHECK(inactivity_days BETWEEN 0 AND 36500));
CREATE TABLE conversations(
    id TEXT PRIMARY KEY NOT NULL,
    slug TEXT NOT NULL UNIQUE CHECK(length(slug)>0),
    title TEXT NOT NULL,
    agent_id TEXT NOT NULL,
    live_agent_id TEXT REFERENCES agents(id) ON DELETE SET NULL CHECK(live_agent_id=agent_id),
    project_id TEXT,
    live_project_id TEXT REFERENCES projects(id) ON DELETE SET NULL CHECK(live_project_id=project_id),
    run_id TEXT REFERENCES runs(id) ON DELETE SET NULL,
    paused INTEGER NOT NULL CHECK(paused IN (0,1)),
    lifecycle TEXT NOT NULL CHECK(lifecycle IN ('active','trash','purging')),
    created_at INTEGER NOT NULL CHECK(created_at BETWEEN 0 AND 9007199254740991),
    updated_at INTEGER NOT NULL CHECK(updated_at BETWEEN 0 AND 9007199254740991),
    last_activity_at INTEGER CHECK(last_activity_at BETWEEN 0 AND 9007199254740991),
    revision TEXT NOT NULL CHECK(length(revision)>0),
    trashed_at INTEGER CHECK(trashed_at BETWEEN 0 AND 9007199254740991),
    purge_at INTEGER CHECK(purge_at BETWEEN 0 AND 9007199254740991),
    restored_at INTEGER CHECK(restored_at BETWEEN 0 AND 9007199254740991),
    retry_after INTEGER CHECK(retry_after BETWEEN 0 AND 9007199254740991),
    lifecycle_error TEXT,
    previous_lifecycle TEXT CHECK(previous_lifecycle IN ('active','trash','purging')),
    session_restart_requested INTEGER NOT NULL CHECK(session_restart_requested IN (0,1)),
    cancelled_by_deletion INTEGER NOT NULL CHECK(cancelled_by_deletion IN (0,1)),
    agent_name TEXT,
    project_name TEXT,
    requested_by TEXT CHECK(requested_by IS NULL OR json_valid(requested_by)),
    wakeup TEXT CHECK(wakeup IS NULL OR json_valid(wakeup)),
    native_principals TEXT CHECK(native_principals IS NULL OR json_valid(native_principals)),
    native_authority_nodes TEXT CHECK(native_authority_nodes IS NULL OR json_valid(native_authority_nodes)),
    native_parent_run_id TEXT,
    native_parent_access TEXT,
    native_provenance_unknown INTEGER CHECK(native_provenance_unknown IN (0,1)),
    native_plugin_grants TEXT CHECK(native_plugin_grants IS NULL OR json_valid(native_plugin_grants)) ,
    environment_state TEXT CHECK(environment_state IN ('cleaning','expired')),
    environment_expired_at INTEGER CHECK(environment_expired_at BETWEEN 0 AND 9007199254740991),
    environment_error TEXT,
    environment_retry_after INTEGER CHECK(environment_retry_after BETWEEN 0 AND 9007199254740991),
    account_id TEXT,
    persisted_at INTEGER NOT NULL
);
CREATE UNIQUE INDEX conversations_run ON conversations(run_id) WHERE run_id IS NOT NULL;
CREATE INDEX conversations_live_agent ON conversations(live_agent_id) WHERE live_agent_id IS NOT NULL;
CREATE INDEX conversations_live_project ON conversations(live_project_id) WHERE live_project_id IS NOT NULL;
CREATE INDEX conversations_updated ON conversations(persisted_at DESC,id);
CREATE TABLE chat_messages(
    id TEXT PRIMARY KEY NOT NULL,
    chat_id TEXT NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
    revision TEXT NOT NULL CHECK(length(revision)>0),
    text TEXT NOT NULL,
    provider TEXT NOT NULL CHECK(provider IN ('','codex','claude','opencode')),
    model TEXT NOT NULL,
    reasoning TEXT NOT NULL,
    speed TEXT NOT NULL,
    mode TEXT NOT NULL CHECK(mode IN ('queue','steer')),
    status TEXT NOT NULL CHECK(status IN ('queued','sending','delivered','cancelled')),
    created_at INTEGER NOT NULL CHECK(created_at BETWEEN 0 AND 9007199254740991),
    question_id TEXT,
    answers TEXT CHECK(answers IS NULL OR json_valid(answers)),
    plan INTEGER NOT NULL DEFAULT 0 CHECK(plan IN (0,1))
);
CREATE INDEX chat_messages_chat ON chat_messages(chat_id,created_at);
CREATE INDEX chat_messages_pending ON chat_messages(chat_id,status,created_at);
CREATE TABLE message_attachments(
    message_id TEXT NOT NULL REFERENCES chat_messages(id) ON DELETE CASCADE,
    position INTEGER NOT NULL CHECK(position BETWEEN 0 AND 7),
    attachment_id TEXT NOT NULL,
    chat_id TEXT NOT NULL,
    name TEXT NOT NULL,
    size INTEGER NOT NULL CHECK(size BETWEEN 0 AND 41943040),
    media_type TEXT NOT NULL,
    kind TEXT NOT NULL CHECK(kind IN ('image','file')),
    width INTEGER CHECK(width>0),
    height INTEGER CHECK(height>0),
    digest TEXT NOT NULL,
    created_at INTEGER NOT NULL CHECK(created_at BETWEEN 0 AND 9007199254740991),
    PRIMARY KEY(message_id,position),
    UNIQUE(message_id,attachment_id)
) WITHOUT ROWID;
";

const COLUMNS: &str = "id,slug,title,agent_id,project_id,run_id,paused,lifecycle,created_at,updated_at,last_activity_at,revision,trashed_at,purge_at,restored_at,retry_after,lifecycle_error,previous_lifecycle,session_restart_requested,cancelled_by_deletion,agent_name,project_name,requested_by,wakeup,native_principals,native_authority_nodes,native_parent_run_id,native_parent_access,native_provenance_unknown,native_plugin_grants,environment_state,environment_expired_at,environment_error,environment_retry_after,account_id";

fn decode<T: serde::de::DeserializeOwned>(
    row: &rusqlite::Row<'_>,
    index: usize,
) -> rusqlite::Result<Option<T>> {
    let value: Option<String> = row.get(index)?;
    value
        .map(|value| {
            serde_json::from_str(&value).map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    index,
                    rusqlite::types::Type::Text,
                    Box::new(error),
                )
            })
        })
        .transpose()
}

fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Conversation> {
    Ok(Conversation {
        id: r.get(0)?,
        slug: r.get(1)?,
        title: r.get(2)?,
        agent_id: r.get(3)?,
        project_id: r.get(4)?,
        run_id: r.get(5)?,
        paused: r.get(6)?,
        lifecycle: decode_enum(r, 7)?.unwrap_or_default(),
        created_at: r.get(8)?,
        updated_at: r.get(9)?,
        last_activity_at: r.get(10)?,
        revision: r.get(11)?,
        trashed_at: r.get(12)?,
        purge_at: r.get(13)?,
        restored_at: r.get(14)?,
        retry_after: r.get(15)?,
        lifecycle_error: r.get(16)?,
        previous_lifecycle: decode_enum(r, 17)?,
        session_restart_requested: r.get(18)?,
        cancelled_by_deletion: r.get(19)?,
        agent_name: r.get(20)?,
        project_name: r.get(21)?,
        requested_by: decode(r, 22)?,
        wakeup: decode(r, 23)?,
        native_principals: decode(r, 24)?,
        native_authority_nodes: decode(r, 25)?,
        native_parent_run_id: r.get(26)?,
        native_parent_access: r.get(27)?,
        native_provenance_unknown: r.get(28)?,
        native_plugin_grants: decode(r, 29)?,
        environment_state: r
            .get::<_, Option<String>>(30)?
            .map(|value| ConversationEnvironmentState::parse(&value).unwrap()),
        environment_expired_at: r.get(31)?,
        environment_error: r.get(32)?,
        environment_retry_after: r.get(33)?,
        account_id: r.get(34)?,
    })
}

fn decode_enum(r: &rusqlite::Row<'_>, i: usize) -> rusqlite::Result<Option<ConversationLifecycle>> {
    let value: Option<String> = r.get(i)?;
    value
        .map(|value| {
            ConversationLifecycle::parse(&value).ok_or_else(|| {
                rusqlite::Error::InvalidColumnType(
                    i,
                    "lifecycle".into(),
                    rusqlite::types::Type::Text,
                )
            })
        })
        .transpose()
}

impl Db<'_> {
    pub fn conversation_retention(&self) -> Result<cairn_contracts::ConversationRetention> {
        Ok(self
            .0
            .prepare_cached("SELECT inactivity_days FROM conversation_retention WHERE singleton=1")?
            .query_row([], |r| {
                Ok(cairn_contracts::ConversationRetention {
                    inactivity_days: r.get(0)?,
                })
            })
            .optional()?
            .unwrap_or_default())
    }

    pub fn save_conversation_retention(
        &self,
        policy: &cairn_contracts::ConversationRetention,
    ) -> Result<()> {
        if policy.inactivity_days > 36500 {
            return Err(Error::bad("Choose a whole number of days from 0 to 36500."));
        }
        self.0.prepare_cached("INSERT INTO conversation_retention VALUES(1,?) ON CONFLICT(singleton) DO UPDATE SET inactivity_days=excluded.inactivity_days")?.execute([policy.inactivity_days])?;
        Ok(())
    }

    pub fn conversation(&self, id: &str) -> Result<Option<Conversation>> {
        Ok(self
            .0
            .prepare_cached(&format!("SELECT {COLUMNS} FROM conversations WHERE id=?"))?
            .query_row([id], row)
            .optional()?)
    }

    pub fn conversations(&self) -> Result<Vec<Conversation>> {
        Ok(self
            .0
            .prepare_cached(&format!(
                "SELECT {COLUMNS} FROM conversations ORDER BY persisted_at DESC,id"
            ))?
            .query_map([], row)?
            .collect::<rusqlite::Result<_>>()?)
    }

    pub fn conversation_for_run(&self, run: &str) -> Result<Option<Conversation>> {
        Ok(self
            .0
            .prepare_cached(&format!(
                "SELECT {COLUMNS} FROM conversations WHERE run_id=?"
            ))?
            .query_row([run], row)
            .optional()?)
    }

    pub fn chat_id_for_slug(&self, slug: &str) -> Result<Option<String>> {
        Ok(self
            .0
            .prepare_cached("SELECT id FROM conversations WHERE slug=?")?
            .query_row([slug], |r| r.get(0))
            .optional()?)
    }

    fn new_conversation_slug(&self) -> Result<String> {
        loop {
            let slug = cairn_protocol::slugs::readable(uuid::Uuid::new_v4().as_bytes());
            let used: bool = self.0.query_row(
                "SELECT EXISTS(SELECT 1 FROM conversations WHERE slug=?)",
                [&slug],
                |r| r.get(0),
            )?;
            if !used {
                return Ok(slug);
            }
        }
    }

    pub fn save_conversation(&self, conversation: &Conversation) -> Result<Conversation> {
        let mut c = conversation.clone();
        let current = self.conversation(&c.id)?;
        if let Some(current) = &current {
            if current.revision != c.revision {
                return Err(Error::conflict(
                    "This conversation changed. Reload before saving.",
                ));
            }
            c.slug.clone_from(&current.slug);
        } else {
            c.slug = self.new_conversation_slug()?;
        }
        c.revision = id();
        self.0.prepare_cached("INSERT INTO conversations(id,slug,title,agent_id,project_id,run_id,paused,lifecycle,created_at,updated_at,last_activity_at,revision,trashed_at,purge_at,restored_at,retry_after,lifecycle_error,previous_lifecycle,session_restart_requested,cancelled_by_deletion,agent_name,project_name,requested_by,wakeup,native_principals,native_authority_nodes,native_parent_run_id,native_parent_access,native_provenance_unknown,native_plugin_grants,environment_state,environment_expired_at,environment_error,environment_retry_after,account_id,persisted_at,live_agent_id,live_project_id) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,(SELECT id FROM agents WHERE id=?),(SELECT id FROM projects WHERE id=?)) ON CONFLICT(id) DO UPDATE SET slug=excluded.slug,title=excluded.title,agent_id=excluded.agent_id,project_id=excluded.project_id,run_id=excluded.run_id,paused=excluded.paused,lifecycle=excluded.lifecycle,created_at=excluded.created_at,updated_at=excluded.updated_at,last_activity_at=excluded.last_activity_at,revision=excluded.revision,trashed_at=excluded.trashed_at,purge_at=excluded.purge_at,restored_at=excluded.restored_at,retry_after=excluded.retry_after,lifecycle_error=excluded.lifecycle_error,previous_lifecycle=excluded.previous_lifecycle,session_restart_requested=excluded.session_restart_requested,cancelled_by_deletion=excluded.cancelled_by_deletion,agent_name=excluded.agent_name,project_name=excluded.project_name,requested_by=excluded.requested_by,wakeup=excluded.wakeup,native_principals=excluded.native_principals,native_authority_nodes=excluded.native_authority_nodes,native_parent_run_id=excluded.native_parent_run_id,native_parent_access=excluded.native_parent_access,native_provenance_unknown=excluded.native_provenance_unknown,native_plugin_grants=excluded.native_plugin_grants,environment_state=excluded.environment_state,environment_expired_at=excluded.environment_expired_at,environment_error=excluded.environment_error,environment_retry_after=excluded.environment_retry_after,account_id=excluded.account_id,persisted_at=excluded.persisted_at,live_agent_id=excluded.live_agent_id,live_project_id=excluded.live_project_id")?.execute(params![
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

    pub(crate) fn rename_conversation_title(&self, id_value: &str, title: &str) -> Result<()> {
        self.0
            .prepare_cached("UPDATE conversations SET title=?,revision=? WHERE id=?")?
            .execute(params![title, id(), id_value])?;
        Ok(())
    }

    pub fn remove_conversation(&self, id: &str) -> Result<()> {
        self.0
            .prepare_cached("DELETE FROM conversations WHERE id=?")?
            .execute([id])?;
        Ok(())
    }

    // Temporary adapters for callers owned by A8-A11/A16/A18. They never read records.
    pub fn conversation_boundary(&self, id: &str) -> Result<Option<Value>> {
        self.conversation(id)?
            .map(serde_json::to_value)
            .transpose()
            .map_err(Into::into)
    }

    pub fn conversations_boundary(&self) -> Result<Vec<Value>> {
        self.conversations()?
            .into_iter()
            .map(|c| Ok(serde_json::to_value(c)?))
            .collect()
    }

    pub fn save_conversation_boundary(&self, value: &Value) -> Result<Value> {
        Ok(serde_json::to_value(self.save_conversation(
            &serde_json::from_value(value.clone())?,
        )?)?)
    }

    pub fn conversation_messages(&self, chat: &str) -> Result<Vec<Message>> {
        self.load_messages(chat, None)
    }

    pub fn conversation_message(&self, chat: &str, id: &str) -> Result<Option<Message>> {
        Ok(self.load_messages(chat, Some(id))?.pop())
    }

    fn load_messages(&self, chat: &str, message: Option<&str>) -> Result<Vec<Message>> {
        if self.0.is_autocommit() {
            let tx = rusqlite::Transaction::new_unchecked(
                self.0,
                rusqlite::TransactionBehavior::Deferred,
            )?;
            let result = Db(&tx).load_messages(chat, message)?;
            tx.commit()?;
            return Ok(result);
        }
        let filter = if message.is_some() {
            "chat_id=?1 AND id=?2"
        } else {
            "chat_id=?1 AND ?2 IS NULL"
        };
        let mut statement = self.0.prepare_cached(&format!("SELECT id,chat_id,revision,text,provider,model,reasoning,speed,mode,status,created_at,question_id,answers,plan FROM chat_messages WHERE {filter} ORDER BY created_at,rowid"))?;
        let rows = statement
            .query_map(params![chat, message], |r| {
                Ok((
                    ChatMessage {
                        id: r.get(0)?,
                        chat_id: r.get(1)?,
                        revision: r.get(2)?,
                        text: r.get(3)?,
                        provider: r.get(4)?,
                        model: r.get(5)?,
                        reasoning: r.get(6)?,
                        speed: r.get(7)?,
                        mode: MessageMode::parse(&r.get::<_, String>(8)?).unwrap(),
                        status: MessageStatus::parse(&r.get::<_, String>(9)?).unwrap(),
                        created_at: r.get(10)?,
                        question_id: r.get(11)?,
                        attachments: Vec::new(),
                        plan: r.get(13)?,
                    },
                    decode::<std::collections::BTreeMap<String, MessageAnswer>>(r, 12)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        // One ordered relation read for the entire conversation, avoiding N+1 per message.
        let mut attachments = std::collections::HashMap::<String, Vec<ChatAttachment>>::new();
        let attachment_filter = if message.is_some() {
            "m.chat_id=?1 AND a.message_id=?2"
        } else {
            "m.chat_id=?1 AND ?2 IS NULL"
        };
        let mut statement = self.0.prepare_cached(&format!("SELECT a.message_id,a.attachment_id,a.chat_id,a.name,a.size,a.media_type,a.kind,a.width,a.height,a.digest,a.created_at FROM message_attachments a JOIN chat_messages m ON m.id=a.message_id WHERE {attachment_filter} ORDER BY a.message_id,a.position"))?;
        for item in statement.query_map(params![chat, message], |r| {
            Ok((
                r.get::<_, String>(0)?,
                ChatAttachment {
                    id: r.get(1)?,
                    chat_id: r.get(2)?,
                    name: r.get(3)?,
                    size: r.get::<_, i64>(4)? as u64,
                    media_type: r.get(5)?,
                    kind: r.get(6)?,
                    width: r.get(7)?,
                    height: r.get(8)?,
                    digest: r.get(9)?,
                    created_at: r.get(10)?,
                },
            ))
        })? {
            let (message, attachment) = item?;
            attachments.entry(message).or_default().push(attachment);
        }
        Ok(rows
            .into_iter()
            .map(|(mut content, answers)| {
                content.attachments = attachments.remove(&content.id).unwrap_or_default();
                Message { content, answers }
            })
            .collect())
    }

    pub fn messages(&self, chat: &str) -> Result<Vec<Value>> {
        self.conversation_messages(chat)?
            .into_iter()
            .map(|m| Ok(serde_json::to_value(m)?))
            .collect()
    }

    pub fn put_message(&self, value: &Value) -> Result<Value> {
        Ok(serde_json::to_value(
            self.save_message(&serde_json::from_value(value.clone())?)?,
        )?)
    }

    pub fn save_message(&self, message: &Message) -> Result<Message> {
        if self.0.is_autocommit() {
            let tx = rusqlite::Transaction::new_unchecked(
                self.0,
                rusqlite::TransactionBehavior::Immediate,
            )?;
            let result = Db(&tx).save_message(message)?;
            tx.commit()?;
            return Ok(result);
        }
        self.0.execute_batch("SAVEPOINT conversation_message")?;
        let before = self.0.total_changes();
        let result = self.write_message(message);
        match result {
            Ok(message) => {
                self.0.execute_batch("RELEASE conversation_message")?;
                Ok(message)
            }
            Err(error) => {
                self.0.execute_batch(
                    "ROLLBACK TO conversation_message; RELEASE conversation_message",
                )?;
                let discarded = self.0.total_changes() - before;
                if discarded > 0 && self.0.query_row("SELECT EXISTS(SELECT 1 FROM pragma_function_list WHERE name='cairn_discarded_changes')", [], |r| r.get::<_, bool>(0))? {
                    self.0.query_row("SELECT cairn_discarded_changes(?)", [i64::try_from(discarded).unwrap_or(i64::MAX)], |_| Ok(()))?;
                }
                Err(error)
            }
        }
    }

    fn write_message(&self, message: &Message) -> Result<Message> {
        let mut m = message.clone();
        m.revision = id();
        self.0.prepare_cached("INSERT INTO chat_messages(id,chat_id,revision,text,provider,model,reasoning,speed,mode,status,created_at,question_id,answers,plan) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?) ON CONFLICT(id) DO UPDATE SET revision=excluded.revision,text=excluded.text,provider=excluded.provider,model=excluded.model,reasoning=excluded.reasoning,speed=excluded.speed,mode=excluded.mode,status=excluded.status,question_id=excluded.question_id,answers=excluded.answers,plan=excluded.plan")?.execute(params![m.id,m.chat_id,m.revision,m.text,m.provider,m.model,m.reasoning,m.speed,m.mode.as_str(),m.status.as_str(),m.created_at,m.question_id,m.answers.as_ref().map(serde_json::to_string).transpose()?,m.plan])?;
        self.0
            .prepare_cached("DELETE FROM message_attachments WHERE message_id=?")?
            .execute([&m.id])?;
        let mut statement = self
            .0
            .prepare_cached("INSERT INTO message_attachments VALUES(?,?,?,?,?,?,?,?,?,?,?,?)")?;
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
}

impl Store {
    pub async fn conversations(&self) -> Result<Vec<Conversation>> {
        self.read(|db| db.conversations()).await
    }

    pub async fn conversation_boundary(&self, id: &str) -> Result<Option<Value>> {
        let id = id.to_owned();
        self.read(move |db| db.conversation_boundary(&id)).await
    }

    pub async fn conversations_boundary(&self) -> Result<Vec<Value>> {
        self.read(|db| db.conversations_boundary()).await
    }

    pub async fn save_conversation_boundary(&self, value: Value) -> Result<Value> {
        let value = value.clone();
        self.transaction(move |db| db.save_conversation_boundary(&value))
            .await
    }
}
