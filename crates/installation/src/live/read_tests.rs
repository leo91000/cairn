use super::*;

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
    let (initial, deadline) = lists.get(&db, &scope, 1).unwrap();
    assert!(deadline.is_none());
    assert_eq!(initial[0]["title"], "before");
    db.0.execute("UPDATE conversations SET title='after' WHERE id='chat'", [])
        .unwrap();
    let (coalesced, deadline) = lists.get(&db, &scope, 2).unwrap();
    assert_eq!(coalesced[0]["title"], "before");
    assert!(deadline.is_some());
    let other = Scope::new("chats", "", "second-account");
    assert_eq!(lists.get(&db, &other, 2).unwrap().0[0]["title"], "after");
    lists
        .cache
        .lock()
        .unwrap()
        .get_mut(&scope.account)
        .unwrap()
        .built_at -= LIST_INTERVAL;
    let (fresh, deadline) = lists.get(&db, &scope, 2).unwrap();
    assert_eq!(fresh[0]["title"], "after");
    assert!(deadline.is_none());
    db.0.execute(
        "UPDATE conversations SET title='external' WHERE id='chat'",
        [],
    )
    .unwrap();
    assert_eq!(lists.get(&db, &scope, 2).unwrap().0[0]["title"], "after");
    lists.invalidate(&scope.account);
    assert_eq!(lists.get(&db, &scope, 2).unwrap().0[0]["title"], "external");
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
    connection.execute_batch("ALTER TABLE chat_messages RENAME TO retained_messages;
CREATE VIEW chat_messages AS SELECT rowid AS rowid,id,chat_id,revision,
    body_probe(text) AS text,provider,model,reasoning,speed,mode,status,created_at,question_id,answers
    FROM retained_messages;").unwrap();
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
