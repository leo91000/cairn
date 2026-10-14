//! Model-provider API keys for OpenCode. Keys are encrypted on the manager and
//! obtained through the existing run-scoped broker, never saved in native sessions.
use super::{Account, Driver, Lease, Login, broker, remove_file, save};
use crate::{
    auth::hex_digest,
    config::now,
    error::{Error, Result},
    provider::Provider,
    service::Service,
    skills::private_dir,
    validation::text,
};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

pub(super) struct Opencode;

fn secret_id(id: &str) -> String {
    format!("opencode-account:{id}")
}

pub(crate) async fn credentials(s: &Service, id: &str) -> Result<Value> {
    s.vault
        .get(&secret_id(id))
        .await?
        .ok_or_else(|| Error::bad("Replace this OpenCode API key in Connections."))
}

#[async_trait::async_trait]
impl Driver for Opencode {
    fn accepts_api_keys(&self) -> bool {
        true
    }

    async fn api_key_providers(&self, s: &Service) -> Result<Value> {
        s.opencode.providers(s).await
    }

    async fn connect_api_key(
        &self,
        s: &Service,
        account: &Account,
        model_provider: &str,
        api_key: &str,
    ) -> Result<()> {
        let providers = self.api_key_providers(s).await?;
        let provider = providers
            .as_array()
            .into_iter()
            .flatten()
            .find(|p| p["id"] == model_provider)
            .ok_or_else(|| Error::bad("Choose a supported OpenCode model provider."))?;
        if let Some(previous) = account.extra.get("modelProvider")
            && previous != model_provider
        {
            return Err(Error::bad(
                "Add a separate account for a different model provider.",
            ));
        }
        let mut account = account.clone();
        account.identity = Some(hex_digest(&format!("{model_provider}:{api_key}")));
        account
            .extra
            .insert("modelProvider".into(), model_provider.into());
        account
            .extra
            .insert("modelProviderName".into(), provider["name"].clone());
        account.plan = Some("API key".into());
        account.exhausted = None;
        account.mark_ready();
        let vault = s.vault.clone();
        let credential = json!({
            "modelProvider": model_provider,
            "key": api_key,
        });
        s.store
            .transaction(move |db| {
                if super::stored(db)?.iter().any(|a| {
                    a.id != account.id
                        && a.provider == Provider::Opencode
                        && a.identity == account.identity
                }) {
                    return Err(Error::conflict(
                        "This API key is already connected. Edit the existing account instead.",
                    ));
                }
                vault.set_in(db, &secret_id(&account.id), &credential)?;
                db.put(super::KIND, &account.to_value())?;
                db.audit(
                    "account.api-key-saved",
                    &json!({
                        "id": account.id,
                        "provider": account.provider,
                    }),
                )
            })
            .await
    }

    async fn initialize(&self, _s: &Service) -> Result<()> {
        Ok(())
    }

    async fn managed(&self, _s: &Service) -> Result<bool> {
        Ok(true)
    }

    async fn authorize(&self, _s: &Service, _home: &Path, _login: &Login) -> Result<()> {
        Err(Error::bad(
            "Connect OpenCode with a model-provider API key in Connections.",
        ))
    }

    async fn adopt(&self, _s: &Service, _id: &str, _home: &Path) -> Result<()> {
        Err(Error::bad("OpenCode uses API keys for this connection."))
    }

    async fn refresh(&self, s: &Service, id: &str, _models: &[String]) -> Result<()> {
        credentials(s, id).await?;
        let mut account = s.accounts.account(s, id).await?;
        account.mark_ready();
        save(s, &account).await
    }

    async fn due(&self, _s: &Service, account: &Account, attempted: i64) -> Result<bool> {
        Ok(account.exhausted.is_some() && now() - attempted >= self.fresh_for())
    }

    fn fresh_for(&self) -> i64 {
        300_000
    }

    fn requires_usage(&self) -> bool {
        false
    }

    async fn supports(&self, s: &Service, id: &str, model: &str) -> Result<bool> {
        if model.is_empty() {
            return Ok(true);
        }
        let account = s.accounts.account(s, id).await?;
        let Some(provider) = account.extra.get("modelProvider").and_then(Value::as_str) else {
            return Ok(false);
        };
        Ok(s.opencode.cached_route(model, provider)?.is_some())
    }

    async fn warm(&self, s: &Service, model: &str) -> Result<()> {
        // Without a catalog no account supports Auto yet, so the run waits and retries.
        // A failed discovery is retried after a pause, not on every admission attempt.
        static FAILED_AT: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);
        let failed_at = FAILED_AT.load(std::sync::atomic::Ordering::Relaxed);
        if model.starts_with(crate::opencode::AUTO)
            && now() - failed_at >= 30_000
            && let Err(error) = s.opencode.models(s).await
        {
            FAILED_AT.store(now(), std::sync::atomic::Ordering::Relaxed);
            tracing::warn!(error = %error.message, "OpenCode model catalog unavailable");
        }
        Ok(())
    }

    fn ordered(&self) -> bool {
        true
    }

    fn home(&self, s: &Service, run_id: &str) -> PathBuf {
        s.config
            .data_dir
            .join("runs")
            .join(run_id)
            .join("home/.opencode")
    }

    async fn prepare(&self, _s: &Service, _id: &str, home: &Path) -> Result<()> {
        private_dir(home).await
    }

    async fn clear(&self, home: &Path) -> Result<()> {
        remove_file(&home.join("data/opencode/auth.json")).await
    }

    async fn recover(&self, s: &Service, run_id: &str) -> Result<()> {
        self.clear(&self.home(s, run_id)).await
    }

    async fn access(
        &self,
        s: &Service,
        lease: &Lease,
        _request: &broker::Request,
    ) -> Result<Value> {
        credentials(s, &lease.account_id).await
    }

    async fn redactions(&self, s: &Service, id: &str) -> Result<Vec<String>> {
        Ok(vec![text(&credentials(s, id).await?, "key").into()])
    }

    async fn forget(&self, s: &Service, id: &str) -> Result<()> {
        s.vault.delete(&secret_id(id)).await
    }
}
