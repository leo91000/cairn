//! Codex: ChatGPT sign-in through the app-server device code, credentials in the encrypted
//! vault, usage from `account/rateLimits/read`, and banked resets redeemed at 2% remaining.
use super::{
    Account, AccountState, Driver, KIND, Lease, Login, broker, lenient, remove_directory,
    remove_file, save,
    usage::{self, Resets, Usage},
};
use crate::{
    auth::hex_digest,
    config::{id, now},
    error::{Error, Result, required},
    provider::Provider,
    rpc::{Incoming, Session},
    service::Service,
    skills::{atomic_write, private_dir},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    time::Duration,
};

/// Set once a Codex account is added: Codex runs then never fall back to the host login.
pub const MANAGED: &str = "codex-accounts-enabled";
const CONFIG: &[u8] = b"cli_auth_credentials_store = \"file\"\nforced_login_method = \"chatgpt\"\n";
/// Banked resets are redeemed when the limiting window reaches this remaining percentage.
const RESET_AT: f64 = 2.;
const INVALID_USAGE: &str = "Codex returned invalid usage data.";

const API_KEYS_UNSUPPORTED: &str =
    "Connect a ChatGPT subscription account. API keys are not supported here.";

const RESET_PENDING: &str = "Banked reset redeemed; waiting for refreshed capacity.";

fn secret(id: &str) -> String {
    format!("codex-account:{id}")
}

fn reset_key(id: &str) -> String {
    format!("codex-reset:{id}")
}

/// The tokens of Codex's `auth.json`. The file itself is stored and written back verbatim.
#[derive(Default, Deserialize)]
struct Tokens {
    #[serde(default, deserialize_with = "lenient::string")]
    access_token: String,
    #[serde(default, deserialize_with = "lenient::string")]
    refresh_token: String,
    #[serde(default, deserialize_with = "lenient::string")]
    id_token: String,
    #[serde(default, deserialize_with = "lenient::string")]
    account_id: String,
}

impl Tokens {
    fn of(auth: &Value) -> Self {
        Self::deserialize(&auth["tokens"]).unwrap_or_default()
    }

    /// The ID token's subject.
    fn subject(&self) -> String {
        self.id_token
            .split('.')
            .nth(1)
            .and_then(|s| URL_SAFE_NO_PAD.decode(s).ok())
            .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
            .and_then(|v| v["sub"].as_str().map(str::to_owned))
            .unwrap_or_default()
    }

    /// Whether `other` may belong to another identity than these tokens.
    fn differs_from(&self, other: &Self) -> bool {
        let subject = self.subject();
        (!self.account_id.is_empty() && other.account_id != self.account_id)
            || (!subject.is_empty() && subject != other.subject())
    }
}

fn auth_input(value: Value) -> Result<Value> {
    if Tokens::of(&value).access_token.is_empty() {
        return Err(Error::bad(API_KEYS_UNSUPPORTED));
    }
    Ok(value)
}

