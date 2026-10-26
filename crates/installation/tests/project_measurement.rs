//! Opt-in local SQLite qualification; no timing assertion and no network traffic.
//! The reference is the records writer/read mapping at 40349271 (#394).
use cairn_contracts::{Agent, AgentId, Project, ProjectId, ProjectSettings, ProjectSource};
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

// The A1 workload stays shared: each converted domain supplies only its typed SQL
// mapping. This is a local layout comparison, not a capability/transport benchmark.
trait StorageEntity:
    serde::Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug
{
    const KIND: &'static str;
    const TABLE: &'static str = Self::KIND;

    fn prepare(_db: &Db<'_>) {}

    fn deadlines(_db: &Db<'_>, _reference: bool) -> Option<usize> {
        None
    }

    const SAVE: &'static str;

    fn id(&self) -> String;

    fn read(db: &Db<'_>, id: &str) -> Self;

    fn list(db: &Db<'_>) -> usize;

    fn write(&self, db: &Db<'_>, replacement: bool);

    fn reference_schema(db: &Connection) {
        db.execute_batch(&format!("DROP TABLE {}; DROP TABLE records;
CREATE TABLE records(id TEXT PRIMARY KEY,kind TEXT NOT NULL,data TEXT NOT NULL,updated_at INTEGER NOT NULL);
CREATE INDEX records_kind ON records(kind,updated_at DESC);
CREATE INDEX records_node_backups_run ON records(json_extract(data,'$.runId')) WHERE kind='node-backups';
CREATE UNIQUE INDEX records_chat_slug ON records(json_extract(data,'$.slug')) WHERE kind='chats';", Self::TABLE)).unwrap();
    }

    fn reference_read(db: &Db<'_>, id: &str) -> Self {
        db.get_as(Self::KIND, id).unwrap().unwrap()
    }

    fn reference_list(db: &Db<'_>) -> usize {
        db.list(Self::KIND).unwrap().len()
    }

    fn reference_write(&self, db: &Db<'_>, _replacement: bool) {
        db.0.prepare_cached("INSERT INTO records VALUES(?,?,?,?) ON CONFLICT(id) DO UPDATE SET data=excluded.data,updated_at=excluded.updated_at").unwrap().execute(params![self.id(), Self::KIND, serde_json::to_string(self).unwrap(), now()]).unwrap();
    }

    fn reference_delete(db: &Db<'_>, id: &str) {
        db.0.prepare_cached("DELETE FROM records WHERE kind=? AND id=?")
            .unwrap()
            .execute(params![Self::KIND, id])
            .unwrap();
    }
}

impl StorageEntity for Project {
    const KIND: &'static str = "projects";
    const SAVE: &'static str = "project.saved";

    fn id(&self) -> String {
        self.id.to_string()
    }

    fn read(db: &Db<'_>, id: &str) -> Self {
        db.project(id).unwrap().unwrap()
    }

    fn list(db: &Db<'_>) -> usize {
        db.projects().unwrap().len()
    }

    fn write(&self, db: &Db<'_>, replacement: bool) {
        db.save_project(self, replacement.then_some(self.revision.as_str()))
            .unwrap();
    }
}

impl StorageEntity for Agent {
    const KIND: &'static str = "agents";
    const SAVE: &'static str = "agent.saved";

    fn id(&self) -> String {
        self.id.to_string()
    }

    fn read(db: &Db<'_>, id: &str) -> Self {
        db.agent(id).unwrap().unwrap()
    }

    fn list(db: &Db<'_>) -> usize {
        db.agents().unwrap().len()
    }

    fn write(&self, db: &Db<'_>, replacement: bool) {
        if replacement {
            assert_eq!(
                db.agent(&self.id()).unwrap().unwrap().revision,
                self.revision
            );
        }
        db.save_agent(self).unwrap();
        db.audit(Self::SAVE, &json!({ "id": self.id })).unwrap();
    }

    fn reference_schema(db: &Connection) {
        db.execute_batch("DROP TABLE agent_mcp_tools; DROP TABLE agent_mcp_selections; DROP TABLE agent_projects; DROP TABLE agent_skills; DROP TABLE agent_nodes; DROP TABLE agent_mcps; DROP TABLE agent_plugins; DROP TABLE agent_exclusive_nodes; DROP TABLE agent_permissions; DROP TABLE agents;").unwrap();
        db.execute_batch(a2::SCHEMA).unwrap();
    }

    fn reference_read(db: &Db<'_>, id: &str) -> Self {
        db.a2_agent(id).unwrap().unwrap()
    }

    fn reference_list(db: &Db<'_>) -> usize {
        db.a2_agents().unwrap().len()
    }

    fn reference_write(&self, db: &Db<'_>, replacement: bool) {
        if replacement {
            assert_eq!(
                db.a2_agent(&self.id()).unwrap().unwrap().revision,
                self.revision
            );
        }
        db.a2_save_agent(self).unwrap();
    }

    fn reference_delete(db: &Db<'_>, id: &str) {
        db.0.prepare_cached("DELETE FROM agents WHERE id=?")
            .unwrap()
            .execute([id])
            .unwrap();
    }
}

#[path = "project_measurement/a2.rs"]
mod a2;

use a2::A2Mapping;

impl StorageEntity for cairn_contracts::Mission {
    const KIND: &'static str = "tasks";
    const TABLE: &'static str = "missions";
    const SAVE: &'static str = "task.saved";

    fn id(&self) -> String {
        self.id.to_string()
    }

    fn read(db: &Db<'_>, id: &str) -> Self {
        db.mission(id).unwrap().unwrap()
    }

    fn list(db: &Db<'_>) -> usize {
        db.missions().unwrap().len()
    }

    fn prepare(db: &Db<'_>) {
        let agent:Agent=serde_json::from_value(json!({
            "id":cairn_installation::config::MAIN_AGENT_ID,"name":"Measurement agent","access":cairn_contracts::AccessPolicy::default(),
            "exclusiveNodes":[],"plugins":[],"revision":"measurement","createdAt":0
        })).unwrap();
        db.save_agent(&agent).unwrap();
    }

    fn write(&self, db: &Db<'_>, replacement: bool) {
        if replacement {
            assert_eq!(
                db.mission(&self.id()).unwrap().unwrap().revision,
                self.revision
            );
        }
        db.save_mission(self).unwrap();
        db.audit(Self::SAVE, &json!({"id":self.id})).unwrap();
    }

    fn deadlines(db: &Db<'_>, reference: bool) -> Option<usize> {
        Some(if reference {
            db.list("tasks")
                .unwrap()
                .into_iter()
                .filter(|mission| {
                    mission["enabled"] == true
                        && mission["archived"] != true
                        && !mission["cron"].is_null()
                        && mission["nextRun"]
                            .as_i64()
                            .is_some_and(|at| at <= 1700000000000)
                })
                .count()
        } else {
            db.due_missions(1700000000000).unwrap().len()
        })
    }
}

#[tokio::test]
#[ignore = "local measurement; run alone with --ignored --nocapture --test-threads=1"]
async fn mission_storage_layouts() {
    let corpus:Vec<cairn_contracts::Mission>=(0..1000).map(|n|serde_json::from_value(json!({
        "id":uuid::Uuid::from_u128((4_u128<<76)|(2_u128<<62)|n),
        "name":format!("Mission {n:04}"),"prompt":"Inspect changes and preserve scheduled admission. ".repeat(24),
        "agentId":cairn_installation::config::MAIN_AGENT_ID,"projectId":null,
        "skills":if n%3==0 { None } else if n%3==1 { Some(vec![]) } else { Some(vec!["global/review","global/test"]) },
        "tags":["review","code"],"cron":if n%2==0 { Some("0 9 * * *") } else { None },
        "timezone":"Europe/Paris","enabled":true,"archived":false,"worktree":true,
        "createdAt":1700000000000_i64,"nextRun":if n%2==0 { Some(1699999999000_i64+n as i64) } else { None },
        "revision":"00000000-0000-4000-8000-000000000002","authorId":"measurement-owner","authorAccessId":"measurement-membership","authorRemoved":false,
        "scheduleFailure":null,"scheduleWaitReason":null,
        "nativePrincipals":["measurement-owner"],"nativeAuthorityNodes":[],"nativeParentRunId":null,"nativeParentAccess":null,"nativeProvenanceUnknown":false,"nativePluginGrants":null,
    })).unwrap()).collect();
    measure(&corpus);
}

fn read<E: StorageEntity>(db: &Connection, reference: bool, id: &str) -> E {
    if reference {
        E::reference_read(&Db(db), id)
    } else {
        E::read(&Db(db), id)
    }
}

fn write<E: StorageEntity>(db: &Connection, reference: bool, entity: &E, replacement: bool) {
    let tx =
        rusqlite::Transaction::new_unchecked(db, rusqlite::TransactionBehavior::Immediate).unwrap();
    if reference {
        entity.reference_write(&Db(&tx), replacement);
        Db(&tx)
            .audit(E::SAVE, &json!({ "id": entity.id() }))
            .unwrap();
    } else {
        entity.write(&Db(&tx), replacement);
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
    measure(&projects);
}

#[tokio::test]
#[ignore = "local measurement; run alone with --ignored --nocapture --test-threads=1"]
async fn agent_storage_layouts() {
    let agents: Vec<Agent> = (0..1000).map(|n| Agent {
        id: AgentId(uuid::Uuid::from_u128((4_u128 << 76) | (2_u128 << 62) | n)),
        settings: serde_json::from_value(json!({
            "name": format!("Agent {n:04}"),
            "provider": (["codex", "claude", "opencode"][n as usize % 3]),
            "model": "provider/model",
            "reasoning": "high",
            "speed": if n % 6 == 0 { "priority" } else { "" },
            "description": "Deterministic agent storage qualification. ".repeat(8),
            "instructions": "Inspect the current repository and preserve authorization. ".repeat(20),
            "access": {
                "backend": "firecracker",
                "nodes": if n % 3 == 0 { json!(null) } else { json!(["00000000-0000-4000-8000-000000000002"]) },
                "projects": null,
                "skills": if n % 2 == 0 { json!(["global/review", "global/build"]) } else { json!([]) },
                "mcps": if n % 3 == 0 { json!(null) } else { json!(["00000000-0000-4000-8000-000000000003"]) },
                "mcpTools": { "00000000-0000-4000-8000-000000000003": if n % 2 == 0 { vec!["read", "search"] } else { vec![] } },
                "installation": false,
                "delegation": false,
                "sandbox": "yolo",
            },
            "exclusiveNodes": ["00000000-0000-4000-8000-000000000002"],
            "plugins": ["00000000-0000-4000-8000-000000000004"],
        })).unwrap(),
        revision: "00000000-0000-4000-8000-000000000002".into(),
        created_at: 1700000000000,
        avatar: (n % 2 == 0).then(|| cairn_contracts::AgentAvatar { status: "ready".into(), revision: "portrait".into(), url: Some(format!("/api/agents/{}/avatar?v=portrait", uuid::Uuid::from_u128((4_u128 << 76) | (2_u128 << 62) | n))), error: None }),
    }).collect();
    measure(&agents);
}

fn measure<E: StorageEntity>(entities: &[E]) {
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
                E::reference_schema(&db);
            }
            E::prepare(&Db(&db));
            let mut insert = Vec::new();
            let cpu_start = cpu();
            for project in entities {
                measured(&mut insert, || write(&db, reference, project, false));
            }
            let insert_cpu = cpu() - cpu_start;
            let mut warm = Vec::new();
            // Two complete warmups; page cache and SQLite prepared statements are warm.
            for _ in 0..2 {
                for p in entities {
                    assert_eq!(read::<E>(&db, reference, &p.id()), *p);
                }
            }
            let cpu_start = cpu();
            for p in entities {
                let result = measured(&mut warm, || read::<E>(&db, reference, &p.id()));
                assert_eq!(result, *p);
            }
            let warm_cpu = cpu() - cpu_start;
            let mut list = Vec::new();
            let cpu_start = cpu();
            for _ in 0..50 {
                let count = measured(&mut list, || {
                    if reference {
                        E::reference_list(&Db(&db))
                    } else {
                        E::list(&Db(&db))
                    }
                });
                assert_eq!(count, entities.len());
            }
            let list_cpu = cpu() - cpu_start;
            let mut due = Vec::new();
            let cpu_start = cpu();
            if E::deadlines(&Db(&db), reference).is_some() {
                for _ in 0..50 {
                    let count = measured(&mut due, || E::deadlines(&Db(&db), reference).unwrap());
                    assert_eq!(count, 500);
                }
            }
            let due_cpu = cpu() - cpu_start;
            let mut replace = Vec::new();
            let cpu_start = cpu();
            for p in entities {
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
            for p in entities.iter().take(100) {
                let reopened = connection(&path);
                let result = measured(&mut cold, || read::<E>(&reopened, reference, &p.id()));
                assert_eq!(result, *p);
            }
            let cold_cpu = cpu() - cpu_start;
            let mut delete = Vec::new();
            let cpu_start = cpu();
            for p in entities {
                measured(&mut delete, || {
                    let tx = rusqlite::Transaction::new_unchecked(
                        &db,
                        rusqlite::TransactionBehavior::Immediate,
                    )
                    .unwrap();
                    if reference {
                        E::reference_delete(&Db(&tx), &p.id());
                    } else {
                        tx.prepare_cached(&format!("DELETE FROM {} WHERE id=?", E::TABLE))
                            .unwrap()
                            .execute([p.id()])
                            .unwrap();
                    }
                    Db(&tx)
                        .audit(&format!("{}.deleted", E::KIND), &json!({ "id": p.id() }))
                        .unwrap();
                    tx.commit().unwrap();
                });
            }
            let delete_cpu = cpu() - cpu_start;
            println!(
                "DOMAIN_STORAGE {}",
                json!({
                    "domain": E::KIND,
                    "reference": reference,
                    "repetition": repetition,
                    "rows": entities.len(),
                    "wireBytes": entities.iter().map(|p| serde_json::to_vec(p).unwrap().len()).sum::<usize>(),
                    "dbBytes": db_bytes,
                    "walBeforeCheckpointBytes": wal,
                    "pages": pages,
                    "insert": distribution(&insert),
                    "replace": distribution(&replace),
                    "warmRead": distribution(&warm),
                    "connectionColdRead": distribution(&cold),
                    "list1000": distribution(&list),
                    "due500": if due.is_empty(){None}else{Some(distribution(&due))},
                    "delete": distribution(&delete),
                    "cpuUs": {
                        "insert": insert_cpu,
                        "replace": replace_cpu,
                        "warmRead": warm_cpu,
                        "coldReadIncludingOpen": cold_cpu,
                        "list": list_cpu,
                        "due": due_cpu,
                        "delete": delete_cpu,
                    },
                })
            );
        }
    }
}
