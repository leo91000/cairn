//! Snapshot produced by the previous real Store/Service, with synthetic accounts and secrets.
mod common;

use axum::body::Body;
use cairn_installation::{reads::sql::ReadKind, service::Service, store::Store};
use common::{RelayContext, relay_fixture::router};
use rusqlite::{Connection, types::Value as SqlValue};
use serde_json::{Value, json};
use std::collections::BTreeMap;

const PREVIOUS: &str = include_str!("fixtures/personal-state-v17.sql");

fn previous(root: &std::path::Path) -> Connection {
    let db = Connection::open(root.join("data/manager.db")).unwrap();
    db.execute_batch(PREVIOUS).unwrap();
    db
}

type Snapshot = BTreeMap<String, (String, Vec<Vec<SqlValue>>)>;

fn snapshot(db: &Connection) -> Snapshot {
    let tables = db
        .prepare("SELECT name,sql FROM sqlite_schema WHERE type='table' ORDER BY name")
        .unwrap()
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    tables
        .into_iter()
        .map(|(name, sql)| {
            let mut statement = db
                .prepare(&format!("SELECT * FROM \"{}\"", name.replace('"', "\"\"")))
                .unwrap();
            let count = statement.column_count();
            let rows = statement
                .query_map([], |r| {
                    (0..count)
                        .map(|i| r.get(i))
                        .collect::<rusqlite::Result<Vec<_>>>()
                })
                .unwrap()
                .collect::<rusqlite::Result<_>>()
                .unwrap();
            (name, (sql, rows))
        })
        .collect()
}