/// `account/rateLimits/read`: a general bucket and one bucket per limited model.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RateLimits {
    #[serde(default)]
    rate_limits: Option<Bucket>,
    #[serde(default)]
    rate_limits_by_limit_id: Option<BTreeMap<String, Bucket>>,
    #[serde(default)]
    ordinary_usage_allowed: Option<Value>,
    #[serde(default)]
    rate_limit_reset_credits: Option<Value>,
    #[serde(default, deserialize_with = "lenient::string")]
    account_id: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Bucket {
    #[serde(default, deserialize_with = "lenient::string")]
    limit_name: String,
    #[serde(default, deserialize_with = "lenient::string")]
    limit_id: String,
    #[serde(default, deserialize_with = "lenient::string")]
    normal_model_slug: String,
    #[serde(default, deserialize_with = "lenient::string")]
    rate_limit_reached_type: String,
    #[serde(default, deserialize_with = "lenient::flag")]
    spend_control_reached: bool,
    #[serde(default)]
    primary: Option<RawWindow>,
    #[serde(default)]
    secondary: Option<RawWindow>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawWindow {
    #[serde(default)]
    used_percent: Option<serde_json::Number>,
    #[serde(default, deserialize_with = "lenient::integer")]
    window_duration_mins: Option<i64>,
    #[serde(default, deserialize_with = "lenient::integer")]
    resets_at: Option<i64>,
}

impl RawWindow {
    fn valid(&self) -> bool {
        self.used_percent
            .as_ref()
            .and_then(serde_json::Number::as_f64)
            .is_some_and(|n| n.is_finite() && n >= 0.)
    }
}

impl Bucket {
    fn windows(&self) -> impl Iterator<Item = (&'static str, &RawWindow)> {
        [("primary", &self.primary), ("secondary", &self.secondary)]
            .into_iter()
            .filter_map(|(slot, window)| window.as_ref().map(|w| (slot, w)))
    }

    fn reached(&self) -> bool {
        !self.rate_limit_reached_type.is_empty() || self.spend_control_reached
    }

    /// The models a per-model bucket limits, by every name Codex gives them.
    fn models(&self, key: &str) -> Vec<String> {
        let mut models = Vec::<String>::new();
        for model in [self.normal_model_slug.as_str(), &self.limit_id, key] {
            if !model.is_empty() && !models.iter().any(|m| m == model) {
                models.push(model.to_owned());
            }
        }
        models
    }

    fn add_windows(&self, key: &str, models: &[String], windows: &mut Vec<usage::Window>) {
        for (slot, window) in self.windows() {
            let minutes = window.window_duration_mins;
            let label = if models.is_empty() || self.limit_name.is_empty() {
                usage::duration_label(minutes)
            } else {
                format!("{} · {}", self.limit_name, usage::duration_label(minutes))
            };
            windows.push(usage::Window {
                id: format!("{key}:{slot}"),
                label,
                used_percent: window.used_percent.clone(),
                resets_at: window.resets_at,
                duration_mins: minutes,
                models: models.to_vec(),
                reached: self.reached(),
            });
        }
    }
}

impl RateLimits {
    fn parse(value: Value) -> Result<Self> {
        let limits = Self::deserialize(value).map_err(|_| Error::bad_gateway(INVALID_USAGE))?;
        let Some(general) = &limits.rate_limits else {
            return Err(Error::bad_gateway(INVALID_USAGE));
        };
        let buckets = std::iter::once(general).chain(limits.buckets().map(|(_, b)| b));
        for bucket in buckets {
            if !bucket.windows().all(|(_, window)| window.valid()) {
                return Err(Error::bad_gateway(INVALID_USAGE));
            }
        }
        Ok(limits)
    }

    fn buckets(&self) -> impl Iterator<Item = (&String, &Bucket)> {
        self.rate_limits_by_limit_id.iter().flatten()
    }

    fn usage(&self) -> Usage {
        let mut windows = Vec::new();
        if let Some(general) = &self.rate_limits {
            general.add_windows("main", &[], &mut windows);
        }
        for (key, bucket) in self.buckets() {
            bucket.add_windows(key, &bucket.models(key), &mut windows);
        }
        let resets = self
            .rate_limit_reset_credits
            .as_ref()
            .filter(|credits| credits.is_object())
            .map(|credits| Resets {
                available: credits["availableCount"].as_u64().unwrap_or(0),
                credits: credits["credits"].as_array().cloned().unwrap_or_default(),
            });
        Usage {
            allowed: self
                .ordinary_usage_allowed
                .as_ref()
                .is_none_or(|v| v != false),
            windows,
            checked_at: Some(now()),
            resets,
            ..Usage::default()
        }
    }
}

/// Codex reports a general bucket and one bucket per limited model, each with a short and
/// a long window.
pub fn normalize(limits: &Value) -> Value {
    RateLimits::deserialize(limits)
        .unwrap_or_default()
        .usage()
        .to_value()
}

async fn read_limits(rpc: &mut Session, tokens: &Tokens) -> Result<RateLimits> {
    let limits = RateLimits::parse(rpc.request("account/rateLimits/read", json!({})).await?)?;
    let different = !limits.account_id.is_empty()
        && !tokens.account_id.is_empty()
        && limits.account_id != tokens.account_id;
    if different {
        return Err(Error::conflict(
            "Codex returned usage for a different account. Reconnect this account.",
        ));
    }
    Ok(limits)
}

/// `account/read`.
#[derive(Debug, Default, Deserialize)]
struct Identity {
    #[serde(default)]
    account: IdentityAccount,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct IdentityAccount {
    #[serde(default, rename = "type", deserialize_with = "lenient::string")]
    kind: String,
    #[serde(default, deserialize_with = "lenient::optional_string")]
    email: Option<String>,
    #[serde(default, deserialize_with = "lenient::optional_string")]
    plan_type: Option<String>,
}

/// Saves the credentials a Codex home holds, refusing a sign-in to a different account.
async fn capture(s: &Service, id: &str, home: &Path) -> Result<()> {
    if home.join("leo-managed-auth").exists() {
        return Ok(());
    }
    let bytes = tokio::fs::read(home.join("auth.json"))
        .await
        .map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                Error::not_found("No saved account credentials in this workspace.")
            } else {
                Error::from(error)
            }
        })?;
    let auth = auth_input(serde_json::from_slice(&bytes)?)?;
    if let Some(previous) = s.vault.get(&secret(id)).await?
        && Tokens::of(&previous).differs_from(&Tokens::of(&auth))
    {
        return Err(super::different_account());
    }
    s.vault.set(&secret(id), &auth).await?;
    tokio::fs::set_permissions(
        home.join("auth.json"),
        std::fs::Permissions::from_mode(0o600),
    )
    .await?;
    Ok(())
}

