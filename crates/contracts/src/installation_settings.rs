//! Installation settings, task storage limits, audit and overview views.
use crate::*;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct GetInstallationSettingsInput {}

/// Installation opt-in for new chat plan requests. Missing policy means disabled.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChatPlanSettings {
    pub enabled: bool,
    #[schemars(length(min = 1, max = 200))]
    pub revision: String,
}

impl Default for ChatPlanSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            revision: "default".into(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct GetChatPlanSettingsInput {}

pub const DEFAULT_TASK_DISK_GIB: u16 = 150;
pub const MIN_TASK_DISK_GIB: u16 = 32;
pub const MAX_TASK_DISK_GIB: u16 = 1024;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct GetTaskStorageLimitsInput {}

/// Installation policy for future native workspace growth. Existing grants never shrink.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TaskStorageLimits {
    #[schemars(range(min = 32, max = 1024))]
    pub max_disk_gi_b: u16,
    #[schemars(length(min = 1, max = 200))]
    pub revision: String,
}

impl Default for TaskStorageLimits {
    fn default() -> Self {
        Self {
            max_disk_gi_b: DEFAULT_TASK_DISK_GIB,
            revision: "default".into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InstallationSettings {
    pub public_url: String,
    pub mcp_url: String,
    pub workspace_roots: Vec<String>,
    pub home: String,
    pub concurrency: u32,
    pub version: String,
    pub commit: String,
    pub protocol: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct ListAuditInput {}

/// One recorded installation change; `detail` is the JSON recorded with it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AuditEntry {
    #[schemars(range(min = 1, max = 9007199254740991_i64))]
    #[ts(type = "number")]
    pub id: i64,
    #[schemars(range(min = 0, max = 9007199254740991_i64))]
    #[ts(type = "number")]
    pub created_at: i64,
    pub action: String,
    pub detail: String,
}

/// The 100 most recent entries, newest first.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ListAuditOutput {
    pub entries: Vec<AuditEntry>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct GetInstallationOverviewInput {}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InstallationOverview {
    /// Retained runs by status.
    pub runs_by_status: std::collections::BTreeMap<String, u32>,
    pub agents: u32,
    pub projects: u32,
    pub missions: u32,
    pub concurrency: u32,
}
