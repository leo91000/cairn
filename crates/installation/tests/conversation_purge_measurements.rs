//! Opt-in local C5 experiment; no production cleanup policy or latency gate.
use crate::common::run_fixture::DbRunFixture;
use cairn_contracts::EventKind;

mod common;

use cairn_installation::{config::id, service::Service, store::Store};
use serde_json::{Value, json};
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

fn clock_seconds(clock: libc::clockid_t) -> f64 {
    let mut time = std::mem::MaybeUninit::<libc::timespec>::zeroed();
    assert_eq!(unsafe { libc::clock_gettime(clock, time.as_mut_ptr()) }, 0);
    let time = unsafe { time.assume_init() };
    time.tv_sec as f64 + time.tv_nsec as f64 / 1_000_000_000.0
}

fn ms(start: Instant) -> f64 {
    start.elapsed().as_secs_f64() * 1000.0
}

fn distribution(samples: &[f64]) -> Value {
    assert!(!samples.is_empty());
    let mut sorted = samples.to_vec();
    sorted.sort_by(f64::total_cmp);
    json!({
        "count": sorted.len(),
        "min": sorted[0],
        "p50": sorted[(sorted.len() - 1) / 2],
        "p95": sorted[(sorted.len() * 95).div_ceil(100) - 1],
        "max": sorted[sorted.len() - 1],
        "sum": sorted.iter().sum::<f64>(),
    })
}

/// Fixed synthetic terminal histories, with no account, node, S3 or worktree.
async fn populate(service: &Service, count: usize) -> Value {
    let mut chats = Vec::new();
    for _ in 0..count {
        let chat = id();
        let run = id();
        let artifact = id();
        let data = &service.config.data_dir;
        for directory in [
            data.join("runs").join(&run),
            data.join("chat-attachments").join(&chat),
        ] {
            std::fs::create_dir_all(&directory).unwrap();
            for file in 0..2 {
                std::fs::write(directory.join(format!("synthetic-{file}")), vec![42; 4096])
                    .unwrap();
            }
        }
        std::fs::create_dir_all(data.join("artifacts")).unwrap();
        std::fs::write(data.join("artifacts").join(&artifact), vec![42; 4096]).unwrap();
        chats.push(json!({
            "id": chat,
            "runId": run,
            "artifactId": artifact,
            "lifecycle": "trash",
            "purgeAt": 0,
            "trashedAt": 0,
            "createdAt": 0,
            "lastActivityAt": 0,
            "paused": true,
        }));
    }
    service
        .store
        .transaction(move |db| {
            for mut chat in chats {
                let artifact = chat.as_object_mut().unwrap().remove("artifactId").unwrap();
                let cid = chat["id"].as_str().unwrap();
                let run = chat["runId"].as_str().unwrap();
                db.add_run_fixture(
                    &json!({
                        "id": run,
                        "taskId": cid,
                        "status": "succeeded",
                        "createdAt": 0,
                    }),
                    None,
                )?;
                for event in 0..16 {
                    db.event(
                        run,
                        &EventKind::from_adapter("agent.message"),
                        &format!("{event}:{}", "x".repeat(1024)),
                        None,
                    )?;
                }
                for _ in 0..8 {
                    db.put_message(&json!({
                        "id": id(),
                        "chatId": cid,
                        "createdAt": 0,
                        "status": "delivered",
                        "text": "x".repeat(512),
                    }))?;
                }
                db.set(
                    &format!("artifact:{run}:{}", artifact.as_str().unwrap()),
                    &json!({
                        "id": artifact,
                        "runId": run,
                        "visibility": "private",
                    }),
                    None,
                )?;
                db.save_checkpoint_boundary(&run, &json!({ "completed": true }))?;
                db.put("chats", &chat)?;
            }
            Ok(())
        })
        .await
        .unwrap();
    service.store.read(|db| {
        let chats = db.list("chats")?;
        let bytes = db.0.query_row(
            "SELECT (SELECT coalesce(sum(length(CAST(data AS BLOB))),0) FROM records WHERE kind='chats') +
             (SELECT coalesce(sum(length(CAST(data AS BLOB))),0) FROM runs) +
             (SELECT coalesce(sum(length(CAST(data AS BLOB))),0) FROM chat_messages) +
             (SELECT coalesce(sum(length(CAST(text AS BLOB))),0) FROM events) +
             (SELECT coalesce(sum(length(CAST(data AS BLOB))),0) FROM kv WHERE key LIKE 'artifact:%' )",
            [], |row| row.get::<_, i64>(0),
        )?;
        Ok(json!({
            "chatJsonBytes": serde_json::to_vec(&chats[0])?.len(),
            "encodedPayloadBytes": bytes,
        }))
    }).await.unwrap()
}

