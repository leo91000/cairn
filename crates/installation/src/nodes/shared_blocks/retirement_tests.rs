use super::*;
use crate::store::Store;
use rusqlite::Connection;

const REFERENCE: &str = include_str!("../../../tests/fixtures/retirement-objects-reference.sql");

fn fixture() -> (tempfile::TempDir, Connection) {
    let root = tempfile::tempdir().unwrap();
    drop(Store::open(root.path()).unwrap());
    let connection = Connection::open(root.path().join("manager.db")).unwrap();
    connection.execute_batch("PRAGMA foreign_keys=ON").unwrap();
    (root, connection)
}

#[test]
fn retirement_eligibility_uses_the_covering_reference_index() {
    let (_root, connection) = fixture();
    let mut statement = connection
        .prepare(&format!("EXPLAIN QUERY PLAN {RETIRE_UNREFERENCED_OBJECTS}"))
        .unwrap();
    let plan = statement
        .query_map(params!["publication-0", 123456], |row| {
            row.get::<_, String>(3)
        })
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert!(
        plan.iter().any(|detail| detail
            .contains("SEARCH r USING COVERING INDEX shared_references_object")),
        "Retirement must inspect reference cardinality without table lookups: {plan:?}"
    );
}

fn snapshot(connection: &Connection, normalize_since: i64) -> Vec<String> {
    let queries = [
        "SELECT json_array(id,destination,hash,size,location,state,CASE WHEN unused_at>=?1 THEN 123456 ELSE unused_at END,retry_at) FROM shared_objects ORDER BY id",
        "SELECT json_array(id,run_id) FROM shared_publications WHERE ?1 IS NOT NULL ORDER BY id",
        "SELECT json_array(publication,object) FROM shared_references WHERE ?1 IS NOT NULL ORDER BY publication,object",
    ];
    let mut state = Vec::new();
    for query in queries {
        state.extend(
            connection
                .prepare(query)
                .unwrap()
                .query_map([normalize_since], |row| row.get::<_, String>(0))
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap(),
        );
    }
    state
}

#[test]
fn retirement_preserves_all_ownership_combinations_and_object_states() {
    let (_root, mut connection) = fixture();
    let transaction = connection.transaction().unwrap();
    for publication in 0..5 {
        transaction
            .execute(
                "INSERT INTO shared_publications VALUES(?1,?1)",
                [format!("publication-{publication}")],
            )
            .unwrap();
    }
    for owners in 0..32 {
        for (index, state) in ["pending", "ready", "invalid", "deleting"]
            .into_iter()
            .enumerate()
        {
            let object = format!("object-{owners:02}-{state}");
            transaction
                .execute(
                    "INSERT INTO shared_objects VALUES(?1,'s3',?1,4096,'{}',?2,?3,17)",
                    params![object, state, if index % 2 == 0 { None } else { Some(111) }],
                )
                .unwrap();
            for publication in 0..5 {
                if owners & (1 << publication) == 0 {
                    continue;
                }
                transaction
                    .execute(
                        "INSERT INTO shared_references VALUES(?1,?2)",
                        params![format!("publication-{publication}"), object],
                    )
                    .unwrap();
            }
        }
    }
    transaction.commit().unwrap();

    connection.execute_batch("SAVEPOINT reference").unwrap();
    connection
        .execute(REFERENCE, params!["publication-0", 123456])
        .unwrap();
    connection
        .execute(
            "DELETE FROM shared_publications WHERE id=?1",
            ["publication-0"],
        )
        .unwrap();
    let expected = snapshot(&connection, i64::MAX);
    connection
        .execute_batch("ROLLBACK TO reference; RELEASE reference")
        .unwrap();

    let before = now();
    let transaction = connection.transaction().unwrap();
    release(&Db(&transaction), "publication-0").unwrap();
    transaction.commit().unwrap();
    let after = now();
    let retired: (i64, i64, i64) = connection
        .query_row(
            "SELECT count(*),min(unused_at),max(unused_at) FROM shared_objects WHERE unused_at>=?1",
            [before],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        retired.0, 4,
        "Only the four exclusively owned objects retire"
    );
    assert!(retired.1 >= before && retired.2 <= after);
    assert_eq!(snapshot(&connection, before), expected);
    let unchanged = snapshot(&connection, i64::MAX);
    release(&Db(&connection), "missing-publication").unwrap();
    assert_eq!(snapshot(&connection, i64::MAX), unchanged);
    assert_eq!(
        connection
            .query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
        0
    );
}