fn personal_keys(db: &Connection) -> Vec<(String, String)> {
    db.prepare("SELECT key,data FROM kv WHERE key LIKE 'read-since:%' OR key LIKE 'read:%' OR key LIKE 'dismissed:%' ORDER BY key")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

#[tokio::test]
async fn real_v17_upgrade_preserves_each_accounts_reads_and_set_aside_outcomes() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("data")).unwrap();
    let db = previous(root.path());
    let before = snapshot(&db);
    let runs_before = common::run_fixture::projection_rows(&db);
    let previous_state = personal_keys(&db);
    assert_eq!(previous_state.len(), 11);
    let proof: Value = serde_json::from_str(
        &db.query_row(
            "SELECT data FROM kv WHERE key='b4-fixture-proof'",
            [],
            |r| r.get::<_, String>(0),
        )
        .unwrap(),
    )
    .unwrap();

    let store = Store::open(&root.path().join("data")).unwrap();
    let accounts = [
        "fixture-owner",
        "00000000-0000-4000-8000-0000000000a2",
        "00000000-0000-4000-8000-0000000000a3",
    ];
    let migrated = store
        .read(move |db| {
            let mut migrated = Vec::new();
            for account in accounts {
                migrated.push((
                    db.read_start(account)?,
                    db.read_marks(account)?,
                    db.dismissals(account)?,
                ));
            }
            Ok(migrated)
        })
        .await
        .unwrap();
    // Every previous key maps to exactly one row, with its identity spelling and value.
    let mut rows = 0;
    for (key, data) in &previous_state {
        let value: Value = serde_json::from_str(data).unwrap();
        let (index, rest) = accounts
            .iter()
            .enumerate()
            .find_map(|(index, account)| {
                [
                    format!("read-since:{account}"),
                    format!("read:{account}:"),
                    format!("dismissed:{account}:"),
                ]
                .into_iter()
                .find_map(|prefix| {
                    key.strip_prefix(&prefix)
                        .map(|rest| (index, (prefix, rest)))
                })
            })
            .unwrap();
        let (start, marks, dismissals) = &migrated[index];
        let (prefix, item) = rest;
        if prefix.starts_with("read-since:") {
            assert_eq!(*start, value.as_i64());
        } else if prefix.starts_with("read:") {
            let (kind, id) = item.split_once(':').unwrap();
            let kind = if kind == "chat" {
                ReadKind::Chat
            } else {
                ReadKind::Mission
            };
            assert_eq!(
                marks.get(&(kind, id.to_owned())).copied(),
                value.as_i64(),
                "{key}"
            );
        } else {
            let id = item.strip_prefix("task:").unwrap();
            let dismissal = dismissals[id];
            assert_eq!(Some(dismissal.finished_at), value["finishedAt"].as_i64());
            assert_eq!(dismissal.until, value["until"].as_i64());
        }
        rows += 1;
    }
    let total: usize = migrated
        .iter()
        .map(|(start, marks, dismissals)| {
            usize::from(start.is_some()) + marks.len() + dismissals.len()
        })
        .sum();
    assert_eq!(total, rows);
    assert!(migrated[0].1.contains_key(&(
        ReadKind::Chat,
        "EC5A395F-3940-4B8D-9F57-1530018C717F".to_owned()
    )));

    let after = snapshot(&db);
    assert_eq!(common::run_fixture::projection_rows(&db), runs_before);
    for (name, old) in &before {
        if matches!(name.as_str(), "chat_messages" | "chat_questions") {
            let (schema, rows) = &after[name];
            let previous_schema = old.0.strip_suffix(')').unwrap();
            assert_eq!(
                schema,
                &format!(
                    "{previous_schema}, plan INTEGER NOT NULL DEFAULT 0 CHECK(plan IN (0,1)))"
                ),
                "only the Plan flag may be added to {name}"
            );
            let previous_rows = rows
                .iter()
                .map(|row| {
                    assert_eq!(row.last(), Some(&SqlValue::Integer(0)));
                    row[..row.len() - 1].to_vec()
                })
                .collect::<Vec<_>>();
            assert_eq!(previous_rows, old.1, "preserved rows in {name}");
        } else if name != "kv" && name != "runs" {
            assert_eq!(after.get(name), Some(old), "unrelated table {name}");
        }
    }
    let kv = |snapshot: &Snapshot| {
        snapshot["kv"]
            .1
            .iter()
            .filter(|row| {
                let SqlValue::Text(key) = &row[0] else {
                    panic!("text key")
                };
                !key.starts_with("read") && !key.starts_with("dismissed:")
            })
            .cloned()
            .collect::<Vec<_>>()
    };
    assert_eq!(kv(&after), kv(&before));
    assert!(personal_keys(&db).is_empty());
    for key in ["read:a:chat:x", "read-since:a", "dismissed:a:task:x"] {
        assert!(
            db.execute("INSERT INTO kv VALUES(?,'1',NULL)", [key])
                .is_err(),
            "{key}"
        );
    }
    assert!(
        !db.prepare("PRAGMA foreign_key_check")
            .unwrap()
            .exists([])
            .unwrap()
    );
    let stable = snapshot(&db);
    drop(store);
    drop(Store::open(&root.path().join("data")).unwrap());
    assert_eq!(snapshot(&db), stable);
    assert_eq!(
        db.pragma_query_value::<i64, _>(None, "user_version", |r| r.get(0))
            .unwrap(),
        20
    );

    // The real service serves each account its own preserved outcomes.
    let s = Service::new(common::config(root.path())).await.unwrap();
    let app = router(s.clone()).await.unwrap();
    let owner = RelayContext::new(&common::relay_fixture::context(&s).await);
    let member = RelayContext::new(
        &common::relay_fixture::member_context(&s, "00000000-0000-4000-8000-0000000000a2").await,
    );
    let activity = async |who: &RelayContext| {
        let request = who
            .authorize(common::request("GET", "/api/tasks/activity"))
            .body(Body::empty())
            .unwrap();
        let runs = common::read_json(common::send(&app, request).await).await;
        proof["runs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|run| {
                runs.as_array()
                    .unwrap()
                    .iter()
                    .find(|listed| listed["id"] == *run)
                    .unwrap()["dismissal"]
                    .clone()
            })
            .collect::<Vec<_>>()
    };
    let until = &proof["until"];
    assert_eq!(
        activity(&owner).await,
        [
            json!({ "until": null }),
            json!({ "until": until }),
            Value::Null
        ]
    );
    assert_eq!(
        activity(&member).await,
        [json!({ "until": until }), Value::Null, Value::Null]
    );
}

