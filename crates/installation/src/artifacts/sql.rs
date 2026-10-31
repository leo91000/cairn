//! Metadata only: immutable versions and revocable links, using the existing Store actors.
use crate::{
    error::Result,
    store::{Db, Store},
};
use cairn_contracts::Deliverable;
use rusqlite::{OptionalExtension, params};

pub(crate) const SCHEMA: &str = include_str!("schema.sql");

const SELECT: &str = "SELECT v.id,v.run_id,v.message_id,v.key,v.version,v.title,v.name,v.group_name,v.kind,v.media_type,v.size,v.digest,v.created_at,v.url,v.preview_status,v.width,v.height,v.duration,v.excerpt,s.visibility,s.public_token,s.public_url FROM artifact_versions v JOIN artifact_visibility s ON s.version_id=v.id";

fn artifact(row: &rusqlite::Row<'_>) -> rusqlite::Result<Deliverable> {
    Ok(Deliverable {
        id: row.get(0)?,
        run_id: row.get(1)?,
        message_id: row.get(2)?,
        key: row.get(3)?,
        version: row.get::<_, i64>(4)? as u64,
        title: row.get(5)?,
        name: row.get(6)?,
        group: row.get(7)?,
        kind: row.get(8)?,
        media_type: row.get(9)?,
        size: row.get::<_, i64>(10)? as u64,
        digest: row.get(11)?,
        created_at: row.get(12)?,
        url: row.get(13)?,
        preview_status: row.get(14)?,
        width: row.get(15)?,
        height: row.get(16)?,
        duration: row.get(17)?,
        excerpt: row.get(18)?,
        visibility: row.get(19)?,
        public_token: row.get(20)?,
        public_url: row.get(21)?,
    })
}

impl Db<'_> {
    pub fn artifacts(&self, run: Option<&str>) -> Result<Vec<Deliverable>> {
        let query = match run {
            Some(_) => format!("{SELECT} WHERE v.run_id=? ORDER BY v.created_at,v.id"),
            None => format!("{SELECT} ORDER BY v.created_at,v.id"),
        };
        let mut statement = self.0.prepare_cached(&query)?;
        let rows = match run {
            Some(run) => statement.query_map([run], artifact)?,
            None => statement.query_map([], artifact)?,
        };
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn artifact(&self, run: &str, id: &str) -> Result<Option<Deliverable>> {
        Ok(self
            .0
            .prepare_cached(&format!("{SELECT} WHERE v.run_id=?1 AND v.id=?2"))?
            .query_row(params![run, id], artifact)
            .optional()?)
    }

    pub fn public_artifact(&self, token: &str) -> Result<Option<Deliverable>> {
        Ok(self
            .0
            .prepare_cached(&format!(
                "{SELECT} WHERE s.public_token=? AND s.visibility='public'"
            ))?
            .query_row([token], artifact)
            .optional()?)
    }

    /// Called in the publication transaction; versions are never overwritten.
    pub fn insert_artifact(&self, item: &Deliverable) -> Result<()> {
        self.0
            .prepare_cached("INSERT INTO artifacts VALUES(?,?) ON CONFLICT DO NOTHING")?
            .execute(params![item.run_id, item.key])?;
        self.0
            .prepare_cached(
                "INSERT INTO artifact_versions VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
            )?
            .execute(params![
                item.id,
                item.run_id,
                item.key,
                i64::try_from(item.version).map_err(crate::error::Error::internal)?,
                item.message_id,
                item.title,
                item.name,
                item.group,
                item.kind,
                item.media_type,
                i64::try_from(item.size).map_err(crate::error::Error::internal)?,
                item.digest,
                item.created_at,
                item.url,
                item.preview_status,
                item.width,
                item.height,
                item.duration,
                item.excerpt
            ])?;
        self.save_artifact_visibility(item)
    }

    pub fn save_artifact_visibility(&self, item: &Deliverable) -> Result<()> {
        self.0.prepare_cached("INSERT INTO artifact_visibility VALUES(?,?,?,?) ON CONFLICT(version_id) DO UPDATE SET visibility=excluded.visibility,public_token=excluded.public_token,public_url=excluded.public_url")?
            .execute(params![item.id,item.visibility.as_deref().unwrap_or("private"),item.public_token,item.public_url])?;
        Ok(())
    }

    pub fn unshare_artifacts(&self, run: &str) -> Result<()> {
        self.0.prepare_cached("UPDATE artifact_visibility SET visibility='private',public_token=NULL,public_url=NULL WHERE version_id IN (SELECT id FROM artifact_versions WHERE run_id=?)")?.execute([run])?;
        Ok(())
    }

    /// Only preview fields change; concurrent visibility changes cannot be overwritten.
    pub fn save_artifact_preview(&self, item: &Deliverable) -> Result<()> {
        self.0.prepare_cached("UPDATE artifact_versions SET preview_status=?,width=?,height=?,duration=? WHERE run_id=? AND id=?")?
            .execute(params![item.preview_status,item.width,item.height,item.duration,item.run_id,item.id])?;
        Ok(())
    }
}

impl Store {
    pub async fn artifacts(&self, run: Option<&str>) -> Result<Vec<Deliverable>> {
        let run = run.map(str::to_owned);
        self.read(move |db| db.artifacts(run.as_deref())).await
    }

    pub async fn artifact(&self, run: &str, id: &str) -> Result<Option<Deliverable>> {
        let (run, id) = (run.to_owned(), id.to_owned());
        self.read(move |db| db.artifact(&run, &id)).await
    }
}
