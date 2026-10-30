//! Version-14 input only; current conversations never fall back to JSON records.
mod storage_v15;

use crate::{
    config::id,
    error::{Error, Result},
    store::Db,
};
use rusqlite::{Connection, TransactionBehavior, params};
use serde::Deserialize;
use storage_v15::{ConversationV15 as Conversation, MessageV15 as Message};

pub(crate) fn upgrade(connection: &mut Connection) -> Result<()> {
    let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let version: i64 = tx.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if matches!(version, 15 | 16) {
        return Ok(());
    }
    if version != 14 {
        return Err(Error::bad(
            "Unexpected installation database version during conversation upgrade.",
        ));
    }

    tx.execute_batch(
        "ALTER TABLE chat_messages RENAME TO chat_messages_v14; DROP INDEX chat_messages_chat;",
    )?;
    tx.execute_batch(include_str!("migration/schema-v15.sql"))?;
    tx.execute_batch(super::engine_switch::SCHEMA)?;
    let conversations = tx
        .prepare("SELECT id,data,updated_at FROM records WHERE kind='chats' ORDER BY id")?
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (key, data, persisted_at) in conversations {
        let mut conversation: Conversation = decode(&data)?;
        if conversation.id != key || conversation.slug.is_empty() {
            return Err(invalid());
        }
        let slug = conversation.slug.clone();
        if conversation.revision.is_empty() {
            conversation.revision = id();
        }
        storage_v15::save_conversation(&Db(&tx), &conversation)?;
        tx.execute(
            "UPDATE conversations SET slug=?,revision=?,persisted_at=? WHERE id=?",
            params![slug, conversation.revision, persisted_at, key],
        )?;
    }

    // rowid breaks equal timestamp ties in the old reader; preserve that order.
    let messages = tx
        .prepare("SELECT id,chat_id,data,created_at FROM chat_messages_v14 ORDER BY rowid")?
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (key, chat, data, created_at) in messages {
        let mut message: Message = decode(&data)?;
        if message.id != key || message.chat_id != chat || message.created_at != created_at {
            return Err(invalid());
        }
        if message.revision.is_empty() {
            message.revision = id();
        }
        storage_v15::save_message(&Db(&tx), &message)?;
        tx.execute(
            "UPDATE chat_messages SET revision=? WHERE id=?",
            params![message.revision, key],
        )?;
    }

    migrate_switches(&tx)?;
    let retention: Option<String> = tx
        .query_row(
            "SELECT data FROM kv WHERE key='conversation-expiration'",
            [],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(data) = retention {
        let policy: cairn_contracts::ConversationRetention = decode(&data)?;
        tx.execute(
            "INSERT INTO conversation_retention VALUES(1,?)",
            [policy.inactivity_days],
        )?;
    }

    tx.execute_batch("DROP TABLE chat_messages_v14;
CREATE TABLE records_v15(id TEXT PRIMARY KEY,kind TEXT NOT NULL CHECK(kind NOT IN ('agentAccounts','chats','nodes','projects','agents','tasks','mcps','plugin-connections','plugin-packages','plugin-requests','plugin-activity','plugin-timers','plugin-events','plugin-deliveries','plugin-storage','plugin-http-receipts')),data TEXT NOT NULL,updated_at INTEGER NOT NULL);
INSERT INTO records_v15 SELECT id,kind,data,updated_at FROM records WHERE kind!='chats';
DROP TABLE records;
ALTER TABLE records_v15 RENAME TO records;
CREATE INDEX records_kind ON records(kind,updated_at DESC);
CREATE INDEX records_node_backups_run ON records(json_extract(data,'$.runId')) WHERE kind='node-backups';
CREATE TABLE kv_v15(key TEXT PRIMARY KEY CHECK(key NOT LIKE 'run-checkpoint:%' AND key NOT LIKE 'chat-coding-agent:%' AND key NOT LIKE 'chat-coding-agent-request:%' AND key!='conversation-expiration'),data TEXT NOT NULL,expires INTEGER);
INSERT INTO kv_v15 SELECT * FROM kv WHERE key NOT LIKE 'chat-coding-agent:%' AND key NOT LIKE 'chat-coding-agent-request:%' AND key!='conversation-expiration';
DROP TABLE kv;
ALTER TABLE kv_v15 RENAME TO kv;")?;
    if tx.prepare("PRAGMA foreign_key_check")?.exists([])? {
        return Err(invalid());
    }
    tx.pragma_update(None, "user_version", 15)?;
    tx.commit()?;
    Ok(())
}

use rusqlite::OptionalExtension;

fn invalid() -> Error {
    Error::bad("Cannot upgrade version 14 conversations; original database retained.")
}

fn decode<T: serde::de::DeserializeOwned>(data: &str) -> Result<T> {
    // Private answers and native provenance must never enter diagnostics.
    serde_json::from_str(data).map_err(|_| invalid())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Switch {
    chat_id: String,
    request_id: String,
    run_id: String,
    turn_id: String,
    provider: String,
    #[serde(default)]
    model: String,
    #[serde(default)]
    pending: bool,
    error: Option<String>,
    retry_at: Option<i64>,
}

fn migrate_switches(tx: &Connection) -> Result<()> {
    // Receipts precede current state, whose progress/model/expiry is authoritative.
    let rows = tx.prepare("SELECT key,data,expires FROM kv WHERE key LIKE 'chat-coding-agent-request:%' OR key LIKE 'chat-coding-agent:%' ORDER BY CASE WHEN key LIKE 'chat-coding-agent-request:%' THEN 0 ELSE 1 END,key")?
        .query_map([], |r| Ok((r.get::<_, String>(0)?,r.get::<_, String>(1)?,r.get::<_, Option<i64>>(2)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (key, data, expires) in rows {
        let value: Switch = decode(&data)?;
        let current = key.starts_with("chat-coding-agent:");
        let expected = if current {
            format!("chat-coding-agent:{}", value.chat_id)
        } else {
            format!(
                "chat-coding-agent-request:{}:{}",
                value.chat_id, value.request_id
            )
        };
        if key != expected || (!current && expires.is_none()) {
            return Err(invalid());
        }
        // Version 12 purge did not delete these keys. They cannot be queried
        // after their conversation is gone; do not resurrect deleted work.
        let parent_exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM conversations WHERE id=?)",
            [&value.chat_id],
            |r| r.get(0),
        )?;
        if !parent_exists {
            continue;
        }
        tx.execute("INSERT INTO conversation_engine_switches(chat_id,request_id,run_id,turn_id,provider,model,pending,error,retry_at,receipt_expires_at,current_expires_at,is_current,revision) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?) ON CONFLICT(chat_id,request_id) DO UPDATE SET model=excluded.model,pending=excluded.pending,error=excluded.error,retry_at=excluded.retry_at,current_expires_at=excluded.current_expires_at,is_current=excluded.is_current",
            params![value.chat_id,value.request_id,value.run_id,value.turn_id,value.provider,value.model,value.pending,value.error,value.retry_at,expires.unwrap_or(0),if current { expires } else { None },current,id()])?;
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests;
