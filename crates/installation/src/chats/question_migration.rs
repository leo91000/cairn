//! Atomic version-15 input only. No historical reader is used by the runtime.
use crate::{
    error::{Error, Result},
    store::Db,
};
use rusqlite::{Connection, TransactionBehavior};

pub(crate) fn upgrade(connection: &mut Connection) -> Result<()> {
    let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let version: i64 = tx.pragma_query_value(None, "user_version", |r| r.get(0))?;
    if version == 16 {
        return Ok(());
    }
    if version != 15 {
        return Err(Error::bad(
            "Unexpected installation database version during questionnaire upgrade.",
        ));
    }
    tx.execute_batch(super::question_storage::SCHEMA)?;
    tx.execute_batch(crate::waits::sql::SCHEMA)?;
    migrate_waits_and_lineage(&tx)?;
    migrate_answers(&tx)?;
    let rows = tx
        .prepare("SELECT key,data FROM kv WHERE key LIKE 'chat-question:%' ORDER BY key")?
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (key, data) in rows {
        let question: cairn_contracts::ChatQuestion =
            serde_json::from_str(&data).map_err(|_| invalid())?;
        if key != format!("chat-question:{}:{}", question.chat_id, question.id) {
            return Err(invalid());
        }
        Db(&tx).save_question(&question).map_err(|_| invalid())?;
    }
    tx.execute_batch("CREATE TABLE kv_v16(key TEXT PRIMARY KEY CHECK(key NOT LIKE 'run-checkpoint:%' AND key NOT LIKE 'chat-coding-agent:%' AND key NOT LIKE 'chat-coding-agent-request:%' AND key NOT LIKE 'chat-question:%' AND key!='conversation-expiration'),data TEXT NOT NULL,expires INTEGER);
INSERT INTO kv_v16 SELECT * FROM kv WHERE key NOT LIKE 'chat-question:%';
DROP TABLE kv;
ALTER TABLE kv_v16 RENAME TO kv;")?;
    if tx.prepare("PRAGMA foreign_key_check")?.exists([])? {
        return Err(invalid());
    }
    tx.pragma_update(None, "user_version", 16)?;
    tx.commit()?;
    Ok(())
}

fn wakeup(data: &str) -> Result<cairn_contracts::ChatWakeup> {
    let mut value: serde_json::Value = serde_json::from_str(data).map_err(|_| invalid())?;
    if value["condition"].is_object() {
        value["condition"] = serde_json::to_value(
            crate::waits::condition_from_wire(&value["condition"]).map_err(|_| invalid())?,
        )?;
    }
    if value["observation"].is_object() {
        value["observation"] = serde_json::to_value(
            crate::waits::observed_from_wire(&value["observation"]).map_err(|_| invalid())?,
        )?;
    }
    if let Some(members) = value["members"].as_array_mut() {
        for member in members {
            if member["observation"].is_object() {
                member["observation"] = serde_json::to_value(
                    crate::waits::observed_from_wire(&member["observation"])
                        .map_err(|_| invalid())?,
                )?;
            }
        }
    }
    serde_json::from_value(value).map_err(|_| invalid())
}

fn migrate_waits_and_lineage(tx: &Connection) -> Result<()> {
    let rows = tx
        .prepare("SELECT id,wakeup,requested_by FROM conversations ORDER BY id")?
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, Option<String>>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (id, wait, requester) in rows {
        if let Some(wait) = wait {
            Db(tx)
                .save_wakeup(true, &id, Some(&wakeup(&wait)?))
                .map_err(|_| invalid())?;
        }
        let requester = requester
            .map(|r| {
                serde_json::from_str::<cairn_contracts::ConversationRequester>(&r)
                    .map_err(|_| invalid())
            })
            .transpose()?;
        Db(tx)
            .save_lineage(&id, requester.as_ref())
            .map_err(|_| invalid())?;
    }
    let rows=tx.prepare("SELECT id,json_extract(boundary,'$.wakeup') FROM runs WHERE json_type(boundary,'$.wakeup')='object'")?
        .query_map([],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
    for (id, wait) in rows {
        Db(tx)
            .save_wakeup(false, &id, Some(&wakeup(&wait)?))
            .map_err(|_| invalid())?;
    }
    tx.execute_batch("UPDATE runs SET boundary=json_remove(boundary,'$.wakeup') WHERE json_type(boundary,'$.wakeup') IS NOT NULL;
ALTER TABLE conversations DROP COLUMN requested_by;
ALTER TABLE conversations DROP COLUMN wakeup;")?;
    Ok(())
}

fn migrate_answers(tx: &Connection) -> Result<()> {
    let rows = tx
        .prepare(
            "SELECT id,text,answers FROM chat_messages WHERE answers IS NOT NULL ORDER BY rowid",
        )?
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (id, text, answers) in rows {
        let answers: std::collections::BTreeMap<String, cairn_contracts::QuestionAnswer> =
            serde_json::from_str(&answers).map_err(|_| invalid())?;
        Db(tx)
            .save_message_answers(&id, Some(&answers), &text)
            .map_err(|_| invalid())?;
        tx.execute(
            "UPDATE chat_messages SET text='Questionnaire response' WHERE id=?",
            [id],
        )?;
    }
    tx.execute_batch("ALTER TABLE chat_messages DROP COLUMN answers;")?;
    Ok(())
}

fn invalid() -> Error {
    Error::bad("Cannot upgrade version 15 questions and waits; original database retained.")
}
