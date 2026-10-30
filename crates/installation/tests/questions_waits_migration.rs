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
        17
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

#[tokio::test]
async fn migrates_plugin_history_mission_snapshots_and_private_engine_metadata() {
    use serde_json::{Value as Json, json};
    let root = tempfile::tempdir().unwrap();
    let db = previous(root.path());
    let run: String = db
        .query_row("SELECT id FROM runs LIMIT 1", [], |r| r.get(0))
        .unwrap();
    let mut wake: Json = serde_json::from_str(
        &db.query_row(
            "SELECT wakeup FROM conversations WHERE wakeup IS NOT NULL LIMIT 1",
            [],
            |r| r.get::<_, String>(0),
        )
        .unwrap(),
    )
    .unwrap();
    let condition = json!({"type":"github_workflow", "pluginId":"github", "projectId":"project", "runId":40972562294u64, "headSha":"a".repeat(40)});
    let result = json!({"state":"completed", "type":"github_workflow", "pluginId":"github", "runId":40972562294u64, "conclusion":"failure", "nested":{"future":[]}, "reason":"Plugin-owned reason", "immediate":false, "message":{"code":200,"text":"ready"}});
    let mut group = wake.clone();
    group["condition"]["conditions"][0] = condition.clone();
    group["members"][0]["observation"] = result.clone();
    db.execute(
        "UPDATE conversations SET wakeup=? WHERE wakeup IS NOT NULL",
        [group.to_string()],
    )
    .unwrap();
    wake["condition"] = condition.clone();
    wake.as_object_mut().unwrap().remove("members");
    wake["observation"] = result.clone();
    db.execute("UPDATE runs SET boundary=json_set(boundary,'$.wakeup',json(?1),'$.waitContinuation',json(?1),'$.chatExecution.wakeup',json(?1)) WHERE id=?2", rusqlite::params![wake.to_string(),run]).unwrap();
    let mut ready = result.clone();
    ready["reason"] = "Keep the event metadata".into();
    ready["immediate"] = true.into();
    for (kind, payload) in [
        ("run.wait", wake.clone()),
        ("chat.wakeup", wake.clone()),
        ("chat.wait_ready", ready),
        (
            "run.wait_ready",
            json!({"state":"completed","type":"group","mode":"any","members":[{"condition":condition,"result":result}]}),
        ),
    ] {
        db.execute("INSERT INTO events(run_id,created_at,type,text,payload) VALUES(?,123,?,'historical wait',?)", rusqlite::params![run,kind,payload.to_string()]).unwrap();
    }
    let sequence: i64 = db
        .query_row(
            "SELECT seq FROM sqlite_sequence WHERE name='events'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let (key, data): (String, String) = db
        .query_row(
            "SELECT key,data FROM kv WHERE key LIKE 'chat-question:%' LIMIT 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    let mut q: Json = serde_json::from_str(&data).unwrap();
    q["engineOpaque"] = json!({"credential":"private-synthetic", "empty":[]});
    q["fields"][0]["future"] = json!({"unknown":null});
    let mut retired = q.clone();
    retired["id"] = "retired-native".into();
    retired["source"] = "codex".into();
    retired["fields"][0]["options"] = json!([{"label":"First"},{"label":"Second"}]);
    db.execute(
        "INSERT INTO kv VALUES(?,?,NULL)",
        rusqlite::params![
            format!(
                "chat-question:{}:retired-native",
                q["chatId"].as_str().unwrap()
            ),
            retired.to_string()
        ],
    )
    .unwrap();
    let expires = cairn_installation::config::now() + 60000;
    db.execute(
        "UPDATE kv SET data=?,expires=? WHERE key=?",
        rusqlite::params![q.to_string(), expires, key],
    )
    .unwrap();
    let store = Store::open(root.path()).unwrap();
    let run_id = run.clone();
    let loaded = store.run(&run).await.unwrap();
    let typed = loaded["wakeup"].clone();
    let _: cairn_contracts::ChatWakeup = serde_json::from_value(typed.clone()).unwrap();
    assert_eq!(typed["condition"]["type"], "plugin");
    assert_eq!(typed["condition"]["input"]["runId"], 40972562294u64);
    assert_eq!(
        typed["observation"]["pluginResult"]["runId"],
        40972562294u64
    );
    let plugin = &typed["observation"]["pluginResult"];
    assert_eq!(plugin["reason"], "Plugin-owned reason");
    assert_eq!(plugin["immediate"], false);
    assert_eq!(plugin["message"], json!({"code":200,"text":"ready"}));
    let conversations = store.conversations().await.unwrap();
    let migrated_group = conversations
        .iter()
        .find_map(|chat| chat.wakeup.as_ref())
        .unwrap();
    let migrated_group = serde_json::to_value(migrated_group).unwrap();
    assert_eq!(
        migrated_group["members"][0]["observation"]["pluginResult"],
        *plugin
    );
    assert_eq!(loaded["waitContinuation"], typed);
    assert_eq!(loaded["chatExecution"]["wakeup"], typed);
    let page = store
        .read(move |db| db.run_page(None, None, 100, 0))
        .await
        .unwrap();
    assert!(page.iter().any(|row| {
        let value: Json = serde_json::from_str(row.get()).unwrap();
        value["id"] == run_id && value["wakeup"] == typed
    }));
    let events = store
        .read(move |db| db.event_page(&run, 0, 1000))
        .await
        .unwrap();
    for event in events
        .iter()
        .filter(|event| event.text == "historical wait")
    {
        let payload: serde_json::Value =
            serde_json::from_str(event.payload.as_ref().unwrap().get()).unwrap();
        if event.kind.as_str().ends_with("wait_ready") {
            let _: cairn_contracts::WaitObservation =
                serde_json::from_value(payload.clone()).unwrap();
            if payload["type"] != "group" {
                assert_eq!(payload["reason"], "Keep the event metadata");
                assert_eq!(payload["immediate"], true);
                assert_eq!(payload["pluginResult"]["nested"]["future"], json!([]));
                assert_eq!(payload["pluginResult"]["message"], plugin["message"]);
            } else {
                assert_eq!(payload["members"][0]["result"]["pluginResult"], *plugin);
            }
        } else {
            let _: cairn_contracts::ChatWakeup = serde_json::from_value(payload.clone()).unwrap();
        }
    }
    assert_eq!(
        db.query_row(
            "SELECT seq FROM sqlite_sequence WHERE name='events'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        sequence
    );
    let (saved_expiry, metadata): (i64, String) = db
        .query_row(
            "SELECT expires,metadata FROM question_import_metadata WHERE question_id=?",
            [q["id"].as_str().unwrap()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    let metadata: Json = serde_json::from_str(&metadata).unwrap();
    assert_eq!(saved_expiry, expires);
    assert_eq!(metadata["engineOpaque"], q["engineOpaque"]);
    assert_eq!(metadata["fields"][0]["future"], q["fields"][0]["future"]);
    let chat = q["chatId"].as_str().unwrap().to_owned();
    let native_chat = chat.clone();
    let retired = store
        .read(move |db| db.question(&native_chat, "retired-native"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        retired.fields[0]
            .options
            .iter()
            .map(|o| o.label.as_str())
            .collect::<Vec<_>>(),
        ["First", "Second"]
    );
    assert!(retired.fields[0].options.iter().all(|o| o.id.is_empty()));
    let mut delivered = retired.clone();
    delivered.status = cairn_contracts::QuestionStatus::Answered;
    let delivered_chat = delivered.chat_id.clone();
    store
        .transaction(move |db| db.save_question(&delivered))
        .await
        .unwrap();
    let delivered = store
        .read(move |db| db.question(&delivered_chat, "retired-native"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(delivered.status, cairn_contracts::QuestionStatus::Answered);
    assert_eq!(delivered.fields, retired.fields);
    let id = q["id"].as_str().unwrap().to_owned();
    db.execute(
        "UPDATE question_import_metadata SET expires=0 WHERE question_id=?",
        [&id],
    )
    .unwrap();
    assert!(
        store
            .read(move |db| db.question(&chat, &id))
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn nested_failed_wait_does_not_hide_a_subsequent_committed_notification() {
    let root = tempfile::tempdir().unwrap();
    let db = previous(root.path());
    let store = Store::open(root.path()).unwrap();
    let mut conversation = store
        .conversations()
        .await
        .unwrap()
        .into_iter()
        .find(|chat| chat.wakeup.is_some())
        .unwrap();
    let before = store.revision();
    conversation.wakeup.as_mut().unwrap().seconds = 0;
    store
        .transaction(move |db| {
            assert!(db.save_conversation(&conversation).is_err());
            db.0.execute(
                "UPDATE conversation_retention SET inactivity_days=inactivity_days+1",
                [],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    assert_eq!(store.revision(), before + 1);
    assert_eq!(
        db.query_row("SELECT count(*) FROM durable_wakeups", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        1
    );
}

#[test]
fn malformed_v15_answer_lists_roll_back_all_data_and_retry_without_private_diagnostics() {
    use serde_json::json;
    for malformed in [
        json!(["synthetic-private", 1]),
        json!([["synthetic-private"]]),
        json!({"selected":[1],"comment":"synthetic-private"}),
    ] {
        let root = tempfile::tempdir().unwrap();
        let db = previous(root.path());
        db.execute(
            "UPDATE chat_messages SET answers=? WHERE answers IS NOT NULL",
            [json!({"field":malformed}).to_string()],
        )
        .unwrap();
        let before = snapshot(&db);
        let error = Store::open(root.path()).err().unwrap();
        assert!(!error.message.contains("synthetic-private"));
        assert_eq!(snapshot(&db), before);
        assert_eq!(
            db.pragma_query_value::<i64, _>(None, "user_version", |row| row.get(0))
                .unwrap(),
            15
        );
        db.execute(
            "UPDATE chat_messages SET answers=? WHERE answers IS NOT NULL",
            [
                json!({"field":["synthetic-private","second","synthetic-private"],"empty":[]})
                    .to_string(),
            ],
        )
        .unwrap();
        drop(Store::open(root.path()).unwrap());
    }
}
