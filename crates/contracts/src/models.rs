//! Secret-free coding-agent model catalog shared by all management transports.
use crate::*;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(default, rename_all = "camelCase")]
pub struct ReasoningOption {
    pub reasoning_effort: String,
    pub description: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(default, rename_all = "camelCase")]
pub struct ServiceTier {
    pub id: String,
    pub name: String,
    pub description: String,
    pub limited: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "lowercase")]
pub enum RouteBilling {
    Subscription,
    Credits,
    #[default]
    Key,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(default, rename_all = "camelCase")]
pub struct ModelRoute {
    pub model: String,
    pub provider: String,
    pub provider_name: String,
    #[schemars(with = "String", pattern("^(subscription|credits|key)$"))]
    pub billing: RouteBilling,
    pub hidden: bool,
    pub supported_reasoning_efforts: Vec<ReasoningOption>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(default, rename_all = "camelCase")]
pub struct CodingAgentModel {
    pub model: String,
    pub display_name: String,
    pub description: String,
    pub hidden: bool,
    pub is_default: bool,
    pub default_reasoning_effort: String,
    pub supported_reasoning_efforts: Vec<ReasoningOption>,
    pub routes: Vec<ModelRoute>,
    /// None means unknown, distinct from a known empty speed list.
    pub service_tiers: Option<Vec<ServiceTier>>,
    pub default_service_tier: Option<String>,
    /// Claude's resolved model behind a subscription alias.
    pub resolved_model: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(default, rename_all = "camelCase")]
pub struct ModelCatalog {
    pub models: Vec<CodingAgentModel>,
    #[ts(type = "number | null")]
    pub checked_at: Option<i64>,
    pub stale: bool,
    pub error: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct ListModelsInput {
    #[serde(default)]
    #[schemars(with = "String", pattern("^(codex|claude|opencode)$"))]
    pub provider: AgentProvider,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_corpus_preserves_routes_speeds_and_unknown_capabilities() {
        let corpus: serde_json::Value =
            serde_json::from_str(include_str!("../../../tests/fixtures/model-contracts.json"))
                .unwrap();
        for value in corpus["catalogs"].as_array().unwrap() {
            let catalog: ModelCatalog = serde_json::from_value(value.clone()).unwrap();
            assert_eq!(
                serde_json::from_value::<ModelCatalog>(serde_json::to_value(&catalog).unwrap())
                    .unwrap(),
                catalog
            );
        }
        for value in corpus["inputs"].as_array().unwrap() {
            assert!(serde_json::from_value::<ListModelsInput>(value.clone()).is_ok());
        }
        for value in corpus["invalidInputs"].as_array().unwrap() {
            assert!(serde_json::from_value::<ListModelsInput>(value.clone()).is_err());
        }
        let unknown: CodingAgentModel =
            serde_json::from_value(serde_json::json!({"model":"old"})).unwrap();
        assert_eq!(unknown.service_tiers, None);
        let known: CodingAgentModel =
            serde_json::from_value(serde_json::json!({"model":"new","serviceTiers":[]})).unwrap();
        assert_eq!(known.service_tiers, Some(vec![]));
    }
}
