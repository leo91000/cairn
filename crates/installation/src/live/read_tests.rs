use super::*;

#[test]
fn a_reference_invalidates_the_streamed_reply_baseline() {
    let card = |revision: i64, text: &str| -> TimelineEntry {
        serde_json::from_value(serde_json::json!({
            "id": "message:1:a", "runId": "run", "position": 1, "revision": revision,
            "source": revision, "turn": 1, "attempt": 0, "createdAt": 1, "updatedAt": revision,
            "content": {"kind": "agentMessage", "itemId": "a", "text": text, "state": "running"}
        }))
        .unwrap()
    };
    let mut replies = Replies::default();
    replies.encode(vec![card(1, "before")], false);
    replies.forget(&[LargeTimelineReference {
        snapshot: card(2, ""),
    }]);
    assert!(matches!(
        replies
            .encode(vec![card(3, "before changed")], false)
            .as_slice(),
        [TimelineChange::Upsert { .. }]
    ));
}

#[tokio::test]
#[ignore = "explicit conversation list and live-page read measurements"]
async fn measure_conversation_reads_by_count_and_snapshot_size() {
    use serde_json::json;

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

    for count in [1, 100, 300] {
        for kib in [0, 128, 512] {
            let root = tempfile::tempdir().unwrap();
            let store = crate::store::Store::open(root.path()).unwrap();
            let snapshot = cairn_contracts::RunSnapshot {
                task: serde_json::from_value(json!({
                    "id": "00000000-0000-4000-8000-000000000003",
                    "name": "Measured task", "prompt": "Measured prompt",
                    "agentId": crate::config::MAIN_AGENT_ID,
                }))
                .unwrap(),
                agent: serde_json::from_value(json!({
                    "id": crate::config::MAIN_AGENT_ID, "name": "Measured agent",
                    "revision": "measured", "createdAt": 0,
                    "instructions": "x".repeat(kib * 1024),
                    "access": cairn_contracts::AccessPolicy::default(),
                    "exclusiveNodes": [], "plugins": [],
                }))
                .unwrap(),
                project: None,
                projects: vec![],
                available_projects: vec![],
                skills: vec![],
                delegation: None,
            };
            store
                .transaction(move |db| {
                    for index in 0..count {
                        let run = format!("run-{index}");
                        let chat = format!("chat-{index}");
                        let state = serde_json::from_value(json!({
                            "id": run, "taskId": chat, "status": "running",
                        }))?;
                        db.admit_run(&state, Some(&snapshot), &json!({}), None)?;
                        db.save_conversation_boundary(&json!({
                            "id": chat, "runId": run, "title": "Measured conversation",
                            "agentId": crate::config::MAIN_AGENT_ID,
                        }))?;
                        db.event(
                            &run,
                            &cairn_contracts::EventKind::Output,
                            "Measured output",
                            None,
                        )?;
                    }
                    Ok(())
                })
                .await
                .unwrap();

            let path = root.path().join("manager.db");
            let connection = rusqlite::Connection::open(&path).unwrap();
            let db = Db(&connection);
            let snapshot_bytes: i64 = connection
                .query_row(
                    "SELECT length(CAST(snapshot AS BLOB)) FROM runs WHERE id='run-0'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            let list_scope = Scope::new("chats", "", "measured-account");
            let lists = ConversationLists::default();
            let mut scope = Scope::new("chats", "chat-0", "measured-account");

            for operation in ["list_rebuild", "live_page", "live_page_cached_list"] {
                scope.with_list = operation == "live_page_cached_list";
                lists.get(&db, &list_scope, 1).unwrap();
                let read = |connection: &rusqlite::Connection| {
                    let transaction = connection.unchecked_transaction().unwrap();
                    let db = Db(&transaction);
                    if operation == "list_rebuild" {
                        return serde_json::to_vec(&conversations(&db, &list_scope).unwrap())
                            .unwrap()
                            .len();
                    }
                    let page = read_page(
                        &db,
                        &scope,
                        Request {
                            after: 0,
                            expected: None,
                            window: true,
                            recent: false,
                            resume: true,
                        },
                        &lists,
                        1,
                        None,
                    )
                    .unwrap();
                    // Include state serialization, as on the first live response.
                    // SSE framing and event serialization are outside this measurement.
                    assert_eq!(page.events.len(), 1);
                    serde_json::to_vec(&page.state).unwrap().len()
                };
                // A fresh private SQLite page cache; the host's OS cache stays warm.
                let measured_connection = rusqlite::Connection::open_with_flags(
                    &path,
                    rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY
                        | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX
                        | rusqlite::OpenFlags::SQLITE_OPEN_PRIVATE_CACHE,
                )
                .unwrap();
                let first = Instant::now();
                let encoded_bytes = read(&measured_connection);
                let sqlite_cold_us = first.elapsed().as_micros();
                let mut warm_us = Vec::new();
                let cpu = cpu_micros();
                for _ in 0..16 {
                    let started = Instant::now();
                    assert_eq!(
                        std::hint::black_box(read(&measured_connection)),
                        encoded_bytes
                    );
                    warm_us.push(started.elapsed().as_micros());
                }
                let warm_cpu_us = cpu_micros() - cpu;
                warm_us.sort_unstable();
                println!(
                    "conversation_read_measurement {}",
                    json!({
                        "operation": operation, "conversations": count,
                        "snapshot_bytes": snapshot_bytes, "encoded_state_or_list_bytes": encoded_bytes,
                        "sqlite_cold_us": sqlite_cold_us, "warm_median_us": warm_us[8],
                        "warm_min_us": warm_us[0], "warm_max_us": warm_us[15],
                        "warm_cpu_us": warm_cpu_us, "samples": warm_us.len(),
                        "database_bytes": std::fs::metadata(&path).unwrap().len(),
                        "wal_bytes": std::fs::metadata(path.with_extension("db-wal")).map_or(0, |file| file.len()),
                    })
                );
            }
        }
    }
}

#[test]
fn a_streaming_reply_resends_non_text_changes_whole() {
    let reply = |revision: i64, text: &str, echo: bool| TimelineEntry {
        id: "message:1:a".into(),
        run_id: "run".into(),
        position: 1,
        revision,
        source: revision,
        turn: 1,
        attempt: 1,
        created_at: 1,
        updated_at: revision,
        content: TimelineContent::AgentMessage {
            item_id: "a".into(),
            text: text.into(),
            state: TimelineState::Running,
            via: None,
            echo,
        },
    };
    let mut replies = Replies::default();
    assert!(matches!(
        replies.encode(vec![reply(1, "One", false)], false)[0],
        TimelineChange::Upsert { .. }
    ));
    assert!(matches!(
        replies.encode(vec![reply(2, "One two", false)], false)[0],
        TimelineChange::Append { .. }
    ));
    assert!(matches!(
        replies.encode(vec![reply(3, "One two!", true)], false)[0],
        TimelineChange::Upsert { .. }
    ));
}

#[tokio::test]
async fn list_cache_coalesces_revisions_and_preserves_account_and_external_refreshes() {
    let root = tempfile::tempdir().unwrap();
    let _store = crate::store::Store::open(root.path()).unwrap();
    let connection = rusqlite::Connection::open(root.path().join("manager.db")).unwrap();
    let db = Db(&connection);
    db.save_conversation_boundary(&serde_json::json!({ "id": "chat", "title": "before" }))
        .unwrap();
    let lists = ConversationLists::default();
    let scope = Scope::new("chats", "", "first-account");
    let frozen = Instant::now();
    let clock = || frozen;
    let (initial, deadline) = lists.get_with_clock(&db, &scope, 1, clock).unwrap();
    assert!(deadline.is_none());
    assert_eq!(initial[0]["title"], "before");
    db.0.execute("UPDATE conversations SET title='after' WHERE id='chat'", [])
        .unwrap();
    let (coalesced, deadline) = lists.get_with_clock(&db, &scope, 2, clock).unwrap();
    assert_eq!(coalesced[0]["title"], "before");
    assert!(deadline.is_some());
    let other = Scope::new("chats", "", "second-account");
    assert_eq!(
        lists.get_with_clock(&db, &other, 2, clock).unwrap().0[0]["title"],
        "after"
    );
    lists
        .cache
        .lock()
        .unwrap()
        .get_mut(&scope.account)
        .unwrap()
        .built_at -= LIST_INTERVAL;
    let (fresh, deadline) = lists.get_with_clock(&db, &scope, 2, clock).unwrap();
    assert_eq!(fresh[0]["title"], "after");
    assert!(deadline.is_none());
    db.0.execute(
        "UPDATE conversations SET title='external' WHERE id='chat'",
        [],
    )
    .unwrap();
    assert_eq!(
        lists.get_with_clock(&db, &scope, 2, clock).unwrap().0[0]["title"],
        "after"
    );
    lists.invalidate(&scope.account);
    assert_eq!(
        lists.get_with_clock(&db, &scope, 2, clock).unwrap().0[0]["title"],
        "external"
    );
}

#[tokio::test]
async fn concurrent_list_subscribers_share_one_rebuild_without_blocking_sqlite_actors() {
    let root = tempfile::tempdir().unwrap();
    let store = crate::store::Store::open(root.path()).unwrap();
    let lists = Arc::new(ConversationLists::default());
    let mut readers = tokio::task::JoinSet::new();
    for _ in 0..8 {
        let store = store.clone();
        let lists = lists.clone();
        readers.spawn(async move {
            let scope = Scope::new("chats", "", "same-account");
            let gate = lists.gate(&scope.account);
            let _permit = gate.lock().await;
            store
                .read(move |db| lists.get(db, &scope, 1))
                .await
                .unwrap();
        });
    }
    while let Some(result) = readers.join_next().await {
        result.unwrap();
    }
    let metrics = lists.metrics.lock().unwrap();
    assert_eq!(metrics.rebuilds, 1);
    assert_eq!(metrics.cache_hits, 7);
}

#[tokio::test]
async fn a_blocked_conversation_read_does_not_block_another_tabs_events() {
    let root = tempfile::tempdir().unwrap();
    let store = crate::store::Store::open(root.path()).unwrap();
    store.transaction(|db| {
        db.save_conversation_boundary(&serde_json::json!({ "id": "slow" }))?;
        db.put_message(&serde_json::json!({ "id": "message", "chatId": "slow", "text": "body", "status": "queued" }))?;
        let state = serde_json::from_value(serde_json::json!({ "id": "fast-run", "taskId": "fast", "status": "running" }))?;
        db.admit_run(&state, None, &serde_json::json!({}), None)?;
        db.save_conversation_boundary(&serde_json::json!({ "id": "fast", "runId": "fast-run" }))?;
        db.event("fast-run", &cairn_contracts::EventKind::Output, "new output", None)?;
        Ok(())
    }).await.unwrap();
    let path = root.path().join("manager.db");
    let connection = rusqlite::Connection::open(&path).unwrap();
    connection
        .execute_batch(
            "ALTER TABLE chat_messages RENAME TO retained_messages;
CREATE VIEW chat_messages AS SELECT rowid AS rowid,id,chat_id,revision,
    body_probe(text) AS text,provider,model,reasoning,speed,mode,status,created_at,question_id
    FROM retained_messages;",
        )
        .unwrap();
    let lists = Arc::new(ConversationLists::default());
    let gate = lists.gate("account");
    let permit = gate.clone().lock_owned().await;
    let (blocked_tx, blocked_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let reader_lists = lists.clone();
    let reader_path = path.clone();
    let slow = tokio::task::spawn_blocking(move || {
        let connection = rusqlite::Connection::open(reader_path).unwrap();
        let blocked_tx = std::sync::Mutex::new(Some(blocked_tx));
        connection
            .create_scalar_function(
                "body_probe",
                1,
                rusqlite::functions::FunctionFlags::SQLITE_UTF8,
                move |ctx| {
                    if let Some(tx) = blocked_tx.lock().unwrap().take() {
                        tx.send(()).unwrap();
                        release_rx.recv().unwrap();
                    }
                    ctx.get::<String>(0)
                },
            )
            .unwrap();
        let scope = Scope {
            with_list: true,
            ..Scope::new("chats", "slow", "account")
        };
        let tx = connection.unchecked_transaction().unwrap();
        read_page(
            &Db(&tx),
            &scope,
            Request {
                after: 0,
                expected: None,
                window: true,
                recent: false,
                resume: true,
            },
            &reader_lists,
            1,
            Some(permit),
        )
        .unwrap();
    });
    tokio::time::timeout(Duration::from_secs(2), blocked_rx)
        .await
        .unwrap()
        .unwrap();
    let next = tokio::time::timeout(Duration::from_secs(1), gate.lock_owned()).await;
    // Always release the fixture, including when the regression reappears.
    if next.is_err() {
        release_tx.send(()).unwrap();
        slow.await.unwrap();
        panic!("the slow conversation held the account's event reads");
    }
    let permit = next.unwrap();
    let fast = tokio::task::spawn_blocking(move || {
        let connection = rusqlite::Connection::open(path).unwrap();
        connection
            .create_scalar_function(
                "body_probe",
                1,
                rusqlite::functions::FunctionFlags::SQLITE_UTF8,
                |ctx| ctx.get::<String>(0),
            )
            .unwrap();
        let scope = Scope {
            with_list: true,
            ..Scope::new("chats", "fast", "account")
        };
        let tx = connection.unchecked_transaction().unwrap();
        read_page(
            &Db(&tx),
            &scope,
            Request {
                after: 0,
                expected: None,
                window: true,
                recent: false,
                resume: true,
            },
            &lists,
            1,
            Some(permit),
        )
        .unwrap()
        .events
    })
    .await;
    release_tx.send(()).unwrap();
    slow.await.unwrap();
    let events = fast.unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].text, "new output");
}

