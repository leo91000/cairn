//! One-time reader of version 13 account records, never a runtime fallback.
use super::{Account, usage::Usage};
use crate::{
    error::{Error, Result},
    store::Db,
};
use rusqlite::{Connection, TransactionBehavior, params};
use serde_json::Value;

pub(crate) fn upgrade(connection: &mut Connection) -> Result<()> {
    let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let version: i64 = tx.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if matches!(version, 14..=19) {
        return Ok(());
    }
    if version != 13 {
        return Err(Error::bad(
            "Unexpected installation database version during account upgrade.",
        ));
    }

    tx.execute_batch(super::sql::SCHEMA)?;
    let records = tx
        .prepare("SELECT id,data,updated_at FROM records WHERE kind='agentAccounts' ORDER BY id")?
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (id, data, updated_at) in records {
        let account = decode(&id, &data)?;
        Db(&tx).save_coding_account(&account)?;
        tx.execute(
            "UPDATE coding_agent_accounts SET updated_at=? WHERE id=?",
            params![updated_at, id],
        )?;
    }

    // No table references records. Preserve all other rows and ordering bytes.
    // Version 13 leases were process-local; there are no persisted slots to copy.
    // Run account references and encrypted vault/KV rows stay byte-for-byte intact.
    tx.execute_batch("CREATE TABLE records_v14(id TEXT PRIMARY KEY,kind TEXT NOT NULL CHECK(kind NOT IN ('agentAccounts','nodes','projects','agents','tasks','mcps','plugin-connections','plugin-packages','plugin-requests','plugin-activity','plugin-timers','plugin-events','plugin-deliveries','plugin-storage','plugin-http-receipts')),data TEXT NOT NULL,updated_at INTEGER NOT NULL);
INSERT INTO records_v14 SELECT id,kind,data,updated_at FROM records WHERE kind!='agentAccounts';
DROP TABLE records;
ALTER TABLE records_v14 RENAME TO records;
CREATE INDEX records_kind ON records(kind,updated_at DESC);
CREATE INDEX records_node_backups_run ON records(json_extract(data,'$.runId')) WHERE kind='node-backups';
CREATE UNIQUE INDEX records_chat_slug ON records(json_extract(data,'$.slug')) WHERE kind='chats';")?;
    if tx.prepare("PRAGMA foreign_key_check")?.exists([])? {
        return Err(Error::bad(
            "Installation account upgrade failed its foreign key check.",
        ));
    }
    tx.pragma_update(None, "user_version", 14)?;
    tx.commit()?;
    Ok(())
}

fn decode(id: &str, data: &str) -> Result<Account> {
    // Never include input or serde errors: records may hold private account data.
    let invalid = || Error::bad("Cannot upgrade version 13 accounts; original database retained.");
    let mut value: Value = serde_json::from_str(data).map_err(|_| invalid())?;
    if value.get("id").and_then(Value::as_str) != Some(id) {
        return Err(invalid());
    }
    // Refuse unknown data rather than silently discard a future business field.
    let object = value.as_object().ok_or_else(invalid)?;
    for field in object.keys() {
        if !matches!(
            field.as_str(),
            "id" | "provider"
                | "name"
                | "enabled"
                | "email"
                | "plan"
                | "identity"
                | "createdAt"
                | "checkedAt"
                | "state"
                | "error"
                | "usage"
                | "lastUsedAt"
                | "exhausted"
                | "maxConcurrentRuns"
                | "resetError"
                | "priority"
                | "modelProvider"
                | "modelProviderName"
        ) {
            return Err(invalid());
        }
    }
    // Unlike the live provider boundary, migration must not turn malformed usage
    // into unknown usage and silently erase it. Check both readings strictly.
    for usage in [
        value.get("usage"),
        value.get("exhausted").and_then(|e| e.get("usage")),
    ]
    .into_iter()
    .flatten()
    {
        if !usage.is_null() {
            check_fields(
                usage,
                &[
                    "allowed",
                    "windows",
                    "checkedAt",
                    "attemptedAt",
                    "resets",
                    "error",
                    "useBalance",
                ],
            )?;
            if let Some(windows) = usage.get("windows").and_then(Value::as_array) {
                for window in windows {
                    check_fields(
                        window,
                        &[
                            "id",
                            "label",
                            "usedPercent",
                            "resetsAt",
                            "durationMins",
                            "models",
                            "reached",
                        ],
                    )?;
                }
            }
            if let Some(resets) = usage.get("resets").filter(|r| !r.is_null()) {
                check_fields(resets, &["available", "credits"])?;
            }
            serde_json::from_value::<Usage>(usage.clone()).map_err(|_| invalid())?;
        }
    }
    if let Some(exhausted) = value.get("exhausted").filter(|e| !e.is_null()) {
        check_fields(exhausted, &["at", "model", "usage"])?;
    }
    value["revision"] = uuid::Uuid::new_v4().to_string().into();
    serde_json::from_value(value).map_err(|_| invalid())
}

fn check_fields(value: &Value, allowed: &[&str]) -> Result<()> {
    if value
        .as_object()
        .is_none_or(|o| o.keys().any(|key| !allowed.contains(&key.as_str())))
    {
        return Err(Error::bad(
            "Cannot upgrade version 13 accounts; original database retained.",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
