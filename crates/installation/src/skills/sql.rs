//! Imported skill provenance. File bytes remain in the existing bundle writer.
use crate::{
    error::Result,
    store::{Db, Store},
};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

pub(crate) const SCHEMA: &str = include_str!("schema.sql");

/// Plugin-defined location alone is extensible. The remaining fields own lifecycle decisions.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SourceRecord {
    pub plugin_id: String,
    pub source: String,
    pub instance: String,
    pub location: BTreeMap<String, Value>,
    pub folder: String,
    pub version: String,
    pub label: String,
    pub url: Option<String>,
    pub imported_at: i64,
    pub revision: String,
}

fn source(row: &rusqlite::Row<'_>) -> rusqlite::Result<SourceRecord> {
    let location: String = row.get(4)?;
    Ok(SourceRecord {
        plugin_id: row.get(0)?,
        source: row.get(1)?,
        instance: row.get(2)?,
        folder: row.get(3)?,
        location: serde_json::from_str(&location).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                4,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })?,
        version: row.get(5)?,
        label: row.get(6)?,
        url: row.get(7)?,
        imported_at: row.get(8)?,
        revision: row.get(9)?,
    })
}

impl Db<'_> {
    pub fn skill_source(&self, scope: &str, name: &str) -> Result<Option<SourceRecord>> {
        Ok(self.0.prepare_cached("SELECT plugin_id,source,recorded_instance,folder,location,version,label,url,imported_at,revision FROM skill_sources WHERE scope=? AND name=?")?
            .query_row(params![scope,name], source).optional()?)
    }

    pub fn skill_sources(&self) -> Result<BTreeMap<(String, String), SourceRecord>> {
        Ok(self.0.prepare_cached("SELECT plugin_id,source,recorded_instance,folder,location,version,label,url,imported_at,revision,scope,name FROM skill_sources")?
            .query_map([], |row| Ok(((row.get(10)?,row.get(11)?),source(row)?)))?
            .collect::<rusqlite::Result<_>>()?)
    }

    pub fn save_skill_source(&self, scope: &str, name: &str, item: &SourceRecord) -> Result<()> {
        let project = (scope != "global").then_some(scope);
        self.0.prepare_cached("INSERT INTO skill_sources VALUES(?,?,(SELECT id FROM projects WHERE id=?),?,?,?,?,?,?,?,?,?,?) ON CONFLICT(scope,name) DO UPDATE SET project_id=excluded.project_id,plugin_id=excluded.plugin_id,source=excluded.source,recorded_instance=excluded.recorded_instance,location=excluded.location,folder=excluded.folder,version=excluded.version,label=excluded.label,url=excluded.url,imported_at=excluded.imported_at,revision=excluded.revision")?
            .execute(params![scope,name,project,item.plugin_id,item.source,item.instance,serde_json::to_string(&item.location)?,item.folder,item.version,item.label,item.url,item.imported_at,item.revision])?;
        self.0.prepare_cached("INSERT INTO skill_source_refs VALUES(?,?,(SELECT id FROM plugin_instances WHERE id=?)) ON CONFLICT(scope,name) DO UPDATE SET instance_id=excluded.instance_id")?
            .execute(params![scope,name,item.instance])?;
        Ok(())
    }

    pub fn forget_skill_source(&self, scope: &str, name: &str) -> Result<()> {
        self.0
            .prepare_cached("DELETE FROM skill_sources WHERE scope=? AND name=?")?
            .execute(params![scope, name])?;
        Ok(())
    }
}

impl Store {
    pub async fn skill_source(&self, scope: &str, name: &str) -> Result<Option<SourceRecord>> {
        let (scope, name) = (scope.to_owned(), name.to_owned());
        self.read(move |db| db.skill_source(&scope, &name)).await
    }
}
