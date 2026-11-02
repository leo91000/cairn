//! Fixture builders fill complete typed run assignments for tests of other domains.
//! These are not production commands or historical readers.
#![allow(dead_code)]

use cairn_contracts::RunState;
use cairn_installation::{
    error::{Result, required},
    runs::RunChanges,
    store::{Db, Store, merge},
};
use serde_json::Value;

/// Give snapshots used by other-domain tests complete current contracts. Explicit
/// test values still win, including malformed values a test intentionally supplies.
pub fn complete_snapshot_fixture(input: &Value) -> Value {
    let agent: cairn_contracts::Agent = serde_json::from_value(serde_json::json!({
        "id": cairn_installation::config::MAIN_AGENT_ID, "name": "Fixture",
        "revision": "fixture", "createdAt": 0, "access": cairn_contracts::AccessPolicy::default(),
        "exclusiveNodes": [], "plugins": [],
    }))
    .unwrap();
    let task: cairn_contracts::RunTaskSnapshot = serde_json::from_value(serde_json::json!({
        "id": "00000000-0000-4000-8000-000000000003", "name": "Fixture", "prompt": "Fixture",
        "agentId": cairn_installation::config::MAIN_AGENT_ID,
    }))
    .unwrap();
    let snapshot = cairn_contracts::RunSnapshot {
        task,
        agent,
        project: None,
        projects: vec![],
        available_projects: vec![],
        skills: vec![],
        delegation: None,
    };
    let mut snapshot = serde_json::to_value(snapshot).unwrap();
    fill_snapshot(&mut snapshot, input);
    snapshot
}

fn fill_snapshot(target: &mut Value, input: &Value) {
    if let (Some(target), Some(input)) = (target.as_object_mut(), input.as_object()) {
        for (key, value) in input {
            if value.is_object() && target.get(key).is_some_and(Value::is_object) {
                fill_snapshot(target.get_mut(key).unwrap(), value);
            } else {
                target.insert(key.clone(), value.clone());
            }
        }
    }
}

/// Complete a deliberately abbreviated continuation fixture through the typed API.
/// Existing explicit values (including invalid ones) remain untouched.
pub fn complete_wakeup_fixture(input: &Value) -> cairn_contracts::ChatWakeup {
    let mut value = serde_json::json!({
        "id": "fixture-wait", "messageId": null, "reason": "Fixture continuation",
        "seconds": 3600, "createdAt": 0, "wakeAt": 3600000,
    });
    fill_snapshot(&mut value, input);
    serde_json::from_value(value).unwrap()
}

pub fn complete_placement_fixture(value: &mut Value) {
    if let Some(resources) = value.get_mut("resources").filter(|value| value.is_object()) {
        let mut defaults = serde_json::json!({"cpu":2,"memoryMiB":4096,"diskMiB":32768});
        merge(&mut defaults, resources);
        *resources = defaults;
    }
    if let Some(request) = value
        .get_mut("moveRequest")
        .filter(|value| value.is_object())
    {
        let mut defaults = serde_json::json!({"reservation":cairn_installation::config::id(),"nodeId":cairn_installation::nodes::LOCAL_NODE_ID,"resources":{"cpu":2,"memoryMiB":4096,"diskMiB":32768}});
        merge(&mut defaults, request);
        *request = defaults;
    }
}

pub trait DbRunFixture {
    fn update_run_boundary_fixture(&self, id: &str, patch: &Value) -> Result<Value>;

    /// JSON assertions in other-domain tests adapt the typed current event page.
    fn events_fixture(&self, run: &str, after: i64, limit: i64) -> Result<Vec<Value>>;

    fn add_run_fixture(&self, input: &Value, dedupe: Option<&str>) -> Result<Value>;
}

