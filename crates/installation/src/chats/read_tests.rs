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
                    // Side-question availability reads the stop request and the announcement.
                    let state = serde_json::from_value(json!({
                        "id": id, "taskId": id, "status": status, "sessionId": "saved",
                        "finishedAt": 123, "accountWaitReason": "fixture wait", "accountRequired": "codex",
                        "cancelRequestedAt": if fresh { Value::Null } else { json!(7) },
                    }))?;
                    db.admit_run(&state, None, &json!({
                        "contextUsage": { "contextTokens": 42, "contextWindow": 100, "estimated": true },
                        "restoredAt": 1, "suspendedByExclusive": if fresh { json!(false) } else { json!({ "reason": "fixture" }) },
                        "chatExecution": { "operation": if index == 1 { "compact" } else { "message" } },
                        "sideQuestions": fresh,
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
        // A running Claude reply that announced side questions takes them in lists too,
        // unless it is stopping.
        for stopping in [false, true] {
            let id = format!("side-{stopping}");
            let state = serde_json::from_value(json!({
                "id": id, "taskId": id, "status": "running",
                "cancelRequestedAt": if stopping { json!(7) } else { Value::Null },
            }))?;
            db.admit_run(&state, None, &json!({
                "chatExecution": { "operation": "message" }, "sideQuestions": true,
            }), None)?;
            db.0.execute("UPDATE runs SET snapshot=? WHERE id=?", rusqlite::params![
                json!({ "agent": { "provider": "claude" } }).to_string(), id,
            ])?;
            let chat: Conversation = serde_json::from_value(json!({ "id": id, "runId": id }))?;
            let full_run = db.run(&id)?;
            let expected = conversation_view_with_run(db, chat.clone(), full_run.as_ref())?;
            let actual = conversation_view(db, chat)?;
            assert_eq!(serde_json::to_value(&actual)?, serde_json::to_value(expected)?, "{id}");
            assert_eq!(actual.can_ask_side_question, !stopping, "{id}");
        }
        let chat: Conversation = serde_json::from_value(json!({ "id": "no-run" }))?;
        let expected = conversation_view_with_run(db, chat.clone(), None)?;
        assert_eq!(serde_json::to_value(conversation_view(db, chat)?)?, serde_json::to_value(expected)?);
        Ok(())
    }).await.unwrap();
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
    body_probe(status,text) AS text,provider,model,reasoning,speed,mode,status,created_at,question_id
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

#[tokio::test]
async fn legacy_and_current_answers_preserve_shapes_order_and_saved_privacy() {
    use serde_json::json;
    for sensitivity in [Some(true), Some(false), None] {
        let root = tempfile::tempdir().unwrap();
        let db = rusqlite::Connection::open(root.path().join("manager.db")).unwrap();
        db.execute_batch(include_str!("../../tests/fixtures/questions-waits-v15.sql"))
            .unwrap();
        let (question_key, definition): (String, String) = db
            .query_row(
                "SELECT key,data FROM kv WHERE key LIKE 'chat-question:%' LIMIT 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        let mut question: serde_json::Value = serde_json::from_str(&definition).unwrap();
        let private = sensitivity.unwrap_or(true);
        question["fields"][0]["secret"] = private.into();
        if sensitivity.is_some() {
            db.execute(
                "UPDATE kv SET data=? WHERE key=?",
                [question.to_string(), question_key],
            )
            .unwrap();
        } else {
            db.execute("DELETE FROM kv WHERE key=?", [question_key])
                .unwrap();
        }
        let chat = question["chatId"].as_str().unwrap().to_owned();
        let field = question["fields"][0]["id"].as_str().unwrap().to_owned();
        // Legacy lists have neither current choice/comment limits nor identities.
        let private_text = "Private historical free text. ".repeat(500);
        let mut items = vec![private_text.clone(), "Second choice".into(), private_text];
        items.extend((0..33).map(|index| format!("Historical item {index}")));
        let answers = json!({
            (field): items,
            "empty": [],
            "current": {"selected": ["second", "first"], "comment": "Private current comment"},
        });
        let delivery = "Private historical delivery text";
        db.execute(
            "UPDATE chat_messages SET answers=?1,text=?2 WHERE chat_id=?3",
            rusqlite::params![answers.to_string(), delivery, chat],
        )
        .unwrap();
        let expected_ids = db
            .prepare("SELECT id FROM chat_messages WHERE chat_id=? ORDER BY created_at,rowid")
            .unwrap()
            .query_map([&chat], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert!(!expected_ids.is_empty());
        let mut replacement: cairn_contracts::ChatQuestion =
            serde_json::from_value(question).unwrap();
        for field in &mut replacement.fields {
            field.secret = !private;
        }

        for _ in 0..2 {
            let store = crate::store::Store::open(root.path()).unwrap();
            let chat = chat.clone();
            let expected = answers.clone();
            let expected_ids = expected_ids.clone();
            let replacement = replacement.clone();
            store
                .transaction(move |db| {
                    let messages = db.conversation_messages(&chat)?;
                    assert_eq!(
                        messages
                            .iter()
                            .map(|message| &message.id)
                            .collect::<Vec<_>>(),
                        expected_ids.iter().collect::<Vec<_>>()
                    );
                    for message in messages {
                        assert_eq!(serde_json::to_value(&message.answers)?, expected);
                        assert_eq!(message.text, delivery);
                        db.save_message(&message)?;
                    }
                    let assert_projection = |detail: serde_json::Value| {
                        assert_eq!(
                            detail["messages"].as_array().unwrap().len(),
                            expected_ids.len()
                        );
                        for message in detail["messages"].as_array().unwrap() {
                            assert_eq!(
                                message["text"],
                                if private { "Private answer" } else { delivery }
                            );
                            assert!(message.get("answers").is_none());
                        }
                        if private {
                            assert!(!detail.to_string().contains("Private historical"));
                            assert!(!detail.to_string().contains("Private current comment"));
                        }
                    };
                    assert_projection(public_detail(db, &chat)?);
                    db.0.execute("DELETE FROM chat_questions WHERE chat_id=?", [&chat])?;
                    assert_projection(public_detail(db, &chat)?);
                    let run: String = db.0.query_row("SELECT id FROM runs LIMIT 1", [], |row| row.get(0))?;
                    let mut conversation = conversation(db, &chat)?;
                    conversation.run_id = Some(run.clone());
                    db.save_conversation(&conversation)?;
                    for restore_definition in [false, true] {
                        if restore_definition {
                            db.save_question(&replacement)?;
                        }
                        assert_projection(public_detail(db, &chat)?);
                        for mut message in db.conversation_messages(&chat)? {
                            message.status = MessageStatus::Queued;
                            db.save_message(&message)?;
                            acknowledge(db, &run, &message.id)?;
                            let (event_text, payload): (String, String) = db.0.query_row(
                                "SELECT text,payload FROM events WHERE run_id=? AND type='chat.user' ORDER BY id DESC LIMIT 1",
                                [&run], |row| Ok((row.get(0)?, row.get(1)?)),
                            )?;
                            let expected_text = if private { PRIVATE_ANSWER_TEXT } else { delivery };
                            assert_eq!(event_text, expected_text);
                            assert_eq!(serde_json::from_str::<Value>(&payload)?["text"], expected_text);
                        }
                    }
                    let stored: String = db.0.query_row(
                        "SELECT text FROM chat_messages WHERE chat_id=? LIMIT 1",
                        [&chat],
                        |row| row.get(0),
                    )?;
                    assert_eq!(stored, "Questionnaire response");
                    Ok(())
                })
                .await
                .unwrap();
            drop(store);
        }
    }
}
