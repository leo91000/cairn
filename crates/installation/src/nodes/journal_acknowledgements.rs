//! Retain ambiguous journal acknowledgements until the node durably fences them.
//! Existing shared references pin their exact dependencies; no second block map.
use crate::{
    error::{Error, Result},
    service::Service,
    store::Db,
    validation::text,
};
use rusqlite::OptionalExtension;
use serde_json::{Value, json};

pub(crate) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS direct_journal_acknowledgements(publication TEXT PRIMARY KEY REFERENCES shared_publications(id),run_id TEXT NOT NULL UNIQUE,node_id TEXT NOT NULL,retired INTEGER NOT NULL DEFAULT 0 CHECK(retired IN (0,1)));
CREATE TABLE IF NOT EXISTS direct_object_quarantines(object TEXT PRIMARY KEY);
";

pub(crate) fn begin(db: &Db<'_>, point: &Value) -> Result<()> {
    db.0.execute(
        "INSERT INTO direct_journal_acknowledgements(publication,run_id,node_id) VALUES(?1,?2,?3)",
        rusqlite::params![
            text(point, "id"),
            text(point, "runId"),
            text(point, "nodeId")
        ],
    )?;
    Ok(())
}

pub(crate) async fn settle(s: &Service, publication: &str) -> Result<()> {
    let publication = publication.to_owned();
    s.store.transaction(move|db|{
        let retired: bool=db.0.query_row("SELECT COALESCE(MAX(retired),0) FROM direct_journal_acknowledgements WHERE publication=?1",[&publication],|r|r.get(0))?;
        db.0.execute("DELETE FROM direct_journal_acknowledgements WHERE publication=?1",[&publication])?;
        if retired {super::shared_blocks::release(db,&publication)?;}
        Ok(())
    }).await
}

#[derive(Clone)]
struct Pending {
    publication: String,
    run: String,
    node: String,
}

async fn pending(s: &Service, object: Option<String>, run: Option<String>) -> Result<Vec<Pending>> {
    s.store.read(move|db|{
        let mut query=db.0.prepare("SELECT a.publication,a.run_id,a.node_id FROM direct_journal_acknowledgements a WHERE (?1 IS NULL OR a.run_id=?1) AND (?2 IS NULL OR EXISTS(SELECT 1 FROM shared_references r WHERE r.publication=a.publication AND r.object=?2)) ORDER BY a.publication")?;
        Ok(query.query_map(rusqlite::params![run,object],|r|Ok(Pending{publication:r.get(0)?,run:r.get(1)?,node:r.get(2)?}))?.collect::<rusqlite::Result<Vec<_>>>()?)
    }).await
}

async fn fence(s: &Service, pending: Pending) -> Result<()> {
    let revoked = pending.node != super::LOCAL_NODE_ID
        && s.store
            .node_boundary(&pending.node)
            .await?
            .is_some_and(|node| node["revoked"] == true);
    if revoked {
        // Permanent revocation excludes this identity from future placement.
        // This evidence applies to recovery, quarantine and retirement alike.
        return settle(s, &pending.publication).await;
    }
    let response = s
        .http
        .post(format!(
            "{}/disks/{}/publication-fence",
            super::controller_url(s, &pending.node),
            pending.run
        ))
        .bearer_auth(super::runner_secret(s).await?)
        .json(&json!({"backupId":pending.publication}))
        .timeout(std::time::Duration::from_secs(120))
        .send()
        .await
        .map_err(|_| {
            Error::unavailable(
                "Journal acknowledgement outcome is unknown; retaining its dependencies.",
            )
        })?;
    if !response.status().is_success() {
        return Err(Error::unavailable(
            "Node must confirm its durable publication fence.",
        ));
    }
    let bytes = super::snapshots::response_block(response).await?;
    let value: Value = serde_json::from_slice(&bytes)
        .map_err(|_| Error::unavailable("Invalid publication fence confirmation."))?;
    if value["fenced"] != pending.publication {
        return Err(Error::unavailable(
            "Invalid publication fence confirmation.",
        ));
    }
    settle(s, &pending.publication).await
}

/// Resolve a prior ambiguous ACK before creating another publication for this
/// disk. The unique run key bounds retained acknowledgement inventories.
pub(crate) async fn recover(s: &Service, run: &str) -> Result<()> {
    for acknowledgement in pending(s, None, Some(run.to_owned())).await? {
        fence(s, acknowledgement).await?;
    }
    Ok(())
}

/// Retirement must resolve ambiguous acknowledgements too: a deleted disk no
/// longer publishes, and an expired run may next be placed on a different node.
/// Fences live outside the disk, so they remain available after disk deletion.
pub(crate) async fn retire(s: &Service, run: &str) -> Result<()> {
    recover(s, run).await
}

/// Stage quarantine against commit/ACK admission, then release the global gate
/// before network I/O. The durable marker blocks only affected dependencies.
pub(crate) async fn quarantine(s: &Service, object: &str) -> Result<()> {
    let integrity = s.publication_integrity.write().await;
    let retained = object.to_owned();
    s.store
        .write(move |db| {
            db.0.execute(
                "INSERT OR IGNORE INTO direct_object_quarantines VALUES(?1)",
                [retained],
            )?;
            Ok(())
        })
        .await?;
    let acknowledgements = pending(s, Some(object.to_owned()), None).await?;
    drop(integrity);
    for acknowledgement in acknowledgements {
        fence(s, acknowledgement).await?;
    }
    let object = object.to_owned();
    s.store.transaction(move|db|{
        db.0.execute("UPDATE shared_objects SET state='invalid' WHERE id=?1 AND state='ready'",[&object])?;
        db.0.execute("DELETE FROM direct_object_quarantines WHERE object=?1",[&object])?;
        let mut affected=db.0.prepare("SELECT DISTINCT run_id FROM direct_read_authorizations WHERE object=?1 UNION SELECT p.run_id FROM shared_publications p JOIN shared_references r ON r.publication=p.id WHERE r.object=?1")?;
        let runs=affected.query_map([&object],|r|r.get::<_,String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
        for run in runs {
            if let Some(current)=db.run(&run)? {
                let mut backup=current["backup"].as_object().cloned().unwrap_or_default();
                backup.insert("snapshotId".into(),Value::Null);
                db.update_run_boundary(&run,&json!({"backup":backup}))?;
            }
        }
        Ok(())
    }).await
}

/// A report may outlive its caller or node. Resume its accepted preparation even
/// if that node never reopens the originating disk or repeats the report.
pub(crate) async fn resume(s: &Service) -> Result<()> {
    const CURSOR: &str = "direct-quarantine-cursor";
    let object = s
        .store
        .read(|db| {
            let cursor = db.kv(CURSOR)?;
            Ok(db.0.query_row(
                "SELECT object FROM direct_object_quarantines ORDER BY object<=?1,object LIMIT 1",
                [cursor.as_ref().and_then(Value::as_str).unwrap_or("")],
                |row| row.get::<_, String>(0),
            ).optional()?)
        })
        .await?;
    if let Some(object) = object {
        // Advance durably before network I/O, including failed attempts. One
        // unavailable fencing target cannot monopolize recovery after restart.
        s.store.set(CURSOR, json!(object), None).await?;
        quarantine(s, &object).await?;
    }
    Ok(())
}
