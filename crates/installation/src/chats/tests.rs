use super::*;

#[tokio::test]
async fn nested_failed_wait_does_not_hide_a_subsequent_committed_notification() {
    let root = tempfile::tempdir().unwrap();
    let store = crate::store::Store::open(root.path()).unwrap();
    let original = store
        .transaction(|db| {
            db.save_conversation_boundary(&json!({ "id": "chat" }))?;
            let mut chat = conversation(db, "chat")?;
            chat.wakeup = Some(serde_json::from_value(json!({
                "id": "wait", "messageId": "request", "reason": "Waiting",
                "seconds": 60, "createdAt": 1, "wakeAt": 60_001,
            }))?);
            let saved = db.save_conversation(&chat)?;
            Ok(saved)
        })
        .await
        .unwrap();
    let before = store.revision();
    let mut invalid = original.clone();
    invalid.wakeup.as_mut().unwrap().seconds = 0;
    store
        .transaction(move |db| {
            assert!(db.save_conversation(&invalid).is_err());
            db.set("committed", &json!(true), None)?;
            Ok(())
        })
        .await
        .unwrap();

    assert_eq!(store.revision(), before + 1);
    assert_eq!(store.kv("committed").await.unwrap(), Some(json!(true)));
    let persisted = store.read(|db| conversation(db, "chat")).await.unwrap();
    assert_eq!(json!(persisted.wakeup), json!(original.wakeup));
}

fn fixture_chat(value: &Value) -> Conversation {
    let mut value = value.clone();
    value
        .as_object_mut()
        .unwrap()
        .entry("id")
        .or_insert_with(|| "chat".into());
    serde_json::from_value(value).unwrap()
}

fn validate_steer(db: &Db<'_>, chat: &Value, message: &Value) -> Result<()> {
    super::validate_steer(
        db,
        &fixture_chat(chat),
        &serde_json::from_value(message.clone())?,
    )
}

fn validate_speed(db: &Db<'_>, chat: &Value, message: &Value) -> Result<()> {
    super::validate_speed(
        db,
        &fixture_chat(chat),
        &serde_json::from_value(message.clone())?,
    )
}

fn require_plan_support(db: &Db<'_>, chat: &Value, message: &Value) -> Result<()> {
    super::require_plan_support(
        db,
        &fixture_chat(chat),
        &serde_json::from_value(message.clone())?,
    )
}

fn preparation_failed(db: &Db<'_>, chat: &Value, error: Error) -> Result<()> {
    super::preparation_failed(db, &fixture_chat(chat), error)
}

fn withdraw(db: &Db<'_>, chat: &str, message: &Value) -> Result<Value> {
    super::withdraw(db, chat, &serde_json::from_value(message.clone())?)
}

fn continue_run(
    db: &Db<'_>,
    chat: &Value,
    run: &Value,
    snapshot: &Value,
    execution: ChatExecution,
) -> Result<()> {
    super::continue_run(db, &fixture_chat(chat), run, snapshot, execution, None)
}

#[tokio::test]
async fn stale_chat_preparation_preserves_the_new_backend_and_the_queued_message() {
    let root = tempfile::tempdir().unwrap();
    let store = crate::store::Store::open(root.path()).unwrap();
    store.transaction(|db| {
        db.add_run_fixture(&json!({
            "id": "run", "taskId": "chat", "createdAt": 0, "status": "succeeded",
            "environmentState": "expired", "environmentAdmissionRevision": "old",
            "snapshot": { "agent": { "id": crate::config::MAIN_AGENT_ID, "provider": "codex", "access": { "backend": "firecracker" } } },
        }), None)?;
        db.save_conversation_boundary(&json!({ "id": "chat", "runId": "run", "agentId": crate::config::MAIN_AGENT_ID, "environmentState": "expired" }))?;
        let stale = db.run("run")?.unwrap();
        let message = db.put_message(&json!({ "id": "next", "chatId": "chat", "text": "Keep queued", "status": "queued", "createdAt": 1 }))?;
        let mut chosen = stale["snapshot"].clone();
        chosen["agent"]["access"]["backend"] = "native".into();
        db.change_run("run", crate::runs::RunChanges {
            snapshot: Some(serde_json::from_value(chosen)?),
            boundary: Some(json!({ "environmentAdmissionRevision": "new" })),
            ..Default::default()
        })?;
        let before = db.run("run")?.unwrap();
        launch_turn(db, "chat", Some(stale.clone()), message, stale)?;
        assert_eq!(db.run("run")?.unwrap(), before);
        assert_eq!(db.messages("chat")?[0]["status"], "queued");
        Ok(())
    }).await.unwrap();
}

