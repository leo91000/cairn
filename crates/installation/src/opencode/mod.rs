//! OpenCode's native HTTP session protocol, as of 1.19. Each execution gets a private
//! server, state, and credentials; engine and model-provider identifiers stay separate.
mod turn;

pub use turn::run;

use crate::{
    config::{Config, now},
    error::{Error, Result},
    process::{Environment, command, read_bounded},
    provider::Provider,
    service::Service,
    skills::private_dir,
    validation::text,
};
use futures_util::StreamExt;
use reqwest::{Method, Response, Url};
use serde_json::{Value, json};
use std::{path::Path, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::Child,
    sync::{Mutex, mpsc},
    task::JoinHandle,
};

/// The oldest release whose HTTP session protocol this adapter speaks. Each image
/// build installs OpenCode's latest release, which CI runs through this adapter's
/// native test before validation (scripts/cli-updates.mjs).
pub const MINIMUM: &str = "1.19.0";
const LIMIT: usize = 16_000_000;

const API_PROVIDERS: &[&str] = &[
    "opencode",
    "opencode-go",
    "openrouter",
    "openai",
    "anthropic",
    "google",
    "groq",
    "deepseek",
    "mistral",
    "xai",
];

pub fn model_id(value: &str) -> Result<(&str, &str)> {
    value
        .split_once('/')
        .filter(|(provider, model)| {
            !provider.is_empty()
                && !model.is_empty()
                && value.len() <= 300
                && provider
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
                && !model.chars().any(|c| c.is_whitespace() || c.is_control())
        })
        .ok_or_else(|| Error::bad("Choose an OpenCode model in provider/model format."))
}

pub fn isolate(env: &mut Environment, home: &Path) {
    env.retain(|key, _| !key.starts_with("OPENCODE_"));
    for key in [
        "OPENROUTER_API_KEY",
        "GROQ_API_KEY",
        "DEEPSEEK_API_KEY",
        "MISTRAL_API_KEY",
        "XAI_API_KEY",
        "GOOGLE_GENERATIVE_AI_API_KEY",
        "GEMINI_API_KEY",
    ] {
        env.remove(key);
    }
    env.insert("OPENCODE_AUTH_CONTENT".into(), "{}".into());
    env.insert(
        "CAIRN_OPENCODE_HOME".into(),
        home.to_string_lossy().into_owned(),
    );
    env.insert(
        "OPENCODE_CONFIG_DIR".into(),
        home.join("config/opencode").to_string_lossy().into_owned(),
    );
}

fn environment(config: &Config, home: &Path) -> Environment {
    let mut env = std::env::vars().collect::<Environment>();
    crate::process::remove_storage_environment(&mut env);
    env.insert("HOME".into(), config.home.to_string_lossy().into_owned());
    isolate(&mut env, home);
    for (variable, directory) in [
        ("XDG_DATA_HOME", "data"),
        ("XDG_CONFIG_HOME", "config"),
        ("XDG_CACHE_HOME", "cache"),
        ("XDG_STATE_HOME", "state"),
    ] {
        env.insert(
            variable.into(),
            home.join(directory).to_string_lossy().into_owned(),
        );
    }
    env.entry("GH_CONFIG_DIR".into()).or_insert_with(|| {
        config
            .home
            .join(".config/gh")
            .to_string_lossy()
            .into_owned()
    });
    // Model-provider environment credentials cannot override a leased API key.
    env.retain(|key, _| {
        !key.ends_with("_API_KEY")
            && key != "ANTHROPIC_AUTH_TOKEN"
            && key != "GOOGLE_APPLICATION_CREDENTIALS"
    });
    // The pinned native release reads global plugins from Global.Path.home.
    // Override that lookup without changing HOME for shell tools and GitHub auth.
    env.insert(
        "OPENCODE_TEST_HOME".into(),
        home.join("native-home").to_string_lossy().into_owned(),
    );
    env.insert("OPENCODE_DISABLE_PROJECT_CONFIG".into(), "true".into());
    env.insert("OPENCODE_DISABLE_SHARE".into(), "true".into());
    env.insert("OPENCODE_DISABLE_AUTOUPDATE".into(), "true".into());
    env
}

pub(super) struct Server {
    child: Child,
    drains: Vec<JoinHandle<()>>,
    client: reqwest::Client,
    url: Url,
    password: String,
    directory: String,
}

