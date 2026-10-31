//! Local conversation-list measurement, comparable across commits: it only uses the
//! conversation store and list view, so it also runs unchanged on a reference commit.

use super::*;
use crate::store::Store;
use std::time::Instant;

pub(super) const CHATS: usize = 200;

pub(super) fn cpu_ms() -> f64 {
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::uninit();
    assert_eq!(
        unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) },
        0
    );
    let usage = unsafe { usage.assume_init() };
    (usage.ru_utime.tv_sec + usage.ru_stime.tv_sec) as f64 * 1000.0
        + (usage.ru_utime.tv_usec + usage.ru_stime.tv_usec) as f64 / 1000.0
}

pub(super) fn distribution(mut samples: Vec<f64>) -> Value {
    samples.sort_by(f64::total_cmp);
    let count = samples.len();
    json!({
        "medianMs": samples[count / 2],
        "p95Ms": samples[count * 95 / 100],
        "meanMs": samples.iter().sum::<f64>() / count as f64,
        "samples": count,
    })
}

pub(super) fn database_bytes(directory: &std::path::Path) -> u64 {
    std::fs::read_dir(directory)
        .unwrap()
        .filter_map(|entry| entry.ok()?.metadata().ok())
        .map(|metadata| metadata.len())
        .sum()
}

pub(super) fn chat_id(index: usize) -> String {
    format!("00000000-0000-4000-8000-{index:012}")
}

pub(super) async fn seed_chats(store: &Store) {
    store
        .transaction(|db| {
            for index in 0..CHATS {
                let chat: Conversation = serde_json::from_value(json!({
                    "id": chat_id(index),
                    "slug": format!("chat-{index}"),
                    "title": format!("Conversation {index}"),
                    "agentId": "agent",
                    "createdAt": index,
                    "updatedAt": index,
                }))?;
                db.save_conversation(&chat)?;
            }
            Ok(())
        })
        .await
        .unwrap();
}

/// Lists every conversation from a store holding what `extra` adds.
pub(super) async fn measure_list<F, Fut>(label: &str, extra: F) -> Value
where
    F: FnOnce(Store) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(root.path()).unwrap();
    seed_chats(&store).await;
    extra(store.clone()).await;
    store
        .write(|db| {
            db.0.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
            Ok(())
        })
        .await
        .unwrap();
    let bytes = database_bytes(root.path());
    drop(store);

    // Cold: a newly opened store, its first list. The OS page cache stays warm.
    let mut cold = Vec::new();
    let cpu = cpu_ms();
    for _ in 0..20 {
        let store = Store::open(root.path()).unwrap();
        let started = Instant::now();
        let listed = store.read(|db| list(db)).await.unwrap();
        cold.push(started.elapsed().as_secs_f64() * 1000.0);
        assert_eq!(listed.len(), CHATS);
    }
    let cold_cpu = (cpu_ms() - cpu) / 20.0;

    let store = Store::open(root.path()).unwrap();
    store.read(|db| list(db)).await.unwrap();
    let mut warm = Vec::new();
    let cpu = cpu_ms();
    for _ in 0..200 {
        let started = Instant::now();
        store.read(|db| list(db)).await.unwrap();
        warm.push(started.elapsed().as_secs_f64() * 1000.0);
    }
    let warm_cpu = (cpu_ms() - cpu) / 200.0;
    json!({
        "workload": label,
        "chats": CHATS,
        "databaseBytes": bytes,
        "coldList": distribution(cold),
        "coldListCpuMs": cold_cpu,
        "warmList": distribution(warm),
        "warmListCpuMs": warm_cpu,
    })
}

#[tokio::test]
#[ignore = "explicit local conversation list measurement"]
async fn conversation_list_measurements() {
    let result = measure_list("200 conversations, no other records", |_| async {}).await;
    println!("{}", serde_json::to_string_pretty(&result).unwrap());
}
