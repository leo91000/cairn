//! The official Claude Code CLI: its environment, agent settings and model catalog.
//! Accounts and credentials are in `accounts::claude`.
use crate::{
    accounts::lenient,
    config::{Config, now},
    error::{Error, Result},
    process::{Environment, command},
    provider::Provider,
    service::Service,
    validation::text,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::{path::Path, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    process::{ChildStdin, ChildStdout},
};

pub const CATALOG: &str = "claude-models";
const EFFORTS: [&str; 6] = ["", "low", "medium", "high", "xhigh", "max"];

/// The CLI's environment with `directory` as its home, without any inherited credentials.
pub fn environment(config: &Config, directory: &Path) -> Environment {
    let mut env = std::env::vars().collect::<Environment>();
    crate::process::remove_storage_environment(&mut env);
    env.insert("HOME".into(), config.home.to_string_lossy().into_owned());
    configure(&mut env, directory);
    env
}

/// Points the CLI at `directory` for its state and credentials, without inherited credentials,
/// updates or browser launches.
pub fn configure(env: &mut Environment, directory: &Path) {
    sanitize(env);
    env.insert(
        "CLAUDE_CONFIG_DIR".into(),
        directory.to_string_lossy().into_owned(),
    );
    env.insert("DISABLE_AUTOUPDATER".into(), "1".into());
    env.insert("BROWSER".into(), "true".into());
}

/// Subscription runs never use API keys, cloud providers or alternate OAuth overrides.
fn sanitize(env: &mut Environment) {
    for key in [
        "ANTHROPIC_API_KEY",
        "ANTHROPIC_BASE_URL",
        "ANTHROPIC_AUTH_TOKEN",
        "CLAUDE_CODE_OAUTH_TOKEN",
        "CLAUDE_CODE_OAUTH_TOKEN_FILE_DESCRIPTOR",
        "CLAUDE_SECURESTORAGE_CONFIG_DIR",
        "CLAUDE_CODE_OAUTH_REFRESH_TOKEN",
        "CLAUDE_CODE_SDK_HAS_OAUTH_REFRESH",
        "CLAUDE_CODE_USE_BEDROCK",
        "CLAUDE_CODE_USE_VERTEX",
        "CLAUDE_CODE_USE_FOUNDRY",
        "CLAUDECODE",
        "CLAUDE_CODE_SIMPLE",
        "CLAUDE_CODE_SAFE_MODE",
    ] {
        env.remove(key);
    }
}

pub fn validate_agent(agent: &Value) -> Result<()> {
    if Provider::of_agent(agent) != Provider::Claude {
        return Ok(());
    }
    if !EFFORTS.contains(&text(agent, "reasoning")) {
        return Err(Error::bad("Choose a supported Claude effort level."));
    }
    if text(agent, "model").starts_with("gpt-") {
        return Err(Error::bad("Choose a Claude model for this agent."));
    }
    Ok(())
}

/// The models a Claude Code subscription offers, as stored under [`CATALOG`] and served.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Catalog {
    #[serde(default)]
    pub models: Vec<CatalogModel>,
    /// The account the catalog was listed with.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default)]
    pub checked_at: Option<i64>,
    #[serde(default)]
    pub stale: bool,
    #[serde(default)]
    pub error: String,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogModel {
    #[serde(default)]
    pub model: String,
    /// The model an alias such as "default" or "opus[1m]" runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_model: Option<String>,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub hidden: bool,
    #[serde(default)]
    pub is_default: bool,
    #[serde(default)]
    pub default_reasoning_effort: String,
    #[serde(default)]
    pub supported_reasoning_efforts: Vec<Effort>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Effort {
    #[serde(default)]
    pub reasoning_effort: String,
    #[serde(default)]
    pub description: String,
}

impl Catalog {
    /// Aliases offered before any account has listed its models.
    pub fn aliases() -> Self {
        let models = [("sonnet", "Sonnet"), ("opus", "Opus"), ("haiku", "Haiku")]
            .into_iter()
            .map(|(model, name)| CatalogModel {
                model: model.into(),
                display_name: Some(name.into()),
                description: Some(
                    "Claude Code alias · connect to load account capabilities".into(),
                ),
                ..CatalogModel::default()
            })
            .collect();
        Self {
            models,
            stale: true,
            error: "Connect a Claude Code account to load available models and effort levels."
                .into(),
            ..Self::default()
        }
    }

    /// The stored catalog, or the aliases when none is stored.
    pub fn read(stored: Option<Value>) -> Self {
        stored
            .and_then(|value| Self::deserialize(value).ok())
            .unwrap_or_else(Self::aliases)
    }

    fn is_recent(&self) -> bool {
        self.checked_at.is_some_and(|at| now() - at < 300_000)
    }

    fn stale(mut self, error: String) -> Self {
        self.stale = true;
        self.error = error;
        self
    }
}

/// A line of Claude Code's stream-json control protocol.
#[derive(Serialize)]
struct ControlRequest<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    request_id: &'a str,
    request: &'a Value,
}

#[derive(Debug, Default, Deserialize)]
struct ControlResponse {
    #[serde(default, rename = "type", deserialize_with = "lenient::string")]
    kind: String,
    #[serde(default, deserialize_with = "lenient::or_default")]
    response: ControlResult,
}

#[derive(Debug, Default, Deserialize)]
struct ControlResult {
    #[serde(default, deserialize_with = "lenient::string")]
    request_id: String,
    #[serde(default, deserialize_with = "lenient::string")]
    subtype: String,
    #[serde(default)]
    response: Value,
}

async fn send_control(stdin: &mut ChildStdin, request_id: &str, request: &Value) -> Result<()> {
    let message = ControlRequest {
        kind: "control_request",
        request_id,
        request,
    };
    let mut line = serde_json::to_vec(&message)?;
    line.push(b'\n');
    stdin.write_all(&line).await?;
    Ok(())
}

/// Initializes the control protocol, then sends `request` if any. Answers the last response.
async fn exchange(
    stdin: &mut ChildStdin,
    stdout: &mut BufReader<tokio::io::Take<ChildStdout>>,
    request: Option<&Value>,
) -> Result<Value> {
    send_control(
        stdin,
        "initialize",
        &serde_json::json!({ "subtype": "initialize" }),
    )
    .await?;
    let mut expected = "initialize";
    let mut line = String::new();
    loop {
        line.clear();
        if stdout.read_line(&mut line).await? == 0 {
            return Err(Error::bad_gateway("Claude Code metadata query stopped."));
        }
        let value: Value = serde_json::from_str(&line)?;
        let message = ControlResponse::deserialize(value).unwrap_or_default();
        if message.kind != "control_response" || message.response.request_id != expected {
            continue;
        }
        if message.response.subtype != "success" {
            // CLI error text can contain private state. Never forward it.
            return Err(Error::bad_gateway(
                "Claude Code could not load account data. Check the connection and CLI version.",
            ));
        }
        if expected == "initialize"
            && let Some(request) = request
        {
            send_control(stdin, "metadata", request).await?;
            expected = "metadata";
            continue;
        }
        return Ok(message.response.response);
    }
}

/// Queries the CLI's control protocol on account `id`. Metadata queries never submit a user
/// message or start a model turn. Caller holds the account lock.
pub async fn query_metadata(s: &Service, id: &str, request: Option<Value>) -> Result<Value> {
    let directory = crate::accounts::claude::account_home(&s.config, id);
    crate::skills::private_dir(&directory).await?;
    let credentials = crate::accounts::claude::metadata_credentials(&s.config, id).await?;
    let args = [
        "-p",
        "--input-format",
        "stream-json",
        "--output-format",
        "stream-json",
        "--verbose",
        "--no-session-persistence",
        "--setting-sources",
        "",
        "--strict-mcp-config",
        "--mcp-config",
        "{\"mcpServers\":{}}",
    ]
    .map(str::to_owned);
    let mut cmd = command(
        &s.config.claude_bin,
        &args,
        &environment(&s.config, &directory),
        Some(&directory),
    );
    cmd.env("CLAUDE_SECURESTORAGE_CONFIG_DIR", credentials.path());
    cmd.stdin(std::process::Stdio::piped());
    let mut child = cmd
        .spawn()
        .map_err(|_| Error::unavailable("Claude Code is not installed."))?;
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap().take(2_000_000));
    let mut stderr = child.stderr.take().unwrap();
    let drain = tokio::spawn(async move {
        let _ = tokio::io::copy(&mut stderr, &mut tokio::io::sink()).await;
    });
    let result = tokio::time::timeout(
        Duration::from_secs(20),
        exchange(&mut stdin, &mut stdout, request.as_ref()),
    )
    .await
    .unwrap_or_else(|_| {
        Err(Error::gateway_timeout(
            "Claude Code metadata query timed out.",
        ))
    });
    // Claude Code writes its account model catalog shortly after answering initialize. Model
    // discovery reads it for the model and effort behind each alias, so let the write finish.
    if request.is_none() && result.is_ok() {
        for _ in 0..50 {
            if !cached_catalog(&directory).await.is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
    // The CLI may already have exited.
    let _ = child.kill().await;
    let _ = child.wait().await;
    drain.abort();
    result
}

/// A model catalog file Claude Code caches for its account.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CachedFile {
    #[serde(default, deserialize_with = "lenient::integer")]
    fetched_at: Option<i64>,
    #[serde(default, deserialize_with = "lenient::or_default")]
    catalog: CachedBody,
}

#[derive(Debug, Default, Deserialize)]
struct CachedBody {
    #[serde(default, deserialize_with = "lenient::or_default")]
    config: CachedConfig,
}

#[derive(Debug, Default, Deserialize)]
struct CachedConfig {
    #[serde(default, deserialize_with = "lenient::or_default")]
    models: Option<Vec<CachedModel>>,
}

#[derive(Clone, Debug, Default, Deserialize)]
struct CachedModel {
    #[serde(default, deserialize_with = "lenient::string")]
    id: String,
    #[serde(default, deserialize_with = "lenient::string")]
    name: String,
    #[serde(default, deserialize_with = "lenient::or_default")]
    thinking: Thinking,
}

#[derive(Clone, Debug, Default, Deserialize)]
struct Thinking {
    #[serde(default, deserialize_with = "lenient::or_default")]
    effort_options: Vec<EffortOption>,
}

#[derive(Clone, Debug, Default, Deserialize)]
struct EffortOption {
    #[serde(default, deserialize_with = "lenient::string")]
    id: String,
    #[serde(default, deserialize_with = "lenient::or_default")]
    badge: Badge,
}

#[derive(Clone, Debug, Default, Deserialize)]
struct Badge {
    #[serde(default, deserialize_with = "lenient::string")]
    message: String,
}

impl CachedModel {
    fn default_effort(&self) -> Option<&str> {
        self.thinking
            .effort_options
            .iter()
            .find(|option| option.badge.message == "Default")
            .map(|option| option.id.as_str())
    }
}

// Claude Code caches its account's model catalog. Unlike the initialize response, it names the
// model behind each alias and marks the default effort. A missing cache only hides those details.
async fn cached_catalog(directory: &Path) -> Vec<CachedModel> {
    let mut newest = (i64::MIN, Vec::new());
    let Ok(mut entries) = tokio::fs::read_dir(directory.join("cache/model-catalog")).await else {
        return Vec::new();
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        let Ok(bytes) = tokio::fs::read(entry.path()).await else {
            continue;
        };
        let Ok(file) = serde_json::from_slice::<CachedFile>(&bytes) else {
            continue;
        };
        let at = file.fetched_at.unwrap_or(0);
        if let Some(models) = file.catalog.config.models
            && at > newest.0
        {
            newest = (at, models);
        }
    }
    newest.1
}

// Applied whenever the catalog is served, including one stored while runs blocked refreshes.
fn present(catalog: &mut Catalog, cached: &[CachedModel]) {
    for row in &mut catalog.models {
        let resolved = row.resolved_model.clone().unwrap_or_default();
        let entry = cached.iter().find(|model| {
            !resolved.is_empty() && model.id == resolved.strip_suffix("[1m]").unwrap_or(&resolved)
        });
        let supported = |effort: &str| {
            row.supported_reasoning_efforts
                .iter()
                .any(|e| e.reasoning_effort == effort)
        };
        if row.default_reasoning_effort.is_empty()
            && let Some(effort) = entry
                .and_then(CachedModel::default_effort)
                .filter(|effort| supported(effort))
        {
            row.default_reasoning_effort = effort.into();
        }
        let named_default = row.model == "default"
            && row
                .display_name
                .as_deref()
                .unwrap_or_default()
                .starts_with("Default");
        if named_default && let Some(name) = default_name(entry, &resolved, row) {
            row.display_name = Some(name);
        }
    }
}

/// The "default" alias names itself "Default (recommended)". Show the model it runs instead.
fn default_name(entry: Option<&CachedModel>, resolved: &str, row: &CatalogModel) -> Option<String> {
    match entry
        .map(|model| model.name.as_str())
        .filter(|name| !name.is_empty())
    {
        Some(name) if resolved.ends_with("[1m]") => Some(format!("{name} (1M context)")),
        Some(name) => Some(name.to_owned()),
        None => row
            .description
            .as_deref()
            .unwrap_or_default()
            .split(" · ")
            .next()
            .filter(|name| !name.is_empty())
            .map(str::to_owned),
    }
}

/// A model as the CLI's initialize response lists it.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ListedModel {
    #[serde(default, deserialize_with = "lenient::string")]
    value: String,
    #[serde(default, deserialize_with = "lenient::string")]
    resolved_model: String,
    #[serde(default, deserialize_with = "lenient::optional_string")]
    display_name: Option<String>,
    #[serde(default, deserialize_with = "lenient::optional_string")]
    description: Option<String>,
    #[serde(default, deserialize_with = "lenient::strings")]
    supported_effort_levels: Vec<String>,
}

