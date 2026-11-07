//! Plugin-owned event delivery and opaque contribution storage. Queue lifecycle
//! fields are typed; plugin payloads, routing/reply state and external HTTP bodies
//! remain extensible. Question retry snapshots serialize named Rust collections.
use crate::{
    error::Result,
    store::{Db, Store},
};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub(crate) const SCHEMA: &str = "
CREATE TABLE plugin_events(
 id TEXT PRIMARY KEY NOT NULL,
 instance_id TEXT NOT NULL REFERENCES plugin_instances(id) ON DELETE CASCADE,
 revision TEXT NOT NULL CHECK(length(revision)>0),
 name TEXT NOT NULL,
 payload TEXT NOT NULL CHECK(json_valid(payload)),
 author_id TEXT CHECK(author_id IS NULL OR length(author_id) BETWEEN 1 AND 64),
 attempts INTEGER NOT NULL CHECK(attempts BETWEEN 0 AND 5),
 created_at INTEGER NOT NULL CHECK(created_at BETWEEN 0 AND 9007199254740991),
 next_attempt_at INTEGER NOT NULL CHECK(next_attempt_at BETWEEN 0 AND 9007199254740991)
) WITHOUT ROWID;
CREATE INDEX plugin_events_instance ON plugin_events(instance_id,created_at,id,next_attempt_at);
CREATE TABLE plugin_deliveries(
 id TEXT PRIMARY KEY NOT NULL,
 instance_id TEXT NOT NULL REFERENCES plugin_instances(id) ON DELETE CASCADE,
 revision TEXT NOT NULL CHECK(length(revision)>0),
 chat_id TEXT NOT NULL,
 thread TEXT NOT NULL,
 user_id TEXT NOT NULL CHECK(length(user_id) BETWEEN 1 AND 64),
 reply TEXT NOT NULL CHECK(json_valid(reply)),
 state TEXT NOT NULL CHECK(json_valid(state)),
 asked TEXT NOT NULL CHECK(json_valid(asked) AND json_type(asked)='array'),
 question_retry_at TEXT NOT NULL CHECK(json_valid(question_retry_at) AND json_type(question_retry_at)='object'),
 progress_at INTEGER NOT NULL CHECK(progress_at BETWEEN 0 AND 9007199254740991),
 attempts INTEGER NOT NULL CHECK(attempts BETWEEN 0 AND 5),
 created_at INTEGER NOT NULL CHECK(created_at BETWEEN 0 AND 9007199254740991),
 next_attempt_at INTEGER NOT NULL CHECK(next_attempt_at BETWEEN 0 AND 9007199254740991)
) WITHOUT ROWID;
CREATE INDEX plugin_deliveries_instance ON plugin_deliveries(instance_id,created_at,id,next_attempt_at);
CREATE TABLE plugin_http_receipts(
 id TEXT PRIMARY KEY NOT NULL,
 instance_id TEXT NOT NULL REFERENCES plugin_instances(id) ON DELETE CASCADE,
 fingerprint TEXT NOT NULL CHECK(length(fingerprint)=64),
 response TEXT CHECK(response IS NULL OR json_valid(response)),
 created_at INTEGER NOT NULL CHECK(created_at BETWEEN 0 AND 9007199254740991)
) WITHOUT ROWID;
CREATE INDEX plugin_http_receipts_instance ON plugin_http_receipts(instance_id,id);
CREATE TABLE plugin_storage(
 instance_id TEXT NOT NULL REFERENCES plugin_instances(id) ON DELETE CASCADE,
 key TEXT NOT NULL CHECK(length(key) BETWEEN 1 AND 200),
 value TEXT NOT NULL CHECK(json_valid(value) AND length(CAST(value AS BLOB))<=65536),
 updated_at INTEGER NOT NULL CHECK(updated_at BETWEEN 0 AND 9007199254740991),
 PRIMARY KEY(instance_id,key)
) WITHOUT ROWID;
";