impl DbRunFixture for Db<'_> {
    fn update_run_boundary_fixture(&self, id: &str, patch: &Value) -> Result<Value> {
        change_run_fixture(self, id, patch)
    }

    fn events_fixture(&self, run: &str, after: i64, limit: i64) -> Result<Vec<Value>> {
        self.event_page(run, after, limit)?
            .into_iter()
            .map(|event| serde_json::to_value(event).map_err(Into::into))
            .collect()
    }

    fn add_run_fixture(&self, input: &Value, dedupe: Option<&str>) -> Result<Value> {
        let state: RunState = serde_json::from_value(input.clone())?;
        let wakeup = input
            .get("wakeup")
            .filter(|v| v.is_object())
            .map(complete_wakeup_fixture);
        let mut payload = input.clone();
        complete_placement_fixture(&mut payload);
        for key in cairn_installation::runs::RUN_FIELDS {
            if *key != "snapshot" {
                payload.as_object_mut().unwrap().remove(*key);
            }
        }
        let snapshot = payload
            .as_object_mut()
            .unwrap()
            .remove("snapshot")
            .map(|s| complete_snapshot_fixture(&s));
        self.admit_run(&state, None, &payload, dedupe)?;
        self.save_wakeup(false, &state.id, wakeup.as_ref())?;
        if let Some(snapshot) = snapshot {
            self.0.execute(
                "UPDATE runs SET snapshot=? WHERE id=?",
                rusqlite::params![snapshot.to_string(), state.id],
            )?;
        }
        required(self.run(&state.id)?, "Run not found")
    }
}

pub fn change_run_fixture(db: &Db<'_>, id: &str, input: &Value) -> Result<Value> {
    let mut current = required(db.run(id)?, "Run not found")?;
    merge(&mut current, input);
    complete_placement_fixture(&mut current);
    let state: RunState = serde_json::from_value(current.clone())?;
    let wakeup = current
        .get("wakeup")
        .filter(|v| v.is_object())
        .map(complete_wakeup_fixture);
    for key in cairn_installation::runs::RUN_FIELDS {
        if *key != "snapshot" {
            current.as_object_mut().unwrap().remove(*key);
        }
    }
    let snapshot = current
        .as_object_mut()
        .unwrap()
        .remove("snapshot")
        .map(|value| complete_snapshot_fixture(&value));
    db.0.execute(
        "UPDATE runs SET snapshot=? WHERE id=?",
        rusqlite::params![snapshot.map(|s| s.to_string()), id],
    )?;
    let mut placement = serde_json::Map::new();
    for field in cairn_installation::nodes::run_placement::FIELDS {
        if let Some(value) = current.as_object_mut().unwrap().remove(*field) {
            placement.insert((*field).into(), value);
        }
    }
    let placement: cairn_installation::nodes::run_placement::RunPlacement =
        serde_json::from_value(Value::Object(placement))?;
    db.change_run(
        id,
        RunChanges {
            wakeup: Some(wakeup),
            task_id: Some(state.task_id),
            project_id: Some(state.project_id),
            status: Some(state.status),
            trigger: Some(state.trigger),
            created_at: Some(state.created_at),
            started_at: Some(state.started_at),
            finished_at: Some(state.finished_at),
            summary: Some(state.summary),
            error: Some(state.error),
            session_id: Some(state.session_id),
            workspace: Some(state.workspace),
            workspace_cleaned_at: Some(state.workspace_cleaned_at),
            recovery_pending: Some(state.recovery_pending),
            resume_available: Some(state.resume_available),
            resume_count: Some(state.resume_count),
            cancel_requested_at: Some(state.cancel_requested_at),
            account_id: Some(state.account_id),
            account_name: Some(state.account_name),
            account_wait_reason: Some(state.account_wait_reason),
            account_required: Some(state.account_required),
            capacity_wait_until: Some(state.capacity_wait_until),
            requested_by: Some(state.requested_by),
            retry: Some(state.retry),
            placement: Some(Box::new(placement.replacement())),
            boundary: Some(current),
            ..Default::default()
        },
    )?;
    required(db.run(id)?, "Run not found")
}

pub trait RunFixture {
    fn update_run_boundary_fixture(
        &self,
        id: &str,
        patch: Value,
    ) -> impl std::future::Future<Output = Result<Value>>;

    fn change_run_fixture(
        &self,
        id: &str,
        input: Value,
    ) -> impl std::future::Future<Output = Result<Value>>;
}

impl RunFixture for Store {
    async fn update_run_boundary_fixture(&self, id: &str, patch: Value) -> Result<Value> {
        self.change_run_fixture(id, patch).await
    }

    async fn change_run_fixture(&self, id: &str, input: Value) -> Result<Value> {
        let id = id.to_owned();
        self.transaction(move |db| change_run_fixture(db, &id, &input))
            .await
    }
}
