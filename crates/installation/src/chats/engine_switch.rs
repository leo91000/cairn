//! Durable recovery of a quota-blocked conversation turn.
use crate::{
    config::{id, now},
    error::{Error, Result},
    store::Db,
};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EngineSwitch {
    pub chat_id: String,
    pub request_id: String,
    pub run_id: String,
    pub turn_id: String,
    pub provider: String,
    pub model: String,
    pub pending: bool,
    pub error: Option<String>,
    pub retry_at: Option<i64>,
    pub receipt_expires_at: i64,
    pub current_expires_at: Option<i64>,
}

pub(crate) const SCHEMA: &str = "
CREATE TABLE conversation_engine_switches(
    chat_id TEXT NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
    request_id TEXT NOT NULL,
    run_id TEXT NOT NULL REFERENCES runs(id) ON DELETE CASCADE,
    turn_id TEXT NOT NULL CHECK(length(turn_id) BETWEEN 1 AND 200),
    provider TEXT NOT NULL CHECK(provider IN ('codex','claude','opencode')),
    model TEXT NOT NULL,
    pending INTEGER NOT NULL CHECK(pending IN (0,1)),
    error TEXT,
    retry_at INTEGER,
    receipt_expires_at INTEGER NOT NULL,
    current_expires_at INTEGER,
    is_current INTEGER NOT NULL CHECK(is_current IN (0,1)),
    revision TEXT NOT NULL,
    PRIMARY KEY(chat_id,request_id)
) WITHOUT ROWID;
CREATE UNIQUE INDEX conversation_engine_switch_current ON conversation_engine_switches(chat_id) WHERE is_current=1;
";

const COLUMNS: &str = "chat_id,request_id,run_id,turn_id,provider,model,pending,error,retry_at,receipt_expires_at,current_expires_at";

fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<EngineSwitch> {
    Ok(EngineSwitch {
        chat_id: r.get(0)?,
        request_id: r.get(1)?,
        run_id: r.get(2)?,
        turn_id: r.get(3)?,
        provider: r.get(4)?,
        model: r.get(5)?,
        pending: r.get(6)?,
        error: r.get(7)?,
        retry_at: r.get(8)?,
        receipt_expires_at: r.get(9)?,
        current_expires_at: r.get(10)?,
    })
}

impl Db<'_> {
    pub fn engine_switch(&self, chat: &str) -> Result<Option<EngineSwitch>> {
        Ok(self.0.prepare_cached(&format!("SELECT {COLUMNS} FROM conversation_engine_switches WHERE chat_id=? AND is_current=1 AND (current_expires_at IS NULL OR current_expires_at>?)"))?.query_row(params![chat,now()],row).optional()?)
    }

    pub fn engine_switch_receipt(&self, chat: &str, request: &str) -> Result<Option<EngineSwitch>> {
        Ok(self.0.prepare_cached(&format!("SELECT {COLUMNS} FROM conversation_engine_switches WHERE chat_id=? AND request_id=? AND (receipt_expires_at>? OR (is_current=1 AND (current_expires_at IS NULL OR current_expires_at>?)))"))?.query_row(params![chat,request,now(),now()],row).optional()?)
    }

    pub fn save_engine_switch(&self, value: &EngineSwitch) -> Result<()> {
        // Called in the conversation writer transaction. Retain earlier idempotency
        // receipts while making exactly one request current for this conversation.
        if value.provider.is_empty()
            || !matches!(value.provider.as_str(), "codex" | "claude" | "opencode")
            || value.turn_id.is_empty()
            || value.turn_id.chars().count() > 200
        {
            return Err(Error::bad("Invalid coding agent switch."));
        }
        self.0.prepare_cached("DELETE FROM conversation_engine_switches WHERE receipt_expires_at<=? AND (is_current=0 OR current_expires_at<=?)")?.execute(params![now(),now()])?;
        self.0.prepare_cached("UPDATE conversation_engine_switches SET is_current=0 WHERE chat_id=? AND request_id<>?")?.execute(params![value.chat_id,value.request_id])?;
        self.0.prepare_cached("INSERT INTO conversation_engine_switches(chat_id,request_id,run_id,turn_id,provider,model,pending,error,retry_at,receipt_expires_at,current_expires_at,is_current,revision) VALUES(?,?,?,?,?,?,?,?,?,?,?,1,?) ON CONFLICT(chat_id,request_id) DO UPDATE SET pending=excluded.pending,error=excluded.error,retry_at=excluded.retry_at,current_expires_at=excluded.current_expires_at,is_current=1,revision=excluded.revision")?.execute(params![value.chat_id,value.request_id,value.run_id,value.turn_id,value.provider,value.model,value.pending,value.error,value.retry_at,value.receipt_expires_at,value.current_expires_at,id()])?;
        Ok(())
    }
}
