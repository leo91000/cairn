//! Explicit measurements of real Store writes; never a timing gate.
mod common;

use cairn_installation::{runs::RunChanges, store::Store};
use common::run_fixture::DbRunFixture;
use serde_json::json;
use std::time::Instant;

#[tokio::test]
async fn lifecycle_updates_do_not_construct_wire_json() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(root.path()).unwrap();
    let functions = store
        .read(|db| {
            let mut statement =
                db.0.prepare("EXPLAIN UPDATE runs SET summary=? WHERE id=?")?;
            let instructions = statement
                .query_map(rusqlite::params!["new summary", "measured"], |row| {
                    Ok((row.get::<_, String>(1)?, row.get::<_, Option<String>>(5)?))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(instructions
                .into_iter()
                .filter(|(opcode, _)| matches!(opcode.as_str(), "Function" | "PureFunc"))
                .filter_map(|(_, function)| function)
                .filter(|function| function.starts_with("json_set("))
                .collect::<Vec<_>>())
        })
        .await
        .unwrap();

    assert!(
        functions.is_empty(),
        "A scalar lifecycle update must not construct the full or summary wire JSON: {functions:?}"
    );
}

fn cpu_micros() -> u128 {
    let mut time = std::mem::MaybeUninit::<libc::timespec>::uninit();
    // SAFETY: clock_gettime initializes the valid output pointer on success.
    assert_eq!(
        unsafe { libc::clock_gettime(libc::CLOCK_PROCESS_CPUTIME_ID, time.as_mut_ptr()) },
        0
    );
    // SAFETY: the successful call above initialized the complete structure.
    let time = unsafe { time.assume_init() };
    time.tv_sec as u128 * 1_000_000 + time.tv_nsec as u128 / 1_000
}

#[tokio::test]
#[ignore = "explicit run metadata write benchmark"]
async fn metadata_writes_by_frozen_snapshot_size() {
    for kib in [0, 128, 512] {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path()).unwrap();
        store
            .transaction(move |db| {
                db.add_run_fixture(
                    &json!({
                        "id": "measured", "taskId": "measured", "status": "succeeded",
                        "storage": { "mode": "on-demand", "dirtyBytes": 1000 },
                        "snapshot": { "agent": { "instructions": "x".repeat(kib * 1024) } },
                    }),
                    None,
                )?;
                Ok(())
            })
            .await
            .unwrap();
        let (snapshot_bytes, page_size, synchronous): (i64, i64, i64) = store
            .read(|db| {
                Ok((
                    db.0.query_row(
                        "SELECT length(snapshot) FROM runs WHERE id='measured'",
                        [],
                        |r| r.get(0),
                    )?,
                    db.0.pragma_query_value(None, "page_size", |r| r.get(0))?,
                    db.0.pragma_query_value(None, "synchronous", |r| r.get(0))?,
                ))
            })
            .await
            .unwrap();
        for operation in [
            "boundary_changed",
            "boundary_unchanged",
            "state_changed",
            "state_unchanged",
        ] {
            // Prepare a warm, fixed-width record. Setup and checkpointing are outside timing.
            store
                .update_run_boundary(
                    "measured",
                    json!({ "storage": { "mode": "on-demand", "dirtyBytes": 1000 } }),
                )
                .await
                .unwrap();
            store
                .change_run(
                    "measured",
                    RunChanges {
                        summary: Some("summary-1000".into()),
                        ..Default::default()
                    },
                )
                .await
                .unwrap();
            store
                .write(|db| {
                    db.0.pragma_update(None, "wal_autocheckpoint", 0)?;
                    let checkpoint: (i64, i64, i64) =
                        db.0.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| {
                            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
                        })?;
                    assert_eq!(checkpoint, (0, 0, 0));
                    Ok(())
                })
                .await
                .unwrap();
            let revision = store.revision();
            let cpu = cpu_micros();
            let mut elapsed = Vec::new();
            for index in 1..=32 {
                let value = if operation.ends_with("unchanged") {
                    1000
                } else {
                    1000 + index
                };
                let started = Instant::now();
                if operation.starts_with("boundary") {
                    let result = store
                        .update_run_boundary(
                            "measured",
                            json!({ "storage": { "mode": "on-demand", "dirtyBytes": value } }),
                        )
                        .await
                        .unwrap();
                    assert_eq!(result["storage"]["dirtyBytes"], value);
                    assert_eq!(
                        result["snapshot"]["agent"]["instructions"]
                            .as_str()
                            .unwrap()
                            .len(),
                        kib * 1024
                    );
                } else {
                    let result = store
                        .change_run(
                            "measured",
                            RunChanges {
                                summary: Some(format!("summary-{value}")),
                                ..Default::default()
                            },
                        )
                        .await
                        .unwrap();
                    assert_eq!(result.summary, format!("summary-{value}"));
                }
                elapsed.push(started.elapsed().as_micros());
            }
            let cpu = cpu_micros() - cpu;
            let wal_bytes = std::fs::metadata(root.path().join("manager.db-wal"))
                .unwrap()
                .len();
            println!(
                "{}",
                json!({
                    "benchmark": "run_metadata_writes", "operation": operation,
                    "writes": 32, "snapshotKiB": kib, "snapshotBytes": snapshot_bytes,
                    "walBytes": wal_bytes, "cpuMicros": cpu, "elapsedMicros": elapsed,
                    "subscriberRevisions": store.revision() - revision,
                    "pageSize": page_size, "synchronous": synchronous,
                    "walAutocheckpoint": 0, "cache": "warm after seed and setup",
                })
            );
        }
    }
}

