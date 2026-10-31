//! A10 mapping comparison against the frozen version-12 records writer (4af96bbe).
use super::*;

#[path = "previous_run.rs"]
mod previous_run;

use cairn_contracts::{
    AcquireExclusiveInput, ExclusiveState, NativeModeRequest, NodeResources, RunId, RunState,
};
use cairn_installation::nodes::{
    attempts::{Attempt, AttemptRole},
    exclusive_state::Reservation,
};

fn identity(n: u128) -> String {
    uuid::Uuid::from_u128((4_u128 << 76) | (2_u128 << 62) | n).to_string()
}

fn attempt(n: u128) -> Attempt {
    Attempt {
        id: identity(10000 + n),
        role: if n.is_multiple_of(2) {
            AttemptRole::Execution
        } else {
            AttemptRole::Destination
        },
        run_id: identity(n),
        node_id: cairn_installation::nodes::LOCAL_NODE_ID.into(),
        resources: NodeResources {
            cpu: 8,
            memory_mi_b: 16384,
            disk_mi_b: 32768,
        },
        runtime_id: "development".into(),
        created_at: 1700000000000 + n as i64,
        lease_expires_at: Some(1700000060000 + n as i64),
        released: n.is_multiple_of(4),
        native_mode: Some(NativeModeRequest::default()),
        additional_disk_mi_b: 128,
        disk_materialized: n.is_multiple_of(3),
        released_at: (n.is_multiple_of(4)).then_some(1700000010000 + n as i64),
        lease_required: true,
        lease_duration_ms: Some(60000),
    }
}

fn prepare_runs(db: &Db<'_>) {
    for n in 0..1000 {
        let state:RunState=serde_json::from_value(json!({"id":identity(n),"taskId":identity(n+20000),"status":"succeeded","createdAt":1700000000000_i64})).unwrap();
        db.admit_run(&state, None, &json!({}), None).unwrap();
    }
}