/// Same tiny autocommit write before and during purge. Queue delay includes
/// dispatch/scheduling; it is not a direct measurement of writer wall occupancy.
async fn probe(store: &Store, sequence: usize) -> (f64, f64) {
    let queued = Instant::now();
    store
        .write(move |db| {
            let delay = ms(queued);
            let start = Instant::now();
            db.set("c5-synthetic-probe", &json!(sequence), None)?;
            Ok((delay, ms(start)))
        })
        .await
        .unwrap()
}

async fn writer_cpu(store: &Store) -> f64 {
    store
        .write(|_| Ok(clock_seconds(libc::CLOCK_THREAD_CPUTIME_ID)))
        .await
        .unwrap()
}

async fn read_write_reference(service: &Service) -> Value {
    let chat = service.store.list("chats").await.unwrap().remove(0);
    let cid = chat["id"].as_str().unwrap();
    let mut warm = Vec::new();
    let mut cold = Vec::new();
    let mut writes = Vec::new();
    for _ in 0..20 {
        let start = Instant::now();
        service.store.get("chats", cid).await.unwrap().unwrap();
        warm.push(ms(start));
        // A fresh reader connection, without flushing the host's page cache.
        let connection =
            rusqlite::Connection::open(service.config.data_dir.join("manager.db")).unwrap();
        let start = Instant::now();
        let _: String = connection
            .query_row(
                "SELECT data FROM records WHERE id=? AND kind='chats'",
                [cid],
                |row| row.get(0),
            )
            .unwrap();
        cold.push(ms(start));
        let start = Instant::now();
        service.store.put("chats", chat.clone()).await.unwrap();
        writes.push(ms(start));
    }
    json!({
        "warmStoreReadMs": distribution(&warm),
        "freshConnectionSqlReadMs": distribution(&cold),
        "storeWriteMs": distribution(&writes),
        "raw": {
            "warmStoreReadMs": warm,
            "freshConnectionSqlReadMs": cold,
            "storeWriteMs": writes,
        },
    })
}