impl From<ListedModel> for CatalogModel {
    fn from(listed: ListedModel) -> Self {
        let supported_reasoning_efforts = listed
            .supported_effort_levels
            .into_iter()
            .map(|effort| Effort {
                reasoning_effort: effort,
                description: String::new(),
            })
            .collect();
        Self {
            is_default: listed.value == "default",
            model: listed.value,
            resolved_model: Some(listed.resolved_model),
            display_name: listed.display_name,
            description: listed.description,
            supported_reasoning_efforts,
            ..Self::default()
        }
    }
}

async fn discover_models(s: &Service, id: &str) -> Result<Catalog> {
    let value = query_metadata(s, id, None).await?;
    let rows = value["models"]
        .as_array()
        .ok_or_else(|| Error::bad_gateway("Update Claude Code to load its model catalog."))?;
    let models = rows
        .iter()
        .map(|row| ListedModel::deserialize(row).unwrap_or_default())
        .filter(|row| !row.value.is_empty())
        .map(CatalogModel::from)
        .collect::<Vec<_>>();
    if models.is_empty() {
        return Err(Error::bad_gateway(
            "Claude Code returned no available models.",
        ));
    }
    Ok(Catalog {
        models,
        source: Some(id.into()),
        checked_at: Some(now()),
        ..Catalog::default()
    })
}