fn records_schema(db: &Connection) {
    db.execute_batch("DROP TABLE records;
CREATE TABLE records(id TEXT PRIMARY KEY,kind TEXT NOT NULL,data TEXT NOT NULL,updated_at INTEGER NOT NULL);
CREATE INDEX records_kind ON records(kind,updated_at DESC);
CREATE INDEX records_node_backups_run ON records(json_extract(data,'$.runId')) WHERE kind='node-backups';
CREATE UNIQUE INDEX records_chat_slug ON records(json_extract(data,'$.slug')) WHERE kind='chats';").unwrap();
}

impl StorageEntity for Attempt {
    const KIND: &'static str = "node-attempts";
    const TABLE: &'static str = "placements";
    const SAVE: &'static str = "attempt.saved";

    fn id(&self) -> String {
        self.id.clone()
    }

    fn prepare(db: &Db<'_>) {
        prepare_runs(db);
    }

    fn read(db: &Db<'_>, id: &str) -> Self {
        db.attempt(id).unwrap().unwrap()
    }

    fn list(db: &Db<'_>) -> usize {
        db.attempts().unwrap().len()
    }

    fn write(&self, db: &Db<'_>, _: bool) {
        db.save_attempt(self).unwrap();
        db.audit(Self::SAVE, &json!({"id":self.id})).unwrap();
    }

    fn reference_schema(db: &Connection) {
        db.execute_batch("DROP TABLE attempts; DROP TABLE placements;")
            .unwrap();
        records_schema(db);
    }
}

impl StorageEntity for Reservation {
    const KIND: &'static str = "node-exclusive-requests";
    const TABLE: &'static str = "exclusive_reservations";
    const SAVE: &'static str = "exclusive.saved";

    fn id(&self) -> String {
        self.id.clone()
    }

    fn prepare(db: &Db<'_>) {
        prepare_runs(db);
        let node:cairn_contracts::ExecutionNode=serde_json::from_value(json!({"id":cairn_installation::nodes::LOCAL_NODE_ID,"name":"Measurement node","revision":"reference","local":true,"createdAt":1700000000000_i64,"capabilities":{"os":"linux","arch":"x86_64","kvm":true,"cpu":16,"memoryMiB":65536,"diskMiB":262144},"limits":{"cpu":8,"memoryMiB":32768,"diskMiB":131072},"storage":cairn_contracts::NodeStoragePolicy::default(),"staleDisks":cairn_contracts::StaleDisks::default()})).unwrap();
        db.save_node(&node).unwrap();
        for n in 0..1000 {
            db.save_attempt(&attempt(n)).unwrap();
        }
    }

    fn read(db: &Db<'_>, id: &str) -> Self {
        db.exclusive_reservation(id).unwrap().unwrap()
    }

    fn list(db: &Db<'_>) -> usize {
        db.exclusive_reservations().unwrap().len()
    }

    fn write(&self, db: &Db<'_>, _: bool) {
        db.save_exclusive_reservation(self).unwrap();
        db.audit(Self::SAVE, &json!({"id":self.id})).unwrap();
    }

    fn reference_schema(db: &Connection) {
        db.execute_batch("DROP TABLE exclusive_members; DROP TABLE exclusive_paused_runs; DROP TABLE exclusive_reservations; DROP TABLE exclusive_sequence;").unwrap();
        records_schema(db);
    }
}

#[tokio::test]
#[ignore = "local measurement; run alone with --ignored --nocapture --test-threads=1"]
async fn attempt_storage_layouts() {
    measure(&(0..1000).map(attempt).collect::<Vec<_>>());
}

#[tokio::test]
#[ignore = "local measurement; run alone with --ignored --nocapture --test-threads=1"]
async fn exclusive_storage_layouts() {
    let corpus = (0..1000)
        .map(|n| {
            let members = [(n + 1) % 1000, (n + 2) % 1000];
            let reason = format!("Build {n}: bounded local compilation");
            Reservation {
                id: identity(n + 30000),
                node_id: cairn_installation::nodes::LOCAL_NODE_ID.into(),
                owner_run_id: identity(n),
                owner_attempt_id: attempt(n).id,
                member_attempt_ids: members.iter().map(|m| attempt(*m).id).collect(),
                message_id: Some(identity(n + 40000)),
                reason: reason.clone(),
                input: AcquireExclusiveInput {
                    duration_seconds: 300,
                    wait_seconds: 0,
                    acquisition_timeout_seconds: 300,
                    reason,
                    member_run_ids: members
                        .iter()
                        .map(|m| RunId(uuid::Uuid::parse_str(&identity(*m)).unwrap()))
                        .collect(),
                    minimum_available_memory_mi_b: 4096,
                },
                created_at: 1700000000000 + n as i64,
                order: n as u64 + 1,
                wait_until: 1700000300000 + n as i64,
                state: if n.is_multiple_of(3) {
                    ExclusiveState::Queued
                } else {
                    ExclusiveState::Acquired
                },
                epoch: (n % 3 != 0).then_some(n as u64 + 1),
                expires_at: (n % 3 != 0).then_some(1700000300000 + n as i64),
                requested_expires_at: None,
                acquisition_started_at: (n % 3 != 0).then_some(1700000001000 + n as i64),
                acquired_at: (n % 3 != 0).then_some(1700000002000 + n as i64),
                release_requested_at: None,
                paused_run_ids: members.iter().map(|m| identity(*m)).collect(),
                error: None,
            }
        })
        .collect::<Vec<_>>();
    measure(&corpus);
}

#[derive(Debug, PartialEq, serde::Serialize, serde::Deserialize)]
struct PlacementRun(Value);

impl PlacementRun {
    fn boundary(&self) -> Value {
        let mut value = self.0.clone();
        for field in cairn_installation::runs::RUN_FIELDS {
            value.as_object_mut().unwrap().remove(*field);
        }
        value
    }
}

impl StorageEntity for PlacementRun {
    const KIND: &'static str = "run-placement";
    const TABLE: &'static str = "runs";
    const SAVE: &'static str = "placement.saved";

    fn id(&self) -> String {
        self.0["id"].as_str().unwrap().into()
    }

    fn read(db: &Db<'_>, id: &str) -> Self {
        let mut run = db.run(id).unwrap().unwrap();
        // A8 now projects an explicit absent wakeup. It is outside this A10
        // corpus; normalize absence to the frozen reference's wire spelling.
        if run["wakeup"].is_null() {
            run.as_object_mut().unwrap().remove("wakeup");
        }
        Self(run)
    }

    fn list(db: &Db<'_>) -> usize {
        db.runs(None, None, 1000, 0, false).unwrap().len()
    }

    fn write(&self, db: &Db<'_>, replacement: bool) {
        if replacement {
            let boundary = self.boundary();
            let fields = cairn_installation::nodes::run_placement::FIELDS
                .iter()
                .map(|key| ((*key).to_owned(), boundary[*key].clone()))
                .collect::<serde_json::Map<_, _>>();
            let placement: cairn_installation::nodes::run_placement::RunPlacement =
                serde_json::from_value(Value::Object(fields)).unwrap();
            db.update_run_placement(&self.id(), placement.replacement())
                .unwrap();
        } else {
            let state: RunState = serde_json::from_value(self.0.clone()).unwrap();
            db.admit_run(&state, None, &self.boundary(), None).unwrap();
        }
        db.audit(Self::SAVE, &json!({"id":self.id()})).unwrap();
    }

    fn reference_schema(db: &Connection) {
        db.execute_batch("DROP TABLE runs;").unwrap();
        db.execute_batch(include_str!("../fixtures/v12-runs.sql"))
            .unwrap();
    }

    fn reference_read(db: &Db<'_>, id: &str) -> Self {
        Self(previous_run::read(db.0, id))
    }

    fn reference_list(db: &Db<'_>) -> usize {
        Self::list(db)
    }

    fn reference_write(&self, db: &Db<'_>, replacement: bool) {
        if replacement {
            db.0.prepare_cached("UPDATE runs SET boundary=? WHERE id=?")
                .unwrap()
                .execute(params![self.boundary().to_string(), self.id()])
                .unwrap();
        } else {
            db.0.prepare_cached("INSERT INTO runs(id,task_id,status,created_at,finished_at,summary,recovery_pending,resume_available,resume_count,boundary) VALUES(?,?,'succeeded',1700000000000,1700000010000,'',0,0,0,?)").unwrap().execute(params![self.id(),self.0["taskId"].as_str().unwrap(),self.boundary().to_string()]).unwrap();
        }
    }

    fn reference_delete(db: &Db<'_>, id: &str) {
        db.0.execute("DELETE FROM runs WHERE id=?", [id]).unwrap();
    }
}

#[tokio::test]
#[ignore = "local measurement; run alone with --ignored --nocapture --test-threads=1"]
async fn run_placement_storage_layouts() {
    let corpus=(0..1000).map(|n|{
        let state:RunState=serde_json::from_value(json!({"id":identity(n),"taskId":identity(n+20000),"status":"succeeded","createdAt":1700000000000_i64,"finishedAt":1700000010000_i64})).unwrap();
        let resources=NodeResources{cpu:8,memory_mi_b:16384,disk_mi_b:32768};
        let placement=cairn_installation::nodes::run_placement::RunPlacement {
            node_id:Some(cairn_installation::nodes::LOCAL_NODE_ID.into()),node_state:None,pinned_node_id:(n%2==0).then(||cairn_installation::nodes::LOCAL_NODE_ID.into()),preferred_node_id:(n%2!=0).then(||cairn_installation::nodes::LOCAL_NODE_ID.into()),target_node_id:None,required_tags:Some(vec!["build".into(),format!("site-{}",n%4)]),resources:Some(resources.clone()),native_mode:Some(NativeModeRequest::default()),native_mode_interruption:None,execution_backend:Some("firecracker".into()),move_request:(n%3==0).then(||cairn_installation::nodes::run_placement::MoveRequest{reservation:identity(n+10000),node_id:cairn_installation::nodes::LOCAL_NODE_ID.into(),resources,requested_at:Some(1700000000100),automatic:false,idle:Some(true),required_tags:Some(vec!["build".into()]),backup_id:Some(identity(n+30000))}),move_reservation:None,movement_error:None,suspended_by_exclusive:None,
        };
        let mut value=serde_json::to_value(state).unwrap();value.as_object_mut().unwrap().extend(serde_json::to_value(placement).unwrap().as_object().unwrap().clone());value["snapshot"]=Value::Null;
        PlacementRun(value)
    }).collect::<Vec<_>>();
    measure(&corpus);
}
