//! Durable file version metadata. Bytes remain in installation storage.
use crate::*;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Deliverable {
    pub id: String,
    pub run_id: String,
    #[serde(default)]
    pub message_id: Option<String>,
    pub key: String,
    #[ts(type = "number")]
    #[schemars(range(min = 1_u64, max = 9007199254740991_u64))]
    #[serde(default = "artifact_version")]
    pub version: u64,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub group: String,
    #[ts(type = "'image' | 'video' | 'audio' | 'pdf' | 'markdown' | 'code' | 'file'")]
    #[schemars(regex(pattern = "^(image|video|audio|pdf|markdown|code|file)$"))]
    #[serde(default = "artifact_kind")]
    pub kind: String,
    #[serde(default = "artifact_media_type")]
    pub media_type: String,
    #[ts(type = "number")]
    #[schemars(range(min = 0_u64, max = 9007199254740991_u64))]
    #[serde(default)]
    pub size: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub digest: Option<String>,
    #[ts(type = "number")]
    #[schemars(range(min = 0_i64, max = 9007199254740991_i64))]
    #[serde(default)]
    pub created_at: i64,
    #[serde(default)]
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable, type = "'private' | 'public'")]
    pub visibility: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub public_token: Option<String>,
    #[serde(default)]
    #[ts(optional = nullable)]
    pub public_url: Option<String>,
    #[ts(type = "'pending' | 'ready' | 'unavailable' | 'none'")]
    #[schemars(regex(pattern = "^(pending|ready|unavailable|none)$"))]
    #[serde(default = "artifact_preview_status")]
    pub preview_status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable, type = "number")]
    pub width: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable, type = "number")]
    pub height: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub duration: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub excerpt: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ListRunArtifactsInput {
    pub run_id: RunId,
    #[serde(default)]
    #[ts(type = "number")]
    #[schemars(range(min = 0_u64, max = 9007199254740991_u64))]
    pub offset: u64,
    #[serde(default = "artifact_page_limit")]
    #[schemars(range(min = 1_u32, max = 100_u32))]
    pub limit: u32,
}

fn artifact_page_limit() -> u32 {
    50
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ListRunArtifactsOutput {
    pub artifacts: Vec<Deliverable>,
    #[ts(type = "number | null")]
    pub next_offset: Option<u64>,
    pub has_more: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReadRunArtifactInput {
    pub run_id: RunId,
    pub artifact_id: uuid::Uuid,
    #[serde(default)]
    #[ts(type = "number")]
    #[schemars(range(min = 0_u64, max = 9007199254740991_u64))]
    pub offset: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReadRunArtifactOutput {
    pub file: Deliverable,
    pub encoding: String,
    #[ts(type = "number")]
    pub offset: u64,
    #[ts(type = "number")]
    pub total_bytes: u64,
    pub data: String,
    #[ts(type = "number | null")]
    pub next_offset: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SetRunArtifactVisibilityInput {
    pub run_id: RunId,
    pub artifact_id: uuid::Uuid,
    pub visibility: ArtifactVisibility,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "lowercase")]
#[schemars(inline)]
pub enum ArtifactVisibility {
    Private,
    Public,
}

impl ArtifactVisibility {
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Private => "private",
            Self::Public => "public",
        }
    }
}

fn artifact_version() -> u64 {
    1
}

fn artifact_kind() -> String {
    "file".to_owned()
}

fn artifact_media_type() -> String {
    "application/octet-stream".to_owned()
}

fn artifact_preview_status() -> String {
    "none".to_owned()
}