#[tokio::test]
async fn v17_upgrade_preserves_legacy_paid_retry_and_checkpoint() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("data")).unwrap();
    let db = previous(root.path());
    let id = "00000000-0000-4000-8000-000000000010";
    // Shape exported by the e1dac worker after scheduling and before fencing.
    let cause = json!({
        "kind": "server",
        "message": "Provider unavailable",
        "startup": false,
    });
    let checkpoint = json!({
        "prepared": { "backend": "native", "isolated": true },
        "runnerId": "old-attempt",
        "remainingMs": null,
        "completed": false,
        "terminalCode": null,
        "providerFailed": true,
        "lastError": "Provider unavailable",
        "retryCause": cause,
        "controllerRecoveries": 3,
    })
    .to_string();
    db.execute(
        "UPDATE run_checkpoints SET state=? WHERE run_id=?",
        (&checkpoint, id),
    )
    .unwrap();
    db.execute(
        "UPDATE runs SET status='queued', recovery_pending=1, retry_attempt=2, retry_limit=5, retry_next_at=1700000005000, retry_started_at=1700000000000, retry_deadline_at=1700000600000, retry_cause=? WHERE id=?",
        (cause.to_string(), id),
    ).unwrap();
    let old_retry: (i64, i64, i64, i64, i64, String) = db.query_row(
        "SELECT retry_attempt,retry_limit,retry_next_at,retry_started_at,retry_deadline_at,retry_cause FROM runs WHERE id=?",
        [id], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?)),
    ).unwrap();
    drop(db);

    let store = Store::open(&root.path().join("data")).unwrap();
    let persisted = store.checkpoint_boundary(id).await.unwrap().unwrap();
    assert_eq!(
        persisted,
        serde_json::from_str::<Value>(&checkpoint).unwrap()
    );
    let db = Connection::open(root.path().join("data/manager.db")).unwrap();
    let actual: (i64, i64, i64, i64, i64, String) = db.query_row(
        "SELECT retry_attempt,retry_limit,retry_next_at,retry_started_at,retry_deadline_at,retry_cause FROM runs WHERE id=?",
        [id], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?)),
    ).unwrap();
    assert_eq!(actual, old_retry);
    assert_eq!(
        db.query_row(
            "SELECT state FROM run_checkpoints WHERE run_id=?",
            [id],
            |r| r.get::<_, String>(0)
        )
        .unwrap(),
        checkpoint
    );
    assert_eq!(
        db.pragma_query_value::<i64, _>(None, "user_version", |r| r.get(0))
            .unwrap(),
        20
    );
}

#[test]
fn malformed_previous_read_state_rolls_back_schema_data_and_version() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("data")).unwrap();
    let db = previous(root.path());
    let corrupt = "UPDATE kv SET data=json_set(data,'$.until',json_extract(data,'$.finishedAt')) WHERE key=(SELECT key FROM kv WHERE key LIKE 'dismissed:%' LIMIT 1)";
    db.execute(corrupt, []).unwrap();
    let before = snapshot(&db);
    assert!(Store::open(&root.path().join("data")).is_err());
    assert_eq!(snapshot(&db), before);
    assert_eq!(
        db.pragma_query_value::<i64, _>(None, "user_version", |r| r.get(0))
            .unwrap(),
        17
    );

    // An unrecognized personal key is not silently dropped either.
    db.execute(
        "UPDATE kv SET data=json_set(data,'$.until',NULL) WHERE key LIKE 'dismissed:%'",
        [],
    )
    .unwrap();
    db.execute("INSERT INTO kv VALUES('read:fixture-owner:agent:00000000-0000-4000-8000-000000000001','1',NULL)", [])
        .unwrap();
    let before = snapshot(&db);
    assert!(Store::open(&root.path().join("data")).is_err());
    assert_eq!(snapshot(&db), before);

    db.execute(
        "DELETE FROM kv WHERE key LIKE 'read:fixture-owner:agent:%'",
        [],
    )
    .unwrap();
    drop(Store::open(&root.path().join("data")).unwrap());
    assert_eq!(
        db.pragma_query_value::<i64, _>(None, "user_version", |r| r.get(0))
            .unwrap(),
        20
    );
}