impl Drop for Server {
    fn drop(&mut self) {
        if let Some(pid) = self.child.id() {
            unsafe {
                libc::kill(-(pid as i32), libc::SIGKILL);
            }
        }
        for task in &self.drains {
            task.abort();
        }
    }
}

impl Server {
    pub(super) async fn start(
        config: &Config,
        home: &Path,
        cwd: &Path,
        native: &Value,
        credentials: Option<&Value>,
    ) -> Result<Self> {
        private_dir(home).await?;
        private_dir(&home.join("tmp")).await?;
        let password = crate::config::id();
        let mut env = environment(config, home);
        env.insert(
            "TMPDIR".into(),
            home.join("tmp").to_string_lossy().into_owned(),
        );
        env.insert("OPENCODE_SERVER_PASSWORD".into(), password.clone());
        env.insert("OPENCODE_SERVER_USERNAME".into(), "cairn".into());
        env.insert("OPENCODE_CONFIG_CONTENT".into(), native.to_string());
        if let Some(credential) = credentials {
            if let Some(auth) = credential["auth"].as_object() {
                env.insert(
                    "OPENCODE_AUTH_CONTENT".into(),
                    Value::Object(auth.clone()).to_string(),
                );
            } else {
                let provider = text(credential, "modelProvider");
                let key = text(credential, "key");
                if provider.is_empty() || key.is_empty() {
                    return Err(Error::bad(
                        "OpenCode account authentication is unavailable.",
                    ));
                }
                let mut auth = serde_json::Map::new();
                auth.insert(
                    provider.into(),
                    json!({
                        "type": "api",
                        "key": key,
                    }),
                );
                env.insert(
                    "OPENCODE_AUTH_CONTENT".into(),
                    Value::Object(auth).to_string(),
                );
            }
        }
        let args = [
            "serve",
            "--hostname",
            "127.0.0.1",
            "--port",
            "0",
            "--log-level",
            "ERROR",
        ]
        .map(str::to_owned);
        let mut cmd = command(&config.opencode_bin, &args, &env, Some(cwd));
        cmd.process_group(0);
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(3))
            .build()
            .map_err(Error::internal)?;
        let mut child = cmd.spawn().map_err(|_| {
            Error::unavailable(format!(
                "Unable to start OpenCode. Install OpenCode {MINIMUM} or newer on the server."
            ))
        })?;
        let stdout = child.stdout.take().unwrap();
        let mut stderr = child.stderr.take().unwrap();
        let drain = tokio::spawn(async move {
            let _ = tokio::io::copy(&mut stderr, &mut tokio::io::sink()).await;
        });
        let mut server = Self {
            child,
            drains: vec![drain],
            client,
            url: Url::parse("http://127.0.0.1:1").unwrap(),
            password,
            directory: cwd.to_string_lossy().into_owned(),
        };
        let mut stdout = BufReader::new(stdout);
        server.url = wait_for_server(&mut stdout).await?;
        server.drains.push(tokio::spawn(async move {
            let _ = tokio::io::copy(&mut stdout, &mut tokio::io::sink()).await;
        }));
        let health = server.get("/global/health").await?;
        if !supported_version(health["version"].as_str().unwrap_or_default()) {
            return Err(Error::bad(format!(
                "This integration requires OpenCode {MINIMUM} or newer. Update OPENCODE_BIN or the runner image."
            )));
        }
        Ok(server)
    }

    fn request(&self, method: Method, path: &str) -> Result<reqwest::RequestBuilder> {
        let mut url = self.url.join(path).map_err(Error::internal)?;
        url.query_pairs_mut()
            .append_pair("directory", &self.directory);
        Ok(self
            .client
            .request(method, url)
            .basic_auth("cairn", Some(&self.password)))
    }

    async fn response(&self, method: Method, path: &str, body: Option<&Value>) -> Result<Response> {
        let mut request = self.request(method, path)?;
        if let Some(body) = body {
            request = request.json(body);
        }
        let response = request
            .timeout(Duration::from_secs(30))
            .send()
            .await
            .map_err(|_| Error::bad_gateway("OpenCode could not complete a session request."))?;
        if !response.status().is_success() {
            return Err(Error::bad_gateway(format!(
                "OpenCode session request failed (HTTP {}).",
                response.status().as_u16()
            )));
        }
        Ok(response)
    }

    pub(super) async fn get(&self, path: &str) -> Result<Value> {
        let response = self.response(Method::GET, path, None).await?;
        let stream = response.bytes_stream().map(|chunk| {
            chunk.map_err(|_| std::io::Error::other("OpenCode response disconnected."))
        });
        let bytes = read_bounded(tokio_util::io::StreamReader::new(stream), LIMIT).await?;
        serde_json::from_slice(&bytes)
            .map_err(|_| Error::bad_gateway("OpenCode returned an invalid session response."))
    }

    pub(super) async fn post(&self, path: &str, body: &Value) -> Result<()> {
        self.response(Method::POST, path, Some(body)).await?;
        Ok(())
    }

    pub(super) async fn create(&self) -> Result<Value> {
        let response = self
            .response(Method::POST, "/session", Some(&json!({})))
            .await?;
        let bytes = response
            .bytes()
            .await
            .map_err(|_| Error::bad_gateway("OpenCode session creation failed."))?;
        if bytes.len() > 8192 {
            return Err(Error::bad_gateway(
                "OpenCode returned an oversized session.",
            ));
        }
        serde_json::from_slice(&bytes)
            .map_err(|_| Error::bad_gateway("OpenCode returned an invalid session."))
    }

    pub(super) async fn subscribe(&mut self) -> Result<mpsc::Receiver<Result<Value>>> {
        let response = self
            .request(Method::GET, "/event")?
            .send()
            .await
            .map_err(|_| Error::bad_gateway("OpenCode event stream is unavailable."))?;
        if !response.status().is_success() {
            return Err(Error::bad_gateway("OpenCode event stream was rejected."));
        }
        let (sender, receiver) = mpsc::channel(64);
        self.drains.push(tokio::spawn(async move {
            let mut stream = response.bytes_stream();
            let mut buffer = Vec::new();
            while let Some(chunk) = stream.next().await {
                let Ok(chunk) = chunk else {
                    break;
                };
                buffer.extend_from_slice(&chunk);
                if buffer.len() > LIMIT {
                    let _ = sender
                        .send(Err(Error::bad_gateway(
                            "OpenCode event exceeded the supported limit.",
                        )))
                        .await;
                    return;
                }
                while let Some(end) = buffer.iter().position(|b| *b == b'\n') {
                    let line = buffer.drain(..=end).collect::<Vec<_>>();
                    let Some(data) = line.strip_prefix(b"data:") else {
                        continue;
                    };
                    let Ok(value) = serde_json::from_slice(data) else {
                        continue;
                    };
                    if sender.send(Ok(value)).await.is_err() {
                        return;
                    }
                }
            }
            let _ = sender
                .send(Err(Error::bad_gateway(
                    "OpenCode disconnected before completing its response.",
                )))
                .await;
        }));
        Ok(receiver)
    }
}

