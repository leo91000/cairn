use super::migration;
use crate::{agent_fixture::StoreAgentFixture, store::Store};
use rusqlite::{Connection, params};
use serde_json::{Value, json};

const NODE: &str = "00000000-0000-4000-8000-000000000019";
const AGENT: &str = "00000000-0000-4000-8000-000000000029";

// Reconstruct the exact version-11 tables changed by #469. All other domain
// schemas are unchanged, so populate those through their public fixture commands.
async fn version11(root: &std::path::Path) -> Connection {
    let store = Store::open(root).unwrap();
    store
        .put_agent_fixture(json!({"id": AGENT, "name":"Retained agent"}))
        .await
        .unwrap();
    drop(store);
    let db = Connection::open(root.join("manager.db")).unwrap();
    crate::chats::migration::tests::version12(&db);
    db.execute_batch("DROP TABLE node_reviewed_agents; DROP TABLE node_trusted_accounts; DROP TABLE node_tags; DROP TABLE nodes;
DROP TABLE records;
CREATE TABLE records(id TEXT PRIMARY KEY,kind TEXT NOT NULL CHECK(kind NOT IN ('projects','agents','tasks','mcps','plugin-connections','plugin-packages','plugin-requests','plugin-activity','plugin-timers','plugin-events','plugin-deliveries','plugin-storage','plugin-http-receipts')),data TEXT NOT NULL,updated_at INTEGER NOT NULL);
CREATE INDEX records_kind ON records(kind,updated_at DESC);
CREATE INDEX records_node_backups_run ON records(json_extract(data,'$.runId')) WHERE kind='node-backups';
CREATE UNIQUE INDEX records_chat_slug ON records(json_extract(data,'$.slug')) WHERE kind='chats';
PRAGMA user_version=11;").unwrap();
    db
}

fn legacy_node() -> Value {
    json!({
        "id": NODE,
        "name": "Existing native node",
        "local": false,
        "accepting": true,
        "revoked": false,
        "capabilities": {
            "os": "linux",
            "arch": "x86_64",
            "backend": "native",
            "kvm": false,
            "ublk": true,
            "nativeModeProtocol": 1,
            "cpu": 16,
            "memoryMiB": 32768,
            "diskMiB": 1048576,
            "diskTotalMiB": 2097152
        },
        "limits": {"cpu": 4, "memoryMiB": 8192, "diskMiB": 32768},
        "slots": 3,
        "runtimeSlots": 3,
        "storage": {
            "cacheMiB": 20480,
            "memoryCacheMiB": 512,
            "reserveMiB": 1024,
            "reservePercent": 7,
            "backupSeconds": 30,
            "maxDirtySeconds": 90
        },
        "runtimeId": "retained-runtime",
        "createdAt": 42,
        "lastSeen": 1234,
        "tags": ["ordered", "native"],
        "systemTags": ["linux", "x86_64"],
        "nativeAccounts": ["person-a", "person-b"],
        "nativeReviewedAgents": [AGENT],
        "nativeEnabled": true,
        "nativeModeRequest": {"mode": "administrator", "generation": "saved-generation"},
        "nativeModeObserved": {
            "mode": "administrator",
            "generation": "saved-generation",
            "status": "applied"
        },
        "nativeModeStatus": "applied",
        "exclusiveReservations": true,
        "executionReady": true,
        "usage": {"memoryMiB": 123, "diskMiB": 456, "diskBytes": 9007199254740001_u64},
        "retainedRuns": ["saved-run"],
        "diskGrowthProtocol": 1,
        "diskCapacity": {
            "protocol": 1,
            "capturedAt": 1234,
            "usedBytes": 1234567890123_u64,
            "freeBytes": 2345678901234_u64,
            "ownedBytes": {"saved-attempt": 1234567890123_u64},
            "retainedBytes": {"saved-run": 789},
            "requests": [{"attemptId": "saved-attempt", "currentMiB": 32768}]
        }
    })
}

fn insert(db: &Connection, id: &str, node: &Value) {
    db.execute(
        "INSERT INTO records VALUES(?1,'nodes',?2,987)",
        params![id, node.to_string()],
    )
    .unwrap();
}

fn assert_preserved(before: &Value, after: &Value) {
    if let Some(fields) = before.as_object() {
        for (field, value) in fields {
            assert!(after.get(field).is_some(), "missing field {field}");
            assert_preserved(value, &after[field]);
        }
        return;
    }
    assert_eq!(before, after);
}

#[tokio::test]
async fn version11_startup_conserves_inventory_credentials_and_references() {
    let root = tempfile::tempdir().unwrap();
    let db = version11(root.path()).await;
    let legacy = legacy_node();
    insert(&db, NODE, &legacy);
    let mut local = legacy_node();
    local["id"] = crate::nodes::LOCAL_NODE_ID.into();
    local["local"] = true.into();
    local["capabilities"]["backend"] = "firecracker".into();
    local
        .as_object_mut()
        .unwrap()
        .remove("nativeReviewedAgents");
    insert(&db, crate::nodes::LOCAL_NODE_ID, &local);
    db.execute(
        "INSERT INTO agent_exclusive_nodes VALUES(?1,?2,0)",
        params![AGENT, NODE],
    )
    .unwrap();
    db.execute(
        "INSERT INTO agent_node_priorities VALUES(?1,?2,0)",
        params![AGENT, NODE],
    )
    .unwrap();
    db.execute("INSERT INTO records VALUES('backup','node-backups','{\"nodeId\":\"unchanged\",\"bytes\":9007199254740001}',12)", []).unwrap();
    db.execute("INSERT INTO kv VALUES('node-token:synthetic','\"00000000-0000-4000-8000-000000000019\"',NULL)", []).unwrap();
    db.execute("INSERT INTO kv VALUES('node-enrollment:synthetic','{\"nodeId\":\"00000000-0000-4000-8000-000000000039\",\"name\":\"Pending\",\"backend\":\"native\"}',9999999999999)", []).unwrap();
    let credentials: Vec<(String, String, Option<i64>)> = db
        .prepare("SELECT * FROM kv ORDER BY key")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    let agent = Store::open(root.path()).unwrap();
    let node = serde_json::to_value(agent.node(NODE).await.unwrap().unwrap()).unwrap();
    assert_preserved(&legacy, &node);
    assert!(!node["revision"].as_str().unwrap().is_empty());
    assert_eq!(node["exclusiveAgents"], json!([AGENT]));
    let local_after = serde_json::to_value(
        agent
            .node(crate::nodes::LOCAL_NODE_ID)
            .await
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    assert_preserved(&local, &local_after);
    assert!(agent.list("nodes").await.unwrap().is_empty());
    let after: Vec<(String, String, Option<i64>)> = db
        .prepare("SELECT * FROM kv ORDER BY key")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(credentials, after);
    assert_eq!(
        db.query_row("SELECT updated_at FROM nodes WHERE id=?", [NODE], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        987
    );
    assert_eq!(
        db.query_row("SELECT data FROM records WHERE id='backup'", [], |r| r
            .get::<_, String>(
            0
        ))
        .unwrap(),
        "{\"nodeId\":\"unchanged\",\"bytes\":9007199254740001}"
    );
    assert_eq!(
        db.query_row(
            "SELECT node_id FROM agent_node_priorities WHERE agent_id=?",
            [AGENT],
            |r| r.get::<_, String>(0)
        )
        .unwrap(),
        NODE
    );
    assert!(
        !db.prepare("PRAGMA foreign_key_check")
            .unwrap()
            .exists([])
            .unwrap()
    );
    assert_eq!(
        db.query_row("PRAGMA integrity_check", [], |r| r.get::<_, String>(0))
            .unwrap(),
        "ok"
    );
    assert!(
        db.execute("INSERT INTO records VALUES('forbidden','nodes','{}',1)", [])
            .is_err()
    );
    drop(agent);
    assert_eq!(
        serde_json::to_value(
            Store::open(root.path())
                .unwrap()
                .node(NODE)
                .await
                .unwrap()
                .unwrap()
        )
        .unwrap(),
        node
    );
}

#[tokio::test]
async fn malformed_later_node_rolls_back_the_entire_upgrade_and_can_retry() {
    let root = tempfile::tempdir().unwrap();
    let db = version11(root.path()).await;
    insert(&db, NODE, &legacy_node());
    insert(
        &db,
        "zz-invalid",
        &json!({"id":"zz-invalid","name":"private-invalid-value"}),
    );
    let error = Store::open(root.path()).err().unwrap();
    assert!(!error.message.contains("private-invalid-value"));
    assert_eq!(
        db.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        11
    );
    assert!(!db.table_exists(None, "nodes").unwrap());
    assert_eq!(
        db.query_row("SELECT count(*) FROM records WHERE kind='nodes'", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        2
    );
    db.execute("DELETE FROM records WHERE id='zz-invalid'", [])
        .unwrap();
    assert!(
        Store::open(root.path())
            .unwrap()
            .node(NODE)
            .await
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn legacy_missing_defaults_and_revocation_are_retained() {
    let root = tempfile::tempdir().unwrap();
    let mut db = version11(root.path()).await;
    let mut node = legacy_node();
    node["revoked"] = true.into();
    node["capabilities"]["nativeModeProtocol"] = Value::Null;
    node["exclusiveReservations"] = Value::Null;
    node["runtimes"] = json!(["codex", "claude"]);
    node["updatedAt"] = 123.into();
    node["capabilities"]["fuse"] = true.into();
    node.as_object_mut().unwrap().remove("storage");
    insert(&db, NODE, &node);
    db.execute_batch("PRAGMA foreign_keys=ON").unwrap();
    migration::upgrade(&mut db).unwrap();
    let store = Store::open(root.path()).unwrap();
    let node = store.node(NODE).await.unwrap().unwrap();
    assert!(node.revoked);
    assert!(!node.exclusive_reservations);
    assert_eq!(node.capabilities.native_mode_protocol, 0);
    assert_eq!(node.storage, cairn_contracts::NodeStoragePolicy::default());
    migration::upgrade(&mut db).unwrap();
}

#[tokio::test]
async fn empty_version11_inventory_upgrades_without_creating_a_node() {
    let root = tempfile::tempdir().unwrap();
    let db = version11(root.path()).await;
    let store = Store::open(root.path()).unwrap();
    assert!(store.nodes().await.unwrap().is_empty());
    assert_eq!(
        db.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        13
    );
}

#[tokio::test]
async fn noncanonical_uuid_cannot_rename_or_overwrite_an_existing_node() {
    for alternative in [
        "00000000000040008000000000000019",
        "{00000000-0000-4000-8000-000000000019}",
    ] {
        let root = tempfile::tempdir().unwrap();
        let db = version11(root.path()).await;
        let canonical = legacy_node();
        insert(&db, NODE, &canonical);
        let mut duplicate = canonical.clone();
        duplicate["id"] = alternative.into();
        duplicate["name"] = "Must never overwrite original".into();
        insert(&db, alternative, &duplicate);
        assert!(Store::open(root.path()).is_err());
        assert!(!db.table_exists(None, "nodes").unwrap());
        assert_eq!(
            db.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            11
        );
        assert_eq!(
            db.query_row("SELECT count(*) FROM records WHERE kind='nodes'", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            2
        );
        let retained: String = db
            .query_row("SELECT data FROM records WHERE id=?", [NODE], |r| r.get(0))
            .unwrap();
        assert_eq!(serde_json::from_str::<Value>(&retained).unwrap(), canonical);
    }
}
