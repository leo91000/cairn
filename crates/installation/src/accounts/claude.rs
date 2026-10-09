//! Claude Code: sign-in through `claude auth login --claudeai`, one private CLI home per
//! account, usage from the CLI's `get_usage` control request, and OAuth rotation by the
//! manager only. Runs receive access-only credential snapshots.
use super::{
    Account, AccountState, Driver, Lease, Login, Phase, broker, lenient, remove_directory,
    remove_file, save,
    usage::{self, Usage},
};
use crate::{
    auth::hex_digest,
    claude::{Catalog, environment},
    config::{Config, now},
    error::{Error, Result},
    process::{bounded_output, command, read_bounded},
    provider::Provider,
    service::Service,
    skills::{atomic_write, private_dir},
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const TOKEN_URL: &str = "https://platform.claude.com/v1/oauth/token";
// Official Claude Code 2.1.280 default; a per-login clientId takes precedence.
const CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
const LIMIT: usize = 128_000;
const USAGE_TTL: i64 = 300_000;
/// The CLI files that follow an account into a run's home. Credentials never do.
const STATE: &str = ".claude.json";
const CREDENTIALS: &str = ".credentials.json";
/// The longest lifetime accepted for a rotated token.
const MAX_LIFETIME_SECONDS: i64 = 366 * 86400;

/// The credential fields a run may hold. Refresh tokens stay on the manager.
const ACCESS_ONLY: [&str; 5] = [
    "accessToken",
    "expiresAt",
    "scopes",
    "subscriptionType",
    "rateLimitTier",
];

const SIGN_IN_FAILED: &str = "Claude Code could not finish sign-in. Try again.";

/// The account's private CLI home, which holds its refresh credentials.
pub fn account_home(config: &Config, id: &str) -> PathBuf {
    config.data_dir.join("claude-accounts").join(id)
}

fn unavailable() -> Error {
    Error::unavailable("Claude authentication is unavailable. Check the account in Connections.")
}

async fn read_private(path: &Path) -> Result<Option<Vec<u8>>> {
    match tokio::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .await
    {
        Ok(file) => Ok(Some(read_bounded(file, LIMIT).await?)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// The CLI's credential file. It is kept verbatim: only rotation changes its OAuth fields.
async fn credentials(home: &Path) -> Result<Value> {
    let bytes = read_private(&home.join(CREDENTIALS))
        .await
        .map_err(|_| unavailable())?
        .ok_or_else(unavailable)?;
    serde_json::from_slice(&bytes).map_err(|_| unavailable())
}

/// The `claudeAiOauth` fields of the credential file the manager reads.
#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OAuth {
    #[serde(default, deserialize_with = "lenient::string")]
    access_token: String,
    #[serde(default, deserialize_with = "lenient::string")]
    refresh_token: String,
    #[serde(default, deserialize_with = "lenient::integer")]
    expires_at: Option<i64>,
    #[serde(default, deserialize_with = "lenient::optional_string")]
    client_id: Option<String>,
    #[serde(default, deserialize_with = "lenient::strings")]
    scopes: Vec<String>,
}

impl OAuth {
    fn of(credentials: &Value) -> Self {
        Self::deserialize(&credentials["claudeAiOauth"]).unwrap_or_default()
    }

    fn expires_within(&self, milliseconds: i64) -> bool {
        self.expires_at.unwrap_or(0) <= now() + milliseconds
    }
}

/// Access-only credentials: an explicit allowlist keeps refresh tokens on the manager.
fn snapshot(value: &Value) -> Result<Value> {
    let oauth = OAuth::of(value);
    if oauth.access_token.is_empty() || oauth.expires_within(30_000) {
        return Err(unavailable());
    }
    let mut safe = Map::new();
    for key in ACCESS_ONLY {
        if let Some(value) = value["claudeAiOauth"].get(key) {
            safe.insert(key.into(), value.clone());
        }
    }
    Ok(json!({ "claudeAiOauth": safe }))
}

#[derive(Serialize)]
struct RefreshRequest<'a> {
    grant_type: &'static str,
    refresh_token: &'a str,
    client_id: &'a str,
    scope: String,
}

#[derive(Deserialize)]
struct RefreshResponse {
    #[serde(default, deserialize_with = "lenient::string")]
    access_token: String,
    #[serde(default, deserialize_with = "lenient::integer")]
    expires_in: Option<i64>,
    #[serde(default, deserialize_with = "lenient::optional_string")]
    refresh_token: Option<String>,
    #[serde(default, deserialize_with = "lenient::optional_string")]
    scope: Option<String>,
    #[serde(default, deserialize_with = "lenient::integer")]
    refresh_token_expires_in: Option<i64>,
}

fn lifetime(seconds: i64) -> bool {
    seconds > 0 && seconds <= MAX_LIFETIME_SECONDS
}

/// Exchanges the refresh token. Provider errors never leave this function.
async fn rotate(oauth: &OAuth, endpoint: &str) -> Result<RefreshResponse> {
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|_| unavailable())?;
    let body = RefreshRequest {
        grant_type: "refresh_token",
        refresh_token: &oauth.refresh_token,
        client_id: oauth.client_id.as_deref().unwrap_or(CLIENT_ID),
        scope: oauth.scopes.join(" "),
    };
    let mut response = client
        .post(endpoint)
        .json(&body)
        .send()
        .await
        .map_err(|_| unavailable())?;
    if !response.status().is_success() {
        return Err(unavailable());
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| unavailable())? {
        if bytes.len() + chunk.len() > LIMIT {
            return Err(unavailable());
        }
        bytes.extend_from_slice(&chunk);
    }
    let rotated: RefreshResponse = serde_json::from_slice(&bytes).map_err(|_| unavailable())?;
    let valid = rotated.expires_in.is_some_and(|n| n > 60 && lifetime(n));
    if !valid || rotated.access_token.is_empty() {
        return Err(unavailable());
    }
    Ok(rotated)
}

fn apply(credentials: &mut Value, rotated: RefreshResponse) {
    let oauth = &mut credentials["claudeAiOauth"];
    oauth["accessToken"] = rotated.access_token.into();
    oauth["expiresAt"] = (now() + rotated.expires_in.unwrap_or(0) * 1000).into();
    if let Some(token) = rotated.refresh_token.filter(|s| !s.is_empty()) {
        oauth["refreshToken"] = token.into();
    }
    if let Some(scope) = rotated.scope {
        oauth["scopes"] = scope.split_whitespace().collect::<Vec<_>>().into();
    }
    if let Some(seconds) = rotated.refresh_token_expires_in.filter(|n| lifetime(*n)) {
        oauth["refreshTokenExpiresAt"] = (now() + seconds * 1000).into();
    }
}

/// Rotates the login when it is about to expire. The caller holds the account lock through
/// persistence, so concurrent runs share one rotation.
async fn access_at(home: &Path, endpoint: &str) -> Result<Value> {
    let mut value = credentials(home).await?;
    let oauth = OAuth::of(&value);
    if oauth.expires_within(300_000) && !oauth.refresh_token.is_empty() {
        apply(&mut value, rotate(&oauth, endpoint).await?);
        atomic_write(&home.join(CREDENTIALS), &serde_json::to_vec(&value)?).await?;
        tokio::fs::File::open(home).await?.sync_all().await?;
    }
    snapshot(&value)
}

/// Access-only credential storage for a manager-side metadata query. Caller holds the
/// account lock.
pub async fn metadata_credentials(config: &Config, id: &str) -> Result<tempfile::TempDir> {
    let snapshot = access_at(&account_home(config, id), TOKEN_URL).await?;
    let directory = tempfile::tempdir()?;
    atomic_write(
        &directory.path().join(CREDENTIALS),
        &serde_json::to_vec(&snapshot)?,
    )
    .await?;
    Ok(directory)
}

/// `claude auth status`. Only documented, non-secret identity fields are read.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AuthStatus {
    #[serde(default, deserialize_with = "lenient::flag")]
    logged_in: bool,
    #[serde(default, deserialize_with = "lenient::optional_string")]
    email: Option<String>,
    #[serde(default, deserialize_with = "lenient::optional_string")]
    subscription_type: Option<String>,
}

