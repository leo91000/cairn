use crate::*;
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SkillInput {
    #[schemars(length(max = 100))]
    #[serde(default = "global_scope")]
    pub scope: String,
    #[schemars(length(max = 64))]
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ListSkillsInput {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub scope: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Skill {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default = "global_scope")]
    pub scope: String,
    #[serde(default)]
    pub content: String,
    #[serde(default)]
    pub path: String,
    #[serde(default = "yes")]
    pub valid: bool,
    #[serde(default)]
    #[schemars(range(max = 2147483647_u32))]
    pub usage_count: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub error: Option<String>,
    #[serde(default)]
    #[schemars(range(max = 2147483647_u32))]
    pub files: u32,
    #[serde(default)]
    #[ts(type = "number")]
    #[schemars(range(min = 0_u64, max = 9007199254740991_u64))]
    pub bytes: u64,
    #[serde(default)]
    #[schemars(range(max = 2147483647_u32))]
    pub executables: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub source: Option<SkillSource>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SkillSource {
    #[serde(default)]
    pub plugin_id: String,
    #[serde(default)]
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub instance: Option<String>,
    #[serde(default)]
    pub label: String,
    #[schemars(with = "crate::PluginValues")]
    #[ts(type = "Record<string, unknown>")]
    pub location: BTreeMap<String, serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub url: Option<String>,
    #[serde(default)]
    pub folder: String,
    #[serde(default)]
    pub version: String,
    #[ts(type = "number")]
    #[serde(default)]
    #[schemars(range(min = 0_i64, max = 9007199254740991_i64))]
    pub imported_at: i64,
    #[serde(default)]
    pub modified: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SkillFile {
    #[serde(default)]
    pub path: String,
    #[ts(type = "number")]
    #[serde(default)]
    #[schemars(range(min = 0_u64, max = 9007199254740991_u64))]
    pub size: u64,
    #[serde(default)]
    pub executable: bool,
    #[serde(default = "yes")]
    pub text: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SkillLimits {
    #[ts(type = "number")]
    #[serde(default = "file_limit")]
    #[schemars(range(min = 0_u64, max = 9007199254740991_u64))]
    pub file_bytes: u64,
    #[ts(type = "number")]
    #[serde(default = "bundle_limit")]
    #[schemars(range(min = 0_u64, max = 9007199254740991_u64))]
    pub bundle_bytes: u64,
    #[serde(default = "file_count")]
    #[schemars(range(max = 2147483647_u32))]
    pub files: u32,
    #[ts(type = "number")]
    #[serde(default = "upload_limit")]
    #[schemars(range(min = 0_u64, max = 9007199254740991_u64))]
    pub upload_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SkillBundle {
    #[serde(default = "global_scope")]
    pub scope: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub revision: String,
    #[serde(default)]
    pub files: Vec<SkillFile>,
    pub limits: SkillLimits,
    #[serde(default)]
    pub source: Option<SkillSource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub description: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SkillFileContent {
    #[serde(default)]
    pub content: String,
    #[schemars(pattern("^(utf-8|base64)$"))]
    #[serde(default = "utf8")]
    pub encoding: String,
    #[ts(type = "number")]
    #[serde(default)]
    #[schemars(range(min = 0_u64, max = 9007199254740991_u64))]
    pub size: u64,
    #[serde(default)]
    pub executable: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SkillUpload {
    pub id: String,
    #[ts(type = "number")]
    #[schemars(range(min = 0_u64, max = 9007199254740991_u64))]
    pub size: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SkillImport {
    pub imported: Option<SkillBundle>,
    #[serde(default)]
    pub candidates: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SkillArchiveChunk {
    #[serde(default)]
    pub filename: String,
    #[serde(default)]
    pub revision: String,
    #[ts(type = "number")]
    #[serde(default)]
    #[schemars(range(min = 0_u64, max = 9007199254740991_u64))]
    pub size: u64,
    #[ts(type = "number")]
    #[serde(default)]
    #[schemars(range(min = 0_u64, max = 9007199254740991_u64))]
    pub offset: u64,
    #[serde(default)]
    pub content: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SkillDiffLine {
    #[schemars(pattern("^(context|added|removed|gap)$"))]
    #[serde(default)]
    #[ts(type = "\"context\" | \"added\" | \"removed\" | \"gap\"")]
    pub kind: String,
    #[serde(default)]
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SkillSourceChange {
    #[serde(default)]
    pub path: String,
    #[schemars(pattern("^(added|removed|modified)$"))]
    #[serde(default)]
    #[ts(type = "\"added\" | \"removed\" | \"modified\"")]
    pub status: String,
    #[serde(default)]
    pub binary: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub lines: Option<Vec<SkillDiffLine>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SkillSourceVersion {
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub date: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SkillSourceStatus {
    pub source: SkillSource,
    pub latest: SkillSourceVersion,
    pub current: bool,
    pub changes: Vec<SkillSourceChange>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(tag = "op", rename_all = "camelCase", deny_unknown_fields)]
pub enum SkillChange {
    Write {
        path: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional = nullable)]
        content: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional = nullable)]
        encoding: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional = nullable)]
        upload: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional = nullable)]
        executable: Option<bool>,
    },
    Delete {
        path: String,
    },
    Move {
        from: String,
        to: String,
    },
    Mode {
        path: String,
        executable: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EditSkillFilesInput {
    #[serde(default = "global_scope")]
    pub scope: String,
    pub name: String,
    pub base: Option<String>,
    #[schemars(length(min = 1, max = 2000))]
    pub changes: Vec<SkillChange>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SaveSkillInput {
    #[schemars(length(max = 100))]
    #[serde(default = "global_scope")]
    pub scope: String,
    #[schemars(length(max = 64))]
    pub name: String,
    #[schemars(length(max = 100000))]
    pub content: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReadSkillFileInput {
    pub scope: String,
    pub name: String,
    #[schemars(length(max = 4096))]
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExportSkillArchiveInput {
    pub scope: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub revision: Option<String>,
    #[serde(default)]
    #[ts(type = "number")]
    #[schemars(range(min = 0_u64, max = 9007199254740991_u64))]
    pub offset: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StageSkillUploadInput {
    #[schemars(length(max = 6990508))]
    pub content: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdateSkillSourceInput {
    pub scope: String,
    pub name: String,
    #[schemars(length(max = 200))]
    pub version: String,
    #[schemars(length(max = 64))]
    pub base: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DeleteSkillOutput {
    pub deleted: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ForgetSkillSourceOutput {
    pub detached: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SkillMetadata {
    pub name: String,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(
    tag = "source",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum ImportSkillInput {
    Archive {
        scope: String,
        upload: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional = nullable)]
        path: Option<String>,
    },
    Plugin {
        scope: String,
        plugin_id: String,
        skill_source: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional = nullable)]
        instance: Option<String>,
        #[schemars(with = "crate::PluginValues")]
        #[ts(type = "Record<string, unknown>")]
        location: BTreeMap<String, serde_json::Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional = nullable)]
        folder: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReadSkillInput {
    #[serde(default = "global_scope")]
    pub scope: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub path: Option<String>,
}

fn global_scope() -> String {
    "global".into()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum ReadSkillOutput {
    Bundle(SkillBundle),
    File(SkillFileContent),
}

fn yes() -> bool {
    true
}

fn utf8() -> String {
    "utf-8".into()
}

fn file_limit() -> u64 {
    4 * 1024 * 1024
}

fn bundle_limit() -> u64 {
    16 * 1024 * 1024
}

fn file_count() -> u32 {
    500
}

fn upload_limit() -> u64 {
    5 * 1024 * 1024
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skill_contracts_round_trip_the_shared_commands_and_reject_unknown_variants() {
        let corpus: serde_json::Value =
            serde_json::from_str(include_str!("../../../tests/fixtures/skill-contracts.json"))
                .unwrap();
        for input in corpus["imports"].as_array().unwrap() {
            let command: ImportSkillInput = serde_json::from_value(input.clone()).unwrap();
            assert_eq!(
                serde_json::from_value::<ImportSkillInput>(serde_json::to_value(&command).unwrap())
                    .unwrap(),
                command
            );
        }
        for input in corpus["changes"].as_array().unwrap() {
            let change: SkillChange = serde_json::from_value(input.clone()).unwrap();
            assert_eq!(
                serde_json::from_value::<SkillChange>(serde_json::to_value(&change).unwrap())
                    .unwrap(),
                change
            );
        }
        for input in corpus["invalidImports"].as_array().unwrap() {
            assert!(serde_json::from_value::<ImportSkillInput>(input.clone()).is_err());
        }
        for input in corpus["invalidChanges"].as_array().unwrap() {
            assert!(serde_json::from_value::<SkillChange>(input.clone()).is_err());
        }
        let bundle: SkillBundle = serde_json::from_value(corpus["bundle"].clone()).unwrap();
        assert_eq!(serde_json::to_value(bundle).unwrap(), corpus["bundle"]);
    }
}
