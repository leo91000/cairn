//! Opt-in local SQLite qualification; no timing assertion and no network traffic.
//! The reference is the records writer/read mapping at 40349271 (#394).
use cairn_contracts::{Project, ProjectId, ProjectSettings, ProjectSource};
use cairn_installation::{
    config::now,
    store::{Db, Store},
};
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use std::{path::Path, time::Instant};

fn cpu() -> i64 {
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::uninit();
    unsafe {
        assert_eq!(libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()), 0);
        let usage = usage.assume_init();
        (usage.ru_utime.tv_sec + usage.ru_stime.tv_sec) * 1_000_000
            + usage.ru_utime.tv_usec
            + usage.ru_stime.tv_usec
    }
}

fn connection(path: &Path) -> Connection {
    let db = Connection::open(path).unwrap();
    db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")
        .unwrap();
    db.set_prepared_statement_cache_capacity(128);
    db
}

fn read(db: &Connection, reference: bool, id: &str) -> Project {
    if reference {
        let raw: String = db
            .prepare_cached("SELECT data FROM records WHERE kind=? AND id=?")
            .unwrap()
            .query_row(params!["projects", id], |r| r.get(0))
            .unwrap();
        serde_json::from_str(&raw).unwrap()
    } else {
        Db(db).project(id).unwrap().unwrap()
    }
}

fn write(db: &Connection, reference: bool, project: &Project, replacement: bool) {
    let tx =
        rusqlite::Transaction::new_unchecked(db, rusqlite::TransactionBehavior::Immediate).unwrap();
    if reference {
        let value = serde_json::to_value(project).unwrap();
        tx.prepare_cached("INSERT INTO records VALUES(?,?,?,?) ON CONFLICT(id) DO UPDATE SET data=excluded.data,updated_at=excluded.updated_at").unwrap()
            .execute(params![project.id.to_string(), "projects", value.to_string(), now()]).unwrap();
        Db(&tx)
            .audit("project.saved", &json!({ "id": project.id }))
            .unwrap();
    } else {
        Db(&tx)
            .save_project(project, replacement.then_some(project.revision.as_str()))
            .unwrap();
    }
    tx.commit().unwrap();
}

fn distribution(samples: &[u128]) -> Value {
    let mut sorted = samples.to_vec();
    sorted.sort_unstable();
    json!({ "p50Us": sorted[sorted.len()/2] as f64 / 1000.0, "p95Us": sorted[(sorted.len()*95/100).min(sorted.len()-1)] as f64 / 1000.0 })
}

fn measured<T>(samples: &mut Vec<u128>, f: impl FnOnce() -> T) -> T {
    let start = Instant::now();
    let output = f();
    samples.push(start.elapsed().as_nanos());
    output
}