async fn wait_for_server(reader: &mut (impl tokio::io::AsyncBufRead + Unpin)) -> Result<Url> {
    let ready = async {
        loop {
            let line = read_bounded_line(reader, 8192).await?;
            if let Some(start) = line.find("http://127.0.0.1:") {
                let address = line[start..]
                    .chars()
                    .take_while(|c| c.is_ascii_alphanumeric() || ":./".contains(*c))
                    .collect::<String>();
                return Url::parse(&address).map_err(|_| {
                    Error::bad_gateway("OpenCode returned an invalid server address.")
                });
            }
        }
    };
    // Restored native state can take longer to initialize on a cold VM. Keep the
    // wait bounded, while the execution's cancellation still drops the process group.
    tokio::time::timeout(Duration::from_secs(120), ready)
        .await
        .map_err(|_| Error::gateway_timeout("OpenCode did not start in time."))?
}

async fn read_bounded_line(
    reader: &mut (impl tokio::io::AsyncBufRead + Unpin),
    limit: usize,
) -> Result<String> {
    let mut result = Vec::new();
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            return Err(Error::bad_gateway(
                "OpenCode exited before starting its server.",
            ));
        }
        let end = available.iter().position(|b| *b == b'\n');
        let count = end.map_or(available.len(), |n| n + 1);
        if result.len() + count > limit {
            return Err(Error::bad_gateway(
                "OpenCode server output exceeded the supported limit.",
            ));
        }
        result.extend_from_slice(&available[..count]);
        reader.consume(count);
        if end.is_some() {
            return Ok(String::from_utf8_lossy(&result).into_owned());
        }
    }
}