/// Writes the vault credentials into a manager-only Codex home.
async fn materialize(s: &Service, id: &str, home: &Path) -> Result<()> {
    // A previous process may have refreshed just before a crash or a failed vault write.
    // Recover that authoritative copy before overwriting it.
    if home.join("auth.json").exists() {
        capture(s, id, home).await?;
    }
    let auth = required(
        s.vault.get(&secret(id)).await?,
        "Reconnect this Codex account before using it.",
    )?;
    private_dir(home).await?;
    atomic_write(&home.join("auth.json"), &serde_json::to_vec(&auth)?).await?;
    atomic_write(&home.join("config.toml"), CONFIG).await
}

/// Runs a manager-only app-server session on the account, saving any rotated credentials.
async fn with_session<T>(
    s: &Service,
    id: &str,
    purpose: &str,
    operation: impl AsyncFnOnce(&mut Session, &Path) -> Result<T>,
) -> Result<T> {
    let home = s.config.data_dir.join(purpose).join(id);
    let result = async {
        materialize(s, id, &home).await?;
        let mut session = Session::codex(&s.config, &home, &[], None).await?;
        let result = operation(&mut session, &home).await;
        session.close().await;
        // Save even when the operation failed after rotating credentials.
        capture(s, id, &home).await?;
        result
    }
    .await;
    // Never delete the only fresh credentials if vault persistence failed.
    if capture(s, id, &home).await.is_ok() {
        remove_directory(&home).await?;
    }
    result
}

pub async fn discover_models(s: &Service, id: &str) -> Result<Value> {
    let _guard = s.accounts.lock(id).await;
    s.accounts.get(s, id).await?;
    if s.accounts.busy(s, id).await? {
        return Err(Error::conflict(
            "Account sign-in or recovery is in progress.",
        ));
    }
    with_session(s, id, "codex-model-discovery", async |session, _| {
        crate::models::discover(session).await
    })
    .await
}

