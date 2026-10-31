//! The Cairn timeline: cards that the server interprets once from engine and
//! lifecycle events. Clients present them natively; they never fold the events.
//! This slice covers messages, the run cycle, errors and retries (C1).
use crate::*;

/// One card of a run's timeline. Its identity is stable across pages, turns and
/// reconnections; a newer revision of the same card replaces the older one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct TimelineEntry {
    pub id: String,
    pub run_id: String,
    /// Event sequence where the card first appeared. Pages are ordered by position.
    #[ts(type = "number")]
    #[serde(deserialize_with = "crate::wire_integer")]
    #[schemars(range(min = 1_i64, max = 9007199254740991_i64))]
    pub position: i64,
    /// Change cursor: the event sequence of the card's latest change, distinct
    /// from its position. A card changed by a later event keeps its position.
    #[ts(type = "number")]
    #[serde(deserialize_with = "crate::wire_integer")]
    #[schemars(range(min = 1_i64, max = 9007199254740991_i64))]
    pub revision: i64,
    /// The event holding the card's current content; read it whole with read_run_content.
    #[ts(type = "number")]
    #[serde(deserialize_with = "crate::wire_integer")]
    #[schemars(range(min = 1_i64, max = 9007199254740991_i64))]
    pub source: i64,
    /// Engine turns started before the card, counted from the run's first event.
    #[ts(type = "number")]
    #[serde(deserialize_with = "crate::wire_integer")]
    #[schemars(range(min = 0_i64, max = 9007199254740991_i64))]
    pub turn: i64,
    /// Execution attempts Cairn started before the card, after the first one.
    #[ts(type = "number")]
    #[serde(deserialize_with = "crate::wire_integer")]
    #[schemars(range(min = 0_i64, max = 9007199254740991_i64))]
    pub attempt: i64,
    #[ts(type = "number")]
    #[serde(deserialize_with = "crate::wire_integer")]
    #[schemars(range(min = 0_i64, max = 9007199254740991_i64))]
    pub created_at: i64,
    #[ts(type = "number")]
    #[serde(deserialize_with = "crate::wire_integer")]
    #[schemars(range(min = 0_i64, max = 9007199254740991_i64))]
    pub updated_at: i64,
    pub content: TimelineContent,
}

/// Metadata for an oversized card. Its source-backed fields are fetched from
/// the immutable source event before the client applies this snapshot.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct LargeTimelineReference {
    pub snapshot: TimelineEntry,
}

/// Reads the original source-backed content for a referenced card snapshot.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReadLargeTimelineInput {
    pub snapshot: TimelineEntry,
    #[ts(type = "number")]
    #[serde(default, deserialize_with = "crate::wire_integer")]
    #[schemars(range(min = 0_i64, max = 9007199254740991_i64))]
    pub offset: i64,
    #[serde(default)]
    #[schemars(pattern("^[0-9a-f]{64}$"))]
    pub sha256: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum TimelineContent {
    /// A message the person sent, as delivered to the agent. A private answer
    /// arrives as its placeholder; the answer itself never enters the timeline.
    UserMessage {
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        message_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        question_id: Option<String>,
        /// When the person sent it, at or before its delivery.
        #[ts(type = "number")]
        #[serde(deserialize_with = "crate::wire_integer")]
        #[schemars(range(min = 0_i64, max = 9007199254740991_i64))]
        sent_at: i64,
        attachments: Vec<ChatAttachment>,
    },
    /// The agent's reply. Successive snapshots of the same reply replace its text.
    AgentMessage {
        item_id: String,
        text: String,
        #[schemars(with = "String", pattern("^(running|done|info|error|stopped)$"))]
        state: TimelineState,
        /// The model provider that wrote this reply, when the engine chose among several.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        via: Option<String>,
        /// The reply only repeats a failure or usage limit the conversation already
        /// reports; a conversation omits it, the run log keeps it.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        #[ts(as = "Option<bool>", optional)]
        echo: bool,
    },
    Notice {
        notice: Box<TimelineNotice>,
    },
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "lowercase")]
pub enum TimelineState {
    Running,
    #[default]
    Done,
    Info,
    Error,
    Stopped,
}

