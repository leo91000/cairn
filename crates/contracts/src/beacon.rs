//! Beacon account contracts. Installation authority never supplies an account proof.
use crate::{Contract, schema_for};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use ts_rs::TS;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ListAccountMethodsInput {}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AccountSignInMethod {
    pub id: String,
    pub kind: String,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum ListAccountMethodsOutput {
    Listed { methods: Vec<AccountSignInMethod> },
    AccountProofRequired { message: String },
}

pub const LIST_ACCOUNT_METHODS_ID: &str = "list_account_methods";
pub const LIST_ACCOUNT_METHODS_PATH: &str = "/api/account/methods";
pub const LIST_ACCOUNT_METHODS_DESCRIPTION: &str = "List the signed-in Cairn account's sign-in methods at Beacon. Installation access and public installation MCP grants do not authorize account access; they return accountProofRequired. No account-proof delegation is implemented.";

/// A typed, fail-closed result, not an invitation to supply a proof in command JSON.
pub fn account_methods_proof_required(_: ListAccountMethodsInput) -> ListAccountMethodsOutput {
    ListAccountMethodsOutput::AccountProofRequired {
        message: "A verified Beacon account session is required. Installation grants cannot supply it; account-proof delegation is not implemented.".into(),
    }
}

pub fn list_account_methods_descriptor() -> Value {
    json!({
        "name": LIST_ACCOUNT_METHODS_ID,
        "description": LIST_ACCOUNT_METHODS_DESCRIPTION,
        "inputSchema": schema_for::<ListAccountMethodsInput>(Contract::Deserialize),
        "outputSchema": schema_for::<ListAccountMethodsOutput>(Contract::Serialize),
        "_meta": {
            "cairn/http": { "method": "GET", "path": LIST_ACCOUNT_METHODS_PATH },
            "cairn/authority": "beaconAccount",
            "cairn/installationGrant": "accountProofRequired",
        },
        "annotations": {
            "readOnlyHint": true,
            "destructiveHint": false,
            "idempotentHint": true,
            "openWorldHint": false,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn account_methods_corpus_has_typed_success_and_refusal_without_caller_identity() {
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
        for value in corpus["invalidInputs"].as_array().unwrap() {
            assert!(
                serde_json::from_value::<ListAccountMethodsInput>(value.clone()).is_err(),
                "{value}"
            );
        }
        assert!(matches!(
            account_methods_proof_required(ListAccountMethodsInput {}),
            ListAccountMethodsOutput::AccountProofRequired { .. }
        ));
    }
}
