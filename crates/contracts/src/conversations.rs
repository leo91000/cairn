//! Conversation commands and public views.
use crate::*;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "lowercase")]
pub enum ConversationLifecycle {
    #[default]
    Active,
    Trash,
    Purging,
}

impl ConversationLifecycle {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Trash => "trash",
            Self::Purging => "purging",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "active" => Self::Active,
            "trash" => Self::Trash,
            "purging" => Self::Purging,
            _ => return None,
        })
    }
}

impl From<ConversationLifecycle> for serde_json::Value {
    fn from(value: ConversationLifecycle) -> Self {
        value.as_str().into()
    }
}

impl PartialEq<ConversationLifecycle> for serde_json::Value {
    fn eq(&self, value: &ConversationLifecycle) -> bool {
        self.as_str() == Some(value.as_str())
    }
}

/// Retirement of a working environment leaves its conversation and files retained.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "lowercase")]
pub enum ConversationEnvironmentState {
    Cleaning,
    Expired,
}

impl ConversationEnvironmentState {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Cleaning => "cleaning",
            Self::Expired => "expired",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "cleaning" => Some(Self::Cleaning),
            "expired" => Some(Self::Expired),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "lowercase")]
pub enum MessageMode {
    #[default]
    Queue,
    Steer,
}

impl MessageMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Queue => "queue",
            Self::Steer => "steer",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "queue" => Self::Queue,
            "steer" => Self::Steer,
            _ => return None,
        })
    }
}

impl From<MessageMode> for serde_json::Value {
    fn from(value: MessageMode) -> Self {
        value.as_str().into()
    }
}

impl PartialEq<MessageMode> for serde_json::Value {
    fn eq(&self, value: &MessageMode) -> bool {
        self.as_str() == Some(value.as_str())
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "lowercase")]
pub enum MessageStatus {
    #[default]
    Queued,
    Sending,
    Delivered,
    Cancelled,
}

impl MessageStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Sending => "sending",
            Self::Delivered => "delivered",
            Self::Cancelled => "cancelled",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "queued" => Self::Queued,
            "sending" => Self::Sending,
            "delivered" => Self::Delivered,
            "cancelled" => Self::Cancelled,
            _ => return None,
        })
    }
}

impl From<MessageStatus> for serde_json::Value {
    fn from(value: MessageStatus) -> Self {
        value.as_str().into()
    }
}