#[tokio::test]
async fn opening_window_includes_an_older_active_background_wait_once() {
    use cairn_contracts::{EventKind, NoticeCode, TimelineContent};
    use serde_json::json;

    let root = tempfile::tempdir().unwrap();
    let store = crate::store::Store::open(root.path()).unwrap();
    store
        .transaction(|db| {
            let run = serde_json::from_value(
                json!({"id":"wait-run", "taskId":"task", "status":"running"}),
            )?;
            db.admit_run(&run, None, &json!({}), None)?;
            db.event(
                "wait-run",
                &EventKind::TurnWaiting,
                "",
                Some(&json!({"tasks":[{"id":"child","description":"Build"}]})),
            )?;
            for _ in 0..120 {
                db.event("wait-run", &EventKind::Diagnostic, "progress", None)?;
            }
            Ok(())
        })
        .await
        .unwrap();
    let cards = store
        .read(|db| {
            let mut range = CardRange {
                run: "wait-run".into(),
                from: 50,
                upto: 121,
                window: true,
                earlier: true,
                active_wait_pending: true,
                position: 0,
                id: String::new(),
            };
            let mut cards = Vec::new();
            loop {
                let (part, more) = range.read(db)?;
                cards.extend(part);
                if !more {
                    break;
                }
            }
            Ok(cards)
        })
        .await
        .unwrap();
    let waits = cards.iter().filter(|card| matches!(&card.content,
        TimelineContent::Notice { notice } if notice.code == NoticeCode::BackgroundTasks && notice.waiting_since.is_some()
    )).collect::<Vec<_>>();
    assert_eq!(waits.len(), 1);
    assert!(waits[0].position < 50);
}

