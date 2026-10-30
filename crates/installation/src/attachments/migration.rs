//! The v16 KV layout is accepted only by this atomic startup migration.
use crate::{
    error::{Error, Result},
    store::Db,
};
use cairn_contracts::ChatAttachment;
use rusqlite::{Connection, TransactionBehavior};

pub(crate) fn upgrade(connection: &mut Connection) -> Result<()> {
    let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let version: i64 = tx.pragma_query_value(None, "user_version", |row| row.get(0))?;
    // Another startup may have completed this and the later upgrades.
    if matches!(version, 17..=19) {
        return Ok(());
    }
    if version != 16 {
        return Err(Error::bad(
            "Unexpected installation database version during attachment upgrade.",
        ));
    }
    tx.execute_batch(super::sql::SCHEMA)?;
    let rows = tx
        .prepare("SELECT key,data,expires FROM kv WHERE key LIKE 'chat-attachment:%' ORDER BY key")?
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<i64>>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (key, data, expires) in rows {
        let a: ChatAttachment = serde_json::from_str(&data)?;
        crate::validation::uuid(&a.chat_id)?;
        crate::validation::uuid(&a.id)?;
        if key != format!("chat-attachment:{}:{}", a.chat_id, a.id) || expires.is_some() {
            return Err(Error::bad(
                "Invalid previous attachment identity or expiry; original database retained.",
            ));
        }
        Db(&tx).insert_attachment(&a, &a.id)?;
    }
    tx.execute_batch("CREATE TABLE kv_v17(key TEXT PRIMARY KEY CHECK(key NOT LIKE 'run-checkpoint:%' AND key NOT LIKE 'chat-coding-agent:%' AND key NOT LIKE 'chat-coding-agent-request:%' AND key NOT LIKE 'chat-question:%' AND key NOT LIKE 'chat-attachment:%' AND key!='conversation-expiration'),data TEXT NOT NULL,expires INTEGER);
INSERT INTO kv_v17 SELECT * FROM kv WHERE key NOT LIKE 'chat-attachment:%';
DROP TABLE kv;
ALTER TABLE kv_v17 RENAME TO kv;")?;
    if tx.prepare("PRAGMA foreign_key_check")?.exists([])? {
        return Err(Error::bad(
            "Invalid attachment references; original database retained.",
        ));
    }
    tx.pragma_update(None, "user_version", 17)?;
    tx.commit()?;
    Ok(())
}