impl PartialEq<MessageStatus> for serde_json::Value {
    fn eq(&self, value: &MessageStatus) -> bool {
        self.as_str() == Some(value.as_str())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UploadChatAttachmentInput {
    #[serde(deserialize_with = "uuid_string")]
    #[schemars(with = "uuid::Uuid")]
    pub chat_id: String,
    #[serde(deserialize_with = "uuid_string")]
    #[schemars(with = "uuid::Uuid")]
    pub attachment_id: String,
    #[schemars(length(min = 1, max = 180))]
    pub name: String,
    #[schemars(length(max = 13981016))]
    pub data: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct ChatAttachment {
    pub id: String,
    #[serde(default)]
    pub chat_id: String,
    pub name: String,
    #[ts(type = "number")]
    #[serde(default)]
    #[schemars(range(min = 0, max = 41943040))]
    pub size: u64,
    #[serde(default = "binary_media")]
    pub media_type: String,
    #[schemars(pattern("^(image|file)$"))]
    #[serde(default = "file_kind")]
    #[ts(type = r#""image" | "file""#)]
    pub kind: String,
    #[serde(default)]
    pub digest: String,
    #[serde(default)]
    #[ts(type = "number")]
    #[serde(deserialize_with = "nonnegative_wire")]
    #[schemars(range(min = 0_i64, max = 9007199254740991_i64))]
    pub created_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub width: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub height: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct ChatMessage {
    pub id: String,
    #[serde(default)]
    pub chat_id: String,
    #[serde(default)]
    pub revision: String,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    #[schemars(pattern("^(|codex|claude|opencode)$"))]
    #[ts(type = r#""" | import('../../accounts').Provider"#)]
    pub provider: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub reasoning: String,
    #[serde(default)]
    pub speed: String,
    #[serde(default)]
    #[schemars(with = "String", pattern("^(queue|steer)$"))]
    pub mode: MessageMode,
    #[serde(default)]
    #[schemars(with = "String", pattern("^(queued|sending|delivered|cancelled)$"))]
    pub status: MessageStatus,
    #[serde(default)]
    #[ts(type = "number")]
    #[serde(deserialize_with = "nonnegative_wire")]
    #[schemars(range(min = 0_i64, max = 9007199254740991_i64))]
    pub created_at: i64,
    #[serde(default)]
    pub attachments: Vec<ChatAttachment>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub question_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct ChatRequester {
    pub chat_id: String,
    #[serde(default)]
    pub title: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct ConversationRequester {
    pub run_id: String,
    pub chat_id: String,
    #[serde(default)]
    pub request_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct ChatCompaction {
    pub supported: bool,
    pub running: bool,
    pub available: bool,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct ContextUsage {
    #[ts(type = "number")]
    #[serde(default)]
    #[serde(deserialize_with = "nonnegative_wire")]
    #[schemars(range(min = 0_i64, max = 9007199254740991_i64))]
    pub context_tokens: i64,
    #[ts(type = "number | null")]
    #[serde(default)]
    #[serde(deserialize_with = "nullable_wire")]
    #[schemars(range(min = 0_i64, max = 9007199254740991_i64))]
    pub context_window: Option<i64>,
    #[ts(type = "number | null")]
    #[serde(default)]
    #[serde(deserialize_with = "nullable_wire")]
    #[schemars(range(min = 0_i64, max = 9007199254740991_i64))]
    pub cached_tokens: Option<i64>,
    #[serde(default)]
    pub estimated: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct CodingAgentSwitch {
    #[schemars(with = "String", pattern("^(codex|claude|opencode)$"))]
    pub provider: AgentProvider,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct Chat {
    pub id: String,
    #[serde(default)]
    pub slug: String,
    #[serde(default)]
    pub revision: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub agent_id: String,
    #[serde(default)]
    pub project_id: Option<String>,
    #[serde(default)]
    pub run_id: Option<String>,
    #[serde(default)]
    pub paused: bool,
    #[serde(default)]
    #[schemars(with = "String", pattern("^(active|trash|purging)$"))]
    pub lifecycle: ConversationLifecycle,
    #[serde(default)]
    #[ts(type = "number")]
    #[serde(deserialize_with = "nonnegative_wire")]
    #[schemars(range(min = 0_i64, max = 9007199254740991_i64))]
    pub created_at: i64,
    #[serde(default)]
    #[ts(type = "number")]
    #[serde(deserialize_with = "nonnegative_wire")]
    #[schemars(range(min = 0_i64, max = 9007199254740991_i64))]
    pub updated_at: i64,
    #[serde(default)]
    #[ts(optional = nullable, type = "number | null")]
    #[serde(deserialize_with = "nullable_wire")]
    #[schemars(range(min = 0_i64, max = 9007199254740991_i64))]
    pub last_activity_at: Option<i64>,
    #[serde(default)]
    #[ts(optional = nullable, type = "number | null")]
    #[serde(deserialize_with = "nullable_wire")]
    #[schemars(range(min = 0_i64, max = 9007199254740991_i64))]
    pub trashed_at: Option<i64>,
    #[serde(default)]
    #[ts(optional = nullable, type = "number | null")]
    #[serde(deserialize_with = "nullable_wire")]
    #[schemars(range(min = 0_i64, max = 9007199254740991_i64))]
    pub purge_at: Option<i64>,
    #[serde(default)]
    #[ts(optional = nullable, type = "number | null")]
    #[serde(deserialize_with = "nullable_wire")]
    #[schemars(range(min = 0_i64, max = 9007199254740991_i64))]
    pub restored_at: Option<i64>,
    #[serde(default)]
    #[ts(optional = nullable, type = "number | null")]
    #[serde(deserialize_with = "nullable_wire")]
    #[schemars(range(min = 0_i64, max = 9007199254740991_i64))]
    pub finished_at: Option<i64>,
    #[serde(default)]
    pub session_restart_requested: bool,
    #[serde(default)]
    pub can_request_new_session: bool,
    #[serde(default)]
    pub unread: bool,
    #[serde(default)]
    #[ts(optional = nullable)]
    pub lifecycle_error: Option<String>,
    #[serde(default)]
    #[ts(optional = nullable)]
    #[schemars(with = "Option<String>", pattern("^(cleaning|expired)$"))]
    pub environment_state: Option<ConversationEnvironmentState>,
    #[serde(default, deserialize_with = "nullable_wire")]
    #[ts(optional = nullable, type = "number | null")]
    #[schemars(range(min = 0_i64, max = 9007199254740991_i64))]
    pub environment_expired_at: Option<i64>,
    #[serde(default)]
    #[ts(optional = nullable)]
    pub environment_error: Option<String>,
    #[serde(default)]
    #[ts(optional = nullable)]
    pub requested_by: Option<ConversationRequester>,
    #[serde(default)]
    #[ts(optional = nullable)]
    pub requester: Option<ChatRequester>,
    #[serde(default)]
    #[ts(optional = nullable)]
    #[schemars(with = "Option<String>", pattern("^(codex|claude|opencode)$"))]
    pub provider: Option<AgentProvider>,
    #[serde(default)]
    #[ts(optional = nullable)]
    pub context_usage: Option<ContextUsage>,
    #[serde(default)]
    #[ts(optional = nullable)]
    pub compaction: Option<ChatCompaction>,
    #[serde(default)]
    #[ts(optional = nullable)]
    pub coding_agent_switch: Option<CodingAgentSwitch>,
    #[serde(default)]
    #[ts(optional = nullable)]
    pub preview: Option<String>,
    #[serde(default)]
    #[ts(optional = nullable)]
    pub error: Option<String>,
    #[serde(default)]
    #[ts(optional = nullable)]
    pub account_wait_reason: Option<String>,
    #[serde(default)]
    #[ts(optional = nullable)]
    #[schemars(with = "Option<String>", pattern("^(codex|claude|opencode)$"))]
    pub account_required: Option<AgentProvider>,
    #[serde(default)]
    pub pending_questions: u32,
    #[serde(default)]
    pub agent_name: String,
    #[serde(default)]
    pub project_name: Option<String>,
    #[serde(default = "idle")]
    #[schemars(
        with = "String",
        pattern("^(idle|queued|running|waiting|succeeded|failed|cancelled|interrupted)$")
    )]
    pub status: ConversationStatus,
    #[serde(default)]
    #[ts(optional = nullable)]
    pub suspended_by_exclusive: Option<crate::ExclusiveReservation>,
    #[serde(default)]
    #[ts(optional = nullable)]
    pub wakeup: Option<ChatWakeup>,
    #[serde(default)]
    pub questions: Vec<ChatQuestion>,
    #[serde(default)]
    #[ts(type = "import('../../contracts').Run | null")]
    pub run: Option<Run>,
    #[serde(default)]
    pub messages: Vec<ChatMessage>,
}

fn idle() -> ConversationStatus {
    ConversationStatus::Idle
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(transparent)]
#[schemars(inline)]
pub struct ConversationId(
    #[serde(deserialize_with = "uuid_string")]
    #[schemars(with = "uuid::Uuid")]
    pub String,
);

impl std::fmt::Display for ConversationId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

// Client-generated identities are case-sensitive persisted keys. Validate the UUID
// without changing its spelling, including when replaying a version-12 command.
fn uuid_string<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
    let value = String::deserialize(deserializer)?;
    uuid::Uuid::parse_str(&value).map_err(serde::de::Error::custom)?;
    Ok(value)
}

fn uuid_strings<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<String>, D::Error> {
    let values = Vec::<String>::deserialize(deserializer)?;
    for value in &values {
        uuid::Uuid::parse_str(value).map_err(serde::de::Error::custom)?;
    }
    Ok(values)
}

fn main_agent() -> AgentId {
    AgentId(uuid::Uuid::parse_str("00000000-0000-4000-8000-000000000001").unwrap())
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateChatInput {
    #[serde(default = "main_agent")]
    pub agent_id: AgentId,
    #[serde(default)]
    pub project_id: Option<ProjectId>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChatInput {
    pub chat_id: ConversationId,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct ChatMessageInput {
    #[serde(deserialize_with = "uuid_string")]
    #[schemars(with = "uuid::Uuid")]
    pub id: String,
    #[serde(default)]
    #[schemars(length(max = 50000))]
    pub text: String,
    #[serde(default, deserialize_with = "uuid_strings")]
    #[schemars(with = "Vec<uuid::Uuid>", length(max = 8))]
    pub attachment_ids: Vec<String>,
    #[serde(default)]
    #[schemars(with = "String", pattern("^(queue|steer)$"))]
    pub mode: MessageMode,
    #[serde(default)]
    #[schemars(pattern("^(|codex|claude|opencode)$"))]
    pub provider: String,
    #[serde(default)]
    #[schemars(length(max = 120), pattern(r"^[\w./:\[\]-]*$"))]
    pub model: String,
    #[serde(default)]
    #[schemars(pattern("^(?:[a-z][a-z0-9_-]*)?$"), length(max = 40))]
    pub reasoning: String,
    #[serde(default)]
    #[schemars(pattern("^(?:[a-z][a-z0-9_-]*)?$"), length(max = 40))]
    pub speed: String,
}

impl ChatMessageInput {
    pub fn validate(&mut self) -> Result<(), String> {
        self.text = self.text.trim().to_owned();
        self.model = self.model.trim().to_owned();
        if self.text.chars().count() > 50000
            || self.model.chars().count() > 120
            || self.attachment_ids.len() > 8
        {
            return Err("Message or attachment selection is too large.".into());
        }
        if self.text.is_empty() && self.attachment_ids.is_empty() {
            return Err("Write a message or attach a file".into());
        }
        if !matches!(self.provider.as_str(), "" | "codex" | "claude" | "opencode") {
            return Err("Invalid coding agent.".into());
        }
        if !self
            .model
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "_./:[]-".contains(c))
        {
            return Err("Invalid model.".into());
        }
        for (label, value) in [("reasoning", &self.reasoning), ("speed", &self.speed)] {
            let valid = value.is_empty()
                || value.as_bytes()[0].is_ascii_lowercase()
                    && value.bytes().all(|c| {
                        c.is_ascii_lowercase() || c.is_ascii_digit() || b"_-".contains(&c)
                    });
            if value.len() > 40 || !valid {
                return Err(format!("Invalid {label}."));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SendChatMessageInput {
    pub chat_id: ConversationId,
    pub message: ChatMessageInput,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RenameChatInput {
    pub chat_id: ConversationId,
    pub revision: String,
    #[schemars(length(min = 1, max = 90))]
    pub title: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SetChatPausedInput {
    pub chat_id: ConversationId,
    pub paused: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConfirmChatInput {
    pub chat_id: ConversationId,
    #[serde(default)]
    pub confirm: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChatMessageReference {
    pub chat_id: ConversationId,
    #[serde(deserialize_with = "uuid_string")]
    #[schemars(with = "uuid::Uuid")]
    pub message_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EditChatMessageInput {
    pub chat_id: ConversationId,
    #[serde(deserialize_with = "uuid_string")]
    #[schemars(with = "uuid::Uuid")]
    pub message_id: String,
    pub message: EditChatMessage,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EditChatMessage {
    #[serde(default)]
    #[schemars(length(max = 50000))]
    pub text: String,
    #[serde(default, deserialize_with = "uuid_strings")]
    #[schemars(with = "Vec<uuid::Uuid>", length(max = 8))]
    pub attachment_ids: Vec<String>,
    #[serde(default)]
    #[schemars(with = "String", pattern("^(queue|steer)$"))]
    pub mode: MessageMode,
    #[serde(default)]
    #[schemars(pattern("^(|codex|claude|opencode)$"))]
    pub provider: String,
    #[serde(default)]
    #[schemars(length(max = 120), pattern(r"^[\w./:\[\]-]*$"))]
    pub model: String,
    #[serde(default)]
    #[schemars(pattern("^(?:[a-z][a-z0-9_-]*)?$"), length(max = 40))]
    pub reasoning: String,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_speed"
    )]
    #[ts(optional, type = "string")]
    #[schemars(with = "String", pattern("^(?:[a-z][a-z0-9_-]*)?$"), length(max = 40))]
    pub speed: Option<String>,
}

fn optional_speed<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<String>, D::Error> {
    String::deserialize(deserializer).map(Some)
}

fn page_limit() -> u8 {
    50
}

fn active_view() -> String {
    "active".into()
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ListChatsInput {
    #[serde(default)]
    pub offset: u32,
    #[serde(default = "page_limit")]
    #[schemars(range(min = 1, max = 100))]
    pub limit: u8,
    #[serde(default = "active_view")]
    #[schemars(pattern("^(active|trash)$"))]
    pub view: String,
    #[serde(default)]
    pub agent_id: Option<AgentId>,
    #[serde(default)]
    pub project_id: Option<ProjectId>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GetChatInput {
    pub chat_id: ConversationId,
    #[serde(default)]
    pub offset: u32,
    #[serde(default = "page_limit")]
    #[schemars(range(min = 1, max = 100))]
    pub limit: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReadChatContentInput {
    pub chat_id: ConversationId,
    #[serde(default, deserialize_with = "wire_integer")]
    #[schemars(range(min = 0_i64, max = 9007199254740991_i64))]
    #[ts(type = "number")]
    pub offset: i64,
    #[serde(default)]
    #[schemars(pattern("^[a-f0-9]{64}$"))]
    pub sha256: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct ChatContentOutput {
    pub chat_id: String,
    pub encoding: String,
    pub format: String,
    #[ts(type = "number")]
    pub offset: i64,
    #[ts(type = "number")]
    pub total_bytes: i64,
    pub sha256: String,
    pub data: String,
    #[ts(type = "number | null")]
    pub next_offset: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct ListChatsOutput {
    pub chats: Vec<Chat>,
    pub next_offset: Option<u32>,
    pub has_more: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct DeleteChatMessageOutput {
    pub deleted: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ChatMessagePage {
    Complete {
        message: ChatMessage,
    },
    Truncated {
        preview: ChatMessagePreview,
        total_bytes: u32,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct ChatMessagePreview {
    pub id: String,
    pub chat_id: String,
    #[schemars(with = "String", pattern("^(queued|sending|delivered|cancelled)$"))]
    pub status: MessageStatus,
    #[ts(type = "number")]
    #[serde(deserialize_with = "nonnegative_wire")]
    #[schemars(range(min = 0_i64, max = 9007199254740991_i64))]
    pub created_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct GetChatOutput {
    pub chat: Chat,
    pub messages: Vec<ChatMessagePage>,
    pub next_offset: Option<u32>,
    pub has_more: bool,
}

fn binary_media() -> String {
    "application/octet-stream".into()
}

fn file_kind() -> String {
    "file".into()
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct ChatExecution {
    pub message_id: String,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub attachments: Vec<ChatAttachment>,
    #[serde(default)]
    pub recovery: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub context: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    #[schemars(pattern("^compact$"))]
    pub operation: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub previous: Option<CompactionReply>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct CompactionReply {
    pub summary: Option<String>,
    #[ts(type = "number | null")]
    #[serde(deserialize_with = "nullable_wire")]
    #[schemars(range(min = 0_i64, max = 9007199254740991_i64))]
    pub finished_at: Option<i64>,
    #[schemars(with = "Option<BTreeMap<String, serde_json::Value>>", extend("x-kotlin-type" = "TaskOutcome"))]
    #[ts(type = "import('../../contracts').TaskOutcome | null")]
    pub outcome: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct CodingAgentChoice {
    #[schemars(with = "String", pattern("^(codex|claude|opencode)$"))]
    pub provider: AgentProvider,
    pub label: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct CodingAgentOptions {
    #[serde(default)]
    pub run_id: Option<String>,
    #[serde(default)]
    pub turn_id: Option<String>,
    #[serde(default)]
    #[schemars(with = "Option<String>", pattern("^(codex|claude|opencode)$"))]
    pub provider: Option<AgentProvider>,
    #[serde(default)]
    pub choices: Vec<CodingAgentChoice>,
    #[serde(default)]
    pub switching: Option<CodingAgentSwitch>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SwitchChatCodingAgentInput {
    pub chat_id: ConversationId,
    pub run_id: RunId,
    #[serde(deserialize_with = "uuid_string")]
    #[schemars(with = "uuid::Uuid")]
    pub request_id: String,
    #[schemars(length(min = 1, max = 200))]
    pub turn_id: String,
    #[schemars(with = "String", pattern("^(codex|claude|opencode)$"))]
    pub provider: AgentProvider,
}

fn nonnegative_wire<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<i64, D::Error> {
    let value = crate::wire_integer(deserializer)?;
    if value < 0 {
        return Err(serde::de::Error::custom(
            "Expected a nonnegative wire integer",
        ));
    }
    Ok(value)
}

fn nullable_wire<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<i64>, D::Error> {
    let value = Option::<i64>::deserialize(deserializer)?;
    if value.is_some_and(|v| !(0..=9_007_199_254_740_991).contains(&v)) {
        return Err(serde::de::Error::custom("Integer exceeds the wire range"));
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conversation_contract_corpus() {
        let corpus: serde_json::Value = serde_json::from_str(include_str!(
            "../../../tests/fixtures/conversation-contracts.json"
        ))
        .unwrap();
        for value in corpus["uploads"].as_array().unwrap() {
            let upload: UploadChatAttachmentInput = serde_json::from_value(value.clone()).unwrap();
            assert_eq!(serde_json::to_value(upload).unwrap(), *value);
        }
        for value in corpus["invalidUploads"].as_array().unwrap() {
            assert!(serde_json::from_value::<UploadChatAttachmentInput>(value.clone()).is_err());
        }
        for value in corpus["creates"].as_array().unwrap() {
            serde_json::from_value::<CreateChatInput>(value.clone()).unwrap();
        }
        for value in corpus["messages"].as_array().unwrap() {
            let mut command: ChatMessageInput = serde_json::from_value(value.clone()).unwrap();
            command.validate().unwrap();
            let encoded = serde_json::to_value(command).unwrap();
            assert_eq!(encoded["id"], value["id"]);
            if value.get("attachmentIds").is_some() {
                assert_eq!(encoded["attachmentIds"], value["attachmentIds"]);
            }
        }
        for value in corpus["invalidMessages"]
            .as_array()
            .unwrap()
            .iter()
            .chain(corpus["serverInvalidMessages"].as_array().unwrap())
        {
            let result = serde_json::from_value::<ChatMessageInput>(value.clone());
            assert!(
                result.is_err() || result.unwrap().validate().is_err(),
                "{value}"
            );
        }
        for value in corpus["edits"].as_array().unwrap() {
            serde_json::from_value::<EditChatMessage>(value.clone()).unwrap();
        }
        for value in corpus["invalidEdits"].as_array().unwrap() {
            assert!(serde_json::from_value::<EditChatMessage>(value.clone()).is_err());
        }
        for value in corpus["views"].as_array().unwrap() {
            let view: Chat = serde_json::from_value(value.clone()).unwrap();
            serde_json::from_value::<Chat>(serde_json::to_value(view).unwrap()).unwrap();
        }
        for value in corpus["invalidViews"].as_array().unwrap() {
            assert!(
                serde_json::from_value::<Chat>(value.clone()).is_err(),
                "{value}"
            );
        }
        for value in corpus["executions"].as_array().unwrap() {
            let execution: ChatExecution = serde_json::from_value(value.clone()).unwrap();
            serde_json::from_value::<ChatExecution>(serde_json::to_value(execution).unwrap())
                .unwrap();
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConversationRetention {
    #[schemars(range(min = 0, max = 36500))]
    pub inactivity_days: u32,
}

impl Default for ConversationRetention {
    fn default() -> Self {
        Self { inactivity_days: 7 }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct GetConversationRetentionInput {}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "lowercase")]
pub enum ConversationStatus {
    Idle,
    Queued,
    Running,
    Waiting,
    Succeeded,
    Failed,
    Cancelled,
    Interrupted,
}

impl ConversationStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Waiting => "waiting",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Interrupted => "interrupted",
        }
    }
}