#[test]
fn oversized_wait_reference_omits_source_owned_tasks_and_ids() {
    let card: TimelineEntry = serde_json::from_value(serde_json::json!({
        "id":"notice:1", "runId":"run", "position":1, "revision":1, "source":1,
        "turn":0, "attempt":0, "createdAt":1, "updatedAt":1,
        "content":{"kind":"notice", "notice":{
            "code":"backgroundTasks", "state":"info", "prominent":false,
            "tasks":["x".repeat(300_000)], "taskIds":["y".repeat(300_000)], "waitingSince":1
        }}
    }))
    .unwrap();
    let reference = large_reference(card);
    let value = serde_json::to_value(reference).unwrap();
    let notice = &value["snapshot"]["content"]["notice"];
    assert!(notice.get("tasks").is_none());
    assert!(notice.get("taskIds").is_none());
    assert_eq!(notice["waitingSince"], 1);
    assert!(serde_json::to_vec(&value).unwrap().len() < 1024);
}

#[tokio::test]
async fn oversized_wait_reference_rehydrates_task_ids_from_the_source() {
    use cairn_contracts::{EventKind, TimelineContent};
    use serde_json::json;

    let root = tempfile::tempdir().unwrap();
    let store = crate::store::Store::open(root.path()).unwrap();
    store.transaction(|db| {
        let run = serde_json::from_value(json!({"id":"large-wait", "taskId":"task", "status":"running"}))?;
        db.admit_run(&run, None, &json!({}), None)?;
        db.event("large-wait", &EventKind::TurnWaiting, "", Some(&json!({"tasks":[{"id":"y".repeat(300_000), "description":"x".repeat(300_000)}]})))?;
        Ok(())
    }).await.unwrap();
    store
        .read(|db| {
            let card = crate::timeline::active_wait(db, "large-wait")?.unwrap();
            let reference = large_reference(card);
            let restored =
                crate::timeline::snapshot_from_source(db, "large-wait", reference.snapshot)?;
            match restored.content {
                TimelineContent::Notice { notice } => {
                    assert_eq!(notice.tasks, vec!["x".repeat(300_000)]);
                    assert_eq!(notice.task_ids, vec!["y".repeat(300_000)]);
                    assert!(notice.waiting_since.is_some());
                }
                _ => panic!("expected background wait"),
            }
            Ok(())
        })
        .await
        .unwrap();
}
