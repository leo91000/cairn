use crate::*;
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SkillInput {
    #[schemars(length(max = 100))]
    pub scope: String,
    #[schemars(length(max = 64))]
    pub name: String,

}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ListSkillsInput {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub scope: Option<String>,

}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub scope: String,
    pub content: String,
    pub path: String,
    pub valid: bool,
    #[serde(default)]
    pub usage_count: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub error: Option<String>,
    #[serde(default)]
    pub files: u32,
    #[serde(default)]
    #[ts(type = "number")]
    pub bytes: u64,
    #[serde(default)]
    pub executables: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub source: Option<SkillSource>,

}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SkillSource {
    pub plugin_id: String,
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub instance: Option<String>,
    pub label: String,
    #[schemars(with = "crate::PluginValues")]
    #[ts(type = "Record<string, unknown>")]
    pub location: BTreeMap<String, serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub url: Option<String>,
    pub folder: String,
    pub version: String,
    #[ts(type = "number")]
    pub imported_at: i64,
    pub modified: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SkillFile {
    pub path: String,
    #[ts(type = "number")]
    pub size: u64,
    pub executable: bool,
    pub text: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SkillLimits {
    #[ts(type = "number")]
    pub file_bytes: u64,
    #[ts(type = "number")]
    pub bundle_bytes: u64,
    pub files: u32,
    #[ts(type = "number")]
    pub upload_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SkillBundle {
    pub scope: String,
    pub name: String,
    pub revision: String,
    pub files: Vec<SkillFile>,
    pub limits: SkillLimits,
    pub source: Option<SkillSource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub description: Option<String>,

}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SkillFileContent {
    pub content: String,
    #[schemars(pattern("^(utf-8|base64)$"))]
    pub encoding: String,
    #[ts(type = "number")]
    pub size: u64,
    pub executable: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SkillUpload {
    pub id: String,
    #[ts(type = "number")]
    pub size: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SkillImport {
    pub imported: Option<SkillBundle>,
    pub candidates: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SkillArchiveChunk {
    pub filename: String,
    pub revision: String,
    #[ts(type = "number")]
    pub size: u64,
    #[ts(type = "number")]
    pub offset: u64,
    pub content: String,

}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SkillDiffLine {
    #[schemars(pattern("^(context|added|removed|gap)$"))]
    pub kind: String,
    pub text: String,

}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SkillSourceChange {
    pub path: String,
    #[schemars(pattern("^(added|removed|modified)$"))]
    pub status: String,
    pub binary: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub lines: Option<Vec<SkillDiffLine>>,

}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SkillSourceVersion {
    pub version: String,
    pub message: String,
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
        #[ts(optional)]
        content: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        encoding: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        upload: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        executable: Option<bool>,
    },
    Delete { path: String },
    Move { from: String, to: String },
    Mode { path: String, executable: bool },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EditSkillFilesInput {
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
    #[ts(optional)]
    pub revision: Option<String>,
    #[serde(default)]
    #[schemars(range(max = 4294967295_u64))]
    #[ts(type = "number")]
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
#[serde(tag = "source", rename_all = "camelCase", rename_all_fields = "camelCase", deny_unknown_fields)]
pub enum ImportSkillInput {
    Archive {
        scope: String,
        upload: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        path: Option<String>,
    },
    Plugin {
        scope: String,
        plugin_id: String,
        skill_source: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        instance: Option<String>,
        #[schemars(with = "crate::PluginValues")]
        #[ts(type = "Record<string, unknown>")]
        location: BTreeMap<String, serde_json::Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        folder: Option<String>,
    },
}
