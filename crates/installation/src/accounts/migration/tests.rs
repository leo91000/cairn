use crate::{accounts::migration, store::Store, vault::Vault};
use rusqlite::{Connection, params};
use serde_json::{Value, json};

// Dump produced by the real Store/Account/Vault at cb2309ba (SQLite v13), upgraded from the real v12 fixture,
// not by undoing the candidate schema. All credentials and identities are synthetic.
const PREVIOUS: &str = include_str!("../../../tests/fixtures/account-pool-v13.sql");
const ACCOUNT: &str = "00000000-0000-4000-8000-000000000001";

fn previous(root: &std::path::Path) -> Connection {
    let db = Connection::open(root.join("manager.db")).unwrap();
    db.execute_batch(PREVIOUS).unwrap();
    std::fs::write(root.join("mcp-encryption-key"), [42_u8; 32]).unwrap();
    db
}

fn rows(db: &Connection, sql: &str) -> Vec<String> {
    db.prepare(sql)
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

fn direct_metadata(db: &Connection) -> Vec<Vec<String>> {
    [
        "SELECT json_array(id,run_id) FROM shared_publications ORDER BY id",
        "SELECT json_array(id,destination,hash,size,location,state,unused_at,retry_at) FROM shared_objects ORDER BY id",
        "SELECT json_array(publication,object) FROM shared_references ORDER BY publication,object",
        "SELECT json_array(object,expires_at,settled) FROM direct_storage_grants ORDER BY object",
        "SELECT json_array(grant_id,object,hash,run_id) FROM direct_read_authorizations ORDER BY grant_id,object",
        "SELECT json_array(publication,run_id,node_id,retired) FROM direct_journal_acknowledgements ORDER BY publication",
        "SELECT object FROM direct_object_quarantines ORDER BY object",
    ]
    .into_iter()
    .map(|sql| rows(db, sql))
    .collect()
}

fn preserved(before: &Value, after: &Value) {
    if let Some(fields) = before.as_object() {
        for (key, value) in fields {
            assert!(after.get(key).is_some(), "Missing {key}");
            preserved(value, &after[key]);
        }
    } else {
        assert_eq!(before, after);
    }
}

#[tokio::test]
async fn real_version13_database_preserves_accounts_usage_secrets_and_references() {
    let root = tempfile::tempdir().unwrap();
    let mut db = previous(root.path());
    let old = rows(
        &db,
        "SELECT data FROM records WHERE kind='agentAccounts' ORDER BY id",
    );
    let kv = rows(
        &db,
        "SELECT json_array(key,data,expires) FROM kv ORDER BY key",
    );
    let other = rows(
        &db,
        "SELECT json_array(id,kind,data,updated_at) FROM records WHERE kind NOT IN ('agentAccounts','chats') ORDER BY id",
    );
    let timestamps = rows(
        &db,
        "SELECT json_array(id,updated_at) FROM records WHERE kind='agentAccounts' ORDER BY id",
    );
    let runs = rows(&db, "SELECT json_array(id,data) FROM runs ORDER BY id");
    let checkpoints = rows(
        &db,
        "SELECT json_array(run_id,state) FROM run_checkpoints ORDER BY run_id",
    );
    let direct = direct_metadata(&db);
    let store = Store::open(root.path()).unwrap();
    assert_eq!(direct_metadata(&db), direct);
    assert_eq!(
        rows(&db, "SELECT json_array(id,data) FROM runs ORDER BY id"),
        runs
    );
    assert_eq!(
        rows(
            &db,
            "SELECT json_array(run_id,state) FROM run_checkpoints ORDER BY run_id"
        ),
        checkpoints
    );
    for data in &old {
        let before: Value = serde_json::from_str(data).unwrap();
        let account = store
            .coding_account(before["id"].as_str().unwrap())
            .await
            .unwrap()
            .unwrap();
        preserved(&before, &account.to_value());
        assert!(uuid::Uuid::parse_str(&account.revision).is_ok());
    }
    assert_eq!(store.coding_accounts().await.unwrap().len(), 3);
    assert_eq!(
        rows(
            &db,
            "SELECT json_array(key,data,expires) FROM kv ORDER BY key"
        ),
        kv
    );
    assert_eq!(
        rows(
            &db,
            "SELECT json_array(id,kind,data,updated_at) FROM records ORDER BY id"
        ),
        other
    );
    assert_eq!(
        store
            .conversation_boundary("retained-chat")
            .await
            .unwrap()
            .unwrap()["accountId"],
        ACCOUNT
    );
    assert_eq!(
        rows(
            &db,
            "SELECT json_array(id,updated_at) FROM coding_agent_accounts ORDER BY id"
        ),
        timestamps
    );
    let vault = Vault::new(store.clone(), root.path()).unwrap();
    assert_eq!(
        vault.get(&format!("synthetic:{ACCOUNT}")).await.unwrap(),
        Some(json!({"token":"synthetic-only-do-not-use"}))
    );
    assert_eq!(
        db.query_row("SELECT count(*) FROM account_leases", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        db.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        15
    );
    assert_eq!(
        db.query_row("PRAGMA integrity_check", [], |r| r.get::<_, String>(0))
            .unwrap(),
        "ok"
    );
    assert!(
        !db.prepare("PRAGMA foreign_key_check")
            .unwrap()
            .exists([])
            .unwrap()
    );
    assert!(
        db.execute(
            "INSERT INTO records VALUES('forbidden','agentAccounts','{}',1)",
            []
        )
        .is_err()
    );
    let mut account = store.coding_account(ACCOUNT).await.unwrap().unwrap();
    account.name = "Saved after upgrade".into();
    store.save_coding_account(&account).await.unwrap();
    drop(vault);
    drop(store);
    migration::upgrade(&mut db).unwrap();
    let reopened = Store::open(root.path()).unwrap();
    assert_eq!(
        reopened.coding_account(ACCOUNT).await.unwrap().unwrap(),
        account
    );
}

#[tokio::test]
async fn invalid_later_account_rolls_back_schema_data_secrets_and_version_then_retries() {
    for defect in ["identity", "usage", "id", "unknown"] {
        let root = tempfile::tempdir().unwrap();
        let db = previous(root.path());
        let data: String = db
            .query_row("SELECT data FROM records WHERE id=?", [ACCOUNT], |r| {
                r.get(0)
            })
            .unwrap();
        let mut value: Value = serde_json::from_str(&data).unwrap();
        value["id"] = "zz-later-account".into();
        match defect {
            "identity" => {} // Duplicate provider/identity fails after an earlier account was copied.
            "usage" => value["usage"]["windows"] = "private-invalid-value".into(),
            "id" => value["id"] = "private-invalid-value".into(),
            _ => value["unsupported"] = "private-invalid-value".into(),
        }
        db.execute(
            "INSERT INTO records VALUES('zz-later-account','agentAccounts',?,987)",
            [value.to_string()],
        )
        .unwrap();
        let direct = direct_metadata(&db);
        let schema = rows(
            &db,
            "SELECT sql FROM sqlite_schema WHERE sql IS NOT NULL ORDER BY name",
        );
        let records = rows(
            &db,
            "SELECT json_array(id,kind,data,updated_at) FROM records ORDER BY id",
        );
        let kv = rows(
            &db,
            "SELECT json_array(key,data,expires) FROM kv ORDER BY key",
        );
        let error = Store::open(root.path()).err().unwrap();
        assert!(!error.message.contains("private-invalid-value"));
        assert_eq!(direct_metadata(&db), direct);
        assert_eq!(
            db.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            13
        );
        assert_eq!(
            rows(
                &db,
                "SELECT sql FROM sqlite_schema WHERE sql IS NOT NULL ORDER BY name"
            ),
            schema
        );
        assert_eq!(
            rows(
                &db,
                "SELECT json_array(id,kind,data,updated_at) FROM records ORDER BY id"
            ),
            records
        );
        assert_eq!(
            rows(
                &db,
                "SELECT json_array(key,data,expires) FROM kv ORDER BY key"
            ),
            kv
        );
        assert!(!db.table_exists(None, "coding_agent_accounts").unwrap());
        db.execute("DELETE FROM records WHERE id='zz-later-account'", [])
            .unwrap();
        assert_eq!(
            Store::open(root.path())
                .unwrap()
                .coding_accounts()
                .await
                .unwrap()
                .len(),
            3
        );
    }
}

#[tokio::test]
async fn empty_version13_pool_upgrades_and_unrelated_foreign_key_failure_rolls_back() {
    let root = tempfile::tempdir().unwrap();
    let db = previous(root.path());
    db.execute("DELETE FROM records WHERE kind='agentAccounts'", [])
        .unwrap();
    // A broken relation in another domain must not commit a partial schema upgrade.
    db.execute_batch("PRAGMA foreign_keys=OFF").unwrap();
    db.execute(
        "INSERT INTO agent_node_priorities VALUES(?1,?2,0)",
        params!["missing-agent", "missing-node"],
    )
    .unwrap();
    assert!(Store::open(root.path()).is_err());
    assert_eq!(
        db.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        13
    );
    assert!(!db.table_exists(None, "account_leases").unwrap());
    db.execute("DELETE FROM agent_node_priorities", []).unwrap();
    let store = Store::open(root.path()).unwrap();
    assert!(store.coding_accounts().await.unwrap().is_empty());
}

#[tokio::test]
async fn real_version12_database_completes_both_atomic_upgrades() {
    let root = tempfile::tempdir().unwrap();
    let db = Connection::open(root.path().join("manager.db")).unwrap();
    db.execute_batch(include_str!("../../../tests/fixtures/account-pool-v12.sql"))
        .unwrap();
    let store = Store::open(root.path()).unwrap();
    assert_eq!(store.coding_accounts().await.unwrap().len(), 3);
    assert_eq!(
        db.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
            .unwrap(),
        15
    );
    assert_eq!(direct_metadata(&db), vec![Vec::<String>::new(); 7]);
}