/// The fingerprint of the signed-in identity: its ID token subject, or its email.
fn fingerprint(tokens: &Tokens, identity: &IdentityAccount, limits: &RateLimits) -> Result<String> {
    let subject = tokens.subject();
    let subject = if subject.is_empty() {
        identity.email.as_deref().unwrap_or_default()
    } else {
        &subject
    };
    if subject.is_empty() {
        return Err(Error::bad(
            "Codex did not return an account identity. Reconnect this account.",
        ));
    }
    let account_id = if tokens.account_id.is_empty() {
        &limits.account_id
    } else {
        &tokens.account_id
    };
    Ok(hex_digest(&format!("{account_id}:{subject}")))
}

async fn read_usage(
    s: &Service,
    id: &str,
    home: &Path,
    model: &str,
    rpc: &mut Session,
) -> Result<()> {
    let identity = rpc
        .request("account/read", json!({ "refreshToken": false }))
        .await?;
    let identity = Identity::deserialize(&identity).unwrap_or_default().account;
    if identity.kind != "chatgpt" {
        return Err(Error::bad(API_KEYS_UNSUPPORTED));
    }
    let auth = auth_input(serde_json::from_slice(
        &tokio::fs::read(home.join("auth.json")).await?,
    )?)?;
    let tokens = Tokens::of(&auth);
    let limits = read_limits(rpc, &tokens).await?;
    let current = limits.usage();
    let mut account = s.accounts.account(s, id).await?;
    let fingerprint = fingerprint(&tokens, &identity, &limits)?;
    if account.identity.as_ref().is_some_and(|i| *i != fingerprint) {
        return Err(super::different_account());
    }
    account.identity = Some(fingerprint);
    account.email = identity.email;
    account.plan = identity.plan_type;
    account.mark_ready();
    account.usage = Some(current);
    // A natural reset comes first: it leaves banked resets for later.
    super::replenish(&mut account, &Codex);
    super::claim(s, account.clone()).await?;
    let model = account.exhausted_model(model);
    let redemption = reset(s, &account, model, rpc, &tokens).await?;
    let mut account = s.accounts.account(s, id).await?;
    // A confirmed banked reset restores capacity before usage reports lower readings.
    if redemption.confirmed && redemption.usage.available(account.exhausted_model("")) {
        account.exhausted = None;
    }
    account.usage = Some(redemption.usage);
    account.reset_error = Some(redemption.error);
    super::replenish(&mut account, &Codex);
    save(s, &account).await
}

/// A banked reset request, persisted until its outcome is known.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct ResetAttempt {
    params: ResetParams,
    #[serde(default)]
    model: String,
    #[serde(default)]
    confirmed: bool,
}

/// `account/rateLimitResetCredit/consume`.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ResetParams {
    idempotency_key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    credit_id: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct ResetResult {
    outcome: Outcome,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
enum Outcome {
    Reset,
    AlreadyRedeemed,
    NothingToReset,
    NoCredit,
}

/// A Codex banked reset credit.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Credit {
    #[serde(default)]
    id: Value,
    #[serde(default, deserialize_with = "lenient::string")]
    status: String,
    #[serde(default, deserialize_with = "lenient::string")]
    reset_type: String,
    #[serde(default, deserialize_with = "lenient::integer")]
    expires_at: Option<i64>,
}

impl Credit {
    fn usable(&self) -> bool {
        self.status == "available"
            && self.reset_type == "codexRateLimits"
            && self.expires_at.is_none_or(|t| t * 1000 > now())
    }
}

/// The usage after a reset check, why capacity is still pending, and whether a redemption
/// was confirmed.
struct Redemption {
    usage: Usage,
    error: String,
    confirmed: bool,
}

impl Redemption {
    fn new(usage: Usage, error: &str, confirmed: bool) -> Self {
        Self {
            usage,
            error: error.into(),
            confirmed,
        }
    }
}

fn restored(usage: &Usage, model: &str) -> bool {
    !usage.blocked(model) && usage.remaining(model).unwrap_or(0.) > RESET_AT
}