async fn trial(count: usize, chained: bool, repeat: usize) {
    let root = tempfile::tempdir().unwrap();
    let mut service = Service::new(common::config(root.path())).await.unwrap();
    common::reconfigure(&mut service, |_| {}).await;
    let bytes = populate(&service, count).await;
    let reference = read_write_reference(&service).await;
    let mut baseline_queue = Vec::new();
    let mut baseline_write = Vec::new();
    for sequence in 0..100 {
        let (queue, write) = probe(&service.store, sequence).await;
        baseline_queue.push(queue);
        baseline_write.push(write);
        tokio::time::sleep(Duration::from_millis(1)).await;
    }

    let stop = CancellationToken::new();
    let probe_stop = stop.clone();
    let probe_store = service.store.clone();
    let probes = tokio::spawn(async move {
        let mut queue = Vec::new();
        let mut writes = Vec::new();
        while !probe_stop.is_cancelled() {
            let (delay, write) = probe(&probe_store, queue.len()).await;
            queue.push(delay);
            writes.push(write);
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        (queue, writes)
    });
    // Let the concurrent probe start before the drain.
    tokio::task::yield_now().await;
    let cpu = clock_seconds(libc::CLOCK_PROCESS_CPUTIME_ID);
    let writer = writer_cpu(&service.store).await;
    let start = Instant::now();
    let mut passes = Vec::new();
    let mut bursts = Vec::new();
    let mut purged = 0;
    while purged < count {
        let burst = Instant::now();
        loop {
            let pass = Instant::now();
            service.cleanup_conversations().await.unwrap();
            passes.push(ms(pass));
            purged += 1;
            if purged == count || !chained || burst.elapsed() >= Duration::from_millis(250) {
                break;
            }
        }
        bursts.push(ms(burst));
        // No artificial 15 s waits: the cadence-only estimate is reported below.
        tokio::task::yield_now().await;
    }
    let elapsed = ms(start);
    let writer_cpu_ms = (writer_cpu(&service.store).await - writer) * 1000.0;
    let process_cpu_ms = (clock_seconds(libc::CLOCK_PROCESS_CPUTIME_ID) - cpu) * 1000.0;
    stop.cancel();
    let (queue, writes) = probes.await.unwrap();
    let remaining = service.store.read(|db| {
        Ok(json!({
            "chats": db.list("chats")?.len(),
            "runs": db.0.query_row("SELECT count(*) FROM runs", [], |row| row.get::<_, i64>(0))?,
            "events": db.0.query_row("SELECT count(*) FROM events", [], |row| row.get::<_, i64>(0))?,
            "messages": db.0.query_row("SELECT count(*) FROM chat_messages", [], |row| row.get::<_, i64>(0))?,
            "artifacts": db.keys("artifact:")?.len(),
            "checkpoints": db.0.query_row("SELECT count(*) FROM run_checkpoints", [], |row| row.get::<_, i64>(0))?,
        }))
    }).await.unwrap();
    for (kind, value) in remaining.as_object().unwrap() {
        assert_eq!(value.as_u64(), Some(0), "remaining {kind}");
    }
    for directory in ["runs", "chat-attachments", "artifacts"] {
        assert_eq!(
            std::fs::read_dir(service.config.data_dir.join(directory))
                .unwrap()
                .count(),
            0
        );
    }
    println!(
        "C5_PURGE {}",
        json!({
            "conversations": count,
            "repeat": repeat,
            "mode": if chained { "chain-250ms" } else { "single-pass" },
            "payload": bytes,
            "fileBytes": count * 5 * 4096,
            "readWriteReference": reference,
            "elapsedMs": elapsed,
            "processCpuMs": process_cpu_ms,
            "writerThreadCpuMsIncludingProbes": writer_cpu_ms,
            "passesMs": distribution(&passes),
            "burstsMs": distribution(&bursts),
            "cadenceOnlyFirstToLastStartMs": (bursts.len() - 1) * 15000,
            "baselineProbeQueueMs": distribution(&baseline_queue),
            "baselineProbeWriteMs": distribution(&baseline_write),
            "purgeProbeQueueMs": distribution(&queue),
            "purgeProbeWriteMs": distribution(&writes),
            "raw": {
                "passesMs": passes,
                "burstsMs": bursts,
                "baselineQueueMs": baseline_queue,
                "baselineWriteMs": baseline_write,
                "purgeQueueMs": queue,
                "purgeWriteMs": writes,
            },
            "remaining": remaining,
        })
    );
    service.shutdown.cancel();
}

#[tokio::test]
#[ignore = "C5 local synthetic measurement; run alone with persistent TMPDIR"]
async fn measure_purge_backlog() {
    let (count, chained) = match std::env::var("CAIRN_PURGE_MEASUREMENT").as_deref() {
        Ok("100-single") => (100, false),
        Ok("100-chain") => (100, true),
        Ok("1000-single") => (1000, false),
        Ok("1000-chain") => (1000, true),
        _ => panic!(
            "Set CAIRN_PURGE_MEASUREMENT to 100-single, 100-chain, 1000-single or 1000-chain"
        ),
    };

    // Check the actual tempfile filesystem, not merely the spelling of TMPDIR.
    // 1 GiB measurement headroom above the 10 GiB production reserve; this
    // guard changes no production admission or host configuration.
    let root = tempfile::tempdir().unwrap();
    let path = std::ffi::CString::new(root.path().as_os_str().as_encoded_bytes()).unwrap();
    let mut stats = std::mem::MaybeUninit::<libc::statvfs>::zeroed();
    assert_eq!(
        unsafe { libc::statvfs(path.as_ptr(), stats.as_mut_ptr()) },
        0
    );
    let stats = unsafe { stats.assume_init() };
    let available = stats.f_bavail as u128 * stats.f_frsize as u128;
    assert!(
        available > 11 * 1024_u128.pow(3),
        "use a persistent TMPDIR with >11 GiB available; got {available} bytes"
    );

    trial(count, chained, 0).await;
}
