//! Package lifecycle and owner-requested preparations. Contributions use the
//! existing manifest rather than another client-maintained manifest model.
use crate::*;
pub use cairn_plugin_api::{Manifest as PluginManifest, Setup as PluginSetup};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PluginFile {
    pub path: String,
    #[ts(type = "number")]
    pub bytes: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PackageVersion {
    pub version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub digest: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub files: Option<Vec<PluginFile>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manifest: Option<PluginManifest>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub setup: Option<PluginSetup>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approved: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
pub enum PluginPackageStatus {
    Active,
    Update,
    NeedsApproval,
    Invalid,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PluginEntry {
    pub id: String,
    pub builtin: bool,
    #[schemars(with = "String", pattern("^(active|update|needs_approval|invalid)$"))]
    pub status: PluginPackageStatus,
    pub active: Option<String>,
    pub manifest: Option<PluginManifest>,
    pub setup: Option<PluginSetup>,
    pub versions: Vec<PackageVersion>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PluginCapability {
    pub name: String,
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PluginCatalog {
    pub packages: Vec<PluginEntry>,
    pub capabilities: Vec<PluginCapability>,
    pub connections: Vec<PluginConnection>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "lowercase")]
pub enum ProgramArchiveFormat {
    Zip,
    Binary,
}

impl ProgramArchiveFormat {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Zip => "zip",
            Self::Binary => "binary",
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "lowercase")]
pub enum PluginManifestFormat {
    #[default]
    Json,
    Toml,
}

impl PluginManifestFormat {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Json => "json",
            Self::Toml => "toml",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ListPluginPackagesInput {}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DeletePluginPackageOutput {
    pub deleted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProgramRelease {
    pub version: String,
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    #[schemars(with = "String", pattern("^(zip|binary)$"))]
    pub format: ProgramArchiveFormat,
    pub executable: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InstalledProgram {
    pub release: ProgramRelease,
    pub archive_sha256: String,
    pub binary_sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProgramState {
    pub name: String,
    pub installer: String,
    pub installed: Option<InstalledProgram>,
    pub previous: Option<InstalledProgram>,
    pub candidate: Option<ProgramRelease>,
    pub digest: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ListPluginProgramsInput {
    pub plugin_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ListPluginProgramsOutput {
    pub programs: Vec<ProgramState>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InstallPluginPackageInput {
    #[ts(type = "any")]
    pub manifest: serde_json::Value,
    #[serde(default)]
    #[schemars(with = "String", pattern("^(json|toml)$"))]
    pub manifest_format: PluginManifestFormat,
    pub source: String,
    pub modules: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ApprovePluginPackageInput {
    pub plugin_id: String,
    pub version: String,
    pub digest: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ActivatePluginPackageInput {
    pub plugin_id: String,
    pub version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PluginPackageInput {
    pub plugin_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PluginOperationInput {
    pub operation_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum PluginProgramInput {
    Check {
        plugin_id: String,
        program: String,
    },
    Install {
        plugin_id: String,
        program: String,
        digest: String,
    },
    Rollback {
        plugin_id: String,
        program: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum PluginOperationResult {
    Project {
        project: Box<Project>,
    },
    SourceProjects {
        page: Box<PluginProjectPage>,
    },
    SkillImport {
        imported: Box<SkillImport>,
    },
    SkillSource {
        status: Box<SkillSourceStatus>,
    },
    SkillBundle {
        bundle: Box<SkillBundle>,
    },
    Package {
        version: Box<PackageVersion>,
    },
    Program {
        program: ProgramState,
    },
    Management {
        output: ManagePluginOutput,
        wrapped: bool,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PluginOperation {
    pub id: String,
    #[schemars(with = "String", pattern("^(running|completed|failed|cancelled)$"))]
    pub state: PluginOperationState,
    pub result: Option<PluginOperationResult>,
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub error_status: Option<u16>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub enum PluginOperationState {
    Running,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PluginOperationStarted {
    pub operation: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_commands_and_operation_results_match_the_shared_corpus() {
        let corpus: serde_json::Value = serde_json::from_str(include_str!(
            "../../../tests/fixtures/plugin-package-contracts.json"
        ))
        .unwrap();
        for value in corpus["installs"].as_array().unwrap() {
            let _: InstallPluginPackageInput = serde_json::from_value(value.clone()).unwrap();
        }
        for value in corpus["invalidInstalls"].as_array().unwrap() {
            assert!(
                serde_json::from_value::<InstallPluginPackageInput>(value.clone()).is_err(),
                "{value}"
            );
        }
        for value in corpus["programs"].as_array().unwrap() {
            let command: PluginProgramInput = serde_json::from_value(value.clone()).unwrap();
            assert_eq!(serde_json::to_value(command).unwrap(), *value);
        }
        for value in corpus["invalidPrograms"].as_array().unwrap() {
            assert!(
                serde_json::from_value::<PluginProgramInput>(value.clone()).is_err(),
                "{value}"
            );
        }
        for value in corpus["operations"].as_array().unwrap() {
            let operation: PluginOperation = serde_json::from_value(value.clone()).unwrap();
            assert_eq!(serde_json::to_value(operation).unwrap(), *value);
        }
    }
}
