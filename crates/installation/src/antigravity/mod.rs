//! Google's Antigravity CLI (`agy`) through its headless stream-json mode, as of 1.5.14.
//! Native state follows `HOME`, so `agy` alone runs with the run's private state as
//! HOME; shell commands it starts get the shell's HOME back through `BASH_ENV`.
pub mod hook;
mod side;
mod turn;

pub use turn::run;

use crate::{
    config::{Config, now},
    error::{Error, Result},
    process::{Environment, bounded_output, command},
    provider::Provider,
    service::Service,
    skills::{atomic_write, private_dir},
    validation::text,
};
use serde_json::{Value, json};
use std::{path::Path, time::Duration};
use tokio::sync::Mutex;

/// The oldest release whose stream-json protocol and hooks this adapter relies on.
/// Each image build installs the release Google's manifests name (scripts/cli-updates.mjs).
pub const MINIMUM: &str = "1.5.14";

/// Account kinds, as saved in an account's model provider.
pub const GEMINI: &str = "gemini";
pub const GOOGLE_CLOUD: &str = "google-cloud";
/// A Google account signed in through `agy`.
pub const GOOGLE: &str = "google";

const EFFORTS: &[&str] = &["minimal", "low", "medium", "high", "xhigh", "max"];

/// Keeps another engine's run, or a tool, from reading this run's Antigravity state.
pub fn isolate(env: &mut Environment, home: &Path) {
    env.retain(|key, _| !key.starts_with("AGY_"));
    env.insert(
        "CAIRN_ANTIGRAVITY_HOME".into(),
        home.to_string_lossy().into_owned(),
    );
}

/// The native process environment. `credentials` comes from the run's broker.
pub(crate) fn environment(
    config: &Config,
    home: &Path,
    credentials: Option<&Value>,
) -> Environment {
    let mut env = std::env::vars().collect::<Environment>();
    crate::process::remove_storage_environment(&mut env);
    // Ambient credentials cannot override a leased account.
    env.retain(|key, _| {
        !key.ends_with("_API_KEY")
            && !key.starts_with("AGY_")
            && !key.starts_with("GOOGLE_CLOUD_")
            && !key.starts_with("GOOGLE_GEMINI_")
            && key != "GOOGLE_APPLICATION_CREDENTIALS"
            && key != "GOOGLE_GENAI_USE_VERTEXAI"
            && key != "ANTHROPIC_AUTH_TOKEN"
    });
    // Without a session bus, `agy` keeps OAuth tokens in its state directory, not a keyring.
    env.remove("DBUS_SESSION_BUS_ADDRESS");
    let previous_bash_env = env.remove("BASH_ENV");
    env.entry("GH_CONFIG_DIR".into()).or_insert_with(|| {
        config
            .home
            .join(".config/gh")
            .to_string_lossy()
            .into_owned()
    });
    isolate(&mut env, home);
    env.insert("HOME".into(), home.to_string_lossy().into_owned());
    env.insert("AGY_CLI_DISABLE_AUTO_UPDATE".into(), "true".into());
    env.insert(
        "BASH_ENV".into(),
        home.join(SHELL_ENV).to_string_lossy().into_owned(),
    );
    if let Some(previous) = previous_bash_env {
        env.insert("CAIRN_PREVIOUS_BASH_ENV".into(), previous);
    }
    if let Some(credentials) = credentials {
        match text(credentials, "kind") {
            GEMINI => {
                env.insert("GEMINI_API_KEY".into(), text(credentials, "key").into());
            }
            GOOGLE_CLOUD => {
                env.insert("AGY_ADC_AUTH".into(), "true".into());
                env.insert(
                    "GOOGLE_APPLICATION_CREDENTIALS".into(),
                    home.join(ADC_FILE).to_string_lossy().into_owned(),
                );
                env.insert(
                    "GOOGLE_CLOUD_PROJECT".into(),
                    text(credentials, "project").into(),
                );
                env.insert(
                    "GOOGLE_CLOUD_QUOTA_PROJECT".into(),
                    text(credentials, "project").into(),
                );
                env.insert(
                    "GOOGLE_CLOUD_LOCATION".into(),
                    text(credentials, "location").into(),
                );
            }
            _ => {}
        }
    }
    env
}

pub(crate) const SHELL_ENV: &str = "cairn-shell-env.sh";
pub(crate) const ADC_FILE: &str = "cairn-adc.json";
/// Where `agy` reads a Google account's token without a keyring.
pub(crate) const TOKEN_FILE: &str = ".gemini/antigravity-cli/antigravity-oauth-token";

