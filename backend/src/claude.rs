//! The official Claude Code CLI: its environment, agent settings and model catalog.
//! Accounts and credentials are in `accounts::claude`.
use crate::{
    config::{Config, now},
    error::{Error, Result},
    process::{Environment, command},
    provider::Provider,
    service::Service,
    validation::text,
};
use serde_json::{Value, json};
use std::{path::Path, time::Duration};
use tokio::io::AsyncWriteExt;

pub const CATALOG: &str = "claude-models";

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
    if Provider::of_agent(agent) == Provider::Claude {
        if !["", "low", "medium", "high", "xhigh", "max"].contains(&text(agent, "reasoning")) {
            return Err(Error::bad("Choose a supported Claude effort level."));
        }
        if text(agent, "model").starts_with("gpt-") {
            return Err(Error::bad("Choose a Claude model for this agent."));
        }
    }
    Ok(())
}

/// Aliases offered before any account has listed its models.
pub fn models() -> Value {
    let rows = [("sonnet", "Sonnet"), ("opus", "Opus"), ("haiku", "Haiku")]
        .iter()
        .map(|(model, name)| {
            json!({
                "model": model,
                "displayName": name,
                "description": "Claude Code alias · connect to load account capabilities",
                "hidden": false,
                "isDefault": false,
                "defaultReasoningEffort": "",
                "supportedReasoningEfforts": []
            })
        })
        .collect::<Vec<_>>();
    json!({
        "models": rows,
        "checkedAt": null,
        "stale": true,
        "error": "Connect a Claude Code account to load available models and effort levels."
    })
}

/// Queries the CLI's control protocol on account `id`. Metadata queries never submit a user
/// message or start a model turn. Caller holds the account lock.
pub async fn query_metadata(s: &Service, id: &str, request: Option<Value>) -> Result<Value> {
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
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
        .map_err(|_| Error::new(503, "Claude Code is not installed."))?;
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap().take(2_000_000));
    let mut stderr = child.stderr.take().unwrap();
    let drain = tokio::spawn(async move {
        let _ = tokio::io::copy(&mut stderr, &mut tokio::io::sink()).await;
    });
    let result = tokio::time::timeout(Duration::from_secs(20), async {
        stdin.write_all(b"{\"type\":\"control_request\",\"request_id\":\"initialize\",\"request\":{\"subtype\":\"initialize\"}}\n").await?;
        let mut expected = "initialize";
        let mut line = String::new();
        loop {
            line.clear();
            if stdout.read_line(&mut line).await? == 0 {
                return Err(Error::new(502, "Claude Code metadata query stopped."));
            }
            let value: Value = serde_json::from_str(&line)?;
            if value["type"] != "control_response" || value["response"]["request_id"] != expected {
                continue;
            }
            if value["response"]["subtype"] != "success" {
                // CLI error text can contain private state. Never forward it.
                return Err(Error::new(502, "Claude Code could not load account data. Check the connection and CLI version."));
            }
            if expected == "initialize" && let Some(request) = &request {
                let message = json!({"type": "control_request","request_id": "metadata","request": request});
                stdin.write_all(format!("{message}\n").as_bytes()).await?;
                expected = "metadata";
                continue;
            }
            return Ok(value["response"]["response"].clone());
        }
    }).await.unwrap_or_else(|_| Err(Error::new(504, "Claude Code metadata query timed out.")));
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
    let _ = child.kill().await;
    let _ = child.wait().await;
    drain.abort();
    result
}

// Claude Code caches its account's model catalog. Unlike the initialize response, it names the
// model behind each alias and marks the default effort. A missing cache only hides those details.
async fn cached_catalog(directory: &Path) -> Vec<Value> {
    let mut newest = (i64::MIN, Vec::new());
    let Ok(mut entries) = tokio::fs::read_dir(directory.join("cache/model-catalog")).await else {
        return Vec::new();
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        let Ok(bytes) = tokio::fs::read(entry.path()).await else {
            continue;
        };
        let Ok(value) = serde_json::from_slice::<Value>(&bytes) else {
            continue;
        };
        let at = value["fetchedAt"].as_i64().unwrap_or(0);
        if let Some(models) = value["catalog"]["config"]["models"]
            .as_array()
            .filter(|_| at > newest.0)
        {
            newest = (at, models.clone());
        }
    }
    newest.1
}

