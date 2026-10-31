use super::*;
use crate::store::Store;
use serde_json::json;

const PREVIOUS: &str = include_str!("../../../tests/fixtures/personal-state-v17.sql");
const LEGACY_COLUMNS: &str = include_str!("../../../tests/fixtures/run-projection-columns-v18.sql");

/// Older domain fixtures start from a current Store, then restore their domain.
/// The run layout must also represent the historical version they advertise.
pub(crate) fn restore_legacy_columns(connection: &Connection) {
    let current: bool = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='view' AND name='run_views')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    if !current {
        return;
    }
    connection.execute_batch("DROP VIEW run_views").unwrap();
    connection.execute_batch(LEGACY_COLUMNS).unwrap();
}

fn previous(root: &std::path::Path) -> Connection {
    let mut connection = Connection::open(root.join("manager.db")).unwrap();
    connection.execute_batch(PREVIOUS).unwrap();
    crate::reads::migration::upgrade(&mut connection).unwrap();
    connection
        .execute(
            "INSERT INTO runs(id,task_id,status,created_at,recovery_pending,resume_available,resume_count,snapshot,boundary) VALUES('projection-large','projection-large','succeeded',1,0,0,0,?,'{}')",
            [json!({"task":{"name":"tâche"},"agent":{"name":"agent","provider":"codex","instructions":"x".repeat(512*1024)}}).to_string()],
        )
        .unwrap();
    connection.execute("INSERT INTO runs(id,task_id,status,created_at,recovery_pending,resume_available,resume_count,snapshot,boundary) VALUES('projection-null','projection-null','queued',2,0,0,0,NULL,'{}')", []).unwrap();
    connection
}

fn projections(connection: &Connection, source: &str) -> Vec<(String, String, String)> {
    let mut statement = connection
        .prepare(&format!(
            "SELECT id,data,summary_data FROM {source} ORDER BY id"
        ))
        .unwrap();
    statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

fn version(connection: &Connection) -> i64 {
    connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap()
}

fn retained_schema(connection: &Connection) -> Vec<String> {
    connection
        .prepare(
            "SELECT json_array(name,type,\"notnull\",dflt_value,pk,hidden)
             FROM pragma_table_xinfo('runs') WHERE hidden!=2 ORDER BY name",
        )
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

#[test]
fn version_eighteen_upgrade_preserves_exact_wire_projection_and_reopens() {
    let root = tempfile::tempdir().unwrap();
    let connection = previous(root.path());
    assert_eq!(version(&connection), 18);
    let expected = projections(&connection, "runs");
    let schema_before = retained_schema(&connection);
    drop(connection);

    let store = Store::open(root.path()).unwrap();
    let connection = Connection::open(root.path().join("manager.db")).unwrap();
    assert_eq!(version(&connection), 19);
    assert_eq!(retained_schema(&connection), schema_before);
    assert_eq!(projections(&connection, "run_views"), expected);
    let removed: i64 = connection
        .query_row(
            "SELECT count(*) FROM pragma_table_xinfo('runs') WHERE name IN ('data','summary_data')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(removed, 0);
    assert!(
        !connection
            .prepare("PRAGMA foreign_key_check")
            .unwrap()
            .exists([])
            .unwrap()
    );
    assert_eq!(
        connection
            .query_row("PRAGMA quick_check", [], |row| row.get::<_, String>(0))
            .unwrap(),
        "ok"
    );
    drop(store);
    let reopened = Store::open(root.path()).unwrap();
    assert_eq!(projections(&connection, "run_views"), expected);
    drop(reopened);
}

#[test]
fn a_view_name_conflict_rolls_back_both_drops_and_version() {
    let root = tempfile::tempdir().unwrap();
    let mut connection = previous(root.path());
    let expected = projections(&connection, "runs");
    connection
        .execute_batch(
            "CREATE TABLE run_views(value TEXT); INSERT INTO run_views VALUES('retained')",
        )
        .unwrap();
    assert!(upgrade(&mut connection).is_err());
    assert_eq!(version(&connection), 18);
    assert_eq!(projections(&connection, "runs"), expected);
    assert_eq!(
        connection
            .query_row("SELECT value FROM run_views", [], |row| row
                .get::<_, String>(0))
            .unwrap(),
        "retained"
    );
    connection.execute_batch("DROP TABLE run_views").unwrap();
    upgrade(&mut connection).unwrap();
    assert_eq!(version(&connection), 19);
    assert_eq!(projections(&connection, "run_views"), expected);
}

#[test]
fn an_unexpected_legacy_projection_is_rejected_before_mutation() {
    let root = tempfile::tempdir().unwrap();
    let mut connection = previous(root.path());
    connection
        .execute_batch("ALTER TABLE runs DROP COLUMN data")
        .unwrap();
    let expected: Vec<String> = connection
        .prepare("SELECT summary_data FROM runs ORDER BY id")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert!(upgrade(&mut connection).is_err());
    assert_eq!(version(&connection), 18);
    let after: Vec<String> = connection
        .prepare("SELECT summary_data FROM runs ORDER BY id")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(after, expected);
}

#[test]
fn concurrent_startups_converge_on_one_preserved_projection() {
    let root = tempfile::tempdir().unwrap();
    let connection = previous(root.path());
    let expected = projections(&connection, "runs");
    drop(connection);
    let handles = (0..2)
        .map(|_| {
            let path = root.path().to_owned();
            std::thread::spawn(move || Store::open(&path).unwrap())
        })
        .collect::<Vec<_>>();
    let stores = handles
        .into_iter()
        .map(|handle| handle.join().unwrap())
        .collect::<Vec<_>>();
    let connection = Connection::open(root.path().join("manager.db")).unwrap();
    assert_eq!(version(&connection), 19);
    assert_eq!(projections(&connection, "run_views"), expected);
    drop(stores);
}

#[test]
fn startup_waits_for_a_rollback_journal_writer_and_preserves_projection() {
    use std::{sync::mpsc, time::Duration};

    let root = tempfile::tempdir().unwrap();
    let mut connection = previous(root.path());
    let expected = projections(&connection, "runs");
    assert_eq!(
        connection
            .pragma_query_value::<String, _>(None, "journal_mode", |row| row.get(0))
            .unwrap(),
        "delete"
    );
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    let (started, starting) = mpsc::channel();
    let (completed, completion) = mpsc::channel();
    let path = root.path().to_owned();
    let starter = std::thread::spawn(move || {
        started.send(()).unwrap();
        assert!(completed.send(Store::open(&path)).is_ok());
    });
    starting.recv_timeout(Duration::from_secs(5)).unwrap();
    // A rollback-mode write reservation makes WAL conversion return BUSY
    // immediately, bypassing SQLite's busy handler. Keep it held long enough
    // to distinguish waiting for release from returning that error to startup.
    let early = completion.recv_timeout(Duration::from_millis(250));
    transaction.commit().unwrap();
    let store = match early {
        Ok(result) => result.unwrap(),
        Err(mpsc::RecvTimeoutError::Timeout) => completion
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .unwrap(),
        Err(error) => panic!("startup stopped before completing: {error}"),
    };
    starter.join().unwrap();
    assert_eq!(version(&connection), 19);
    assert_eq!(projections(&connection, "run_views"), expected);
    assert_eq!(
        connection
            .query_row("PRAGMA quick_check", [], |row| row.get::<_, String>(0))
            .unwrap(),
        "ok"
    );
    assert!(
        !connection
            .prepare("PRAGMA foreign_key_check")
            .unwrap()
            .exists([])
            .unwrap()
    );
    drop(store);
}
