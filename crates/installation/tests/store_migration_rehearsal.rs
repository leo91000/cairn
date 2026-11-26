//! Exercise the Store-only rehearsal helper against a private database copy.
#[path = "../examples/browser_fixture/store_migration.rs"]
mod store_migration;

use cairn_installation::store::Store;
use rusqlite::Connection;
use serde_json::json;

#[test]
fn rehearsal_upgrades_only_the_copy_and_is_stable_on_reopen() {
    let source = tempfile::tempdir().unwrap();
    let copy = tempfile::tempdir().unwrap();
    drop(Store::open(source.path()).unwrap());
    let source_file = source.path().join("manager.db");
    let copy_file = copy.path().join("manager.db");
    let db = Connection::open(&source_file).unwrap();
    db.execute_batch(
        "INSERT INTO kv VALUES('migration:local-conversation-disks:v1','{\"completedAt\":1}',NULL);
         INSERT INTO kv VALUES('retained-fixture','true',NULL);
         CREATE TABLE shared_publications(id TEXT PRIMARY KEY,run_id TEXT NOT NULL);
         INSERT INTO shared_publications VALUES('retired-publication','legacy-run');
         PRAGMA user_version=37;",
    )
    .unwrap();
    db.execute("VACUUM INTO ?1", [copy_file.to_str().unwrap()])
        .unwrap();
    drop(db);
    let original = std::fs::read(&source_file).unwrap();

    let upgraded = store_migration::upgrade_copy(copy.path()).unwrap();
    assert_eq!(
        upgraded,
        json!({
            "versionBefore": 37,
            "versionAfter": 38,
            "quickCheck": "ok",
            "foreignKeyCheck": "ok",
        })
    );
    assert_eq!(std::fs::read(&source_file).unwrap(), original);
    let migrated = Connection::open(&copy_file).unwrap();
    assert!(!migrated.table_exists(None, "shared_publications").unwrap());
    assert_eq!(
        migrated
            .query_row(
                "SELECT data FROM kv WHERE key='retained-fixture'",
                [],
                |row| row.get::<_, String>(0)
            )
            .unwrap(),
        "true"
    );
    drop(migrated);
    let reopened = store_migration::upgrade_copy(copy.path()).unwrap();
    assert_eq!(reopened["versionBefore"], 38);
    assert_eq!(reopened["versionAfter"], 38);
}

#[test]
fn rehearsal_refuses_missing_or_unsupported_databases_with_private_diagnostics() {
    const MESSAGE: &str = "Store-only migration rehearsal failed; private copy retained.";
    let root = tempfile::tempdir().unwrap();
    let database = root.path().join("manager.db");
    let error = store_migration::upgrade_copy(root.path()).unwrap_err();
    assert_eq!(error.message, MESSAGE);
    assert!(!database.exists());

    let db = Connection::open(&database).unwrap();
    db.pragma_update(None, "user_version", 36).unwrap();
    let error = store_migration::upgrade_copy(root.path()).unwrap_err();
    assert_eq!(error.message, MESSAGE);
    assert_eq!(
        db.pragma_query_value::<i64, _>(None, "user_version", |row| row.get(0))
            .unwrap(),
        36
    );
}