pub(crate) async fn write_token(home: &Path, token: &Value) -> Result<()> {
    private_dir(&home.join(".gemini/antigravity-cli")).await?;
    atomic_write(&home.join(TOKEN_FILE), &serde_json::to_vec(token)?).await
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// Writes the files `agy` reads before it starts: the shell environment that restores
/// the shell's HOME for commands, and an application-default credential file.
pub(crate) async fn prepare_native_home(
    config: &Config,
    home: &Path,
    credentials: Option<&Value>,
) -> Result<()> {
    private_dir(home).await?;
    let script = format!(
        "# Written by Cairn: commands keep the shell's HOME, not Antigravity's state.\n\
         export HOME={home}\n\
         unset GEMINI_API_KEY GOOGLE_APPLICATION_CREDENTIALS AGY_ADC_AUTH GOOGLE_CLOUD_PROJECT GOOGLE_CLOUD_QUOTA_PROJECT GOOGLE_CLOUD_LOCATION CAIRN_ANTIGRAVITY_HOME\n\
         if [ -n \"${{CAIRN_PREVIOUS_BASH_ENV:-}}\" ]; then\n  \
           __cairn_bash_env=\"$CAIRN_PREVIOUS_BASH_ENV\"; unset CAIRN_PREVIOUS_BASH_ENV; . \"$__cairn_bash_env\"; unset __cairn_bash_env\n\
         fi\n\
         unset BASH_ENV\n",
        home = shell_quote(&config.home.to_string_lossy()),
    );
    atomic_write(&home.join(SHELL_ENV), script.as_bytes()).await?;
    match credentials {
        Some(credentials) if credentials["kind"] == GOOGLE_CLOUD => {
            let file = serde_json::to_vec(&credentials["credentials"])?;
            atomic_write(&home.join(ADC_FILE), &file).await?;
        }
        _ => crate::accounts::remove_file(&home.join(ADC_FILE)).await?,
    }
    match credentials {
        // An access token only: the manager keeps the refresh token.
        Some(credentials) if credentials["kind"] == GOOGLE => {
            write_token(home, &credentials["token"]).await?;
        }
        Some(_) => crate::accounts::remove_file(&home.join(TOKEN_FILE)).await?,
        None => {}
    }
    Ok(())
}

/// A plain `MAJOR.MINOR.PATCH` release.
fn release(version: &str) -> Option<[u64; 3]> {
    let mut parts = version.trim().split('.').map(|part| {
        (!part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
            .then(|| part.parse().ok())
            .flatten()
    });
    let release = [parts.next()??, parts.next()??, parts.next()??];
    parts.next().is_none().then_some(release)
}

/// A stable release at or after [`MINIMUM`].
pub(crate) fn supported_version(version: &str) -> bool {
    release(version)
        .is_some_and(|version| release(MINIMUM).is_some_and(|minimum| version >= minimum))
}

pub(crate) async fn check_version(config: &Config, env: &Environment) -> Result<()> {
    let output = bounded_output(
        command(&config.antigravity_bin, &["--version".into()], env, None),
        Duration::from_secs(30),
        4096,
    )
    .await
    .map_err(|_| Error::unavailable("The Antigravity CLI is not installed."))?;
    if !output.success || !supported_version(&output.stdout) {
        return Err(Error::unavailable(format!(
            "This Antigravity CLI release is not supported. Install {MINIMUM} or later."
        )));
    }
    Ok(())
}

/// Splits a model's display name into its family and reasoning level:
/// `Gemini 4 Neon (High)` is `Gemini 4 Neon` at `high`.
fn display_level(name: &str) -> (&str, Option<String>) {
    if let Some(stripped) = name.strip_suffix(')')
        && let Some((family, level)) = stripped.rsplit_once(" (")
    {
        let level = level.to_ascii_lowercase();
        if EFFORTS.contains(&level.as_str()) {
            return (family, Some(level));
        }
    }
    (name, None)
}

fn common_prefix(slugs: &[&str]) -> String {
    let first = slugs.first().copied().unwrap_or_default();
    let mut length = first.len();
    for slug in &slugs[1..] {
        length = first
            .bytes()
            .zip(slug.bytes())
            .take(length)
            .take_while(|(a, b)| a == b)
            .count();
    }
    first[..length].trim_end_matches(['-', '.', '_']).to_owned()
}

pub fn valid_slug(slug: &str) -> bool {
    !slug.is_empty()
        && slug.len() <= 100
        && slug
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "._-@".contains(c))
}

/// Groups `agy models` output (`slug<TAB>display name` lines) into one entry per model
/// family, each reasoning level naming the native slug that runs it.
pub(crate) fn parse_models(output: &str) -> Vec<Value> {
    let mut families = Vec::<(String, Vec<(Option<String>, String)>)>::new();
    for line in output.lines() {
        let Some((slug, name)) = line.split_once('\t') else {
            continue;
        };
        let (slug, name) = (slug.trim(), name.trim());
        if !valid_slug(slug) || name.is_empty() || name.len() > 200 {
            continue;
        }
        let (family, level) = display_level(name);
        match families.iter_mut().find(|(f, _)| f == family) {
            Some((_, levels)) => {
                if !levels.iter().any(|(l, _)| *l == level) {
                    levels.push((level, slug.into()));
                }
            }
            None => families.push((family.into(), vec![(level, slug.into())])),
        }
    }
    families
        .into_iter()
        .map(|(family, mut levels)| {
            levels.sort_by_key(|(level, _)| {
                level
                    .as_deref()
                    .and_then(|l| EFFORTS.iter().position(|e| *e == l))
                    .unwrap_or(EFFORTS.len())
            });
            let slugs = levels.iter().map(|(_, s)| s.as_str()).collect::<Vec<_>>();
            let leveled = levels.iter().all(|(level, _)| level.is_some());
            let id = if leveled && levels.len() > 1 {
                common_prefix(&slugs)
            } else {
                slugs[0].to_owned()
            };
            let id = if valid_slug(&id) {
                id
            } else {
                slugs[0].to_owned()
            };
            let default = levels
                .iter()
                .find(|(level, _)| level.as_deref() == Some("high"))
                .or_else(|| levels.last())
                .and_then(|(level, _)| level.clone())
                .unwrap_or_default();
            json!({
                "model": id,
                "displayName": family,
                "defaultReasoningEffort": if leveled { default } else { String::new() },
                "supportedReasoningEfforts": if leveled {
                    levels.iter().map(|(level, _)| json!({
                        "reasoningEffort": level,
                        "description": "Reasoning level",
                    })).collect::<Vec<_>>()
                } else {
                    Vec::new()
                },
                "slugs": levels.iter().map(|(level, slug)| {
                    (level.clone().unwrap_or_default(), Value::from(slug.as_str()))
                }).collect::<serde_json::Map<_, _>>(),
            })
        })
        .collect()
}

/// The native slug that runs `model` at `reasoning`, in one account kind's catalog.
pub(crate) fn slug(models: &[Value], model: &str, reasoning: &str) -> Option<String> {
    let entry = models.iter().find(|entry| entry["model"] == model)?;
    let slugs = entry["slugs"].as_object()?;
    let level = if reasoning.is_empty() {
        text(entry, "defaultReasoningEffort")
    } else {
        reasoning
    };
    slugs
        .get(level)
        .or_else(|| (slugs.len() == 1).then(|| slugs.values().next()).flatten())
        .and_then(Value::as_str)
        .map(str::to_owned)
}

pub fn kind_name(kind: &str) -> &'static str {
    match kind {
        GOOGLE_CLOUD => "Google Cloud",
        GOOGLE => "Google account",
        _ => "Gemini API",
    }
}