#[derive(Default)]
pub struct Catalog {
    cached: Mutex<Option<(i64, Value)>>,
    models: Mutex<Option<ModelCache>>,
    /// The last model catalog, readable without waiting for a refresh in progress.
    latest: std::sync::RwLock<Option<Value>>,
}

struct ModelCache {
    accounts: Vec<(String, Option<String>)>,
    at: i64,
    value: Value,
}

impl Catalog {
    async fn raw(&self, s: &Service) -> Result<Value> {
        let mut cached = self.cached.lock().await;
        if let Some((at, value)) = &*cached
            && now() - at < 300_000
        {
            return Ok(value.clone());
        }
        let home = s.config.data_dir.join("opencode-catalog");
        private_dir(&home).await?;
        let native = json!({
            "share": "disabled",
            "autoupdate": false,
            "plugin": [],
        });
        let server = Server::start(&s.config, &home, &home, &native, None).await?;
        let value = server.get("/provider").await?;
        *cached = Some((now(), value.clone()));
        Ok(value)
    }

    pub async fn providers(&self, s: &Service) -> Result<Value> {
        let raw = self.raw(s).await?;
        Ok(raw["all"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|p| API_PROVIDERS.contains(&text(p, "id")))
            .map(|p| {
                json!({
                    "id": p["id"],
                    "name": provider_name(p),
                })
            })
            .collect::<Vec<_>>()
            .into())
    }

    /// One entry per model, with every connected provider offering it as a route in the
    /// user's connection order. An entry's own `auto/…` id runs on the first available route.
    pub async fn models(&self, s: &Service) -> Result<Value> {
        let mut accounts = s
            .accounts
            .of(s, Provider::Opencode)
            .await?
            .into_iter()
            .filter(crate::accounts::Account::is_active)
            .collect::<Vec<_>>();
        accounts.sort_by(|a, b| a.priority().cmp(&b.priority()).then(a.id.cmp(&b.id)));
        let mut connected = Vec::<String>::new();
        for provider in accounts
            .iter()
            .filter_map(|a| a.extra.get("modelProvider").and_then(Value::as_str))
        {
            if !connected.iter().any(|p| p == provider) {
                connected.push(provider.to_owned());
            }
        }
        if connected.is_empty() {
            return Ok(json!({
                "models": [],
                "checkedAt": null,
                "stale": false,
                "error": "Connect an OpenCode model-provider API key in Connections to choose a model.",
            }));
        }
        let fingerprint = accounts
            .iter()
            .map(|a| (a.id.clone(), a.identity.clone()))
            .collect::<Vec<_>>();
        let mut cached = self.models.lock().await;
        if let Some(cache) = &*cached
            && cache.accounts == fingerprint
            && now() - cache.at < 300_000
        {
            return Ok(cache.value.clone());
        }
        let mut auth = serde_json::Map::new();
        for account in &accounts {
            let credential = crate::accounts::opencode::credentials(s, &account.id).await?;
            let provider = text(&credential, "modelProvider");
            auth.entry(provider.to_owned()).or_insert_with(|| {
                json!({
                    "type": "api",
                    "key": credential["key"],
                })
            });
        }
        // OpenCode's Zen inventory includes paid models only when a key is connected.
        // Use manager-side keys for discovery, then cache only public model metadata.
        let home = s.config.data_dir.join("opencode-models");
        private_dir(&home).await?;
        let native = json!({
            "share": "disabled",
            "autoupdate": false,
            "plugin": [],
        });
        let server = Server::start(
            &s.config,
            &home,
            &home,
            &native,
            Some(&json!({ "auth": auth })),
        )
        .await?;
        let raw = server.get("/provider").await?;
        let value = json!({
            "models": group(&raw, &connected),
            "checkedAt": now(),
            "stale": false,
            "error": "",
        });
        *cached = Some(ModelCache {
            accounts: fingerprint,
            at: now(),
            value: value.clone(),
        });
        *self
            .latest
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(value.clone());
        Ok(value)
    }