pub async fn model_catalog(s: &Service) -> Result<Value> {
    let mut catalog = stored_catalog(s).await?;
    let cached = match &catalog.source {
        Some(source) => {
            cached_catalog(&crate::accounts::claude::account_home(&s.config, source)).await
        }
        None => Vec::new(),
    };
    present(&mut catalog, &cached);
    Ok(serde_json::to_value(catalog)?)
}

async fn stored_catalog(s: &Service) -> Result<Catalog> {
    let cached = Catalog::read(s.store.kv(CATALOG).await?);
    if cached.is_recent() {
        return Ok(cached);
    }
    // Any signed-in account can list the models of its subscription.
    let mut accounts = s.accounts.of(s, Provider::Claude).await?;
    accounts.retain(crate::accounts::Account::is_active);
    accounts.sort_by_key(|a| a.created_at);
    let Some(account) = accounts.first() else {
        return Ok(cached.stale("Connect a Claude Code account to load available models.".into()));
    };
    let id = account.id.as_str();
    let _guard = s.accounts.lock(id).await;
    if s.accounts.busy(s, id).await? {
        return Ok(cached);
    }
    match discover_models(s, id).await {
        Ok(catalog) => {
            s.store
                .set(CATALOG, serde_json::to_value(&catalog)?, None)
                .await?;
            Ok(catalog)
        }
        Err(e) => Ok(cached.stale(e.message)),
    }
}
