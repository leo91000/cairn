use crate::common::run_fixture::DbRunFixture;

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
async fn failed_paused_chats_without_wakeups_do_not_need_the_writer() {
    for status in ["failed", "cancelled"] {
        let root = tempfile::TempDir::new().unwrap();
        let service = Service::new(common::config(root.path())).await.unwrap();
        service
            .store
            .transaction(move |db| {
                db.add_run_fixture(
                    &json!({
                        "id": "run",
                        "taskId": "chat",
                        "status": status,
                        "createdAt": 0,
                    }),
                    None,
                )?;
                db.put(
                    "chats",
                    &json!({
                        "id": "chat",
                        "runId": "run",
                        "paused": true,
                        "wakeup": null,
                    }),
                )
            })
            .await
            .unwrap();
        let before = service.store.get("chats", "chat").await.unwrap();
        let mut changes = service.store.subscribe();
        changes.borrow_and_update();

        // Occupy the writer, without holding a SQLite transaction or any reader.
        let (entered, ready) = tokio::sync::oneshot::channel();
        let (release, wait) = std::sync::mpsc::channel();
        let store = service.store.clone();
        let writer = tokio::spawn(async move {
            store
                .write(move |_| {
                    entered.send(()).unwrap();
                    wait.recv_timeout(Duration::from_secs(10)).unwrap();
                    Ok(())
                })
                .await
        });
        ready.await.unwrap();

        let ticks = tokio::time::timeout(Duration::from_secs(1), async {
            for _ in 0..10 {
                service.chat_tick(&HashSet::new()).await.unwrap();
            }
        })
        .await;

        release.send(()).unwrap();
        writer.await.unwrap().unwrap();
        assert!(ticks.is_ok(), "{status}: idle ticks waited for the writer");
        assert_eq!(service.store.get("chats", "chat").await.unwrap(), before);
        assert!(!changes.has_changed().unwrap());
        service.shutdown.cancel();
    }
}

#[tokio::test]
async fn an_idle_tick_does_not_lose_a_concurrently_committed_wakeup() {
    let root = tempfile::TempDir::new().unwrap();
    let service = Service::new(common::config(root.path())).await.unwrap();
    service
        .store
        .transaction(|db| {
            db.add_run_fixture(
                &json!({
                    "id": "run",
                    "taskId": "chat",
                    "status": "failed",
                    "createdAt": 0,
                }),
                None,
            )?;
            db.put(
                "chats",
                &json!({ "id": "chat", "runId": "run", "paused": true }),
            )
        })
        .await
        .unwrap();

    let (entered, ready) = tokio::sync::oneshot::channel();
    let (release, wait) = std::sync::mpsc::channel();
    let store = service.store.clone();
    let writer = tokio::spawn(async move {
        store
            .write(move |db| {
                entered.send(()).unwrap();
                wait.recv_timeout(Duration::from_secs(10)).unwrap();
                let mut chat = db.get("chats", "chat")?.unwrap();
                chat["wakeup"] = json!({ "id": "concurrent-wakeup" });
                chat["title"] = "Concurrent edit".into();
                db.put("chats", &chat)
            })
            .await
    });
    ready.await.unwrap();

    // The pending writer adds the wakeup after the idle tick's read.
    let tick =
        tokio::time::timeout(Duration::from_secs(1), service.chat_tick(&HashSet::new())).await;
    release.send(()).unwrap();
    writer.await.unwrap().unwrap();
    tick.unwrap().unwrap();
    assert!(service.store.get("chats", "chat").await.unwrap().unwrap()["wakeup"].is_object());

    service.chat_tick(&HashSet::new()).await.unwrap();

    let chat = service.store.get("chats", "chat").await.unwrap().unwrap();
    assert!(chat["wakeup"].is_null());
    assert_eq!(chat["title"], "Concurrent edit");
    assert_eq!(chat["paused"], true);
    service.shutdown.cancel();
}