async fn status(config: &Config, home: &Path) -> Result<AuthStatus> {
    private_dir(home).await?;
    let output = bounded_output(
        command(
            &config.claude_bin,
            &["auth".into(), "status".into()],
            &environment(config, home),
            Some(home),
        ),
        Duration::from_secs(15),
        32000,
    )
    .await
    .map_err(|_| Error::unavailable("Claude Code is not installed on the server."))?;
    let value: Value = serde_json::from_str(&output.stdout).map_err(|_| {
        Error::bad_gateway("Update Claude Code: authentication status was not valid JSON.")
    })?;
    let mut status = AuthStatus::deserialize(value).unwrap_or_default();
    status.logged_in &= output.success;
    Ok(status)
}

fn identity(email: &str) -> String {
    hex_digest(&format!("claude:{}", email.trim().to_lowercase()))
}

/// The CLI's `get_usage` answer.
#[derive(Debug, Default, Deserialize)]
struct UsageReport {
    #[serde(default, deserialize_with = "lenient::flag")]
    rate_limits_available: bool,
    #[serde(default, deserialize_with = "lenient::or_default")]
    rate_limits: Limits,
}

#[derive(Debug, Default, Deserialize)]
struct Limits {
    #[serde(default, deserialize_with = "lenient::or_default")]
    five_hour: Limit,
    #[serde(default, deserialize_with = "lenient::or_default")]
    seven_day: Limit,
    #[serde(default, deserialize_with = "lenient::or_default")]
    seven_day_opus: Limit,
    #[serde(default, deserialize_with = "lenient::or_default")]
    seven_day_sonnet: Limit,
    #[serde(default, deserialize_with = "lenient::or_default")]
    seven_day_oauth_apps: Limit,
    #[serde(default, deserialize_with = "lenient::or_default")]
    model_scoped: Vec<Value>,
}

