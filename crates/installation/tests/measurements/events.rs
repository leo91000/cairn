//! A6 qualification reuses A1/A2 clocks, connections and distributions.
use super::{connection, cpu, distribution, measured};
use cairn_contracts::{EventKind, RunState};
use cairn_installation::store::{Db, Store};
use rusqlite::Connection;
use serde_json::{Value, json};

#[path = "events_reference.rs"]
pub(super) mod frozen;

const RUN: &str = "00000000-0000-4000-8000-000000000001";
const BYTES: usize = 256 * 1024;

fn frame(n: usize) -> (&'static str, Value) {
    match n % 20 {
        0 => ("turn.started", json!({"type": "turn.started"})),
        1 => (
            "chat.user",
            json!({"messageId": format!("user-{n}"), "text": "Inspect this repository"}),
        ),
        19 => (
            "item.completed",
            json!({"item": {"id": "answer", "type": "agent_message", "text": "Answer 🦊 [redacted]. ".repeat(25)}}),
        ),
        2..=12 => (
            "item.updated",
            json!({"item": {"id": "answer", "type": "agent_message", "text": "Answer 🦊 [redacted]. ".repeat(n % 20 + 1)}}),
        ),
        _ => (
            "item.completed",
            json!({"item": {"id": format!("tool-{n}"), "type": "command_execution", "command": "rg --files", "aggregated_output": "crates/source.rs\n".repeat(20)}}),
        ),
    }
}

fn write(db: &Connection, reference: bool, n: usize) {
    let (kind, payload) = frame(n);
    if reference {
        frozen::Reference(db)
            .event(RUN, kind, "", Some(&payload))
            .unwrap();
    } else {
        Db(db)
            .event(RUN, &EventKind::from_adapter(kind), "", Some(&payload))
            .unwrap();
    }
    if n.is_multiple_of(20) {
        let checkpoint = json!({"launched": false, "remainingMs": null});
        if reference {
            db.execute(
                "INSERT INTO kv VALUES(?,?,NULL) ON CONFLICT(key) DO UPDATE SET data=excluded.data",
                rusqlite::params![
                    format!("run-checkpoint:{RUN}"),
                    serde_json::to_string(&checkpoint).unwrap()
                ],
            )
            .unwrap();
        } else {
            Db(db).save_checkpoint_boundary(RUN, &checkpoint).unwrap();
        }
    }
}

fn read(db: &Connection, reference: bool, after: i64, window: bool) -> Vec<u8> {
    if reference {
        let store = frozen::Reference(db);
        let events = if window {
            store.events_before(RUN, after).unwrap().0
        } else {
            store.event_batch(RUN, after, 100, BYTES).unwrap()
        };
        serde_json::to_vec(&events).unwrap()
    } else {
        let store = Db(db);
        let events = if window {
            store.events_before(RUN, after).unwrap().0
        } else {
            store.event_batch(RUN, after, 100, BYTES).unwrap()
        };
        serde_json::to_vec(&events).unwrap()
    }
}

