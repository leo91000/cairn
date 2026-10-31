//! Version-21 input only: the timeline projection is built from the events every
//! version already retains. No event, run or other domain row is rewritten.
use crate::{
    error::{Error, Result},
    store::{Db, Event},
};
use cairn_contracts::EventKind;
use rusqlite::{Connection, TransactionBehavior};

pub(crate) fn upgrade(connection: &mut Connection) -> Result<()> {
    let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let version: i64 = tx.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if (22..=crate::store::DATABASE_VERSION).contains(&version) {
        return Ok(());
    }
    if version != 21 {
        return Err(Error::bad(
            "Unexpected installation database version during timeline upgrade.",
        ));
    }
    tx.execute_batch(super::SCHEMA).map_err(|_| invalid())?;

    let runs = tx
        .prepare("SELECT DISTINCT run_id FROM events ORDER BY run_id")?
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for run in runs {
        // Each run is replayed in event order with the writer's reducer, one row at a time.
        let mut replay = super::Replay::default();
        let mut events = tx.prepare_cached(
            "SELECT id,run_id,created_at,type,text,payload FROM events WHERE run_id=? ORDER BY id",
        )?;
        let mut rows = events.query([&run])?;
        while let Some(row) = rows.next()? {
            let event = Event {
                id: row.get(0)?,
                run_id: row.get(1)?,
                created_at: row.get(2)?,
                kind: EventKind::from_adapter(&row.get::<_, String>(3)?),
                text: row.get(4)?,
                payload: row
                    .get::<_, Option<String>>(5)?
                    .map(serde_json::value::RawValue::from_string)
                    .transpose()?,
                display_id: None,
                timeline: false,
            };
            replay.apply(&event).map_err(|_| invalid())?;
        }
        drop(rows);
        drop(events);
        let (state, cards) = replay.finish();
        let db = Db(&tx);
        for card in &cards {
            super::save_card(&db, &run, card).map_err(|_| invalid())?;
        }
        super::save_state(&db, &run, &state).map_err(|_| invalid())?;
    }
    if tx.prepare("PRAGMA foreign_key_check")?.exists([])? {
        return Err(invalid());
    }
    tx.pragma_update(None, "user_version", 22)?;
    tx.commit()?;
    Ok(())
}

fn invalid() -> Error {
    Error::bad("Cannot build the version-22 timeline; original database retained.")
}