#[derive(Debug, Default, Deserialize)]
struct Limit {
    #[serde(default)]
    utilization: Option<Value>,
    #[serde(default, deserialize_with = "lenient::optional_string")]
    resets_at: Option<String>,
    #[serde(default, deserialize_with = "lenient::optional_string")]
    display_name: Option<String>,
}

impl Limit {
    fn window(
        &self,
        id: String,
        label: &str,
        minutes: i64,
        models: Vec<String>,
    ) -> Option<usage::Window> {
        let used = self
            .utilization
            .as_ref()
            .and_then(Value::as_f64)
            .filter(|n| n.is_finite() && *n >= 0.0)?;
        let resets_at = self
            .resets_at
            .as_deref()
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .map(|date| date.timestamp());
        Some(usage::Window {
            id,
            label: label.into(),
            used_percent: serde_json::Number::from_f64(used),
            resets_at,
            duration_mins: Some(minutes),
            models,
            reached: false,
        })
    }
}

const WEEK: i64 = 10080;

fn usage_windows(value: &Value) -> Result<Vec<usage::Window>> {
    let report = UsageReport::deserialize(value).unwrap_or_default();
    if !report.rate_limits_available {
        return Err(Error::bad_gateway(
            "Usage limits are unavailable for this Claude account.",
        ));
    }
    let limits = &report.rate_limits;
    let mut windows = Vec::new();
    windows.extend(
        limits
            .five_hour
            .window("five_hour".into(), "5-hour window", 300, vec![]),
    );
    windows.extend(
        limits
            .seven_day
            .window("seven_day".into(), "Weekly", WEEK, vec![]),
    );
    for (key, label, model, limit) in [
        (
            "seven_day_opus",
            "Weekly · Opus",
            "opus",
            &limits.seven_day_opus,
        ),
        (
            "seven_day_sonnet",
            "Weekly · Sonnet",
            "sonnet",
            &limits.seven_day_sonnet,
        ),
        (
            "seven_day_oauth_apps",
            "Weekly · OAuth apps",
            "oauth-apps",
            &limits.seven_day_oauth_apps,
        ),
    ] {
        windows.extend(limit.window(key.into(), label, WEEK, vec![model.into()]));
    }
    for (index, row) in limits.model_scoped.iter().take(20).enumerate() {
        let limit = Limit::deserialize(row).unwrap_or_default();
        let Some(name) = limit
            .display_name
            .as_deref()
            .filter(|s| !s.is_empty() && s.len() <= 100)
        else {
            continue;
        };
        let label = format!("Weekly · {name}");
        windows.extend(limit.window(
            format!("model_{index}"),
            &label,
            WEEK,
            vec![name.to_lowercase()],
        ));
    }
    if windows.is_empty() {
        return Err(Error::bad_gateway(
            "Claude Code has not returned usage limits yet.",
        ));
    }
    Ok(windows)
}

