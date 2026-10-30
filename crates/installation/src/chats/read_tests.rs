use super::*;

#[tokio::test]
async fn card_question_counts_preserve_visibility_without_reading_definitions() {
    let root = tempfile::tempdir().unwrap();
    let store = crate::store::Store::open(root.path()).unwrap();
    store.transaction(|db| {
        for chat in ["chat", "other", "empty"] {
            db.save_conversation_boundary(&json!({ "id": chat }))?;
        }
        let time = crate::config::now();
        for (index, status) in ["pending", "pending", "pending", "pending", "answering", "answered", "cancelled", "pending"].into_iter().enumerate() {
            let question = serde_json::from_value(json!({
                "id": format!("question-{index}"), "chatId": "chat", "runId": "run",
                "source": "cairn", "status": status, "blocking": false, "createdAt": 0,
                "fields": [{ "id": "field", "title": "Retained definition", "mode": "single", "secret": false, "options": [] }],
            }))?;
            db.save_question(&question)?;
            if index == 0 {
                continue;
            }
            let expires = match index {
                1 => Some(time - 1),
                2 => Some(time + 86_400_000),
                3 => Some(time),
                _ => None,
            };
            db.0.execute("INSERT INTO question_import_metadata(question_id,expires,metadata) VALUES(?,?,'{}')", rusqlite::params![format!("question-{index}"), expires])?;
        }
        let other = serde_json::from_value(json!({
            "id": "other-question", "chatId": "other", "runId": "run",
            "source": "cairn", "status": "pending", "blocking": false, "createdAt": 0,
            "fields": [{ "id": "field", "title": "Other definition", "mode": "single", "secret": false, "options": [] }],
        }))?;
        db.save_question(&other)?;
        Ok(())
    }).await.unwrap();

    let connection = rusqlite::Connection::open(root.path().join("manager.db")).unwrap();
    let db = Db(&connection);
    let chat = db.conversation("chat").unwrap().unwrap();
    let expected = conversation_view(&db, chat.clone()).unwrap();
    let reference = db
        .questions("chat")
        .unwrap()
        .iter()
        .filter(|q| q.status == QuestionStatus::Pending)
        .count();
    assert_eq!(reference, 3);
    assert_eq!(expected.pending_questions as usize, reference);
    assert_eq!(db.pending_question_count("other").unwrap(), 1);
    assert_eq!(db.pending_question_count("empty").unwrap(), 0);
    assert_eq!(db.pending_question_count("missing").unwrap(), 0);

    connection
        .create_scalar_function(
            "definition_probe",
            1,
            rusqlite::functions::FunctionFlags::SQLITE_UTF8,
            |_| -> rusqlite::Result<String> {
                Err(rusqlite::Error::UserFunctionError(Box::new(
                    std::io::Error::other("question definition was read"),
                )))
            },
        )
        .unwrap();
    connection.execute_batch("ALTER TABLE question_fields RENAME TO retained_question_fields;
CREATE VIEW question_fields AS SELECT question_key,id,position,definition_probe(title) AS title,secret,mode,min_choices,max_choices FROM retained_question_fields;
PRAGMA query_only=ON; BEGIN;").unwrap();
    let before = connection.total_changes();
    assert!(
        db.questions("chat").is_err(),
        "the reference must exercise the definition probe"
    );
    assert_eq!(
        serde_json::to_value(conversation_view(&db, chat).unwrap()).unwrap(),
        serde_json::to_value(expected).unwrap()
    );
    assert_eq!(connection.total_changes(), before);
}

#[tokio::test]
#[ignore = "explicit conversation-card question-count measurement"]
async fn measure_conversation_cards_with_question_history() {
    use sha2::Digest;

    fn cpu_micros() -> u128 {
        let mut time = std::mem::MaybeUninit::<libc::timespec>::uninit();
        // SAFETY: a successful clock_gettime initializes the supplied timespec.
        assert_eq!(
            unsafe { libc::clock_gettime(libc::CLOCK_PROCESS_CPUTIME_ID, time.as_mut_ptr()) },
            0
        );
        // SAFETY: the successful call above initialized every field.
        let time = unsafe { time.assume_init() };
        time.tv_sec as u128 * 1_000_000 + time.tv_nsec as u128 / 1_000
    }

    for (count, questions_per_chat, every) in [(100, 0, 1), (100, 4, 6), (300, 8, 1)] {
        let root = tempfile::tempdir().unwrap();
        let store = crate::store::Store::open(root.path()).unwrap();
        let write_started = std::time::Instant::now();
        store
            .transaction(move |db| {
                for index in 0..count {
                    let chat = format!("chat-{index}");
                    db.save_conversation_boundary(
                        &json!({ "id": chat, "title": "Measured conversation" }),
                    )?;
                    if index % every != 0 {
                        continue;
                    }
                    for ordinal in 0..questions_per_chat {
                        let question = serde_json::from_value(json!({
                            "id": format!("question-{index}-{ordinal}"), "chatId": chat,
                            "runId": "measured-run", "source": "cairn", "blocking": false,
                            "status": if ordinal % 2 == 0 { "pending" } else { "answered" },
                            "createdAt": ordinal,
                            "fields": [{
                                "id": "field", "title": "Measured question", "secret": false,
                                "mode": "single", "options": (0..8).map(|option| json!({
                                    "id": format!("option-{option}"), "label": "x".repeat(128),
                                    "description": "y".repeat(128), "recommended": false,
                                })).collect::<Vec<_>>(),
                            }],
                        }))?;
                        db.save_question(&question)?;
                    }
                }
                // Make both reference and candidate fixtures byte-for-byte comparable.
                db.0.execute("UPDATE conversations SET slug='measured-'||id,revision='measured',persisted_at=0", [])?;
                Ok(())
            })
            .await
            .unwrap();
        let setup_write_us = write_started.elapsed().as_micros();
        let connection = rusqlite::Connection::open(root.path().join("manager.db")).unwrap();
        connection
            .execute_batch("PRAGMA query_only=ON; BEGIN")
            .unwrap();
        let db = Db(&connection);
        let before_changes = connection.total_changes();
        let started = std::time::Instant::now();
        let cold = list(&db).unwrap();
        let cold_us = started.elapsed().as_micros();
        assert_eq!(cold.len(), count);
        let encoded = serde_json::to_vec(&cold).unwrap();
        let encoded_bytes = encoded.len();
        let encoded_sha256 = format!("{:x}", sha2::Sha256::digest(&encoded));
        let mut samples = Vec::new();
        for _ in 0..7 {
            let cpu = cpu_micros();
            let started = std::time::Instant::now();
            assert_eq!(list(&db).unwrap(), cold);
            samples.push(
                json!({ "wallUs": started.elapsed().as_micros(), "cpuUs": cpu_micros() - cpu }),
            );
        }
        assert_eq!(connection.total_changes(), before_changes);
        println!(
            "{}",
            json!({
                "benchmark": "conversation_cards_question_history", "conversations": count,
                "questionsPerChat": questions_per_chat, "questionEvery": every,
                "setupWriteUs": setup_write_us, "encodedBytes": encoded_bytes,
                "encodedSha256": encoded_sha256,
                "coldConnectionReadUs": cold_us, "warmReadSamples": samples,
                "readChanges": connection.total_changes() - before_changes,
                "sqlite": rusqlite::version(),
            })
        );
    }
}

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
    body_probe(status,text) AS text,provider,model,reasoning,speed,mode,status,created_at,question_id,plan
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
