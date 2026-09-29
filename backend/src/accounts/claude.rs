//! Claude Code: sign-in through `claude auth login --claudeai`, one private CLI home per
//! account, usage from the CLI's `get_usage` control request, and OAuth rotation by the
//! manager only. Runs receive access-only credential snapshots.
use super::{Driver, KIND, Lease, Login, broker, remove_directory, remove_file};
use crate::{
    auth::hex_digest,
    claude::environment,
    config::{Config, now},
    error::{Error, Result},
    process::{bounded_output, command, read_bounded},
    provider::Provider,
    service::Service,
    skills::{atomic_write, private_dir},
    store::merge,
    validation::text,
};
use serde_json::{Value, json};
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

/// The account's private CLI home, which holds its refresh credentials.
pub fn account_home(config: &Config, id: &str) -> PathBuf {
    config.data_dir.join("claude-accounts").join(id)
}

fn unavailable() -> Error {
    Error::new(
        503,
        "Claude authentication is unavailable. Check the account in Connections.",
    )
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

async fn credentials(home: &Path) -> Result<Value> {
    let bytes = read_private(&home.join(CREDENTIALS))
        .await
        .map_err(|_| unavailable())?
        .ok_or_else(unavailable)?;
    serde_json::from_slice(&bytes).map_err(|_| unavailable())
}

/// Access-only credentials: an explicit allowlist keeps refresh tokens on the manager.
fn snapshot(value: &Value) -> Result<Value> {
    let oauth = &value["claudeAiOauth"];
    if text(oauth, "accessToken").is_empty()
        || oauth["expiresAt"].as_i64().unwrap_or(0) <= now() + 30_000
    {
        return Err(unavailable());
    }
    let mut safe = json!({});
    for key in [
        "accessToken",
        "expiresAt",
        "scopes",
        "subscriptionType",
        "rateLimitTier",
    ] {
        if let Some(value) = oauth.get(key) {
            safe[key] = value.clone();
        }
    }
    Ok(json!({"claudeAiOauth": safe}))
}

/// Rotates the login when it is about to expire. The caller holds the account lock through
/// persistence, so concurrent runs share one rotation.
async fn access_at(home: &Path, endpoint: &str) -> Result<Value> {
    let mut value = credentials(home).await?;
    let oauth = &value["claudeAiOauth"];
    if oauth["expiresAt"].as_i64().unwrap_or(0) <= now() + 300_000 {
        if text(oauth, "refreshToken").is_empty() {
            return snapshot(&value);
        }
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|_| unavailable())?;
        let body = json!({
            "grant_type": "refresh_token",
            "refresh_token": oauth["refreshToken"],
            "client_id": oauth["clientId"].as_str().unwrap_or(CLIENT_ID),
            "scope": oauth["scopes"].as_array().into_iter().flatten().filter_map(Value::as_str).collect::<Vec<_>>().join(" ")
        });
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
        let rotated: Value = serde_json::from_slice(&bytes).map_err(|_| unavailable())?;
        let seconds = rotated["expires_in"]
            .as_i64()
            .filter(|n| *n > 60 && *n <= 366 * 86400)
            .ok_or_else(unavailable)?;
        if text(&rotated, "access_token").is_empty() {
            return Err(unavailable());
        }
        let oauth = &mut value["claudeAiOauth"];
        oauth["accessToken"] = rotated["access_token"].clone();
        oauth["expiresAt"] = (now() + seconds * 1000).into();
        if let Some(token) = rotated["refresh_token"].as_str().filter(|s| !s.is_empty()) {
            oauth["refreshToken"] = token.into();
        }
        if let Some(scope) = rotated["scope"].as_str() {
            oauth["scopes"] = json!(scope.split_whitespace().collect::<Vec<_>>());
        }
        if let Some(seconds) = rotated["refresh_token_expires_in"]
            .as_i64()
            .filter(|n| *n > 0 && *n <= 366 * 86400)
        {
            oauth["refreshTokenExpiresAt"] = (now() + seconds * 1000).into();
        }
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

/// Only documented, non-secret identity fields ever leave the CLI's authentication status.
async fn status(config: &Config, home: &Path) -> Result<Value> {
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
    .map_err(|_| Error::new(503, "Claude Code is not installed on the server."))?;
    let value: Value = serde_json::from_str(&output.stdout).map_err(|_| {
        Error::new(
            502,
            "Update Claude Code: authentication status was not valid JSON.",
        )
    })?;
    Ok(json!({
        "connected": output.success && value["loggedIn"]==true,
        "email": value["email"].as_str(),
        "subscriptionType": value["subscriptionType"].as_str()
    }))
}

fn identity(email: &str) -> String {
    hex_digest(&format!("claude:{}", email.trim().to_lowercase()))
}

fn usage_windows(value: &Value) -> Result<Vec<Value>> {
    if value["rate_limits_available"] != true {
        return Err(Error::new(
            502,
            "Usage limits are unavailable for this Claude account.",
        ));
    }
    let limits = &value["rate_limits"];
    let mut windows = Vec::new();
    let mut add = |id: String, label: &str, minutes: i64, models: Vec<String>, row: &Value| {
        let Some(used) = row["utilization"]
            .as_f64()
            .filter(|n| n.is_finite() && *n >= 0.0)
        else {
            return;
        };
        let resets_at = row["resets_at"]
            .as_str()
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .map(|date| date.timestamp());
        windows.push(json!({
            "id": id,
            "label": label,
            "usedPercent": used,
            "resetsAt": resets_at,
            "durationMins": minutes,
            "models": models,
            "reached": false
        }));
    };
    add(
        "five_hour".into(),
        "5-hour window",
        300,
        vec![],
        &limits["five_hour"],
    );
    add(
        "seven_day".into(),
        "Weekly",
        10080,
        vec![],
        &limits["seven_day"],
    );
    for (key, label, model) in [
        ("seven_day_opus", "Weekly · Opus", "opus"),
        ("seven_day_sonnet", "Weekly · Sonnet", "sonnet"),
        ("seven_day_oauth_apps", "Weekly · OAuth apps", "oauth-apps"),
    ] {
        add(key.into(), label, 10080, vec![model.into()], &limits[key]);
    }
    for (index, row) in limits["model_scoped"]
        .as_array()
        .into_iter()
        .flatten()
        .take(20)
        .enumerate()
    {
        if let Some(name) = row["display_name"]
            .as_str()
            .filter(|s| !s.is_empty() && s.len() <= 100)
        {
            add(
                format!("model_{index}"),
                &format!("Weekly · {name}"),
                10080,
                vec![name.to_lowercase()],
                row,
            );
        }
    }
    if windows.is_empty() {
        return Err(Error::new(
            502,
            "Claude Code has not returned usage limits yet.",
        ));
    }
    Ok(windows)
}

/// A model-scoped window names a model family, such as "opus". It also limits each catalog
/// model that runs the family, such as "opus[1m]", or "default" when it resolves to Opus.
fn scope(windows: &mut [Value], catalog: &Value) {
    for window in windows {
        let Some(family) = window["models"][0].as_str().map(str::to_lowercase) else {
            continue;
        };
        for row in catalog["models"].as_array().into_iter().flatten() {
            let model = text(row, "model");
            let runs = [model, text(row, "resolvedModel")]
                .iter()
                .any(|name| name.to_lowercase().contains(&family));
            let models = window["models"].as_array_mut().unwrap();
            if runs && !models.iter().any(|m| m == model) {
                models.push(model.into());
            }
        }
    }
}

/// Reads usage at most every five minutes. A failure keeps the last known windows.
async fn read_usage(s: &Service, id: &str, previous: &Value) -> Value {
    let mut current = if previous.is_object() {
        previous.clone()
    } else {
        super::usage::empty()
    };
    if current["attemptedAt"]
        .as_i64()
        .is_some_and(|at| now() - at < USAGE_TTL)
    {
        return current;
    }
    current["attemptedAt"] = now().into();
    match crate::claude::query_metadata(
        s,
        id,
        Some(json!({"subtype": "get_usage","skip_behaviors": true})),
    )
    .await
    .and_then(|v| usage_windows(&v))
    {
        Ok(mut windows) => {
            let catalog = s
                .store
                .kv(crate::claude::CATALOG)
                .await
                .ok()
                .flatten()
                .unwrap_or_else(crate::claude::models);
            scope(&mut windows, &catalog);
            current["windows"] = windows.into();
            current["checkedAt"] = now().into();
            current["error"] = Value::Null;
        }
        Err(_) => {
            current["error"] = "Claude Code usage is temporarily unavailable. It will be checked again automatically.".into();
        }
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
    let interrupted = legacy.join("sync-required").exists();
    if legacy.join(CREDENTIALS).exists()
        && s.accounts.records(s, Provider::Claude).await?.is_empty()
    {
        let saved = s.store.kv("claude-status").await?.unwrap_or(Value::Null);
        let limit = s
            .store
            .kv("claude-concurrency")
            .await?
            .and_then(|v| v.as_u64())
            .unwrap_or(4);
        // A guest may have rotated the refresh token during the old serialized transfer.
        let mut account = super::record(
            Provider::Claude,
            "Claude",
            if interrupted { "error" } else { "ready" },
        );
        let id = text(&account, "id").to_owned();
        let email = saved["email"].as_str();
        merge(
            &mut account,
            &json!({
                "email": email,
                "plan": saved["subscriptionType"],
                "identity": email.map(identity),
                "maxConcurrentRuns": limit,
                "error": if interrupted {"Reconnect this account after an interrupted credential transfer."} else {""}
            }),
        );
        private_dir(&s.config.data_dir.join("claude-accounts")).await?;
        tokio::fs::rename(&legacy, account_home(&s.config, &id)).await?;
        remove_file(&account_home(&s.config, &id).join("sync-required")).await?;
        s.store.put(KIND, account).await?;
        s.store
            .audit("account.imported", json!({"id": id,"provider": "claude"}))
            .await?;
    } else {
        remove_directory(&legacy).await?;
    }
    for key in ["claude-status", "claude-usage", "claude-concurrency"] {
        s.store.delete(key).await?;
    }
    Ok(())
}

/// Local runs used the shared home, so their session transcripts move into each run.
async fn migrate_sessions(s: &Service, legacy: &Path) -> Result<()> {
    let runs = s
        .store
        .read(|db| {
            db.json_rows(
                "SELECT data FROM runs WHERE \
                    json_extract(data,'$.snapshot.agent.provider')='claude' AND \
                    json_extract(data,'$.sessionId') IS NOT NULL AND \
                    COALESCE(json_extract(data,'$.isolated'),0)!=1",
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
        let session = format!("{}.jsonl", text(&run, "sessionId"));
        let target = s
            .config
            .data_dir
            .join("runs")
            .join(text(&run, "id"))
            .join("home/.claude/projects");
        for directory in &directories {
            let source = legacy.join("projects").join(directory).join(&session);
            if source.exists() && !target.join(directory).join(&session).exists() {
                private_dir(&target.join(directory)).await?;
                copy_private(&source, &target.join(directory).join(&session)).await?;
            }
        }
    }
    Ok(())
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
            Error::new(
                503,
                "Claude Code is not installed. Install the supported CLI on the server.",
            )
        })?;
        let mut stdin = child.stdin.take().unwrap();
        let mut stdout = child.stdout.take().unwrap();
        let mut stderr = child.stderr.take().unwrap();
        let mut output = String::new();
        let mut a = [0u8; 4096];
        let mut b = [0u8; 4096];
        let (mut stdout_open, mut stderr_open) = (true, true);
        let deadline = tokio::time::sleep(Duration::from_secs(900));
        tokio::pin!(deadline);
        let result: Result<bool> = async {
            loop {
                tokio::select! {
                    _ = login.stop.cancelled() => return Err(Error::new(409, "Sign-in cancelled.")),
                    _ = s.shutdown.cancelled() => return Err(Error::new(409, "Sign-in cancelled.")),
                    _ = &mut deadline => return Err(Error::new(408, "Sign-in expired. Start again to get a new link.")),
                    code = login.code() => if let Some(code) = code {
                        stdin.write_all(code.as_bytes()).await?;
                        stdin.write_all(b"\n").await?;
                    },
                    status = child.wait() => return Ok(status?.success()),
                    n = stdout.read(&mut a),
                    if stdout_open => {
                        let n = n?;
                        stdout_open = n > 0;
                        output.push_str(&String::from_utf8_lossy(&a[..n]));
                    },
                    n = stderr.read(&mut b),
                    if stderr_open => {
                        let n = n?;
                        stderr_open = n > 0;
                        output.push_str(&String::from_utf8_lossy(&b[..n]));
                    },
                }
                if output.len() > 64_000 {
                    return Err(Error::new(
                        502,
                        "Claude Code sign-in returned too much output.",
                    ));
                }
                if let Some(url) = login_url(&output) {
                    login.update(|v| {
                        v["phase"] = "authorizing".into();
                        v["url"] = url.into();
                        v["acceptsCode"] = true.into();
                    });
                }
            }
        }
        .await;
        let _ = child.kill().await;
        let _ = child.wait().await;
        match result {
            Ok(true) => Ok(()),
            // CLI errors can contain private state. Never forward them.
            Ok(false) => Err(Error::bad(
                "Claude Code could not finish sign-in. Try again.",
            )),
            Err(error) => Err(error),
        }
    }

    async fn adopt(&self, s: &Service, id: &str, home: &Path) -> Result<()> {
        let signed_in = status(&s.config, home).await?;
        let email = text(&signed_in, "email");
        if signed_in["connected"] != true || email.is_empty() {
            return Err(Error::bad(
                "Claude Code could not finish sign-in. Try again.",
            ));
        }
        let mut account = s.accounts.get(s, id).await?;
        let fingerprint = identity(email);
        if account["identity"].is_string() && account["identity"] != fingerprint.as_str() {
            return Err(super::different_account());
        }
        merge(
            &mut account,
            &json!({
                "identity": fingerprint,
                "email": email,
                "plan": signed_in["subscriptionType"],
                "state": "ready",
                "error": "",
                "checkedAt": now(),
                "usage": null,
                "exhausted": null
            }),
        );
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
        if current["connected"] != true {
            return Err(Error::new(409, "Reconnect this Claude account."));
        }
        let mut account = s.accounts.get(s, id).await?;
        let usage = read_usage(s, id, &account["usage"]).await;
        merge(
            &mut account,
            &json!({
                "email": current["email"],
                "plan": current["subscriptionType"],
                "state": "ready",
                "error": "",
                "checkedAt": now()
            }),
        );
        account["usage"] = usage;
        s.store.put(KIND, account).await?;
        Ok(())
    }

    async fn due(&self, _s: &Service, _account: &Value, attempted: i64) -> Result<bool> {
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

    async fn access(&self, s: &Service, lease: &Lease, _request: &Value) -> Result<Value> {
        let _rotation = s.accounts.lock(&lease.account_id).await;
        access_at(&account_home(&s.config, &lease.account_id), TOKEN_URL).await
    }

    async fn redactions(&self, s: &Service, id: &str) -> Result<Vec<String>> {
        let value = credentials(&account_home(&s.config, id))
            .await
            .unwrap_or(Value::Null);
        Ok(["accessToken", "refreshToken"]
            .iter()
            .filter_map(|key| {
                value["claudeAiOauth"][key]
                    .as_str()
                    .filter(|v| !v.is_empty())
                    .map(str::to_owned)
            })
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

    pub async fn sync(&mut self) -> Result<()> {
        let result = async {
            let value = broker::request(&self.socket, &json!({}), Duration::from_secs(40))
                .await
                .map_err(|_| unavailable())?;
            let value = snapshot(&value)?;
            let bytes = serde_json::to_vec(&value)?;
            if bytes != self.previous
                || tokio::fs::read(self.home.join(CREDENTIALS))
                    .await
                    .ok()
                    .as_deref()
                    != Some(bytes.as_slice())
            {
                atomic_write(&self.home.join(CREDENTIALS), &bytes).await?;
                self.previous = bytes;
            }
            self.expires = value["claudeAiOauth"]["expiresAt"].as_i64().unwrap_or(0);
            Ok(())
        }
        .await;
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

    #[test]
    fn model_windows_limit_the_catalog_models_of_their_family() {
        let catalog = json!({
            "models": [{"model": "opus"},{"model": "opus[1m]"},{"model": "default","resolvedModel": "claude-opus-fixture[1m]"},{"model": "sonnet","resolvedModel": "claude-sonnet-fixture"}]
        });
        let mut windows = vec![json!({"models":[]}), json!({"models":["opus"]})];
        scope(&mut windows, &catalog);
        assert_eq!(windows[0]["models"], json!([]));
        assert_eq!(windows[1]["models"], json!(["opus", "opus[1m]", "default"]));
    }

    fn stored() -> Value {
        json!({
            "claudeAiOauth": {
                "accessToken": "old-access",
                "refreshToken": "private-refresh",
                "expiresAt": now()-1000,
                "scopes": ["user:profile","user:inference"],
                "subscriptionType": "max",
                "clientId": "login-client"
            },
            "otherSecret": "must-stay-on-host"
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
        let router = axum::Router::new().route(
            "/token",
            axum::routing::post(move |axum::Json(v): axum::Json<Value>| {
                let count = count.clone();
                async move {
                    assert_eq!(v["grant_type"], "refresh_token");
                    assert_eq!(v["refresh_token"], "private-refresh");
                    assert_eq!(v["client_id"], "login-client");
                    count.fetch_add(1, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    axum::Json(json!({
                        "access_token": "new-access",
                        "refresh_token": "new-private-refresh",
                        "expires_in": 3600,
                        "scope": "user:profile user:inference"
                    }))
                }
            }),
        );
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
                "five_hour": {"utilization": 25,"resets_at": "2030-01-01T12:00:00Z"},
                "seven_day_opus": {"utilization": 80},
                "model_scoped": [{"display_name": "Fixture","utilization": 5}],
                "accessToken": "secret"
            }
        }))
        .unwrap();
        assert_eq!(windows.len(), 3);
        assert_eq!(windows[0]["models"], json!([]));
        assert_eq!(windows[0]["resetsAt"], 1893499200);
        assert_eq!(windows[1]["models"], json!(["opus"]));
        assert_eq!(windows[2]["label"], "Weekly · Fixture");
        assert!(!serde_json::to_string(&windows).unwrap().contains("secret"));
        assert!(usage_windows(&json!({"rate_limits_available":false})).is_err());
    }
}
