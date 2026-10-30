use super::*;

#[tokio::test]
async fn lightweight_cards_match_full_execution_metadata() {
    let root = tempfile::tempdir().unwrap();
    let store = crate::store::Store::open(root.path()).unwrap();
    store.transaction(|db| {
        for (index, status) in ["queued", "running", "waiting", "succeeded", "failed", "interrupted", "cancelled"].into_iter().enumerate() {
            for provider in ["codex", "claude", "opencode", "unknown"] {
                for fresh in [false, true] {
                    let id = format!("{status}-{provider}-{fresh}");
                    let state = serde_json::from_value(json!({
                        "id": id, "taskId": id, "status": status, "sessionId": "saved",
                        "finishedAt": 123, "accountWaitReason": "fixture wait", "accountRequired": "codex",
                    }))?;
                    db.admit_run(&state, None, &json!({
                        "contextUsage": { "contextTokens": 42, "contextWindow": 100, "estimated": true },
                        "restoredAt": 1, "suspendedByExclusive": if fresh { json!(false) } else { json!({ "reason": "fixture" }) },
                        "chatExecution": { "operation": if index == 1 { "compact" } else { "message" } },
                    }), None)?;
                    db.0.execute("UPDATE runs SET snapshot=? WHERE id=?", rusqlite::params![
                        json!({ "agent": { "provider": provider, "instructions": "x".repeat(128 * 1024) } }).to_string(), id,
                    ])?;
                    db.0.execute("INSERT INTO run_checkpoints(run_id,state) VALUES(?,?)", rusqlite::params![
                        id, json!({ "freshSession": fresh, "prepared": { "fixture": "y".repeat(128 * 1024) } }).to_string(),
                    ])?;
                    let chat: Conversation = serde_json::from_value(json!({ "id": id, "runId": id, "agentName": "Retained agent" }))?;
                    let full_run = db.run(&id)?;
                    let expected = conversation_view_with_run(db, chat.clone(), full_run.as_ref())?;
                    let actual = conversation_view(db, chat)?;
                    assert_eq!(serde_json::to_value(&actual)?, serde_json::to_value(expected)?, "{id}");
                    if status == "succeeded" && provider == "codex" {
                        assert_eq!(actual.compaction.unwrap().available, !fresh);
                    }
                }
            }
        }
        let chat: Conversation = serde_json::from_value(json!({ "id": "no-run" }))?;
        let expected = conversation_view_with_run(db, chat.clone(), None)?;
        assert_eq!(serde_json::to_value(conversation_view(db, chat)?)?, serde_json::to_value(expected)?);
        Ok(())
    }).await.unwrap();
}

#[tokio::test]
async fn lightweight_runs_keep_the_plan_boundary_of_the_turn() {
    let root = tempfile::tempdir().unwrap();
    let store = crate::store::Store::open(root.path()).unwrap();
    store
        .transaction(|db| {
            for (id, execution) in [
                ("planning", json!({ "messageId": "m", "plan": true })),
                (
                    "compacting-after-plan",
                    json!({ "operation": "compact", "afterPlan": true }),
                ),
                ("ordinary", json!({ "messageId": "m" })),
                (
                    "explicitly-ordinary",
                    json!({ "messageId": "m", "plan": false, "afterPlan": false }),
                ),
            ] {
                let state =
                    serde_json::from_value(json!({ "id": id, "taskId": id, "status": "running" }))?;
                db.admit_run(&state, None, &json!({ "chatExecution": execution }), None)?;
                let full = db.run(id)?.unwrap();
                let light = db.conversation_run(id)?.unwrap();
                assert_eq!(planning(&light), planning(&full), "{id}");
                assert_eq!(follows_plan(&light), follows_plan(&full), "{id}");
                assert_eq!(is_compaction(&light), is_compaction(&full), "{id}");
            }
            Ok(())
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn a_window_does_not_read_delivered_message_bodies() {
    let root = tempfile::tempdir().unwrap();
    let store = crate::store::Store::open(root.path()).unwrap();
    store
        .transaction(|db| {
            db.save_conversation_boundary(&json!({ "id": "chat" }))?;
            for status in ["delivered", "queued", "cancelled"] {
                db.put_message(
                    &json!({ "id": status, "chatId": "chat", "text": status, "status": status }),
                )?;
            }
            Ok(())
        })
        .await
        .unwrap();
    let connection = rusqlite::Connection::open(root.path().join("manager.db")).unwrap();
    connection.execute_batch("ALTER TABLE chat_messages RENAME TO retained_messages;
CREATE VIEW chat_messages AS SELECT rowid AS rowid,id,chat_id,revision,
    body_probe(status,text) AS text,provider,model,reasoning,speed,mode,status,created_at,question_id,answers,plan
    FROM retained_messages;").unwrap();
    connection
        .create_scalar_function(
            "body_probe",
            2,
            rusqlite::functions::FunctionFlags::SQLITE_UTF8,
            |ctx| {
                if ctx.get::<String>(0)? == "delivered" {
                    return Err(rusqlite::Error::UserFunctionError(Box::new(
                        std::io::Error::other("delivered body was read"),
                    )));
                }
                ctx.get::<String>(1)
            },
        )
        .unwrap();
    let db = Db(&connection);
    let window = public_stream_detail(&db, "chat", true).unwrap();
    assert_eq!(window["messages"].as_array().unwrap().len(), 2);
    assert!(
        window["messages"]
            .as_array()
            .unwrap()
            .iter()
            .all(|message| message["status"] != "delivered")
    );
    assert!(
        public_detail(&db, "chat").is_err(),
        "the full read must exercise the probe"
    );
}