/// A model-scoped window names a model family, such as "opus". It also limits each catalog
/// model that runs the family, such as "opus[1m]", or "default" when it resolves to Opus.
fn scope(windows: &mut [usage::Window], catalog: &Catalog) {
    for window in windows {
        let Some(family) = window.models.first().map(|m| m.to_lowercase()) else {
            continue;
        };
        for row in &catalog.models {
            let model = row.model.as_str();
            let resolved = row.resolved_model.as_deref().unwrap_or_default();
            let runs = [model, resolved]
                .iter()
                .any(|name| name.to_lowercase().contains(&family));
            if runs && !window.models.iter().any(|m| m == model) {
                window.models.push(model.into());
            }
        }
    }
}

/// Reads usage at most every five minutes. A failure keeps the last known windows.
async fn read_usage(s: &Service, id: &str, previous: Option<Usage>) -> Usage {
    let mut current = previous.unwrap_or_default();
    if current
        .attempted_at
        .is_some_and(|at| now() - at < USAGE_TTL)
    {
        return current;
    }
    current.attempted_at = Some(now());
    let request = json!({ "subtype": "get_usage", "skip_behaviors": true });
    let windows = crate::claude::query_metadata(s, id, Some(request))
        .await
        .and_then(|v| usage_windows(&v));
    match windows {
        Ok(mut windows) => {
            let catalog = s.store.kv(crate::claude::CATALOG).await.ok().flatten();
            scope(&mut windows, &Catalog::read(catalog));
            current.windows = windows;
            current.checked_at = Some(now());
            current.set_error(None);
        }
        Err(_) => current.set_error(Some(
            "Claude Code usage is temporarily unavailable. It will be checked again automatically.",
        )),
    }
    current
}

fn login_url(output: &str) -> Option<String> {
    output
        .split_whitespace()
        .filter_map(|s| url::Url::parse(s).ok())
        .find(|u| {
            u.scheme() == "https"
                && matches!(
                    u.host_str(),
                    Some(
                        "claude.ai"
                            | "claude.com"
                            | "platform.claude.com"
                            | "console.anthropic.com"
                    )
                )
                && u.username().is_empty()
                && u.password().is_none()
        })
        .map(|u| u.to_string())
}

async fn copy_private(source: &Path, target: &Path) -> Result<()> {
    if let Some(bytes) = read_private(source).await? {
        atomic_write(target, &bytes).await?;
    }
    Ok(())
}

/// Earlier versions connected one Claude account in `DATA_DIR/claude`, shared as the CLI home
/// of local runs. It becomes an ordinary account, and local runs keep their sessions.
async fn migrate(s: &Service) -> Result<()> {
    let legacy = s.config.data_dir.join("claude");
    if !legacy.exists() {
        return Ok(());
    }
    migrate_sessions(s, &legacy).await?;
    if legacy.join(CREDENTIALS).exists() && s.accounts.of(s, Provider::Claude).await?.is_empty() {
        import_legacy(s, &legacy).await?;
    } else {
        remove_directory(&legacy).await?;
    }
    for key in ["claude-status", "claude-usage", "claude-concurrency"] {
        s.store.delete(key).await?;
    }
    Ok(())
}

