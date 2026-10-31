//! C1 qualification reuses the A1/A2 clocks, connections and distributions and the
//! frozen A6 event mapping as reference: the base writer without a projection and the
//! base UI reads that carried every reply snapshot. The candidate writes each event
//! with its timeline card and reads windows of UI events plus their cards.
use super::{connection, cpu, distribution, measured};
use cairn_contracts::{EventKind, RunState};
use cairn_installation::store::{Db, Store};
use rusqlite::Connection;
use serde_json::{Value, json};
#[cfg(target_os = "linux")]
use std::os::fd::AsRawFd;

use super::event_measurement::frozen;

const RUN: &str = "00000000-0000-4000-8000-000000000001";
const BYTES: usize = 256 * 1024;

/// A deterministic conversation of fixed-size frames: messages, the run cycle,
/// streamed replies, tools, errors and retries in every 20-event turn.
fn frame(n: usize) -> (&'static str, String, Value) {
    let turn = n / 20;
    match n % 20 {
        0 => (
            "chat.user",
            "Inspect this repository".into(),
            json!({"messageId": format!("user-{n}"), "text": "Inspect this repository", "attachments": []}),
        ),
        1 => (
            "turn.started",
            String::new(),
            json!({"type": "turn.started"}),
        ),
        2..=10 => (
            "item.updated",
            String::new(),
            json!({"item": {"id": "answer", "type": "agent_message", "text": "Answer 🦊 [redacted]. ".repeat(n % 20 * 3)}}),
        ),
        11 => (
            "item.completed",
            String::new(),
            json!({"item": {"id": format!("err-{turn}"), "type": "error", "message": "Reconnecting... 1/5 (stream disconnected: 503)"}}),
        ),
        12..=16 => (
            "item.completed",
            String::new(),
            json!({"item": {"id": format!("tool-{n}"), "type": "command_execution", "command": "rg --files", "exit_code": 0, "aggregated_output": "crates/source.rs\n".repeat(20)}}),
        ),
        17 => (
            "item.completed",
            String::new(),
            json!({"item": {"id": "answer", "type": "agent_message", "text": "Answer 🦊 [redacted]. ".repeat(30)}}),
        ),
        18 if turn % 3 == 2 => (
            "turn.failed",
            "Rate limit exceeded".into(),
            json!({"error": {"message": "Rate limit exceeded"}}),
        ),
        18 => (
            "turn.completed",
            String::new(),
            json!({"usage": {"input_tokens": 1200, "output_tokens": 340}}),
        ),
        _ if turn % 3 == 2 => (
            "retry.scheduled",
            "Temporary agent error.".into(),
            json!({"retry": {"attempt": 1, "limit": 3}}),
        ),
        _ => ("status", "succeeded".into(), json!({})),
    }
}

fn write(db: &Connection, reference: bool, n: usize) {
    let (kind, text, payload) = frame(n);
    if reference {
        frozen::Reference(db)
            .event(RUN, kind, &text, Some(&payload))
            .unwrap();
    } else {
        Db(db)
            .event(RUN, &EventKind::from_adapter(kind), &text, Some(&payload))
            .unwrap();
    }
}

/// The candidate's wire shape, serialized directly like the live batch and history page.
#[derive(serde::Serialize)]
struct Wire {
    events: Vec<cairn_installation::store::Event>,
    entries: Vec<cairn_contracts::TimelineEntry>,
}

/// What a client receives: the base UI's events, or the candidate's events and cards.
fn read(db: &Connection, reference: bool, after: i64, window: bool) -> Vec<u8> {
    if reference {
        let store = frozen::Reference(db);
        let events = if window {
            store.events_before(RUN, after).unwrap().0
        } else {
            store.event_batch(RUN, after, 100, BYTES).unwrap()
        };
        return serde_json::to_vec(&events).unwrap();
    }
    let store = Db(db);
    let (events, cursor) = if window {
        // The opening window the live stream sends, like the reference's 100-event page.
        let (events, more) = store.live_events_between(RUN, 0, after).unwrap();
        let oldest = if more {
            events.first().map_or(0, |e| e.id)
        } else {
            0
        };
        let cursor = events.last().map_or(0, |event| event.id);
        let cards = read_cards(&store, oldest, cursor, true);
        return serde_json::to_vec(&Wire {
            events: cairn_installation::live::ui_events(events),
            entries: cards,
        })
        .unwrap();
    } else {
        let events = store.event_batch(RUN, after, 100, BYTES).unwrap();
        let cursor = events.last().map_or(after, |e| e.id);
        (events, cursor)
    };
    let cards = read_cards(&store, after, cursor, false);
    serde_json::to_vec(&Wire {
        events: cairn_installation::live::ui_events(events),
        entries: cards,
    })
    .unwrap()
}

fn read_cards(
    db: &Db<'_>,
    after: i64,
    upto: i64,
    window: bool,
) -> Vec<cairn_contracts::TimelineEntry> {
    let mut cards = Vec::new();
    let mut position = 0;
    let mut id = String::new();
    let mut earlier = window;
    loop {
        let (part, more) = if window {
            cairn_installation::timeline::window_chunk(db, RUN, after, upto, position, &id, earlier)
        } else {
            cairn_installation::timeline::changed_chunk(db, RUN, after, upto, position, &id)
        }
        .unwrap();
        if let Some(last) = part.last() {
            position = if window && !earlier {
                last.position
            } else {
                last.revision
            };
            id.clone_from(&last.id);
        }
        cards.extend(part);
        if !more && earlier {
            earlier = false;
            position = 0;
            id.clear();
            continue;
        }
        if !more {
            break;
        }
    }
    cards
}

