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
    let contract: serde_json::Value = serde_json::from_str(include_str!(
        "../../../deploy/installations/data-compatibility.json"
    ))
    .unwrap();
    assert_eq!(version, contract["databaseVersion"].as_i64().unwrap());
    // Every pre-epoch-9 manager refuses database versions newer than seven.
    assert!(version > 7);
    drop(reopened);
    db.pragma_update(None, "user_version", version + 1).unwrap();
    assert_eq!(
        Store::open(root.path()).err().unwrap().message,
        "This database belongs to a newer application version."
    );
    let retained: i64 = db
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(
        retained,
        version + 1,
        "A refused newer database must never be downgraded"
    );
}

#[tokio::test]
async fn upgrades_version_eight_and_keeps_waiting_missions_in_the_active_slot() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(root.path()).unwrap();
    store
        .transaction(|db| {
            db.0.execute_batch(
                "DROP INDEX runs_active_task;
CREATE UNIQUE INDEX runs_active_task ON runs(task_id) WHERE status IN ('queued','running');
PRAGMA user_version=8;",
            )?;
            db.add_run(
                &json!({ "id": "mission", "taskId": "task", "status": "queued", "createdAt": 1 }),
                None,
            )?;
            Ok(())
        })
        .await
        .unwrap();
    drop(store);
    let upgraded = Store::open(root.path()).unwrap();
    upgraded.transaction(|db| {
        db.patch_run("mission", &json!({ "status": "waiting", "wakeup": { "reason": "Wait" } }))?;
        assert_eq!(db.active()?.len(), 1);
        assert!(db.add_run(&json!({ "id": "duplicate", "taskId": "task", "status": "queued", "createdAt": 2 }), None).is_err());
        db.patch_run("mission", &json!({ "status": "succeeded" }))?;
        db.add_run(&json!({ "id": "next", "taskId": "task", "status": "queued", "createdAt": 2 }), None)?;
        Ok(())
    }).await.unwrap();
    let db = Connection::open(root.path().join("manager.db")).unwrap();
    let version: i64 = db
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(version, 9);
    assert!(
        version > 8,
        "Older managers must refuse mission wait records"
    );
}
