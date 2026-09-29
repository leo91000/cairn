//! Codex: ChatGPT sign-in through the app-server device code, credentials in the encrypted
//! vault, usage from `account/rateLimits/read`, and banked resets redeemed at 2% remaining.
use super::{Driver, KIND, Lease, Login, broker, remove_directory, remove_file, usage};
use crate::{
    auth::hex_digest,
    config::{id, now},
    error::{Error, Result, required},
    provider::Provider,
    rpc::{Incoming, Session},
    service::Service,
    skills::{atomic_write, private_dir},
    store::merge,
    validation::text,
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};

/// Set once a Codex account is added: Codex runs then never fall back to the host login.
pub const MANAGED: &str = "codex-accounts-enabled";
const CONFIG: &[u8] = b"cli_auth_credentials_store = \"file\"\nforced_login_method = \"chatgpt\"\n";
/// Banked resets are redeemed when the limiting window reaches this remaining percentage.
const RESET_AT: f64 = 2.;

fn secret(id: &str) -> String {
    format!("codex-account:{id}")
}

fn reset_key(id: &str) -> String {
    format!("codex-reset:{id}")
}

fn subject(token: &str) -> String {
    token
        .split('.')
        .nth(1)
        .and_then(|s| URL_SAFE_NO_PAD.decode(s).ok())
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .and_then(|v| v["sub"].as_str().map(str::to_owned))
        .unwrap_or_default()
}

fn auth_input(value: Value) -> Result<Value> {
    if text(&value["tokens"], "access_token").is_empty() {
        return Err(Error::bad(
            "Connect a ChatGPT subscription account. API keys are not supported here.",
        ));
    }
    Ok(value)
}

fn limits_input(value: Value) -> Result<Value> {
    if !value["rateLimits"].is_object() {
        return Err(Error::bad_gateway("Codex returned invalid usage data."));
    }
    let mut all = vec![&value["rateLimits"]];
    if let Some(b) = value["rateLimitsByLimitId"].as_object() {
        all.extend(b.values());
    }
    for bucket in all {
        for key in ["primary", "secondary"] {
            let window = &bucket[key];
            if !window.is_null()
                && window["usedPercent"]
                    .as_f64()
                    .is_none_or(|n| !n.is_finite() || n < 0.)
            {
                return Err(Error::bad_gateway("Codex returned invalid usage data."));
            }
        }
    }
    Ok(value)
}

/// Codex reports a general bucket and one bucket per limited model, each with a short and
/// a long window.
pub fn normalize(limits: &Value) -> Value {
    let mut windows = Vec::new();
    let mut add = |key: &str, bucket: &Value, models: Vec<String>| {
        for slot in ["primary", "secondary"] {
            let window = &bucket[slot];
            if window.is_null() {
                continue;
            }
            let minutes = window["windowDurationMins"].as_i64();
            let name = text(bucket, "limitName");
            let label = if models.is_empty() || name.is_empty() {
                usage::duration_label(minutes)
            } else {
                format!("{name} · {}", usage::duration_label(minutes))
            };
            windows.push(json!({
                "id": format!("{key}:{slot}"),
                "label": label,
                "usedPercent": window["usedPercent"],
                "resetsAt": window["resetsAt"],
                "durationMins": minutes,
                "models": models,
                "reached": !text(bucket,"rateLimitReachedType").is_empty() || bucket["spendControlReached"]==true
            }));
        }
    };
    add("main", &limits["rateLimits"], Vec::new());
    for (key, bucket) in limits["rateLimitsByLimitId"]
        .as_object()
        .into_iter()
        .flatten()
    {
        let mut models = Vec::new();
        for model in [
            text(bucket, "normalModelSlug"),
            text(bucket, "limitId"),
            key,
        ] {
            if !model.is_empty() && !models.iter().any(|m| m == model) {
                models.push(model.to_owned());
            }
        }
        add(key, bucket, models);
    }
    let credits = &limits["rateLimitResetCredits"];
    json!({
        "allowed": limits["ordinaryUsageAllowed"] != false,
        "windows": windows,
        "checkedAt": now(),
        "resets": if credits.is_object() {json!({
            "available": credits["availableCount"].as_u64().unwrap_or(0),
            "credits": credits["credits"].as_array().cloned().unwrap_or_default()
        })} else {Value::Null}
    })
}