/// Redeems one banked reset when the limiting window reaches 2% remaining. A persisted request
/// key is reused across timeouts and restarts, and a confirmed redemption cannot spend another
/// credit until fresh usage shows capacity again.
async fn reset(
    s: &Service,
    account: &Account,
    model: &str,
    rpc: &mut Session,
    tokens: &Tokens,
) -> Result<Redemption> {
    let current = account.usage();
    let key = reset_key(&account.id);
    let attempt = s
        .store
        .kv(&key)
        .await?
        .and_then(|value| ResetAttempt::deserialize(&value).ok());
    if let Some(attempt) = &attempt
        && let Some(settled) = settle(s, &key, account, attempt, &current).await?
    {
        return Ok(settled);
    }
    let low =
        account.exhausted.is_some() || current.remaining(model).is_some_and(|n| n <= RESET_AT);
    if !account.enabled || !low {
        return Ok(Redemption::new(current, "", false));
    }
    let attempt = match attempt {
        Some(attempt) => attempt,
        None if current.available_resets() == 0 => {
            return Ok(Redemption::new(current, "", false));
        }
        None => {
            let attempt = new_attempt(&current, model);
            s.store
                .set(&key, serde_json::to_value(&attempt)?, None)
                .await?;
            attempt
        }
    };
    let redeemed = redeem(s, &key, account, attempt, current, rpc, tokens).await;
    Ok(redeemed.unwrap_or_else(|_| {
        Redemption::new(
            account.usage(),
            "Unable to confirm banked reset. Retrying automatically without spending another reset.",
            false,
        )
    }))
}

/// Settles an earlier attempt once usage shows its outcome.
async fn settle(
    s: &Service,
    key: &str,
    account: &Account,
    attempt: &ResetAttempt,
    current: &Usage,
) -> Result<Option<Redemption>> {
    let restored = restored(current, &attempt.model);
    if account.exhausted.is_none() && restored {
        s.store.delete(key).await?;
        return Ok(Some(Redemption::new(
            current.clone(),
            "",
            attempt.confirmed,
        )));
    }
    if !attempt.confirmed {
        return Ok(None);
    }
    if !restored {
        return Ok(Some(Redemption::new(current.clone(), RESET_PENDING, false)));
    }
    s.store.delete(key).await?;
    Ok(Some(Redemption::new(current.clone(), "", true)))
}

/// A new request for the usable credit that expires first.
fn new_attempt(current: &Usage, model: &str) -> ResetAttempt {
    let credit = current
        .resets
        .iter()
        .flat_map(|r| &r.credits)
        .map(|c| Credit::deserialize(c).unwrap_or_default())
        .filter(Credit::usable)
        .min_by_key(|c| c.expires_at.unwrap_or(i64::MAX));
    ResetAttempt {
        params: ResetParams {
            idempotency_key: id(),
            credit_id: credit.map(|c| c.id),
        },
        model: model.into(),
        confirmed: false,
    }
}

async fn redeem(
    s: &Service,
    key: &str,
    account: &Account,
    mut attempt: ResetAttempt,
    current: Usage,
    rpc: &mut Session,
    tokens: &Tokens,
) -> Result<Redemption> {
    let result = rpc
        .request(
            "account/rateLimitResetCredit/consume",
            serde_json::to_value(&attempt.params)?,
        )
        .await?;
    let outcome = ResetResult::deserialize(&result)
        .map_err(|_| Error::bad_gateway("Invalid reset result"))?
        .outcome;
    match outcome {
        Outcome::NothingToReset => {
            s.store.delete(key).await?;
            let waiting = "Banked reset is not eligible yet; checking again automatically.";
            Ok(Redemption::new(current, waiting, false))
        }
        Outcome::NoCredit => {
            s.store.delete(key).await?;
            let waiting = "No banked reset available; waiting for capacity or another account.";
            Ok(Redemption::new(current, waiting, false))
        }
        Outcome::Reset | Outcome::AlreadyRedeemed => {
            attempt.confirmed = true;
            s.store
                .set(key, serde_json::to_value(&attempt)?, None)
                .await?;
            s.store
                .audit(
                    "account.reset",
                    json!({ "id": account.id, "outcome": outcome }),
                )
                .await?;
            let current = read_limits(rpc, tokens).await?.usage();
            let confirmed = restored(&current, &attempt.model);
            if confirmed {
                s.store.delete(key).await?;
            }
            let error = if confirmed { "" } else { RESET_PENDING };
            Ok(Redemption::new(current, error, confirmed))
        }
    }
}

