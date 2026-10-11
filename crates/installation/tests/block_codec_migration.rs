use cairn_installation::store::Store;
use rusqlite::Connection;
use serde_json::json;

#[tokio::test]
async fn upgrades_version_five_without_changing_records_and_fences_older_binaries() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(root.path()).unwrap();
    store
        .set("retained", json!({ "blockFormat": "shared-v1" }), None)
        .await
        .unwrap();
    store
        .transaction(|db| {
            db.0.execute_batch("PRAGMA user_version=5;")?;
            Ok(())
        })
        .await
        .unwrap();
    drop(store);

    let reopened = Store::open(root.path()).unwrap();
    assert_eq!(
        reopened.kv("retained").await.unwrap(),
        Some(json!({ "blockFormat": "shared-v1" }))
    );
    let db = Connection::open(root.path().join("manager.db")).unwrap();
    let version: i64 = db
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(version, 6);
    // The previous executable refuses every database newer than version 5.
    assert!(version > 5);
    drop(reopened);
    db.execute_batch("PRAGMA user_version=7;").unwrap();
    assert_eq!(
        Store::open(root.path()).err().unwrap().message,
        "This database belongs to a newer application version."
    );
    let version: i64 = db
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(
        version, 7,
        "A refused newer database must never be downgraded"
    );
}
