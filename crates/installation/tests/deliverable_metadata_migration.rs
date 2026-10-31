//! Frozen by the real v21 Store (f836a3df), retaining metadata from the preceding fixture.
use cairn_installation::store::Store;
use rusqlite::{Connection, types::Value as SqlValue};
use serde_json::Value;
use std::collections::BTreeMap;

const PREVIOUS: &str = include_str!("fixtures/deliverable-metadata-v21.sql");
const RUN: &str = "00000000-0000-4000-8000-000000000010";
const TOKEN: &str = "00000000-0000-4000-8000-000000000e01";

fn previous(path: &std::path::Path) -> Connection {
    let db = Connection::open(path.join("manager.db")).unwrap();
    db.execute_batch(PREVIOUS).unwrap();
    db
}

type Snapshot = BTreeMap<String, (String, Vec<Vec<SqlValue>>)>;

fn schema(db: &Connection) -> BTreeMap<(String, String), String> {
    db.prepare("SELECT type,name,sql FROM sqlite_schema WHERE sql IS NOT NULL ORDER BY type,name")
        .unwrap()
        .query_map([], |row| Ok(((row.get(0)?, row.get(1)?), row.get(2)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

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
    db.prepare("SELECT key,data FROM kv WHERE key GLOB 'artifact:*' OR key GLOB 'skill-source:*'")
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
    // Domain namespaces are case-sensitive; opaque KV keys that merely resemble
    // them must survive instead of being deleted by SQLite's case-insensitive LIKE.
    let opaque_keys = [
        "Artifact:opaque",
        "Artifact-share:opaque",
        "Skill-source:opaque",
    ];
    for key in opaque_keys {
        db.execute(
            "INSERT INTO kv VALUES(?,'{\"unrelated\":true}',NULL)",
            [key],
        )
        .unwrap();
    }
    db.execute("INSERT INTO kv VALUES(NULL,'{\"nullKey\":true}',NULL)", [])
        .unwrap();
    let before = snapshot(&db);
    let before_schema = schema(&db);
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
    let mut duplicate_link = versions
        .iter()
        .find(|version| version.public_token.is_none())
        .unwrap()
        .clone();
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
    let public = public.unwrap();
    assert_eq!(public.public_token.as_deref(), Some(TOKEN));
    duplicate_link.visibility = Some("public".to_owned());
    duplicate_link.public_token = public.public_token.clone();
    duplicate_link.public_url = public.public_url.clone();
    assert!(
        store
            .transaction(move |db| db.save_artifact_visibility(&duplicate_link))
            .await
            .is_err()
    );
    assert_eq!(
        store
            .read(|db| Ok(db.public_artifact(TOKEN)?.unwrap().id))
            .await
            .unwrap(),
        public.id
    );
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
        22
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
    let after_schema = schema(&db);
    for ((kind, name), definition) in before_schema {
        if kind != "table" || name != "kv" {
            assert_eq!(
                after_schema.get(&(kind, name)),
                Some(&definition),
                "Unrelated schema objects must remain unchanged"
            );
        }
    }
    for (table, (sql, rows)) in before {
        if table != "kv" {
            assert_eq!(after[&table], (sql, rows), "{table}");
        }
    }
    assert_eq!(db.query_row("SELECT count(*) FROM kv WHERE key GLOB 'artifact:*' OR key GLOB 'artifact-share:*' OR key GLOB 'skill-source:*'",[],|row| row.get::<_,i64>(0)).unwrap(),0);
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
    for key in opaque_keys {
        assert_eq!(
            db.query_row("SELECT data FROM kv WHERE key=?", [key], |row| row
                .get::<_, String>(0))
                .unwrap(),
            "{\"unrelated\":true}"
        );
    }
    assert_eq!(
        db.query_row("SELECT data FROM kv WHERE key IS NULL", [], |row| row
            .get::<_, String>(0))
            .unwrap(),
        "{\"nullKey\":true}"
    );
    drop(store);
    let reopened = Store::open(root.path()).unwrap();
    assert_eq!(reopened.artifacts(Some(RUN)).await.unwrap().len(), 3);
}

#[test]
fn invalid_previous_metadata_leaves_schema_and_every_row_intact() {
    for corruption in [
        "UPDATE kv SET data='invalid' WHERE key GLOB 'artifact:*'",
        "UPDATE kv SET expires=1 WHERE key GLOB 'skill-source:*'",
        "DELETE FROM kv WHERE key GLOB 'artifact-share:*'",
        "INSERT INTO kv VALUES('artifact-share:00000000-0000-4000-8000-000000000e02','{\"runId\":\"missing\",\"id\":\"missing\"}',NULL)",
        "UPDATE kv SET data=json_set(data,'$.size',536870913) WHERE key GLOB 'artifact:*'",
        "UPDATE kv SET data=json_set(data,'$.version',0) WHERE key GLOB 'artifact:*'",
        "UPDATE kv SET data=json_set(data,'$.runId','00000000-0000-4000-8000-000000000bad') WHERE key GLOB 'artifact:*'",
    ] {
        let root = tempfile::tempdir().unwrap();
        let db = previous(root.path());
        db.execute_batch(corruption).unwrap();
        let before = snapshot(&db);
        let before_schema = schema(&db);
        assert!(Store::open(root.path()).is_err(), "{corruption}");
        assert_eq!(snapshot(&db), before, "{corruption}");
        assert_eq!(schema(&db), before_schema, "{corruption}");
        assert_eq!(
            db.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            21
        );
    }
}