    /// The `provider/model` that runs `model` on `provider`, if that provider offers it.
    pub async fn route(&self, s: &Service, model: &str, provider: &str) -> Result<Option<Value>> {
        if !model.starts_with(AUTO) {
            return pinned(model, provider);
        }
        Ok(lookup(&self.models(s).await?, model, provider))
    }

    /// [`Catalog::route`] from the last catalog, without discovery: account selection
    /// asks while holding its locks, after [`Catalog::models`] warmed the catalog.
    pub fn cached_route(&self, model: &str, provider: &str) -> Result<Option<Value>> {
        if !model.starts_with(AUTO) {
            return pinned(model, provider);
        }
        let latest = self
            .latest
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Ok(latest
            .as_ref()
            .and_then(|catalog| lookup(catalog, model, provider)))
    }
}

impl Catalog {
    /// The last catalog's name for `provider/model`.
    pub fn display_name(&self, model: &str) -> Option<String> {
        let latest = self
            .latest
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        latest.as_ref()?["models"]
            .as_array()?
            .iter()
            .find(|entry| {
                entry["routes"]
                    .as_array()
                    .is_some_and(|routes| routes.iter().any(|r| r["model"] == model))
            })
            .map(|entry| text(entry, "displayName").to_owned())
    }
}

fn pinned(model: &str, provider: &str) -> Result<Option<Value>> {
    let (owner, _) = model_id(model)?;
    Ok((owner == provider).then(|| json!({ "model": model, "provider": provider })))
}

/// The route of `auto/…` `model` on `provider`. Auto never selects a deprecated route.
fn lookup(catalog: &Value, model: &str, provider: &str) -> Option<Value> {
    let entry = catalog["models"]
        .as_array()?
        .iter()
        .find(|entry| entry["model"] == model)?;
    let route = entry["routes"]
        .as_array()?
        .iter()
        .find(|route| route["provider"] == provider && route["hidden"] != true)?;
    let mut route = route.clone();
    route["displayName"] = entry["displayName"].clone();
    Some(route)
}

/// Selects a model on whichever connected provider is available first.
pub const AUTO: &str = "auto/";

/// The same model under different providers' names: `opencode/claude-sonnet-4-5`,
/// `anthropic/claude-sonnet-4.5` and `openrouter/anthropic/claude-sonnet-4.5`.
pub fn canonical(model: &str) -> String {
    model
        .rsplit('/')
        .next()
        .unwrap_or(model)
        .to_ascii_lowercase()
        .replace(['.', '_'], "-")
}

/// Who pays for a provider's usage, shown beside each route.
fn billing(provider: &str) -> &'static str {
    match provider {
        "opencode-go" => "subscription",
        "opencode" => "credits",
        _ => "key",
    }
}

/// Reasoning levels from least to most; other variant names follow alphabetically.
const EFFORTS: &[&str] = &[
    "none", "minimal", "low", "medium", "high", "xhigh", "max", "ultra",
];

fn efforts(model: &Value) -> Vec<Value> {
    let mut variants = model["variants"]
        .as_object()
        .into_iter()
        .flatten()
        .filter(|(name, value)| {
            let mut chars = name.chars();
            name.len() <= 40
                && chars.next().is_some_and(|c| c.is_ascii_lowercase())
                && chars
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
                && value["disabled"] != true
        })
        .map(|(variant, _)| variant.as_str())
        .collect::<Vec<_>>();
    variants.sort_by_key(|v| {
        (
            EFFORTS.iter().position(|e| e == v).unwrap_or(EFFORTS.len()),
            *v,
        )
    });
    variants
        .into_iter()
        .map(|variant| {
            json!({
                "reasoningEffort": variant,
                "description": "Model variant",
            })
        })
        .collect()
}

