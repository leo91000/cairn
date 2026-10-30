//! Atomic version-15 input only. No historical reader is used by the runtime.
use crate::{
    error::{Error, Result},
    store::Db,
};
use rusqlite::{Connection, TransactionBehavior};

pub(crate) fn upgrade(connection: &mut Connection) -> Result<()> {
    let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let version: i64 = tx.pragma_query_value(None, "user_version", |r| r.get(0))?;
    if matches!(version, 16..=18) {
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
    migrate_wait_snapshots(&tx)?;
    let rows = tx
        .prepare("SELECT key,data,expires FROM kv WHERE key LIKE 'chat-question:%' ORDER BY key")?
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<i64>>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (key, data, expires) in rows {
        let question: cairn_contracts::ChatQuestion =
            serde_json::from_str(&data).map_err(|_| invalid())?;
        if key != format!("chat-question:{}:{}", question.chat_id, question.id) {
            return Err(invalid());
        }
        Db(&tx).import_question(&question).map_err(|_| invalid())?;
        let mut metadata: serde_json::Value = serde_json::from_str(&data).map_err(|_| invalid())?;
        for key in [
            "id",
            "chatId",
            "runId",
            "blocking",
            "source",
            "status",
            "createdAt",
            "messageId",
        ] {
            metadata.as_object_mut().ok_or_else(invalid)?.remove(key);
        }
        if let Some(fields) = metadata["fields"].as_array_mut() {
            for field in fields {
                for key in ["id", "title", "secret", "mode", "minChoices", "maxChoices"] {
                    field.as_object_mut().ok_or_else(invalid)?.remove(key);
                }
                if let Some(options) = field
                    .get_mut("options")
                    .and_then(serde_json::Value::as_array_mut)
                {
                    for option in options {
                        for key in ["id", "label", "description", "recommended"] {
                            option.as_object_mut().ok_or_else(invalid)?.remove(key);
                        }
                    }
                }
            }
        }
        // Keep extension data and the previous expiry without exposing either in
        // the generated public model. The normalized columns remain authoritative.
        if let Some(fields) = metadata["fields"].as_array_mut() {
            for field in fields.iter_mut() {
                if field
                    .get("options")
                    .and_then(serde_json::Value::as_array)
                    .is_some_and(|options| {
                        options
                            .iter()
                            .all(|option| option.as_object().is_some_and(serde_json::Map::is_empty))
                    })
                {
                    field.as_object_mut().unwrap().remove("options");
                }
            }
            if fields
                .iter()
                .all(|field| field.as_object().is_some_and(serde_json::Map::is_empty))
            {
                metadata.as_object_mut().unwrap().remove("fields");
            }
        }
        if expires.is_some() || metadata != serde_json::json!({}) {
            tx.execute(
                "INSERT INTO question_import_metadata VALUES(?,?,?)",
                rusqlite::params![question.id, expires, serde_json::to_string(&metadata)?],
            )?;
        }
    }
    // Definitions supply the initial sensitivity snapshot for historical replies.
    migrate_answers(&tx)?;
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

// Retained events are snapshots, not current authority. Convert their owned
// condition/result fields without dropping event metadata or plugin payloads.
fn observation(value: &mut serde_json::Value, event_envelope: bool) -> Result<()> {
    if !value.is_object() {
        return Err(invalid());
    }
    if value["pluginId"].is_string() {
        let object = value.as_object_mut().unwrap();
        let mut plugin = serde_json::Map::new();
        let keys = object.keys().cloned().collect::<Vec<_>>();
        for key in keys {
            let envelope_field = matches!(key.as_str(), "state" | "type" | "display")
                || (key == "message" && object[&key].is_string())
                || (event_envelope && matches!(key.as_str(), "reason" | "immediate"));
            if !envelope_field {
                plugin.insert(key.clone(), object.remove(&key).unwrap());
            }
        }
        object.insert("pluginResult".into(), plugin.into());
    }
    if value["condition"].is_object() {
        value["condition"] = serde_json::to_value(
            crate::waits::condition_from_wire(&value["condition"]).map_err(|_| invalid())?,
        )?;
    }
    if value["type"] == "group" {
        let members = value
            .get_mut("members")
            .and_then(serde_json::Value::as_array_mut)
            .ok_or_else(invalid)?;
        for member in members {
            member["condition"] = serde_json::to_value(
                crate::waits::condition_from_wire(&member["condition"]).map_err(|_| invalid())?,
            )?;
            if !member["result"].is_null() {
                observation(&mut member["result"], false)?;
            }
        }
    }
    Ok(())
}

fn wakeup_snapshot(value: &mut serde_json::Value) -> Result<()> {
    if value["condition"].is_object() {
        value["condition"] = serde_json::to_value(
            crate::waits::condition_from_wire(&value["condition"]).map_err(|_| invalid())?,
        )?;
    }
    if value["observation"].is_object() {
        observation(&mut value["observation"], false)?;
    }
    if let Some(members) = value
        .get_mut("members")
        .and_then(serde_json::Value::as_array_mut)
    {
        for member in members {
            if member["observation"].is_object() {
                observation(&mut member["observation"], false)?;
            }
        }
    }
    Ok(())
}

fn wakeup(data: &str) -> Result<cairn_contracts::ChatWakeup> {
    let mut value: serde_json::Value = serde_json::from_str(data).map_err(|_| invalid())?;
    wakeup_snapshot(&mut value)?;
    serde_json::from_value(value).map_err(|_| invalid())
}

fn migrate_wait_snapshots(tx: &Connection) -> Result<()> {
    let events = tx.prepare("SELECT id,type,payload FROM events WHERE type IN ('chat.wait','chat.wait_ready','chat.wakeup','run.wait','run.wait_ready','run.wakeup') AND payload IS NOT NULL")?
        .query_map([], |r| Ok((r.get::<_,i64>(0)?, r.get::<_,String>(1)?, r.get::<_,String>(2)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (id, kind, payload) in events {
        let mut value: serde_json::Value = serde_json::from_str(&payload).map_err(|_| invalid())?;
        if kind.ends_with("wait_ready") {
            observation(&mut value, true)?;
        } else {
            wakeup_snapshot(&mut value)?;
        }
        tx.execute(
            "UPDATE events SET payload=? WHERE id=?",
            rusqlite::params![serde_json::to_string(&value)?, id],
        )?;
    }
    let runs = tx.prepare("SELECT id,boundary FROM runs WHERE json_type(boundary,'$.waitContinuation')='object' OR json_type(boundary,'$.chatExecution.wakeup')='object'")?
        .query_map([], |r| Ok((r.get::<_,String>(0)?, r.get::<_,String>(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (id, boundary) in runs {
        let mut value: serde_json::Value =
            serde_json::from_str(&boundary).map_err(|_| invalid())?;
        if value["waitContinuation"].is_object() {
            wakeup_snapshot(&mut value["waitContinuation"])?;
        }
        if value["chatExecution"]["wakeup"].is_object() {
            wakeup_snapshot(&mut value["chatExecution"]["wakeup"])?;
        }
        tx.execute(
            "UPDATE runs SET boundary=? WHERE id=?",
            rusqlite::params![serde_json::to_string(&value)?, id],
        )?;
    }
    Ok(())
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
        let answers: std::collections::BTreeMap<String, super::storage::MessageAnswer> =
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
