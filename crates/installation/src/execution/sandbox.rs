use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Filesystem and approval policy of an agent (`access.sandbox`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Sandbox {
    ReadOnly,
    WorkspaceWrite,
    Yolo,
}

impl Sandbox {
    /// `None` for a missing or unknown mode.
    pub fn of(value: &Value) -> Option<Self> {
        match value.as_str()? {
            "read-only" => Some(Self::ReadOnly),
            "workspace-write" => Some(Self::WorkspaceWrite),
            "yolo" => Some(Self::Yolo),
            _ => None,
        }
    }

    pub fn is_read_only(value: &Value) -> bool {
        Self::of(value) == Some(Self::ReadOnly)
    }

    pub fn is_yolo(value: &Value) -> bool {
        Self::of(value) == Some(Self::Yolo)
    }
}