/// What a lifecycle, error or retry card reports. Clients localize the code and
/// its arguments; `detail` is only ever content reported by the work itself.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct TimelineNotice {
    #[schemars(
        with = "String",
        pattern(
            "^(sessionStarted|turnStarted|turnCompleted|backgroundTasks|runStatus|accountSelected|speed|failure|error|connection|retryScheduled|engineRetry|usageLimit|engineNotice|diagnostic)$"
        )
    )]
    pub code: NoticeCode,
    #[schemars(with = "String", pattern("^(running|done|info|error|stopped)$"))]
    pub state: TimelineState,
    /// A conversation shows the card. Other cards remain in the run log only.
    pub prominent: bool,
    /// The engine's or worker's own words, such as a failure reason or stderr.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub detail: Option<String>,
    /// The run status a `runStatus` card reports.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub status: Option<String>,
    /// Where a failure was reported: the turn, the run, the engine or the runner.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    #[schemars(with = "Option<String>", pattern("^(turn|run|engine|runner)$"))]
    pub origin: Option<FailureOrigin>,
    /// Cairn scheduled this retry of the failure; it has not started yet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub retry: Option<TimelineRetry>,
    /// Automatic retries Cairn made before this failure repeated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub retried: Option<u32>,
    /// Work continued after this interruption or engine retry.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    #[ts(as = "Option<bool>", optional)]
    pub recovered: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub engine_retry: Option<EngineRetry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub usage_limit: Option<UsageLimit>,
    /// An engine notice's level, as the engine reports it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub level: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    #[schemars(with = "Option<String>", pattern("^(standard|confirmed|unconfirmed)$"))]
    pub speed: Option<SpeedNotice>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub account: Option<AccountNotice>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub usage: Option<TokenUsage>,
    /// The engine stopped on the account's usage limit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub quota: Option<QuotaFailure>,
    /// Background tasks a turn waits for.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[ts(as = "Option<Vec<String>>", optional)]
    pub tasks: Vec<String>,
    /// Source task IDs aligned with `tasks`; an empty ID denotes an unspecified ID.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[ts(as = "Option<Vec<String>>", optional)]
    pub task_ids: Vec<String>,
    /// Start of the current uninterrupted background wait, or absent after work resumes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "number")]
    #[schemars(range(min = 0_i64, max = 9007199254740991_i64))]
    pub waiting_since: Option<i64>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub enum NoticeCode {
    SessionStarted,
    TurnStarted,
    TurnCompleted,
    BackgroundTasks,
    #[default]
    RunStatus,
    AccountSelected,
    Speed,
    /// A turn, run, engine or runner failed.
    Failure,
    /// The engine reported an error it continues after.
    Error,
    /// The provider connection was interrupted while the engine reconnects.
    Connection,
    /// Cairn scheduled an automatic retry of a failed attempt.
    RetryScheduled,
    /// The engine retries its own request.
    EngineRetry,
    UsageLimit,
    EngineNotice,
    /// Process stderr, shown as written.
    Diagnostic,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "lowercase")]
pub enum FailureOrigin {
    Turn,
    Run,
    Engine,
    Runner,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "lowercase")]
