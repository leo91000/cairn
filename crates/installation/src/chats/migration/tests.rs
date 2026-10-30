use super::*;
use crate::store::Store;
use serde_json::{Value, json};

const CHAT: &str = "00000000-0000-4000-8000-000000000001";

// Exact affected version-14 DDL from 57855b96, the last pre-A7 main. Other
// domain tables retain their current schemas, matching the inventory migration's fixture.
pub(crate) fn version14(db: &Connection) {
    db.execute_batch("DROP TABLE question_legacy_answer_items; DROP TABLE question_answer_choices; DROP TABLE question_answers; DROP TABLE question_answer_deliveries; DROP TABLE question_options; DROP TABLE question_fields; DROP TABLE question_import_metadata; DROP TABLE chat_questions; DROP TABLE wait_conditions; DROP TABLE durable_wakeups; DROP TABLE conversation_lineage;
DROP TABLE conversation_engine_switches; DROP TABLE message_attachments; DROP TABLE chat_messages; DROP TABLE conversations; DROP TABLE conversation_retention;
DROP TABLE records;
CREATE TABLE records(id TEXT PRIMARY KEY,kind TEXT NOT NULL CHECK(kind NOT IN ('agentAccounts','nodes','projects','agents','tasks','mcps','plugin-connections','plugin-packages','plugin-requests','plugin-activity','plugin-timers','plugin-events','plugin-deliveries','plugin-storage','plugin-http-receipts')),data TEXT NOT NULL,updated_at INTEGER NOT NULL);
CREATE INDEX records_kind ON records(kind,updated_at DESC);
CREATE INDEX records_node_backups_run ON records(json_extract(data,'$.runId')) WHERE kind='node-backups';
CREATE UNIQUE INDEX records_chat_slug ON records(json_extract(data,'$.slug')) WHERE kind='chats';
CREATE TABLE chat_messages(id TEXT PRIMARY KEY,chat_id TEXT NOT NULL,data TEXT NOT NULL,created_at INTEGER NOT NULL);
CREATE INDEX chat_messages_chat ON chat_messages(chat_id,created_at);
DROP TABLE kv;
CREATE TABLE kv(key TEXT PRIMARY KEY CHECK(key NOT LIKE 'run-checkpoint:%'),data TEXT NOT NULL,expires INTEGER);
PRAGMA user_version=14;").unwrap();
}

// Dump produced by the previous main's real Store and domain writers.
const PREVIOUS: &str = include_str!("../../../tests/fixtures/conversations-v14.sql");

fn previous(root: &std::path::Path) -> Connection {
    let db = Connection::open(root.join("manager.db")).unwrap();
    db.execute_batch(PREVIOUS).unwrap();
    std::fs::write(root.join("mcp-encryption-key"), [42_u8; 32]).unwrap();
    db
}

async fn database(root: &std::path::Path) -> Connection {
    let db = previous(root);
    // Each focused scenario supplies its own conversation/message/switch inputs.
    // The account pool and its other persisted domains stay populated.
    db.execute_batch("DELETE FROM chat_messages; DELETE FROM records WHERE kind='chats';
DELETE FROM kv WHERE key LIKE 'chat-coding-agent:%' OR key LIKE 'chat-coding-agent-request:%' OR key='conversation-expiration';").unwrap();
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
        "wakeup":{"id":"retained-wait","messageId":null,"reason":"Retained delay","seconds":60,"createdAt":51,"wakeAt":60051},
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
    db.execute_batch("INSERT INTO shared_objects(id,destination,hash,size,location,state) VALUES('retained-v13-object','bucket','hash',4096,'{}','ready');
INSERT INTO direct_storage_grants VALUES('retained-v13-object',9007199254740001,1);
INSERT INTO direct_read_authorizations VALUES('retained-grant','retained-v13-object','hash','historical-run');").unwrap();

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
    assert_eq!(
        db.query_row("SELECT expires_at,settled FROM direct_storage_grants WHERE object='retained-v13-object'", [], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))).unwrap(),
        (9007199254740001, 1)
    );
    assert_eq!(
        db.query_row("SELECT run_id FROM direct_read_authorizations WHERE grant_id='retained-grant' AND object='retained-v13-object'", [], |row| row.get::<_, String>(0)).unwrap(),
        "historical-run"
    );
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
async fn upgraded_uppercase_message_ids_keep_retries_edits_and_deletion() {
    use cairn_contracts::{ChatMessageReference, EditChatMessageInput, SendChatMessageInput};

    const MESSAGE: &str = "ABCDEFAB-CDEF-4ABC-8DEF-ABCDEFABCDE1";
    let root = tempfile::tempdir().unwrap();
    let db = database(root.path()).await;
    let chat = json!({"id": CHAT, "slug": "retained-uppercase", "paused": true});
    db.execute(
        "INSERT INTO records VALUES(?,'chats',?,1)",
        params![CHAT, chat.to_string()],
    )
    .unwrap();
    let message = json!({
        "id": MESSAGE, "chatId": CHAT, "text": "Retained message", "createdAt": 99,
        "mode": "queue", "status": "queued", "provider": "codex",
    });
    db.execute(
        "INSERT INTO chat_messages VALUES(?,?,?,99)",
        params![MESSAGE, CHAT, message.to_string()],
    )
    .unwrap();
    drop(db);

    let store = Store::open(root.path()).unwrap();
    store
        .transaction(|db| {
            let retry: SendChatMessageInput = serde_json::from_value(json!({
                "chatId": CHAT,
                "message": {"id": MESSAGE, "text": "Retained message", "provider": "codex"},
            }))?;
            let sent = crate::chats::send(
                db,
                &retry.chat_id.to_string(),
                crate::chats::parse_message(serde_json::to_value(retry.message)?)?,
                None,
                &crate::native::policy::Origin::default(),
            )?;
            assert_eq!(sent["id"], MESSAGE);
            assert_eq!(db.conversation_messages(CHAT)?.len(), 1);

            let edit: EditChatMessageInput = serde_json::from_value(json!({
                "chatId": CHAT, "messageId": MESSAGE,
                "message": {"text": "Edited after upgrade", "provider": "codex"},
            }))?;
            let mut values = serde_json::to_value(edit.message)?;
            values["id"] = edit.message_id.clone().into();
            let edited = crate::chats::edit(
                db,
                &edit.chat_id.to_string(),
                &edit.message_id,
                Some(crate::chats::parse_edit(values)?),
            )?;
            assert_eq!(edited["id"], MESSAGE);
            assert_eq!(edited["text"], "Edited after upgrade");

            let remove: ChatMessageReference =
                serde_json::from_value(json!({"chatId": CHAT, "messageId": MESSAGE}))?;
            crate::chats::edit(db, &remove.chat_id.to_string(), &remove.message_id, None)?;
            assert!(db.conversation_messages(CHAT)?.is_empty());
            Ok(())
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn upgraded_uppercase_questionnaire_answers_remain_idempotent() {
    use crate::{config::Config, service::Service};

    const MESSAGE: &str = "ABCDEFAB-CDEF-4ABC-8DEF-ABCDEFABCDE3";
    for (question_status, message_status) in [("answering", "queued"), ("answered", "delivered")] {
        let root = tempfile::tempdir().unwrap();
        let db = database(root.path()).await;
        let question_id = "a".repeat(64);
        let answers = json!({"choice": {"selected": [], "comment": "Retained private answer"}});
        db.execute(
            "INSERT INTO records VALUES(?,'chats',?,1)",
            params![
                CHAT,
                json!({"id": CHAT, "slug": "retained-answer", "paused": true}).to_string()
            ],
        )
        .unwrap();
        let question = json!({
            "id": question_id,
            "chatId": CHAT,
            "runId": "retained-question-run",
            "blocking": false,
            "status": question_status,
            "messageId": MESSAGE,
            "fields": [{"id": "choice", "title": "Private question", "secret": true}],
        });
        db.execute(
            "INSERT INTO kv VALUES(?,?,NULL)",
            params![
                format!("chat-question:{CHAT}:{question_id}"),
                question.to_string()
            ],
        )
        .unwrap();
        let message = json!({
            "id": MESSAGE,
            "chatId": CHAT,
            "text": "Retained private answer",
            "createdAt": 99,
            "mode": "steer",
            "status": message_status,
            "questionId": question_id,
            "answers": answers,
        });
        db.execute(
            "INSERT INTO chat_messages VALUES(?,?,?,99)",
            params![MESSAGE, CHAT, message.to_string()],
        )
        .unwrap();
        drop(db);

        let config: Config = serde_json::from_value(json!({
            "dataDir": root.path(),
            "home": root.path().join("home"),
            "workspaceRoots": [root.path()],
            "publicUrl": "http://localhost:4310",
            "host": "127.0.0.1",
            "port": 0,
            "codexBin": "unused-fixture",
            "concurrency": 1,
            "logger": false,
            "workerEnabled": false,
            "runnerUrl": "",
        }))
        .unwrap();
        let service = Service::new(config).await.unwrap();
        let retried = service
            .question_answer(
                CHAT,
                &question_id,
                json!({"id": MESSAGE, "answers": answers}),
            )
            .await
            .unwrap();
        assert_eq!(retried["messageId"], MESSAGE);
        assert_eq!(retried["status"], question_status);
        let messages = service.store.read(|db| db.messages(CHAT)).await.unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["id"], MESSAGE);
        assert_eq!(messages[0]["answers"], answers);
    }
}

#[tokio::test]
async fn historical_answer_lists_survive_upgrade_reopen_and_public_redaction() {
    let root = tempfile::tempdir().unwrap();
    let db = database(root.path()).await;
    seed(&db);
    // Public detail intentionally hides all messages from trashed conversations.
    db.execute(
        "UPDATE records SET data=? WHERE kind='chats' AND id=?",
        params![
            json!({
                "id": CHAT,
                "slug": "historical-answer-lists",
                "agentId": "deleted-agent",
                "title": "Historical answers",
                "paused": true,
                "lifecycle": "active",
                "createdAt": 42,
                "updatedAt": 51,
            })
            .to_string(),
            CHAT
        ],
    )
    .unwrap();
    let answers = json!({
        "old": ["Private free text", "second choice", "Private free text"],
        "empty": [],
        "current": {"selected": ["b", "a"], "comment": "Private comment"},
    });
    db.execute(
        "UPDATE chat_messages SET data=json_set(data,'$.answers',json(?))",
        [answers.to_string()],
    )
    .unwrap();
    db.execute(
        "INSERT INTO kv VALUES(?,?,NULL)",
        params![
            format!("chat-question:{CHAT}:question"),
            json!({
                "id": "question", "chatId": CHAT, "runId": "historical-run",
                "blocking": false, "status": "answered",
                "fields": [{"id": "old", "title": "Private question", "secret": true}],
            })
            .to_string()
        ],
    )
    .unwrap();
    drop(db);

    for _ in 0..2 {
        let store = Store::open(root.path()).unwrap();
        let expected = answers.clone();
        store
            .transaction(move |db| {
                let messages = db.conversation_messages(CHAT)?;
                assert_eq!(messages.len(), 2);
                for message in messages {
                    assert_eq!(serde_json::to_value(&message.answers)?, expected);
                    // Rewriting an existing message must retain its historical answers too.
                    db.save_message(&message)?;
                }
                let detail = crate::chats::public_detail(db, CHAT)?;
                assert_eq!(detail["messages"].as_array().unwrap().len(), 2);
                for message in detail["messages"].as_array().unwrap() {
                    assert_eq!(message["text"], "Private answer");
                    assert!(message.get("answers").is_none());
                }
                assert!(!detail.to_string().contains("Private free text"));
                assert!(!detail.to_string().contains("Private comment"));
                Ok(())
            })
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn malformed_historical_answers_roll_back_without_private_diagnostics() {
    for answer in [
        json!(["synthetic-private-answer", 1]),
        json!([["synthetic-private-answer"], "synthetic-private-comment"]),
        json!({"selected": [1], "comment": "synthetic-private-comment"}),
    ] {
        let root = tempfile::tempdir().unwrap();
        let db = database(root.path()).await;
        seed(&db);
        let answers = json!({"field": answer});
        db.execute(
            "UPDATE chat_messages SET data=json_set(data,'$.answers',json(?)) WHERE id='a-message'",
            [answers.to_string()],
        )
        .unwrap();
        let before: String = db
            .query_row(
                "SELECT group_concat(data) FROM chat_messages ORDER BY rowid",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let error = Store::open(root.path()).err().unwrap();
        assert_eq!(error.message, invalid().message);
        assert_eq!(
            db.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            14
        );
        assert_eq!(
            db.query_row(
                "SELECT group_concat(data) FROM chat_messages ORDER BY rowid",
                [],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
            before
        );
        assert!(
            db.prepare("SELECT data FROM records WHERE kind='chats'")
                .unwrap()
                .exists([])
                .unwrap()
        );
        assert!(db.prepare("SELECT * FROM conversations").is_err());
    }
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
        14
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
            // A real version-14 unique index prevents duplicate slugs already.
            // A non-canonical identity remains a valid old record and must survive.
            let mut other = conversation();
            other["id"] = "second".into();
            other["slug"] = "second-slug".into();
            // Two unkeyed delegations of one requester must both remain valid.
            other["requestedBy"]["requestId"] = Value::Null;
            db.execute(
                "UPDATE records SET data=json_remove(data,'$.requestedBy.requestId') WHERE id=?",
                [CHAT],
            )
            .unwrap();
            db.execute(
                "INSERT INTO records VALUES('second','chats',?,124)",
                [other.to_string()],
            )
            .unwrap();
            assert!(Store::open(root.path()).is_ok());
            assert_eq!(db.query_row("SELECT count(*) FROM conversation_lineage WHERE requester_chat_id='parent-chat' AND request_id IS NULL", [], |r| r.get::<_, i64>(0)).unwrap(), 2);
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
                14
            );
            assert!(!db.table_exists(None, "conversations").unwrap());
        }
    }
}

#[tokio::test]
async fn engine_switch_retains_current_progress_and_all_receipt_lifetimes() {
    let root = tempfile::tempdir().unwrap();
    let db = database(root.path()).await;
    seed(&db);
    // This scenario is a version-14 input, before the current wait tables exist.
    db.execute("INSERT INTO runs(id,task_id,status,created_at,recovery_pending,resume_available,resume_count,boundary) VALUES('saved-run','chat','failed',1,0,0,0,'{}')", []).unwrap();
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

#[tokio::test]
async fn receipts_left_by_an_old_purge_do_not_resurrect_work_or_block_startup() {
    let root = tempfile::tempdir().unwrap();
    let db = database(root.path()).await;
    let receipt = json!({"chatId":"purged","requestId":"old","runId":"removed","turnId":"turn","provider":"codex"});
    db.execute(
        "INSERT INTO kv VALUES('chat-coding-agent-request:purged:old',?,1234)",
        [receipt.to_string()],
    )
    .unwrap();
    db.execute(
        "INSERT INTO kv VALUES('chat-coding-agent:purged',?,NULL)",
        [receipt.to_string()],
    )
    .unwrap();
    let store = Store::open(root.path()).unwrap();
    assert!(store.conversations().await.unwrap().is_empty());
    assert_eq!(
        db.query_row(
            "SELECT count(*) FROM conversation_engine_switches",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
}

fn snapshot(db: &Connection) -> Value {
    let mut state = serde_json::Map::new();
    for table in [
        "coding_agent_accounts",
        "account_usage",
        "account_usage_windows",
        "account_exhaustion",
        "account_leases",
        "shared_objects",
        "shared_publications",
        "shared_references",
        "direct_storage_grants",
        "direct_read_authorizations",
        "direct_journal_acknowledgements",
        "direct_object_quarantines",
        "runs",
        "run_checkpoints",
        "audit",
        "sqlite_sequence",
        "kv",
        "records",
    ] {
        let columns: Vec<String> = db
            .prepare(&format!("PRAGMA table_info({table})"))
            .unwrap()
            .query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        let fields = columns
            .iter()
            .map(|c| format!("quote({c})"))
            .collect::<Vec<_>>()
            .join(",");
        let filter = match table {
            "records" => " WHERE kind!='chats'",
            "kv" => {
                " WHERE key NOT LIKE 'chat-coding-agent:%' AND key NOT LIKE 'chat-coding-agent-request:%' AND key!='conversation-expiration'"
            }
            _ => "",
        };
        let rows: Vec<String> = db
            .prepare(&format!(
                "SELECT json_array({fields}) FROM {table}{filter} ORDER BY 1"
            ))
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        state.insert(table.into(), json!(rows));
    }
    Value::Object(state)
}

#[tokio::test]
async fn actual_version14_preserves_conversations_accounts_leases_and_credentials() {
    let root = tempfile::tempdir().unwrap();
    let db = previous(root.path());
    let chats = Db(&db)
        .json_rows(
            "SELECT data FROM records WHERE kind='chats' ORDER BY id",
            [],
        )
        .unwrap();
    let mut old_messages = Vec::new();
    for chat in &chats {
        let id = chat["id"].as_str().unwrap();
        old_messages.push((
            id.to_owned(),
            Db(&db)
                .json_rows(
                    "SELECT data FROM chat_messages WHERE chat_id=? ORDER BY created_at,rowid",
                    [id],
                )
                .unwrap(),
        ));
    }
    let retained = snapshot(&db);
    assert_eq!(
        db.query_row("SELECT count(*) FROM coding_agent_accounts", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        3
    );
    assert_eq!(
        db.query_row("SELECT count(*) FROM account_leases", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert!(
        db.query_row("SELECT count(*) FROM account_usage_windows", [], |r| r
            .get::<_, i64>(0))
            .unwrap()
            > 1
    );
    for _ in 0..2 {
        let store = Store::open(root.path()).unwrap();
        assert_eq!(
            db.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
                .unwrap(),
            16
        );
        assert_eq!(snapshot(&db), retained);
        for before in &chats {
            let after = Db(&db)
                .conversation_boundary(before["id"].as_str().unwrap())
                .unwrap()
                .unwrap();
            assert_subset(before, &after);
        }
        for (chat, messages) in &old_messages {
            assert_subset(&json!(messages), &json!(Db(&db).messages(chat).unwrap()));
        }
        assert_eq!(
            db.query_row(
                "SELECT inactivity_days FROM conversation_retention",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            19
        );
        let vault = crate::vault::Vault::new(store.clone(), root.path()).unwrap();
        assert_eq!(
            vault
                .get("synthetic:00000000-0000-4000-8000-000000000001")
                .await
                .unwrap(),
            Some(json!({"token":"synthetic-only-do-not-use"}))
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
    }
}
