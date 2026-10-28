//! One-time input reader for the version 11 JSON inventory. Never used at runtime.
use crate::{
    error::{Error, Result},
    store::Db,
};
use cairn_contracts::{ExecutionNode, StaleDisks};
use rusqlite::{Connection, TransactionBehavior, params};
use serde_json::{Value, json};

pub(crate) fn upgrade(connection: &mut Connection) -> Result<()> {
    let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    // Recheck under the write lock: another startup may have completed the upgrade.
    let version: i64 = tx.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version == 12 {
        return Ok(());
    }
    if version != 11 {
        return Err(Error::bad(
            "Unexpected installation database version during upgrade.",
        ));
    }

    tx.execute_batch(super::SCHEMA)?;
    let records = tx
        .prepare("SELECT id,data,updated_at FROM records WHERE kind='nodes' ORDER BY id")?
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (id, data, updated_at) in records {
        let node = decode(&id, &data)?;
        Db(&tx).save_node(&node)?;
        tx.execute(
            "UPDATE nodes SET updated_at=?1 WHERE id=?2",
            params![updated_at, id],
        )?;
    }

    // Keep every non-inventory record byte-for-byte, including its ordering timestamp.
    // No table references records: domain references retain the same node UUIDs.
    tx.execute_batch(
        "CREATE TABLE records_v12(id TEXT PRIMARY KEY,kind TEXT NOT NULL CHECK(kind NOT IN ('nodes','projects','agents','tasks','mcps','plugin-connections','plugin-packages','plugin-requests','plugin-activity','plugin-timers','plugin-events','plugin-deliveries','plugin-storage','plugin-http-receipts')),data TEXT NOT NULL,updated_at INTEGER NOT NULL);
INSERT INTO records_v12 SELECT id,kind,data,updated_at FROM records WHERE kind!='nodes';
DROP TABLE records;
ALTER TABLE records_v12 RENAME TO records;
CREATE INDEX records_kind ON records(kind,updated_at DESC);
CREATE INDEX records_node_backups_run ON records(json_extract(data,'$.runId')) WHERE kind='node-backups';
CREATE UNIQUE INDEX records_chat_slug ON records(json_extract(data,'$.slug')) WHERE kind='chats';",
    )?;
    if tx.prepare("PRAGMA foreign_key_check")?.exists([])? {
        return Err(Error::bad(
            "Installation upgrade failed its foreign key check.",
        ));
    }
    tx.pragma_update(None, "user_version", 12)?;
    tx.commit()?;
    Ok(())
}

fn decode(id: &str, data: &str) -> Result<ExecutionNode> {
    // Do not include deserializer errors: old records can contain private data.
    let invalid =
        || Error::bad("Cannot upgrade version 11 node inventory; original database retained.");
    let mut value: Value = serde_json::from_str(data).map_err(|_| invalid())?;
    let object = value.as_object_mut().ok_or_else(invalid)?;
    if object.get("id").and_then(Value::as_str) != Some(id) {
        return Err(invalid());
    }
    object
        .entry("revision")
        .or_insert_with(|| json!(uuid::Uuid::new_v4()));
    object
        .entry("staleDisks")
        .or_insert(serde_json::to_value(StaleDisks::default())?);
    // These old response projections/observations have no inventory column in
    // version 12. The authoritative update timestamp is copied from records.
    for field in [
        "sharedResources",
        "releasedPlacements",
        "runtimes",
        "updatedAt",
    ] {
        object.remove(field);
    }
    // Legacy heartbeat writers explicitly stored null when a controller omitted
    // this optional capability. The new contract expresses it as false/zero.
    for field in ["exclusiveReservations", "executionReady"] {
        if object.get(field).is_some_and(Value::is_null) {
            object.remove(field);
        }
    }
    if let Some(capabilities) = object
        .get_mut("capabilities")
        .and_then(Value::as_object_mut)
    {
        // Version 12 admits storage through ublk; historical fuse observations
        // never grant capabilities or alter the enrolled execution backend.
        capabilities.remove("fuse");
        if capabilities
            .get("nativeModeProtocol")
            .is_some_and(Value::is_null)
        {
            capabilities.remove("nativeModeProtocol");
        }
    }
    if object.get("storage").is_none_or(Value::is_null) {
        object.insert("storage".into(), json!({}));
    }
    let node: ExecutionNode = serde_json::from_value(value).map_err(|_| invalid())?;
    // UUID parsing accepts alternate spellings. Preserve every string reference
    // and prevent an UPSERT from merging two differently spelled legacy keys.
    if node.id.to_string() != id {
        return Err(invalid());
    }
    Ok(node)
}
