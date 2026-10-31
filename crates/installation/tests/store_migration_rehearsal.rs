//! Rehearse only Store initialization against a copy, without service startup.
#[path = "../examples/browser_fixture/store_migration.rs"]
mod store_migration;

use rusqlite::Connection;
use serde_json::json;

const PREVIOUS: &str = include_str!("fixtures/questions-waits-v15.sql");

#[test]
fn rehearsal_upgrades_only_the_private_copy_and_is_stable_on_reopen() {
    let source = tempfile::tempdir().unwrap();
    let copy = tempfile::tempdir().unwrap();
    let database = source.path().join("manager.db");
    let db = Connection::open(&database).unwrap();
    db.execute_batch(PREVIOUS).unwrap();
    drop(db);
    let original = std::fs::read(&database).unwrap();
    std::fs::copy(&database, copy.path().join("manager.db")).unwrap();

    let result = store_migration::upgrade_copy(copy.path()).unwrap();
    assert_eq!(
        result,
        json!({
            "versionBefore": 15,
            "versionAfter": 21,
            "quickCheck": "ok",
            "foreignKeyCheck": "ok",
        })
    );
    assert_eq!(std::fs::read(&database).unwrap(), original);
    let reopened = store_migration::upgrade_copy(copy.path()).unwrap();
    assert_eq!(reopened["versionBefore"], 21);
    assert_eq!(reopened["versionAfter"], 21);
}

#[test]
fn rehearsal_refuses_missing_database_and_keeps_diagnostics_private() {
    let root = tempfile::tempdir().unwrap();
    let error = store_migration::upgrade_copy(root.path()).unwrap_err();
    assert_eq!(
        error.message,
        "Store-only migration rehearsal failed; private copy retained."
    );
    assert!(!root.path().join("manager.db").exists());
    let db = Connection::open(root.path().join("manager.db")).unwrap();
    db.execute_batch(PREVIOUS).unwrap();
    db.execute(
        "UPDATE kv SET data='{}' WHERE key LIKE 'chat-question:%'",
        [],
    )
    .unwrap();
    let error = store_migration::upgrade_copy(root.path()).unwrap_err();
    assert_eq!(
        error.message,
        "Store-only migration rehearsal failed; private copy retained."
    );
    assert_eq!(
        db.pragma_query_value::<i64, _>(None, "user_version", |row| row.get(0))
            .unwrap(),
        15
    );
    assert!(
        db.prepare("SELECT wakeup,requested_by FROM conversations")
            .is_ok()
    );
    assert!(db.prepare("SELECT answers FROM chat_messages").is_ok());
}