async fn read_limits(rpc: &mut Session, auth: &Value) -> Result<Value> {
    let limits = limits_input(rpc.request("account/rateLimits/read", json!({})).await?)?;
    if !text(&limits, "accountId").is_empty()
        && !text(&auth["tokens"], "account_id").is_empty()
        && limits["accountId"] != auth["tokens"]["account_id"]
    {
        return Err(Error::conflict(
            "Codex returned usage for a different account. Reconnect this account.",
        ));
    }
    Ok(limits)
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
        && ((!text(&previous["tokens"], "account_id").is_empty()
            && auth["tokens"]["account_id"] != previous["tokens"]["account_id"])
            || (!subject(text(&previous["tokens"], "id_token")).is_empty()
                && subject(text(&previous["tokens"], "id_token"))
                    != subject(text(&auth["tokens"], "id_token"))))
    {
        return Err(super::different_account());
    }
    s.vault.set(&secret(id), &auth).await?;
    use std::os::unix::fs::PermissionsExt;
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

async fn read_usage(
    s: &Service,
    id: &str,
    home: &Path,
    model: &str,
    rpc: &mut Session,
) -> Result<()> {
    let identity = rpc
        .request("account/read", json!({"refreshToken": false}))
        .await?;
    if identity["account"]["type"] != "chatgpt" {
        return Err(Error::bad(
            "Connect a ChatGPT subscription account. API keys are not supported here.",
        ));
    }
    let auth = auth_input(serde_json::from_slice(
        &tokio::fs::read(home.join("auth.json")).await?,
    )?)?;
    let limits = read_limits(rpc, &auth).await?;
    let current = normalize(&limits);
    let mut account = s.accounts.get(s, id).await?;
    let subject = subject(text(&auth["tokens"], "id_token"));
    let subject = if subject.is_empty() {
        text(&identity["account"], "email")
    } else {
        &subject
    };
    if subject.is_empty() {
        return Err(Error::bad(
            "Codex did not return an account identity. Reconnect this account.",
        ));
    }
    let account_id = text(&auth["tokens"], "account_id");
    let account_id = if account_id.is_empty() {
        text(&limits, "accountId")
    } else {
        account_id
    };
    let fingerprint = hex_digest(&format!("{account_id}:{subject}"));
    if account["identity"].is_string() && account["identity"] != fingerprint {
        return Err(super::different_account());
    }
    merge(
        &mut account,
        &json!({
            "identity": fingerprint,
            "email": identity["account"]["email"],
            "plan": identity["account"]["planType"],
            "state": "ready",
            "checkedAt": now(),
            "error": "",
            "usage": current
        }),
    );
    // A natural reset comes first: it leaves banked resets for later.
    super::replenish(&mut account, &Codex);
    super::claim(s, account.clone()).await?;
    let model = if account["exhausted"].is_null() {
        model
    } else {
        text(&account["exhausted"], "model")
    };
    let (current, reset_error, confirmed) = reset(s, &account, model, rpc, &auth).await?;
    let mut account = s.accounts.get(s, id).await?;
    let model = text(&account["exhausted"], "model").to_owned();
    // A confirmed banked reset restores capacity before usage reports lower readings.
    if confirmed
        && !usage::blocked(&current, &model)
        && usage::remaining(&current, &model).unwrap_or(0.) > 0.
    {
        account["exhausted"] = Value::Null;
    }
    merge(
        &mut account,
        &json!({"usage": current,"resetError": reset_error}),
    );
    super::replenish(&mut account, &Codex);
    s.store.put(KIND, account).await?;
    Ok(())
}

/// Redeems one banked reset when the limiting window reaches 2% remaining. A persisted request
/// key is reused across timeouts and restarts, and a confirmed redemption cannot spend another
/// credit until fresh usage shows capacity again.
async fn reset(
    s: &Service,
    account: &Value,
    model: &str,
    rpc: &mut Session,
    auth: &Value,
) -> Result<(Value, String, bool)> {
    let mut current = account["usage"].clone();
    let key = reset_key(text(account, "id"));
    let mut attempt = s.store.kv(&key).await?;
    let restored = |current: &Value, model: &str| {
        !usage::blocked(current, model) && usage::remaining(current, model).unwrap_or(0.) > RESET_AT
    };
    if let Some(a) = &attempt {
        if account["exhausted"].is_null() && restored(&current, text(a, "model")) {
            s.store.delete(&key).await?;
            return Ok((current, String::new(), a["confirmed"] == true));
        }
        if a["confirmed"] == true {
            if !restored(&current, text(a, "model")) {
                return Ok((
                    current,
                    "Banked reset redeemed; waiting for refreshed capacity.".into(),
                    false,
                ));
            }
            s.store.delete(&key).await?;
            return Ok((current, String::new(), true));
        }
    }
    if account["enabled"] != true
        || (account["exhausted"].is_null()
            && usage::remaining(&current, model).is_none_or(|n| n > RESET_AT))
    {
        return Ok((current, String::new(), false));
    }
    if attempt.is_none() && current["resets"]["available"].as_u64().unwrap_or(0) == 0 {
        return Ok((current, String::new(), false));
    }
    if attempt.is_none() {
        let credit = current["resets"]["credits"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|c| {
                c["status"] == "available"
                    && c["resetType"] == "codexRateLimits"
                    && c["expiresAt"].as_i64().is_none_or(|t| t * 1000 > now())
            })
            .min_by_key(|c| c["expiresAt"].as_i64().unwrap_or(i64::MAX));
        let mut value =
            json!({"params": {"idempotencyKey": id()},"model": model,"confirmed": false});
        if let Some(credit) = credit {
            value["params"]["creditId"] = credit["id"].clone();
        }
        s.store.set(&key, value.clone(), None).await?;
        attempt = Some(value);
    }
    let mut attempt = attempt.unwrap();
    let result = async {
        let result = rpc
            .request(
                "account/rateLimitResetCredit/consume",
                attempt["params"].clone(),
            )
            .await?;
        match text(&result, "outcome") {
            outcome @ ("nothingToReset" | "noCredit") => {
                s.store.delete(&key).await?;
                Ok((
                    current.clone(),
                    if outcome == "nothingToReset" {
                        "Banked reset is not eligible yet; checking again automatically."
                    } else {
                        "No banked reset available; waiting for capacity or another account."
                    }
                    .into(),
                    false,
                ))
            }
            "reset" | "alreadyRedeemed" => {
                attempt["confirmed"] = true.into();
                s.store.set(&key, attempt.clone(), None).await?;
                s.store
                    .audit(
                        "account.reset",
                        json!({"id": account["id"],"outcome": result["outcome"]}),
                    )
                    .await?;
                current = normalize(&read_limits(rpc, auth).await?);
                let confirmed = restored(&current, text(&attempt, "model"));
                if confirmed {
                    s.store.delete(&key).await?;
                }
                Ok((
                    current.clone(),
                    if confirmed {
                        String::new()
                    } else {
                        "Banked reset redeemed; waiting for refreshed capacity.".into()
                    },
                    confirmed,
                ))
            }
            _ => Err(Error::bad_gateway("Invalid reset result")),
        }
    }
    .await;
    Ok(result.unwrap_or_else(|_: Error| {
        (
            account["usage"].clone(),
            "Unable to confirm banked reset. Retrying automatically without spending another reset."
                .into(),
            false,
        )
    }))
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
        if let Some(exhausted) = object.get_mut("exhausted").filter(|e| e.is_object())
            && let Some(limits) = exhausted.as_object_mut().unwrap().remove("limits")
        {
            exhausted["usage"] = normalize(&limits);
        }
        s.store.put(KIND, account).await?;
    }
    Ok(())
}