/// Records written before usage was normalized kept Codex's raw rate limits.
async fn upgrade_records(s: &Service) -> Result<()> {
    for mut account in s.store.list(KIND).await? {
        let Some(object) = account.as_object_mut() else {
            continue;
        };
        let Some(limits) = object.remove("limits") else {
            continue;
        };
        if !limits.is_null() {
            let mut usage = normalize(&limits);
            usage["checkedAt"] = object.get("checkedAt").cloned().unwrap_or(Value::Null);
            object.insert("usage".into(), usage);
        }
        if let Some(exhausted) = object.get_mut("exhausted").and_then(Value::as_object_mut)
            && let Some(limits) = exhausted.remove("limits")
        {
            exhausted.insert("usage".into(), normalize(&limits));
        }
        s.store.put(KIND, account).await?;
    }
    Ok(())
}

/// Recovers credentials a manager session may have rotated just before a crash.
async fn recover_sessions(s: &Service) -> Result<()> {
    for account in s.accounts.of(s, Provider::Codex).await? {
        for purpose in ["codex-monitor", "codex-model-discovery"] {
            let home = s.config.data_dir.join(purpose).join(&account.id);
            if home.join("auth.json").exists() {
                capture(s, &account.id, &home).await?;
            }
            remove_directory(&home).await?;
        }
    }
    Ok(())
}

/// Adopts an existing ChatGPT login of the host once. The CLI's own file stays untouched.
async fn import_host_login(s: &Service) -> Result<()> {
    let home = s.config.home.join(".codex");
    let Ok(bytes) = tokio::fs::read(home.join("auth.json")).await else {
        return Ok(());
    };
    let signed_in = serde_json::from_slice::<Value>(&bytes)
        .ok()
        .and_then(|v| auth_input(v).ok())
        .is_some();
    if !signed_in {
        return Ok(());
    }
    let account = Account::new(Provider::Codex, "Primary account", AccountState::Ready);
    let imported = account.to_value();
    s.store
        .transaction(move |db| {
            db.put(KIND, &imported)?;
            Codex.added(db)
        })
        .await?;
    capture(s, &account.id, &home).await?;
    s.store
        .audit(
            "account.imported",
            json!({ "id": account.id, "provider": "codex" }),
        )
        .await
}

/// What a run receives from its broker: Codex app-server external tokens.
#[derive(Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ExternalTokens {
    #[serde(default, deserialize_with = "lenient::string")]
    access_token: String,
    #[serde(default, deserialize_with = "lenient::string")]
    chatgpt_account_id: String,
    #[serde(default, deserialize_with = "lenient::optional_string")]
    chatgpt_plan_type: Option<String>,
}

pub struct Codex;

#[async_trait::async_trait]
impl Driver for Codex {
    async fn initialize(&self, s: &Service) -> Result<()> {
        upgrade_records(s).await?;
        // Earlier versions signed in here; an unfinished sign-in never survives a restart.
        remove_directory(&s.config.data_dir.join("codex-login")).await?;
        recover_sessions(s).await?;
        if s.store.kv(MANAGED).await?.is_some() {
            return Ok(());
        }
        import_host_login(s).await
    }

    async fn managed(&self, s: &Service) -> Result<bool> {
        Ok(s.store.kv(MANAGED).await?.is_some())
    }

    /// From the first added account on, Codex runs need a managed account, never the host login.
    fn added(&self, db: &mut crate::store::Db<'_>) -> Result<()> {
        db.set(MANAGED, &Value::Bool(true), None)
    }