fn json<T: serde::de::DeserializeOwned>(
    row: &rusqlite::Row<'_>,
    index: usize,
) -> rusqlite::Result<T> {
    let value: String = row.get(index)?;
    serde_json::from_str(&value).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            index,
            rusqlite::types::Type::Text,
            Box::new(error),
        )
    })
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Event {
    pub id: String,
    #[serde(rename = "connectionId")]
    pub instance_id: String,
    pub revision: String,
    pub name: String,
    pub payload: Value,
    pub author_id: Option<String>,
    pub attempts: u32,
    pub created_at: i64,
    pub next_attempt_at: i64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Delivery {
    pub id: String,
    #[serde(rename = "connectionId")]
    pub instance_id: String,
    pub revision: String,
    pub chat_id: String,
    pub thread: String,
    pub user_id: String,
    pub reply: Value,
    #[serde(default)]
    pub state: Value,
    #[serde(default)]
    pub asked: Vec<String>,
    #[serde(default)]
    pub question_retry_at: std::collections::BTreeMap<String, i64>,
    #[serde(default)]
    pub progress_at: i64,
    pub attempts: u32,
    pub created_at: i64,
    pub next_attempt_at: i64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Receipt {
    pub id: String,
    #[serde(rename = "connectionId")]
    pub instance_id: String,
    pub fingerprint: String,
    pub response: Option<Value>,
    pub created_at: i64,
}

impl Db<'_> {
    pub fn plugin_event(&self, id: &str) -> Result<Option<Event>> {
        let mut stmt = self.0.prepare_cached("SELECT id,instance_id,revision,name,payload,author_id,attempts,created_at,next_attempt_at FROM plugin_events WHERE id=?1")?;
        Ok(stmt
            .query_row([id], |row| {
                Ok(Event {
                    id: row.get(0)?,
                    instance_id: row.get(1)?,
                    revision: row.get(2)?,
                    name: row.get(3)?,
                    payload: json(row, 4)?,
                    author_id: row.get(5)?,
                    attempts: row.get(6)?,
                    created_at: row.get(7)?,
                    next_attempt_at: row.get(8)?,
                })
            })
            .optional()?)
    }

    pub fn plugin_events(&self, instance: &str, due: i64) -> Result<Vec<Event>> {
        let mut stmt = self.0.prepare_cached("SELECT id,instance_id,revision,name,payload,author_id,attempts,created_at,next_attempt_at FROM plugin_events WHERE instance_id=?1 AND next_attempt_at<=?2 ORDER BY created_at,id")?;
        Ok(stmt
            .query_map(params![instance, due], |row| {
                Ok(Event {
                    id: row.get(0)?,
                    instance_id: row.get(1)?,
                    revision: row.get(2)?,
                    name: row.get(3)?,
                    payload: json(row, 4)?,
                    author_id: row.get(5)?,
                    attempts: row.get(6)?,
                    created_at: row.get(7)?,
                    next_attempt_at: row.get(8)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?)
    }

    pub fn save_plugin_event(&self, value: &Event) -> Result<()> {
        self.0.execute("INSERT INTO plugin_events(id,instance_id,revision,name,payload,author_id,attempts,created_at,next_attempt_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9) ON CONFLICT(id) DO UPDATE SET instance_id=excluded.instance_id,revision=excluded.revision,name=excluded.name,payload=excluded.payload,author_id=excluded.author_id,attempts=excluded.attempts,created_at=excluded.created_at,next_attempt_at=excluded.next_attempt_at",params![value.id,value.instance_id,value.revision,value.name,serde_json::to_string(&value.payload)?,value.author_id,value.attempts,value.created_at,value.next_attempt_at])?;
        Ok(())
    }

    pub fn remove_plugin_event(&self, id: &str) -> Result<()> {
        self.0
            .execute("DELETE FROM plugin_events WHERE id=?1", [id])?;
        Ok(())
    }

    pub fn plugin_delivery(&self, id: &str) -> Result<Option<Delivery>> {
        let mut stmt = self.0.prepare_cached("SELECT id,instance_id,revision,chat_id,thread,user_id,reply,state,asked,question_retry_at,progress_at,attempts,created_at,next_attempt_at FROM plugin_deliveries WHERE id=?1")?;
        Ok(stmt
            .query_row([id], |row| {
                Ok(Delivery {
                    id: row.get(0)?,
                    instance_id: row.get(1)?,
                    revision: row.get(2)?,
                    chat_id: row.get(3)?,
                    thread: row.get(4)?,
                    user_id: row.get(5)?,
                    reply: json(row, 6)?,
                    state: json(row, 7)?,
                    asked: json(row, 8)?,
                    question_retry_at: json(row, 9)?,
                    progress_at: row.get(10)?,
                    attempts: row.get(11)?,
                    created_at: row.get(12)?,
                    next_attempt_at: row.get(13)?,
                })
            })
            .optional()?)
    }

    pub fn plugin_deliveries(&self, instance: &str, due: i64) -> Result<Vec<Delivery>> {
        let mut stmt = self.0.prepare_cached("SELECT id,instance_id,revision,chat_id,thread,user_id,reply,state,asked,question_retry_at,progress_at,attempts,created_at,next_attempt_at FROM plugin_deliveries WHERE instance_id=?1 AND next_attempt_at<=?2 ORDER BY created_at,id")?;
        Ok(stmt
            .query_map(params![instance, due], |row| {
                Ok(Delivery {
                    id: row.get(0)?,
                    instance_id: row.get(1)?,
                    revision: row.get(2)?,
                    chat_id: row.get(3)?,
                    thread: row.get(4)?,
                    user_id: row.get(5)?,
                    reply: json(row, 6)?,
                    state: json(row, 7)?,
                    asked: json(row, 8)?,
                    question_retry_at: json(row, 9)?,
                    progress_at: row.get(10)?,
                    attempts: row.get(11)?,
                    created_at: row.get(12)?,
                    next_attempt_at: row.get(13)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?)
    }

    pub fn save_plugin_delivery(&self, value: &Delivery) -> Result<()> {
        self.0.execute("INSERT INTO plugin_deliveries(id,instance_id,revision,chat_id,thread,user_id,reply,state,asked,question_retry_at,progress_at,attempts,created_at,next_attempt_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14) ON CONFLICT(id) DO UPDATE SET instance_id=excluded.instance_id,revision=excluded.revision,chat_id=excluded.chat_id,thread=excluded.thread,user_id=excluded.user_id,reply=excluded.reply,state=excluded.state,asked=excluded.asked,question_retry_at=excluded.question_retry_at,progress_at=excluded.progress_at,attempts=excluded.attempts,created_at=excluded.created_at,next_attempt_at=excluded.next_attempt_at",params![value.id,value.instance_id,value.revision,value.chat_id,value.thread,value.user_id,serde_json::to_string(&value.reply)?,serde_json::to_string(&value.state)?,serde_json::to_string(&value.asked)?,serde_json::to_string(&value.question_retry_at)?,value.progress_at,value.attempts,value.created_at,value.next_attempt_at])?;
        Ok(())
    }

    pub fn remove_plugin_delivery(&self, id: &str) -> Result<()> {
        self.0
            .execute("DELETE FROM plugin_deliveries WHERE id=?1", [id])?;
        Ok(())
    }

    pub fn plugin_receipt(&self, id: &str) -> Result<Option<Receipt>> {
        let mut stmt = self.0.prepare_cached("SELECT id,instance_id,fingerprint,response,created_at FROM plugin_http_receipts WHERE id=?1")?;
        Ok(stmt
            .query_row([id], |row| {
                Ok(Receipt {
                    id: row.get(0)?,
                    instance_id: row.get(1)?,
                    fingerprint: row.get(2)?,
                    response: row
                        .get::<_, Option<String>>(3)?
                        .map(|v| serde_json::from_str(&v))
                        .transpose()
                        .map_err(|e| {
                            rusqlite::Error::FromSqlConversionFailure(
                                3,
                                rusqlite::types::Type::Text,
                                Box::new(e),
                            )
                        })?,
                    created_at: row.get(4)?,
                })
            })
            .optional()?)
    }

    pub fn plugin_receipts(&self, instance: &str) -> Result<Vec<Receipt>> {
        let mut stmt = self.0.prepare_cached("SELECT id,instance_id,fingerprint,response,created_at FROM plugin_http_receipts WHERE instance_id=?1 ORDER BY id")?;
        Ok(stmt
            .query_map([instance], |row| {
                Ok(Receipt {
                    id: row.get(0)?,
                    instance_id: row.get(1)?,
                    fingerprint: row.get(2)?,
                    response: row
                        .get::<_, Option<String>>(3)?
                        .map(|v| serde_json::from_str(&v))
                        .transpose()
                        .map_err(|e| {
                            rusqlite::Error::FromSqlConversionFailure(
                                3,
                                rusqlite::types::Type::Text,
                                Box::new(e),
                            )
                        })?,
                    created_at: row.get(4)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?)
    }

    pub fn plugin_receipt_count(&self, instance: &str) -> Result<u32> {
        Ok(self.0.query_row(
            "SELECT count(*) FROM plugin_http_receipts WHERE instance_id=?1",
            [instance],
            |row| row.get(0),
        )?)
    }

    pub fn save_plugin_receipt(&self, value: &Receipt) -> Result<()> {
        self.0.execute("INSERT INTO plugin_http_receipts(id,instance_id,fingerprint,response,created_at) VALUES(?1,?2,?3,?4,?5) ON CONFLICT(id) DO UPDATE SET instance_id=excluded.instance_id,fingerprint=excluded.fingerprint,response=excluded.response,created_at=excluded.created_at",params![value.id,value.instance_id,value.fingerprint,value.response.as_ref().map(serde_json::to_string).transpose()?,value.created_at])?;
        Ok(())
    }

    pub fn remove_plugin_receipt(&self, id: &str) -> Result<()> {
        self.0
            .execute("DELETE FROM plugin_http_receipts WHERE id=?1", [id])?;
        Ok(())
    }

    pub fn plugin_storage(&self, instance: &str, key: &str) -> Result<Option<Value>> {
        let mut stmt = self
            .0
            .prepare_cached("SELECT value FROM plugin_storage WHERE instance_id=?1 AND key=?2")?;
        Ok(stmt
            .query_row(params![instance, key], |row| json(row, 0))
            .optional()?)
    }

    pub fn plugin_storage_keys(&self, instance: &str) -> Result<Vec<String>> {
        let mut stmt = self
            .0
            .prepare_cached("SELECT key FROM plugin_storage WHERE instance_id=?1 ORDER BY key")?;
        Ok(stmt
            .query_map([instance], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?)
    }

    pub fn save_plugin_storage(&self, instance: &str, key: &str, value: &Value) -> Result<()> {
        self.0.execute("INSERT INTO plugin_storage VALUES(?1,?2,?3,?4) ON CONFLICT(instance_id,key) DO UPDATE SET value=excluded.value,updated_at=excluded.updated_at",params![instance,key,serde_json::to_string(value)?,crate::config::now()])?;
        Ok(())
    }

    pub fn remove_plugin_storage(&self, instance: &str, key: &str) -> Result<()> {
        self.0.execute(
            "DELETE FROM plugin_storage WHERE instance_id=?1 AND key=?2",
            params![instance, key],
        )?;
        Ok(())
    }
}

impl Store {
    pub async fn save_plugin_delivery(&self, delivery: Delivery) -> Result<()> {
        self.transaction(move |db| db.save_plugin_delivery(&delivery))
            .await
    }

    pub async fn plugin_events(&self, instance: &str, due: i64) -> Result<Vec<Event>> {
        let instance = instance.to_owned();
        self.read(move |db| db.plugin_events(&instance, due)).await
    }

    pub async fn plugin_deliveries(&self, instance: &str, due: i64) -> Result<Vec<Delivery>> {
        let instance = instance.to_owned();
        self.read(move |db| db.plugin_deliveries(&instance, due))
            .await
    }
}
