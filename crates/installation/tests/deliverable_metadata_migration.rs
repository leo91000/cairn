//! Frozen by the real v18 Store (b3ebdc4b), including metadata written by its fixture binary.
use cairn_installation::store::Store;
use rusqlite::{Connection, types::Value as SqlValue};
use serde_json::Value;
use std::collections::BTreeMap;

const PREVIOUS: &str = include_str!("fixtures/deliverable-metadata-v18.sql");
const RUN: &str = "00000000-0000-4000-8000-000000000010";
const TOKEN: &str = "00000000-0000-4000-8000-000000000e01";

fn previous(path: &std::path::Path) -> Connection {
    let db = Connection::open(path.join("manager.db")).unwrap();
    db.execute_batch(PREVIOUS).unwrap();
    db
}

type Snapshot = BTreeMap<String, (String, Vec<Vec<SqlValue>>)>;

fn snapshot(db: &Connection) -> Snapshot {
    let tables = db
        .prepare("SELECT name,sql FROM sqlite_schema WHERE type='table' ORDER BY name")
        .unwrap()
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
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
                .query_map([], |row| {
                    (0..count)
                        .map(|i| row.get(i))
                        .collect::<rusqlite::Result<Vec<_>>>()
                })
                .unwrap()
                .collect::<rusqlite::Result<_>>()
                .unwrap();
            (name, (sql, rows))
        })
        .collect()
}

fn documents(db: &Connection) -> BTreeMap<String, Value> {
    db.prepare("SELECT key,data FROM kv WHERE key LIKE 'artifact:%' OR key LIKE 'skill-source:%'")
        .unwrap()
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .unwrap()
        .map(|row| {
            let (key, data) = row.unwrap();
            (key, serde_json::from_str(&data).unwrap())
        })
        .collect()
}

#[tokio::test]
async fn real_previous_database_preserves_versions_links_sources_and_unrelated_data() {
    let root = tempfile::tempdir().unwrap();
    let db = previous(root.path());
    let before = snapshot(&db);
    let documents = documents(&db);
    let store = Store::open(root.path()).unwrap();
    let (versions, source, public) = store
        .read(|db| {
            Ok((
                db.artifacts(Some(RUN))?,
                db.skill_source("global", "guide")?,
                db.public_artifact(TOKEN)?,
            ))
        })
        .await
        .unwrap();
    assert_eq!(versions.len(), 3);
    for version in versions {
        let mut actual = serde_json::to_value(&version).unwrap();
        // Omitted optional null fields carry the same public semantics and no data.
        let expected = &documents[&format!("artifact:{RUN}:{}", version.id)];
        for (key, value) in expected.as_object().unwrap() {
            if value.is_null() && actual.get(key).is_none() {
                actual[key] = Value::Null;
            }
        }
        assert_eq!(&actual, expected);
    }
    assert_eq!(
        serde_json::to_value(source.unwrap()).unwrap(),
        documents["skill-source:global/guide"]
    );
    assert_eq!(public.unwrap().public_token.as_deref(), Some(TOKEN));
    let historical = store
        .skill_source("00000000-0000-4000-8000-000000000dad", "guide")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        serde_json::to_value(historical).unwrap(),
        documents["skill-source:00000000-0000-4000-8000-000000000dad/guide"]
    );
    assert_eq!(
        db.query_row(
            "SELECT count(*) FROM skill_source_refs WHERE instance_id IS NULL",
            [],
            |row| row.get::<_, i64>(0)
        )
        .unwrap(),
        2
    );
    assert_eq!(
        db.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        19
    );
    assert_eq!(
        db.query_row("PRAGMA quick_check", [], |row| row.get::<_, String>(0))
            .unwrap(),
        "ok"
    );
    assert!(
        !db.prepare("PRAGMA foreign_key_check")
            .unwrap()
            .exists([])
            .unwrap()
    );
    let after = snapshot(&db);
    for (table, (sql, rows)) in before {
        if table != "kv" {
            assert_eq!(after[&table], (sql, rows), "{table}");
        }
    }
    assert_eq!(db.query_row("SELECT count(*) FROM kv WHERE key LIKE 'artifact:%' OR key LIKE 'artifact-share:%' OR key LIKE 'skill-source:%'",[],|row| row.get::<_,i64>(0)).unwrap(),0);
    for key in [
        "artifact:forbidden",
        "artifact-share:forbidden",
        "skill-source:forbidden",
    ] {
        assert!(
            db.execute("INSERT INTO kv VALUES(?,'{}',NULL)", [key])
                .is_err()
        );
    }
    assert_eq!(
        db.query_row(
            "SELECT data FROM kv WHERE key='unrelated-a16-proof'",
            [],
            |row| row.get::<_, String>(0)
        )
        .unwrap(),
        "{\"unchanged\":true}"
    );
    drop(store);
    let reopened = Store::open(root.path()).unwrap();
    assert_eq!(reopened.artifacts(Some(RUN)).await.unwrap().len(), 3);
}

#[test]
fn invalid_previous_metadata_leaves_schema_and_every_row_intact() {
    for corruption in [
        "UPDATE kv SET data='invalid' WHERE key LIKE 'artifact:%'",
        "UPDATE kv SET expires=1 WHERE key LIKE 'skill-source:%'",
        "DELETE FROM kv WHERE key LIKE 'artifact-share:%'",
        "INSERT INTO kv VALUES('artifact-share:00000000-0000-4000-8000-000000000e02','{\"runId\":\"missing\",\"id\":\"missing\"}',NULL)",
        "UPDATE kv SET data=json_set(data,'$.size',536870913) WHERE key LIKE 'artifact:%'",
        "UPDATE kv SET data=json_set(data,'$.version',0) WHERE key LIKE 'artifact:%'",
        "UPDATE kv SET data=json_set(data,'$.runId','00000000-0000-4000-8000-000000000bad') WHERE key LIKE 'artifact:%'",
    ] {
        let root = tempfile::tempdir().unwrap();
        let db = previous(root.path());
        db.execute_batch(corruption).unwrap();
        let before = snapshot(&db);
        assert!(Store::open(root.path()).is_err(), "{corruption}");
        assert_eq!(snapshot(&db), before, "{corruption}");
        assert_eq!(
            db.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            18
        );
    }
}
