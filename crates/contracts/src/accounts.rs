//! Public coding-agent account contracts. No credential or identity fingerprint crosses this boundary.
use crate::*;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "lowercase")]
pub enum AccountState {
    #[default]
    Pending,
    Ready,
    Error,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub enum AccountStatus {
    SignIn,
    Reconnect,
    Paused,
    Unavailable,
    Waiting,
    Full,
    Next,
    Low,
    #[default]
    Ready,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(default, rename_all = "camelCase")]
pub struct CodingAgentAccount {
    pub id: String,
    #[schemars(with = "String", pattern("^(codex|claude|opencode)$"))]
    pub provider: AgentProvider,
    pub name: String,
    #[serde(default = "account_enabled")]
    pub enabled: bool,
    pub revision: String,
    pub email: Option<String>,
    pub plan: Option<String>,
    pub model_provider: Option<String>,
    pub model_provider_name: Option<String>,
    #[ts(type = "number | null")]
    pub priority: Option<u64>,
    #[schemars(with = "String", pattern("^(pending|ready|error)$"))]
    pub state: AccountState,
    #[schemars(
        with = "String",
        pattern("^(signIn|reconnect|paused|unavailable|waiting|full|next|low|ready)$")
    )]
    pub status: AccountStatus,
    pub error: String,
    pub reset_error: String,
    #[ts(type = "number | null")]
    pub created_at: Option<i64>,
    #[ts(type = "number | null")]
    pub checked_at: Option<i64>,
    #[ts(type = "number | null")]
    pub last_used_at: Option<i64>,
    #[ts(type = "number")]
    #[serde(default = "account_parallel_runs")]
    pub max_concurrent_runs: u64,
    pub active_run_ids: Vec<String>,
    pub usage: Option<AccountUsage>,
    pub remaining_percent: Option<f64>,
    #[ts(type = "number | null")]
    pub resets_at: Option<i64>,
    #[serde(default = "account_enabled")]
    pub stale: bool,
    pub exhausted: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "lowercase")]
pub enum SignInState {
    #[default]
    Pending,
    Complete,
    Failed,
    Cancelled,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "lowercase")]
pub enum SignInPhase {
    #[default]
    Starting,
    Authorizing,
    Verifying,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema, TS)]
#[serde(default, rename_all = "camelCase")]
pub struct AccountSignIn {
    pub account_id: String,
    #[schemars(with = "String", pattern("^(codex|claude|opencode)$"))]
    pub provider: AgentProvider,
    #[schemars(with = "String", pattern("^(pending|complete|failed|cancelled)$"))]
    pub state: SignInState,
    #[schemars(with = "String", pattern("^(starting|authorizing|verifying)$"))]
    pub phase: SignInPhase,
    pub url: Option<String>,
    pub code: Option<String>,
    pub accepts_code: bool,
    #[ts(type = "number | null")]
    pub expires_at: Option<i64>,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema, TS)]
#[serde(default, rename_all = "camelCase")]
pub struct CodingAgentAccounts {
    pub accounts: Vec<CodingAgentAccount>,
    pub sign_in: Option<AccountSignIn>,
    #[schemars(with = "Vec<String>", inner(pattern("^(codex|claude|opencode)$")))]
    pub required: Vec<AgentProvider>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AccountInput {
    #[schemars(with = "Uuid")]
    pub account_id: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct ListAccountsInput {}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AccountSettings {
    #[schemars(length(min = 1, max = 100))]
    pub name: String,
    pub enabled: bool,
    #[ts(type = "number")]
    #[schemars(range(min = 1, max = 9007199254740991_u64))]
    #[serde(deserialize_with = "parallel_runs")]
    pub max_concurrent_runs: u64,
}

fn parallel_runs<'de, D: serde::Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
    let n = u64::deserialize(d)?;
    if !(1..=9_007_199_254_740_991).contains(&n) {
        return Err(serde::de::Error::custom(
            "Parallel runs must be a positive wire integer.",
        ));
    }
    Ok(n)
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReplaceAccountInput {
    pub revision: String,
    #[schemars(with = "Uuid")]
    pub account_id: String,
    pub settings: AccountSettings,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OrderAccountsInput {
    #[schemars(with = "String", pattern("^(codex|claude|opencode)$"))]
    pub provider: AgentProvider,
    #[schemars(with = "Vec<Uuid>", length(max = 10))]
    pub ids: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConnectAccountInput {
    #[schemars(with = "String", pattern("^(codex|claude|opencode)$"))]
    pub provider: AgentProvider,
    #[schemars(length(min = 1, max = 100))]
    pub name: String,
    #[serde(default)]
    pub model_provider: Option<String>,
    #[serde(default)]
    pub api_key: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReplaceAccountKeyInput {
    #[schemars(with = "Uuid")]
    pub account_id: String,
    pub model_provider: String,
    pub api_key: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AccountCodeInput {
    pub code: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, TS)]
pub struct AccountMutationOutput {
    pub done: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct ApiKeyProvider {
    pub id: String,
    pub name: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, TS)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ConnectAccountOutput {
    SignIn { sign_in: AccountSignIn },
    Connected { account: CodingAgentAccount },
    HumanRequired { message: String, path: String },
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AccountProviderInput {
    #[schemars(with = "String", pattern("^(codex|claude|opencode)$"))]
    pub provider: AgentProvider,
}

fn account_enabled() -> bool {
    true
}

fn account_parallel_runs() -> u64 {
    4
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_account_corpus_qualifies_usage_and_bounded_parallelism() {
        let corpus: serde_json::Value = serde_json::from_str(include_str!(
            "../../../tests/fixtures/coding-agent-account-contracts.json"
        ))
        .unwrap();
        for value in corpus["accounts"].as_array().unwrap() {
            let account: CodingAgentAccount = serde_json::from_value(value.clone()).unwrap();
            assert_eq!(
                serde_json::from_value::<CodingAgentAccount>(
                    serde_json::to_value(&account).unwrap()
                )
                .unwrap(),
                account
            );
        }
        for value in corpus["settings"].as_array().unwrap() {
            assert!(serde_json::from_value::<AccountSettings>(value.clone()).is_ok());
        }
        for value in corpus["invalidSettings"].as_array().unwrap() {
            assert!(serde_json::from_value::<AccountSettings>(value.clone()).is_err());
        }
        let _: AccountSignIn = serde_json::from_value(corpus["signIn"].clone()).unwrap();
    }
}
