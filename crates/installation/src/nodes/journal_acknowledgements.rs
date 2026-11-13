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
CREATE TABLE IF NOT EXISTS direct_journal_acknowledgements(publication TEXT PRIMARY KEY REFERENCES shared_publications(id),run_id TEXT NOT NULL,node_id TEXT NOT NULL,retired INTEGER NOT NULL DEFAULT 0 CHECK(retired IN (0,1)),detached INTEGER NOT NULL DEFAULT 0 CHECK(detached IN (0,1)));
CREATE UNIQUE INDEX IF NOT EXISTS direct_journal_acknowledgements_active_run ON direct_journal_acknowledgements(run_id) WHERE detached=0;
CREATE TABLE IF NOT EXISTS direct_object_quarantines(object TEXT PRIMARY KEY);
";

/// Existing retired acknowledgements keep their dependencies without reserving
/// the active run slot of a replacement environment.
pub(crate) fn upgrade(connection: &mut rusqlite::Connection) -> Result<()> {
    let transaction =
        connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let version: i64 = transaction.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if (32..=crate::store::DATABASE_VERSION).contains(&version) {
        return Ok(());
    }
    if version != 31 {
        return Err(Error::bad(
            "Unexpected database version during detached acknowledgement upgrade.",
        ));
    }
    transaction.execute_batch("ALTER TABLE direct_journal_acknowledgements RENAME TO previous_direct_journal_acknowledgements;
CREATE TABLE direct_journal_acknowledgements(publication TEXT PRIMARY KEY REFERENCES shared_publications(id),run_id TEXT NOT NULL,node_id TEXT NOT NULL,retired INTEGER NOT NULL DEFAULT 0 CHECK(retired IN (0,1)),detached INTEGER NOT NULL DEFAULT 0 CHECK(detached IN (0,1)));
INSERT INTO direct_journal_acknowledgements(publication,run_id,node_id,retired) SELECT publication,run_id,node_id,retired FROM previous_direct_journal_acknowledgements;
DROP TABLE previous_direct_journal_acknowledgements;")?;
    transaction.execute_batch(SCHEMA)?;
    transaction.pragma_update(None, "user_version", 32)?;
    transaction.commit()?;
    Ok(())
}

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

async fn pending(
    s: &Service,
    object: Option<String>,
    run: Option<String>,
    active: bool,
) -> Result<Vec<Pending>> {
    s.store.read(move|db|{
        let mut query=db.0.prepare("SELECT a.publication,a.run_id,a.node_id FROM direct_journal_acknowledgements a WHERE (?3=0 OR a.detached=0) AND (?1 IS NULL OR a.run_id=?1) AND (?2 IS NULL OR EXISTS(SELECT 1 FROM shared_references r WHERE r.publication=a.publication AND r.object=?2)) ORDER BY a.publication")?;
        Ok(query.query_map(rusqlite::params![run,object,active],|r|Ok(Pending{publication:r.get(0)?,run:r.get(1)?,node:r.get(2)?}))?.collect::<rusqlite::Result<Vec<_>>>()?)
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
    for acknowledgement in pending(s, None, Some(run.to_owned()), true).await? {
        fence(s, acknowledgement).await?;
    }
    Ok(())
}

/// Retirement must resolve ambiguous acknowledgements too: a deleted disk no
/// longer publishes, and an expired run may next be placed on a different node.
/// Fences live outside the disk, so they remain available after disk deletion.
pub(crate) async fn retire(s: &Service, run: &str) -> Result<()> {
    for acknowledgement in pending(s, None, Some(run.to_owned()), false).await? {
        fence(s, acknowledgement).await?;
    }
    Ok(())
}

/// A retired environment may outlive subsequent publications of the same run.
/// Fence only the identities captured by its logical detachment.
pub(crate) async fn retire_publications(s: &Service, publications: &[String]) -> Result<()> {
    let publications = serde_json::to_string(publications)?;
    let retired = s.store.read(move |db| {
        let mut query = db.0.prepare("SELECT publication,run_id,node_id FROM direct_journal_acknowledgements WHERE publication IN (SELECT value FROM json_each(?1)) ORDER BY publication")?;
        Ok(query.query_map([publications], |row| Ok(Pending {
            publication: row.get(0)?, run: row.get(1)?, node: row.get(2)?,
        }))?.collect::<rusqlite::Result<Vec<_>>>()?)
    }).await?;
    for acknowledgement in retired {
        fence(s, acknowledgement).await?;
    }
    Ok(())
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
    let acknowledgements = pending(s, Some(object.to_owned()), None, false).await?;
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

#[cfg(test)]
mod migration_tests {
    use super::*;

    #[test]
    fn version_31_preserves_pins_and_releases_only_detached_acknowledgement_slots() {
        let mut db = rusqlite::Connection::open_in_memory().unwrap();
        db.execute_batch("PRAGMA foreign_keys=ON;
CREATE TABLE shared_publications(id TEXT PRIMARY KEY,run_id TEXT NOT NULL);
INSERT INTO shared_publications VALUES('old','run'),('next','run'),('third','run');
CREATE TABLE direct_journal_acknowledgements(publication TEXT PRIMARY KEY REFERENCES shared_publications(id),run_id TEXT NOT NULL UNIQUE,node_id TEXT NOT NULL,retired INTEGER NOT NULL DEFAULT 0 CHECK(retired IN (0,1)));
INSERT INTO direct_journal_acknowledgements VALUES('old','run','source',1);
PRAGMA user_version=31;").unwrap();
        upgrade(&mut db).unwrap();
        let row: (String, String, String, bool, bool) = db.query_row(
            "SELECT publication,run_id,node_id,retired,detached FROM direct_journal_acknowledgements",
            [], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?)),
        ).unwrap();
        assert_eq!(
            row,
            ("old".into(), "run".into(), "source".into(), true, false)
        );
        let insert_next = "INSERT INTO direct_journal_acknowledgements(publication,run_id,node_id) VALUES('next','run','destination')";
        assert!(
            db.execute(insert_next, []).is_err(),
            "collection retirement must not free an active journal slot"
        );
        db.execute(
            "UPDATE direct_journal_acknowledgements SET detached=1 WHERE publication='old'",
            [],
        )
        .unwrap();
        db.execute(insert_next, []).unwrap();
        let insert_third = "INSERT INTO direct_journal_acknowledgements(publication,run_id,node_id) VALUES('third','run','destination')";
        assert!(db.execute(insert_third, []).is_err());
        db.execute(
            "UPDATE direct_journal_acknowledgements SET detached=1 WHERE publication='next'",
            [],
        )
        .unwrap();
        db.execute(insert_third, []).unwrap();
        upgrade(&mut db).unwrap();
        assert_eq!(
            db.query_row(
                "SELECT count(*) FROM direct_journal_acknowledgements WHERE detached=1",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
            2
        );
        assert_eq!(
            db.query_row(
                "SELECT count(*) FROM direct_journal_acknowledgements",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
            3
        );
        assert_eq!(
            db.pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
                .unwrap(),
            32
        );
        assert!(
            db.prepare("PRAGMA foreign_key_check")
                .unwrap()
                .query([])
                .unwrap()
                .next()
                .unwrap()
                .is_none()
        );
    }
}
