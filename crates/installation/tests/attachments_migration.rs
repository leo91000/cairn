//! Snapshot produced by the previous real Service, with synthetic uploads and secrets.
mod common;

use cairn_installation::{service::Service, store::Store};
use rusqlite::{Connection, types::Value as SqlValue};
use serde_json::{Value, json};
use std::collections::BTreeMap;

const PREVIOUS: &str = include_str!("fixtures/attachments-v16.sql");

fn previous(root: &std::path::Path) -> Connection {
    let db = Connection::open(root.join("data/manager.db")).unwrap();
    db.execute_batch(PREVIOUS).unwrap();
    db
}

fn snapshot(db: &Connection) -> BTreeMap<String, (String, Vec<Vec<SqlValue>>)> {
    let tables = db
        .prepare("SELECT name,sql FROM sqlite_schema WHERE type='table' ORDER BY name")
        .unwrap()
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    tables
        .into_iter()
        .map(|(name, sql)| {
            let mut statement = db
                .prepare(&format!("SELECT * FROM \"{}\"", name.replace('"', "\"\"")))
                .unwrap();
            let count = statement.column_count();
            let rows = statement
                .query_map([], |r| {
                    (0..count)
                        .map(|i| r.get(i))
                        .collect::<rusqlite::Result<Vec<_>>>()
                })
                .unwrap()
                .collect::<rusqlite::Result<_>>()
                .unwrap();
            (name, (sql, rows))
        })
        .collect()
}

#[tokio::test]
async fn real_v16_upgrade_preserves_metadata_files_quota_and_every_unrelated_table() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("data")).unwrap();
    let db = previous(root.path());
    let before = snapshot(&db);
    let uploads: Vec<Value> = db
        .prepare("SELECT data FROM kv WHERE key LIKE 'chat-attachment:%' ORDER BY key")
        .unwrap()
        .query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .map(|r| serde_json::from_str(&r.unwrap()).unwrap())
        .collect();
    for a in &uploads {
        let directory = root
            .path()
            .join("data/chat-attachments")
            .join(a["chatId"].as_str().unwrap());
        std::fs::create_dir_all(&directory).unwrap();
        let file = directory.join(a["id"].as_str().unwrap());
        let bytes: &[u8] = if a["name"] == "screenshot.gif" {
            b"GIF89a\x02\x00\x03\x00"
        } else {
            b"existing notes"
        };
        if a["size"].as_u64().unwrap() < 100 {
            std::fs::write(file, bytes).unwrap();
        }
    }
    let store = Store::open(&root.path().join("data")).unwrap();
    for a in uploads {
        let (chat, id) = (
            a["chatId"].as_str().unwrap().to_owned(),
            a["id"].as_str().unwrap().to_owned(),
        );
        let saved = store
            .read(move |db| db.uploaded_attachment(&chat, &id))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(saved.storage_name, a["id"]);
        assert_eq!(serde_json::to_value(saved.metadata).unwrap(), a);
    }
    let after = snapshot(&db);
    for (name, old) in &before {
        if name != "kv" {
            assert_eq!(after.get(name), Some(old), "unrelated table {name}");
        }
    }
    assert_eq!(
        db.query_row(
            "SELECT count(*) FROM kv WHERE key LIKE 'chat-attachment:%'",
            [],
            |r| r.get::<_, u32>(0)
        )
        .unwrap(),
        0
    );
    assert!(
        db.execute("INSERT INTO kv VALUES('chat-attachment:old','{}',NULL)", [])
            .is_err()
    );
    assert!(
        !db.prepare("PRAGMA foreign_key_check")
            .unwrap()
            .exists([])
            .unwrap()
    );
    let stable = snapshot(&db);
    drop(store);
    drop(Store::open(&root.path().join("data")).unwrap());
    assert_eq!(snapshot(&db), stable);
    assert_eq!(
        db.pragma_query_value::<i64, _>(None, "user_version", |r| r.get(0))
            .unwrap(),
        19
    );
    let s = Service::new(common::config(root.path())).await.unwrap();
    let proof = s.store.kv("d2-fixture-proof").await.unwrap().unwrap();
    let chat = proof["chat"].as_str().unwrap();
    let file = "ABCDEFAB-CDEF-4ABC-8DEF-ABCDEFABCDE2";
    let retry = s
        .store_attachment(chat, file, "screenshot.gif", b"GIF89a\x02\x00\x03\x00")
        .await
        .unwrap();
    assert_eq!(retry.width, Some(2));
    assert_eq!(retry.height, Some(3));
    assert_eq!(
        s.store_attachment(chat, file, "screenshot.gif", b"changed")
            .await
            .unwrap_err()
            .status,
        409
    );
    assert_eq!(
        s.store_attachment(
            proof["fullChat"].as_str().unwrap(),
            &uuid::Uuid::new_v4().to_string(),
            "extra.txt",
            b"x"
        )
        .await
        .unwrap_err()
        .status,
        413
    );
    let run = uuid::Uuid::new_v4().to_string();
    s.prepare_chat_files(&run, &json!([retry])).await.unwrap();
    assert_eq!(
        std::fs::read(
            root.path()
                .join("data/runs")
                .join(run)
                .join("chat-input/attachments")
                .join(file)
                .join("screenshot.gif")
        )
        .unwrap(),
        b"GIF89a\x02\x00\x03\x00"
    );
}

#[test]
fn malformed_previous_attachment_rolls_back_all_data_schema_counters_and_version() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("data")).unwrap();
    let db = previous(root.path());
    db.execute("UPDATE kv SET data=json_set(data,'$.size',10485761) WHERE key=(SELECT key FROM kv WHERE key LIKE 'chat-attachment:%' LIMIT 1)",[]).unwrap();
    let before = snapshot(&db);
    assert!(Store::open(&root.path().join("data")).is_err());
    assert_eq!(snapshot(&db), before);
    assert_eq!(
        db.pragma_query_value::<i64, _>(None, "user_version", |r| r.get(0))
            .unwrap(),
        16
    );
    db.execute("UPDATE kv SET data=json_set(data,'$.size',0) WHERE key=(SELECT key FROM kv WHERE key LIKE 'chat-attachment:%' LIMIT 1)",[]).unwrap();
    drop(Store::open(&root.path().join("data")).unwrap());
}