async fn import_legacy(s: &Service, legacy: &Path) -> Result<()> {
    // A guest may have rotated the refresh token during the old serialized transfer.
    let interrupted = legacy.join("sync-required").exists();
    let saved = s.store.kv("claude-status").await?.unwrap_or(Value::Null);
    let limit = s
        .store
        .kv("claude-concurrency")
        .await?
        .and_then(|v| v.as_u64())
        .unwrap_or(4);
    let state = if interrupted {
        AccountState::Error
    } else {
        AccountState::Ready
    };
    let mut account = Account::new(Provider::Claude, "Claude", state);
    let email = saved["email"].as_str();
    account.email = email.map(str::to_owned);
    account.plan = saved["subscriptionType"].as_str().map(str::to_owned);
    account.identity = email.map(identity);
    account.max_concurrent_runs = Some(limit);
    if interrupted {
        account.error = "Reconnect this account after an interrupted credential transfer.".into();
    }
    let home = account_home(&s.config, &account.id);
    private_dir(&s.config.data_dir.join("claude-accounts")).await?;
    tokio::fs::rename(legacy, &home).await?;
    remove_file(&home.join("sync-required")).await?;
    save(s, &account).await?;
    s.store
        .audit(
            "account.imported",
            json!({ "id": account.id, "provider": "claude" }),
        )
        .await
}

/// Local runs used the shared home, so their session transcripts move into each run.
async fn migrate_sessions(s: &Service, legacy: &Path) -> Result<()> {
    let runs = s
        .store
        .read(|db| {
            db.json_rows(
                "SELECT data FROM runs WHERE json_extract(data,'$.snapshot.agent.provider')='claude' AND json_extract(data,'$.sessionId') IS NOT NULL AND COALESCE(json_extract(data,'$.isolated'),0)!=1",
                [],
            )
        })
        .await?;
    let Ok(mut projects) = tokio::fs::read_dir(legacy.join("projects")).await else {
        return Ok(());
    };
    let mut directories = Vec::new();
    while let Some(entry) = projects.next_entry().await? {
        directories.push(entry.file_name());
    }
    for run in runs {
        let session = format!("{}.jsonl", crate::validation::text(&run, "sessionId"));
        let target = s
            .config
            .data_dir
            .join("runs")
            .join(crate::validation::text(&run, "id"))
            .join("home/.claude/projects");
        for directory in &directories {
            let source = legacy.join("projects").join(directory).join(&session);
            let destination = target.join(directory).join(&session);
            if source.exists() && !destination.exists() {
                private_dir(&target.join(directory)).await?;
                copy_private(&source, &destination).await?;
            }
        }
    }
    Ok(())
}

/// Drives `claude auth login` until it exits, publishing its link and passing pasted codes.
/// Answers whether the CLI reported success.
async fn follow_login(
    s: &Service,
    login: &Login,
    child: &mut tokio::process::Child,
) -> Result<bool> {
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    let mut output = String::new();
    let mut a = [0u8; 4096];
    let mut b = [0u8; 4096];
    let (mut stdout_open, mut stderr_open) = (true, true);
    let deadline = tokio::time::sleep(Duration::from_secs(900));
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            () = login.stop.cancelled() => return Err(Error::conflict("Sign-in cancelled.")),
            () = s.shutdown.cancelled() => return Err(Error::conflict("Sign-in cancelled.")),
            () = &mut deadline => {
                return Err(Error::timeout("Sign-in expired. Start again to get a new link."));
            }
            code = login.code() => {
                if let Some(code) = code {
                    stdin.write_all(code.as_bytes()).await?;
                    stdin.write_all(b"\n").await?;
                }
            }
            status = child.wait() => return Ok(status?.success()),
            n = stdout.read(&mut a), if stdout_open => {
                let n = n?;
                stdout_open = n > 0;
                output.push_str(&String::from_utf8_lossy(&a[..n]));
            }
            n = stderr.read(&mut b), if stderr_open => {
                let n = n?;
                stderr_open = n > 0;
                output.push_str(&String::from_utf8_lossy(&b[..n]));
            }
        }
        if output.len() > 64_000 {
            return Err(Error::bad_gateway(
                "Claude Code sign-in returned too much output.",
            ));
        }
        if let Some(url) = login_url(&output) {
            login.update(|v| {
                v.phase = Phase::Authorizing;
                v.url = Some(url);
                v.accepts_code = true;
            });
        }
    }
}

pub struct Claude;

#[async_trait::async_trait]
impl Driver for Claude {
    async fn initialize(&self, s: &Service) -> Result<()> {
        migrate(s).await
    }

    async fn managed(&self, _s: &Service) -> Result<bool> {
        Ok(true)
    }