#[tokio::test]
async fn stale_chat_preparation_preserves_pending_environment_retirement() {
    let root = tempfile::tempdir().unwrap();
    let store = crate::store::Store::open(root.path()).unwrap();
    store.transaction(|db| {
        crate::chat_plan_settings::save(db, cairn_contracts::ChatPlanSettings {
            enabled: true,
            ..Default::default()
        })?;
        db.add_run_fixture(&json!({
            "id": "run", "taskId": "chat", "status": "succeeded", "trigger": "chat",
            "chatExecution": { "messageId": "previous", "text": "Done", "attachments": [], "recovery": false },
            "snapshot": { "agent": { "id": crate::config::MAIN_AGENT_ID, "provider": "codex" } },
        }), None)?;
        db.save_conversation_boundary(&json!({
            "id": "chat", "runId": "run", "agentId": crate::config::MAIN_AGENT_ID,
            "nativeProvenanceUnknown": false,
        }))?;
        let stale = db.run("run")?.unwrap();
        db.save_agent(&serde_json::from_value(stale["snapshot"]["agent"].clone())?)?;
        let mut chat = conversation(db, "chat")?;
        chat.wakeup = Some(serde_json::from_value(json!({
            "id": "wait", "messageId": "previous", "reason": "Waiting for background work",
            "seconds": 60, "createdAt": 1, "wakeAt": 60_001,
        }))?);
        let preserved_wait = chat.wakeup.clone();
        db.save_conversation(&chat)?;
        let mut saved = RunCheckpoint::fresh(None);
        saved.runner_id = Some(Some("attempt".into()));
        saved.held_environment = Some(crate::worker::checkpoint::HeldEnvironment {
            attempt: "attempt".into(),
            generation: "generation".into(),
            expires_at: now() - 1,
        });
        saved.store_in(db, "run")?;
        let message = send(db, "chat", parse_message(json!({
            "id": crate::config::id(), "text": "Follow-up",
        }))?, None, &crate::native::policy::Origin::default())?;
        assert!(conversation(db, "chat")?.wakeup.is_some());
        let before_admission = db.run("run")?.unwrap();
        assert!(steering_run(db, &before_admission)?.is_none());
        launch_turn(db, "chat", Some(before_admission.clone()), message.clone(), stale.clone())?;
        assert_eq!(db.run("run")?.unwrap(), before_admission);
        assert_eq!(db.messages("chat")?[0]["status"], "queued");
        assert!(RunCheckpoint::load_in(db, "run")?.unwrap().held_environment.is_some());

        saved.retired_environment_deadline = saved.held_environment.take().map(|held| held.expires_at);
        saved.settled = Some(Some(crate::worker::checkpoint::Settled::from_state(
            &db.run_state("run")?.unwrap(),
        )));
        saved.store_in(db, "run")?;
        db.change_run("run", crate::runs::RunChanges {
            status: Some(RunStatus::Queued),
            recovery_pending: Some(true),
            ..Default::default()
        })?;
        let mut retiring = db.run("run")?.unwrap();
        assert!(steering_run(db, &before_admission)?.is_none());
        let arrived = message["createdAt"].as_i64().unwrap();
        let plan = send(db, "chat", parse_message(json!({
            "id": crate::config::id(), "text": "Plan the next change", "mode": "steer", "plan": true,
        }))?, None, &crate::native::policy::Origin::default())?;
        assert_eq!(plan["status"], "queued");
        assert!(conversation(db, "chat")?.wakeup.is_none());
        // The run view now reflects the wait the follow-up cleared on its conversation.
        retiring["wakeup"] = Value::Null;
        launch_turn(db, "chat", Some(stale.clone()), message, stale)?;
        assert_eq!(db.run("run")?.unwrap(), retiring);
        assert_eq!(db.messages("chat")?[0]["status"], "queued");
        let mut saved = RunCheckpoint::load_in(db, "run")?.unwrap();
        assert_eq!(saved.settled().unwrap().status, RunStatus::Succeeded);

        let mut outcome = crate::worker::checkpoint::Settled::failure(
            RunStatus::Failed,
            "The filesystem could not be flushed.",
        );
        outcome.finished_at = Some(arrived + 1);
        saved.settled = Some(Some(outcome.clone()));
        saved.retired_environment_deadline = Some(arrived);
        saved.store_in(db, "run")?;
        let plan = send(db, "chat", parse_message(json!({
            "id": crate::config::id(), "text": "Plan after the interruption", "mode": "steer", "plan": true,
        }))?, None, &crate::native::policy::Origin::default())?;
        assert_eq!(plan["status"], "queued");
        let selected = send(db, "chat", parse_message(json!({
            "id": crate::config::id(), "text": "Choose a model for my turn", "mode": "steer", "model": "chosen-model",
        }))?, None, &crate::native::policy::Origin::default())?;
        let late = parse_message(json!({
            "id": crate::config::id(), "text": "Follow the selected model's turn", "mode": "steer",
        }))?;
        let queued = send(db, "chat", late.clone(), None, &crate::native::policy::Origin::default())?;
        assert_eq!(send(db, "chat", late, None, &crate::native::policy::Origin::default())?, queued);
        db.change_run("run", outcome.run_changes())?;
        settle_turn(db, "chat", &db.run("run")?.unwrap())?;
        assert!(db.messages("chat")?.iter().all(|message| message["status"] == "queued"));
        assert!(!conversation(db, "chat")?.paused);
        let first = next_queued(db, "chat")?.unwrap();
        let mut next = db.run("run")?.unwrap();
        next["status"] = "running".into();
        next["chatExecution"]["messageId"] = first["id"].clone();
        next["snapshot"]["agent"]["model"] = "first-model".into();
        let mut stopping = db.run("run")?.unwrap();
        stopping["cancelRequestedAt"] = (arrived + 2).into();
        assert!(turn_end(db, &stopping)? < arrived);
        saved.retired_environment_deadline = None;
        saved.store_in(db, "run")?;
        assert!(claim_steering(db, "chat", false, &next)?.is_empty());
        assert!(!continuation::has_unhandled_input(db, &conversation(db, "chat")?, &next)?);
        let mut chosen = db.conversation_message("chat", text(&selected, "id"))?.unwrap();
        assert_eq!(chosen.status, MessageStatus::Queued);
        assert_eq!(chosen.model, "chosen-model");
        chosen.model = "first-model".into();
        assert!(!changes_running_agent(&chosen, &next)?);
        assert!(!steering_fits_turn(db, &chosen, &next)?);
        db.save_message(&chosen)?;
        assert!(!continuation::has_unhandled_input(db, &conversation(db, "chat")?, &next)?);
        for closing in [false, true] {
            let mut chat = conversation(db, "chat")?;
            chat.wakeup = preserved_wait.clone();
            db.save_conversation(&chat)?;
            db.change_run("run", crate::runs::RunChanges {
                status: Some(RunStatus::Succeeded), recovery_pending: Some(false), error: Some(None),
                ..Default::default()
            })?;
            saved.retired_environment_deadline = Some(arrived);
            saved.settled = Some(Some(crate::worker::checkpoint::Settled::from_state(&db.run_state("run")?.unwrap())));
            if closing {
                saved.closing_held_generation = Some("generation".into());
                saved.held_environment = Some(crate::worker::checkpoint::HeldEnvironment {
                    attempt: "attempt".into(), generation: "generation".into(), expires_at: arrived,
                });
            }
            saved.store_in(db, "run")?;
            let input = send(db, "chat", parse_message(json!({
                "id": crate::config::id(), "text": "Continue sooner", "mode": "steer",
            }))?, None, &crate::native::policy::Origin::default())?;
            assert!(conversation(db, "chat")?.wakeup.is_none());
            assert_eq!(db.kv(&retained_followup_key(text(&input, "id")))?.is_some(), closing);
        }
        Ok(())
    }).await.unwrap();
}

#[tokio::test]
async fn purging_question_markers_during_a_public_read_never_exposes_its_answer() {
    let root = tempfile::tempdir().unwrap();
    let store = crate::store::Store::open(root.path()).unwrap();
    store
        .transaction(|db| {
            db.save_conversation_boundary(&json!({ "id": "chat" }))?;
            let question: Question = serde_json::from_value(json!({
                "id": "private-question",
                "chatId": "chat",
                "runId": "run",
                "blocking": false,
                "status": "answering",
                "fields": [{ "id": "field", "title": "Private fixture", "secret": true }],
            }))?;
            save_question(db, &question)?;
            db.put_message(&json!({
                "id": "reply",
                "chatId": "chat",
                "text": "private-fixture-answer",
                "questionId": question.id,
                "answers": { "field": { "selected": [], "comment": "private-fixture-answer" } },
            }))?;
            Ok(())
        })
        .await
        .unwrap();
    let path = root.path().join("manager.db");
    let connection = rusqlite::Connection::open(&path).unwrap();
    // The view lets a second WAL connection purge the marker at the exact
    // point the reader captures the message, without production test hooks.
    connection
        .execute_batch(
            "ALTER TABLE chat_messages RENAME TO retained_messages;
CREATE VIEW chat_messages AS SELECT rowid AS rowid,id,chat_id,revision,purge_marker(text) AS text,
provider,model,reasoning,speed,mode,status,created_at,question_id,plan FROM retained_messages;",
        )
        .unwrap();
    connection
        .create_scalar_function(
            "purge_marker",
            1,
            rusqlite::functions::FunctionFlags::SQLITE_UTF8,
            move |context| {
                let text: String = context.get(0)?;
                rusqlite::Connection::open(&path)?.execute(
                    "DELETE FROM chat_questions WHERE chat_id='chat' AND id='private-question'",
                    [],
                )?;
                Ok(text)
            },
        )
        .unwrap();
    let result = public_detail(&Db(&connection), "chat").unwrap();
    assert_eq!(result["messages"][0]["text"], "Private answer");
    assert!(!result.to_string().contains("private-fixture-answer"));
    assert!(connection.is_autocommit());
    assert!(
        Db(&connection)
            .question("chat", "private-question")
            .unwrap()
            .is_none()
    );
}

#[test]
fn automatic_profiles_keep_the_conversations_environment_on_follow_up_turns() {
    for backend in ["firecracker", "native"] {
        for message in [json!({}), json!({ "provider": "claude" })] {
            let previous = json!({ "snapshot": { "agent": { "provider": "codex", "access": { "backend": backend } } } });
            let mut next = json!({ "snapshot": { "agent": { "provider": "codex", "access": { "backend": "auto" } } } });
            select_agent(&mut next, Some(&previous), &message).unwrap();
            assert_eq!(next["snapshot"]["agent"]["access"]["backend"], backend);
        }
    }
}