#[tokio::test]
#[ignore = "explicit run wire read benchmark"]
async fn wire_reads_by_frozen_snapshot_size() {
    for kib in [0, 128, 512] {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path()).unwrap();
        store
            .transaction(move |db| {
                db.add_run_fixture(
                    &json!({
                        "id": "measured", "taskId": "measured", "status": "succeeded",
                        "snapshot": {
                            "task": { "name": "Measured task" },
                            "agent": { "name": "Measured agent", "provider": "codex", "instructions": "x".repeat(kib * 1024) },
                        },
                    }),
                    None,
                )?;
                Ok(())
            })
            .await
            .unwrap();
        for full in [false, true] {
            let read = |connection: &rusqlite::Connection| {
                cairn_installation::store::Db(connection)
                    .runs(Some("succeeded"), None, 10, 0, full)
                    .unwrap()
            };
            let connection = rusqlite::Connection::open(root.path().join("manager.db")).unwrap();
            let expected = read(&connection);
            assert_eq!(expected.len(), 1);
            if full {
                assert_eq!(
                    expected[0]["snapshot"]["agent"]["instructions"]
                        .as_str()
                        .unwrap()
                        .len(),
                    kib * 1024
                );
            } else {
                assert!(expected[0].get("snapshot").is_none());
                assert_eq!(expected[0]["taskName"], "Measured task");
            }
            let encoded_bytes = serde_json::to_vec(&expected).unwrap().len();
            for cold in [false, true] {
                let cpu = cpu_micros();
                let mut elapsed = Vec::new();
                for _ in 0..32 {
                    let started = Instant::now();
                    let result = if cold {
                        let fresh =
                            rusqlite::Connection::open(root.path().join("manager.db")).unwrap();
                        read(&fresh)
                    } else {
                        read(&connection)
                    };
                    elapsed.push(started.elapsed().as_micros());
                    assert_eq!(result, expected);
                }
                println!(
                    "{}",
                    json!({
                        "benchmark": "run_wire_reads", "snapshotKiB": kib,
                        "full": full, "connectionCold": cold, "reads": 32,
                        "encodedBytes": encoded_bytes, "cpuMicros": cpu_micros() - cpu,
                        "elapsedMicros": elapsed, "osCache": "warm; no host cache eviction",
                        "coldTimingIncludesConnectionOpen": cold,
                    })
                );
            }
        }
    }
}
