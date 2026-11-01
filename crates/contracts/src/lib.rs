pub mod beacon;

use schemars::JsonSchema;
pub use schemars::generate::Contract;
use serde::{Deserialize, Serialize};
use ts_rs::TS;
use uuid::Uuid;

/// Wire UUID for an existing run; distinct from a mission/task identifier.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(transparent)]
#[schemars(inline)]
pub struct RunId(pub Uuid);

impl std::fmt::Display for RunId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResumeExecutionSelection {
    #[schemars(length(max = 120))]
    pub model: String,
    #[serde(default, skip_serializing_if = "is_false")]
    #[schemars(default, extend("default" = false))]
    pub preserve_model: bool,
    #[schemars(length(max = 40), pattern(r"^(?:[a-z][a-z0-9_-]*)?$"))]
    pub reasoning: String,
    #[schemars(length(max = 40), pattern(r"^(?:[a-z][a-z0-9_-]*)?$"))]
    pub speed: String,
}

fn is_false(value: &bool) -> bool {
    !value
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResumeRunInput {
    pub run_id: RunId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(length(min = 1, max = 200))]
    #[ts(optional = nullable)]
    pub turn_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub selection: Option<ResumeExecutionSelection>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResumeRunOutput {
    pub run_id: RunId,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum QualificationResult {
    Resumed { run_id: RunId },
    Rejected { response: ResumeRunError },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResumeRunError {
    pub error: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct QualificationFixture {
    pub id: RunId,
    pub result: QualificationResult,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub omitted: Option<String>,
    pub nullable: Option<String>,
    #[schemars(range(min = -9007199254740991_i64, max = 9007199254740991_i64))]
    #[ts(type = "number")]
    #[serde(deserialize_with = "wire_integer")]
    pub count: i64,
    pub labels: Vec<String>,
    pub names: std::collections::BTreeMap<String, String>,
}

pub const RESUME_RUN_ID: &str = "resume_run";
pub const RESUME_RUN_SCOPE: &str = "run";
pub const RESUME_RUN_PATH: &str = "/runs/{runId}/resume";
pub const RESUME_RUN_DESCRIPTION: &str = "Resume an existing stopped chat run with its saved workspace, session and checkpoint. An optional selection changes only the model, reasoning and speed for this same coding agent; turnId is required and guards against a stale selection. An empty model resolves to the current catalog default unless preserveModel is true, in which case the saved model must also be blank and remains unchanged. For Codex, an empty selection speed inherits the saved request and standard explicitly selects Standard. Mission runs retain their existing resume behavior without model selection. This does not create a new run (unlike run_task or retry).";

/// One schema profile for exported files and live MCP discovery.
pub fn schema_for<T: JsonSchema>(contract: Contract) -> serde_json::Value {
    let mut settings = schemars::generate::SchemaSettings::draft07();
    settings.contract = contract;
    let generator = settings.into_generator();
    serde_json::to_value(generator.into_root_schema_for::<T>()).expect("serializable JSON Schema")
}

fn wire_integer<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<i64, D::Error> {
    let count = i64::deserialize(deserializer)?;
    if !(-9_007_199_254_740_991..=9_007_199_254_740_991).contains(&count) {
        return Err(serde::de::Error::custom("Integer exceeds the wire range"));
    }
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resume_selection_schema_leaves_model_ids_to_provider_catalogs() {
        let schema = schema_for::<ResumeExecutionSelection>(Contract::Deserialize);
        let model = &schema["properties"]["model"];
        assert_eq!(model["maxLength"], 120);
        assert!(model.get("pattern").is_none());
        assert_eq!(schema["properties"]["preserveModel"]["default"], false);
        assert!(
            !schema["required"]
                .as_array()
                .unwrap()
                .iter()
                .any(|field| field == "preserveModel")
        );
        assert_eq!(
            schema["properties"]["reasoning"]["pattern"],
            "^(?:[a-z][a-z0-9_-]*)?$"
        );
    }

    #[test]
    fn preserve_model_schema_is_optional_nonnullable_and_defaults_false() {
        for contract in [Contract::Deserialize, Contract::Serialize] {
            let schema = schema_for::<ResumeExecutionSelection>(contract);
            let property = &schema["properties"]["preserveModel"];
            assert_eq!(property["type"], "boolean");
            assert_eq!(property["default"], false);
            assert!(
                !schema["required"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|field| field == "preserveModel")
            );
        }
    }

    #[test]
    fn resume_selection_preserve_model_is_optional_and_omitted_when_false() {
        let selection: ResumeExecutionSelection = serde_json::from_value(serde_json::json!({
            "model": "",
            "reasoning": "",
            "speed": "",
        }))
        .unwrap();
        assert!(!selection.preserve_model);
        assert_eq!(
            serde_json::to_value(selection).unwrap(),
            serde_json::json!({ "model": "", "reasoning": "", "speed": "" })
        );

        let selection: ResumeExecutionSelection = serde_json::from_value(serde_json::json!({
            "model": "",
            "preserveModel": true,
            "reasoning": "",
            "speed": "standard",
        }))
        .unwrap();
        assert!(selection.preserve_model);
        assert!(
            serde_json::from_value::<ResumeExecutionSelection>(serde_json::json!({
                "model": "",
                "preserveModel": "true",
                "reasoning": "",
                "speed": "",
            }))
            .is_err()
        );
    }

    #[test]
    fn qualification_corpus_decodes_and_round_trips() {
        let corpus: serde_json::Value = serde_json::from_str(include_str!(
            "../../../tests/fixtures/resume-contracts.json"
        ))
        .unwrap();
        for value in corpus["valid"].as_array().unwrap() {
            let decoded: QualificationFixture = serde_json::from_value(value.clone()).unwrap();
            let encoded = serde_json::to_value(&decoded).unwrap();
            assert_eq!(
                serde_json::from_value::<QualificationFixture>(encoded).unwrap(),
                decoded
            );
        }
        let defaulted: QualificationFixture =
            serde_json::from_value(corpus["valid"][0].clone()).unwrap();
        assert!(!defaulted.enabled);
        assert!(defaulted.omitted.is_none());
        assert!(defaulted.nullable.is_none());
        assert!(
            !serde_json::to_value(defaulted)
                .unwrap()
                .as_object()
                .unwrap()
                .contains_key("omitted")
        );
        for value in corpus["invalid"]
            .as_array()
            .unwrap()
            .iter()
            .chain(corpus["serverInvalid"].as_array().unwrap())
        {
            assert!(
                serde_json::from_value::<QualificationFixture>(value.clone()).is_err(),
                "{value}"
            );
        }
    }
}

mod projects;

pub use projects::*;

mod agents;

pub use agents::*;

mod missions;

pub use missions::*;

mod mcps;

pub use mcps::*;

mod plugins;

pub use plugins::*;

mod runs;

pub use runs::*;

mod run_status;

pub use run_status::*;

mod run_retry;

pub use run_retry::*;

pub mod plugin_packages;

pub use plugin_packages::*;

pub mod events;

pub use events::*;

mod timeline;

pub use timeline::*;

mod nodes;

pub use nodes::*;

mod account_usage;

pub use account_usage::*;

mod accounts;

pub use accounts::*;

mod models;

pub use models::*;

mod conversations;
mod questions;
mod skills;

pub use questions::*;

pub use conversations::*;
pub use skills::*;

mod waits;

pub use waits::*;

mod delegated_tasks;

pub use delegated_tasks::*;

#[cfg(test)]
mod a8_contract_tests;

mod personal;

pub use personal::*;

mod installation_settings;

pub use installation_settings::*;

mod placement;

pub use placement::*;
