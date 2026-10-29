//! Real v15 Store fixture, synthetic credentials/content; no network or live installation.
use cairn_installation::store::Store;
use rusqlite::{Connection, types::Value};
use std::collections::BTreeMap;

const PREVIOUS: &str = include_str!("fixtures/questions-waits-v15.sql");

fn previous(path: &std::path::Path) -> Connection {
    let db = Connection::open(path.join("manager.db")).unwrap();
    db.execute_batch(PREVIOUS).unwrap();
    db
}

fn snapshot(db: &Connection) -> BTreeMap<String, (String, Vec<Vec<Value>>)> {
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
                        .collect::<rusqlite::Result<Vec<Value>>>()
                })
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap();
            (name, (sql, rows))
        })
        .collect()
}

#[tokio::test]
async fn upgrades_real_previous_store_with_order_privacy_and_unrelated_data_intact() {
    let root = tempfile::tempdir().unwrap();
    let db = previous(root.path());
    let before = snapshot(&db);
    let questions: Vec<String> = db
        .prepare("SELECT data FROM kv WHERE key LIKE 'chat-question:%' ORDER BY key")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    let store = Store::open(root.path()).unwrap();
    assert_eq!(
        db.pragma_query_value::<i64, _>(None, "user_version", |r| r.get(0))
            .unwrap(),
        16
    );
    for q in questions {
        let expected: serde_json::Value = serde_json::from_str(&q).unwrap();
        let chat = expected["chatId"].as_str().unwrap().to_owned();
        let id = expected["id"].as_str().unwrap().to_owned();
        let question = store
            .read(move |db| db.question(&chat, &id))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(serde_json::to_value(question).unwrap(), expected);
    }
    let chats = store.conversations().await.unwrap();
    let sleeping = chats.iter().find(|c| c.wakeup.is_some()).unwrap();
    let wakeup = sleeping.wakeup.as_ref().unwrap();
    assert_eq!(wakeup.id, "wake-v15");
    assert_eq!(wakeup.wake_at, 3600123);
    assert_eq!(wakeup.message_id.as_deref(), Some("turn-v15"));
    let slots = wakeup.members.as_ref().unwrap();
    assert_eq!(slots.len(), 2);
    assert_eq!(slots[0].next_check_at, Some(987));
    assert_eq!(slots[0].last_error.as_deref(), Some("retry synthetic"));
    assert_eq!(
        slots[1].observation.as_ref().unwrap().waiting_on.as_deref(),
        Some("requester")
    );
    assert!(chats.iter().any(|c| c.requested_by.is_some()));
    let after = snapshot(&db);
    for (name, old) in &before {
        if !matches!(
            name.as_str(),
            "conversations" | "chat_messages" | "kv" | "runs"
        ) {
            assert_eq!(after.get(name), Some(old), "unrelated table {name}");
        }
    }
    assert_eq!(
        db.query_row(
            "SELECT count(*) FROM kv WHERE key LIKE 'chat-question:%'",
            [],
            |r| r.get::<_, u32>(0)
        )
        .unwrap(),
        0
    );
    assert!(db.prepare("SELECT answers FROM chat_messages").is_err());
    assert!(
        db.prepare("SELECT wakeup,requested_by FROM conversations")
            .is_err()
    );
    assert_eq!(
        db.query_row("SELECT count(*) FROM question_answer_deliveries", [], |r| r
            .get::<_, u32>(0))
            .unwrap(),
        2
    );
    assert!(
        !db.query_row("SELECT text FROM chat_messages LIMIT 1", [], |r| r
            .get::<_, String>(0))
            .unwrap()
            .contains("private")
    );
    assert!(
        !db.prepare("PRAGMA foreign_key_check")
            .unwrap()
            .exists([])
            .unwrap()
    );
    let stable = snapshot(&db);
    drop(store);
    drop(Store::open(root.path()).unwrap());
    assert_eq!(snapshot(&db), stable);
}

#[test]
fn malformed_previous_question_rolls_back_schema_data_and_version_then_can_retry() {
    let root = tempfile::tempdir().unwrap();
    let db = previous(root.path());
    let (key, data): (String, String) = db
        .query_row(
            "SELECT key,data FROM kv WHERE key LIKE 'chat-question:%'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    db.execute("UPDATE kv SET data='{}' WHERE key=?", [&key])
        .unwrap();
    let before = snapshot(&db);
    assert!(Store::open(root.path()).is_err());
    assert_eq!(
        db.pragma_query_value::<i64, _>(None, "user_version", |r| r.get(0))
            .unwrap(),
        15
    );
    assert_eq!(snapshot(&db), before);
    db.execute("UPDATE kv SET data=? WHERE key=?", [data, key])
        .unwrap();
    drop(Store::open(root.path()).unwrap());
}
