//! The v21 documents are read only here, inside one startup transaction.
use super::sql::SCHEMA;
use crate::skills::sql::SourceRecord;
use crate::{
    error::{Error, Result},
    store::Db,
    validation::uuid,
};
use cairn_contracts::Deliverable;
use rusqlite::{Connection, TransactionBehavior};
use serde::Deserialize;
use std::collections::BTreeMap;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ShareV21 {
    run_id: String,
    id: String,
}

fn invalid() -> Error {
    Error::bad("Invalid previous artifact or skill source metadata; original database retained.")
}

pub(crate) fn upgrade(connection: &mut Connection) -> Result<()> {
    let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let version: i64 = tx.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if (22..=crate::store::DATABASE_VERSION).contains(&version) {
        return Ok(());
    }
    if version != 21 {
        return Err(invalid());
    }
    tx.execute_batch(SCHEMA)?;
    tx.execute_batch(crate::skills::sql::SCHEMA)?;
    let rows = tx.prepare("SELECT key,data,expires FROM kv WHERE key GLOB 'artifact:*' OR key GLOB 'artifact-share:*' OR key GLOB 'skill-source:*' ORDER BY key")?
        .query_map([], |row| Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?,row.get::<_,Option<i64>>(2)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let db = Db(&tx);
    let mut links = BTreeMap::new();
    for (key, data, expires) in &rows {
        if expires.is_some() {
            return Err(invalid());
        }
        if let Some(token) = key.strip_prefix("artifact-share:") {
            uuid(token)?;
            let link: ShareV21 = serde_json::from_str(data)?;
            links.insert(token, link);
        }
    }
    let mut used = 0;
    for (key, data, _) in &rows {
        if let Some(identity) = key.strip_prefix("artifact:") {
            let item: Deliverable = serde_json::from_str(data)?;
            uuid(&item.run_id)?;
            uuid(&item.id)?;
            if identity != format!("{}:{}", item.run_id, item.id) {
                return Err(invalid());
            }
            match (
                item.visibility.as_deref(),
                &item.public_token,
                &item.public_url,
            ) {
                (Some("public"), Some(token), Some(_)) => {
                    let link = links.get(token.as_str()).ok_or_else(invalid)?;
                    if link.run_id != item.run_id || link.id != item.id {
                        return Err(invalid());
                    }
                    used += 1;
                }
                (None | Some("private"), None, None) => {}
                _ => return Err(invalid()),
            }
            db.insert_artifact(&item)?;
        } else if let Some(identity) = key.strip_prefix("skill-source:") {
            let (scope, name) = identity.split_once('/').ok_or_else(invalid)?;
            super::super::skills::name(name)?;
            if scope != "global" {
                uuid(scope)?;
            }
            let item: SourceRecord = serde_json::from_str(data)?;
            db.save_skill_source(scope, name, &item)?;
        }
    }
    if used != links.len() {
        return Err(invalid());
    }
    // Preserve the existing KV constraints and add domain exclusions without a fallback path.
    let old: String = tx.query_row(
        "SELECT sql FROM sqlite_schema WHERE type='table' AND name='kv'",
        [],
        |row| row.get(0),
    )?;
    let start = old.find('(').ok_or_else(invalid)?;
    let definition = old[start..]
        .trim_end()
        .strip_suffix(')')
        .ok_or_else(invalid)?;
    tx.execute_batch(&format!("CREATE TABLE kv_a16{definition},CONSTRAINT no_a16 CHECK(key NOT GLOB 'artifact:*' AND key NOT GLOB 'artifact-share:*' AND key NOT GLOB 'skill-source:*'));"))?;
    tx.execute_batch("INSERT INTO kv_a16 SELECT * FROM kv WHERE key IS NULL OR (key NOT GLOB 'artifact:*' AND key NOT GLOB 'artifact-share:*' AND key NOT GLOB 'skill-source:*'); DROP TABLE kv; ALTER TABLE kv_a16 RENAME TO kv;")?;
    if tx.prepare("PRAGMA foreign_key_check")?.exists([])? {
        return Err(invalid());
    }
    tx.pragma_update(None, "user_version", 22)?;
    tx.commit()?;
    Ok(())
}
