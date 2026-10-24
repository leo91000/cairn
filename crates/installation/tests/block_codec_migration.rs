use cairn_installation::store::Store;
use rusqlite::Connection;
use serde_json::json;

#[tokio::test]
async fn upgrades_version_ten_preserving_plugin_packages_and_fences_older_binaries() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(root.path()).unwrap();
    let package = json!({ "id": "package:notes", "pluginId": "notes", "active": "1", "approvals": { "1": "reviewed-digest" } });
    let connection = json!({ "id": "mailbox", "pluginId": "notes", "settings": { "team": "core" }, "secretKeys": ["token"] });
    let encrypted = json!({ "ciphertext": "retained-vault-entry" });
    store.put("plugin-packages", package.clone()).await.unwrap();
    store
        .put("plugin-connections", connection.clone())
        .await
        .unwrap();
    store
        .set("plugin-secrets:mailbox", encrypted.clone(), None)
        .await
        .unwrap();
    let directory = root.path().join("plugins/notes/1");
    std::fs::create_dir_all(&directory).unwrap();
    let manifest =
        br#"{"id":"notes","name":"Notes","version":"1","apiVersion":2,"capabilities":[]}"#;
    std::fs::write(directory.join("manifest.json"), manifest).unwrap();
    std::fs::write(directory.join("main.luau"), "return {}").unwrap();
    store
        .transaction(|db| {
            db.0.execute_batch("PRAGMA user_version=10;")?;
            Ok(())
        })
        .await
        .unwrap();
    drop(store);

    let upgraded = Store::open(root.path()).unwrap();
    assert_eq!(
        upgraded
            .get("plugin-packages", "package:notes")
            .await
            .unwrap(),
        Some(package)
    );
    assert_eq!(
        upgraded.get("plugin-connections", "mailbox").await.unwrap(),
        Some(connection)
    );
    assert_eq!(
        upgraded.kv("plugin-secrets:mailbox").await.unwrap(),
        Some(encrypted)
    );
    assert_eq!(
        std::fs::read(directory.join("manifest.json")).unwrap(),
        manifest
    );
    assert_eq!(
        std::fs::read_to_string(directory.join("main.luau")).unwrap(),
        "return {}"
    );
    let db = Connection::open(root.path().join("manager.db")).unwrap();
    let version: i64 = db
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(version, 13);
    // Epoch-11 managers reject anything above ten before reading installed manifests.
    assert!(version > 10);
    drop(upgraded);
    db.pragma_update(None, "user_version", version + 1).unwrap();
    assert_eq!(
        Store::open(root.path()).err().unwrap().message,
        "This database belongs to a newer application version."
    );
    assert_eq!(
        db.pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
            .unwrap(),
        version + 1
    );
}

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
    assert!(version >= 9);
    assert!(
        version > 8,
        "Older managers must refuse mission wait records"
    );
}

#[tokio::test]
async fn upgrades_version_nine_preserving_native_nodes_and_fences_older_binaries() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(root.path()).unwrap();
    let node = json!({ "id": "desktop", "slots": 1, "nativeAccounts": ["owner"], "capabilities": { "backend": "native" } });
    let agent = json!({ "id": "omni", "access": { "backend": "native", "nodes": ["desktop"] } });
    store.put("nodes", node.clone()).await.unwrap();
    store.put("agents", agent.clone()).await.unwrap();
    store
        .transaction(|db| {
            db.0.execute_batch("PRAGMA user_version=9;")?;
            Ok(())
        })
        .await
        .unwrap();
    drop(store);

    let upgraded = Store::open(root.path()).unwrap();
    assert_eq!(upgraded.get("nodes", "desktop").await.unwrap(), Some(node));
    assert_eq!(upgraded.get("agents", "omni").await.unwrap(), Some(agent));
    let db = Connection::open(root.path().join("manager.db")).unwrap();
    let version: i64 = db
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert!(version >= 10);
    // Epoch-10 managers enforce a maximum of nine before opening application records.
    assert!(version > 9);
    drop(upgraded);
    db.pragma_update(None, "user_version", version + 1).unwrap();
    assert_eq!(
        Store::open(root.path()).err().unwrap().message,
        "This database belongs to a newer application version."
    );
    assert_eq!(
        db.pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
            .unwrap(),
        version + 1
    );
}

#[tokio::test]
async fn upgrades_version_eleven_preserving_protected_native_defaults() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(root.path()).unwrap();
    let node = json!({ "id": "native", "nativeEnabled": true, "capabilities": { "backend": "native" }, "nativeAccounts": ["trusted"] });
    let agent = json!({ "id": "agent", "access": { "backend": "native", "nodes": null } });
    store.put("nodes", node.clone()).await.unwrap();
    store.put("agents", agent.clone()).await.unwrap();
    store
        .transaction(|db| {
            db.0.execute_batch("PRAGMA user_version=11;")?;
            Ok(())
        })
        .await
        .unwrap();
    drop(store);
    let upgraded = Store::open(root.path()).unwrap();
    assert_eq!(
        upgraded.get("nodes", "native").await.unwrap(),
        Some(node.clone())
    );
    assert_eq!(
        upgraded.get("agents", "agent").await.unwrap(),
        Some(agent.clone())
    );
    assert_eq!(
        cairn_installation::nodes::native_mode::requested(&node),
        json!({ "mode": "protected", "generation": null })
    );
    assert!(cairn_installation::nodes::native_mode::applied(&node));
    let db = Connection::open(root.path().join("manager.db")).unwrap();
    assert_eq!(
        db.pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
            .unwrap(),
        13
    );
    // An epoch-12 manager rejects SQLite versions above eleven before mutations.
    assert!(
        db.pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
            .unwrap()
            > 11
    );
}

#[tokio::test]
async fn upgrades_version_twelve_preserving_native_administrator_reviews() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(root.path()).unwrap();
    let node = json!({ "id": "native", "nativeModeRequest": { "mode": "administrator", "generation": "reviewed" }, "nativeModeObserved": { "mode": "administrator", "generation": "reviewed", "status": "applied" }, "nativeReviewedAgents": ["agent"] });
    store.put("nodes", node.clone()).await.unwrap();
    store
        .transaction(|db| {
            db.0.execute_batch("PRAGMA user_version=12;")?;
            Ok(())
        })
        .await
        .unwrap();
    drop(store);
    let upgraded = Store::open(root.path()).unwrap();
    assert_eq!(
        upgraded.get("nodes", "native").await.unwrap(),
        Some(node.clone())
    );
    assert!(cairn_installation::nodes::native_mode::reviewed(
        &node,
        &json!({ "id": "agent" })
    ));
    assert!(!cairn_installation::nodes::native_mode::reviewed(
        &node,
        &json!({ "id": "unreviewed" })
    ));
    let db = Connection::open(root.path().join("manager.db")).unwrap();
    let version: i64 = db
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(version, 13);
    // Epoch-13 managers refuse this database before reading native disk state.
    assert!(version > 12);
}
