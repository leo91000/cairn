//! Synthetic account fixtures for suites whose engine protocol inputs remain JSON.
#![allow(dead_code)]
use cairn_contracts::AccountSettings;
use cairn_installation::{accounts::Account, error::Result, service::Service, store::Store};
use serde_json::Value;

pub trait AccountFixtures {
    fn account_fixtures(&self) -> impl Future<Output = Result<Vec<Value>>> + Send;
    fn account_fixture(&self, id: &str) -> impl Future<Output = Result<Option<Value>>> + Send;
    fn save_account_fixture(&self, value: Value) -> impl Future<Output = Result<()>> + Send;
}
impl AccountFixtures for Store {
    async fn account_fixtures(&self) -> Result<Vec<Value>> {
        Ok(self
            .coding_accounts()
            .await?
            .into_iter()
            .map(|a| a.to_value())
            .collect())
    }
    async fn account_fixture(&self, id: &str) -> Result<Option<Value>> {
        Ok(self.coding_account(id).await?.map(|a| a.to_value()))
    }
    async fn save_account_fixture(&self, value: Value) -> Result<()> {
        self.save_coding_account(&Account::from_value(value)?).await
    }
}

pub async fn update_account_fixture(s: &Service, id: &str, patch: &Value) -> Result<Value> {
    let account = s.accounts.account(s, id).await?;
    let mut settings = serde_json::to_value(AccountSettings {
        name: account.name,
        enabled: account.enabled,
        max_concurrent_runs: account.max_concurrent_runs.unwrap_or(4),
    })?;
    for (key, value) in patch.as_object().expect("fixture settings object") {
        settings[key] = value.clone();
    }
    let settings: AccountSettings = serde_json::from_value(settings)
        .map_err(|e| cairn_installation::error::Error::bad(e.to_string()))?;
    Ok(serde_json::to_value(
        s.accounts.update(s, id, &settings).await?,
    )?)
}

pub fn account_values(accounts:Vec<cairn_contracts::CodingAgentAccount>) -> Vec<Value> {
    accounts.into_iter().map(|account|serde_json::to_value(account).unwrap()).collect()
}
