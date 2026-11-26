//! Google's Antigravity CLI (`agy`) through its headless stream-json mode, as of 1.5.14.
//! Native state follows `HOME`, so `agy` alone runs with the run's private state as
//! HOME; shell commands it starts get the shell's HOME back through `BASH_ENV`.
pub mod hook;
mod side;

pub(crate) use side::SIDE_QUESTIONS;
pub(crate) use turn::PLAN_HOME;

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
use std::{
    path::Path,
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};
use tokio::sync::Mutex;

/// The oldest release whose stream-json protocol and hooks this adapter relies on.
/// Each image build installs the release Google's manifests name (scripts/release/cli-updates.mjs).
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

/// Removes the credential files of the native `home`, with the temporary files of
/// writes a cancellation or a failure interrupted before their rename.
pub(crate) fn remove_credentials(home: &Path) {
    let token = home.join(TOKEN_FILE);
    for file in [home.join(ADC_FILE), token.clone()] {
        let _ = std::fs::remove_file(file);
    }
    for directory in [home, token.parent().unwrap()] {
        let Ok(entries) = std::fs::read_dir(directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with('.') && name.ends_with(".tmp") {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
}

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

pub(crate) const CLI_UNAVAILABLE: &str = "The Antigravity CLI is not installed on this execution node. On a native node, rerun the native installer to update its supervisor, then restart cairn-native-node.";
/// Identifies a version probe failure before the adapter can submit input.
pub const CLI_UNAVAILABLE_CODE: &str = "cairn_antigravity_cli_unavailable";

pub(crate) async fn check_version(config: &Config, env: &Environment) -> Result<()> {
    let output = bounded_output(
        command(&config.antigravity_bin, &["--version".into()], env, None),
        Duration::from_secs(30),
        4096,
    )
    .await
    .map_err(|_| Error::coded(503, CLI_UNAVAILABLE_CODE, CLI_UNAVAILABLE))?;
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

/// A slug without its level segment, wherever it is: `gemini-4-neon-high` and
/// `gemini-3.1-pro-low-thinking` become `gemini-4-neon` and `gemini-3.1-pro-thinking`.
fn without_level(slug: &str, level: &str) -> Option<String> {
    let segment = format!("-{level}");
    let (start, _) = slug.rmatch_indices(&segment).find(|(start, _)| {
        let end = start + segment.len();
        end == slug.len() || slug[end..].starts_with('-')
    })?;
    Some(format!(
        "{}{}",
        &slug[..start],
        &slug[start + segment.len()..]
    ))
}

/// Repository configuration `agy` loads from the working directory or any parent
/// (the git root): its hooks run commands and its MCP servers start commands, even
/// when Cairn's hook refuses the tool (measured on 1.5.14). No setting disables it.
const REPOSITORY_COMMANDS: &[&str] = &[".agents/hooks.json", ".agents/mcp_config.json"];

/// The repository file that would run commands during a turn that must have no
/// effect, if any.
pub(crate) fn repository_commands(cwd: &Path) -> Option<std::path::PathBuf> {
    cwd.ancestors()
        .flat_map(|directory| {
            REPOSITORY_COMMANDS
                .iter()
                .map(move |file| directory.join(file))
        })
        .find(|path| path.exists())
}

pub(crate) fn repository_commands_refusal(path: &Path) -> Error {
    Error::conflict(format!(
        "{} defines commands that Antigravity runs even when Cairn refuses a tool, so this plan turn or side question cannot be bounded. Remove or rename that file, or ask in an ordinary turn.",
        path.display()
    ))
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
            // The family's identifier does not depend on how many levels an account
            // kind lists: `gemini-4-neon-high` alone is still `gemini-4-neon`.
            let stripped = levels
                .iter()
                .find_map(|(level, slug)| without_level(slug, level.as_deref()?));
            let id = match stripped {
                Some(base) if leveled => base,
                _ if leveled && levels.len() > 1 => common_prefix(&slugs),
                _ => slugs[0].to_owned(),
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
    generation: AtomicU64,
}

struct CatalogCache {
    generation: u64,
    accounts: Vec<(String, Option<String>)>,
    at: i64,
    /// How long the listing stays current: briefly when an account kind failed, so
    /// its accounts are not refused for long.
    lifetime: i64,
    value: Value,
}

fn disconnected_catalog() -> Value {
    json!({
        "models": [],
        "checkedAt": null,
        "stale": false,
        "error": "Connect an Antigravity account (Gemini API key, Google Cloud credentials or Google account) in Connections to choose a model.",
    })
}

impl Catalog {
    pub(crate) fn invalidate(&self) {
        self.generation.fetch_add(1, Ordering::AcqRel);
    }

    async fn list(s: &Service, credentials: &Value) -> Result<Vec<Value>> {
        // A private directory per listing: credential files never stay on the manager.
        let home = s
            .config
            .data_dir
            .join("antigravity-catalog")
            .join(crate::config::id());
        let listed = Self::list_in(s, &home, credentials).await;
        let _ = crate::accounts::remove_directory(&home).await;
        listed
    }

    async fn list_in(s: &Service, home: &Path, credentials: &Value) -> Result<Vec<Value>> {
        let home = home.to_owned();
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
        self.inventory(s, None).await
    }

    /// Lists the models of the same accounts again, unless a listing is still current
    /// or, with `since`, finished after that time: launches waiting together for a
    /// relisting share one.
    async fn inventory(&self, s: &Service, since: Option<i64>) -> Result<Value> {
        let generation = self.generation.load(Ordering::Acquire);
        let mut accounts = s
            .accounts
            .of(s, Provider::Antigravity)
            .await?
            .into_iter()
            .filter(crate::accounts::Account::is_active)
            .collect::<Vec<_>>();
        accounts.sort_by(|a, b| a.priority().cmp(&b.priority()).then(a.id.cmp(&b.id)));
        if accounts.is_empty() {
            return Ok(disconnected_catalog());
        }
        let fingerprint = accounts
            .iter()
            .map(|a| (a.id.clone(), a.identity.clone()))
            .collect::<Vec<_>>();
        let mut cached = self.cached.lock().await;
        if let Some(cache) = &*cached
            && cache.generation == generation
            && cache.accounts == fingerprint
            && since.map_or(now() - cache.at < cache.lifetime, |since| cache.at >= since)
        {
            return Ok(cache.value.clone());
        }
        let mut kinds = Vec::<(String, Vec<Value>)>::new();
        let mut failure = None;
        let mut reconnect = None;
        for account in &accounts {
            let kind = account.model_provider.clone().unwrap_or_default();
            if kinds.iter().any(|(k, _)| *k == kind) {
                continue;
            }
            // A revoked Google sign-in must not hide the models of healthy accounts.
            let credentials =
                match crate::accounts::antigravity::catalog_credentials(s, &account.id).await {
                    Ok(Some(credentials)) => credentials,
                    Ok(None) => continue,
                    Err(error)
                        if error.code.as_deref()
                            == Some(crate::accounts::antigravity::RECONNECT_REQUIRED) =>
                    {
                        reconnect = Some(error);
                        continue;
                    }
                    Err(error) => {
                        failure = Some(error);
                        continue;
                    }
                };
            match Self::list(s, &credentials).await {
                Ok(models) => kinds.push((kind, models)),
                Err(error) => failure = Some(error),
            }
        }
        if kinds.is_empty() {
            return match failure {
                Some(error) => Err(error),
                None => Ok(disconnected_catalog()),
            };
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
        let problem = failure.or(reconnect);
        let complete = problem.is_none();
        let value = json!({
            "models": models,
            "checkedAt": now(),
            "stale": false,
            "error": problem.map(|e| e.message).unwrap_or_default(),
        });
        *self
            .latest
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(kinds);
        *cached = Some(CatalogCache {
            generation,
            accounts: fingerprint,
            at: now(),
            lifetime: if complete { 300_000 } else { 30_000 },
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
        Ok(slug(&self.kind(s, kind).await?, model, reasoning))
    }

    /// The model an account of `kind` runs for an agent without one: its kind's first.
    pub async fn default_model(&self, s: &Service, kind: &str) -> Result<Option<String>> {
        Ok(self
            .kind(s, kind)
            .await?
            .first()
            .map(|m| text(m, "model").to_owned()))
    }

    /// Whether an account of `kind` offers `model`, without waiting for discovery.
    pub fn offers(&self, kind: &str, model: &str) -> bool {
        self.cached_kind(kind)
            .is_some_and(|models| models.iter().any(|m| m["model"] == model))
    }

    /// The models of `kind`, listed again when the latest listing lacks them: a kind
    /// whose listing failed briefly must not keep its accounts' turns from launching.
    async fn kind(&self, s: &Service, kind: &str) -> Result<Vec<Value>> {
        let started = now();
        if let Some(models) = self.cached_kind(kind) {
            return Ok(models);
        }
        self.inventory(s, Some(started)).await?;
        self.cached_kind(kind).ok_or_else(|| {
            Error::unavailable(
                "Antigravity could not list this account's models. Try again shortly.",
            )
        })
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

    #[test]
    fn credentials_and_interrupted_writes_are_removed() {
        let root = tempfile::TempDir::new().unwrap();
        let home = root.path();
        let token = home.join(TOKEN_FILE);
        std::fs::create_dir_all(token.parent().unwrap()).unwrap();
        for file in [
            home.join(ADC_FILE),
            token.clone(),
            home.join(".interrupted.tmp"),
            token.with_file_name(".interrupted.tmp"),
            home.join(SHELL_ENV),
        ] {
            std::fs::write(file, "secret").unwrap();
        }
        remove_credentials(home);
        let left = |directory: &Path| {
            std::fs::read_dir(directory)
                .unwrap()
                .filter_map(|e| e.ok()?.path().is_file().then_some(()))
                .count()
        };
        assert_eq!(left(home), 1, "only the shell script stays");
        assert_eq!(left(token.parent().unwrap()), 0);
    }

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
                "gemini-3.1-pro-thinking",
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
            slug(&models, "gemini-3.1-pro-thinking", "").unwrap(),
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
    fn a_family_keeps_its_identifier_whatever_levels_an_account_kind_lists() {
        let one = parse_models("gemini-4-neon-high\tGemini 4 Neon (High)\n");
        assert_eq!(one[0]["model"], "gemini-4-neon");
        assert_eq!(
            slug(&one, "gemini-4-neon", "high").unwrap(),
            "gemini-4-neon-high"
        );
        let irregular = parse_models("gemini-3.5-flash\tGemini 3.5 Flash (High)\n");
        assert_eq!(irregular[0]["model"], "gemini-3.5-flash");
        // A level in the middle of the slug is removed too, with one level or two.
        let low = parse_models("gemini-3.1-pro-low-thinking\tGemini 3.1 Pro (Low)\n");
        let both = parse_models(
            "gemini-3.1-pro-low-thinking\tGemini 3.1 Pro (Low)\ngemini-3.1-pro-high-thinking\tGemini 3.1 Pro (High)\n",
        );
        assert_eq!(low[0]["model"], "gemini-3.1-pro-thinking");
        assert_eq!(both[0]["model"], "gemini-3.1-pro-thinking");
        assert_eq!(
            slug(&both, "gemini-3.1-pro-thinking", "high").unwrap(),
            "gemini-3.1-pro-high-thinking"
        );
        assert!(without_level("gemini-4-neon-higher", "high").is_none());
    }

    #[test]
    fn repository_hooks_and_mcp_servers_are_found_in_parents() {
        let root = tempfile::TempDir::new().unwrap();
        let nested = root.path().join("crate/src");
        std::fs::create_dir_all(&nested).unwrap();
        assert!(repository_commands(&nested).is_none());
        std::fs::create_dir_all(root.path().join(".agents")).unwrap();
        std::fs::write(root.path().join(".agents/mcp_config.json"), "{}").unwrap();
        assert_eq!(
            repository_commands(&nested).unwrap(),
            root.path().join(".agents/mcp_config.json")
        );
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
