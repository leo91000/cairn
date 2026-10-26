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

pub fn change_run_fixture(db: &Db<'_>, id: &str, input: &Value) -> Result<Value> {
    let mut current = required(db.run(id)?, "Run not found")?;
    merge(&mut current, input);
    let state: RunState = serde_json::from_value(current.clone())?;
    for key in serde_json::to_value(&state)?.as_object().unwrap().keys() {
        current.as_object_mut().unwrap().remove(key);
    }
    db.change_run(
        id,
        RunChanges {
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
            boundary: Some(current),
        },
    )
}

pub trait RunFixture {
    fn change_run_fixture(
        &self,
        id: &str,
        input: Value,
    ) -> impl std::future::Future<Output = Result<Value>>;
}

impl RunFixture for Store {
    async fn change_run_fixture(&self, id: &str, input: Value) -> Result<Value> {
        let id = id.to_owned();
        self.transaction(move |db| change_run_fixture(db, &id, &input))
            .await
    }
}
