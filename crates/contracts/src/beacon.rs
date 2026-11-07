//! Shared app contracts for Beacon account reads.
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AccountSignInMethod {
    pub id: String,
    pub kind: String,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ListAccountMethodsOutput {
    pub methods: Vec<AccountSignInMethod>,
}

pub const LIST_ACCOUNT_METHODS_PATH: &str = "/api/account/methods";

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    #[test]
    fn account_methods_corpus_has_only_app_responses() {
        let corpus: Value = serde_json::from_str(include_str!(
            "../../../tests/fixtures/account-methods-contracts.json"
        ))
        .unwrap();
        for value in corpus["valid"].as_array().unwrap() {
            let decoded: ListAccountMethodsOutput = serde_json::from_value(value.clone()).unwrap();
            assert_eq!(serde_json::to_value(decoded).unwrap(), *value);
        }
        for value in corpus["invalid"].as_array().unwrap() {
            assert!(
                serde_json::from_value::<ListAccountMethodsOutput>(value.clone()).is_err(),
                "{value}"
            );
        }
    }
}