#[tokio::test]
#[ignore = "local A6 measurement; run alone with --ignored --nocapture --test-threads=1"]
async fn event_storage_layouts() {
    for count in [100, 10_000, 100_000] {
        for repetition in 0..5 {
            let mut content: Option<Value> = None;
            for reference in if repetition % 2 == 0 {
                [true, false]
            } else {
                [false, true]
            } {
                let root = tempfile::tempdir().unwrap();
                let store = Store::open(root.path()).unwrap();
                drop(store);
                let path = root.path().join("cairn.sqlite");
                // Store's actual filename is deliberately discovered once rather
                // than creating a parallel database or a separate Cargo build.
                let path = if path.exists() {
                    path
                } else {
                    std::fs::read_dir(root.path())
                        .unwrap()
                        .map(|p| p.unwrap().path())
                        .find(|p| {
                            p.extension()
                                .is_some_and(|ext| ext == "sqlite" || ext == "db")
                        })
                        .unwrap()
                };
                let db = connection(&path);
                if reference {
                    db.execute_batch("DROP TABLE events; CREATE TABLE events(id INTEGER PRIMARY KEY AUTOINCREMENT,run_id TEXT NOT NULL REFERENCES runs(id) ON DELETE CASCADE,created_at INTEGER NOT NULL,type TEXT NOT NULL,text TEXT NOT NULL,payload TEXT);
CREATE INDEX events_run ON events(run_id,id);
CREATE INDEX events_turns ON events(run_id,id) WHERE type='turn.started';
CREATE INDEX events_user_input ON events(run_id,id) WHERE type='chat.user';
CREATE INDEX events_messages ON events(run_id,json_extract(payload,'$.item.id'),id) WHERE json_extract(payload,'$.item.type')='agent_message';
DROP TABLE run_checkpoints; DROP TABLE kv; CREATE TABLE kv(key TEXT PRIMARY KEY,data TEXT NOT NULL,expires INTEGER);").unwrap();
                }
                let state: RunState = serde_json::from_value(
                    json!({"id":RUN,"taskId":"task","status":"running","trigger":"chat"}),
                )
                .unwrap();
                Db(&db).admit_run(&state, None, &json!({}), None).unwrap();
                let tx = db.unchecked_transaction().unwrap();
                for n in 0..count {
                    write(&tx, reference, n);
                }
                tx.commit().unwrap();
                let mut encoded = read(&db, reference, i64::MAX, true);
                let opening_wire_bytes = encoded.len();
                let mut value: Value = serde_json::from_slice(&encoded).unwrap();
                // Both mappings write real timestamps; compare the deterministic
                // corpus separately from wall-clock append time.
                for event in value.as_array_mut().unwrap() {
                    event.as_object_mut().unwrap().remove("createdAt");
                }
                if let Some(previous) = &content {
                    let shared = previous
                        .as_array()
                        .unwrap()
                        .len()
                        .min(value.as_array().unwrap().len());
                    let old = previous.as_array().unwrap();
                    let new = value.as_array().unwrap();
                    assert_eq!(&old[old.len() - shared..], &new[new.len() - shared..]);
                } else {
                    content = Some(value);
                }
                for _ in 0..2 {
                    for n in 0..50 {
                        std::hint::black_box(read(&db, reference, (n * count / 50) as i64, false));
                    }
                }
                let mut cursor = Vec::new();
                let start_cpu = cpu();
                for n in 0..100 {
                    encoded = measured(&mut cursor, || {
                        read(&db, reference, (n * count / 100) as i64, false)
                    });
                    std::hint::black_box(&encoded);
                }
                let cursor_cpu = cpu() - start_cpu;
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
                for n in 0..50 {
                    let fresh = connection(&path);
                    std::hint::black_box(measured(&mut cold, || {
                        read(&fresh, reference, (n * count / 50) as i64, false)
                    }));
                }
                let cold_cpu = cpu() - start_cpu;
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
                    .prepare("SELECT name,sum(pgsize) FROM dbstat GROUP BY name ORDER BY name")
                    .unwrap()
                    .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
                    .unwrap()
                    .map(Result::unwrap)
                    .collect();
                println!(
                    "EVENT_STORAGE {}",
                    json!({"reference":reference,"rows":count,"repetition":repetition,"dbBytes":std::fs::metadata(&path).unwrap().len(),"walBytes":wal,"encodedPayloadBytes":payload,"openingWireBytes":opening_wire_bytes,"pages":pages,"cursor":distribution(&cursor),"opening":distribution(&opening),"connectionCold":distribution(&cold),"write":distribution(&writes),"cpuUs":{"cursor":cursor_cpu,"opening":opening_cpu,"coldIncludingOpen":cold_cpu,"write":write_cpu}})
                );
            }
        }
    }
}

/// Run this same harness on the current base and candidate (no frozen reader).
/// Cold means a fresh SQLite connection; the host's filesystem cache stays warm.
#[tokio::test]
#[ignore = "local pagination measurement; run alone with --ignored --nocapture --test-threads=1"]
async fn compressed_history_budget_measurement() {
    use rand::{RngCore, SeedableRng};
    use std::io::Write;

    for workload in ["repetitive", "mixed", "noise"] {
        let mut rng = rand::rngs::StdRng::seed_from_u64(391);
        let corpus: Vec<_> = (0..1500).map(|n| {
            let output = if workload == "noise" || workload == "mixed" && n % 10 == 0 {
                let mut bytes = [0; 2048];
                rng.fill_bytes(&mut bytes);
                bytes.iter().map(|byte| char::from(b'!' + byte % 94)).collect()
            } else {
                "src/module/example.rs\n".repeat(80)
            };
            json!({"item":{"id":format!("tool-{n}"),"type":"command_execution","command":"rg --files","aggregated_output":output}})
        }).collect();
        {
            let root = tempfile::tempdir().unwrap();
            let store = Store::open(root.path()).unwrap();
            drop(store);
            let path = root.path().join("manager.db");
            let db = connection(&path);
            let run: RunState = serde_json::from_value(
                json!({"id":RUN,"taskId":"task","status":"running","trigger":"chat"}),
            )
            .unwrap();
            Db(&db).admit_run(&run, None, &json!({}), None).unwrap();
            let tx = db.unchecked_transaction().unwrap();
            for payload in &corpus {
                Db(&tx)
                    .event(RUN, &EventKind::ItemCompleted, "tool", Some(payload))
                    .unwrap();
            }
            tx.commit().unwrap();
            let page = |db: &Connection, before| {
                let (events, more) = Db(db).events_before(RUN, before).unwrap();
                let oldest = events.first().map_or(before, |e| e.id);
                (
                    serde_json::to_vec(&events).unwrap(),
                    oldest,
                    more,
                    events.len(),
                )
            };
            let encode = |bytes: &[u8]| {
                let mut gzip =
                    flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
                gzip.write_all(bytes).unwrap();
                gzip.finish().unwrap().len()
            };
            let (bytes, _, _, events) = page(&db, i64::MAX);
            let compressed_bytes = encode(&bytes);
            let mut warm = Vec::new();
            let start_cpu = cpu();
            for _ in 0..12 {
                std::hint::black_box(measured(&mut warm, || page(&db, i64::MAX)));
            }
            let warm_cpu = cpu() - start_cpu;
            let mut cold = Vec::new();
            let start_cpu = cpu();
            for _ in 0..12 {
                let fresh = connection(&path);
                std::hint::black_box(measured(&mut cold, || page(&fresh, i64::MAX)));
            }
            let cold_cpu = cpu() - start_cpu;
            let mut before = i64::MAX;
            let mut pages = 0;
            let mut raw_total = 0;
            let mut wire_total = 0;
            let mut received = 0;
            let start_cpu = cpu();
            let started = std::time::Instant::now();
            loop {
                let (bytes, oldest, more, count) = page(&db, before);
                assert!(oldest < before);
                before = oldest;
                pages += 1;
                raw_total += bytes.len();
                wire_total += encode(&bytes);
                received += count;
                if !more {
                    break;
                }
            }
            let history_us = started.elapsed().as_micros();
            let history_cpu = cpu() - start_cpu;
            assert_eq!(received, corpus.len());
            let mut writes = Vec::new();
            let start_cpu = cpu();
            for payload in corpus.iter().take(100) {
                measured(&mut writes, || {
                    let tx = db.unchecked_transaction().unwrap();
                    Db(&tx)
                        .event(RUN, &EventKind::ItemCompleted, "tool", Some(payload))
                        .unwrap();
                    tx.commit().unwrap();
                });
            }
            let write_cpu = cpu() - start_cpu;
            println!(
                "HISTORY_BUDGET {}",
                json!({
                    "label":std::env::var("CAIRN_HISTORY_MEASUREMENT_LABEL").unwrap_or_else(|_|"candidate".into()),
                    "workload":workload,"rows":corpus.len(),"openingEvents":events,
                    "openingRawBytes":bytes.len(),"openingGzipBytes":compressed_bytes,
                    "pages":pages,"historyRawBytes":raw_total,"historyGzipBytes":wire_total,
                    "historyUs":history_us,"historyCpuUs":history_cpu,
                    "warm":distribution(&warm),"connectionCold":distribution(&cold),"write":distribution(&writes),
                    "cpuUs":{"warm12":warm_cpu,"cold12IncludingOpen":cold_cpu,"write100":write_cpu},
                    "coldDefinition":"fresh SQLite connection; warm OS cache",
                    "writePath":"unchanged current event writer, one committed event per transaction",
                    "debugAssertions":cfg!(debug_assertions)
                })
            );
        }
    }
}
