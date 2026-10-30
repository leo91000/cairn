//! Installation settings, storage, audit and overview views. Storage views are
//! redacted: S3 credentials are accepted on write and never returned.
use crate::*;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct GetInstallationSettingsInput {}

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
pub struct GetStorageSettingsInput {}

/// The default S3 storage for new disks, without its credentials.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StorageSettings {
    pub configured: bool,
    pub integrated: bool,
    pub environment_managed: bool,
    pub bucket: String,
    pub endpoint: String,
    pub region: String,
}

/// Credentials are written to the installation's private storage file only.
#[derive(Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SaveStorageSettingsInput {
    #[schemars(length(min = 1, max = 63), pattern("^[a-z0-9.-]+$"))]
    pub bucket: String,
    #[schemars(length(min = 9, max = 2048), pattern("^https://"))]
    pub endpoint: String,
    #[schemars(length(min = 1, max = 100))]
    pub region: String,
    #[schemars(length(min = 1, max = 256))]
    pub access_key_id: String,
    #[schemars(length(min = 1, max = 256))]
    pub secret_access_key: String,
    /// Required for Cloudflare R2: public domains and bucket locks are disabled.
    #[serde(default)]
    pub private_bucket_confirmed: bool,
}

impl std::fmt::Debug for SaveStorageSettingsInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SaveStorageSettingsInput")
            .field("bucket", &self.bucket)
            .field("endpoint", &self.endpoint)
            .field("region", &self.region)
            .field("private_bucket_confirmed", &self.private_bucket_confirmed)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct CheckStorageInput {}

/// A completed write, read and deletion of a probe object. Failures are errors.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StorageCheck {
    pub ok: bool,
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
