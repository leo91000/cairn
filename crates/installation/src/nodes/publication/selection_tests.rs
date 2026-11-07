use super::*;
use crate::store::{Db, Store};
use rusqlite::Connection;

const REFERENCE: &str =
    include_str!("../../../tests/fixtures/publication-candidates-reference.sql");

fn wire_rows(connection: &Connection, query: &str) -> Vec<String> {
    let mut rows = connection
        .prepare_cached(query)
        .unwrap()
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    rows.sort();
    rows
}

fn decoded_rows(connection: &Connection, query: &str) -> Vec<Value> {
    let mut rows = Db(connection).json_rows(query, []).unwrap();
    rows.sort_by(|left, right| text(left, "id").cmp(text(right, "id")));
    rows
}

#[test]
fn eligibility_scan_does_not_construct_full_run_json() {
    let root = tempfile::tempdir().unwrap();
    drop(Store::open(root.path()).unwrap());
    let connection = Connection::open(root.path().join("manager.db")).unwrap();
    // Prune the returned wire value to isolate the actual candidate predicates.
    // A rejected row must not reconstruct the frozen snapshot just to filter it.
    let query = format!("EXPLAIN SELECT 1 FROM ({SYNC_CANDIDATES})");
    let mut statement = connection.prepare(&query).unwrap();
    let functions = statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(1)?, row.get::<_, Option<String>>(5)?))
        })
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap()
        .into_iter()
        .filter(|(opcode, _)| matches!(opcode.as_str(), "Function" | "PureFunc"))
        .filter_map(|(_, function)| function)
        .filter(|function| function.starts_with("json_set("))
        .collect::<Vec<_>>();
    assert!(
        functions.is_empty(),
        "Candidate eligibility must not reconstruct full wire JSON: {functions:?}"
    );
}

#[test]
fn candidate_selection_preserves_every_status_and_wire_bytes() {
    let root = tempfile::tempdir().unwrap();
    drop(Store::open(root.path()).unwrap());
    let mut connection = Connection::open(root.path().join("manager.db")).unwrap();
    let transaction = connection.transaction().unwrap();
    let db = Db(&transaction);
    let backups = [
        Value::Null,
        json!({ "requestedRevision": 2, "acknowledgedRevision": 1 }),
        json!({ "requestedRevision": 1, "acknowledgedRevision": 1 }),
        json!({ "status": "pending" }),
        json!({ "status": "saving" }),
        json!({ "status": "error" }),
        json!({ "status": "synced" }),
        json!({ "id": "legacy", "blockEnvelope": 2 }),
        json!({ "id": "current", "blockEnvelope": 3 }),
        json!({ "id": "text-envelope", "blockEnvelope": "3" }),
        json!({ "id": false }),
        json!({ "id": 0 }),
        json!({ "id": { "opaque": true } }),
        json!({ "id": null }),
    ];
    let mut count = 0;
    for status in [
        "queued",
        "running",
        "waiting",
        "succeeded",
        "failed",
        "cancelled",
        "interrupted",
    ] {
        for mode in [Value::Null, json!("on-demand"), json!("native")] {
            for dirty in [0, 4096] {
                for backup in &backups {
                    let id = format!("candidate-{count}");
                    db.add_run_fixture(
                        &json!({
                            "id": id,
                            "taskId": id,
                            "status": status,
                            "createdAt": count,
                            "storage": { "mode": mode, "dirtyBytes": dirty },
                            "backup": backup,
                            "snapshot": { "agent": { "instructions": "frozen task instructions" } },
                        }),
                        None,
                    )
                    .unwrap();
                    count += 1;
                }
            }
        }
    }
    transaction.commit().unwrap();
    let reference = wire_rows(&connection, REFERENCE);
    assert_eq!(count, 588);
    assert!(!reference.is_empty() && reference.len() < count);
    assert_eq!(wire_rows(&connection, SYNC_CANDIDATES), reference);
}

