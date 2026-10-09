mod common;

use cairn_installation::{config::MAIN_AGENT_ID, service::Service, store::Store};
use serde_json::json;
use std::{collections::HashSet, time::Duration};

#[tokio::test]
async fn an_idle_scheduler_pass_does_not_wake_live_subscribers() {
    let root = tempfile::TempDir::new().unwrap();
    let mut service = Service::new(common::config(root.path())).await.unwrap();
    common::reconfigure(&mut service, |_| {}).await;
    service
        .chat_create(json!({ "agentId": MAIN_AGENT_ID }))
        .await
        .unwrap();
    let changes = service.store.subscribe();

    service.chat_tick(&HashSet::new()).await.unwrap();

    assert!(
        !changes.has_changed().unwrap(),
        "An idle chat pass must not trigger another full read for every live UI subscriber"
    );
    service.shutdown.cancel();
}

#[tokio::test]
async fn a_zero_row_delete_after_a_real_write_does_not_notify_again() {
    let root = tempfile::TempDir::new().unwrap();
    let store = Store::open(root.path()).unwrap();
    let mut changes = store.subscribe();
    store.set("existing", json!(true), None).await.unwrap();
    assert!(changes.has_changed().unwrap());
    changes.borrow_and_update();

    store.delete("absent").await.unwrap();

    assert!(!changes.has_changed().unwrap());
    assert_eq!(store.kv("existing").await.unwrap(), Some(json!(true)));
}

#[tokio::test]
async fn cancelling_a_writer_caller_still_notifies_after_its_commit() {
    let root = tempfile::TempDir::new().unwrap();
    let store = Store::open(root.path()).unwrap();
    let mut changes = store.subscribe();
    let (entered, ready) = tokio::sync::oneshot::channel();
    let (release, wait) = std::sync::mpsc::channel();
    let writer = store.clone();
    let caller = tokio::spawn(async move {
        writer
            .transaction(move |db| {
                db.set("committed", &json!(true), None)?;
                entered.send(()).unwrap();
                wait.recv().unwrap();
                Ok(())
            })
            .await
    });
    ready.await.unwrap();
    assert!(
        !changes.has_changed().unwrap(),
        "The transaction is not committed yet"
    );
    caller.abort();
    assert!(caller.await.unwrap_err().is_cancelled());
    release.send(()).unwrap();

    tokio::time::timeout(Duration::from_secs(1), changes.changed())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(store.kv("committed").await.unwrap(), Some(json!(true)));
}