/// Groups `/provider`'s models across `connected` providers, in that order.
fn group(raw: &Value, connected: &[String]) -> Vec<Value> {
    let providers = raw["all"].as_array().map_or(&[][..], Vec::as_slice);
    // The first connection whose provider names a default model.
    let preferred = connected
        .iter()
        .find(|id| raw["default"][id.as_str()].is_string())
        .unwrap_or(&connected[0]);
    let mut entries = Vec::<Value>::new();
    for provider in connected
        .iter()
        .filter_map(|id| providers.iter().find(|p| p["id"] == id.as_str()))
    {
        let provider_id = text(provider, "id");
        let mut models = provider["models"]
            .as_object()
            .into_iter()
            .flatten()
            .collect::<Vec<_>>();
        models.sort_by_key(|(id, _)| id.as_str());
        for (id, model) in models {
            let qualified = format!("{provider_id}/{id}");
            let key = canonical(id);
            if qualified.len() > 100 || AUTO.len() + key.len() > 100 {
                continue;
            }
            let route = json!({
                "model": qualified,
                "provider": provider_id,
                "providerName": provider_name(provider),
                "billing": billing(provider_id),
                "hidden": model["status"] == "deprecated",
                "supportedReasoningEfforts": efforts(model),
            });
            let is_default = provider_id == preferred && raw["default"][provider_id] == id.as_str();
            if let Some(entry) = entries.iter_mut().find(|e| e["key"] == key.as_str()) {
                // A provider can list one model under two names; keep its first.
                if entry["routes"]
                    .as_array()
                    .is_some_and(|r| r.iter().any(|r| r["provider"] == provider_id))
                {
                    continue;
                }
                entry["routes"].as_array_mut().unwrap().push(route);
                entry["isDefault"] = (entry["isDefault"] == true || is_default).into();
                continue;
            }
            entries.push(json!({
                "key": key,
                "model": format!("{AUTO}{key}"),
                "displayName": text(model, "name"),
                "isDefault": is_default,
                "defaultReasoningEffort": "",
                "routes": [route],
            }));
        }
    }
    for entry in &mut entries {
        let routes = entry["routes"].as_array().cloned().unwrap_or_default();
        let available = routes
            .iter()
            .filter(|r| r["hidden"] != true)
            .collect::<Vec<_>>();
        let names = available
            .iter()
            .map(|r| text(r, "providerName"))
            .collect::<Vec<_>>();
        // Auto may run on any route, so it offers only the variants all of them share.
        let shared = available.first().map_or_else(Vec::new, |first| {
            first["supportedReasoningEfforts"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|effort| {
                    available.iter().all(|r| {
                        r["supportedReasoningEfforts"]
                            .as_array()
                            .is_some_and(|e| e.contains(effort))
                    })
                })
                .cloned()
                .collect()
        });
        entry["hidden"] = available.is_empty().into();
        entry["supportedReasoningEfforts"] = shared.into();
        entry["description"] = match names.as_slice() {
            [] => "Deprecated model".into(),
            [only] => format!("Via {only}"),
            [first, rest @ ..] => format!("Via {first}, then {}", rest.join(", then ")),
        }
        .into();
    }
    entries.sort_by_key(|entry| {
        (
            entry["isDefault"] != true,
            text(entry, "displayName").to_lowercase(),
        )
    });
    entries
}