pub struct Codex;

#[async_trait::async_trait]
impl Driver for Codex {
    async fn initialize(&self, s: &Service) -> Result<()> {
        upgrade_records(s).await?;
        // Earlier versions signed in here; an unfinished sign-in never survives a restart.
        remove_directory(&s.config.data_dir.join("codex-login")).await?;
        // Recover credentials a manager session may have rotated just before a crash.
        for account in s.accounts.records(s, Provider::Codex).await? {
            let id = text(&account, "id");
            for purpose in ["codex-monitor", "codex-model-discovery"] {
                let home = s.config.data_dir.join(purpose).join(id);
                if home.join("auth.json").exists() {
                    capture(s, id, &home).await?;
                }
                remove_directory(&home).await?;
            }
        }
        if s.store.kv(MANAGED).await?.is_some() {
            return Ok(());
        }
        // Adopt an existing ChatGPT login of the host once. The CLI's own file stays untouched.
        let home = s.config.home.join(".codex");
        let Ok(bytes) = tokio::fs::read(home.join("auth.json")).await else {
            return Ok(());
        };
        if serde_json::from_slice::<Value>(&bytes)
            .ok()
            .and_then(|v| auth_input(v).ok())
            .is_none()
        {
            return Ok(());
        }
        let account = super::record(Provider::Codex, "Primary account", "ready");
        let imported = account.clone();
        s.store
            .transaction(move |db| {
                db.put(KIND, &imported)?;
                Codex.added(db)
            })
            .await?;
        capture(s, text(&account, "id"), &home).await?;
        s.store
            .audit(
                "account.imported",
                json!({"id": account["id"],"provider": "codex"}),
            )
            .await
    }