    async fn authorize(&self, s: &Service, home: &Path, login: &Login) -> Result<()> {
        private_dir(&home.join(".codex")).await?;
        atomic_write(&home.join(".codex/config.toml"), CONFIG).await?;
        let mut config = s.config.clone();
        config.home = home.to_owned();
        crate::codex_login::run(&config, home, &login.view, &login.stop).await
    }

    async fn adopt(&self, s: &Service, id: &str, home: &Path) -> Result<()> {
        let previous = s.vault.get(&secret(id)).await?;
        capture(s, id, &home.join(".codex")).await?;
        let verified = self.refresh(s, id, &[]).await;
        if verified.is_err() {
            // Keep the previous working credentials when the new sign-in cannot be verified.
            match previous {
                Some(previous) => s.vault.set(&secret(id), &previous).await?,
                None => s.vault.delete(&secret(id)).await?,
            }
        }
        verified
    }

    async fn refresh(&self, s: &Service, id: &str, models: &[String]) -> Result<()> {
        let usage = s.accounts.account(s, id).await?.usage();
        // Watch the model closest to exhaustion among the account's runs.
        let model = models
            .iter()
            .min_by(|a, b| {
                usage
                    .remaining(a)
                    .partial_cmp(&usage.remaining(b))
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .cloned()
            .unwrap_or_default();
        with_session(s, id, "codex-monitor", async |session, home| {
            let result = read_usage(s, id, home, &model, session).await;
            if result.is_ok() {
                // Refresh capabilities with the same session, without failing usage on a
                // model-list outage.
                let _ = crate::models::refresh_from_session(s, id, session).await;
            }
            result
        })
        .await
    }

    async fn due(&self, s: &Service, account: &Account, attempted: i64) -> Result<bool> {
        let model = s.accounts.active(&account.id).await.first().map_or_else(
            || account.exhausted_model("").to_owned(),
            |l| l.model.clone(),
        );
        let pending = s.store.kv(&reset_key(&account.id)).await?.is_some();
        let usage = account.usage();
        let attention = account.exhausted.is_some()
            || pending
            || usage.remaining(&model).is_some_and(|n| n <= super::LOW);
        let redeemable = pending || usage.available_resets() > 0;
        // Poll quickly near exhaustion while a banked reset can restore capacity.
        let interval = if account.enabled && attention && redeemable {
            15_000
        } else {
            60_000
        };
        Ok(now() - attempted >= interval)
    }

    fn fresh_for(&self) -> i64 {
        90_000
    }

    fn requires_usage(&self) -> bool {
        true
    }

    async fn supports(&self, s: &Service, id: &str, model: &str) -> Result<bool> {
        crate::models::account_supports(s, id, model).await
    }

    fn home(&self, s: &Service, run_id: &str) -> PathBuf {
        s.config.data_dir.join("runs").join(run_id).join("codex")
    }

    async fn prepare(&self, _s: &Service, _id: &str, home: &Path) -> Result<()> {
        private_dir(home).await?;
        atomic_write(&home.join("leo-managed-auth"), b"1").await?;
        remove_file(&home.join("auth.json")).await
    }

    async fn clear(&self, home: &Path) -> Result<()> {
        remove_file(&home.join("auth.json")).await
    }

    async fn recover(&self, s: &Service, run_id: &str) -> Result<()> {
        let run = s.config.data_dir.join("runs").join(run_id);
        for home in ["codex", "home/.codex"] {
            self.clear(&run.join(home)).await?;
        }
        Ok(())
    }

    async fn access(&self, s: &Service, lease: &Lease, request: &broker::Request) -> Result<Value> {
        // Rotation is serialized with usage monitoring; ordinary reads only need the vault.
        let _rotation = if request.refresh {
            Some(s.accounts.lock(&lease.account_id).await)
        } else {
            None
        };
        let stored = async || {
            required(
                s.vault.get(&secret(&lease.account_id)).await?,
                "Reconnect this account.",
            )
        };
        let mut tokens = Tokens::of(&stored().await?);
        // Different runs reporting the same expired token share one refresh.
        if request.refresh && request.previous == hex_digest(&tokens.access_token) {
            with_session(s, &lease.account_id, "codex-monitor", async |session, _| {
                session
                    .request("account/read", json!({ "refreshToken": true }))
                    .await
            })
            .await?;
            tokens = Tokens::of(&stored().await?);
        }
        let account = s.accounts.account(s, &lease.account_id).await?;
        if tokens.account_id.is_empty() {
            return Err(Error::bad("Reconnect this account to verify its identity."));
        }
        let external = ExternalTokens {
            access_token: tokens.access_token,
            chatgpt_account_id: tokens.account_id,
            chatgpt_plan_type: account.plan,
        };
        Ok(serde_json::to_value(external)?)
    }

    async fn redactions(&self, s: &Service, id: &str) -> Result<Vec<String>> {
        let auth = s.vault.get(&secret(id)).await?.unwrap_or(Value::Null);
        let tokens = Tokens::of(&auth);
        Ok([tokens.access_token, tokens.refresh_token, tokens.id_token]
            .into_iter()
            .filter(|token| !token.is_empty())
            .collect())
    }

    async fn forget(&self, s: &Service, id: &str) -> Result<()> {
        let id = id.to_owned();
        s.store
            .transaction(move |db| {
                db.delete(&format!("mcp-secret:{}", secret(&id)))?;
                db.delete(&reset_key(&id))?;
                db.delete(&format!("codex-models:{id}"))
            })
            .await
    }
}

/// The run side of the broker: Codex app-server external-token authentication.
pub struct Client {
    path: PathBuf,
    previous: String,
    account: String,
}

impl Client {
    pub fn new(home: &Path) -> Option<Self> {
        Self::from_socket(broker::socket(home))
    }

    pub fn from_socket(path: PathBuf) -> Option<Self> {
        path.exists().then_some(Self {
            path,
            previous: String::new(),
            account: String::new(),
        })
    }

    pub async fn tokens(&mut self, refresh: bool) -> Result<Value> {
        let request = broker::Request {
            refresh,
            previous: self.previous.clone(),
        };
        let result = broker::request(&self.path, &request, Duration::from_secs(9)).await?;
        let tokens = ExternalTokens::deserialize(&result).unwrap_or_default();
        let switched = !self.account.is_empty() && tokens.chatgpt_account_id != self.account;
        if tokens.access_token.is_empty() || tokens.chatgpt_account_id.is_empty() || switched {
            return Err(Error::unavailable("Account authentication is unavailable."));
        }
        self.previous = hex_digest(&tokens.access_token);
        self.account = tokens.chatgpt_account_id;
        Ok(result)
    }

    pub async fn login(&mut self, session: &mut Session) -> Result<()> {
        let started = std::time::Instant::now();
        let mut tokens = self.tokens(false).await?;
        tracing::info!(target: "leo_performance", operation = "account_broker", event = "tokens_ready",
            elapsed_ms = started.elapsed().as_millis() as u64);
        tokens["type"] = "chatgptAuthTokens".into();
        let result = session.request("account/login/start", tokens).await?;
        if result["type"] != "chatgptAuthTokens" {
            return Err(Error::unavailable(
                "Update Codex to support shared account authentication.",
            ));
        }
        Ok(())
    }

    pub async fn refresh(&mut self, rpc: &crate::rpc::Rpc, incoming: &Incoming) -> Result<()> {
        let id = incoming
            .id
            .clone()
            .ok_or_else(|| Error::bad("Invalid account refresh request."))?;
        let previous = &incoming.params["previousAccountId"];
        if previous.is_string() && *previous != self.account {
            return rpc.reject(id).await;
        }
        match self.tokens(true).await {
            Ok(tokens) => rpc.reply(id, tokens).await,
            Err(_) => rpc.reject(id).await,
        }
    }
}