#[tokio::test]
#[ignore = "local measurement; run alone with --ignored --nocapture --test-threads=1"]
async fn project_storage_layouts() {
    let projects: Vec<_> = (0..1000)
        .map(|n| Project {
            id: ProjectId(uuid::Uuid::from_u128((4_u128 << 76) | (2_u128 << 62) | n)),
            settings: ProjectSettings {
                name: format!("Project {n:04}"),
                path: format!("/workspace/project-{n:04}"),
                source_mode: "remote".into(),
                base_branch: "main".into(),
                description: "Deterministic project storage qualification. ".repeat(8),
            },
            origin: format!("forge.example:team/project-{n:04}.git"),
            source: (n % 2 == 0).then(|| ProjectSource {
                plugin_id: "forge".into(),
                source: "repositories".into(),
                instance: "00000000-0000-4000-8000-000000000001".into(),
                id: format!("team/project-{n:04}"),
            }),
            revision: "00000000-0000-4000-8000-000000000002".into(),
            created_at: 1700000000000,
        })
        .collect();
    // Alternate order across repetitions to expose shared-host variation.
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
                db.execute_batch("DROP TABLE projects; DROP TABLE records;
CREATE TABLE records(id TEXT PRIMARY KEY,kind TEXT NOT NULL,data TEXT NOT NULL,updated_at INTEGER NOT NULL);
CREATE INDEX records_kind ON records(kind,updated_at DESC);
CREATE INDEX records_node_backups_run ON records(json_extract(data,'$.runId')) WHERE kind='node-backups';
CREATE UNIQUE INDEX records_chat_slug ON records(json_extract(data,'$.slug')) WHERE kind='chats';").unwrap();
            }
            let mut insert = Vec::new();
            let cpu_start = cpu();
            for project in &projects {
                measured(&mut insert, || write(&db, reference, project, false));
            }
            let insert_cpu = cpu() - cpu_start;
            let mut warm = Vec::new();
            // Two complete warmups; page cache and SQLite prepared statements are warm.
            for _ in 0..2 {
                for p in &projects {
                    assert_eq!(read(&db, reference, &p.id.to_string()), *p);
                }
            }
            let cpu_start = cpu();
            for p in &projects {
                let result = measured(&mut warm, || read(&db, reference, &p.id.to_string()));
                assert_eq!(result, *p);
            }
            let warm_cpu = cpu() - cpu_start;
            let mut list = Vec::new();
            let cpu_start = cpu();
            for _ in 0..50 {
                let count = measured(&mut list, || {
                    if reference {
                        Db(&db).list("projects").unwrap().len()
                    } else {
                        Db(&db).projects().unwrap().len()
                    }
                });
                assert_eq!(count, projects.len());
            }
            let list_cpu = cpu() - cpu_start;
            let mut replace = Vec::new();
            let cpu_start = cpu();
            for p in &projects {
                measured(&mut replace, || write(&db, reference, p, true));
            }
            let replace_cpu = cpu() - cpu_start;
            let wal =
                std::fs::metadata(path.with_file_name("manager.db-wal")).map_or(0, |m| m.len());
            db.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
                .unwrap();
            let db_bytes = std::fs::metadata(&path).unwrap().len();
            let pages: Vec<(String, i64)> = db
                .prepare("SELECT name,sum(pgsize) FROM dbstat GROUP BY name ORDER BY name")
                .unwrap()
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
                .unwrap()
                .collect::<rusqlite::Result<_>>()
                .unwrap();
            let mut cold = Vec::new();
            let cpu_start = cpu();
            for p in projects.iter().take(100) {
                let reopened = connection(&path);
                let result = measured(&mut cold, || read(&reopened, reference, &p.id.to_string()));
                assert_eq!(result, *p);
            }
            let cold_cpu = cpu() - cpu_start;
            let mut delete = Vec::new();
            let cpu_start = cpu();
            for p in &projects {
                measured(&mut delete, || {
                    let tx = rusqlite::Transaction::new_unchecked(
                        &db,
                        rusqlite::TransactionBehavior::Immediate,
                    )
                    .unwrap();
                    let sql = if reference {
                        "DELETE FROM records WHERE kind='projects' AND id=?"
                    } else {
                        "DELETE FROM projects WHERE id=?"
                    };
                    tx.prepare_cached(sql)
                        .unwrap()
                        .execute([p.id.to_string()])
                        .unwrap();
                    Db(&tx)
                        .audit("projects.deleted", &json!({"id":p.id}))
                        .unwrap();
                    tx.commit().unwrap();
                });
            }
            let delete_cpu = cpu() - cpu_start;
            println!(
                "PROJECT_STORAGE {}",
                json!({
                    "reference":reference,"repetition":repetition,"rows":projects.len(),
                    "wireBytes":projects.iter().map(|p|serde_json::to_vec(p).unwrap().len()).sum::<usize>(),
                    "dbBytes":db_bytes,"walBeforeCheckpointBytes":wal,"pages":pages,
                    "insert":distribution(&insert),"replace":distribution(&replace),"warmRead":distribution(&warm),"connectionColdRead":distribution(&cold),"list1000":distribution(&list),"delete":distribution(&delete),
                    "cpuUs":{"insert":insert_cpu,"replace":replace_cpu,"warmRead":warm_cpu,"coldReadIncludingOpen":cold_cpu,"list":list_cpu,"delete":delete_cpu},
                })
            );
        }
    }
}
