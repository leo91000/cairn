use super::*;
use crate::store::Store;
use serde_json::{Value, json};

const CHAT: &str = "00000000-0000-4000-8000-000000000001";

// Exact affected version-12 DDL from 4af96bbe, the last pre-A7 main. Other
// domain tables retain their current schemas, matching the inventory migration's fixture.
pub(crate) fn version12(db: &Connection) {
    db.execute_batch("DROP TABLE conversation_engine_switches; DROP TABLE message_attachments; DROP TABLE chat_messages; DROP TABLE conversations; DROP TABLE conversation_retention;
DROP TABLE records;
CREATE TABLE records(id TEXT PRIMARY KEY,kind TEXT NOT NULL CHECK(kind NOT IN ('nodes','projects','agents','tasks','mcps','plugin-connections','plugin-packages','plugin-requests','plugin-activity','plugin-timers','plugin-events','plugin-deliveries','plugin-storage','plugin-http-receipts')),data TEXT NOT NULL,updated_at INTEGER NOT NULL);
CREATE INDEX records_kind ON records(kind,updated_at DESC);
CREATE INDEX records_node_backups_run ON records(json_extract(data,'$.runId')) WHERE kind='node-backups';
CREATE UNIQUE INDEX records_chat_slug ON records(json_extract(data,'$.slug')) WHERE kind='chats';
CREATE TABLE chat_messages(id TEXT PRIMARY KEY,chat_id TEXT NOT NULL,data TEXT NOT NULL,created_at INTEGER NOT NULL);
CREATE INDEX chat_messages_chat ON chat_messages(chat_id,created_at);
DROP TABLE kv;
CREATE TABLE kv(key TEXT PRIMARY KEY CHECK(key NOT LIKE 'run-checkpoint:%'),data TEXT NOT NULL,expires INTEGER);
PRAGMA user_version=12;").unwrap();
}

async fn database(root: &std::path::Path) -> Connection {
    drop(Store::open(root).unwrap());
    let db = Connection::open(root.join("manager.db")).unwrap();
    version12(&db);
    db
}

fn conversation() -> Value {
    json!({
        "id": CHAT, "slug":"quiet-cedar-existing", "revision":"retained-revision",
        "title":"Preserved", "agentId":"deleted-agent", "projectId":"deleted-project",
        "paused":true, "lifecycle":"trash", "createdAt":42,"updatedAt":51,
        "lastActivityAt":50,"trashedAt":52,"purgeAt":9007199254740001_i64,
        "sessionRestartRequested":true,"cancelledByDeletion":true,
        "requestedBy":{"runId":"parent-run","chatId":"parent-chat","requestId":"request"},
        "nativePrincipals":["second","first"],"nativeAuthorityNodes":["node-b","node-a"],
        "nativeParentRunId":"parent-run","nativeParentAccess":"owner-proof",
        "nativeProvenanceUnknown":false,"nativePluginGrants":[],
        "wakeup":{"kind":"delay","privateState":"retained"},
        "environmentState":"cleaning","environmentExpiredAt":60,"environmentRetryAfter":65,
        "environmentError":"retry"
    })
}

fn seed(db: &Connection) {
    db.execute(
        "INSERT INTO records VALUES(?,'chats',?,123)",
        params![CHAT, conversation().to_string()],
    )
    .unwrap();
    db.execute(
        "INSERT INTO records VALUES('other','attachments','{\"bytes\":9007199254740001}',321)",
        [],
    )
    .unwrap();
    db.execute(
        "INSERT INTO kv VALUES('credential:synthetic','\"synthetic-secret\"',9007199254740001)",
        [],
    )
    .unwrap();
    db.execute(
        "INSERT INTO kv VALUES('conversation-expiration','{\"inactivityDays\":19}',NULL)",
        [],
    )
    .unwrap();
    // Reverse lexical IDs and equal timestamps exercise rowid tie ordering.
    for id in ["z-message", "a-message"] {
        let message = json!({"id":id,"chatId":CHAT,"text":"private answer","createdAt":99,
            "revision":"message-revision", "mode":"steer","status":"sending",
            "questionId":"question", "answers":{"field":{"selected":["b","a"],"comment":"secret"}},
            "attachments":[{"id":"b","chatId":CHAT,"name":"B","kind":"file"},{"id":"a","chatId":CHAT,"name":"A","kind":"image","width":10,"height":20}]
        });
        db.execute(
            "INSERT INTO chat_messages VALUES(?,?,?,99)",
            params![id, CHAT, message.to_string()],
        )
        .unwrap();
    }
}

fn assert_subset(before: &Value, after: &Value) {
    if let Some(object) = before.as_object() {
        for (key, value) in object {
            assert_subset(value, &after[key]);
        }
    } else if let Some(items) = before.as_array() {
        let after = after.as_array().unwrap();
        assert_eq!(items.len(), after.len());
        for (before, after) in items.iter().zip(after) {
            assert_subset(before, after);
        }
    } else {
        assert_eq!(before, after);
    }
}

#[tokio::test]
async fn startup_preserves_conversations_messages_order_secrets_and_settings() {
    let root = tempfile::tempdir().unwrap();
    let db = database(root.path()).await;
    seed(&db);
    let old_messages: Vec<Value> = db
        .prepare("SELECT data FROM chat_messages ORDER BY rowid")
        .unwrap()
        .query_map([], |r| {
            Ok(serde_json::from_str::<Value>(&r.get::<_, String>(0)?).unwrap())
        })
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    let store = Store::open(root.path()).unwrap();
    let after = store.conversation_boundary(CHAT).await.unwrap().unwrap();
    assert_subset(&conversation(), &after);
    let messages = store.read(|db| db.messages(CHAT)).await.unwrap();
    for (before, after) in old_messages.iter().zip(&messages) {
        assert_subset(before, after);
    }
    assert_eq!(messages.len(), 2);
    assert_eq!(
        db.query_row("SELECT persisted_at FROM conversations", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        123
    );
    assert_eq!(
        db.query_row(
            "SELECT inactivity_days FROM conversation_retention",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        19
    );
    assert_eq!(
        db.query_row(
            "SELECT data FROM kv WHERE key='credential:synthetic'",
            [],
            |r| r.get::<_, String>(0)
        )
        .unwrap(),
        "\"synthetic-secret\""
    );
    assert_eq!(
        db.query_row("SELECT data FROM records WHERE id='other'", [], |r| r
            .get::<_, String>(0))
            .unwrap(),
        "{\"bytes\":9007199254740001}"
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
        db.execute("INSERT INTO records VALUES('forbidden','chats','{}',0)", [])
            .is_err()
    );
    assert!(
        db.execute(
            "INSERT INTO kv VALUES('conversation-expiration','{}',NULL)",
            []
        )
        .is_err()
    );
    drop(store);
    assert_eq!(
        Store::open(root.path())
            .unwrap()
            .conversation_boundary(CHAT)
            .await
            .unwrap()
            .unwrap(),
        after
    );
}

#[tokio::test]
async fn late_invalid_message_rolls_back_schema_and_all_original_data() {
    let root = tempfile::tempdir().unwrap();
    let db = database(root.path()).await;
    seed(&db);
    db.execute(
        "INSERT INTO chat_messages VALUES('bad',?,'{\"id\":\"private-invalid-value\"}',100)",
        [CHAT],
    )
    .unwrap();
    let before_schema: Vec<String> = db
        .prepare("SELECT sql FROM sqlite_schema WHERE sql IS NOT NULL ORDER BY name")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    let error = Store::open(root.path()).err().unwrap();
    assert!(!error.message.contains("private-invalid-value"));
    assert_eq!(
        db.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        12
    );
    let after_schema: Vec<String> = db
        .prepare("SELECT sql FROM sqlite_schema WHERE sql IS NOT NULL ORDER BY name")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(before_schema, after_schema);
    assert_eq!(
        db.query_row("SELECT count(*) FROM chat_messages", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        3
    );
    assert_eq!(
        db.query_row("SELECT data FROM records WHERE id=?", [CHAT], |r| r
            .get::<_, String>(0))
            .unwrap(),
        conversation().to_string()
    );
    db.execute("DELETE FROM chat_messages WHERE id='bad'", [])
        .unwrap();
    assert!(Store::open(root.path()).is_ok());
}

#[tokio::test]
async fn broken_run_reference_aborts_and_historical_resource_ids_remain_valid() {
    for duplicate in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let db = database(root.path()).await;
        seed(&db);
        if duplicate {
            // A real version-12 unique index prevents duplicate slugs already.
            // A non-canonical identity remains a valid old record and must survive.
            let mut other = conversation();
            other["id"] = "second".into();
            other["slug"] = "second-slug".into();
            db.execute(
                "INSERT INTO records VALUES('second','chats',?,124)",
                [other.to_string()],
            )
            .unwrap();
            assert!(Store::open(root.path()).is_ok());
        } else {
            let mut invalid = conversation();
            invalid["runId"] = "missing-run".into();
            db.execute(
                "UPDATE records SET data=? WHERE id=?",
                params![invalid.to_string(), CHAT],
            )
            .unwrap();
            assert!(Store::open(root.path()).is_err());
            assert_eq!(
                db.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
                    .unwrap(),
                12
            );
            assert!(!db.table_exists(None, "conversations").unwrap());
        }
    }
}

#[tokio::test]
async fn engine_switch_retains_current_progress_and_all_receipt_lifetimes() {
    use crate::run_fixture::DbRunFixture;
    let root = tempfile::tempdir().unwrap();
    let db = database(root.path()).await;
    seed(&db);
    Db(&db)
        .add_run_fixture(
            &json!({"id":"saved-run","taskId":"chat","status":"failed","createdAt":1}),
            None,
        )
        .unwrap();
    let expires = crate::config::now() + 500_000;
    let first = json!({"chatId":CHAT,"requestId":"first","runId":"saved-run","turnId":"turn-1","provider":"codex"});
    let second = json!({"chatId":CHAT,"requestId":"second","runId":"saved-run","turnId":"turn-2","provider":"claude"});
    for value in [&first, &second] {
        db.execute(
            "INSERT INTO kv VALUES(?,?,?)",
            params![
                format!(
                    "chat-coding-agent-request:{CHAT}:{}",
                    value["requestId"].as_str().unwrap()
                ),
                value.to_string(),
                expires
            ],
        )
        .unwrap();
    }
    let mut pending = second.clone();
    pending["model"] = "saved-model".into();
    pending["pending"] = true.into();
    pending["error"] = "retry safely".into();
    pending["retryAt"] = 1234.into();
    db.execute(
        "INSERT INTO kv VALUES(?,?,NULL)",
        params![format!("chat-coding-agent:{CHAT}"), pending.to_string()],
    )
    .unwrap();
    let store = Store::open(root.path()).unwrap();
    let state = store
        .read(|db| db.engine_switch(CHAT))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(state.request_id, "second");
    assert_eq!(state.model, "saved-model");
    assert!(state.pending);
    assert_eq!(state.retry_at, Some(1234));
    assert_eq!(state.error.as_deref(), Some("retry safely"));
    assert_eq!(state.receipt_expires_at, expires);
    assert_eq!(state.current_expires_at, None);
    assert_eq!(
        store
            .read(|db| db.engine_switch_receipt(CHAT, "first"))
            .await
            .unwrap()
            .unwrap()
            .receipt_expires_at,
        expires
    );
    assert_eq!(
        db.query_row(
            "SELECT count(*) FROM kv WHERE key LIKE 'chat-coding-agent%'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
}
