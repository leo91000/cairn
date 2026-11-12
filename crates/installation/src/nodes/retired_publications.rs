//! Finish captured acknowledgements from before local disks. No publication or
//! object-storage client runs here; remote objects and deletion queues stay intact.
use crate::{
    error::{Error, Result},
    service::Service,
    store::Db,
    validation::text,
};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::Path;

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RetiredPublications {
    points: Vec<Value>,
    publications: Vec<String>,
}

fn release(db: &Db<'_>, publication: &str) -> Result<()> {
    db.0.execute("DELETE FROM shared_references WHERE publication=?1", [publication])?;
    db.0.execute("DELETE FROM shared_publications WHERE id=?1", [publication])?;
    Ok(())
}

/// Capture exact old identities in the logical detachment transaction. A pending
/// acknowledgement keeps its dependencies until its durable fence is observed.
pub(crate) fn detach_environment_in(db: &Db<'_>, run: &str) -> Result<RetiredPublications> {
    let points = db.node_backups_for_run(run)?;
    let publications = db.0.prepare_cached("SELECT p.id FROM shared_publications p WHERE p.run_id=?1 AND NOT EXISTS(SELECT 1 FROM direct_journal_acknowledgements a WHERE a.publication=p.id AND a.detached=1)")?
        .query_map([run], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for publication in &publications {
        let retained = db.0.execute(
            "UPDATE direct_journal_acknowledgements SET retired=1,detached=1 WHERE publication=?1",
            [publication],
        )?;
        if retained == 0 {
            release(db, publication)?;
        }
    }
    for point in &points {
        db.remove("node-backups", text(point, "id"))?;
    }
    for grant in db.list("node-disk-grants")? {
        if grant["runId"] == run {
            db.remove("node-disk-grants", text(&grant, "id"))?;
        }
    }
    db.0.execute("DELETE FROM direct_read_authorizations WHERE run_id=?1", [run])?;
    Ok(RetiredPublications { points, publications })
}

#[derive(Clone)]
struct Pending {
    publication: String,
    run: String,
    node: String,
}

async fn settle(s: &Service, publication: &str) -> Result<()> {
    let publication = publication.to_owned();
    s.store.transaction(move |db| {
        let retired: bool = db.0.query_row("SELECT COALESCE(MAX(retired),0) FROM direct_journal_acknowledgements WHERE publication=?1", [&publication], |row| row.get(0))?;
        db.0.execute("DELETE FROM direct_journal_acknowledgements WHERE publication=?1", [&publication])?;
        if retired {
            release(db, &publication)?;
        }
        Ok(())
    }).await
}

async fn fence(s: &Service, pending: Pending) -> Result<()> {
    let revoked = pending.node != super::LOCAL_NODE_ID
        && s.store.node_boundary(&pending.node).await?.is_some_and(|node| node["revoked"] == true);
    if revoked {
        return settle(s, &pending.publication).await;
    }
    let response = s.http.post(format!("{}/disks/{}/publication-fence", super::controller_url(s, &pending.node), pending.run))
        .bearer_auth(super::runner_secret(s).await?)
        .json(&json!({ "backupId": pending.publication }))
        .timeout(std::time::Duration::from_secs(120))
        .send().await.map_err(|_| Error::unavailable("Legacy journal acknowledgement outcome is unknown; retaining its dependencies."))?;
    if !response.status().is_success() {
        return Err(Error::unavailable("Node must confirm its durable legacy publication fence."));
    }
    let mut bytes = Vec::new();
    let mut chunks = response.bytes_stream();
    while let Some(chunk) = chunks.next().await {
        let chunk = chunk.map_err(|_| Error::unavailable("Legacy publication fence response interrupted."))?;
        if bytes.len().saturating_add(chunk.len()) > 16_384 {
            return Err(Error::unavailable("Legacy publication fence response is too large."));
        }
        bytes.extend_from_slice(&chunk);
    }
    let value: Value = serde_json::from_slice(&bytes)
        .map_err(|_| Error::unavailable("Invalid legacy publication fence confirmation."))?;
    if value["fenced"] != pending.publication {
        return Err(Error::unavailable("Invalid legacy publication fence confirmation."));
    }
    settle(s, &pending.publication).await
}

pub(crate) async fn retire_publications(s: &Service, publications: &[String]) -> Result<()> {
    let publications = serde_json::to_string(publications)?;
    let pending = s.store.read(move |db| {
        let mut query = db.0.prepare("SELECT publication,run_id,node_id FROM direct_journal_acknowledgements WHERE publication IN (SELECT value FROM json_each(?1)) ORDER BY publication")?;
        Ok(query.query_map([publications], |row| Ok(Pending {
            publication: row.get(0)?, run: row.get(1)?, node: row.get(2)?,
        }))?.collect::<rusqlite::Result<Vec<_>>>()?)
    }).await?;
    for acknowledgement in pending {
        fence(s, acknowledgement).await?;
    }
    Ok(())
}

pub(crate) async fn retire_run(s: &Service, run: &str) -> Result<()> {
    let run = run.to_owned();
    let publications = s.store.read(move |db| {
        let mut query = db.0.prepare("SELECT publication FROM direct_journal_acknowledgements WHERE run_id=?1 ORDER BY publication")?;
        Ok(query.query_map([run], |row| row.get::<_, String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?)
    }).await?;
    retire_publications(s, &publications).await
}

pub(crate) async fn retire_detached_environment(
    s: &Service,
    _run: &str,
    retired: &RetiredPublications,
    _directory: &Path,
) -> Result<()> {
    retire_publications(s, &retired.publications).await
}