    async fn authorize(&self, s: &Service, home: &Path, login: &Login) -> Result<()> {
        let mut cmd = command(
            &s.config.claude_bin,
            &["auth".into(), "login".into(), "--claudeai".into()],
            &environment(&s.config, home),
            Some(home),
        );
        cmd.stdin(std::process::Stdio::piped());
        let mut child = cmd.spawn().map_err(|_| {
            Error::unavailable(
                "Claude Code is not installed. Install the supported CLI on the server.",
            )
        })?;
        let result = follow_login(s, login, &mut child).await;
        // The CLI may already have exited.
        let _ = child.kill().await;
        let _ = child.wait().await;
        match result {
            Ok(true) => Ok(()),
            // CLI errors can contain private state. Never forward them.
            Ok(false) => Err(Error::bad(SIGN_IN_FAILED)),
            Err(error) => Err(error),
        }
    }

    async fn adopt(&self, s: &Service, id: &str, home: &Path) -> Result<()> {
        let signed_in = status(&s.config, home).await?;
        let email = signed_in.email.unwrap_or_default();
        if !signed_in.logged_in || email.is_empty() {
            return Err(Error::bad(SIGN_IN_FAILED));
        }
        let mut account = s.accounts.account(s, id).await?;
        let fingerprint = identity(&email);
        if account.identity.as_ref().is_some_and(|i| *i != fingerprint) {
            return Err(super::different_account());
        }
        account.identity = Some(fingerprint);
        account.email = Some(email);
        account.plan = signed_in.subscription_type;
        account.mark_ready();
        account.usage = None;
        account.exhausted = None;
        super::claim(s, account).await?;
        let target = account_home(&s.config, id);
        private_dir(&target).await?;
        for name in [CREDENTIALS, STATE] {
            copy_private(&home.join(name), &target.join(name)).await?;
        }
        // The model catalog is rediscovered with the new sign-in.
        s.store.delete(crate::claude::CATALOG).await?;
        self.refresh(s, id, &[]).await
    }

    async fn refresh(&self, s: &Service, id: &str, _models: &[String]) -> Result<()> {
        let current = status(&s.config, &account_home(&s.config, id)).await?;
        if !current.logged_in {
            return Err(Error::conflict("Reconnect this Claude account."));
        }
        let mut account = s.accounts.account(s, id).await?;
        let usage = read_usage(s, id, account.usage.take()).await;
        account.email = current.email;
        account.plan = current.subscription_type;
        account.mark_ready();
        account.usage = Some(usage);
        save(s, &account).await
    }

    async fn due(&self, _s: &Service, _account: &Account, attempted: i64) -> Result<bool> {
        Ok(now() - attempted >= USAGE_TTL)
    }

    fn fresh_for(&self) -> i64 {
        3 * USAGE_TTL
    }

    fn requires_usage(&self) -> bool {
        false
    }

    async fn supports(&self, _s: &Service, _id: &str, _model: &str) -> Result<bool> {
        Ok(true)
    }

    fn home(&self, s: &Service, run_id: &str) -> PathBuf {
        s.config
            .data_dir
            .join("runs")
            .join(run_id)
            .join("home/.claude")
    }

    async fn prepare(&self, s: &Service, id: &str, home: &Path) -> Result<()> {
        private_dir(home).await?;
        copy_private(&account_home(&s.config, id).join(STATE), &home.join(STATE)).await?;
        self.clear(home).await
    }

    async fn clear(&self, home: &Path) -> Result<()> {
        remove_file(&home.join(CREDENTIALS)).await
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
        let _rotation = s.accounts.lock(&lease.account_id).await;
        access_at(&account_home(&s.config, &lease.account_id), TOKEN_URL).await
    }

    async fn redactions(&self, s: &Service, id: &str) -> Result<Vec<String>> {
        let value = credentials(&account_home(&s.config, id))
            .await
            .unwrap_or(Value::Null);
        let oauth = OAuth::of(&value);
        Ok([oauth.access_token, oauth.refresh_token]
            .into_iter()
            .filter(|token| !token.is_empty())
            .collect())
    }

