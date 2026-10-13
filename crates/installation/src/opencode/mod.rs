//! OpenCode 1.19's native HTTP session protocol. Each execution gets a private
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

pub const VERSION: &str = "1.19.0";
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
            Error::unavailable("Unable to start OpenCode. Install OpenCode 1.19.0 on the server.")
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
        let ready = async {
            loop {
                let line = read_bounded_line(&mut stdout, 8192).await?;
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
        server.url = tokio::time::timeout(Duration::from_secs(45), ready)
            .await
            .map_err(|_| Error::gateway_timeout("OpenCode did not start in time."))??;
        server.drains.push(tokio::spawn(async move {
            let _ = tokio::io::copy(&mut stdout, &mut tokio::io::sink()).await;
        }));
        let health = server.get("/global/health").await?;
        if health["version"] != VERSION {
            return Err(Error::bad(
                "This integration requires OpenCode 1.19.0. Update OPENCODE_BIN or the runner image.",
            ));
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

    pub async fn models(&self, s: &Service) -> Result<Value> {
        let accounts = s
            .accounts
            .of(s, Provider::Opencode)
            .await?
            .into_iter()
            .filter(crate::accounts::Account::is_ready)
            .collect::<Vec<_>>();
        let connected = accounts
            .iter()
            .filter_map(|a| {
                a.extra
                    .get("modelProvider")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .collect::<Vec<_>>();
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
        let preferred = &connected[0];
        let mut models = Vec::new();
        for provider in raw["all"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|p| connected.iter().any(|id| p["id"] == id.as_str()))
        {
            let provider_id = text(provider, "id");
            for (id, model) in provider["models"].as_object().into_iter().flatten() {
                if format!("{provider_id}/{id}").len() > 100 {
                    continue;
                }
                let variants = model["variants"]
                    .as_object()
                    .into_iter()
                    .flatten()
                    .filter(|(name, value)| {
                        let mut chars = name.chars();
                        name.len() <= 40
                            && chars.next().is_some_and(|c| c.is_ascii_lowercase())
                            && chars.all(|c| {
                                c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-'
                            })
                            && value["disabled"] != true
                    })
                    .map(|(variant, _)| {
                        json!({
                            "reasoningEffort": variant,
                            "description": "Model variant",
                        })
                    })
                    .collect::<Vec<_>>();
                models.push(json!({
                    "model": format!("{provider_id}/{id}"),
                    "displayName": format!("{} · {}", text(model, "name"), provider_name(provider)),
                    "description": format!("{} model via OpenCode", provider_name(provider)),
                    "hidden": model["status"] == "deprecated",
                    "isDefault": provider_id == preferred && raw["default"][provider_id] == id.as_str(),
                    "defaultReasoningEffort": "",
                    "supportedReasoningEfforts": variants,
                }));
            }
        }
        models.sort_by_key(|model| {
            (
                !model["isDefault"].as_bool().unwrap_or(false),
                text(model, "displayName").to_owned(),
            )
        });
        let value = json!({
            "models": models,
            "checkedAt": now(),
            "stale": false,
            "error": "",
        });
        *cached = Some(ModelCache {
            accounts: fingerprint,
            at: now(),
            value: value.clone(),
        });
        Ok(value)
    }
}

fn provider_name(provider: &Value) -> &str {
    if provider["id"] == "opencode" {
        "OpenCode Zen"
    } else {
        text(provider, "name")
    }
}
