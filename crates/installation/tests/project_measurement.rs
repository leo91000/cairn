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

    fn delete_candidate(db: &Db<'_>, id: &str) {
        db.0.prepare_cached(&format!("DELETE FROM {} WHERE id=?", Self::TABLE))
            .unwrap()
            .execute([id])
            .unwrap();
    }

    fn deadlines(_db: &Db<'_>, _reference: bool) -> Option<usize> {
        None
    }

    const SAVE: &'static str;

    fn id(&self) -> String;

    fn setup(_db: &Connection) {}

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
    fn setup(db: &Connection) {
        Db(db)
            .save_mcp_server(&cairn_contracts::McpServer {
                id: "00000000-0000-4000-8000-000000000003".into(),
                name: "Measurement MCP".into(),
                url: "https://example.test/mcp".into(),
                ..Default::default()
            })
            .unwrap();
    }

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

    fn prepare(db: &Db<'_>) {
        let mut instance = cairn_installation::plugins::connections::Connection::draft(
            "00000000-0000-4000-8000-000000000004",
            "fixture",
            json!({}),
            Vec::new(),
        );
        instance.name = "Qualification".into();
        instance.revision = "r1".into();
        db.save_plugin_instance(&instance).unwrap();
    }

    fn reference_schema(db: &Connection) {
        db.execute_batch("DROP TABLE agent_mcp_tools; DROP TABLE agent_mcp_selections; DROP TABLE agent_projects; DROP TABLE agent_skills; DROP TABLE agent_nodes; DROP TABLE agent_mcps; DROP TABLE instance_grants; DROP TABLE agent_exclusive_nodes; DROP TABLE agent_permissions; DROP TABLE agents;").unwrap();
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

use cairn_installation::plugins::connections::Connection as PluginInstance;

impl StorageEntity for PluginInstance {
    const KIND: &'static str = "plugin_instances";
    const SAVE: &'static str = "plugin.connection.saved";

    fn id(&self) -> String {
        self.id.clone()
    }

    fn prepare(db: &Db<'_>) {
        let tx =
            rusqlite::Transaction::new_unchecked(db.0, rusqlite::TransactionBehavior::Immediate)
                .unwrap();
        for n in 0..1000 {
            Db(&tx)
                .save_agent(&Agent {
                    id: AgentId(uuid::Uuid::from_u128(
                        (4_u128 << 76) | (2_u128 << 62) | (n + 10000),
                    )),
                    settings: serde_json::from_value(json!({
                        "name": format!("Agent {n}"),
                        "access": cairn_contracts::AccessPolicy::default(),
                        "exclusiveNodes": [],
                        "plugins": [],
                    }))
                    .unwrap(),
                    revision: "fixture".into(),
                    created_at: 1700000000000,
                    avatar: None,
                })
                .unwrap();
        }
        tx.commit().unwrap();
    }

    fn read(db: &Db<'_>, id: &str) -> Self {
        db.plugin_instance(id).unwrap().unwrap()
    }

    fn list(db: &Db<'_>) -> usize {
        db.plugin_instances().unwrap().len()
    }

    fn write(&self, db: &Db<'_>, replacement: bool) {
        if replacement {
            assert_eq!(
                db.plugin_instance(&self.id).unwrap().unwrap().revision,
                self.revision
            );
        }
        db.save_plugin_instance(self).unwrap();
        db.0.prepare_cached("INSERT INTO instance_grants VALUES(?,?,0) ON CONFLICT DO NOTHING")
            .unwrap()
            .execute(params![self.agent_ids[0], self.id])
            .unwrap();
        db.audit(Self::SAVE, &json!({ "id": self.id })).unwrap();
    }

    fn delete_candidate(db: &Db<'_>, id: &str) {
        db.0.prepare_cached("DELETE FROM instance_grants WHERE instance_id=?")
            .unwrap()
            .execute([id])
            .unwrap();
        db.remove_plugin_instance(id).unwrap();
    }

    // Frozen A3 layout at 6653951f: grant authority is already relational; only
    // connection documents, delegated people and policy envelopes move in A12.
    fn reference_schema(db: &Connection) {
        db.execute_batch("DROP TABLE instance_grants; DROP TABLE instance_people; DROP TABLE external_delegations; DROP TABLE plugin_instances; DROP TABLE records;
CREATE TABLE records(id TEXT PRIMARY KEY,kind TEXT NOT NULL,data TEXT NOT NULL,updated_at INTEGER NOT NULL);
CREATE INDEX records_kind ON records(kind,updated_at DESC);
CREATE INDEX records_node_backups_run ON records(json_extract(data,'$.runId')) WHERE kind='node-backups';
CREATE UNIQUE INDEX records_chat_slug ON records(json_extract(data,'$.slug')) WHERE kind='chats';
CREATE TABLE agent_plugins(agent_id TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,instance_id TEXT NOT NULL CHECK(length(instance_id)=36),position INTEGER NOT NULL CHECK(position>=0),PRIMARY KEY(agent_id,instance_id),UNIQUE(agent_id,position)) WITHOUT ROWID;
CREATE INDEX agent_plugins_resource ON agent_plugins(instance_id,agent_id);").unwrap();
    }

    fn reference_read(db: &Db<'_>, id: &str) -> Self {
        if db.0.is_autocommit() {
            let tx =
                rusqlite::Transaction::new_unchecked(db.0, rusqlite::TransactionBehavior::Deferred)
                    .unwrap();
            return Self::reference_read(&Db(&tx), id);
        }
        let text: String = db
            .0
            .prepare_cached("SELECT data FROM records WHERE kind='plugin-connections' AND id=?")
            .unwrap()
            .query_row([id], |row| row.get(0))
            .unwrap();
        let mut value: Value = serde_json::from_str(&text).unwrap();
        let agents =
            db.0.prepare_cached(
                "SELECT agent_id FROM agent_plugins WHERE instance_id=? ORDER BY agent_id",
            )
            .unwrap()
            .query_map([id], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        value["agentIds"] = json!(agents);
        serde_json::from_value(value).unwrap()
    }

    fn reference_list(db: &Db<'_>) -> usize {
        if db.0.is_autocommit() {
            let tx =
                rusqlite::Transaction::new_unchecked(db.0, rusqlite::TransactionBehavior::Deferred)
                    .unwrap();
            return Self::reference_list(&Db(&tx));
        }
        let mut values = db
            .json_rows(
                "SELECT data FROM records WHERE kind='plugin-connections' ORDER BY updated_at DESC",
                [],
            )
            .unwrap();
        for value in &mut values {
            let agents =
                db.0.prepare_cached(
                    "SELECT agent_id FROM agent_plugins WHERE instance_id=? ORDER BY agent_id",
                )
                .unwrap()
                .query_map([value["id"].as_str().unwrap()], |row| {
                    row.get::<_, String>(0)
                })
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap();
            value["agentIds"] = json!(agents);
        }
        values.len()
    }

    fn reference_write(&self, db: &Db<'_>, replacement: bool) {
        if replacement {
            assert_eq!(Self::reference_read(db, &self.id).revision, self.revision);
        }
        let mut value = serde_json::to_value(self).unwrap();
        value.as_object_mut().unwrap().remove("agentIds");
        db.0.prepare_cached("INSERT INTO records VALUES(?,'plugin-connections',?,?) ON CONFLICT(id) DO UPDATE SET data=excluded.data,updated_at=excluded.updated_at").unwrap().execute(params![self.id,serde_json::to_string(&value).unwrap(),self.updated_at]).unwrap();
        db.0.prepare_cached("INSERT INTO agent_plugins VALUES(?,?,0) ON CONFLICT DO NOTHING")
            .unwrap()
            .execute(params![self.agent_ids[0], self.id])
            .unwrap();
    }

    fn reference_delete(db: &Db<'_>, id: &str) {
        db.0.prepare_cached("DELETE FROM agent_plugins WHERE instance_id=?")
            .unwrap()
            .execute([id])
            .unwrap();
        db.0.prepare_cached("DELETE FROM records WHERE kind='plugin-connections' AND id=?")
            .unwrap()
            .execute([id])
            .unwrap();
    }
}

#[tokio::test]
#[ignore = "local measurement; run alone with --ignored --nocapture --test-threads=1"]
async fn plugin_storage_layouts() {
    let instances: Vec<_> = (0..1000)
        .map(|n| {
            let instance_id =
                uuid::Uuid::from_u128((4_u128 << 76) | (2_u128 << 62) | n).to_string();
            let agent_id =
                uuid::Uuid::from_u128((4_u128 << 76) | (2_u128 << 62) | (n + 10000)).to_string();
            let mut instance = PluginInstance::draft(
                &instance_id,
                "fixture",
                json!({
                    "channel": format!("channel-{n}"),
                    "description": "Deterministic instance storage qualification. ".repeat(8),
                    "scope": true,
                    "audiences": ["engineering","support"],
                }),
                vec!["clientSecret".into()],
            );
            instance.name = format!("Instance {n:04}");
            instance.revision = "revision".into();
            instance.enabled = n % 3 != 0;
            instance.updated_at = 1700000000000;
            instance.max_messages_per_hour = 20;
            instance.agent_ids = vec![agent_id.clone()];
            if n % 3 != 0 {
                instance.agent_id = agent_id;
                instance.owner_access_id = Some("owner-access".into());
                let mut access = cairn_contracts::AccessPolicy::default();
                if n % 5 == 1 {
                    access.backend = "native".into();
                    instance.native_authority =
                        Some(cairn_installation::plugins::connections::NativeAuthority {
                            native_principals: vec!["owner-account".into()],
                            native_authority_nodes: vec![
                                "00000000-0000-4000-8000-000000000002".into(),
                            ],
                            native_provenance_unknown: false,
                            native_parent_run_id: None,
                            native_parent_access: None,
                            native_plugin_grants: None,
                            plugin_owner_account_id: "owner-account".into(),
                        });
                }
                instance.granted_access = Some(access);
                instance.access_mode = if n % 2 == 0 {
                    cairn_contracts::PluginAccessMode::Everyone
                } else {
                    cairn_contracts::PluginAccessMode::Requests
                };
                instance.user_ids = vec!["person1".into(), "person2".into()];
                instance.people = vec![
                    cairn_contracts::PluginLabel {
                        id: "person1".into(),
                        name: "First person".into(),
                    },
                    cairn_contracts::PluginLabel {
                        id: "person2".into(),
                        name: "Second person".into(),
                    },
                ];
            }
            instance
        })
        .collect();
    measure(&instances);
}

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
    json!({
        "p50Us": sorted[sorted.len()/2] as f64 / 1000.0,
        "p95Us": sorted[(sorted.len()*95/100).min(sorted.len()-1)] as f64 / 1000.0,
    })
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
            // Supporting entities use current domain writers. Seed them before
            // the reference replaces any of their grant tables.
            E::prepare(&Db(&db));
            if reference {
                E::reference_schema(&db);
            }
            E::setup(&db);
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
                        E::delete_candidate(&Db(&tx), &p.id());
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

impl StorageEntity for cairn_contracts::McpServer {
    const KIND: &'static str = "mcps";
    const TABLE: &'static str = "mcp_servers";
    const SAVE: &'static str = "mcp.saved";

    fn id(&self) -> String {
        self.id.clone()
    }

    fn read(db: &Db<'_>, id: &str) -> Self {
        db.mcp_server(id).unwrap().unwrap()
    }

    fn list(db: &Db<'_>) -> usize {
        db.mcp_servers().unwrap().len()
    }

    fn write(&self, db: &Db<'_>, replacement: bool) {
        if replacement {
            assert_eq!(
                db.mcp_server(&self.id).unwrap().unwrap().revision,
                self.revision
            );
        }
        db.save_mcp_server(self).unwrap();
        db.audit(Self::SAVE, &json!({"id":self.id})).unwrap();
    }

    // Frozen MCP mapping at 6653951f: the existing DTO was decoded from records
    // Values, not directly from SQL text. Match that entity boundary on both sides.
    fn reference_read(db: &Db<'_>, id: &str) -> Self {
        let record = db.get(Self::KIND, id).unwrap().unwrap();
        serde::Deserialize::deserialize(&record).unwrap()
    }

    fn reference_list(db: &Db<'_>) -> usize {
        let servers: Vec<Self> = db
            .list(Self::KIND)
            .unwrap()
            .iter()
            .map(|record| serde::Deserialize::deserialize(record).unwrap())
            .collect();
        servers.len()
    }

    fn reference_write(&self, db: &Db<'_>, replacement: bool) {
        // The old save reads previous revision/creation time/tools before replacing.
        if replacement {
            assert_eq!(Self::reference_read(db, &self.id).revision, self.revision);
        }
        let record = serde_json::to_value(self).unwrap();
        db.0.prepare_cached("INSERT INTO records VALUES(?,?,?,?) ON CONFLICT(id) DO UPDATE SET data=excluded.data,updated_at=excluded.updated_at").unwrap().execute(params![self.id,Self::KIND,serde_json::to_string(&record).unwrap(),now()]).unwrap();
    }

    fn reference_schema(db: &Connection) {
        db.execute_batch("DROP TABLE records;
CREATE TABLE records(id TEXT PRIMARY KEY,kind TEXT NOT NULL,data TEXT NOT NULL,updated_at INTEGER NOT NULL);
CREATE INDEX records_kind ON records(kind,updated_at DESC);
CREATE INDEX records_node_backups_run ON records(json_extract(data,'$.runId')) WHERE kind='node-backups';
CREATE UNIQUE INDEX records_chat_slug ON records(json_extract(data,'$.slug')) WHERE kind='chats';").unwrap();
    }
}

#[test]
#[ignore = "local measurement; run alone with --ignored --nocapture --test-threads=1"]
fn mcp_storage_layouts() {
    use cairn_contracts::{AuthKind, ConnectionState, McpServer, Transport};
    let servers:Vec<_>=(0..1000).map(|index|McpServer{
        id:format!("00000000-0000-4000-8000-{index:012}"),
        name:format!("Connection {index}"),transport:if index%3==0{Transport::Stdio}else{Transport::Http},
        url:format!("https://example.test/workspaces/{index}/mcp"),command:if index%3==0{"node"}else{""}.into(),
        args:if index%3==0{vec!["server.js".into(),"--workspace".into(),index.to_string(),String::new()]}else{vec![]},
        auth:if index%3==0{AuthKind::None}else{AuthKind::OAuth},client_id:format!("client-{index}"),scopes:"read write".into(),
        enabled:index%5!=0,enabled_tools:match index%3{0=>None,1=>Some(vec![]),_=>Some(vec!["echo".into(),"search".into()])},
        created_at:1_700_000_000_000,revision:1,state:ConnectionState::Connected,checked_at:Some(1_700_000_001_000),error:String::new(),allow_private_network:false,
        tools:(0..8).map(|tool|json!({
            "name":format!("tool_{tool}"),
            "description":"External tool description. ".repeat(12),
            "inputSchema":{"type":"object","properties":{"query":{"type":"string"}},"required":["query"]},
        })).collect(),
    }).collect();
    measure(&servers);
}

// A5 reuses the A1/A2 qualification loop, changing only the domain mapping.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
struct MeasuredRun(Value);

impl MeasuredRun {
    fn state(&self) -> cairn_contracts::RunState {
        serde_json::from_value(self.0.clone()).unwrap()
    }
}

impl StorageEntity for MeasuredRun {
    const KIND: &'static str = "runs";
    const SAVE: &'static str = "run.saved";

    fn id(&self) -> String {
        self.state().id
    }

    fn read(db: &Db<'_>, id: &str) -> Self {
        Self(db.run(id).unwrap().unwrap())
    }

    fn list(db: &Db<'_>) -> usize {
        db.runs(None, None, 1000, 0, false).unwrap().len()
    }

    fn write(&self, db: &Db<'_>, replacement: bool) {
        let state = self.state();
        if replacement {
            db.change_run(
                &state.id,
                cairn_installation::runs::RunChanges {
                    status: Some(state.status),
                    summary: Some(state.summary),
                    finished_at: Some(state.finished_at),
                    ..Default::default()
                },
            )
            .unwrap();
        } else {
            let snapshot: cairn_contracts::RunSnapshot =
                serde_json::from_value(self.0["snapshot"].clone()).unwrap();
            let mut boundary = self.0.clone();
            for field in cairn_installation::runs::RUN_FIELDS {
                boundary.as_object_mut().unwrap().remove(*field);
            }
            db.admit_run(&state, Some(&snapshot), &boundary, None)
                .unwrap();
        }
        db.audit(Self::SAVE, &json!({"id": state.id})).unwrap();
    }

    fn reference_schema(db: &Connection) {
        // Exact pre-A5 seven-column runs layout and indexes at 7fecc082.
        db.execute_batch("DROP TABLE runs;
CREATE TABLE runs(id TEXT PRIMARY KEY,task_id TEXT NOT NULL,project_id TEXT NOT NULL,status TEXT NOT NULL,created_at INTEGER NOT NULL,data TEXT NOT NULL,dedupe TEXT UNIQUE);
CREATE INDEX runs_status ON runs(status,created_at);
CREATE INDEX runs_task ON runs(task_id,created_at DESC);
CREATE INDEX runs_created ON runs(created_at DESC,id DESC);
CREATE UNIQUE INDEX runs_active_task ON runs(task_id) WHERE status IN ('queued','running','waiting');").unwrap();
    }

    fn reference_read(db: &Db<'_>, id: &str) -> Self {
        let data: String =
            db.0.prepare_cached("SELECT data FROM runs WHERE id=?")
                .unwrap()
                .query_row([id], |r| r.get(0))
                .unwrap();
        Self(serde_json::from_str(&data).unwrap())
    }

    fn reference_list(db: &Db<'_>) -> usize {
        let mut query = db.0.prepare_cached("SELECT json_set(json_remove(data,'$.snapshot','$.summary'),'$.taskName',json_extract(data,'$.snapshot.task.name'),'$.agentName',json_extract(data,'$.snapshot.agent.name'),'$.provider',json_extract(data,'$.snapshot.agent.provider')) FROM runs ORDER BY created_at DESC,id DESC LIMIT 1000 OFFSET 0").unwrap();
        let rows: Vec<Value> = query
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .map(|r| serde_json::from_str(&r.unwrap()).unwrap())
            .collect();
        rows.len()
    }

    fn reference_write(&self, db: &Db<'_>, replacement: bool) {
        let state = self.state();
        if replacement {
            db.0.prepare_cached("UPDATE runs SET status=?,data=? WHERE id=?")
                .unwrap()
                .execute(params![state.status.as_str(), self.0.to_string(), state.id])
                .unwrap();
        } else {
            db.0.prepare_cached("INSERT INTO runs(id,task_id,project_id,status,created_at,data,dedupe) VALUES(?,?,?,?,?,?,NULL)").unwrap().execute(params![state.id,state.task_id,state.project_id.unwrap_or_default(),state.status.as_str(),state.created_at,self.0.to_string()]).unwrap();
        }
    }

    fn reference_delete(db: &Db<'_>, id: &str) {
        db.0.prepare_cached("DELETE FROM runs WHERE id=?")
            .unwrap()
            .execute([id])
            .unwrap();
    }
}

#[tokio::test]
#[ignore = "local measurement; run alone with --ignored --nocapture --test-threads=1"]
async fn run_storage_layouts() {
    let corpus: Vec<_> = (0..1000).map(|n| {
        let id = uuid::Uuid::from_u128((4_u128 << 76) | (2_u128 << 62) | n).to_string();
        let state: cairn_contracts::RunState = serde_json::from_value(json!({
            "id": id, "taskId": id, "status": if n % 3 == 0 { "failed" } else { "succeeded" },
            "projectId": "00000000-0000-4000-8000-000000000005", "trigger": "manual",
            "createdAt": 1700000000000_i64 + n as i64, "startedAt": 1700000000001_i64 + n as i64,
            "finishedAt": 1700000000100_i64 + n as i64, "sessionId": format!("session-{n}"),
            "summary": "Completed local task with preserved conversation context. ".repeat(12),
            "resumeAvailable": true,
            "requestedBy": if n % 2 == 0 { json!({"runId":"parent","chatId":"chat"}) } else { Value::Null },
        })).unwrap();
        let snapshot: cairn_contracts::RunSnapshot = serde_json::from_value(json!({
            "task": {"id":id,"agentId":"00000000-0000-4000-8000-000000000001", "name":format!("Mission {n}"), "prompt":"Inspect the repository and preserve authorization. ".repeat(20)},
            "agent": {"id":"00000000-0000-4000-8000-000000000001", "name":"Agent", "revision":"fixture", "createdAt":0, "instructions":"Inspect callers and validate behavior. ".repeat(20),"access":cairn_contracts::AccessPolicy::default(),"exclusiveNodes":[],"plugins":[]},
            "project":null, "skills":[{"name":"review","path":"global/review","content":"Review changes independently. ".repeat(30)}],
        })).unwrap();
        let mut document = serde_json::to_value(state).unwrap();
        document["snapshot"] = serde_json::to_value(snapshot).unwrap();
        document["isolated"] = true.into();
        document["nodeId"] = format!("node-{}",n%3).into();
        document["executionBackend"] = "native".into();
        MeasuredRun(document)
    }).collect();
    measure(&corpus);
}
