//! Durable event envelopes. Engine payloads are redacted by execution adapters;
//! the timeline presents messages, the run cycle, errors and retries (C1). Wait,
//! tool and artifact payloads remain boundaries of A8/A16 and C2–C4.
use crate::*;

/// The event's ordering identity is distinct from a client's presentation identity.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct RunEvent {
    #[ts(type = "number")]
    #[serde(deserialize_with = "crate::wire_integer")]
    #[schemars(range(min = -9007199254740991_i64, max = 9007199254740991_i64))]
    pub id: i64,
    #[ts(type = "number")]
    #[serde(deserialize_with = "crate::wire_integer")]
    #[schemars(range(min = -9007199254740991_i64, max = 9007199254740991_i64))]
    pub created_at: i64,
    #[serde(rename = "type")]
    #[schemars(with = "String")]
    #[ts(type = "string")]
    pub kind: EventKind,
    pub text: String,
    /// Redacted adapter JSON, retained without constructing a second object tree.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<EventPayload>")]
    #[ts(type = "Record<string, unknown>", optional)]
    pub payload: Option<Box<serde_json::value::RawValue>>,
    /// Client cache identity; never supplied by the event writer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(type = "number", optional)]
    pub display_id: Option<i64>,
    #[serde(default)]
    pub run_id: String,
    /// The timeline presents this event. Clients render it only through its
    /// card and read it as a boundary of families that are not converted yet.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    #[ts(as = "Option<bool>", optional)]
    pub timeline: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[serde(transparent)]
#[schemars(inline, extend("x-cairn-json-object" = true))]
pub struct EventPayload(
    #[ts(type = "Record<string, unknown>")]
    pub  std::collections::BTreeMap<String, serde_json::Value>,
);

/// Cairn markers are closed variants. An external engine can emit an unknown
/// marker; the adapter preserves its redacted payload and diagnoses it rather
/// than mistaking it for a lifecycle transition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EventKind {
    Status,
    Output,
    Diagnostic,
    Artifact,
    ChatUser,
    ChatDelegated,
    ChatWait,
    ChatWaitReady,
    ChatWakeup,
    RunWait,
    RunWaitReady,
    RunWakeup,
    RetryScheduled,
    ThreadStarted,
    TurnStarted,
    TurnCompleted,
    TurnFailed,
    TurnWaiting,
    ItemStarted,
    ItemUpdated,
    ItemCompleted,
    ContextUsage,
    EngineNotice,
    EngineRetry,
    ProviderLimit,
    RunnerFailed,
    Error,
    External(String),
}

impl EventKind {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Status => "status",
            Self::Output => "output",
            Self::Diagnostic => "diagnostic",
            Self::Artifact => "artifact",
            Self::ChatUser => "chat.user",
            Self::ChatDelegated => "chat.delegated",
            Self::ChatWait => "chat.wait",
            Self::ChatWaitReady => "chat.wait_ready",
            Self::ChatWakeup => "chat.wakeup",
            Self::RunWait => "run.wait",
            Self::RunWaitReady => "run.wait_ready",
            Self::RunWakeup => "run.wakeup",
            Self::RetryScheduled => "retry.scheduled",
            Self::ThreadStarted => "thread.started",
            Self::TurnStarted => "turn.started",
            Self::TurnCompleted => "turn.completed",
            Self::TurnFailed => "turn.failed",
            Self::TurnWaiting => "turn.waiting",
            Self::ItemStarted => "item.started",
            Self::ItemUpdated => "item.updated",
            Self::ItemCompleted => "item.completed",
            Self::ContextUsage => "context.usage",
            Self::EngineNotice => "engine.notice",
            Self::EngineRetry => "engine.retry",
            Self::ProviderLimit => "provider.limit",
            Self::RunnerFailed => "runner.failed",
            Self::Error => "error",
            Self::External(kind) => kind,
        }
    }

    pub fn from_adapter(kind: &str) -> Self {
        match kind {
            "status" => Self::Status,
            "output" => Self::Output,
            "diagnostic" => Self::Diagnostic,
            "artifact" => Self::Artifact,
            "chat.user" => Self::ChatUser,
            "chat.delegated" => Self::ChatDelegated,
            "chat.wait" => Self::ChatWait,
            "chat.wait_ready" => Self::ChatWaitReady,
            "chat.wakeup" => Self::ChatWakeup,
            "run.wait" => Self::RunWait,
            "run.wait_ready" => Self::RunWaitReady,
            "run.wakeup" => Self::RunWakeup,
            "retry.scheduled" => Self::RetryScheduled,
            "thread.started" => Self::ThreadStarted,
            "turn.started" => Self::TurnStarted,
            "turn.completed" => Self::TurnCompleted,
            "turn.failed" => Self::TurnFailed,
            "turn.waiting" => Self::TurnWaiting,
            "item.started" => Self::ItemStarted,
            "item.updated" => Self::ItemUpdated,
            "item.completed" => Self::ItemCompleted,
            "context.usage" => Self::ContextUsage,
            "engine.notice" => Self::EngineNotice,
            "engine.retry" => Self::EngineRetry,
            "provider.limit" => Self::ProviderLimit,
            "runner.failed" => Self::RunnerFailed,
            "error" => Self::Error,
            kind => Self::External(kind.to_owned()),
        }
    }
}

