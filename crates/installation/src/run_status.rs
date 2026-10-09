use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Lifecycle status persisted in `runs.status` and in the run document.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RunStatus {
    Queued,
    Running,
    Succeeded,
    Failed,
    Cancelled,
    Interrupted,
}

impl RunStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Interrupted => "interrupted",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "queued" => Self::Queued,
            "running" => Self::Running,
            "succeeded" => Self::Succeeded,
            "failed" => Self::Failed,
            "cancelled" => Self::Cancelled,
            "interrupted" => Self::Interrupted,
            _ => return None,
        })
    }

    /// Reads the `status` field of a run document.
    pub fn of(run: &Value) -> Option<Self> {
        run["status"].as_str().and_then(Self::parse)
    }

    /// Queued and running runs hold the task's active slot (`runs_active_task`).
    pub const fn is_active(self) -> bool {
        matches!(self, Self::Queued | Self::Running)
    }

    /// Terminal states from which a stopped session may be resumed.
    pub const fn is_resumable(self) -> bool {
        matches!(self, Self::Failed | Self::Interrupted | Self::Cancelled)
    }
}

impl std::fmt::Display for RunStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl From<RunStatus> for Value {
    fn from(status: RunStatus) -> Self {
        Self::String(status.as_str().to_owned())
    }
}

impl PartialEq<RunStatus> for Value {
    fn eq(&self, other: &RunStatus) -> bool {
        self.as_str() == Some(other.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::RunStatus;
    use serde_json::json;

    #[test]
    fn round_trips_through_run_documents() {
        for status in [
            RunStatus::Queued,
            RunStatus::Running,
            RunStatus::Succeeded,
            RunStatus::Failed,
            RunStatus::Cancelled,
            RunStatus::Interrupted,
        ] {
            let run = json!({ "status": status });
            assert_eq!(RunStatus::of(&run), Some(status));
            assert_eq!(run["status"], status);
            assert_eq!(serde_json::to_value(status).unwrap(), status.as_str());
        }
        assert_eq!(RunStatus::of(&json!({ "status": "unknown" })), None);
        assert_eq!(RunStatus::of(&json!({})), None);
    }
}
