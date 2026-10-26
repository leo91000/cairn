//! Model-provider API keys for OpenCode. Keys are encrypted on the manager and
//! obtained through the existing run-scoped broker, never saved in native sessions.
use super::{
    Account, Driver, Lease, Login, broker, remove_file, save,
    usage::{Usage, Window},
};
use crate::{
    auth::hex_digest,
    config::now,
    error::{Error, Result},
    provider::Provider,
    service::Service,
    skills::private_dir,
    validation::text,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};

pub const USAGE_URL: &str = "https://opencode.ai/zen/go/v1/usage";
const USAGE_TTL: i64 = 60_000;

const USAGE_ERROR: &str =
    "OpenCode Go usage is temporarily unavailable. It will be checked again automatically.";

#[derive(Deserialize)]
#[serde(untagged)]
enum GoUsage {
    Current {
        usage: GoWindows,
        #[serde(default, rename = "useBalance")]
        use_balance: bool,
    },
    Legacy {
        #[serde(flatten)]
        windows: GoWindows,
        #[serde(rename = "useBalance")]
        use_balance: bool,
    },
}

#[derive(Deserialize)]
struct GoWindows {
    #[serde(alias = "rollingUsage")]
    rolling: GoWindow,
    #[serde(alias = "weeklyUsage")]
    weekly: GoWindow,
    #[serde(alias = "monthlyUsage")]
    monthly: GoWindow,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GoWindow {
    status: GoStatus,
    #[serde(alias = "percent")]
    usage_percent: f64,
    #[serde(flatten)]
    reset: GoReset,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum GoReset {
    Absolute {
        #[serde(rename = "resetsAt")]
        at: chrono::DateTime<chrono::FixedOffset>,
    },
    Relative {
        #[serde(rename = "resetInSec")]
        seconds: i64,
    },
}

#[derive(Deserialize)]
#[serde(rename_all = "kebab-case")]
enum GoStatus {
    Ok,
    RateLimited,
}

/// Read the current Go envelope and the original response without estimating usage from prices.
fn normalize(bytes: &[u8], checked: i64) -> Result<Usage> {
    let response: GoUsage =
        serde_json::from_slice(bytes).map_err(|_| Error::bad_gateway(USAGE_ERROR))?;

    let (windows, use_balance) = match response {
        GoUsage::Current { usage, use_balance }
        | GoUsage::Legacy {
            windows: usage,
            use_balance,
        } => (usage, use_balance),
    };

    let mut usage = Usage {
        checked_at: Some(checked),
        attempted_at: Some(checked),
        ..Usage::default()
    };

    for (id, label, minutes, window) in [
        ("go:rolling", "5-hour window", 300, windows.rolling),
        ("go:weekly", "Weekly", 10080, windows.weekly),
        ("go:monthly", "Monthly", 43200, windows.monthly),
    ] {
        let reset_limit = 366 * 86400;
        let resets_at = match window.reset {
            GoReset::Absolute { at } => at.timestamp(),
            GoReset::Relative { seconds } if (0..=reset_limit).contains(&seconds) => {
                checked / 1000 + seconds
            }
            GoReset::Relative { .. } => return Err(Error::bad_gateway(USAGE_ERROR)),
        };

        if !(0.0..=100.0).contains(&window.usage_percent)
            || !(0..=checked / 1000 + reset_limit).contains(&resets_at)
        {
            return Err(Error::bad_gateway(USAGE_ERROR));
        }

        usage.windows.push(Window {
            id: id.into(),
            label: label.into(),
            used_percent: serde_json::Number::from_f64(window.usage_percent),
            resets_at: Some(resets_at),
            duration_mins: Some(minutes),
            models: Vec::new(),
            reached: matches!(window.status, GoStatus::RateLimited)
                || window.usage_percent >= 100.0,
        });
    }

    usage.extra.insert("useBalance".into(), use_balance.into());
    Ok(usage)
}

/// Provider response bodies and transport errors never leave this boundary.
async fn fetch_usage(key: &str, endpoint: &str) -> Result<Usage> {
    let unavailable = || Error::bad_gateway(USAGE_ERROR);
    // A loopback fixture can replace the endpoint in integration tests. Provider
    // keys must otherwise go only to the official Go endpoint.
    let url = url::Url::parse(endpoint).map_err(|_| unavailable())?;
    if endpoint != USAGE_URL && !matches!(url.host_str(), Some("127.0.0.1" | "[::1]")) {
        return Err(unavailable());
    }
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|_| unavailable())?;
    let mut response = client
        .get(endpoint)
        .bearer_auth(key)
        .send()
        .await
        .map_err(|_| unavailable())?;
    if response.status() == reqwest::StatusCode::UNAUTHORIZED {
        return Err(Error::bad(
            "Replace this OpenCode Go API key in Connections.",
        ));
    }
    if !response.status().is_success() {
        return Err(unavailable());
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| unavailable())? {
        if bytes.len() + chunk.len() > 64_000 {
            return Err(unavailable());
        }
        bytes.extend_from_slice(&chunk);
    }
    normalize(&bytes, now())
}

async fn refresh_at(s: &Service, id: &str, endpoint: &str) -> Result<()> {
    let credential = credentials(s, id).await?;
    let mut account = s.accounts.account(s, id).await?;
    if credential["modelProvider"] == "opencode-go" {
        let mut usage = account.usage();
        if !usage.attempted_at.is_some_and(|at| now() - at < USAGE_TTL) {
            usage.attempted_at = Some(now());
            match fetch_usage(text(&credential, "key"), endpoint).await {
                Ok(current) => {
                    usage = current;
                    account.mark_ready();
                }
                Err(error) if error.status == 400 => {
                    account.usage = Some(usage);
                    save(s, &account).await?;
                    return Err(error);
                }
                Err(_) => usage.set_error(Some(USAGE_ERROR)),
            }
        }
        account.usage = Some(usage);
    } else {
        account.mark_ready();
    }
    save(s, &account).await
}

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
        account.usage = None;
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
        refresh_at(s, id, &s.config.opencode_go_usage_url).await
    }

    async fn due(&self, _s: &Service, account: &Account, attempted: i64) -> Result<bool> {
        let go = account.extra.get("modelProvider").and_then(Value::as_str) == Some("opencode-go");
        let attempted = account
            .usage
            .as_ref()
            .and_then(|u| u.attempted_at)
            .unwrap_or(0)
            .max(attempted);
        let interval = if go { USAGE_TTL } else { self.fresh_for() };
        Ok((go || account.exhausted.is_some()) && now() - attempted >= interval)
    }

    fn fresh_for(&self) -> i64 {
        300_000
    }

    fn requires_usage(&self) -> bool {
        false
    }

    // OpenCode exposes no speed choice.
    async fn supports(&self, s: &Service, id: &str, model: &str, _speed: &str) -> Result<bool> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        accounts::{AccountState, KIND},
        config::Config,
    };
    use axum::{
        Router,
        extract::State,
        http::{HeaderMap, StatusCode},
        routing::get,
    };
    use std::sync::{Arc, Mutex};
    use tempfile::TempDir;

    const KEY: &str = "synthetic-go-usage-key";
    const MODEL: &str = "auto/ling-3-1-flash-free";

    fn reading(monthly: u64, balance: bool) -> Value {
        json!({
            "useBalance": balance,
            "rollingUsage": { "status": "ok", "usagePercent": 25, "resetInSec": 7200 },
            "weeklyUsage": { "status": "ok", "usagePercent": 60, "resetInSec": 86400 },
            "monthlyUsage": { "status": if monthly >= 100 { "rate-limited" } else { "ok" }, "usagePercent": monthly, "resetInSec": 604800 },
        })
    }

    fn current_reading(monthly: u64) -> Value {
        json!({
            "usage": {
                "rolling": { "status": "ok", "percent": 25, "resetsAt": "2027-01-01T02:00:00.000Z" },
                "weekly": { "status": "ok", "percent": 60, "resetsAt": "2027-01-02T00:00:00.000Z" },
                "monthly": { "status": if monthly >= 100 { "rate-limited" } else { "ok" }, "percent": monthly, "resetsAt": "2027-01-08T00:00:00.000Z" },
            },
        })
    }

    #[test]
    fn official_windows_preserve_percentages_and_absolute_reset_times() {
        let usage = normalize(
            &serde_json::to_vec(&reading(85, false)).unwrap(),
            1_800_000_000_000,
        )
        .unwrap();
        assert_eq!(usage.remaining(""), Some(15.0));
        assert_eq!(usage.windows.len(), 3);
        assert_eq!(usage.windows[0].resets_at, Some(1_800_007_200));
        assert_eq!(usage.windows[1].duration_mins, Some(10080));
        assert_eq!(usage.windows[2].label, "Monthly");
        assert_eq!(usage.windows[2].resets_at, Some(1_800_604_800));
        assert!(!usage.blocked(MODEL));
        let old = Usage::from_value(&usage.to_value());
        assert_eq!(old, usage);
    }

    #[tokio::test]
    async fn current_endpoint_response_reports_usage_instead_of_unavailable() {
        let root = TempDir::new().unwrap();
        let (url, _, task) = endpoint(current_reading(100)).await;
        let s = service(&root, &url).await;
        let account = s
            .accounts
            .connect_api_key(&s, Provider::Opencode, "Go", "opencode-go", KEY, None)
            .await
            .unwrap();
        let id = text(&account, "id");
        s.accounts.refresh(&s, id).await.unwrap();
        let usage = s.accounts.account(&s, id).await.unwrap().usage();
        assert_eq!(usage.remaining(""), Some(0.0), "Go quotas must be readable");
        assert_eq!(usage.windows.len(), 3);
        assert_eq!(usage.windows[0].resets_at, Some(1_798_768_800));
        assert_eq!(usage.windows[1].resets_at, Some(1_798_848_000));
        assert_eq!(usage.windows[2].resets_at, Some(1_799_366_400));
        assert!(usage.blocked(MODEL));
        assert!(!usage.uses_balance());
        assert!(usage.extra.get("error").is_none_or(Value::is_null));
        task.abort();
    }

    #[test]
    fn current_windows_preserve_capacity_and_reject_malformed_readings() {
        let checked = 1_798_761_600_000;
        let response = current_reading(85);
        let usage = normalize(&serde_json::to_vec(&response).unwrap(), checked).unwrap();
        assert_eq!(usage.remaining(""), Some(15.0));
        assert!(!usage.blocked(MODEL));
        assert_eq!(usage.checked_at, Some(checked));

        for (path, invalid) in [
            ("/usage/rolling/status", json!("unknown")),
            ("/usage/weekly/percent", json!(101)),
            ("/usage/weekly/percent", Value::Null),
            ("/usage/monthly/resetsAt", json!("not a date")),
            ("/usage/monthly/resetsAt", json!("2030-01-01T00:00:00Z")),
            ("/usage/monthly", Value::Null),
        ] {
            let mut malformed = response.clone();
            *malformed.pointer_mut(path).unwrap() = invalid;
            assert!(normalize(&serde_json::to_vec(&malformed).unwrap(), checked).is_err());
        }
    }

    #[test]
    fn incomplete_or_unrecognized_usage_never_becomes_zero_usage() {
        for value in [
            json!({}),
            json!({ "useBalance": false }),
            {
                let mut v = reading(30, false);
                v["rollingUsage"]["status"] = "unknown".into();
                v
            },
            {
                let mut v = reading(30, false);
                v["weeklyUsage"]["resetInSec"] = (-1).into();
                v
            },
            {
                let mut v = reading(30, false);
                v["monthlyUsage"]["usagePercent"] = 101.into();
                v
            },
            {
                let mut v = reading(30, false);
                v["rollingUsage"]["usagePercent"] = Value::Null;
                v
            },
        ] {
            assert!(normalize(&serde_json::to_vec(&value).unwrap(), now()).is_err());
        }
    }

    type Response = Arc<Mutex<(StatusCode, String)>>;

    async fn endpoint(value: Value) -> (String, Response, tokio::task::JoinHandle<()>) {
        let response = Arc::new(Mutex::new((StatusCode::OK, value.to_string())));
        let app = Router::new()
            .route(
                "/usage",
                get(
                    |State(response): State<Response>, headers: HeaderMap| async move {
                        assert_eq!(headers["authorization"], format!("Bearer {KEY}"));
                        response.lock().unwrap().clone()
                    },
                ),
            )
            .with_state(response.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/usage", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (url, response, task)
    }

    async fn service(root: &TempDir, endpoint: &str) -> Arc<Service> {
        Service::new(Config {
            data_dir: root.path().join("data"),
            home: root.path().join("home"),
            workspace_roots: vec![root.path().to_owned()],
            public_url: "http://localhost:4310".into(),
            host: "127.0.0.1".into(),
            port: 0,
            codex_bin: "codex".into(),
            claude_bin: "claude".into(),
            opencode_bin: concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../tests/fixtures/opencode.mjs"
            )
            .into(),
            opencode_go_usage_url: endpoint.into(),
            concurrency: 1,
            logger: false,
            worker_enabled: false,
            runner_url: String::new(),
        })
        .await
        .unwrap()
    }

    async fn expire(s: &Service, id: &str) {
        let mut account = s.accounts.account(s, id).await.unwrap();
        if let Some(usage) = &mut account.usage {
            usage.attempted_at = Some(now() - USAGE_TTL - 1);
        }
        save(s, &account).await.unwrap();
    }

    #[tokio::test]
    async fn go_quotas_drive_ordered_fallback_recovery_and_balance_continuation() {
        let root = TempDir::new().unwrap();
        let (url, response, task) = endpoint(reading(85, false)).await;
        let s = service(&root, &url).await;
        let go = s
            .accounts
            .connect_api_key(&s, Provider::Opencode, "Go", "opencode-go", KEY, None)
            .await
            .unwrap();
        let zen = s
            .accounts
            .connect_api_key(
                &s,
                Provider::Opencode,
                "Zen",
                "opencode",
                "synthetic-zen-key",
                None,
            )
            .await
            .unwrap();
        let go_id = text(&go, "id");
        s.accounts
            .reorder(
                &s,
                Provider::Opencode,
                &[go_id.into(), text(&zen, "id").into()],
            )
            .await
            .unwrap();
        s.opencode.models(&s).await.unwrap();
        s.accounts.refresh(&s, go_id).await.unwrap();
        let lease = s
            .accounts
            .acquire(&s, "before-limit", Provider::Opencode, MODEL)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(lease.account_id, go_id);
        s.accounts.release(&lease).await.unwrap();

        *response.lock().unwrap() = (StatusCode::OK, reading(100, false).to_string());
        expire(&s, go_id).await;
        s.accounts.refresh(&s, go_id).await.unwrap();
        let lease = s
            .accounts
            .acquire(&s, "after-limit", Provider::Opencode, MODEL)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(lease.account_id, text(&zen, "id"));
        s.accounts.release(&lease).await.unwrap();
        let listed = s.accounts.list(&s).await.unwrap();
        let limited = listed.iter().find(|a| a["id"] == go_id).unwrap();
        assert_eq!(limited["status"], "waiting");
        assert_eq!(limited["remainingPercent"].as_f64(), Some(0.0));
        assert!(!serde_json::to_string(&listed).unwrap().contains(KEY));
        assert!(
            s.accounts
                .acquire(
                    &s,
                    "pinned-limit",
                    Provider::Opencode,
                    "opencode-go/ling-3.1-flash-free"
                )
                .await
                .is_err()
        );

        s.accounts.exhausted(&s, go_id, MODEL).await.unwrap();
        *response.lock().unwrap() = (StatusCode::OK, reading(10, false).to_string());
        expire(&s, go_id).await;
        s.accounts.refresh(&s, go_id).await.unwrap();
        let recovered = s.accounts.account(&s, go_id).await.unwrap();
        assert!(recovered.exhausted.is_none());
        let lease = s
            .accounts
            .acquire(&s, "after-reset", Provider::Opencode, MODEL)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(lease.account_id, go_id);
        s.accounts.release(&lease).await.unwrap();

        *response.lock().unwrap() = (StatusCode::OK, reading(100, true).to_string());
        expire(&s, go_id).await;
        s.accounts.refresh(&s, go_id).await.unwrap();
        let lease = s
            .accounts
            .acquire(&s, "with-balance", Provider::Opencode, MODEL)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(lease.account_id, go_id);
        s.accounts.release(&lease).await.unwrap();
        let usage = s.accounts.account(&s, go_id).await.unwrap().usage();
        assert_eq!(usage.remaining(MODEL), Some(0.0));
        assert!(usage.available(MODEL));
        assert!(usage.uses_balance());

        // A rolling reset does not measure restored Zen credits while the
        // monthly quota stays exhausted: keep the fence until the bounded retry.
        s.accounts.exhausted(&s, go_id, MODEL).await.unwrap();
        let mut rolling_reset = reading(100, true);
        rolling_reset["rollingUsage"]["usagePercent"] = 0.into();
        *response.lock().unwrap() = (StatusCode::OK, rolling_reset.to_string());
        expire(&s, go_id).await;
        s.accounts.refresh(&s, go_id).await.unwrap();
        assert!(
            s.accounts
                .account(&s, go_id)
                .await
                .unwrap()
                .exhausted
                .is_some()
        );
        let lease = s
            .accounts
            .acquire(&s, "balance-limited", Provider::Opencode, MODEL)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(lease.account_id, text(&zen, "id"));
        s.accounts.release(&lease).await.unwrap();

        let mut account = s.accounts.account(&s, go_id).await.unwrap();
        account.exhausted.as_mut().unwrap().at = now() - Opencode.fresh_for() - 1;
        save(&s, &account).await.unwrap();
        expire(&s, go_id).await;
        s.accounts.refresh(&s, go_id).await.unwrap();
        assert!(
            s.accounts
                .account(&s, go_id)
                .await
                .unwrap()
                .exhausted
                .is_none()
        );
        let lease = s
            .accounts
            .acquire(&s, "balance-retry", Provider::Opencode, MODEL)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(lease.account_id, go_id);
        s.accounts.release(&lease).await.unwrap();

        let replaced = s
            .accounts
            .connect_api_key(
                &s,
                Provider::Opencode,
                "Go",
                "opencode-go",
                "replacement-go-key",
                Some(go_id),
            )
            .await
            .unwrap();
        assert!(replaced["usage"].is_null());
        task.abort();
    }

    #[tokio::test]
    async fn admission_preserves_refresh_results_received_while_waiting_for_the_account() {
        let root = TempDir::new().unwrap();
        let (url, _response, task) = endpoint(reading(85, false)).await;
        let s = service(&root, &url).await;
        let go = s
            .accounts
            .connect_api_key(&s, Provider::Opencode, "Go", "opencode-go", KEY, None)
            .await
            .unwrap();
        let zen = s
            .accounts
            .connect_api_key(
                &s,
                Provider::Opencode,
                "Zen",
                "opencode",
                "synthetic-zen-key",
                None,
            )
            .await
            .unwrap();
        let go_id = text(&go, "id");
        let zen_id = text(&zen, "id");
        s.accounts
            .reorder(&s, Provider::Opencode, &[go_id.into(), zen_id.into()])
            .await
            .unwrap();
        s.opencode.models(&s).await.unwrap();

        for (monthly, rejected) in [(100, false), (85, true), (30, false)] {
            let mut account = s.accounts.account(&s, go_id).await.unwrap();
            account.mark_ready();
            account.usage =
                Some(normalize(&serde_json::to_vec(&reading(85, false)).unwrap(), now()).unwrap());
            save(&s, &account).await.unwrap();

            // Hold exactly the lock used during a provider request. Wait until
            // admission has selected its snapshot and queued behind this lock.
            let guard = s.accounts.lock(go_id).await;
            let account_lock = s.accounts.locks.lock().await.get(go_id).unwrap().clone();
            let admission_service = s.clone();
            let admission = tokio::spawn(async move {
                admission_service
                    .accounts
                    .acquire(
                        &admission_service,
                        "concurrent-refresh",
                        Provider::Opencode,
                        MODEL,
                    )
                    .await
            });
            tokio::time::timeout(Duration::from_secs(10), async {
                loop {
                    let waiting = Arc::strong_count(&account_lock) >= 4;
                    if waiting {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();

            account.usage = Some(
                normalize(
                    &serde_json::to_vec(&reading(monthly, false)).unwrap(),
                    now(),
                )
                .unwrap(),
            );
            if rejected {
                account.state = AccountState::Error;
                account.error = "Replace this OpenCode Go API key in Connections.".into();
            }
            let expected_usage = account.usage();
            let expected_state = account.state;
            save(&s, &account).await.unwrap();
            drop(guard);

            let lease = admission.await.unwrap().unwrap().unwrap();
            assert_eq!(
                lease.account_id,
                if monthly == 100 || rejected {
                    zen_id
                } else {
                    go_id
                }
            );
            let current = s.accounts.account(&s, go_id).await.unwrap();
            assert_eq!(current.usage(), expected_usage);
            assert_eq!(current.state, expected_state);
            assert_eq!(current.error, account.error);
            s.accounts.release(&lease).await.unwrap();
        }
        task.abort();
    }

    #[tokio::test]
    async fn unavailable_usage_keeps_last_reading_and_throttles_retries() {
        let root = TempDir::new().unwrap();
        let (url, response, task) = endpoint(reading(35, false)).await;
        let s = service(&root, &url).await;
        let mut account = Account::new(Provider::Opencode, "Go", AccountState::Ready);
        account
            .extra
            .insert("modelProvider".into(), "opencode-go".into());
        let id = account.id.clone();
        s.store.put(KIND, account.to_value()).await.unwrap();
        s.vault
            .set(
                &secret_id(&id),
                &json!({ "modelProvider": "opencode-go", "key": KEY }),
            )
            .await
            .unwrap();
        s.accounts.refresh(&s, &id).await.unwrap();
        let previous = s.accounts.account(&s, &id).await.unwrap().usage();
        for (status, body) in [
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("private provider detail: {KEY}"),
            ),
            (StatusCode::OK, "{}".into()),
            (StatusCode::OK, "x".repeat(65_000)),
            (StatusCode::FOUND, KEY.into()),
            (StatusCode::FORBIDDEN, KEY.into()),
        ] {
            *response.lock().unwrap() = (status, body);
            expire(&s, &id).await;
            s.accounts.refresh(&s, &id).await.unwrap();
            let account = s.accounts.account(&s, &id).await.unwrap();
            let usage = account.usage();
            assert_eq!(usage.windows, previous.windows);
            assert_eq!(usage.checked_at, previous.checked_at);
            assert_eq!(usage.extra["error"], USAGE_ERROR);
            assert_eq!(account.state, AccountState::Ready);
            assert!(!Opencode.due(&s, &account, 0).await.unwrap());
            assert!(!account.to_value().to_string().contains(KEY));
        }
        *response.lock().unwrap() = (StatusCode::OK, reading(80, false).to_string());
        s.accounts.refresh(&s, &id).await.unwrap();
        assert_eq!(
            s.accounts.account(&s, &id).await.unwrap().usage().windows,
            previous.windows
        );
        expire(&s, &id).await;
        s.accounts.refresh(&s, &id).await.unwrap();
        assert_eq!(
            s.accounts
                .account(&s, &id)
                .await
                .unwrap()
                .usage()
                .remaining(""),
            Some(20.0)
        );
        *response.lock().unwrap() = (StatusCode::UNAUTHORIZED, KEY.into());
        let error = fetch_usage(KEY, &url).await.unwrap_err();
        assert_eq!(error.status, 400);
        assert!(!error.message.contains(KEY));
        expire(&s, &id).await;
        s.accounts.refresh(&s, &id).await.unwrap();
        let account = s.accounts.account(&s, &id).await.unwrap();
        assert_eq!(account.state, AccountState::Error);
        assert!(!account.error.contains(KEY));
        assert!(!Opencode.due(&s, &account, now()).await.unwrap());

        // Neither a TTL skip nor a failed usage request authenticates this key.
        let rejected = account.error;
        for (status, body) in [
            (StatusCode::INTERNAL_SERVER_ERROR, KEY.into()),
            (StatusCode::OK, "{}".into()),
        ] {
            *response.lock().unwrap() = (status, body);
            s.accounts.refresh(&s, &id).await.unwrap();
            assert_eq!(
                s.accounts.account(&s, &id).await.unwrap().state,
                AccountState::Error
            );
            expire(&s, &id).await;
            s.accounts.refresh(&s, &id).await.unwrap();
            let account = s.accounts.account(&s, &id).await.unwrap();
            assert_eq!(account.state, AccountState::Error);
            assert_eq!(account.error, rejected);
            assert!(!account.to_value().to_string().contains(KEY));
            assert!(
                s.accounts
                    .acquire(&s, "rejected", Provider::Opencode, "")
                    .await
                    .is_err()
            );
        }

        *response.lock().unwrap() = (StatusCode::OK, reading(80, false).to_string());
        expire(&s, &id).await;
        s.accounts.refresh(&s, &id).await.unwrap();
        let account = s.accounts.account(&s, &id).await.unwrap();
        assert_eq!(account.state, AccountState::Ready);
        assert!(account.error.is_empty());
        task.abort();
    }

    #[tokio::test]
    async fn usage_endpoint_overrides_cannot_send_keys_to_another_provider() {
        let error = fetch_usage(KEY, "https://example.test/usage")
            .await
            .unwrap_err();
        assert_eq!(error.message, USAGE_ERROR);
        assert!(!error.message.contains(KEY));
    }

    #[tokio::test]
    async fn other_providers_never_call_the_go_usage_endpoint() {
        let root = TempDir::new().unwrap();
        let s = service(&root, "http://127.0.0.1:1/must-not-be-called").await;
        let account = s
            .accounts
            .connect_api_key(&s, Provider::Opencode, "Zen", "opencode", "zen-key", None)
            .await
            .unwrap();
        let id = text(&account, "id");
        refresh_at(&s, id, "http://127.0.0.1:1/must-not-be-called")
            .await
            .unwrap();
        let account = s.accounts.account(&s, id).await.unwrap();
        assert!(account.usage.is_none());
        assert!(!Opencode.due(&s, &account, 0).await.unwrap());
    }
}