/// Each connected account kind with its models.
type Inventories = Vec<(String, Vec<Value>)>;

/// Model inventories of each connected account kind, read with manager-side credentials.
#[derive(Default)]
pub struct Catalog {
    cached: Mutex<Option<CatalogCache>>,
    latest: std::sync::RwLock<Option<Inventories>>,
}

struct CatalogCache {
    accounts: Vec<(String, Option<String>)>,
    at: i64,
    value: Value,
}

impl Catalog {
    async fn list(s: &Service, credentials: &Value) -> Result<Vec<Value>> {
        let home = s
            .config
            .data_dir
            .join("antigravity-catalog")
            .join(text(credentials, "kind"));
        prepare_native_home(&s.config, &home, Some(credentials)).await?;
        let settings_dir = home.join(".gemini/antigravity-cli");
        private_dir(&settings_dir).await?;
        let mut settings = json!({});
        if credentials["kind"] == GEMINI {
            settings["modelProvider"] = GEMINI.into();
        }
        atomic_write(
            &settings_dir.join("settings.json"),
            &serde_json::to_vec(&settings)?,
        )
        .await?;
        let env = environment(&s.config, &home, Some(credentials));
        check_version(&s.config, &env).await?;
        let output = bounded_output(
            command(
                &s.config.antigravity_bin,
                &["models".into()],
                &env,
                Some(&home),
            ),
            Duration::from_secs(60),
            1_000_000,
        )
        .await?;
        let models = parse_models(&output.stdout);
        if !output.success || models.is_empty() {
            return Err(Error::bad_gateway(
                "Antigravity could not list models. Check this account's credentials.",
            ));
        }
        Ok(models)
    }

