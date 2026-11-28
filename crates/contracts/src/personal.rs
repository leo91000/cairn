//! What one Cairn account has read and snoozed. These states belong to the
//! account of the verified caller, never to every member of the installation.
use crate::*;

/// Work an account can read: a conversation's latest turn or pending question.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum ReadTarget {
    Chat { chat_id: ConversationId },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReadMark {
    pub target: ReadTarget,
    /// Observed Chat.readAt or finish time, including questions and errors. Later work stays unread.
    #[schemars(range(min = 1, max = 9007199254740991_i64))]
    #[ts(type = "number")]
    pub finished_at: i64,
    /// The observed deadline when explicitly marking a snoozed conversation as read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 1, max = 9007199254740991_i64))]
    #[ts(optional, type = "number")]
    pub snoozed_until: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MarkReadInput {
    #[schemars(length(min = 1, max = 200))]
    pub items: Vec<ReadMark>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MarkReadOutput {
    #[schemars(range(min = 1, max = 200))]
    pub read: u32,
    #[schemars(range(min = 0, max = 9007199254740991_i64))]
    #[ts(type = "number")]
    pub read_at: i64,
}

/// How an account snoozes a conversation until a later time, or brings it back early.
/// A snooze lasts until its deadline or until the conversation has later activity.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum ConversationSnoozeAction {
    Snooze {
        #[schemars(range(min = 1, max = 9007199254740991_i64))]
        #[ts(type = "number")]
        finished_at: i64,
        /// At most 30 days ahead.
        #[schemars(range(min = 1, max = 9007199254740991_i64))]
        #[ts(type = "number")]
        until: i64,
    },
    Restore,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SetConversationSnoozeInput {
    pub chat_id: ConversationId,
    pub action: ConversationSnoozeAction,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SetConversationSnoozeOutput {
    pub chat_id: ConversationId,
    #[schemars(range(min = 1, max = 9007199254740991_i64))]
    #[ts(type = "number | null")]
    pub snoozed_until: Option<i64>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    const MAX: i64 = 9_007_199_254_740_991;

    fn corpus() -> Value {
        serde_json::from_str(include_str!(
            "../../../tests/fixtures/personal-settings-contracts.json"
        ))
        .unwrap()
    }

    fn round_trip<T: serde::de::DeserializeOwned + Serialize>(values: &Value) {
        for value in values.as_array().unwrap() {
            let decoded: T = serde_json::from_value(value.clone()).unwrap();
            assert_eq!(&serde_json::to_value(decoded).unwrap(), value);
        }
    }

    /// Serde decodes the structure; the server applies the schema's bounds.
    fn mark(value: &Value) -> bool {
        serde_json::from_value::<MarkReadInput>(value.clone()).is_ok_and(|input| {
            (1..=200).contains(&input.items.len())
                && input.items.iter().all(|item| {
                    (1..=MAX).contains(&item.finished_at)
                        && item
                            .snoozed_until
                            .is_none_or(|until| (1..=MAX).contains(&until))
                })
        })
    }

    #[test]
    fn personal_and_settings_corpus_keeps_its_wire_meaning() {
        let corpus = corpus();
        round_trip::<MarkReadInput>(&corpus["marks"]);
        round_trip::<SetConversationSnoozeInput>(&corpus["snoozes"]);
        round_trip::<SetConversationSnoozeOutput>(&corpus["snoozeOutputs"]);
        round_trip::<InstallationSettings>(&corpus["settings"]);
        round_trip::<InstallationOverview>(&corpus["overviews"]);
        round_trip::<ListAuditOutput>(&corpus["audits"]);
        for value in corpus["marks"].as_array().unwrap() {
            assert!(mark(value));
        }
        for value in corpus["invalidMarks"].as_array().unwrap() {
            assert!(!mark(value), "{value}");
        }
        for value in corpus["invalidSnoozes"].as_array().unwrap() {
            assert!(
                serde_json::from_value::<SetConversationSnoozeInput>(value.clone()).is_err(),
                "{value}"
            );
        }
    }
}