#[test]
#[ignore = "explicit publication candidate scan measurement; run alone"]
fn candidate_scan_measurement() {
    let root = tempfile::tempdir().unwrap();
    drop(Store::open(root.path()).unwrap());
    let database = root.path().join("manager.db");
    let mut connection = Connection::open(&database).unwrap();
    let seed_started = std::time::Instant::now();
    let seed_cpu = cpu_micros();
    let transaction = connection.transaction().unwrap();
    let db = Db(&transaction);
    for index in 0..64 {
        let id = format!("measured-{index}");
        db.add_run_fixture(
            &json!({
                "id": id,
                "taskId": id,
                "status": if index % 4 == 0 { "running" } else { "succeeded" },
                "createdAt": index,
                "storage": {
                    "mode": if index % 4 == 3 { "native" } else { "on-demand" },
                    "dirtyBytes": if index % 4 == 1 { 4096 } else { 0 },
                },
                "backup": { "requestedRevision": 1, "acknowledgedRevision": 1, "blockEnvelope": 3 },
                "snapshot": { "agent": { "instructions": "x".repeat(128 * 1024) } },
            }),
            None,
        )
        .unwrap();
    }
    transaction.commit().unwrap();
    let fixture_write_micros = seed_started.elapsed().as_micros();
    let fixture_write_cpu_micros = cpu_micros() - seed_cpu;
    connection
        .execute_batch(
            "PRAGMA wal_checkpoint(TRUNCATE); PRAGMA query_only=ON; PRAGMA foreign_keys=ON;",
        )
        .unwrap();
    let database_bytes = std::fs::metadata(&database).unwrap().len();
    let wal = database.with_extension("db-wal");
    let wal_bytes = || match std::fs::metadata(&wal) {
        Ok(metadata) => metadata.len(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0,
        Err(error) => panic!("Cannot observe benchmark WAL: {error}"),
    };
    let initial_wal_bytes = wal_bytes();
    let reference = wire_rows(&connection, REFERENCE);
    assert_eq!(reference.len(), 32);
    assert_eq!(wire_rows(&connection, SYNC_CANDIDATES), reference);
    let encoded_bytes: usize = reference.iter().map(String::len).sum();
    let decoded = decoded_rows(&connection, REFERENCE);
    assert_eq!(decoded_rows(&connection, SYNC_CANDIDATES), decoded);
    for connection_cold in [false, true] {
        for pair in 0..4 {
            let order = if pair % 2 == 0 {
                [("reference", REFERENCE), ("candidate", SYNC_CANDIDATES)]
            } else {
                [("candidate", SYNC_CANDIDATES), ("reference", REFERENCE)]
            };
            for (variant, query) in order {
                let cpu = cpu_micros();
                let started = std::time::Instant::now();
                for _ in 0..8 {
                    let rows = if connection_cold {
                        let fresh = Connection::open(&database).unwrap();
                        fresh
                            .execute_batch("PRAGMA query_only=ON; PRAGMA foreign_keys=ON;")
                            .unwrap();
                        decoded_rows(&fresh, query)
                    } else {
                        decoded_rows(&connection, query)
                    };
                    assert_eq!(rows, decoded);
                }
                println!(
                    "{}",
                    json!({
                        "benchmark": "publication_candidates",
                        "pair": pair,
                        "variant": variant,
                        "sqlite": rusqlite::version(),
                        "rows": 64,
                        "selectedRows": reference.len(),
                        "snapshotKiB": 128,
                        "reads": 8,
                        "wireBytesPerRead": encoded_bytes,
                        "databaseBytes": database_bytes,
                        "fixtureWriteTransactionMicros": fixture_write_micros,
                        "fixtureWriteCpuMicros": fixture_write_cpu_micros,
                        "cpuMicros": cpu_micros() - cpu,
                        "elapsedMicros": started.elapsed().as_micros(),
                        "connectionCold": connection_cold,
                        "coldTimingIncludesConnectionOpenClose": connection_cold,
                        "osCache": "warm; no host cache eviction",
                        "walBytesUnchanged": wal_bytes() == initial_wal_bytes,
                    })
                );
            }
        }
    }
    assert_eq!(wal_bytes(), initial_wal_bytes);
}

fn cpu_micros() -> u128 {
    let mut time = std::mem::MaybeUninit::<libc::timespec>::uninit();
    // SAFETY: a successful clock_gettime initializes the output pointer.
    assert_eq!(
        unsafe { libc::clock_gettime(libc::CLOCK_PROCESS_CPUTIME_ID, time.as_mut_ptr()) },
        0
    );
    // SAFETY: successful initialization was checked above.
    let time = unsafe { time.assume_init() };
    time.tv_sec as u128 * 1_000_000 + time.tv_nsec as u128 / 1000
}