    async fn forget(&self, s: &Service, id: &str) -> Result<()> {
        let directory = account_home(&s.config, id);
        if directory.join(CREDENTIALS).exists() {
            // Best effort: let the CLI revoke the login before its files are deleted.
            let _ = bounded_output(
                command(
                    &s.config.claude_bin,
                    &["auth".into(), "logout".into()],
                    &environment(&s.config, &directory),
                    Some(&directory),
                ),
                Duration::from_secs(15),
                32000,
            )
            .await;
        }
        remove_directory(&directory).await
    }
}

/// The run side of the broker: keeps the CLI's credential file current with access-only
/// snapshots.
pub struct Client {
    socket: PathBuf,
    home: PathBuf,
    expires: i64,
    previous: Vec<u8>,
}

impl Client {
    pub fn new(home: &Path) -> Self {
        Self {
            socket: broker::socket(home),
            home: home.into(),
            expires: 0,
            previous: Vec::new(),
        }
    }

    async fn write(&mut self) -> Result<()> {
        // Claude Code runs always ask for current credentials.
        let value = broker::request(&self.socket, &Map::new(), Duration::from_secs(40))
            .await
            .map_err(|_| unavailable())?;
        let value = snapshot(&value)?;
        let bytes = serde_json::to_vec(&value)?;
        let path = self.home.join(CREDENTIALS);
        let unchanged = bytes == self.previous
            && tokio::fs::read(&path).await.ok().as_deref() == Some(bytes.as_slice());
        if !unchanged {
            atomic_write(&path, &bytes).await?;
            self.previous = bytes;
        }
        self.expires = OAuth::of(&value).expires_at.unwrap_or(0);
        Ok(())
    }