impl serde::Serialize for EventKind {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> serde::Deserialize<'de> for EventKind {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let kind = String::deserialize(deserializer)?;
        Ok(Self::from_adapter(&kind))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct HistoryPage {
    pub events: Vec<RunEvent>,
    /// Timeline cards that first appeared within this page or last changed in it.
    #[serde(default)]
    pub entries: Vec<TimelineEntry>,
    /// Oversized timeline cards, fetched in bounded chunks before this page is applied.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[ts(as = "Option<Vec<LargeTimelineReference>>", optional)]
    pub large_entries: Vec<LargeTimelineReference>,
    pub history: String,
    #[ts(type = "number")]
    pub oldest: i64,
    pub has_older: bool,
}

/// Agent windows use bounded previews; UI history retains its full event frames.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct RunHistoryWindowOutput {
    pub events: Vec<RunEventPage>,
    pub history: String,
    #[ts(type = "number")]
    pub oldest: i64,
    pub has_older: bool,
}

/// An oversized event retains its cursor, while its exact JSON remains available
/// through read_run_content. No success-shaped partial event is returned.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum RunEventPage {
    Complete {
        event: RunEvent,
    },
    Truncated {
        preview: EventPreview,
        #[ts(type = "number")]
        total_bytes: u64,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct EventPreview {
    #[ts(type = "number")]
    #[serde(deserialize_with = "crate::wire_integer")]
    #[schemars(range(min = -9007199254740991_i64, max = 9007199254740991_i64))]
    pub id: i64,
    pub run_id: String,
    #[ts(type = "number")]
    #[serde(deserialize_with = "crate::wire_integer")]
    #[schemars(range(min = -9007199254740991_i64, max = 9007199254740991_i64))]
    pub created_at: i64,
    #[serde(rename = "type")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReadRunHistoryInput {
    pub run_id: RunId,
    #[serde(default = "history_end", deserialize_with = "crate::wire_integer")]
    #[schemars(range(min = 1_i64, max = 9007199254740991_i64))]
    #[ts(type = "number")]
    pub before: i64,
    #[schemars(length(min = 1, max = 200))]
    pub history: String,
}

fn history_end() -> i64 {
    9_007_199_254_740_991
}

impl ReadRunHistoryInput {
    pub fn validate(&self) -> Result<(), String> {
        if self.before < 1 || self.history.is_empty() || self.history.len() > 200 {
            return Err("Invalid history cursor or revision.".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_and_history_contracts_round_trip_adapter_boundaries() {
        let corpus: serde_json::Value =
            serde_json::from_str(include_str!("../../../tests/fixtures/event-contracts.json"))
                .unwrap();
        for value in corpus["events"].as_array().unwrap() {
            let event: RunEvent = serde_json::from_value(value.clone()).unwrap();
            assert_eq!(serde_json::to_value(event).unwrap(), *value);
        }
        let window: RunHistoryWindowOutput =
            serde_json::from_value(corpus["windowOutput"].clone()).unwrap();
        assert_eq!(
            serde_json::to_value(window).unwrap(),
            corpus["windowOutput"]
        );
        let history: HistoryPage = serde_json::from_value(corpus["history"].clone()).unwrap();
        assert_eq!(serde_json::to_value(history).unwrap(), corpus["history"]);
        let input: ReadRunHistoryInput = serde_json::from_value(corpus["window"].clone()).unwrap();
        input.validate().unwrap();
        for value in corpus["invalidEvents"].as_array().unwrap() {
            assert!(serde_json::from_value::<RunEvent>(value.clone()).is_err());
        }
    }
}