    /// One entry per model offered by any connected account kind.
    pub async fn models(&self, s: &Service) -> Result<Value> {
        let mut accounts = s
            .accounts
            .of(s, Provider::Antigravity)
            .await?
            .into_iter()
            .filter(crate::accounts::Account::is_active)
            .collect::<Vec<_>>();
        accounts.sort_by(|a, b| a.priority().cmp(&b.priority()).then(a.id.cmp(&b.id)));
        if accounts.is_empty() {
            return Ok(json!({
                "models": [],
                "checkedAt": null,
                "stale": false,
                "error": "Connect a Gemini API key or Google Cloud credentials for Antigravity in Connections to choose a model.",
            }));
        }
        let fingerprint = accounts
            .iter()
            .map(|a| (a.id.clone(), a.identity.clone()))
            .collect::<Vec<_>>();
        let mut cached = self.cached.lock().await;
        if let Some(cache) = &*cached
            && cache.accounts == fingerprint
            && now() - cache.at < 300_000
        {
            return Ok(cache.value.clone());
        }
        let mut kinds = Vec::<(String, Vec<Value>)>::new();
        let mut failure = None;
        for account in &accounts {
            let kind = account.model_provider.clone().unwrap_or_default();
            if kinds.iter().any(|(k, _)| *k == kind) {
                continue;
            }
            let credentials = crate::accounts::antigravity::credentials(s, &account.id).await?;
            match Self::list(s, &credentials).await {
                Ok(models) => kinds.push((kind, models)),
                Err(error) => failure = Some(error),
            }
        }
        if kinds.is_empty() {
            return Err(
                failure.unwrap_or_else(|| Error::bad_gateway("Antigravity could not list models."))
            );
        }
        let mut entries = Vec::<Value>::new();
        for (kind, models) in &kinds {
            for model in models {
                if let Some(entry) = entries.iter_mut().find(|e| e["model"] == model["model"]) {
                    entry["kinds"]
                        .as_array_mut()
                        .unwrap()
                        .push(kind.as_str().into());
                    // Only levels every kind offers can run on whichever account is leased.
                    let offered = model["supportedReasoningEfforts"]
                        .as_array()
                        .cloned()
                        .unwrap_or_default();
                    let shared = entry["supportedReasoningEfforts"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter(|effort| offered.contains(effort))
                        .cloned()
                        .collect::<Vec<_>>();
                    entry["supportedReasoningEfforts"] = shared.into();
                    continue;
                }
                let mut entry = model.clone();
                entry["kinds"] = json!([kind]);
                entries.push(entry);
            }
        }
        let models = entries
            .into_iter()
            .enumerate()
            .map(|(index, mut entry)| {
                let names = entry["kinds"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|kind| kind_name(kind.as_str().unwrap_or_default()))
                    .collect::<Vec<_>>();
                let efforts = entry["supportedReasoningEfforts"].clone();
                let default = text(&entry, "defaultReasoningEffort").to_owned();
                let default = if efforts
                    .as_array()
                    .is_some_and(|e| e.iter().any(|e| e["reasoningEffort"] == default.as_str()))
                {
                    default
                } else {
                    efforts[0]["reasoningEffort"]
                        .as_str()
                        .unwrap_or_default()
                        .to_owned()
                };
                entry["description"] = format!("Via {}", names.join(", ")).into();
                entry["defaultReasoningEffort"] = default.into();
                entry["isDefault"] = (index == 0).into();
                entry["hidden"] = false.into();
                entry["routes"] = json!([]);
                if let Some(object) = entry.as_object_mut() {
                    object.remove("slugs");
                    object.remove("kinds");
                }
                entry
            })
            .collect::<Vec<_>>();
        let value = json!({
            "models": models,
            "checkedAt": now(),
            "stale": false,
            "error": failure.map(|e| e.message).unwrap_or_default(),
        });
        *self
            .latest
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(kinds);
        *cached = Some(CatalogCache {
            accounts: fingerprint,
            at: now(),
            value: value.clone(),
        });
        Ok(value)
    }

    /// The native slug for `model` on an account of `kind`, from the latest inventory.
    pub async fn resolve(
        &self,
        s: &Service,
        kind: &str,
        model: &str,
        reasoning: &str,
    ) -> Result<Option<String>> {
        if self.cached_kind(kind).is_none() {
            self.models(s).await?;
        }
        Ok(self
            .cached_kind(kind)
            .and_then(|models| slug(&models, model, reasoning)))
    }

    /// Whether an account of `kind` offers `model`, without waiting for discovery.
    pub fn offers(&self, kind: &str, model: &str) -> bool {
        self.cached_kind(kind)
            .is_some_and(|models| models.iter().any(|m| m["model"] == model))
    }

    fn cached_kind(&self, kind: &str) -> Option<Vec<Value>> {
        self.latest
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()?
            .iter()
            .find(|(k, _)| k == kind)
            .map(|(_, models)| models.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LISTING: &str = "Fetching available models...\n\
        gemini-4-neon-high\tGemini 4 Neon (High)\n\
        gemini-4-neon-xhigh\tGemini 4 Neon (XHigh)\n\
        gemini-4-neon-low\tGemini 4 Neon (Low)\n\
        gemini-3.5-flash-medium\tGemini 3.5 Flash (Medium)\n\
        gemini-3.5-flash\tGemini 3.5 Flash (High)\n\
        gemini-3.1-pro-low-thinking\tGemini 3.1 Pro (Low)\n\
        gemini-3-flash-preview\tGemini 3 Flash\n\
        gemini-pro-orchestrator\tOrchestrator\n\
        bad slug\tIgnored\n";

    #[test]
    fn listings_group_levels_under_one_model_and_keep_their_native_slugs() {
        let models = parse_models(LISTING);
        let ids = models.iter().map(|m| text(m, "model")).collect::<Vec<_>>();
        assert_eq!(
            ids,
            [
                "gemini-4-neon",
                "gemini-3.5-flash",
                "gemini-3.1-pro-low-thinking",
                "gemini-3-flash-preview",
                "gemini-pro-orchestrator"
            ]
        );
        assert_eq!(models[0]["displayName"], "Gemini 4 Neon");
        assert_eq!(models[0]["defaultReasoningEffort"], "high");
        assert_eq!(
            models[0]["supportedReasoningEfforts"]
                .as_array()
                .unwrap()
                .iter()
                .map(|e| text(e, "reasoningEffort"))
                .collect::<Vec<_>>(),
            ["low", "high", "xhigh"]
        );
        assert_eq!(
            slug(&models, "gemini-4-neon", "xhigh").unwrap(),
            "gemini-4-neon-xhigh"
        );
        assert_eq!(
            slug(&models, "gemini-4-neon", "").unwrap(),
            "gemini-4-neon-high"
        );
        assert_eq!(
            slug(&models, "gemini-3.5-flash", "high").unwrap(),
            "gemini-3.5-flash"
        );
        assert!(slug(&models, "gemini-4-neon", "medium").is_none());
        // A model with one level or none runs its only slug.
        assert_eq!(
            slug(&models, "gemini-3.1-pro-low-thinking", "").unwrap(),
            "gemini-3.1-pro-low-thinking"
        );
        assert_eq!(models[3]["supportedReasoningEfforts"], json!([]));
        assert_eq!(
            slug(&models, "gemini-3-flash-preview", "").unwrap(),
            "gemini-3-flash-preview"
        );
        assert!(slug(&models, "unknown", "").is_none());
    }

    #[test]
    fn only_stable_releases_from_the_minimum_are_supported() {
        assert!(supported_version("1.5.14\n"));
        assert!(supported_version("1.6.0"));
        assert!(!supported_version("1.5.13"));
        assert!(!supported_version("1.5.14-rc1"));
        assert!(!supported_version(""));
    }

    #[test]
    fn the_native_environment_drops_ambient_credentials_and_restores_the_shell_home() {
        let config: Config = serde_json::from_value(json!({
            "dataDir": "/data", "home": "/home/node", "workspaceRoots": [],
            "publicUrl": "http://localhost", "host": "127.0.0.1", "port": 1,
            "codexBin": "codex", "concurrency": 1, "logger": false,
            "workerEnabled": false, "runnerUrl": "",
        }))
        .unwrap();
        let home = Path::new("/runs/r/home/.antigravity");
        let env = environment(
            &config,
            home,
            Some(&json!({ "kind": GOOGLE_CLOUD, "project": "p", "location": "global" })),
        );
        assert_eq!(env["HOME"], "/runs/r/home/.antigravity");
        assert_eq!(env["AGY_CLI_DISABLE_AUTO_UPDATE"], "true");
        assert_eq!(env["AGY_ADC_AUTH"], "true");
        assert_eq!(
            env["GOOGLE_APPLICATION_CREDENTIALS"],
            "/runs/r/home/.antigravity/cairn-adc.json"
        );
        assert_eq!(
            env["BASH_ENV"],
            "/runs/r/home/.antigravity/cairn-shell-env.sh"
        );
        assert!(!env.contains_key("GEMINI_API_KEY"));
    }
}
