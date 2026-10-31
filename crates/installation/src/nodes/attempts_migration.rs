//! Startup-only reader of the previous document layout.
use super::attempts::Attempt;
use crate::{
    error::{Error, Result},
    store::Db,
};
use rusqlite::{Connection, TransactionBehavior};

pub(crate) fn upgrade(connection: &mut Connection) -> Result<()> {
    let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let version: i64 = tx.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if (22..=crate::store::DATABASE_VERSION).contains(&version) {
        tx.commit()?;
        return Ok(());
    }
    if version != 21 {
        return Err(Error::bad(
            "Unexpected database version during placement upgrade.",
        ));
    }
    tx.execute_batch(super::attempts::SCHEMA)?;
    tx.execute_batch(super::exclusive_state::SCHEMA)?;
    let records = tx
        .prepare("SELECT id,data FROM records WHERE kind='node-attempts' ORDER BY updated_at,id")?
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (id, data) in records {
        let invalid =
            || Error::bad("Cannot upgrade attempt ownership; original database retained.");
        let attempt: Attempt = serde_json::from_str(&data).map_err(|_| invalid())?;
        if attempt.id != id {
            return Err(invalid());
        }
        Db(&tx).save_attempt(&attempt).map_err(|_| invalid())?;
    }
    let records = tx.prepare("SELECT id,data FROM records WHERE kind='node-exclusive-requests' ORDER BY updated_at,id")?.query_map([], |row| Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
    for (id, data) in records {
        let invalid =
            || Error::bad("Cannot upgrade exclusive reservations; original database retained.");
        let record: super::exclusive_state::Reservation =
            serde_json::from_str(&data).map_err(|_| invalid())?;
        if record.id != id {
            return Err(invalid());
        }
        Db(&tx)
            .save_exclusive_reservation(&record)
            .map_err(|_| invalid())?;
    }
    tx.execute("UPDATE exclusive_sequence SET queue_order=max(coalesce((SELECT json_extract(data,'$') FROM kv WHERE key='exclusive-request-order'),0),coalesce((SELECT max(queue_order) FROM exclusive_reservations),0)), epoch=max(coalesce((SELECT json_extract(data,'$') FROM kv WHERE key='exclusive-epoch'),0),coalesce((SELECT max(epoch) FROM exclusive_reservations),0)) WHERE singleton=1",[])?;
    tx.execute(
        "DELETE FROM kv WHERE key IN ('exclusive-request-order','exclusive-epoch')",
        [],
    )?;
    tx.execute_batch(super::run_placement::ALTER)?;
    let runs = tx
        .prepare("SELECT id,boundary FROM runs")?
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (id, data) in runs {
        let invalid = || Error::bad("Cannot upgrade run placement; original database retained.");
        let mut boundary: serde_json::Value = serde_json::from_str(&data).map_err(|_| invalid())?;
        let mut placement = serde_json::Map::new();
        for field in super::run_placement::FIELDS {
            if let Some(value) = boundary.as_object_mut().ok_or_else(invalid)?.remove(*field) {
                placement.insert((*field).into(), value);
            }
        }
        let placement: super::run_placement::RunPlacement =
            serde_json::from_value(serde_json::Value::Object(placement)).map_err(|_| invalid())?;
        Db(&tx)
            .save_run_placement(&id, &placement)
            .map_err(|_| invalid())?;
        tx.execute(
            "UPDATE runs SET boundary=? WHERE id=?",
            rusqlite::params![boundary.to_string(), id],
        )?;
    }
    tx.execute_batch(crate::runs::REBUILD_PROJECTIONS)?;
    if tx
        .prepare("SELECT 1 FROM pragma_table_info('nodes') WHERE name='exclusive_reservation'")?
        .exists([])?
    {
        tx.execute_batch("ALTER TABLE nodes DROP COLUMN exclusive_reservation")?;
    }
    tx.execute_batch("CREATE TABLE records_v22(id TEXT PRIMARY KEY,kind TEXT NOT NULL CHECK(kind NOT IN ('agentAccounts','chats','node-exclusive-requests','node-attempts','nodes','projects','agents','tasks','mcps','plugin-connections','plugin-packages','plugin-requests','plugin-activity','plugin-timers','plugin-events','plugin-deliveries','plugin-storage','plugin-http-receipts')),data TEXT NOT NULL,updated_at INTEGER NOT NULL);
INSERT INTO records_v22 SELECT id,kind,data,updated_at FROM records WHERE kind NOT IN ('node-attempts','node-exclusive-requests');
DROP TABLE records;
ALTER TABLE records_v22 RENAME TO records;
CREATE INDEX records_kind ON records(kind,updated_at DESC);
CREATE INDEX records_node_backups_run ON records(json_extract(data,'$.runId')) WHERE kind='node-backups';")?;
    if tx.prepare("PRAGMA foreign_key_check")?.exists([])? {
        return Err(Error::bad(
            "Placement upgrade failed its foreign key check.",
        ));
    }
    tx.pragma_update(None, "user_version", 22)?;
    tx.commit()?;
    Ok(())
}