/// Local actor/SQLite measurements; fresh connections do not evict the OS cache.
#[tokio::test]
#[ignore = "explicit F22 local scheduler measurements"]
async fn measure_failed_paused_scheduler() {
    use std::time::Instant;

    fn median(mut samples: Vec<f64>) -> f64 {
        samples.sort_by(f64::total_cmp);
        samples[samples.len() / 2]
    }

    fn cpu_seconds() -> f64 {
        let mut usage = std::mem::MaybeUninit::<libc::rusage>::zeroed();
        assert_eq!(
            unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) },
            0
        );
        let usage = unsafe { usage.assume_init() };
        (usage.ru_utime.tv_sec + usage.ru_stime.tv_sec) as f64
            + (usage.ru_utime.tv_usec + usage.ru_stime.tv_usec) as f64 / 1_000_000.0
    }

    for status in ["failed", "cancelled"] {
        let root = tempfile::TempDir::new().unwrap();
        let service = Service::new(common::config(root.path())).await.unwrap();
        let chat = json!({
            "id": "chat",
            "runId": "run",
            "paused": true,
            "wakeup": null,
        });
        let chat = service.store.put("chats", chat).await.unwrap();
        service
            .store
            .write(move |db| {
                db.add_run_fixture(
                    &json!({
                        "id": "run",
                        "taskId": "chat",
                        "status": status,
                        "createdAt": 0,
                    }),
                    None,
                )
            })
            .await
            .unwrap();

        let before_cpu = cpu_seconds();
        let mut warm = Vec::new();
        let mut cold = Vec::new();
        let mut writes = Vec::new();
        let mut ticks = Vec::new();
        for _ in 0..50 {
            let start = Instant::now();
            service.store.get("chats", "chat").await.unwrap();
            warm.push(start.elapsed().as_secs_f64() * 1000.0);

            let reopened = Store::open(&service.config.data_dir).unwrap();
            let start = Instant::now();
            reopened.get("chats", "chat").await.unwrap();
            cold.push(start.elapsed().as_secs_f64() * 1000.0);
            drop(reopened);

            let start = Instant::now();
            service.store.put("chats", chat.clone()).await.unwrap();
            writes.push(start.elapsed().as_secs_f64() * 1000.0);

            let start = Instant::now();
            service.chat_tick(&HashSet::new()).await.unwrap();
            ticks.push(start.elapsed().as_secs_f64() * 1000.0);
        }
        println!(
            "{}",
            json!({
                "status": status,
                "chatEncodedBytes": serde_json::to_vec(&chat).unwrap().len(),
                "warmReadMedianMs": median(warm),
                "coldConnectionReadMedianMs": median(cold),
                "writeMedianMs": median(writes),
                "tickMedianMs": median(ticks),
                "cpuSeconds": cpu_seconds() - before_cpu,
            })
        );
        assert_eq!(
            service.store.get("chats", "chat").await.unwrap(),
            Some(chat)
        );
        service.shutdown.cancel();
    }
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

#[tokio::test]
async fn rolled_back_state_and_events_never_publish_a_store_revision() {
    let root = tempfile::TempDir::new().unwrap();
    let store = Store::open(root.path()).unwrap();
    store
        .transaction(|db| {
            db.add_run_fixture(
                &json!({"id": "run", "taskId": "task", "status": "running"}),
                None,
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let changes = store.subscribe();
    let result = store
        .transaction(|db| {
            db.change_run(
                "run",
                cairn_installation::runs::RunChanges {
                    session_id: Some(Some("must-roll-back".into())),
                    ..Default::default()
                },
            )?;
            db.event("run", &cairn_contracts::EventKind::TurnStarted, "", None)?;
            Err::<(), _>(cairn_installation::error::Error::conflict(
                "Abort publication",
            ))
        })
        .await;
    assert!(result.is_err());
    assert!(!changes.has_changed().unwrap());
    assert!(store.run_state("run").await.unwrap().session_id.is_none());
    assert!(
        store
            .read(|db| db.event_page("run", 0, 100))
            .await
            .unwrap()
            .is_empty()
    );
    store.transaction(|_| Ok(())).await.unwrap();
    assert!(!changes.has_changed().unwrap());
    store
        .transaction(|db| {
            db.change_run(
                "run",
                cairn_installation::runs::RunChanges {
                    session_id: Some(Some("committed".into())),
                    ..Default::default()
                },
            )?;
            db.event("run", &cairn_contracts::EventKind::TurnStarted, "", None)
        })
        .await
        .unwrap();
    assert!(changes.has_changed().unwrap());
    assert_eq!(
        store.run_state("run").await.unwrap().session_id.as_deref(),
        Some("committed")
    );
    assert_eq!(
        store
            .read(|db| db.event_page("run", 0, 100))
            .await
            .unwrap()
            .len(),
        1
    );
}
