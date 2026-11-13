//! Schema retained only to read and upgrade installations created before ADR-0083.
use crate::error::{Error, Result};

pub(super) const LEGACY_DISK_SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS shared_publications(id TEXT PRIMARY KEY,run_id TEXT NOT NULL);
CREATE INDEX IF NOT EXISTS shared_publications_run ON shared_publications(run_id);
CREATE TABLE IF NOT EXISTS shared_objects(id TEXT PRIMARY KEY,destination TEXT NOT NULL,hash TEXT NOT NULL,size INTEGER NOT NULL,location TEXT NOT NULL,state TEXT NOT NULL CHECK(state IN ('pending','ready','invalid','deleting')),unused_at INTEGER,retry_at INTEGER NOT NULL DEFAULT 0);
CREATE UNIQUE INDEX IF NOT EXISTS shared_objects_reuse ON shared_objects(destination,hash) WHERE state='ready';
CREATE INDEX IF NOT EXISTS shared_objects_garbage ON shared_objects(retry_at,unused_at) WHERE unused_at IS NOT NULL;
CREATE TABLE IF NOT EXISTS shared_references(publication TEXT NOT NULL REFERENCES shared_publications(id) ON DELETE CASCADE,object TEXT NOT NULL REFERENCES shared_objects(id),PRIMARY KEY(publication,object));
CREATE INDEX IF NOT EXISTS shared_references_object ON shared_references(object);
CREATE TABLE IF NOT EXISTS remote_deletions(id TEXT PRIMARY KEY,location TEXT NOT NULL,key TEXT NOT NULL,prefix INTEGER NOT NULL DEFAULT 0,retry_at INTEGER NOT NULL DEFAULT 0);
CREATE INDEX IF NOT EXISTS remote_deletions_due ON remote_deletions(retry_at);
CREATE TABLE IF NOT EXISTS direct_storage_grants(object TEXT PRIMARY KEY REFERENCES shared_objects(id) ON DELETE CASCADE,expires_at INTEGER NOT NULL,settled INTEGER NOT NULL DEFAULT 0 CHECK(settled IN (0,1)));
CREATE TABLE IF NOT EXISTS direct_read_authorizations(grant_id TEXT NOT NULL,object TEXT NOT NULL,hash TEXT NOT NULL,run_id TEXT NOT NULL,PRIMARY KEY(grant_id,object));CREATE INDEX IF NOT EXISTS direct_read_authorizations_object ON direct_read_authorizations(object);
";

pub(super) const LEGACY_DISK_ACK_SCHEMA: &str = "
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
    transaction.execute_batch(LEGACY_DISK_ACK_SCHEMA)?;
    transaction.pragma_update(None, "user_version", 32)?;
    transaction.commit()?;
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
