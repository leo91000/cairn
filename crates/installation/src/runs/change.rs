use cairn_contracts::{AgentProvider, Retry, RunRequester, RunState, RunStatus};
use serde_json::Value;

/// Changes prepared by the named admission/control/settlement operations. Nullable
/// assignments distinguish clearing a value from leaving it untouched. Unlike a
/// JSON merge, every lifecycle assignment is checked by Rust and SQL.
#[derive(Default)]
pub struct RunChanges {
    pub wakeup: Option<Option<cairn_contracts::ChatWakeup>>,

    pub placement: Option<Box<crate::nodes::run_placement::PlacementChanges>>,
    pub snapshot: Option<cairn_contracts::RunSnapshot>,
    pub task_id: Option<String>,
    pub project_id: Option<Option<String>>,
    pub status: Option<RunStatus>,
    pub trigger: Option<String>,
    pub created_at: Option<i64>,
    pub started_at: Option<Option<i64>>,
    pub finished_at: Option<Option<i64>>,
    pub summary: Option<String>,
    pub error: Option<Option<String>>,
    pub session_id: Option<Option<String>>,
    pub workspace: Option<Option<String>>,
    pub workspace_cleaned_at: Option<Option<i64>>,
    pub recovery_pending: Option<bool>,
    pub resume_available: Option<bool>,
    pub resume_count: Option<u32>,
    pub cancel_requested_at: Option<Option<i64>>,
    pub account_id: Option<Option<String>>,
    pub account_name: Option<Option<String>>,
    pub account_wait_reason: Option<Option<String>>,
    pub account_required: Option<Option<AgentProvider>>,
    pub capacity_wait_until: Option<Option<i64>>,
    pub requested_by: Option<Option<RunRequester>>,
    pub retry: Option<Option<Retry>>,
    /// Temporary payload adapter for unconverted domains only (A6-A13/A18).
    pub boundary: Option<Value>,
}

impl RunChanges {
    pub fn apply(
        self,
        state: &mut RunState,
    ) -> (Option<cairn_contracts::RunSnapshot>, Option<Value>) {
        if let Some(value) = self.task_id {
            state.task_id = value;
        }
        if let Some(value) = self.project_id {
            state.project_id = value;
        }
        if let Some(value) = self.status {
            state.status = value;
        }
        if let Some(value) = self.trigger {
            state.trigger = value;
        }
        if let Some(value) = self.created_at {
            state.created_at = value;
        }
        if let Some(value) = self.started_at {
            state.started_at = value;
        }
        if let Some(value) = self.finished_at {
            state.finished_at = value;
        }
        if let Some(value) = self.summary {
            state.summary = value;
        }
        if let Some(value) = self.error {
            state.error = value;
        }
        if let Some(value) = self.session_id {
            state.session_id = value;
        }
        if let Some(value) = self.workspace {
            state.workspace = value;
        }
        if let Some(value) = self.workspace_cleaned_at {
            state.workspace_cleaned_at = value;
        }
        if let Some(value) = self.recovery_pending {
            state.recovery_pending = value;
        }
        if let Some(value) = self.resume_available {
            state.resume_available = value;
        }
        if let Some(value) = self.resume_count {
            state.resume_count = value;
        }
        if let Some(value) = self.cancel_requested_at {
            state.cancel_requested_at = value;
        }
        if let Some(value) = self.account_id {
            state.account_id = value;
        }
        if let Some(value) = self.account_name {
            state.account_name = value;
        }
        if let Some(value) = self.account_wait_reason {
            state.account_wait_reason = value;
        }
        if let Some(value) = self.account_required {
            state.account_required = value;
        }
        if let Some(value) = self.capacity_wait_until {
            state.capacity_wait_until = value;
        }
        if let Some(value) = self.requested_by {
            state.requested_by = value;
        }
        if let Some(value) = self.retry {
            state.retry = value;
        }
        (self.snapshot, self.boundary)
    }
}