// Applied whenever the catalog is served, including one stored while runs blocked refreshes.
fn present(catalog: &mut Value, cached: &[Value]) {
    for row in catalog["models"].as_array_mut().into_iter().flatten() {
        let resolved = text(row, "resolvedModel").to_owned();
        let entry = cached.iter().find(|model| {
            !resolved.is_empty()
                && model["id"] == resolved.strip_suffix("[1m]").unwrap_or(&resolved)
        });
        let efforts = row["supportedReasoningEfforts"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|effort| text(effort, "reasoningEffort").to_owned())
            .collect::<Vec<_>>();
        if text(row, "defaultReasoningEffort").is_empty()
            && let Some(effort) = entry
                .and_then(|model| model["thinking"]["effort_options"].as_array())
                .into_iter()
                .flatten()
                .find(|option| option["badge"]["message"] == "Default")
                .map(|option| text(option, "id").to_owned())
                .filter(|effort| efforts.contains(effort))
        {
            row["defaultReasoningEffort"] = effort.into();
        }
        // The "default" alias names itself "Default (recommended)". Show the model it runs instead.
        if row["model"] == "default" && text(row, "displayName").starts_with("Default") {
            let name = match entry
                .map(|model| text(model, "name"))
                .filter(|name| !name.is_empty())
            {
                Some(name) if resolved.ends_with("[1m]") => format!("{name} (1M context)"),
                Some(name) => name.to_owned(),
                None => match text(row, "description")
                    .split(" · ")
                    .next()
                    .filter(|name| !name.is_empty())
                {
                    Some(name) => name.to_owned(),
                    None => continue,
                },
            };
            row["displayName"] = name.into();
        }
    }
}

async fn discover_models(s: &Service, id: &str) -> Result<Value> {
    let value = query_metadata(s, id, None).await?;
    let rows = value["models"]
        .as_array()
        .ok_or_else(|| Error::new(502, "Update Claude Code to load its model catalog."))?;
    let models = rows
        .iter()
        .filter(|row| !text(row, "value").is_empty())
        .map(|row| {
            json!({
                "model": row["value"],
                "resolvedModel": text(row,"resolvedModel"),
                "displayName": row["displayName"],
                "description": row["description"],
                "hidden": false,
                "isDefault": row["value"]=="default",
                "defaultReasoningEffort": "",
                "supportedReasoningEfforts": row["supportedEffortLevels"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .map(|effort| json!({"reasoningEffort": effort, "description": ""}))
                    .collect::<Vec<_>>()
            })
        })
        .collect::<Vec<_>>();
    if models.is_empty() {
        return Err(Error::new(502, "Claude Code returned no available models."));
    }
    Ok(json!({
        "models": models,
        "source": id,
        "checkedAt": now(),
        "stale": false,
        "error": ""
    }))
}

pub async fn model_catalog(s: &Service) -> Result<Value> {
    let mut catalog = stored_catalog(s).await?;
    let source = crate::accounts::claude::account_home(&s.config, text(&catalog, "source"));
    let cached = if catalog["source"].is_string() {
        cached_catalog(&source).await
    } else {
        Vec::new()
    };
    present(&mut catalog, &cached);
    Ok(catalog)
}

async fn stored_catalog(s: &Service) -> Result<Value> {
    let mut cached = s.store.kv(CATALOG).await?.unwrap_or_else(models);
    if cached["checkedAt"]
        .as_i64()
        .is_some_and(|at| now() - at < 300_000)
    {
        return Ok(cached);
    }
    // Any signed-in account can list the models of its subscription.
    let mut accounts = s.accounts.records(s, Provider::Claude).await?;
    accounts.retain(|a| a["state"] == "ready" && a["enabled"] == true);
    accounts.sort_by_key(|a| a["createdAt"].as_i64());
    let Some(account) = accounts.first() else {
        cached["stale"] = true.into();
        cached["error"] = "Connect a Claude Code account to load available models.".into();
        return Ok(cached);
    };
    let id = text(account, "id");
    let _guard = s.accounts.lock(id).await;
    if s.accounts.busy(s, id).await? {
        return Ok(cached);
    }
    match discover_models(s, id).await {
        Ok(catalog) => {
            s.store.set(CATALOG, catalog.clone(), None).await?;
            Ok(catalog)
        }
        Err(e) => {
            cached["stale"] = true.into();
            cached["error"] = e.message.into();
            Ok(cached)
        }
    }
}