fn reference_release(db: &Db<'_>, publication: &str) -> Result<()> {
    let retained = db.0.execute(
        "UPDATE direct_journal_acknowledgements SET retired=1 WHERE publication=?1",
        [publication],
    )?;
    if retained > 0 {
        return Ok(());
    }
    db.0.execute(REFERENCE, params![publication, now()])?;
    db.0.execute("DELETE FROM shared_publications WHERE id=?1", [publication])?;
    Ok(())
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

#[test]
#[ignore = "explicit committed publication retirement measurement; run alone"]
fn retirement_transaction_measurement() {
    let (root, mut seed) = fixture();
    let seed_started = std::time::Instant::now();
    let seed_cpu = cpu_micros();
    let transaction = seed.transaction().unwrap();
    for publication in 0..4 {
        transaction
            .execute(
                "INSERT INTO shared_publications VALUES(?1,?1)",
                [format!("publication-{publication}")],
            )
            .unwrap();
    }
    {
        let mut objects = transaction
            .prepare("INSERT INTO shared_objects VALUES(?1,'s3',?2,4096,'{}','ready',NULL,0)")
            .unwrap();
        let mut references = transaction
            .prepare("INSERT INTO shared_references VALUES(?1,?2)")
            .unwrap();
        for index in 0..65_536 {
            let object = format!("object-{index:032x}");
            let owner = index % 4;
            objects
                .execute(params![object, format!("{index:064x}")])
                .unwrap();
            references
                .execute(params![format!("publication-{owner}"), object])
                .unwrap();
            if index % 5 == 0 {
                references
                    .execute(params![format!("publication-{}", (owner + 1) % 4), object])
                    .unwrap();
            }
        }
    }
    transaction.commit().unwrap();
    let fixture_write_micros = seed_started.elapsed().as_micros();
    let fixture_write_cpu_micros = cpu_micros() - seed_cpu;
    seed.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")
        .unwrap();
    let seed_path = root.path().join("manager.db");
    let database_bytes = std::fs::metadata(&seed_path).unwrap().len();
    let references: i64 = seed
        .query_row("SELECT count(*) FROM shared_references", [], |row| {
            row.get(0)
        })
        .unwrap();
    let owned_references: i64 = seed
        .query_row(
            "SELECT count(*) FROM shared_references WHERE publication='publication-0'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    drop(seed);

    let mut expected = None;
    let mut expected_effects = None;
    for pair in 0..4 {
        let order = if pair % 2 == 0 {
            ["reference", "candidate"]
        } else {
            ["candidate", "reference"]
        };
        for variant in order {
            let trial = tempfile::tempdir_in(root.path()).unwrap();
            let database = trial.path().join("manager.db");
            std::fs::copy(&seed_path, &database).unwrap();
            let mut connection = Connection::open(&database).unwrap();
            connection
                .execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")
                .unwrap();
            let changes_before = connection.total_changes();
            let before = now();
            let cpu = cpu_micros();
            let started = std::time::Instant::now();
            let transaction = connection
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                .unwrap();
            if variant == "reference" {
                reference_release(&Db(&transaction), "publication-0").unwrap();
            } else {
                release(&Db(&transaction), "publication-0").unwrap();
            }
            transaction.commit().unwrap();
            let elapsed_micros = started.elapsed().as_micros();
            let used_cpu = cpu_micros() - cpu;
            let changed_rows = connection.total_changes() - changes_before;
            let wal_bytes = std::fs::metadata(database.with_extension("db-wal"))
                .unwrap()
                .len();
            let state = snapshot(&connection, before);
            let encoded_state_bytes: usize = state.iter().map(String::len).sum();
            if let Some(expected) = &expected {
                assert_eq!(&state, expected);
            } else {
                expected = Some(state);
            }
            if let Some(effects) = expected_effects {
                assert_eq!((changed_rows, wal_bytes), effects);
            } else {
                expected_effects = Some((changed_rows, wal_bytes));
            }
            assert_eq!(
                connection
                    .query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |row| {
                        row.get::<_, i64>(0)
                    })
                    .unwrap(),
                0
            );
            println!(
                "{}",
                json!({
                    "benchmark": "publication_retirement",
                    "pair": pair,
                    "variant": variant,
                    "sqlite": rusqlite::version(),
                    "objects": 65_536,
                    "references": references,
                    "retiringPublicationReferences": owned_references,
                    "databaseBytes": database_bytes,
                    "encodedFinalStateBytes": encoded_state_bytes,
                    "fixtureWriteTransactionMicros": fixture_write_micros,
                    "fixtureWriteCpuMicros": fixture_write_cpu_micros,
                    "changedRows": changed_rows,
                    "walBytes": wal_bytes,
                    "cpuMicros": used_cpu,
                    "elapsedMicros": elapsed_micros,
                    "connection": "new; open and pragma setup excluded",
                    "osCache": "warm after fixture copy; no host cache eviction",
                    "boundary": "BEGIN IMMEDIATE, actual release statements, FK cascades and COMMIT included",
                })
            );
        }
    }
}