    async fn managed(&self, s: &Service) -> Result<bool> {
        Ok(s.store.kv(MANAGED).await?.is_some())
    }

    /// From the first added account on, Codex runs need a managed account, never the host login.
    fn added(&self, db: &mut crate::store::Db<'_>) -> Result<()> {
        db.set(MANAGED, &json!(true), None)
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
        let account = s.accounts.get(s, id).await?;
        // Watch the model closest to exhaustion among the account's runs.
        let model = models
            .iter()
            .min_by(|a, b| {
                usage::remaining(&account["usage"], a)
                    .partial_cmp(&usage::remaining(&account["usage"], b))
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

    async fn due(&self, s: &Service, account: &Value, attempted: i64) -> Result<bool> {
        let id = text(account, "id");
        let model = s
            .accounts
            .active(id)
            .await
            .first()
            .map(|l| l.model.clone())
            .unwrap_or_else(|| text(&account["exhausted"], "model").into());
        let pending = s.store.kv(&reset_key(id)).await?.is_some();
        let attention = !account["exhausted"].is_null()
            || pending
            || usage::remaining(&account["usage"], &model).is_some_and(|n| n <= super::LOW);
        // Poll quickly near exhaustion while a banked reset can restore capacity.
        let interval = if account["enabled"] == true
            && attention
            && (pending
                || account["usage"]["resets"]["available"]
                    .as_u64()
                    .unwrap_or(0)
                    > 0)
        {
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

    async fn access(&self, s: &Service, lease: &Lease, request: &Value) -> Result<Value> {
        // Rotation is serialized with usage monitoring; ordinary reads only need the vault.
        let _rotation = if request["refresh"] == true {
            Some(s.accounts.lock(&lease.account_id).await)
        } else {
            None
        };
        let mut auth = required(
            s.vault.get(&secret(&lease.account_id)).await?,
            "Reconnect this account.",
        )?;
        // Different runs reporting the same expired token share one refresh.
        if request["refresh"] == true
            && request["previous"] == hex_digest(text(&auth["tokens"], "access_token"))
        {
            with_session(s, &lease.account_id, "codex-monitor", async |session, _| {
                session
                    .request("account/read", json!({"refreshToken": true}))
                    .await
            })
            .await?;
            auth = required(
                s.vault.get(&secret(&lease.account_id)).await?,
                "Reconnect this account.",
            )?;
        }
        let account = s.accounts.get(s, &lease.account_id).await?;
        let account_id = text(&auth["tokens"], "account_id");
        if account_id.is_empty() {
            return Err(Error::bad("Reconnect this account to verify its identity."));
        }
        Ok(json!({
            "accessToken": auth["tokens"]["access_token"],
            "chatgptAccountId": account_id,
            "chatgptPlanType": account["plan"]
        }))
    }

    async fn redactions(&self, s: &Service, id: &str) -> Result<Vec<String>> {
        let auth = s.vault.get(&secret(id)).await?.unwrap_or(Value::Null);
        Ok(["access_token", "refresh_token", "id_token"]
            .iter()
            .filter_map(|key| {
                auth["tokens"][key]
                    .as_str()
                    .filter(|v| !v.is_empty())
                    .map(str::to_owned)
            })
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
        let result = broker::request(
            &self.path,
            &json!({"refresh": refresh,"previous": self.previous}),
            Duration::from_secs(9),
        )
        .await?;
        if text(&result, "accessToken").is_empty()
            || text(&result, "chatgptAccountId").is_empty()
            || (!self.account.is_empty() && result["chatgptAccountId"] != self.account)
        {
            return Err(Error::unavailable("Account authentication is unavailable."));
        }
        self.previous = hex_digest(text(&result, "accessToken"));
        self.account = text(&result, "chatgptAccountId").into();
        Ok(result)
    }

    pub async fn login(&mut self, session: &mut Session) -> Result<()> {
        let mut tokens = self.tokens(false).await?;
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
        if incoming.params["previousAccountId"].is_string()
            && incoming.params["previousAccountId"] != self.account
        {
            return rpc.reject(id).await;
        }
        match self.tokens(true).await {
            Ok(tokens) => rpc.reply(id, tokens).await,
            Err(_) => rpc.reject(id).await,
        }
    }
}
