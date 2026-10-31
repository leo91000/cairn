use crate::{
    node_fixture::DbNodeFixture,
    store::{Db, Store},
};
use rusqlite::{Connection, params};
use serde_json::{Value, json};

const NODE: &str = "00000000-0000-4000-8000-000000000091";
const RUN: &str = "00000000-0000-4000-8000-000000000092";
const ATTEMPT: &str = "00000000-0000-4000-8000-000000000093";
const RESERVATION: &str = "00000000-0000-4000-8000-000000000094";

async fn previous(root: &std::path::Path) -> Connection {
    drop(Store::open(root).unwrap());
    let mut db = Connection::open(root.join("manager.db")).unwrap();
    crate::placement_migration_fixture::previous_layout(&db);
    db.execute_batch("DROP TABLE records;
CREATE TABLE records(id TEXT PRIMARY KEY,kind TEXT NOT NULL CHECK(kind NOT IN ('agentAccounts','chats','nodes','projects','agents','tasks','mcps','plugin-connections','plugin-packages','plugin-requests','plugin-activity','plugin-timers','plugin-events','plugin-deliveries','plugin-storage','plugin-http-receipts')),data TEXT NOT NULL,updated_at INTEGER NOT NULL);
CREATE INDEX records_kind ON records(kind,updated_at DESC);
CREATE INDEX records_node_backups_run ON records(json_extract(data,'$.runId')) WHERE kind='node-backups';
ALTER TABLE nodes ADD COLUMN exclusive_reservation TEXT;
PRAGMA user_version=18;").unwrap();
    Db(&db).put_node_fixture(&json!({"id":NODE})).unwrap();
    crate::runs::projections::upgrade(&mut db).unwrap();
    db.pragma_update(None, "user_version", 20).unwrap();
    crate::worker::progress::upgrade(&mut db).unwrap();
    db
}

fn attempt() -> Value {
    json!({"id":ATTEMPT,"role":"execution","runId":RUN,"nodeId":NODE,"resources":{"cpu":4,"memoryMiB":8192,"diskMiB":32768},"runtimeId":"retained-runtime","createdAt":123,"leaseExpiresAt":999999,"released":false,"nativeMode":{"mode":"administrator","generation":"retained-generation"},"additionalDiskMiB":128,"diskMaterialized":true,"leaseRequired":true,"leaseDurationMs":60000})
}

fn seed(db: &Connection) -> Value {
    let boundary = json!({"nodeId":NODE,"pinnedNodeId":NODE,"preferredNodeId":NODE,"nodeState":"restoring","requiredTags":["first","second"],"resources":{"cpu":4,"memoryMiB":8192,"diskMiB":32768},"nativeMode":{"mode":"administrator","generation":"retained-generation"},"nativeModeInterruption":"newer-generation","executionBackend":"native","moveReservation":ATTEMPT,"movementError":"retained error","storage":{"opaqueA11":9007199254740001_u64},"otherBoundary":"unchanged"});
    db.execute("INSERT INTO runs(id,task_id,status,created_at,recovery_pending,resume_available,resume_count,boundary) VALUES(?1,'task','running',123,1,1,7,?2)",params![RUN,boundary.to_string()]).unwrap();
    db.execute(
        "INSERT INTO records VALUES(?1,'node-attempts',?2,456)",
        params![ATTEMPT, attempt().to_string()],
    )
    .unwrap();
    let reason = crate::run_output::redact(&format!("{}Bearer x", "x".repeat(492)), &[]);
    assert!(reason.chars().count() > 500);
    let reservation = json!({"id":RESERVATION,"nodeId":NODE,"ownerRunId":RUN,"ownerAttemptId":ATTEMPT,"memberAttemptIds":[],"messageId":"turn","reason":reason,"input":{"durationSeconds":300,"waitSeconds":0,"acquisitionTimeoutSeconds":300,"reason":reason,"memberRunIds":[],"minimumAvailableMemoryMiB":1024},"createdAt":124,"order":42,"waitUntil":300124,"state":"releasing","epoch":73,"expiresAt":300125,"requestedExpiresAt":300127,"acquisitionStartedAt":125,"acquiredAt":126,"releaseRequestedAt":127,"pausedRunIds":[RUN],"error":null});
    db.execute(
        "INSERT INTO records VALUES(?1,'node-exclusive-requests',?2,457)",
        params![RESERVATION, reservation.to_string()],
    )
    .unwrap();
    db.execute(
        "INSERT INTO records VALUES('other','node-backups','{\"exact\":9007199254740001}',458)",
        [],
    )
    .unwrap();
    for (key, data) in [
        ("exclusive-epoch", "99"),
        ("exclusive-request-order", "100"),
        ("credential", "{\"encrypted\":\"private-test-envelope\"}"),
    ] {
        db.execute("INSERT INTO kv VALUES(?1,?2,NULL)", params![key, data])
            .unwrap();
    }
    boundary
}

#[tokio::test]
async fn startup_preserves_attempt_ownership_placement_counters_and_private_boundaries() {
    let root = tempfile::tempdir().unwrap();
    let db = previous(root.path()).await;
    let boundary = seed(&db);
    let reason: String = db
        .query_row(
            "SELECT json_extract(data,'$.reason') FROM records WHERE id=?",
            [RESERVATION],
            |row| row.get(0),
        )
        .unwrap();
    let store = Store::open(root.path()).unwrap();
    let run = store.run(RUN).await.unwrap();
    for (field, value) in boundary.as_object().unwrap() {
        assert_eq!(&run[field], value, "{field}");
    }
    let saved = serde_json::to_value(store.attempt(ATTEMPT).await.unwrap().unwrap()).unwrap();
    for (field, value) in attempt().as_object().unwrap() {
        assert_eq!(&saved[field], value, "{field}");
    }
    let record = store
        .exclusive_reservation(RESERVATION)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(record.epoch, Some(73));
    assert_eq!(record.reason, reason);
    assert_eq!(record.input.reason, reason);
    assert_eq!(record.order, 42);
    assert_eq!(record.paused_run_ids, [RUN]);
    assert_eq!(record.requested_expires_at, Some(300127));
    assert_eq!(
        store
            .node(NODE)
            .await
            .unwrap()
            .unwrap()
            .exclusive_reservation
            .unwrap()
            .id,
        RESERVATION
    );
    store
        .transaction(|db| {
            assert_eq!(db.next_exclusive_order()?, 101);
            assert_eq!(db.next_exclusive_epoch(70)?, 100);
            Ok(())
        })
        .await
        .unwrap();
    assert_eq!(
        db.query_row("SELECT data FROM records WHERE id='other'", [], |r| r
            .get::<_, String>(0))
            .unwrap(),
        "{\"exact\":9007199254740001}"
    );
    assert_eq!(
        db.query_row("SELECT data FROM kv WHERE key='credential'", [], |r| r
            .get::<_, String>(
            0
        ))
        .unwrap(),
        "{\"encrypted\":\"private-test-envelope\"}"
    );
    assert!(
        !db.prepare("PRAGMA foreign_key_check")
            .unwrap()
            .exists([])
            .unwrap()
    );
    assert_eq!(
        db.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        crate::store::DATABASE_VERSION
    );
    drop(store);
    assert!(Store::open(root.path()).is_ok());
}

#[tokio::test]
async fn startup_preserves_embedded_nul_reasons_and_rejects_empty_reasons() {
    for reason in ["\0", "\0work", " \0work\0 "] {
        let root = tempfile::tempdir().unwrap();
        let db = previous(root.path()).await;
        seed(&db);
        let data: String = db
            .query_row(
                "SELECT data FROM records WHERE id=?",
                [RESERVATION],
                |row| row.get(0),
            )
            .unwrap();
        let mut legacy: Value = serde_json::from_str(&data).unwrap();
        legacy["reason"] = reason.into();
        legacy["input"]["reason"] = reason.into();
        db.execute(
            "UPDATE records SET data=? WHERE id=?",
            params![legacy.to_string(), RESERVATION],
        )
        .unwrap();

        let store = Store::open(root.path()).unwrap();
        let record = store
            .exclusive_reservation(RESERVATION)
            .await
            .unwrap()
            .unwrap();
        record.input.validate().unwrap();
        assert_eq!(record.reason, reason);
        assert_eq!(record.input.reason, reason);
        store
            .transaction(move |db| {
                db.save_exclusive_reservation(&record)?;
                for invalid in ["", "   "] {
                    assert!(
                        db.0.execute(
                            "UPDATE exclusive_reservations SET reason=? WHERE id=?",
                            params![invalid, RESERVATION],
                        )
                        .is_err()
                    );
                }
                assert_eq!(
                    db.exclusive_reservation(RESERVATION)?.unwrap().reason,
                    reason
                );
                Ok(())
            })
            .await
            .unwrap();
        drop(store);
        assert!(Store::open(root.path()).is_ok());
    }
}

#[tokio::test]
async fn failed_late_conversion_rolls_back_schema_rows_counters_and_version() {
    let root = tempfile::tempdir().unwrap();
    let db = previous(root.path()).await;
    seed(&db);
    db.execute("INSERT INTO records VALUES('bad','node-exclusive-requests','{\"private\":\"must-not-escape\"}',999)",[]).unwrap();
    let schema: Vec<(String, String)> = db
        .prepare("SELECT name,sql FROM sqlite_master WHERE sql IS NOT NULL ORDER BY name")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    let error = Store::open(root.path()).err().unwrap();
    assert!(!error.message.contains("must-not-escape"));
    let after: Vec<(String, String)> = db
        .prepare("SELECT name,sql FROM sqlite_master WHERE sql IS NOT NULL ORDER BY name")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(after, schema);
    assert_eq!(
        db.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        21
    );
    assert_eq!(
        db.query_row(
            "SELECT data FROM kv WHERE key='exclusive-epoch'",
            [],
            |row| row.get::<_, String>(0)
        )
        .unwrap(),
        "99"
    );
    assert!(
        db.prepare("SELECT 1 FROM records WHERE id=?")
            .unwrap()
            .exists([ATTEMPT])
            .unwrap()
    );
    db.execute("DELETE FROM records WHERE id='bad'", [])
        .unwrap();
    assert!(Store::open(root.path()).is_ok());
}

#[tokio::test]
async fn failed_attempt_write_does_not_leave_an_allocation_when_the_caller_commits() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(root.path()).unwrap();
    store
        .transaction(|db| {
            let mut attempt: super::attempts::Attempt = serde_json::from_value(attempt())?;
            attempt.node_id = super::LOCAL_NODE_ID.into();
            attempt.lease_duration_ms = Some(u64::MAX);
            assert!(db.save_attempt(&attempt).is_err());
            assert!(db.attempt(ATTEMPT)?.is_none());
            assert!(
                !db.0
                    .prepare("SELECT 1 FROM placements WHERE id=?")?
                    .exists([ATTEMPT])?
            );
            Ok(())
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn previous_manager_database_upgrades_without_losing_unconverted_state() {
    qualify_previous_manager_database(None).await;
}

#[tokio::test]
async fn previous_manager_database_preserves_small_derived_disk_allowances() {
    for allowance in [0, 116, 120] {
        qualify_previous_manager_database(Some(allowance)).await;
    }
}

async fn qualify_previous_manager_database(allowance: Option<u64>) {
    // Produced by the real previous manager Store/writers at ff735120; all
    // credentials and identities in this fixture are synthetic.
    let mut source = Connection::open_in_memory().unwrap();
    source
        .execute_batch(include_str!("../../tests/fixtures/placements-v15.sql"))
        .unwrap();
    // Apply the actual previous manager migrations to obtain its version-21 layout, retaining hooks and writer-owned progress.
    crate::chats::question_migration::upgrade(&mut source).unwrap();
    crate::attachments::migration::upgrade(&mut source).unwrap();
    crate::reads::migration::upgrade(&mut source).unwrap();
    crate::runs::projections::upgrade(&mut source).unwrap();
    source.execute_batch(crate::plugins::hooks::SCHEMA).unwrap();
    source.pragma_update(None, "user_version", 20).unwrap();
    crate::worker::progress::upgrade(&mut source).unwrap();
    if let Some(allowance) = allowance {
        let allowance = i64::try_from(allowance).unwrap();
        // The previous writer stored derived allowances without reapplying the
        // configured budget minimum. Exercise those values in the real v15 DB.
        assert!(source.execute(
            "UPDATE records SET data=json_set(data,'$.resources.diskMiB',?) WHERE kind='node-attempts'",
            [allowance],
        ).unwrap() > 0);
        assert!(source.execute(
            "UPDATE runs SET boundary=json_set(boundary,'$.resources.diskMiB',?) WHERE json_type(boundary,'$.resources')='object'",
            [allowance],
        ).unwrap() > 0);
        source.execute(
            "UPDATE runs SET boundary=json_set(boundary,'$.moveRequest.resources.diskMiB',?) WHERE json_type(boundary,'$.moveRequest')='object'",
            [allowance],
        ).unwrap();
    }
    assert_eq!(
        source
            .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
            .unwrap(),
        21
    );
    let runs: Vec<(String, Value)> = source
        .prepare("SELECT id,data FROM run_views ORDER BY id")
        .unwrap()
        .query_map([], |row| {
            let data: String = row.get(1)?;
            Ok((row.get(0)?, serde_json::from_str(&data).unwrap()))
        })
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert!(!runs.is_empty());
    crate::worker::progress::Progress::launched(
        "retained-progress-execution".into(),
        None,
        None,
        None,
        1,
    )
    .store_in(&Db(&source), &runs[0].0)
    .unwrap();
    let tables: Vec<String> = source
        .prepare("SELECT name FROM sqlite_master WHERE type='table' AND name!='runs' ORDER BY name")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    let unchanged = |db: &Connection| -> Vec<(String, Vec<String>)> {
        tables
            .iter()
            .map(|table| {
                let columns: Vec<String> = source
                    .prepare(&format!("PRAGMA table_info(\"{table}\")"))
                    .unwrap()
                    .query_map([], |row| row.get::<_, String>(1))
                    .unwrap()
                    .collect::<rusqlite::Result<Vec<_>>>()
                    .unwrap()
                    .into_iter()
                    .filter(|column| table != "nodes" || column != "exclusive_reservation")
                    .collect();
                let fields = columns
                    .iter()
                    .map(|column| format!("quote(\"{column}\")"))
                    .collect::<Vec<_>>()
                    .join(",");
                let filter = match table.as_str() {
                    "records" => " WHERE kind NOT IN ('node-attempts','node-exclusive-requests')",
                    "kv" => " WHERE key NOT IN ('exclusive-epoch','exclusive-request-order')",
                    _ => "",
                };
                let rows = db
                    .prepare(&format!(
                        "SELECT json_array({fields}) FROM \"{table}\"{filter} ORDER BY 1"
                    ))
                    .unwrap()
                    .query_map([], |row| row.get(0))
                    .unwrap()
                    .collect::<rusqlite::Result<Vec<String>>>()
                    .unwrap();
                (table.clone(), rows)
            })
            .collect()
    };
    let old_attempts = Db(&source).list("node-attempts").unwrap();
    let old_reservations = Db(&source).list("node-exclusive-requests").unwrap();
    let before = unchanged(&source);
    let root = tempfile::tempdir().unwrap();
    let destination = root.path().join("manager.db");
    source
        .execute("VACUUM INTO ?", [destination.to_str().unwrap()])
        .unwrap();
    let store = Store::open(root.path()).unwrap();
    for (id, expected) in runs {
        let actual = store.run(&id).await.unwrap();
        for (field, value) in expected.as_object().unwrap() {
            assert_eq!(&actual[field], value, "{field}");
        }
    }
    for expected in old_attempts {
        let actual = serde_json::to_value(
            store
                .attempt(expected["id"].as_str().unwrap())
                .await
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        for (field, value) in expected.as_object().unwrap() {
            assert_eq!(&actual[field], value, "{field}");
        }
    }
    for expected in old_reservations {
        let actual = serde_json::to_value(
            store
                .exclusive_reservation(expected["id"].as_str().unwrap())
                .await
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        for (field, value) in expected.as_object().unwrap() {
            assert_eq!(&actual[field], value, "{field}");
        }
    }
    let after = Connection::open(destination).unwrap();
    assert_eq!(unchanged(&after), before);
    assert!(
        !after
            .prepare("PRAGMA foreign_key_check")
            .unwrap()
            .exists([])
            .unwrap()
    );
    assert_eq!(
        after
            .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
            .unwrap(),
        crate::store::DATABASE_VERSION
    );
}