#[tokio::test]
async fn a_merge_during_preparation_keeps_queued_work_unpaused() {
    let root = tempfile::tempdir().unwrap();
    let store = crate::store::Store::open(root.path()).unwrap();
    store
        .transaction(|db| {
            let stale = json!({ "id": "chat", "agentId": "source", "paused": false });
            let transferred = json!({ "id": "chat", "agentId": "target", "paused": false });
            let transferred = db.save_conversation_boundary(&transferred)?;
            let queued =
                json!({ "id": "message", "chatId": "chat", "createdAt": 1, "status": "queued" });
            let queued = db.put_message(&queued)?;
            // The stale agent lookup failed before launch_turn's identity guard.
            preparation_failed(db, &stale, Error::not_found("Record not found"))?;
            assert_eq!(db.conversation_boundary("chat")?.unwrap(), transferred);
            assert!(db.kv("chat-error:chat")?.is_none());
            assert_eq!(db.messages("chat")?, vec![queued]);
            // A real error on the current profile still pauses and reports normally.
            preparation_failed(db, &transferred, Error::bad("Current preparation failed"))?;
            assert_eq!(db.conversation_boundary("chat")?.unwrap()["paused"], true);
            assert_eq!(
                db.kv("chat-error:chat")?.unwrap()["message"],
                "Current preparation failed"
            );
            Ok(())
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn chats_and_run_summaries_show_the_coding_agent_of_the_latest_turn() {
    let root = tempfile::tempdir().unwrap();
    let store = crate::store::Store::open(root.path()).unwrap();
    store
        .transaction(|db| {
            for (id, agent) in [
                ("switched", json!({ "provider": "claude" })),
                ("codex", json!({ "provider": "codex" })),
            ] {
                let run = json!({
                    "id": id,
                    "taskId": id,
                    "createdAt": 0,
                    "status": "succeeded",
                    "snapshot": { "agent": agent },
                });
                db.add_run_fixture(&run, None)?;
                db.save_conversation_boundary(
                    &json!({ "id": id, "runId": id, "agentId": "agent" }),
                )?;
            }
            db.save_conversation_boundary(
                &json!({ "id": "new", "runId": null, "agentId": "agent" }),
            )?;

            let chats = list(db)?;
            let provider =
                |id: &str| chats.iter().find(|chat| chat["id"] == id).unwrap()["provider"].clone();
            assert_eq!(provider("switched"), "claude");
            assert_eq!(provider("codex"), "codex");
            assert!(provider("new").is_null());

            let runs = db.runs(None, None, 10, 0, false)?;
            let provider =
                |id: &str| runs.iter().find(|run| run["id"] == id).unwrap()["provider"].clone();
            assert_eq!(provider("switched"), "claude");
            assert_eq!(provider("codex"), "codex");
            Ok(())
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn a_failed_turn_returns_what_it_never_delivered() {
    use crate::conversation_writers::{EntryWriter, record_message};

    let root = tempfile::tempdir().unwrap();
    let store = crate::store::Store::open(root.path()).unwrap();
    store
        .transaction(|db| {
            // Stopped before its execution saved a checkpoint.
            let run = db.add_run_fixture(
                &json!({
                    "id": "run",
                    "taskId": "chat",
                    "createdAt": 0,
                    "status": "cancelled",
                    "cancelRequestedAt": 500,
                    "finishedAt": 900,
                    "chatExecution": { "messageId": "turn", "text": "Turn", "attachments": [], "recovery": false },
                    "snapshot": { "agent": { "provider": "codex" } },
                }),
                None,
            )?;
            let profile = serde_json::from_value(run["snapshot"]["agent"].clone())?;
            db.save_agent(&profile)?;
            db.save_conversation_boundary(&json!({
                "id": "chat",
                "runId": "run",
                "agentId": crate::config::MAIN_AGENT_ID,
                "nativeProvenanceUnknown": false,
            }))?;
            let message = |id: &str, status: &str, created_at: i64| {
                json!({
                    "id": id,
                    "chatId": "chat",
                    "text": id,
                    "status": status,
                    "createdAt": created_at,
                })
            };
            db.put_message(&message("delivered", "delivered", 10))?;
            // The turn's own message, never acknowledged, was written before others.
            db.put_message(&message("turn", "sending", 600))?;
            db.put_message(&message("follow-up", "queued", 400))?;
            db.put_message(&message("from-rule", "queued", 420))?;
            let mut answer = message("answer", "queued", 450);
            answer["questionId"] = "question".into();
            db.put_message(&answer)?;
            // Written after Stop was pressed: it starts the next turn.
            db.put_message(&message("after", "queued", 700))?;
            for id in ["turn", "follow-up"] {
                record_message(db, "chat", id, &EntryWriter::Person("alice".into()))?;
            }
            record_message(db, "chat", "from-rule", &EntryWriter::Rule("rule".into()))?;

            settle_turn(db, "chat", &run)?;
            // A turn is settled once: a person's later resumption is left alone.
            assert!(settled(db, &run)?);
            pause_in(db, "chat", false)?;
            settle_turn(db, "chat", &run)?;
            assert!(!conversation(db, "chat")?.paused);
            pause_in(db, "chat", true)?;
            let status = |id: &str| -> Result<MessageStatus> {
                Ok(find_message(db, "chat", id)?.unwrap().status)
            };
            assert_eq!(status("delivered")?, MessageStatus::Delivered);
            assert_eq!(status("turn")?, MessageStatus::Cancelled);
            assert_eq!(status("follow-up")?, MessageStatus::Cancelled);
            // A rule has no composer: its message stays queued behind a pause, as before.
            assert_eq!(status("from-rule")?, MessageStatus::Queued);
            assert!(conversation(db, "chat")?.paused);
            // Answers are not returned, and a message written since starts the next turn.
            assert_eq!(status("answer")?, MessageStatus::Queued);
            assert_eq!(status("after")?, MessageStatus::Queued);
            assert!(unsent(db, "chat", &run)?.returning.is_empty());

            // Only the person who wrote a message gets it back.
            let returned_to = |id: &str| -> Result<Value> {
                Ok(db
                    .messages("chat")?
                    .into_iter()
                    .find(|message| message["id"] == id)
                    .unwrap()["returnedTo"]
                    .clone())
            };
            assert_eq!(returned_to("turn")?, "alice");
            assert_eq!(returned_to("follow-up")?, "alice");
            assert!(returned_to("from-rule")?.is_null());
            assert!(returned_to("after")?.is_null());
            // The turn whose message went back cannot be replayed.
            assert!(turn_returned(db, "run")?);
            let refused = crate::worker::control::requeue(db, "run", None, None, None, None)
                .unwrap_err();
            assert!(refused.message.contains("went back"), "{}", refused.message);

            // Retrying a send whose message came back queues nothing: it is refused.
            let retried = send(
                db,
                "chat",
                json!({ "id": "follow-up", "text": "follow-up" }),
                None,
                &crate::native::policy::Origin::Human("alice".into()),
            )
            .unwrap_err();
            assert!(retried.message.contains("cancelled"), "{}", retried.message);

            // The composer that restores a returned message withdraws it and its mark.
            edit_as(db, "chat", "turn", None, &crate::native::policy::Origin::Human("alice".into()))?;
            assert!(find_message(db, "chat", "turn")?.is_none());
            assert!(db.kv(&returned_key("turn"))?.is_none());

            // A message cancelled for another reason, such as deletion, goes to nobody.
            db.put_message(&message("deleted", "cancelled", 300))?;
            record_message(db, "chat", "deleted", &EntryWriter::Person("alice".into()))?;
            assert!(returned_to("deleted")?.is_null());

            // The next message starts a turn although the stopped one never launched, with
            // a new retry budget.
            db.change_run(
                "run",
                crate::runs::RunChanges {
                    retry: Some(Some(serde_json::from_value(json!({
                        "attempt": 5, "limit": 5, "nextAttemptAt": null,
                        "cause": { "kind": "timeout", "message": "Old" },
                        "startedAt": 1, "deadlineAt": 2,
                    }))?)),
                    ..Default::default()
                },
            )?;
            pause_in(db, "chat", false)?;
            let after = db.messages("chat")?.into_iter().find(|message| message["id"] == "after").unwrap();
            launch_turn(db, "chat", Some(run.clone()), after, db.run("run")?.unwrap())?;
            let launched = db.run("run")?.unwrap();
            assert_eq!(launched["status"], "queued");
            assert_eq!(launched["chatExecution"]["messageId"], "after");
            assert!(launched["retry"].is_null());
            assert_eq!(status("after")?, MessageStatus::Sending);
            assert!(!turn_returned(db, "run")?);
            Ok(())
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn a_failed_compaction_returns_nothing_and_holds_rule_messages() {
    let root = tempfile::tempdir().unwrap();
    let store = crate::store::Store::open(root.path()).unwrap();
    store
        .transaction(|db| {
            let run = db.add_run_fixture(
                &json!({
                    "id": "run",
                    "taskId": "chat",
                    "createdAt": 0,
                    "status": "failed",
                    "finishedAt": 900,
                    "chatExecution": { "messageId": "first", "text": "", "attachments": [], "recovery": false, "operation": COMPACT },
                    "snapshot": { "agent": { "provider": "codex" } },
                }),
                None,
            )?;
            db.save_conversation_boundary(&json!({
                "id": "chat",
                "runId": "run",
                "agentId": "agent",
            }))?;
            db.put_message(&json!({
                "id": "person",
                "chatId": "chat",
                "text": "Next",
                "status": "queued",
                "createdAt": 100,
            }))?;
            crate::conversation_writers::record_message(
                db,
                "chat",
                "person",
                &crate::conversation_writers::EntryWriter::Person("alice".into()),
            )?;
            db.put_message(&json!({
                "id": "rule",
                "chatId": "chat",
                "text": "From a rule",
                "status": "queued",
                "createdAt": 200,
            }))?;
            // Nothing is returned: a person's message follows the compaction, while a
            // rule's waits behind a pause for a person, as after any failure.
            let unsent = unsent(db, "chat", &run)?;
            assert!(unsent.returning.is_empty());
            assert_eq!(unsent.held.iter().map(|message| message.id.as_str()).collect::<Vec<_>>(), ["rule"]);
            settle_turn(db, "chat", &run)?;
            assert!(conversation(db, "chat")?.paused);
            Ok(())
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn uncertain_launches_and_answers_are_held_not_returned() {
    use crate::conversation_writers::{EntryWriter, record_message};

    for (launched, answer) in [(true, false), (false, true)] {
        let root = tempfile::tempdir().unwrap();
        let store = crate::store::Store::open(root.path()).unwrap();
        store
            .transaction(move |db| {
                let run = db.add_run_fixture(
                    &json!({
                        "id": "run",
                        "taskId": "chat",
                        "createdAt": 0,
                        "startedAt": 1,
                        "status": "failed",
                        "finishedAt": 900,
                        "chatExecution": { "messageId": "turn", "text": "Turn", "attachments": [], "recovery": false },
                        "snapshot": { "agent": { "provider": "codex" } },
                    }),
                    None,
                )?;
                db.save_checkpoint_boundary("run", &json!({ "launched": launched }))?;
                db.save_conversation_boundary(&json!({
                    "id": "chat",
                    "runId": "run",
                    "agentId": "agent",
                }))?;
                let mut turn = json!({
                    "id": "turn",
                    "chatId": "chat",
                    "text": "turn",
                    "status": "sending",
                    "createdAt": 600,
                });
                if answer {
                    turn["questionId"] = "question".into();
                }
                db.put_message(&turn)?;
                record_message(db, "chat", "turn", &EntryWriter::Person("alice".into()))?;
                let steering = json!({
                    "id": "steering",
                    "chatId": "chat",
                    "text": "steer",
                    "mode": "steer",
                    "status": "sending",
                    "createdAt": 650,
                });
                db.put_message(&steering)?;
                record_message(db, "chat", "steering", &EntryWriter::Person("alice".into()))?;

                // The agent may have the message, or an answer cannot go back to a
                // composer: either waits, queued again, behind a pause as before.
                settle_turn(db, "chat", &run)?;
                let turn = find_message(db, "chat", "turn")?.unwrap();
                assert_eq!(turn.status, MessageStatus::Queued, "launched {launched}");
                // Steering sent to a launched turn may have been read; never launched, it
                // goes back with the turn.
                let steering = find_message(db, "chat", "steering")?.unwrap();
                let expected = if launched { MessageStatus::Queued } else { MessageStatus::Cancelled };
                assert_eq!(steering.status, expected, "launched {launched}");
                // A message written since does not skip the replay of the held turn, which
                // tells the agent to check what it already did.
                db.put_message(&json!({
                    "id": "since",
                    "chatId": "chat",
                    "text": "since",
                    "status": "queued",
                    "createdAt": 1000,
                }))?;
                assert!(!skips_replay(db, "chat", &run)?);
                assert!(conversation(db, "chat")?.paused);
                assert!(!turn_returned(db, "run")?);
                Ok(())
            })
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn launches_without_a_message_are_not_this_turns() {
    use crate::conversation_writers::{EntryWriter, record_message};

    // A mission continued in chat: its launch delivered none of the chat's messages.
    // A turn without a message that launched may have read the steering it claimed.
    for (message_id, returned) in [(Some("turn"), true), (None, false)] {
        let root = tempfile::tempdir().unwrap();
        let store = crate::store::Store::open(root.path()).unwrap();
        store
            .transaction(move |db| {
                let mut execution = json!({ "text": "", "attachments": [], "recovery": false });
                if let Some(id) = message_id {
                    execution["messageId"] = id.into();
                }
                let run = db.add_run_fixture(
                    &json!({
                        "id": "run",
                        "taskId": "chat",
                        "createdAt": 0,
                        "startedAt": 1,
                        "status": "cancelled",
                        "cancelRequestedAt": 500,
                        "finishedAt": 900,
                        "chatExecution": execution,
                        "snapshot": { "agent": { "provider": "codex" } },
                    }),
                    None,
                )?;
                db.save_checkpoint_boundary(
                    "run",
                    &json!({
                        "launched": true,
                        "launchedTurn": "",
                    }),
                )?;
                db.save_conversation_boundary(&json!({
                    "id": "chat",
                    "runId": "run",
                    "agentId": "agent",
                }))?;
                if message_id.is_some() {
                    db.put_message(&json!({
                        "id": "turn",
                        "chatId": "chat",
                        "text": "turn",
                        "status": "sending",
                        "createdAt": 100,
                    }))?;
                    record_message(db, "chat", "turn", &EntryWriter::Person("alice".into()))?;
                }
                db.put_message(&json!({
                    "id": "steering",
                    "chatId": "chat",
                    "text": "steer",
                    "mode": "steer",
                    "status": "sending",
                    "createdAt": 200,
                }))?;
                record_message(db, "chat", "steering", &EntryWriter::Person("alice".into()))?;
                settle_turn(db, "chat", &run)?;
                let expected = if returned {
                    MessageStatus::Cancelled
                } else {
                    MessageStatus::Queued
                };
                assert_eq!(
                    find_message(db, "chat", "steering")?.unwrap().status,
                    expected,
                    "{message_id:?}"
                );
                if returned {
                    assert_eq!(
                        find_message(db, "chat", "turn")?.unwrap().status,
                        MessageStatus::Cancelled
                    );
                    assert!(turn_returned(db, "run")?);
                }
                Ok(())
            })
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn an_answer_claimed_after_stop_replays_the_stopped_turn() {
    let root = tempfile::tempdir().unwrap();
    let store = crate::store::Store::open(root.path()).unwrap();
    store
        .transaction(|db| {
            let run = db.add_run_fixture(
                &json!({
                    "id": "run",
                    "taskId": "chat",
                    "createdAt": 0,
                    "startedAt": 1,
                    "status": "cancelled",
                    "cancelRequestedAt": 500,
                    "finishedAt": 900,
                    "chatExecution": { "messageId": "turn", "text": "Turn", "attachments": [], "recovery": false },
                    "snapshot": { "agent": { "provider": "codex" } },
                }),
                None,
            )?;
            db.save_checkpoint_boundary("run", &json!({
                "launched": true,
                "launchedTurn": "turn",
            }))?;
            db.save_conversation_boundary(&json!({
                "id": "chat",
                "runId": "run",
                "agentId": "agent",
            }))?;
            db.put_message(&json!({
                "id": "turn",
                "chatId": "chat",
                "text": "turn",
                "status": "delivered",
                "createdAt": 100,
            }))?;
            // Answered while the turn was being stopped: the stopping agent claimed it.
            db.put_message(&json!({
                "id": "answer",
                "chatId": "chat",
                "text": "answer",
                "questionId": "question",
                "mode": "steer",
                "status": "sending",
                "createdAt": 600,
            }))?;
            db.put_message(&json!({
                "id": "since",
                "chatId": "chat",
                "text": "since",
                "status": "queued",
                "createdAt": 1000,
            }))?;
            settle_turn(db, "chat", &run)?;
            // The agent may have read the answer: it is held, and resuming replays the
            // turn first, as a recovery, before the message written since.
            assert_eq!(find_message(db, "chat", "answer")?.unwrap().status, MessageStatus::Sending);
            assert!(conversation(db, "chat")?.paused);
            assert!(turn_held(db, "chat", &run)?);
            assert!(!skips_replay(db, "chat", &run)?);
            Ok(())
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn later_turns_and_steering_return_only_what_never_left() {
    use crate::conversation_writers::{EntryWriter, record_message};
    use crate::native::policy::Origin;

    let root = tempfile::tempdir().unwrap();
    let store = crate::store::Store::open(root.path()).unwrap();
    store
        .transaction(|db| {
            // An earlier turn launched; this turn's message never reached a process.
            let run = db.add_run_fixture(
                &json!({
                    "id": "run",
                    "taskId": "chat",
                    "createdAt": 0,
                    "startedAt": 1,
                    "status": "cancelled",
                    "cancelRequestedAt": 500,
                    "finishedAt": 900,
                    "chatExecution": { "messageId": "second", "text": "Second", "attachments": [], "recovery": false },
                    "snapshot": { "agent": { "provider": "codex" } },
                }),
                None,
            )?;
            db.save_checkpoint_boundary("run", &json!({
                "launched": true,
                "launchedTurn": "first",
            }))?;
            db.save_conversation_boundary(&json!({
                "id": "chat",
                "runId": "run",
                "agentId": "agent",
            }))?;
            let message = |id: &str, status: &str, created_at: i64| {
                json!({
                    "id": id,
                    "chatId": "chat",
                    "text": id,
                    "status": status,
                    "createdAt": created_at,
                })
            };
            db.put_message(&message("first", "delivered", 10))?;
            db.put_message(&message("second", "sending", 400))?;
            // Claimed for steering, but no process launched for this turn to read it.
            let mut steering = message("steering", "sending", 450);
            steering["mode"] = "steer".into();
            db.put_message(&steering)?;
            db.put_message(&message("since", "queued", 700))?;
            for id in ["second", "steering", "since"] {
                record_message(db, "chat", id, &EntryWriter::Person("alice".into()))?;
            }
            // The turn's own message still waits: resuming replays it first.
            assert!(!skips_replay(db, "chat", &run)?);

            settle_turn(db, "chat", &run)?;
            // Once it went back to its author, resuming sends what was written since.
            assert!(skips_replay(db, "chat", &run)?);
            assert_eq!(find_message(db, "chat", "second")?.unwrap().status, MessageStatus::Cancelled);
            assert_eq!(find_message(db, "chat", "steering")?.unwrap().status, MessageStatus::Cancelled);
            // Nothing is held: the cancellation itself paused the conversation, not this.
            assert!(!conversation(db, "chat")?.paused);

            // Only the author of a returned message takes it back.
            let refused = edit_as(db, "chat", "second", None, &Origin::Human("bob".into())).unwrap_err();
            assert!(refused.message.contains("person who wrote it"), "{}", refused.message);
            edit_as(db, "chat", "second", None, &Origin::Human("alice".into()))?;
            assert!(find_message(db, "chat", "second")?.is_none());
            Ok(())
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn a_turn_after_a_failure_gives_up_the_retained_environment() {
    let root = tempfile::tempdir().unwrap();
    let store = crate::store::Store::open(root.path()).unwrap();
    store
        .transaction(|db| {
            let run = db.add_run_fixture(
                &json!({
                    "id": "run",
                    "taskId": "chat",
                    "createdAt": 0,
                    "startedAt": 1,
                    "status": "failed",
                    "finishedAt": 900,
                    "sessionId": "native",
                    "chatExecution": { "messageId": "turn", "text": "Turn", "attachments": [], "recovery": false },
                    "snapshot": { "agent": { "provider": "codex" } },
                }),
                None,
            )?;
            let profile = serde_json::from_value(run["snapshot"]["agent"].clone())?;
            db.save_agent(&profile)?;
            db.save_conversation_boundary(&json!({
                "id": "chat",
                "runId": "run",
                "agentId": crate::config::MAIN_AGENT_ID,
            }))?;
            let mut saved = RunCheckpoint::fresh(None);
            saved.launched = Some(true);
            saved.prepared = Some(Some(json!({ "workspace": "preserved" })));
            saved.runner_id = Some(Some("attempt".into()));
            saved.remaining_ms = Some(Some(0));
            saved.held_environment = Some(crate::worker::checkpoint::HeldEnvironment {
                attempt: "attempt".into(),
                generation: "generation".into(),
                expires_at: 1,
            });
            saved.store_in(db, "run")?;
            let message = db.put_message(&json!({
                "id": "next",
                "chatId": "chat",
                "text": "Again",
                "status": "queued",
                "createdAt": 1000,
            }))?;
            launch_turn(db, "chat", Some(run.clone()), message, run)?;
            // Like a manual resume: the expired hold goes, recovery fences it, and the new
            // turn has a full budget instead of the old deadline.
            let checkpoint = RunCheckpoint::load_in(db, "run")?.unwrap();
            assert!(checkpoint.held_environment.is_none());
            assert_ne!(checkpoint.remaining_ms, Some(Some(0)));
            assert_eq!(db.run("run")?.unwrap()["recoveryPending"], true);
            Ok(())
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn compaction_runs_only_between_turns_and_keeps_the_last_reply() {
    let root = tempfile::tempdir().unwrap();
    let store = crate::store::Store::open(root.path()).unwrap();
    store
        .transaction(|db| {
            let origin = crate::native::policy::Origin::default();
            let outcome = json!({ "status": "completed", "reason": "Done", "messageId": "first" });
            let run = json!({
                "id": "run",
                "taskId": "chat",
                "createdAt": 0,
                "status": "succeeded",
                "sessionId": "native-session",
                "summary": "Last reply",
                "finishedAt": 500,
                "outcome": outcome,
                "contextUsage": { "contextTokens": 150_000, "contextWindow": 200_000, "cachedTokens": 120_000, "estimated": false },
                "chatExecution": { "messageId": "first", "text": "First", "attachments": [], "recovery": false },
                "snapshot": { "agent": { "provider": "codex" } },
            });
            db.add_run_fixture(&run, None)?;
            db.save_checkpoint_boundary("run", &json!({ "completed": true, "launched": true }))?;
            db.save_conversation_boundary(&json!({
                    "id": "chat",
                    "runId": "run",
                    "agentId": "agent",
                    "nativeProvenanceUnknown": false,
                }),
            )?;

            let idle = view(db, chat(db, "chat")?)?;
            assert_eq!(idle["contextUsage"]["contextTokens"], 150_000);
            assert_eq!(idle["compaction"], json!({ "supported": true, "running": false, "available": true, "reason": null }));

            let mut saved = RunCheckpoint::load_in(db, "run")?.unwrap();
            saved.held_environment = Some(crate::worker::checkpoint::HeldEnvironment {
                attempt: "attempt".into(),
                generation: "generation".into(),
                expires_at: now() - 1,
            });
            saved.store_in(db, "run")?;
            let expiring = view(db, chat(db, "chat")?)?;
            assert_eq!(expiring["compaction"]["available"], false);
            let error = compact(db, "chat", &origin).unwrap_err();
            assert_eq!(expiring["compaction"]["reason"], error.message);
            assert_eq!(db.run("run")?.unwrap()["summary"], "Last reply");
            saved.held_environment = None;
            saved.store_in(db, "run")?;

            // A queued follow-up is delivered first: compaction never reorders the queue.
            let queued = json!({ "id": "queued", "chatId": "chat", "text": "Next", "status": "queued", "createdAt": 1 });
            db.put_message(&queued)?;
            let error = compact(db, "chat", &origin).unwrap_err();
            assert!(error.message.contains("queued messages"), "{}", error.message);
            withdraw(db, "chat", &queued)?;

            let started = compact(db, "chat", &origin)?;
            let marker: String = db.0.query_row(
                "SELECT payload FROM events WHERE run_id='run' AND type='status' AND text='Compacting the conversation context' ORDER BY id DESC LIMIT 1",
                [], |row| row.get(0),
            )?;
            assert_eq!(serde_json::from_str::<Value>(&marker)?, json!({ "chatOperation": COMPACT }));
            assert_eq!(started["compaction"]["running"], true);
            assert_eq!(started["compaction"]["available"], false);
            let compacting = db.run("run")?.unwrap();
            assert_eq!(compacting["status"], "queued");
            assert_eq!(compacting["recoveryPending"], true);
            assert_eq!(compacting["chatExecution"]["operation"], COMPACT);
            assert_eq!(
                compacting["chatExecution"]["previous"],
                json!({ "summary": "Last reply", "finishedAt": 500, "outcome": outcome })
            );
            // The previous reply and its outcome are not replaced by a compaction.
            assert_eq!(compacting["summary"], "Last reply");
            assert_eq!(compacting["outcome"], outcome);
            assert!(db.messages("chat")?.is_empty());

            // A retried request keeps the compaction already started.
            let retried = compact(db, "chat", &origin)?;
            assert_eq!(db.run("run")?.unwrap()["chatExecution"], compacting["chatExecution"]);
            assert_eq!(retried["compaction"]["running"], true);
            Ok(())
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn compaction_explains_why_it_is_unavailable() {
    let root = tempfile::tempdir().unwrap();
    let store = crate::store::Store::open(root.path()).unwrap();
    store
        .transaction(|db| {
            let cases = [
                (
                    "opencode",
                    "succeeded",
                    Some("session"),
                    "OpenCode does not support",
                ),
                (
                    "claude",
                    "failed",
                    Some("session"),
                    "Continue or resolve the last turn",
                ),
                (
                    "claude",
                    "running",
                    Some("session"),
                    "Wait for the current turn",
                ),
                ("codex", "succeeded", None, "no saved native session"),
            ];
            for (index, (provider, status, session, reason)) in cases.into_iter().enumerate() {
                let id = format!("case-{index}");
                db.add_run_fixture(
                    &json!({
                        "id": id,
                        "taskId": id,
                        "createdAt": 0,
                        "status": status,
                        "sessionId": session,
                        "snapshot": { "agent": { "provider": provider } },
                    }),
                    None,
                )?;
                db.save_conversation_boundary(&json!({
                        "id": id,
                        "runId": id,
                        "agentId": "agent",
                        "nativeProvenanceUnknown": false,
                    }),
                )?;
                let state = view(db, chat(db, &id)?)?["compaction"].clone();
                assert_eq!(state["available"], false, "{provider} {status}");
                assert_eq!(state["supported"], provider != "opencode");
                assert!(text(&state, "reason").contains(reason), "{state}");
                assert!(view(db, chat(db, &id)?)?["contextUsage"].is_null());
                let error =
                    compact(db, &id, &crate::native::policy::Origin::default()).unwrap_err();
                assert!(error.message.contains(reason), "{}", error.message);
            }

            db.save_conversation_boundary(&json!({ "id": "empty", "runId": null, "agentId": "agent" }),
            )?;
            let state = view(db, chat(db, "empty")?)?["compaction"].clone();
            assert_eq!(state["supported"], false);
            assert!(text(&state, "reason").contains("first message"));

            // A run that names no coding agent still shows its conversation.
            db.add_run_fixture(
                &json!({ "id": "unnamed", "taskId": "unnamed", "createdAt": 0, "status": "succeeded" }),
                None,
            )?;
            db.save_conversation_boundary(&json!({
                    "id": "unnamed",
                    "runId": "unnamed",
                    "agentId": "agent",
                    "nativeProvenanceUnknown": false,
                }),
            )?;
            let state = view(db, chat(db, "unnamed")?)?["compaction"].clone();
            assert_eq!(state["supported"], false);
            assert_eq!(state["available"], false);
            assert!(text(&state, "reason").contains("does not support"));
            Ok(())
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn continuation_consumes_chat_and_checkpoint_fresh_session_requests_once() {
    use crate::store::Store;

    for checkpoint_restart in [false, true] {
        for (isolated, native) in [(false, false), (true, false), (true, true)] {
            let root = tempfile::tempdir().unwrap();
            let store = Store::open(root.path()).unwrap();
            store
                .transaction(move |db| {
                    let run = json!({
                        "id": "run",
                        "taskId": "chat",
                        "createdAt": 0,
                        "status": "failed",
                        "sessionId": "old-native",
                        "snapshot": { "agent": { "provider": "codex" } },
                    });
                    let run = db.add_run_fixture(&run, None)?;
                    let profile = serde_json::from_value(run["snapshot"]["agent"].clone())?;
                    db.save_agent(&profile)?;
                    db.event("run", &EventKind::ChatUser, "Context to preserve", None)?;
                    let chat = json!({
                        "id": "chat",
                        "runId": "run",
                        "sessionRestartRequested": !checkpoint_restart,
                        "agentId": crate::config::MAIN_AGENT_ID,
                    });
                    db.save_conversation_boundary(&chat)?;
                    db.save_question(&serde_json::from_value(json!({
                        "id": "old-session", "chatId":"chat", "runId":"run",
                        "source":"codex", "status":"pending", "blocking":true,
                        "fields":[{"id":"field","title":"Question from previous session"}],
                    }))?)?;
                    let mut prepared = json!({ "isolated": isolated, "workspace": "preserved" });
                    if native {
                        prepared["backend"] = "native".into();
                    }
                    db.save_checkpoint_boundary(
                        "run",
                        &json!({
                            "freshSession": checkpoint_restart,
                            "completed": true,
                            "launched": true,
                            "prepared": prepared,
                            "controllerRecoveries": 2,
                        }),
                    )?;
                    let message = json!({
                        "id": "next",
                        "chatId": "chat",
                        "text": "Continue",
                        "status": "queued",
                        "createdAt": 1,
                    });
                    let message = db.put_message(&message)?;
                    launch_turn(db, "chat", Some(run.clone()), message, run)?;
                    let queued = db.run("run")?.unwrap();
                    assert_eq!(queued["status"], "queued");
                    assert!(queued["sessionId"].is_null());
                    assert_eq!(queued["resumeAvailable"], false);
                    assert!(
                        text(&queued["chatExecution"], "context").contains("Context to preserve")
                    );
                    let checkpoint = db.checkpoint_boundary("run")?.unwrap();
                    assert_eq!(checkpoint["prepared"], prepared);
                    assert_eq!(checkpoint["launched"], false);
                    assert_eq!(checkpoint["workspaceInitialized"], true);
                    assert_eq!(checkpoint["freshSession"], false);
                    assert!(checkpoint["controllerRecoveries"].is_null());
                    let chat = db.conversation_boundary("chat")?.unwrap();
                    assert_eq!(chat["sessionRestartRequested"], false);
                    let question =
                        serde_json::to_value(db.question("chat", "old-session")?.unwrap())?;
                    assert_eq!(question["status"], "cancelled");
                    assert_eq!(question["blocking"], false);

                    // A later ordinary continuation resumes the newly established session.
                    let _resumed = db.change_run(
                        "run",
                        crate::runs::RunChanges {
                            session_id: Some(Some("new-native".into())),
                            ..Default::default()
                        },
                    )?;
                    let resumed = db.run("run")?.unwrap();
                    let mut interrupted = db.checkpoint_boundary("run")?.unwrap();
                    interrupted["completed"] = false.into();
                    interrupted["outputInterrupted"] = true.into();
                    db.save_checkpoint_boundary("run", &interrupted)?;
                    continue_run(
                        db,
                        &chat,
                        &resumed,
                        &resumed,
                        ChatExecution {
                            message_id: "later".into(),
                            text: "Keep going".into(),
                            attachments: Vec::new(),
                            recovery: false,
                            context: None,
                            plan: false,
                            after_plan: false,
                            operation: None,
                            previous: None,
                        },
                    )?;
                    let next = db.run("run")?.unwrap();
                    assert!(db.checkpoint_boundary("run")?.unwrap()["outputInterrupted"].is_null());
                    assert_eq!(next["sessionId"], "new-native");
                    assert!(next["chatExecution"]["context"].is_null());
                    Ok(())
                })
                .await
                .unwrap();
        }
    }
}

#[tokio::test]
async fn codex_threads_of_missions_continued_in_chat_receive_the_interactive_rule() {
    // Provider, thread the mission started, and whether each turn carries the rule.
    for (provider, source, expected) in [
        ("codex", Some("thread"), true),
        ("codex", Some("earlier-thread"), false),
        ("codex", None, false),
        ("claude", Some("thread"), false),
    ] {
        let root = tempfile::tempdir().unwrap();
        let store = crate::store::Store::open(root.path()).unwrap();
        store
            .transaction(move |db| {
                let mut run = json!({
                    "id": "run",
                    "taskId": "chat",
                    "createdAt": 0,
                    "status": "succeeded",
                    "trigger": "chat",
                    "sessionId": "thread",
                    "snapshot": { "agent": { "provider": provider } },
                });
                if let Some(source) = source {
                    run["sourceTaskPrompt"] = "Check the deployment.".into();
                    run["sourceSessionId"] = source.into();
                }
                let run = db.add_run_fixture(&run, None)?;
                db.save_checkpoint_boundary("run", &json!({ "completed": true }))?;
                let chat = json!({ "id": "chat", "runId": "run" });
                db.save_conversation_boundary(&chat)?;
                // Compaction can drop earlier messages, so every turn repeats it.
                for message in ["Now fix it", "Thanks"] {
                    let execution = ChatExecution {
                        message_id: message.into(),
                        text: message.into(),
                        attachments: Vec::new(),
                        recovery: false,
                        context: None,
                        plan: false,
                        after_plan: false,
                        operation: None,
                        previous: None,
                    };
                    continue_run(db, &chat, &run, &run, execution)?;
                    let continued = db.run("run")?.unwrap();
                    let turn = text(&continued["chatExecution"], "text");
                    assert_eq!(
                        turn.contains(crate::run_output::INTERACTIVE),
                        expected,
                        "{provider} {source:?}"
                    );
                    assert_eq!(turn.starts_with(CONTINUED_MISSION), expected);
                    assert!(turn.ends_with(message));
                    assert_eq!(continued["sessionId"], "thread");
                }
                Ok(())
            })
            .await
            .unwrap();
    }
}

#[test]
fn implicit_follow_up_inherits_the_previous_engine_model_and_reasoning() {
    let template = json!({
        "snapshot": {
            "agent": { "provider": "codex", "model": "gpt-6-sol", "reasoning": "high" },
        },
    });
    let previous = json!({
        "snapshot": {
            "agent": { "provider": "opencode", "model": "opencode/ling-3.1-flash-free", "reasoning": "medium" },
        },
    });
    let mut snapshot = template.clone();
    select_agent(
        &mut snapshot,
        Some(&previous),
        &json!({ "text": "Continue" }),
    )
    .unwrap();
    assert_eq!(snapshot["snapshot"]["agent"], previous["snapshot"]["agent"]);

    let mut explicit = template;
    select_agent(
        &mut explicit,
        Some(&previous),
        &json!({ "provider": "opencode" }),
    )
    .unwrap();
    assert_eq!(
        explicit["snapshot"]["agent"]["model"],
        "opencode/ling-3.1-flash-free"
    );
    assert_eq!(explicit["snapshot"]["agent"]["reasoning"], "medium");
}

#[test]
fn provider_changes_must_wait_for_the_next_turn_and_model_aliases_are_valid() {
    let root = tempfile::tempdir().unwrap();
    drop(crate::store::Store::open(root.path()).unwrap());
    let connection = rusqlite::Connection::open(root.path().join("manager.db")).unwrap();
    let run = json!({
        "status": "running",
        "snapshot": {
            "agent": { "provider": "codex", "model": "gpt-6-sol", "reasoning": "high" },
        },
    });
    let db = Db(&connection);
    let mut run = run;
    run["id"] = "run".into();
    run["taskId"] = "chat".into();
    db.add_run_fixture(&run, None).unwrap();
    let chat = json!({ "runId": "run" });
    let mut message = parse_message(json!({
        "id": id(),
        "text": "Continue",
        "provider": "claude",
        "model": "opus[1m]",
        "mode": "steer",
    }))
    .unwrap();
    assert_eq!(
        validate_steer(&db, &chat, &message).unwrap_err().status,
        409
    );
    message["mode"] = "queue".into();
    validate_steer(&db, &chat, &message).unwrap();
    message["mode"] = "steer".into();
    message["provider"] = "codex".into();
    message["model"] = "gpt-6-sol".into();
    validate_steer(&db, &chat, &message).unwrap();

    // The running turn keeps its speed; the run has none, so only an explicit one differs.
    message["speed"] = "priority".into();
    assert_eq!(
        validate_steer(&db, &chat, &message).unwrap_err().status,
        409
    );
    message["speed"] = "".into();
    validate_steer(&db, &chat, &message).unwrap();
    // Asking for Standard does not change a turn without a speed: it runs at Standard.
    message["speed"] = "standard".into();
    validate_steer(&db, &chat, &message).unwrap();
    message["speed"] = "".into();

    let unknown = json!({
        "id": id(),
        "text": "Continue",
        "provider": "unknown"
    });
    assert!(parse_message(unknown).is_err());
}

#[tokio::test]
async fn plan_requests_cannot_steer_a_working_turn_or_run_on_opencode() {
    let root = tempfile::tempdir().unwrap();
    let store = crate::store::Store::open(root.path()).unwrap();
    store
        .transaction(|db| {
            let claude = cairn_contracts::AgentId(uuid::Uuid::new_v4());
            let settings = serde_json::from_value(json!({
                "name": "claude",
                "provider": "claude",
                "access": cairn_contracts::AccessPolicy::default(),
                "exclusiveNodes": [],
                "plugins": [],
            }))?;
            db.save_agent(&crate::agents::new_agent(settings, Some(claude)))?;
            for (id, provider, status, chat_execution) in [
                (
                    "working",
                    "claude",
                    "running",
                    json!({ "messageId": "first" }),
                ),
                (
                    "planning",
                    "claude",
                    "running",
                    json!({ "messageId": "first", "plan": true }),
                ),
                ("legacy", "opencode", "succeeded", Value::Null),
            ] {
                db.add_run_fixture(
                    &json!({
                        "id": id,
                        "taskId": id,
                        "createdAt": 0,
                        "status": status,
                        "snapshot": { "agent": { "provider": provider } },
                        "chatExecution": chat_execution,
                    }),
                    None,
                )?;
            }
            let chat = |id: &str| json!({ "id": id, "runId": id, "agentId": claude });
            let (working, planning_chat, opencode) =
                (chat("working"), chat("planning"), chat("legacy"));
            let mut message = parse_message(
                json!({ "id": id(), "text": "Plan instead", "mode": "steer", "plan": true }),
            )?;

            // A plan request never joins a turn that can already change files.
            assert_eq!(
                validate_steer(db, &working, &message).unwrap_err().status,
                409
            );
            validate_steer(db, &planning_chat, &message)?;
            // Ordinary steering of a plan turn stays restricted by that turn.
            message["plan"] = false.into();
            validate_steer(db, &planning_chat, &message)?;

            message["plan"] = true.into();
            message["mode"] = "queue".into();
            validate_steer(db, &working, &message)?;
            require_plan_support(db, &working, &message)?;
            // The inherited engine and an explicit choice are both checked.
            assert_eq!(
                require_plan_support(db, &opencode, &message)
                    .unwrap_err()
                    .status,
                400
            );
            message["provider"] = "codex".into();
            require_plan_support(db, &opencode, &message)?;
            message["provider"] = "opencode".into();
            assert!(require_plan_support(db, &working, &message).is_err());

            // A queued message switching to OpenCode decides for the plan request after it.
            db.save_conversation_boundary(&working)?;
            db.put_message(&json!({
                "id": "ahead",
                "chatId": "working",
                "createdAt": 1,
                "status": "queued",
                "provider": "opencode",
            }))?;
            message["provider"] = "".into();
            assert!(require_plan_support(db, &working, &message).is_err());

            // Older clients omit the field: their messages are never plan turns.
            let older = parse_message(json!({ "id": id(), "text": "Go" }))?;
            assert_eq!(older["plan"], false);
            require_plan_support(db, &opencode, &older)?;
            Ok(())
        })
        .await
        .unwrap();
}

#[test]
fn an_edit_without_a_plan_request_keeps_the_stored_one() {
    let kept = parse_edit(json!({ "id": id(), "text": "Revised" })).unwrap();
    assert!(kept.get("plan").is_none());
    let mut stored = json!({ "text": "Plan it", "plan": true });
    crate::store::merge(&mut stored, &kept);
    assert_eq!(stored["plan"], true);
    let ordinary = parse_edit(json!({ "id": id(), "text": "Do it", "plan": false })).unwrap();
    assert_eq!(ordinary["plan"], false);
}

#[test]
fn plan_turns_mark_the_execution_and_the_turn_that_follows() {
    let run = |plan: bool| json!({ "chatExecution": { "plan": plan } });
    assert!(planning(&run(true)));
    assert!(!planning(&run(false)));
    assert!(!planning(&json!({})));

    let execution = ChatExecution {
        message_id: "message".into(),
        text: "Implement".into(),
        attachments: Vec::new(),
        recovery: false,
        context: None,
        plan: false,
        after_plan: true,
        operation: None,
        previous: None,
    };
    let value = serde_json::to_value(execution).unwrap();
    assert!(value.get("plan").is_none());
    assert_eq!(value["afterPlan"], true);

    // A compaction starts no turn: the turn after it still follows the plan turn.
    assert!(follows_plan(&run(true)));
    assert!(!follows_plan(&run(false)));
    let compaction = |after_plan: bool| json!({ "chatExecution": { "operation": COMPACT, "afterPlan": after_plan } });
    assert!(follows_plan(&compaction(true)));
    assert!(!follows_plan(&compaction(false)));
    assert!(!follows_plan(
        &json!({ "chatExecution": { "afterPlan": true } })
    ));

    let event = serde_json::to_value(UserMessageEvent {
        message_id: "message",
        question_id: None,
        text: "Plan it",
        attachments: &[],
        created_at: 1,
        plan: true,
    })
    .unwrap();
    assert_eq!(event["plan"], true);
    let ordinary = serde_json::to_value(UserMessageEvent {
        message_id: "message",
        question_id: None,
        text: "Do it",
        attachments: &[],
        created_at: 1,
        plan: false,
    })
    .unwrap();
    assert!(ordinary.get("plan").is_none());
}

#[test]
fn speed_is_independent_of_the_model_but_belongs_to_codex() {
    let codex = |speed: Option<&str>| {
        let mut agent = json!({ "provider": "codex", "model": "gpt-6-sol", "reasoning": "high" });
        if let Some(speed) = speed {
            agent["speed"] = speed.into();
        }
        json!({ "snapshot": { "agent": agent } })
    };
    let select = |previous: &Value, message: Value| {
        let mut snapshot = codex(None);
        select_agent(&mut snapshot, Some(previous), &message).unwrap();
        snapshot["snapshot"]["agent"].clone()
    };

    // A model change resets the reasoning level, not the speed.
    let previous = codex(Some("priority"));
    let agent = select(&previous, json!({ "model": "gpt-6-astra" }));
    assert_eq!(agent["model"], "gpt-6-astra");
    assert_eq!(agent["reasoning"], "");
    assert_eq!(agent["speed"], "priority");

    // Reasoning changes keep it too, and an explicit speed replaces it.
    let agent = select(&previous, json!({ "reasoning": "low" }));
    assert_eq!(agent["speed"], "priority");
    let agent = select(&previous, json!({ "speed": "standard" }));
    assert_eq!(agent["speed"], "standard");
    assert_eq!(agent["reasoning"], "high");

    // Earlier runs and messages without a speed keep none.
    let agent = select(&codex(None), json!({ "text": "Continue" }));
    assert!(agent.get("speed").is_none());

    // Naming the conversation's coding agent keeps its latest turn's speed, even after the
    // agent's profile gained another default.
    let mut faster_profile = codex(Some("priority"));
    let continued = json!({ "provider": "codex", "model": "gpt-6-sol", "reasoning": "high" });
    select_agent(&mut faster_profile, Some(&codex(None)), &continued).unwrap();
    assert!(faster_profile["snapshot"]["agent"].get("speed").is_none());
    let mut standard_profile = codex(None);
    select_agent(&mut standard_profile, Some(&previous), &continued).unwrap();
    assert_eq!(standard_profile["snapshot"]["agent"]["speed"], "priority");

    // Another coding agent has no speed, and returning to Codex starts from its default.
    let agent = select(&previous, json!({ "provider": "claude" }));
    assert!(agent.get("speed").is_none());
    let claude = json!({ "snapshot": { "agent": { "provider": "claude", "model": "opus", "speed": "priority" } } });
    let agent = select(&claude, json!({ "text": "Continue" }));
    assert!(agent.get("speed").is_none());
    let agent = select(&claude, json!({ "provider": "codex" }));
    assert!(agent.get("speed").is_none());
    let agent = select(&claude, json!({ "provider": "codex", "speed": "priority" }));
    assert_eq!(agent["speed"], "priority");
}

#[tokio::test]
async fn a_speed_is_refused_for_turns_another_coding_agent_runs() {
    let root = tempfile::tempdir().unwrap();
    let store = crate::store::Store::open(root.path()).unwrap();
    store
        .transaction(|db| {
            let codex = cairn_contracts::AgentId(uuid::Uuid::new_v4());
            let claude = cairn_contracts::AgentId(uuid::Uuid::new_v4());
            for (agent_id, provider) in [(codex, "codex"), (claude, "claude")] {
                let settings = serde_json::from_value(json!({
                    "name": provider,
                    "provider": provider,
                    "access": cairn_contracts::AccessPolicy::default(),
                    "exclusiveNodes": [],
                    "plugins": [],
                }))?;
                db.save_agent(&crate::agents::new_agent(settings, Some(agent_id)))?;
            }
            let run = json!({
                "id": "claude-run",
                "taskId": "switched",
                "createdAt": 0,
                "status": "succeeded",
                "snapshot": { "agent": { "provider": "claude" } },
            });
            db.add_run_fixture(&run, None)?;
            let new_codex = json!({ "id": "new-codex", "runId": null, "agentId": codex });
            let new_claude = json!({ "id": "new-claude", "runId": null, "agentId": claude });
            let switched = json!({ "id": "switched", "runId": "claude-run", "agentId": codex });
            db.save_conversation_boundary(&switched)?;
            let fast =
                |provider: &str| json!({ "id": id(), "provider": provider, "speed": "priority" });
            let refused = |chat: &Value, message: &Value| {
                validate_speed(db, chat, message).unwrap_err().status == 400
            };

            // Without a provider, the message runs with the conversation's coding agent.
            validate_speed(db, &new_codex, &fast(""))?;
            assert!(refused(&new_claude, &fast("")));
            assert!(refused(&switched, &fast("")));
            validate_speed(db, &switched, &fast("codex"))?;
            assert!(refused(&new_codex, &fast("opencode")));
            validate_speed(db, &new_claude, &json!({ "id": id(), "speed": "" }))?;
            // Codex's name for Standard is not a speed choice.
            assert!(refused(
                &new_codex,
                &json!({ "id": id(), "speed": "default" })
            ));

            // A queued message switching coding agent decides for the ones after it.
            let mut ahead = json!({
                "id": "ahead",
                "chatId": "switched",
                "createdAt": 1,
                "status": "queued",
                "provider": "codex",
            });
            db.put_message(&ahead)?;
            validate_speed(db, &switched, &fast(""))?;
            ahead["status"] = "delivered".into();
            db.put_message(&ahead)?;
            assert!(refused(&switched, &fast("")));

            // A steering message joins the running turn, whatever the queue switches to next.
            let running = json!({
                "id": "codex-run",
                "taskId": "steering",
                "createdAt": 0,
                "status": "running",
                "snapshot": { "agent": { "provider": "codex", "speed": "priority" } },
            });
            db.add_run_fixture(&running, None)?;
            let steering = json!({ "id": "steering", "runId": "codex-run", "agentId": codex });
            db.save_conversation_boundary(&steering)?;
            let switching = json!({
                "id": "switching",
                "chatId": "steering",
                "createdAt": 1,
                "status": "queued",
                "provider": "claude",
            });
            db.put_message(&switching)?;
            let mut steer = fast("");
            steer["mode"] = "steer".into();
            validate_speed(db, &steering, &steer)?;
            steer["mode"] = "queue".into();
            assert!(refused(&steering, &steer));
            Ok(())
        })
        .await
        .unwrap();
}