pub enum SpeedNotice {
    /// Codex does not list the selected speed for this model: the turn runs at Standard.
    Standard,
    Confirmed,
    Unconfirmed,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct TimelineRetry {
    pub attempt: u32,
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct EngineRetry {
    pub attempt: Option<u32>,
    pub max_retries: Option<u32>,
    /// The engine's reason code, such as `overloaded` or `rate_limit`.
    pub reason: Option<String>,
    pub http_status: Option<u32>,
    #[ts(type = "number | null")]
    pub delay_ms: Option<u64>,
    pub no_response: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct UsageLimit {
    /// The provider's status, such as `allowed`, `allowed_warning` or `rejected`.
    pub status: String,
    pub window: Option<String>,
    pub blocked: bool,
    /// When the window resets. A limit no longer applies afterwards.
    #[ts(type = "number | null")]
    #[schemars(range(min = 0_i64, max = 9007199254740991_i64))]
    pub resets_at: Option<i64>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct QuotaFailure {
    /// The engine's own report of the limit.
    pub message: String,
    /// When the engine said the limit resets.
    #[ts(type = "number | null")]
    #[schemars(range(min = 0_i64, max = 9007199254740991_i64))]
    pub resets_at: Option<i64>,
    /// The latest limit of the current turn or execution, until work moves on.
    pub current: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct AccountNotice {
    /// The engine's product name, as Cairn's status reported it.
    pub agent: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct TokenUsage {
    #[ts(type = "number")]
    pub input_tokens: u64,
    #[ts(type = "number")]
    pub output_tokens: u64,
}

/// A live change of one card. A stream sends `append` only for a reply whose text
/// grows after the text it already delivered; any other change sends the card.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum TimelineChange {
    Upsert {
        entry: Box<TimelineEntry>,
    },
    Append {
        id: String,
        #[ts(type = "number")]
        #[serde(deserialize_with = "crate::wire_integer")]
        #[schemars(range(min = 1_i64, max = 9007199254740991_i64))]
        revision: i64,
        #[ts(type = "number")]
        #[serde(deserialize_with = "crate::wire_integer")]
        #[schemars(range(min = 1_i64, max = 9007199254740991_i64))]
        source: i64,
        #[ts(type = "number")]
        #[serde(deserialize_with = "crate::wire_integer")]
        #[schemars(range(min = 0_i64, max = 9007199254740991_i64))]
        updated_at: i64,
        text: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReadRunTimelineInput {
    pub run_id: RunId,
    /// Cards that first appeared before this position; the newest page by default.
    #[serde(default = "timeline_end", deserialize_with = "crate::wire_integer")]
    #[schemars(range(min = 1_i64, max = 9007199254740991_i64))]
    #[ts(type = "number")]
    pub before: i64,
    /// Resume the change feed from a cursor returned by this operation.
    #[serde(default)]
    pub changes: Option<TimelineCursor>,
    /// Keep the first page's cursor while loading older pages, then read changes
    /// from that cursor. Older cards can reflect changes made during pagination.
    #[serde(default)]
    pub snapshot: Option<TimelineCursor>,
}

/// A cursor over card revisions, bound to the retained event history.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TimelineCursor {
    #[ts(type = "number")]
    #[serde(deserialize_with = "crate::wire_integer")]
    #[schemars(range(min = 0_i64, max = 9007199254740991_i64))]
    pub revision: i64,
    /// None means all cards at this revision have been delivered.
    pub id: Option<String>,
    pub history: String,
}

fn timeline_end() -> i64 {
    9_007_199_254_740_991
}

impl ReadRunTimelineInput {
    pub fn validate(&self) -> Result<(), String> {
        if self.before < 1
            || (self.changes.is_some() && self.snapshot.is_some())
            || (self.before != timeline_end() && self.changes.is_none() && self.snapshot.is_none())
            || self
                .snapshot
                .as_ref()
                .is_some_and(|cursor| cursor.id.is_some())
            || self
                .changes
                .iter()
                .chain(self.snapshot.iter())
                .any(|cursor| {
                    cursor.revision < 0
                        || cursor.revision > timeline_end()
                        || cursor.history.is_empty()
                        || cursor.history.len() > 200
                        || cursor
                            .id
                            .as_ref()
                            .is_some_and(|id| id.is_empty() || id.chars().count() > 1024)
                })
        {
            return Err("Invalid timeline cursor.".into());
        }
        Ok(())
    }
}

/// Tool windows use the same cards with explicit previews for oversized content.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct RunTimelineOutput {
    pub entries: Vec<TimelineEntryPage>,
    /// Pass as `before` for the previous page while hasOlder is true.
    #[ts(type = "number")]
    pub oldest: i64,
    pub has_older: bool,
    /// Save the first page's cursor, then request changes after loading the pages.
    pub change_cursor: TimelineCursor,
    pub has_more_changes: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum TimelineEntryPage {
    Complete {
        entry: Box<TimelineEntry>,
    },
    /// Read the card using its entryId and snapshot with read_run_content.
    Truncated {
        preview: TimelinePreview,
        #[ts(type = "number")]
        total_bytes: u64,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct TimelinePreview {
    pub id: String,
    #[ts(type = "number")]
    pub position: i64,
    #[ts(type = "number")]
    pub revision: i64,
    #[ts(type = "number")]
    pub source: i64,
    /// The card's kind: userMessage, agentMessage or notice.
    pub kind: String,
    pub text: String,
    /// Source-backed snapshot for stable chunk reads while the card changes.
    pub snapshot: Box<TimelineEntry>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timeline_contracts_round_trip_the_shared_corpus() {
        let corpus: serde_json::Value = serde_json::from_str(include_str!(
            "../../../tests/fixtures/timeline-contracts.json"
        ))
        .unwrap();
        for value in corpus["entries"].as_array().unwrap() {
            let entry: TimelineEntry = serde_json::from_value(value.clone()).unwrap();
            assert_eq!(serde_json::to_value(entry).unwrap(), *value);
        }
        for value in corpus["changes"].as_array().unwrap() {
            let change: TimelineChange = serde_json::from_value(value.clone()).unwrap();
            assert_eq!(serde_json::to_value(change).unwrap(), *value);
        }
        let window: RunTimelineOutput = serde_json::from_value(corpus["window"].clone()).unwrap();
        assert_eq!(serde_json::to_value(window).unwrap(), corpus["window"]);
        let input: ReadRunTimelineInput = serde_json::from_value(corpus["input"].clone()).unwrap();
        input.validate().unwrap();
        let defaulted: ReadRunTimelineInput =
            serde_json::from_value(serde_json::json!({"runId": input.run_id})).unwrap();
        assert_eq!(defaulted.before, timeline_end());
        for value in corpus["invalidEntries"].as_array().unwrap() {
            assert!(
                serde_json::from_value::<TimelineEntry>(value.clone()).is_err(),
                "{value}"
            );
        }
    }
}