    pub async fn sync(&mut self) -> Result<()> {
        let result = self.write().await;
        // Transient manager restarts must not interrupt a still-authenticated conversation.
        if result.is_err() && self.expires > now() + 30_000 {
            return Ok(());
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use tokio::net::UnixListener;

    fn window(models: &[&str]) -> usage::Window {
        serde_json::from_value(json!({ "models": models })).unwrap()
    }

    #[test]
    fn model_windows_limit_the_catalog_models_of_their_family() {
        let catalog = Catalog::read(Some(json!({
            "models": [
                { "model": "opus" },
                { "model": "opus[1m]" },
                { "model": "default", "resolvedModel": "claude-opus-fixture[1m]" },
                { "model": "sonnet", "resolvedModel": "claude-sonnet-fixture" },
            ],
        })));
        let mut windows = vec![window(&[]), window(&["opus"])];
        scope(&mut windows, &catalog);
        assert!(windows[0].models.is_empty());
        assert_eq!(windows[1].models, ["opus", "opus[1m]", "default"]);
    }

    fn stored() -> Value {
        json!({
            "claudeAiOauth": {
                "accessToken": "old-access",
                "refreshToken": "private-refresh",
                "expiresAt": now() - 1000,
                "scopes": ["user:profile", "user:inference"],
                "subscriptionType": "max",
                "clientId": "login-client",
            },
            "otherSecret": "must-stay-on-host",
        })
    }

    #[tokio::test]
    async fn concurrent_requests_rotate_once_and_keep_refresh_state_on_host() {
        let root = tempfile::TempDir::new().unwrap();
        atomic_write(
            &root.path().join(CREDENTIALS),
            &serde_json::to_vec(&stored()).unwrap(),
        )
        .await
        .unwrap();
        let requests = Arc::new(AtomicUsize::new(0));
        let count = requests.clone();
        let token = move |axum::Json(v): axum::Json<Value>| {
            let count = count.clone();
            async move {
                assert_eq!(v["grant_type"], "refresh_token");
                assert_eq!(v["refresh_token"], "private-refresh");
                assert_eq!(v["client_id"], "login-client");
                assert_eq!(v["scope"], "user:profile user:inference");
                count.fetch_add(1, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(50)).await;
                axum::Json(json!({
                    "access_token": "new-access",
                    "refresh_token": "new-private-refresh",
                    "expires_in": 3600,
                    "scope": "user:profile user:inference",
                }))
            }
        };
        let router = axum::Router::new().route("/token", axum::routing::post(token));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/token", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let gate = Arc::new(tokio::sync::Mutex::new(()));
        let mut jobs = Vec::new();
        for _ in 0..8 {
            let gate = gate.clone();
            let home = root.path().to_owned();
            let endpoint = endpoint.clone();
            jobs.push(tokio::spawn(async move {
                let _gate = gate.lock().await;
                access_at(&home, &endpoint).await.unwrap()
            }));
        }
        for job in jobs {
            let value = job.await.unwrap();
            assert_eq!(value["claudeAiOauth"]["accessToken"], "new-access");
            assert!(!value.to_string().contains("refresh"));
            assert!(!value.to_string().contains("must-stay-on-host"));
        }
        assert_eq!(requests.load(Ordering::SeqCst), 1);
        assert_eq!(
            credentials(root.path()).await.unwrap()["claudeAiOauth"]["refreshToken"],
            "new-private-refresh"
        );
        server.abort();
    }

    #[tokio::test]
    async fn failed_refresh_is_private_and_does_not_destroy_credentials() {
        let root = tempfile::TempDir::new().unwrap();
        let original = stored();
        atomic_write(
            &root.path().join(CREDENTIALS),
            &serde_json::to_vec(&original).unwrap(),
        )
        .await
        .unwrap();
        let router = axum::Router::new().route(
            "/token",
            axum::routing::post(|| async {
                (
                    axum::http::StatusCode::UNAUTHORIZED,
                    "private-refresh should never appear in errors",
                )
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/token", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let error = access_at(root.path(), &endpoint).await.unwrap_err();
        assert!(!error.message.contains("private-refresh"));
        assert_eq!(credentials(root.path()).await.unwrap(), original);
        server.abort();
    }

    #[tokio::test]
    async fn client_replaces_credentials_without_refresh_tokens_and_survives_transient_outage() {
        let root = tempfile::TempDir::new().unwrap();
        let socket = root.path().join(broker::SOCKET);
        let listener = UnixListener::bind(&socket).unwrap();
        let mut value = stored();
        value["claudeAiOauth"]["expiresAt"] = (now() + 3600000).into();
        let serve = tokio::spawn(async move {
            for token in ["first", "second"] {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                let mut byte = [0u8; 1];
                while stream.read_exact(&mut byte).await.is_ok() && byte[0] != b'\n' {
                    request.push(byte[0]);
                }
                value["claudeAiOauth"]["accessToken"] = token.into();
                stream
                    .write_all(format!("{value}\n").as_bytes())
                    .await
                    .unwrap();
            }
        });
        let mut client = Client {
            socket,
            home: root.path().into(),
            expires: 0,
            previous: Vec::new(),
        };
        client.sync().await.unwrap();
        assert_eq!(
            credentials(root.path()).await.unwrap()["claudeAiOauth"]["accessToken"],
            "first"
        );
        client.sync().await.unwrap();
        serve.await.unwrap();
        let written = credentials(root.path()).await.unwrap();
        assert_eq!(written["claudeAiOauth"]["accessToken"], "second");
        assert!(!written.to_string().contains("refresh"));
        client.sync().await.unwrap();
        client.expires = 0;
        assert!(client.sync().await.is_err());
    }

    #[test]
    fn usage_windows_scope_model_limits() {
        let windows = usage_windows(&json!({
            "rate_limits_available": true,
            "rate_limits": {
                "five_hour": { "utilization": 25, "resets_at": "2030-01-01T12:00:00Z" },
                "seven_day_opus": { "utilization": 80 },
                "model_scoped": [{ "display_name": "Fixture", "utilization": 5 }],
                "accessToken": "secret",
            },
        }))
        .unwrap();
        assert_eq!(windows.len(), 3);
        assert!(windows[0].models.is_empty());
        assert_eq!(windows[0].resets_at, Some(1_893_499_200));
        assert_eq!(windows[1].models, ["opus"]);
        assert_eq!(windows[2].label, "Weekly · Fixture");
        let stored = serde_json::to_value(&windows).unwrap();
        assert_eq!(stored[0]["usedPercent"], 25.0);
        assert!(!stored.to_string().contains("secret"));
        assert!(usage_windows(&json!({ "rate_limits_available": false })).is_err());
    }
}