/// A plain `MAJOR.MINOR.PATCH` release.
fn release(version: &str) -> Option<[u64; 3]> {
    let mut parts = version.split('.').map(|part| {
        (!part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
            .then(|| part.parse().ok())
            .flatten()
    });
    let release = [parts.next()??, parts.next()??, parts.next()??];
    parts.next().is_none().then_some(release)
}

/// A stable release at or after [`MINIMUM`]; prereleases and snapshots never qualify.
fn supported_version(version: &str) -> bool {
    release(version)
        .is_some_and(|version| release(MINIMUM).is_some_and(|minimum| version >= minimum))
}

fn provider_name(provider: &Value) -> &str {
    if provider["id"] == "opencode" {
        "OpenCode Zen"
    } else {
        text(provider, "name")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_stable_releases_from_the_minimum_are_supported() {
        for version in ["1.19.0", "1.19.4", "1.19.12", "1.20.0", "1.190.0", "2.0.0"] {
            assert!(supported_version(version), "{version}");
        }
        for version in [
            "",
            "1.19",
            "1.19.",
            "1.18.9",
            "0.99.99",
            "1.19.4.1",
            "+1.19.4",
            "1.19.5-beta.1",
            "0.0.0-dev-202610221304",
            "1.19.4 ",
        ] {
            assert!(!supported_version(version), "{version}");
        }
    }

    #[tokio::test(start_paused = true)]
    async fn restored_native_state_can_start_after_the_previous_deadline() {
        use tokio::io::AsyncWriteExt;
        let (mut writer, reader) = tokio::io::duplex(256);
        let output = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(60)).await;
            writer
                .write_all(b"opencode server listening on http://127.0.0.1:12345\n")
                .await
                .unwrap();
        });
        let url = wait_for_server(&mut BufReader::new(reader)).await.unwrap();
        assert_eq!(url.as_str(), "http://127.0.0.1:12345/");
        output.await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn native_startup_still_has_a_deadline_and_exits_fail_immediately() {
        let (_writer, reader) = tokio::io::duplex(256);
        let started = tokio::time::Instant::now();
        let error = wait_for_server(&mut BufReader::new(reader))
            .await
            .unwrap_err();
        assert!(error.message.contains("did not start in time"));
        assert_eq!(started.elapsed(), Duration::from_secs(120));

        let (writer, reader) = tokio::io::duplex(256);
        drop(writer);
        let started = tokio::time::Instant::now();
        let error = wait_for_server(&mut BufReader::new(reader))
            .await
            .unwrap_err();
        assert!(error.message.contains("exited before starting"));
        assert_eq!(started.elapsed(), Duration::ZERO);
    }

    fn model(name: &str, variants: &[&str]) -> Value {
        json!({
            "name": name,
            "status": "active",
            "variants": variants.iter().map(|v| (v.to_string(), json!({}))).collect::<serde_json::Map<_, _>>(),
        })
    }

    #[test]
    fn one_model_under_several_providers_is_one_entry_in_connection_order() {
        let raw = json!({
            "all": [
                { "id": "opencode", "name": "OpenCode", "models": {
                    "claude-sonnet-4-5": model("Claude Sonnet 4.5", &["low", "high"]),
                    "kimi-k2": model("Kimi K2", &[]),
                } },
                { "id": "opencode-go", "name": "OpenCode Go", "models": {
                    "claude-sonnet-4.5": model("Claude Sonnet 4.5", &["high", "max"]),
                } },
                { "id": "openrouter", "name": "OpenRouter", "models": {
                    "anthropic/claude-sonnet-4.5": model("Anthropic: Claude Sonnet 4.5", &["high"]),
                } },
                { "id": "anthropic", "name": "Anthropic", "models": {
                    "claude-sonnet-4-5": model("Claude Sonnet 4.5", &["high"]),
                } },
            ],
            "default": { "opencode": "kimi-k2" },
        });
        let connected = ["opencode-go", "opencode", "openrouter"].map(String::from);
        let entries = group(&raw, &connected);
        assert_eq!(entries.len(), 2);
        // Go names no default, so the first connection that does chooses it.
        assert_eq!(entries[0]["model"], "auto/kimi-k2");
        assert_eq!(entries[0]["isDefault"], true);
        assert_eq!(entries[0]["description"], "Via OpenCode Zen");
        let sonnet = &entries[1];
        assert_eq!(sonnet["model"], "auto/claude-sonnet-4-5");
        assert_eq!(sonnet["displayName"], "Claude Sonnet 4.5");
        let routes = sonnet["routes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| (text(r, "model"), text(r, "billing")))
            .collect::<Vec<_>>();
        // Anthropic is not connected, so it offers no route.
        assert_eq!(
            routes,
            [
                ("opencode-go/claude-sonnet-4.5", "subscription"),
                ("opencode/claude-sonnet-4-5", "credits"),
                ("openrouter/anthropic/claude-sonnet-4.5", "key"),
            ]
        );
        assert_eq!(
            sonnet["description"],
            "Via OpenCode Go, then OpenCode Zen, then OpenRouter"
        );
        // Auto can run on any route, so it offers only the level they all share.
        assert_eq!(
            sonnet["supportedReasoningEfforts"],
            json!([{ "reasoningEffort": "high", "description": "Model variant" }])
        );
    }

    #[test]
    fn canonical_names_ignore_provider_prefixes_and_separators() {
        assert_eq!(
            canonical("anthropic/claude-sonnet-4.5"),
            "claude-sonnet-4-5"
        );
        assert_eq!(canonical("Claude_Sonnet_4-5"), "claude-sonnet-4-5");
        assert_ne!(
            canonical("ling-3.1-flash-free"),
            canonical("ling-3.1-flash")
        );
    }
}