#[tokio::test]
#[ignore = "local C1 measurement; run alone with --ignored --nocapture --test-threads=1"]
async fn timeline_storage_layouts() {
    for count in [100, 10_000, 100_000] {
        for repetition in 0..5 {
            for reference in if repetition % 2 == 0 {
                [true, false]
            } else {
                [false, true]
            } {
                let root = tempfile::tempdir().unwrap();
                drop(Store::open(root.path()).unwrap());
                let path = root.path().join("manager.db");
                let db = connection(&path);
                if reference {
                    db.execute_batch("DROP TABLE timeline_entries; DROP TABLE timeline_state;")
                        .unwrap();
                }
                let state: RunState = serde_json::from_value(
                    json!({"id":RUN,"taskId":"task","status":"running","trigger":"chat"}),
                )
                .unwrap();
                Db(&db).admit_run(&state, None, &json!({}), None).unwrap();
                let tx = db.unchecked_transaction().unwrap();
                let start_cpu = cpu();
                let started = std::time::Instant::now();
                for n in 0..count {
                    write(&tx, reference, n);
                }
                tx.commit().unwrap();
                let bulk_us = started.elapsed().as_micros();
                let bulk_cpu = cpu() - start_cpu;
                let opening_wire_bytes = read(&db, reference, i64::MAX, true).len();
                for _ in 0..2 {
                    for n in 0..50 {
                        std::hint::black_box(read(&db, reference, (n * count / 50) as i64, false));
                    }
                }
                let mut batches = Vec::new();
                let mut batch_bytes = 0;
                let start_cpu = cpu();
                for n in 0..100 {
                    batch_bytes += measured(&mut batches, || {
                        read(&db, reference, (n * count / 100) as i64, false)
                    })
                    .len();
                }
                let batch_cpu = cpu() - start_cpu;
                let mut opening = Vec::new();
                let start_cpu = cpu();
                for _ in 0..100 {
                    std::hint::black_box(measured(&mut opening, || {
                        read(&db, reference, i64::MAX, true)
                    }));
                }
                let opening_cpu = cpu() - start_cpu;
                let mut cold = Vec::new();
                let start_cpu = cpu();
                for _ in 0..50 {
                    let fresh = connection(&path);
                    std::hint::black_box(measured(&mut cold, || {
                        read(&fresh, reference, i64::MAX, true)
                    }));
                }
                let cold_cpu = cpu() - start_cpu;
                // Evict only this benchmark's database, never the host-wide page cache.
                // Checkpoint first so the measured read does not depend on a cached WAL.
                #[cfg(target_os = "linux")]
                let (os_cold, os_cold_cpu) = {
                    db.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)").unwrap();
                    let file = std::fs::File::open(&path).unwrap();
                    file.sync_all().unwrap();
                    let mut samples = Vec::new();
                    let start_cpu = cpu();
                    for _ in 0..10 {
                        // SAFETY: the descriptor remains open, and the advice covers the file.
                        let result = unsafe {
                            libc::posix_fadvise(file.as_raw_fd(), 0, 0, libc::POSIX_FADV_DONTNEED)
                        };
                        assert_eq!(result, 0, "could not evict benchmark database pages");
                        let fresh = connection(&path);
                        std::hint::black_box(measured(&mut samples, || {
                            read(&fresh, reference, i64::MAX, true)
                        }));
                    }
                    (Some(distribution(&samples)), Some(cpu() - start_cpu))
                };
                #[cfg(not(target_os = "linux"))]
                let (os_cold, os_cold_cpu): (
                    Option<serde_json::Value>,
                    Option<i64>,
                ) = (None, None);
                let mut writes = Vec::new();
                let start_cpu = cpu();
                for n in count..count + 200 {
                    measured(&mut writes, || {
                        let tx = db.unchecked_transaction().unwrap();
                        write(&tx, reference, n);
                        tx.commit().unwrap();
                    });
                }
                let write_cpu = cpu() - start_cpu;
                let wal =
                    std::fs::metadata(format!("{}-wal", path.display())).map_or(0, |m| m.len());
                db.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)").unwrap();
                let payload: i64 = db.query_row("SELECT sum(length(CAST(text AS BLOB))+coalesce(length(CAST(payload AS BLOB)),0)) FROM events", [], |r| r.get(0)).unwrap();
                let pages: Vec<(String, i64)> = db
                    .prepare("SELECT name,sum(pgsize) FROM dbstat WHERE name LIKE 'events%' OR name LIKE 'timeline%' OR name LIKE 'sqlite_autoindex_timeline%' GROUP BY name ORDER BY name")
                    .unwrap()
                    .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
                    .unwrap()
                    .map(Result::unwrap)
                    .collect();
                println!(
                    "TIMELINE_STORAGE {}",
                    json!({
                        "reference": reference,
                        "rows": count,
                        "repetition": repetition,
                        "dbBytes": std::fs::metadata(&path).unwrap().len(),
                        "walBytes": wal,
                        "encodedPayloadBytes": payload,
                        "pages": pages,
                        "openingWireBytes": opening_wire_bytes,
                        "batchWireBytes": batch_bytes,
                        "bulkWriteUs": bulk_us,
                        "batch": distribution(&batches),
                        "opening": distribution(&opening),
                        "connectionColdOpening": distribution(&cold),
                        "osColdOpening": os_cold,
                        "write": distribution(&writes),
                        "cpuUs": {
                            "bulkWrite": bulk_cpu,
                            "batch100": batch_cpu,
                            "opening100": opening_cpu,
                            "cold50IncludingOpen": cold_cpu,
                            "osCold10IncludingOpen": os_cold_cpu,
                            "write200": write_cpu,
                        },
                    })
                );
            }
        }
    }
}
