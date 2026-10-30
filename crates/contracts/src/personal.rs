//! What one Cairn account has read and set aside. These states belong to the
//! account of the verified caller, never to every member of the installation.
use crate::*;

/// Work an account can read: a conversation's latest turn or a mission's latest run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum ReadTarget {
    Chat { chat_id: ConversationId },
    Mission { task_id: MissionId },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReadMark {
    pub target: ReadTarget,
    /// When the work the account saw finished. Later work stays unread.
    #[schemars(range(min = 1, max = 9007199254740991_i64))]
    #[ts(type = "number")]
    pub finished_at: i64,
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

/// How an account sets a blocked mission outcome aside. Each applies until the
/// mission finishes again; input the agent asked for can only be snoozed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum OutcomeAsideAction {
    Dismiss {
        #[schemars(range(min = 1, max = 9007199254740991_i64))]
        #[ts(type = "number")]
        finished_at: i64,
    },
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
pub struct SetOutcomeAsideInput {
    pub task_id: MissionId,
    pub action: OutcomeAsideAction,
}

/// An outcome set aside by the calling account: dismissed (`until` null) or snoozed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OutcomeDismissal {
    #[schemars(range(min = 1, max = 9007199254740991_i64))]
    #[ts(type = "number | null")]
    pub until: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SetOutcomeAsideOutput {
    pub task_id: MissionId,
    pub dismissal: Option<OutcomeDismissal>,
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
                && input
                    .items
                    .iter()
                    .all(|item| (1..=MAX).contains(&item.finished_at))
        })
    }

    #[test]
    fn personal_and_settings_corpus_keeps_its_wire_meaning() {
        let corpus = corpus();
        round_trip::<MarkReadInput>(&corpus["marks"]);
        round_trip::<SetOutcomeAsideInput>(&corpus["asides"]);
        round_trip::<SetOutcomeAsideOutput>(&corpus["asideOutputs"]);
        round_trip::<InstallationSettings>(&corpus["settings"]);
        round_trip::<InstallationOverview>(&corpus["overviews"]);
        round_trip::<ListAuditOutput>(&corpus["audits"]);
        round_trip::<StorageSettings>(&corpus["storageViews"]);
        for value in corpus["marks"].as_array().unwrap() {
            assert!(mark(value));
        }
        for value in corpus["invalidMarks"].as_array().unwrap() {
            assert!(!mark(value), "{value}");
        }
        for value in corpus["invalidAsides"].as_array().unwrap() {
            assert!(
                serde_json::from_value::<SetOutcomeAsideInput>(value.clone()).is_err(),
                "{value}"
            );
        }
        for value in corpus["storageInputs"].as_array().unwrap() {
            let input: SaveStorageSettingsInput = serde_json::from_value(value.clone()).unwrap();
            assert!(!format!("{input:?}").contains("fixture-secret"));
        }
        let invalid = corpus["invalidStorageInputs"].as_array().unwrap();
        // Bucket names and HTTPS endpoints are also server invariants.
        for value in [&invalid[0], &invalid[3]] {
            assert!(serde_json::from_value::<SaveStorageSettingsInput>(value.clone()).is_err());
        }
        for value in corpus["storageViews"].as_array().unwrap() {
            let keys = value.as_object().unwrap().keys().collect::<Vec<_>>();
            assert!(
                !keys
                    .iter()
                    .any(|key| key.contains("ccess") || key.contains("ecret"))
            );
        }
    }
}
