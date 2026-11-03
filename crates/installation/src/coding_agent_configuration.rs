//! One native document, installation inheritance, and revision-guarded editing.
//! Engine credentials and generated run permissions never enter these documents.
use crate::{
    config::id,
    error::{Error, Result, required},
    provider::Provider,
    store::Db,
    validation::text,
};
use cairn_contracts::*;
use rusqlite::{OptionalExtension, params};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

mod document;
#[cfg(test)]
mod tests;

pub(crate) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS coding_agent_configurations(
 scope TEXT NOT NULL,
 provider TEXT NOT NULL CHECK(provider IN ('codex','claude','opencode')),
 agent_id TEXT REFERENCES agents(id) ON DELETE CASCADE,
 document TEXT NOT NULL,
 presentation TEXT NOT NULL,
 removed TEXT NOT NULL CHECK(json_valid(removed) AND json_type(removed)='array'),
 revision TEXT NOT NULL,
 legacy_baseline TEXT CHECK(legacy_baseline IS NULL OR (json_valid(legacy_baseline) AND json_type(legacy_baseline)='object')),
 PRIMARY KEY(scope,provider),
 CHECK(scope=coalesce(agent_id,''))
);";

struct Record {
    value: Value,
    removed: Vec<String>,
    revision: String,
    document: String,
    presentation: String,
    legacy_baseline: Option<Value>,
}

pub(crate) fn parse(provider: Provider, document: &str) -> Result<Value> {
    if document.len() > 200_000 {
        return Err(Error::bad("Native configuration exceeds 200 KB."));
    }
    let value = match provider {
        Provider::Codex => {
            let value: toml::Value = toml::from_str(document)
                .map_err(|error| Error::bad(format!("Invalid TOML: {}", error.message())))?;
            serde_json::to_value(value)?
        }
        Provider::Claude if document.trim().is_empty() => json!({}),
        Provider::Claude => serde_json::from_str(document).map_err(|error| {
            Error::bad(format!(
                "Invalid JSON at line {}, column {}.",
                error.line(),
                error.column()
            ))
        })?,
        Provider::Opencode if document.trim().is_empty() => json!({}),
        Provider::Opencode => {
            jsonc_parser::parse_to_serde_value(document, &document::options(provider)).map_err(
                |error| {
                    Error::bad(format!(
                        "Invalid JSONC at line {}, column {}.",
                        error.line_display(),
                        error.column_display()
                    ))
                },
            )?
        }
    };
    if !value.is_object() {
        return Err(Error::bad("Native configuration must be an object."));
    }
    Ok(value)
}

pub(crate) fn render(provider: Provider, value: &Value) -> Result<String> {
    match provider {
        Provider::Codex => toml::to_string_pretty(value)
            .map_err(|_| Error::bad("This value cannot be represented in native TOML.")),
        _ => Ok(serde_json::to_string_pretty(value)? + "\n"),
    }
}

fn pointer(key: &str) -> String {
    key.replace('~', "~0").replace('/', "~1")
}

fn leaves(value: &Value, prefix: &str, paths: &mut BTreeSet<String>) {
    if let Some(object) = value.as_object()
        && !object.is_empty()
    {
        for (key, value) in object {
            leaves(value, &format!("{prefix}/{}", pointer(key)), paths);
        }
        return;
    }
    if !prefix.is_empty() {
        paths.insert(prefix.to_owned());
    }
}

fn paths(value: &Value) -> BTreeSet<String> {
    let mut paths = BTreeSet::new();
    leaves(value, "", &mut paths);
    paths
}

fn has_ancestor(paths: &BTreeSet<String>, path: &str) -> bool {
    path.match_indices('/')
        .any(|(index, _)| paths.contains(&path[..index]))
}

fn set(value: &mut Value, path: &str, replacement: Option<Value>) -> Result<()> {
    if !path.starts_with('/') {
        return Err(Error::bad("Choose a valid setting path."));
    }
    let keys: Vec<_> = path[1..]
        .split('/')
        .map(|key| key.replace("~1", "/").replace("~0", "~"))
        .collect();
    set_keys(value, &keys, replacement);
    Ok(())
}

fn set_keys(current: &mut Value, keys: &[String], replacement: Option<Value>) {
    if !current.is_object() {
        if replacement.is_none() {
            return;
        }
        *current = json!({});
    }
    let object = current.as_object_mut().unwrap();
    let key = &keys[0];
    if keys.len() == 1 {
        if let Some(value) = replacement {
            object.insert(key.clone(), value);
        } else {
            object.remove(key);
        }
        return;
    }
    if replacement.is_none() {
        if let Some(child) = object.get_mut(key) {
            let was_empty = child.as_object().is_some_and(serde_json::Map::is_empty);
            set_keys(child, &keys[1..], None);
            if !was_empty && child.as_object().is_some_and(serde_json::Map::is_empty) {
                object.remove(key);
            }
        }
        return;
    }
    let child = object.entry(key.clone()).or_insert_with(|| json!({}));
    set_keys(child, &keys[1..], replacement);
}

fn record(db: &Db<'_>, provider: Provider, agent: Option<AgentId>) -> Result<Record> {
    let scope = agent.map_or_else(String::new, |id| id.to_string());
    let row: Option<(String, String, String, String, Option<String>)> = db.0.query_row(
        "SELECT document,removed,revision,presentation,legacy_baseline FROM coding_agent_configurations WHERE scope=? AND provider=?",
        params![scope, provider.as_str()],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
    ).optional()?;
    let (document, removed, revision, presentation, legacy_baseline) = row.unwrap_or_else(|| {
        (
            if provider == Provider::Codex {
                ""
            } else {
                "{}\n"
            }
            .into(),
            "[]".into(),
            "initial".into(),
            if provider == Provider::Codex {
                ""
            } else {
                "{}\n"
            }
            .into(),
            None,
        )
    });
    Ok(Record {
        value: parse(provider, &document)?,
        removed: serde_json::from_str(&removed)?,
        revision,
        document,
        presentation,
        legacy_baseline: legacy_baseline
            .map(|value| serde_json::from_str(&value))
            .transpose()?,
    })
}

fn merge(base: &mut Value, overrides: &Value) {
    for path in paths(overrides) {
        // Every path came from the parsed object.
        set(base, &path, overrides.pointer(&path).cloned()).unwrap();
    }
}

fn mappings(provider: Provider) -> &'static [(&'static str, &'static str)] {
    match provider {
        Provider::Codex => &[
            ("/model", "model"),
            ("/model_reasoning_effort", "reasoning"),
            ("/developer_instructions", "instructions"),
            ("/service_tier", "speed"),
        ],
        Provider::Claude => &[("/model", "model"), ("/effortLevel", "reasoning")],
        Provider::Opencode => &[("/model", "model")],
    }
}

pub(crate) fn preferences(value: &Value, provider: Provider) -> CodingAgentPreferences {
    let mut result = CodingAgentPreferences::default();
    for (path, field) in mappings(provider) {
        let value = value.pointer(path).and_then(Value::as_str).unwrap_or("");
        match *field {
            "model" => result.model = value.into(),
            "reasoning" => result.reasoning = value.into(),
            "speed" => {
                result.speed = if value == crate::models::CODEX_STANDARD_TIER {
                    crate::models::STANDARD_SPEED.into()
                } else {
                    value.into()
                }
            }
            _ => {}
        }
    }
    result
}

pub(crate) fn installation_preferences(
    db: &Db<'_>,
    provider: Provider,
) -> Result<CodingAgentPreferences> {
    Ok(preferences(&record(db, provider, None)?.value, provider))
}

/// Read once for an agent list; do not issue a configuration query per profile.
pub(crate) fn attach_defaults(db: &Db<'_>, agents: &mut [Agent]) -> Result<()> {
    if agents.is_empty() {
        return Ok(());
    }
    let scope = (agents.len() == 1).then(|| agents[0].id.to_string());
    let statement = db
        .0
        .prepare_cached("SELECT scope,provider,document,removed FROM coding_agent_configurations WHERE ?1 IS NULL OR scope='' OR scope=?1");
    let mut statement = match statement {
        Ok(statement) => statement,
        // Earlier migrations read agent grants before this additive table exists.
        Err(_)
            if db
                .0
                .pragma_query_value::<i64, _>(None, "user_version", |row| row.get(0))?
                < 25 =>
        {
            return Ok(());
        }
        Err(error) => return Err(error.into()),
    };
    let records = statement
        .query_map([scope], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut documents = BTreeMap::new();
    for (scope, provider, document, removed) in records {
        let value = parse(Provider::parse(&provider)?, &document)?;
        let removed: Vec<String> = serde_json::from_str(&removed)?;
        documents.insert((scope, provider), (value, removed));
    }
    for agent in agents {
        for provider in [Provider::Codex, Provider::Claude, Provider::Opencode] {
            let name = provider.as_str().to_owned();
            let mut value = documents
                .get(&(String::new(), name.clone()))
                .map_or_else(|| json!({}), |(value, _)| value.clone());
            if let Some((own, removed)) = documents.get(&(agent.id.to_string(), name.clone())) {
                merge(&mut value, own);
                for path in removed {
                    set(&mut value, path, None)?;
                }
            }
            let defaults = preferences(&value, provider);
            if defaults != CodingAgentPreferences::default() {
                agent
                    .coding_agent_defaults
                    .get_or_insert_with(BTreeMap::new)
                    .insert(name, defaults);
            }
        }
    }
    Ok(())
}

/// Validate resolved preferences while preserving empty, inheriting profile fields.
pub(crate) fn validate_profile(db: &Db<'_>, agent: &Agent) -> Result<()> {
    let provider = Provider::from(agent.settings.provider);
    let mut value = record(db, provider, None)?.value;
    let mut own = record(db, provider, Some(agent.id))?;
    clear_changed_profile_settings(&mut own, db.agent(&agent.id.to_string())?.as_ref(), agent)?;
    merge(&mut value, &own.value);
    for path in own.removed {
        set(&mut value, &path, None)?;
    }
    let mut resolved = serde_json::to_value(agent)?;
    for (path, field) in mappings(provider) {
        if text(&resolved, field).is_empty()
            && let Some(value) = value.pointer(path)
        {
            resolved[*field] = value.clone();
        }
    }
    provider.adapter().validate_agent(&resolved)
}

/// Ordinary profile edits own the same mapped properties as the native editor.
fn clear_changed_profile_settings(
    own: &mut Record,
    previous: Option<&Agent>,
    agent: &Agent,
) -> Result<bool> {
    let provider = Provider::from(agent.settings.provider);
    let previous_settings = previous
        .map(|agent| serde_json::to_value(&agent.settings))
        .transpose()?;
    let settings = serde_json::to_value(&agent.settings)?;
    let same_provider =
        previous.is_some_and(|previous| previous.settings.provider == agent.settings.provider);
    let mut changed = false;
    for (path, field) in mappings(provider) {
        let edited = if same_provider {
            previous_settings.as_ref().unwrap()[*field] != settings[*field]
        } else {
            !text(&settings, field).is_empty()
        };
        if !edited {
            continue;
        }
        changed |=
            own.value.pointer(path).is_some() || own.removed.iter().any(|removed| removed == path);
        set(&mut own.value, path, None)?;
        own.removed.retain(|removed| removed != path);
    }
    Ok(changed)
}

pub(crate) fn synchronize_profile(db: &Db<'_>, agent: &Agent) -> Result<()> {
    let Some(previous) = db.agent(&agent.id.to_string())? else {
        return Ok(());
    };
    let provider = Provider::from(agent.settings.provider);
    capture_legacy_defaults(db, provider, Some(agent.id))?;
    let mut own = record(db, provider, Some(agent.id))?;
    if !clear_changed_profile_settings(&mut own, Some(&previous), agent)? {
        return Ok(());
    }
    db.0.execute(
        "UPDATE coding_agent_configurations SET document=?,removed=?,revision=? WHERE scope=? AND provider=?",
        params![render(provider, &own.value)?, serde_json::to_string(&own.removed)?, id(), agent.id.to_string(), provider.as_str()],
    )?;
    Ok(())
}

pub(crate) fn next_selection(
    db: &Db<'_>,
    agent_id: &str,
    run: Option<&Value>,
) -> Result<Option<CodingAgentPreferences>> {
    // Restored historical runs may have no recoverable execution assignment.
    // Display preferences must not prevent reading their history or recovery controls.
    if run.is_some_and(|run| Provider::of_run(run).is_err()) {
        return Ok(None);
    }
    let Some(agent) = db.agent_snapshot(agent_id)? else {
        return Ok(None);
    };
    let mut snapshot = json!({ "snapshot": { "agent": agent } });
    crate::chats::select_agent(&mut snapshot, run, &json!({}))?;
    freeze_message(db, &mut snapshot, run, &json!({}))?;
    let agent = &snapshot["snapshot"]["agent"];
    Ok(Some(CodingAgentPreferences {
        model: text(agent, "model").into(),
        reasoning: text(agent, "reasoning").into(),
        speed: text(agent, "speed").into(),
    }))
}

fn effective(
    db: &Db<'_>,
    provider: Provider,
    agent: Option<AgentId>,
) -> Result<(Value, Value, Record, Option<Agent>, String)> {
    let defaults = record(db, provider, None)?;
    let Some(agent_id) = agent else {
        return Ok((
            defaults.value.clone(),
            json!({}),
            defaults,
            None,
            String::new(),
        ));
    };
    let saved = required(db.agent(&agent_id.to_string())?, "Agent not found")?;
    let own = record(db, provider, agent)?;
    let mut value = defaults.value.clone();
    merge(&mut value, &own.value);
    for path in &own.removed {
        set(&mut value, path, None)?;
    }
    if Provider::from(saved.settings.provider) == provider {
        let settings = serde_json::to_value(&saved.settings)?;
        for (path, field) in mappings(provider) {
            if !text(&settings, field).is_empty() {
                let configured = if *field == "speed" {
                    json!(crate::models::service_tier(text(&settings, field)))
                } else {
                    settings[field].clone()
                };
                set(&mut value, path, Some(configured))?;
            }
        }
    }
    let source_revision = format!("{}:{}", defaults.revision, saved.revision);
    Ok((value, defaults.value, own, Some(saved), source_revision))
}

fn managed(provider: Provider) -> Vec<ManagedCodingAgentSetting> {
    let mut entries = vec![
        (
            "credentials",
            "Accounts and credentials are managed in Connections and never appear in this document.",
        ),
        (
            "permissions",
            "Cairn resource grants remain the limit. Change execution access on the agent page.",
        ),
        (
            "mcp",
            "Cairn generates authorized MCP tools and private run credentials.",
        ),
    ];
    match provider {
        Provider::Codex => {
            entries.push((
                "features.plugins",
                "Native Codex plugins are disabled; use Cairn skills and plugins.",
            ));
            entries.push((
                "notify",
                "Native command callbacks cannot run outside Cairn's permission checks. Use Cairn skills and plugins.",
            ));
            entries.push((
                "hooks",
                "Native lifecycle commands and their trust state are not supported. Use Cairn skills and plugins.",
            ));
            entries.push((
                "features.default_mode_request_user_input",
                "Cairn supplies questionnaire tools; Codex's native default-mode question tool remains disabled.",
            ));
        }
        Provider::Claude => {
            entries.push((
                "enabledPlugins",
                "Native plugin loading is not supported by the managed execution adapter.",
            ));
            entries.push((
                "hooks / command helpers",
                "Native command callbacks and authentication helpers are not supported. Use Cairn skills, plugins and Connections.",
            ));
            entries.push((
                "env",
                "Only CLAUDE_AUTOCOMPACT_PCT_OVERRIDE, CLAUDE_CODE_MAX_OUTPUT_TOKENS and MAX_THINKING_TOKENS are editable native environment preferences. Cairn controls the runtime environment.",
            ));
        }
        Provider::Opencode => {
            entries.push((
                "plugin",
                "Native OpenCode plugins are not supported by the managed execution adapter.",
            ));
            entries.push((
                "compaction.auto",
                "Automatic OpenCode compaction is not supported by Cairn session recovery.",
            ));
            entries.push((
                "autoupdate / share",
                "Cairn runs validated engine versions with native sharing disabled.",
            ));
            entries.push((
                "configuration substitutions",
                "File and environment substitutions are not supported in keys or values. Use literal values that Cairn can validate and freeze.",
            ));
            entries.push((
                "agent / mode permissions",
                "Native agent permissions and legacy tool overrides cannot replace Cairn's granted tool selection. Change access on the Cairn agent page.",
            ));
            entries.push((
                "formatter / lsp commands",
                "Custom executable commands and their environments are not supported. Use Cairn skills and plugins.",
            ));
        }
    }
    entries
        .into_iter()
        .map(|(path, reason)| ManagedCodingAgentSetting {
            path: path.into(),
            reason: reason.into(),
        })
        .collect()
}

pub(crate) fn validate(provider: Provider, value: &Value) -> Result<()> {
    if provider == Provider::Opencode && contains_interpolation(value) {
        return Err(Error::bad(
            "OpenCode file and environment substitutions are not supported. Use literal configuration values so Cairn can validate and freeze them before execution.",
        ));
    }
    if provider == Provider::Codex
        && value.as_object().is_some_and(|object| {
            object
                .keys()
                .any(|key| !key.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_'))
        })
    {
        return Err(Error::bad(
            "Native Codex configuration keys must use letters, digits and underscores.",
        ));
    }
    let mut profile = json!({
        "name": "Native configuration",
        "provider": provider,
        "access": { "mcpTools": {} },
        "exclusiveNodes": [],
        "plugins": [],
    });
    for (path, _) in mappings(provider) {
        if value.pointer(path).is_some_and(|value| !value.is_string()) {
            return Err(Error::bad(format!("{path} must be text.")));
        }
    }
    for (path, field) in mappings(provider) {
        if let Some(value) = value.pointer(path) {
            profile[*field] = if *field == "speed" && value == crate::models::CODEX_STANDARD_TIER {
                json!(crate::models::STANDARD_SPEED)
            } else {
                value.clone()
            };
        }
    }
    let mut profile: AgentSettings = serde_json::from_value(profile)?;
    profile.validate().map_err(Error::bad)?;
    crate::models::validate_speed(&profile.speed)?;
    if provider == Provider::Claude {
        crate::claude::validate_agent(&serde_json::to_value(&profile)?)?;
        if let Some(env) = value.get("env") {
            let env = env.as_object().ok_or_else(|| {
                Error::bad("Claude environment settings must be an object of text values.")
            })?;
            if env.values().any(|value| !value.is_string()) {
                return Err(Error::bad("Claude environment values must be text."));
            }
            if env.keys().any(|key| {
                !matches!(
                    key.as_str(),
                    "CLAUDE_AUTOCOMPACT_PCT_OVERRIDE"
                        | "CLAUDE_CODE_MAX_OUTPUT_TOKENS"
                        | "MAX_THINKING_TOKENS"
                )
            }) {
                return Err(Error::bad(
                    "Claude environment settings are managed by Cairn. Supported native preferences are CLAUDE_AUTOCOMPACT_PCT_OVERRIDE, CLAUDE_CODE_MAX_OUTPUT_TOKENS and MAX_THINKING_TOKENS.",
                ));
            }
        }
    }
    if provider == Provider::Opencode && !profile.model.is_empty() {
        crate::opencode::model_id(&profile.model)?;
    }
    let blocked: &[&str] = match provider {
        Provider::Codex => &[
            "/model_provider",
            "/model_providers",
            "/forced_login_method",
            "/cli_auth_credentials_store",
            "/mcp_servers",
            "/sandbox_mode",
            "/sandbox_workspace_write",
            "/default_permissions",
            "/permissions",
            "/approval_policy",
            "/shell_environment_policy",
            "/model_instructions_file",
            "/experimental_instructions_file",
            "/profile",
            "/profiles",
            "/notify",
            "/hooks",
            "/plugins",
            "/marketplaces",
        ],
        Provider::Claude => &[
            "/permissions",
            "/sandbox",
            "/apiKeyHelper",
            "/forceLoginMethod",
            "/forceLoginOrgUUID",
            "/enabledMcpjsonServers",
            "/disabledMcpjsonServers",
            "/enableAllProjectMcpServers",
            "/enabledPlugins",
            "/extraKnownMarketplaces",
            "/appendPlugins",
            "/hooks",
            "/statusLine",
            "/fileSuggestion",
            "/awsAuthRefresh",
            "/awsCredentialExport",
            "/otelHeadersHelper",
            "/disableAllHooks",
            "/attribution/commit",
            "/env/HOME",
            "/env/USERPROFILE",
            "/env/CLAUDE_CONFIG_DIR",
            "/env/XDG_CONFIG_HOME",
            "/env/XDG_DATA_HOME",
            "/env/XDG_STATE_HOME",
            "/env/XDG_CACHE_HOME",
            "/env/PATH",
        ],
        Provider::Opencode => &[
            "/provider",
            "/providers",
            "/plugins",
            "/enabled_providers",
            "/disabled_providers",
            "/mcp",
            "/server",
            "/permission",
            "/permissions",
            "/tools",
            "/small_model",
        ],
    };
    for path in blocked {
        if value.pointer(path).is_some() {
            return Err(Error::bad(format!(
                "{} is managed by Cairn. Use the account, access or tools settings instead.",
                &path[1..]
            )));
        }
    }
    if provider == Provider::Codex
        && value
            .get("agents")
            .and_then(Value::as_object)
            .is_some_and(|agents| {
                agents
                    .values()
                    .any(|role| role.get("config_file").is_some())
            })
    {
        return Err(Error::bad(
            "Codex role config_file references are not supported: external configuration files cannot be frozen or transported with the message. Use Cairn agent profiles for model, reasoning and instructions.",
        ));
    }
    for path in paths(value) {
        let lower = path.to_ascii_lowercase();
        if lower.contains("api_key")
            || lower.contains("apikey")
            || lower.contains("auth_token")
            || lower.contains("password")
            || lower.contains("/cairn_")
            || lower.contains("/codex_home")
            || lower.contains("/opencode_")
            || lower.contains("base_url")
            || lower.contains("baseurl")
            || lower.ends_with("_secret")
            || lower.ends_with("_token")
            || lower.contains("claude_code_use_")
        {
            return Err(Error::bad(format!(
                "{path} cannot contain credentials or replace Cairn's managed environment. Use Connections for credentials."
            )));
        }
    }
    if value
        .pointer("/features/plugins")
        .is_some_and(|v| v != false)
    {
        return Err(Error::bad(
            "Native Codex plugins are not supported. Use Cairn skills and plugins.",
        ));
    }
    if provider == Provider::Codex
        && value
            .pointer("/features/default_mode_request_user_input")
            .is_some_and(|value| value != false)
    {
        return Err(Error::bad(
            "features.default_mode_request_user_input is managed by Cairn and cannot be enabled. Use Cairn's questionnaire tools.",
        ));
    }
    if provider == Provider::Codex {
        for feature in ["hooks", "codex_hooks"] {
            if value
                .pointer(&format!("/features/{feature}"))
                .is_some_and(|value| value != false)
            {
                return Err(Error::bad(
                    "Native Codex hooks are not supported and cannot be enabled. Use Cairn skills and plugins.",
                ));
            }
        }
    }
    if provider == Provider::Opencode {
        for group in ["agent", "mode", "agents"] {
            if let Some(agents) = value.get(group).and_then(Value::as_object) {
                for agent in agents.values() {
                    if agent.get("permission").is_some()
                        || agent.get("permissions").is_some()
                        || agent.get("tools").is_some()
                    {
                        return Err(Error::bad(
                            "OpenCode native agent tools and permissions are managed by Cairn. Use the Cairn agent's access settings instead.",
                        ));
                    }
                }
            }
        }
        for group in ["formatter", "lsp"] {
            if let Some(callbacks) = value.get(group).and_then(Value::as_object) {
                for callback in callbacks.values() {
                    if ["command", "env", "environment"]
                        .iter()
                        .any(|key| callback.get(*key).is_some())
                    {
                        return Err(Error::bad(
                            "OpenCode formatter and language-server commands or environments are not supported by Cairn's managed execution. Use Cairn skills and plugins.",
                        ));
                    }
                }
            }
        }
        if value
            .get("compaction")
            .is_some_and(|value| !value.is_object())
        {
            return Err(Error::bad(
                "OpenCode compaction settings must be an object.",
            ));
        }
        for (path, expected) in [
            ("/autoupdate", json!(false)),
            ("/share", json!("disabled")),
            ("/plugin", json!([])),
            ("/compaction/auto", json!(false)),
            ("/compaction/prune", json!(false)),
        ] {
            if value.pointer(path).is_some_and(|v| v != &expected) {
                return Err(Error::bad(format!(
                    "{path} is not supported by Cairn's OpenCode execution and recovery. It cannot be enabled."
                )));
            }
        }
    }
    for setting in settings(provider, value, &BTreeSet::new()) {
        let Some(value) = value.pointer(&setting.path) else {
            continue;
        };
        let valid = match setting.kind.as_str() {
            "integer" => value
                .as_u64()
                .is_some_and(|n| n > 0 && n <= 9_007_199_254_740_991),
            "boolean" => value.is_boolean(),
            "select" => value
                .as_str()
                .is_some_and(|v| setting.choices.iter().any(|c| c == v)),
            _ => value.is_string(),
        };
        if !valid {
            return Err(Error::bad(format!(
                "Enter a valid value for {} ({}).",
                setting.label, setting.path
            )));
        }
    }
    Ok(())
}

/// OpenCode expands substitutions before parsing JSON, including property names.
/// Reject them before they can introduce a managed option after our validation.
fn contains_interpolation(value: &Value) -> bool {
    fn interpolated(text: &str) -> bool {
        text.contains("{env:") || text.contains("{file:")
    }
    match value {
        Value::String(text) => interpolated(text),
        Value::Array(values) => values.iter().any(contains_interpolation),
        Value::Object(values) => values
            .iter()
            .any(|(key, value)| interpolated(key) || contains_interpolation(value)),
        _ => false,
    }
}

fn settings(
    provider: Provider,
    value: &Value,
    customized: &BTreeSet<String>,
) -> Vec<CodingAgentSetting> {
    type SettingDefinition = (
        &'static str,
        &'static str,
        &'static str,
        &'static str,
        &'static str,
        &'static [&'static str],
    );
    let definitions: &[SettingDefinition] = match provider {
        Provider::Codex => &[
            (
                "/model_context_window",
                "Context window",
                "Tokens available to the model. This does not increase the model's actual capacity.",
                "Context",
                "integer",
                &[],
            ),
            (
                "/model_auto_compact_token_limit",
                "Automatic compaction threshold",
                "Compact history after this many tokens. Leave automatic to use the model default.",
                "Context",
                "integer",
                &[],
            ),
            (
                "/tool_output_token_limit",
                "Tool output limit",
                "Maximum tokens retained from a tool response.",
                "Context",
                "integer",
                &[],
            ),
            (
                "/model_verbosity",
                "Response detail",
                "Preferred detail for models that support verbosity.",
                "Responses",
                "select",
                &["low", "medium", "high"],
            ),
            (
                "/web_search",
                "Web search",
                "Use live results, cached results, or disable native search.",
                "Tools",
                "select",
                &["disabled", "cached", "live"],
            ),
            (
                "/features/multi_agent",
                "Native subagents",
                "Let Codex use its native subagents within the execution's permissions.",
                "Tools",
                "boolean",
                &[],
            ),
        ],
        Provider::Claude => &[
            (
                "/alwaysThinkingEnabled",
                "Extended thinking",
                "Enable extended thinking by default.",
                "Responses",
                "boolean",
                &[],
            ),
            (
                "/language",
                "Response language",
                "Preferred language for Claude's responses.",
                "Responses",
                "text",
                &[],
            ),
            (
                "/outputStyle",
                "Output style",
                "Name of an output style available in the execution environment.",
                "Responses",
                "text",
                &[],
            ),
            (
                "/env/CLAUDE_AUTOCOMPACT_PCT_OVERRIDE",
                "Automatic compaction percentage",
                "Native percentage override, written as a string; for example 80.",
                "Context",
                "text",
                &[],
            ),
            (
                "/env/CLAUDE_CODE_MAX_OUTPUT_TOKENS",
                "Maximum response tokens",
                "Native output budget, written as a string.",
                "Context",
                "text",
                &[],
            ),
        ],
        Provider::Opencode => &[
            (
                "/snapshot",
                "Workspace snapshots",
                "Enable OpenCode's native workspace snapshots.",
                "Workspace",
                "boolean",
                &[],
            ),
            (
                "/logLevel",
                "Native log level",
                "Verbosity of the native engine logs.",
                "Diagnostics",
                "select",
                &["DEBUG", "INFO", "WARN", "ERROR"],
            ),
            (
                "/default_agent",
                "Native agent",
                "Name of an OpenCode agent available in the execution.",
                "Responses",
                "text",
                &[],
            ),
        ],
    };
    let mut output: Vec<_> = definitions
        .iter()
        .map(
            |(path, label, description, group, kind, choices)| CodingAgentSetting {
                path: (*path).into(),
                label: (*label).into(),
                description: (*description).into(),
                group: (*group).into(),
                kind: (*kind).into(),
                choices: choices.iter().map(|v| (*v).into()).collect(),
                value: value.pointer(path).map(Value::to_string),
                inherited: !customized.contains(*path),
            },
        )
        .collect();
    for (path, field) in mappings(provider) {
        let label = match *field {
            "model" => "Model",
            "reasoning" => "Reasoning effort",
            "instructions" => "Additional instructions",
            "speed" => "Speed",
            _ => unreachable!(),
        };
        output.push(CodingAgentSetting {
            path: (*path).into(), label: label.into(), description: "Shared with the corresponding Cairn agent setting. Editing either updates the same value.".into(), group: "Agent defaults".into(), kind: "text".into(), choices: vec![], value: value.pointer(path).map(Value::to_string), inherited: !customized.contains(*path),
        });
    }
    output
}

pub(crate) fn view(
    db: &Db<'_>,
    input: CodingAgentConfigurationInput,
) -> Result<CodingAgentConfiguration> {
    let provider = Provider::from(input.provider);
    let (value, inherited, record, agent, sources) = effective(db, provider, input.agent_id)?;
    let mut customized = paths(&record.value);
    customized.extend(record.removed);
    if let Some(agent) = &agent
        && Provider::from(agent.settings.provider) == provider
    {
        let settings = serde_json::to_value(&agent.settings)?;
        for (path, field) in mappings(provider) {
            if !text(&settings, field).is_empty() {
                customized.insert((*path).into());
            }
        }
    }
    let document = if input.agent_id.is_none() {
        record.document
    } else {
        document::rewrite(provider, &record.presentation, &value)?
    };
    Ok(CodingAgentConfiguration {
        provider: input.provider, agent_id: input.agent_id, agent_name: agent.map(|a| a.settings.name),
        revision: format!("{}:{sources}", record.revision),
        filename: match provider { Provider::Codex => "config.toml", Provider::Claude => "settings.json", Provider::Opencode => "opencode.jsonc" }.into(),
        document, inherited_document: render(provider, &inherited)?, customized_paths: customized.iter().cloned().collect(),
        settings: settings(provider, &value, &customized), managed: managed(provider),
        notices: vec!["Changes apply to the next message, including existing conversations. An active turn keeps its current configuration.".into(), "The native process may restart between messages to load the new settings. The saved conversation and workspace are preserved.".into()],
    })
}

pub(crate) fn edit(
    db: &Db<'_>,
    input: &EditCodingAgentConfigurationInput,
    save: bool,
) -> Result<CodingAgentConfiguration> {
    if input.changes.len() > 200
        || input.inherit_paths.len() > 1000
        || input.changes.iter().any(|change| change.path.len() > 500)
        || input.inherit_paths.iter().any(|path| path.len() > 500)
    {
        return Err(Error::bad("Too many settings or an invalid setting path."));
    }
    let provider = Provider::from(input.provider);
    let target = CodingAgentConfigurationInput {
        provider: input.provider,
        agent_id: input.agent_id,
    };
    let current = view(db, target)?;
    if current.revision != input.revision {
        return Err(Error::conflict(
            "Configuration or inherited settings changed. Reload to review the latest values before saving.",
        ));
    }
    let (before, inherited, own, agent, _) = effective(db, provider, input.agent_id)?;
    let mapped_profile = agent
        .as_ref()
        .is_some_and(|agent| Provider::from(agent.settings.provider) == provider);
    let mut value = if input.reset {
        inherited.clone()
    } else {
        parse(provider, &input.document)?
    };
    for change in &input.changes {
        let replacement: Value = serde_json::from_str(&change.value)
            .map_err(|_| Error::bad("Enter a valid JSON setting value."))?;
        let replacement = if replacement.is_null() {
            inherited.pointer(&change.path).cloned()
        } else {
            Some(replacement)
        };
        set(&mut value, &change.path, replacement)?;
    }
    validate(provider, &value)?;
    let mut inherited_paths: BTreeSet<String> = input
        .inherit_paths
        .iter()
        .filter(|path| value.pointer(path) == inherited.pointer(path))
        .cloned()
        .collect();
    for change in &input.changes {
        if change.value.trim() == "null" {
            inherited_paths.insert(change.path.clone());
        } else {
            inherited_paths.remove(&change.path);
        }
    }
    let mut overrides = json!({});
    let mut removed = BTreeSet::new();
    if input.agent_id.is_some() {
        // Derive the sparse document from the desired effective value. This
        // handles whole-object and partial restorations with the same semantics.
        let mut differences = BTreeSet::new();
        document::changed(Some(&inherited), Some(&value), "", &mut differences);
        for path in differences {
            if let Some(replacement) = value.pointer(&path) {
                set(&mut overrides, &path, Some(replacement.clone()))?;
            } else {
                removed.insert(path);
            }
        }
        // An explicit empty object replaces that inherited object. Deleting its
        // final child must keep this container in the native document.
        for path in paths(&value) {
            let desired = value.pointer(&path).unwrap();
            if desired.as_object().is_some_and(serde_json::Map::is_empty)
                && Some(desired) != inherited.pointer(&path)
            {
                set(&mut overrides, &path, Some(desired.clone()))?;
            }
        }
        let inherits = |path: &str| {
            input.reset || inherited_paths.contains(path) || has_ancestor(&inherited_paths, path)
        };
        // Existing equal-valued customizations remain pinned until explicitly
        // reset; deletions remain meaningful if an installation default vanished.
        for path in paths(&own.value) {
            if !inherits(&path)
                && before.pointer(&path) == value.pointer(&path)
                && value.pointer(&path) == own.value.pointer(&path)
            {
                set(&mut overrides, &path, own.value.pointer(&path).cloned())?;
            }
        }
        for path in own.removed {
            if !inherits(&path) && before.pointer(&path).is_none() && value.pointer(&path).is_none()
            {
                removed.insert(path);
            }
        }
        let empty_objects: BTreeSet<_> = paths(&overrides)
            .into_iter()
            .filter(|path| {
                overrides
                    .pointer(path)
                    .and_then(Value::as_object)
                    .is_some_and(serde_json::Map::is_empty)
            })
            .collect();
        let all_removed = removed.clone();
        removed.retain(|path| {
            !has_ancestor(&all_removed, path)
                && !empty_objects.contains(path)
                && !has_ancestor(&empty_objects, path)
        });
    } else {
        overrides = value.clone();
    }
    let document = if input.reset && parse(provider, &input.document).is_err() {
        render(provider, &value)?
    } else {
        document::rewrite(provider, &input.document, &value)?
    };
    let mut output = current;
    output.document = document.clone();
    let mut customized = paths(&overrides);
    customized.extend(removed.iter().cloned());
    for (path, _) in mappings(provider) {
        if before.pointer(path) == value.pointer(path)
            && output.customized_paths.iter().any(|custom| custom == path)
            && !input.reset
            && !inherited_paths.contains(*path)
        {
            customized.insert((*path).into());
        }
    }
    output.settings = settings(provider, &value, &customized);
    output.customized_paths = customized.into_iter().collect();
    let mut changed_agent = None;
    if let Some(mut agent) = agent
        && Provider::from(agent.settings.provider) == provider
    {
        let before_settings = agent.settings.clone();
        for (path, field) in mappings(provider) {
            if before.pointer(path) == value.pointer(path)
                && !input.reset
                && overrides.pointer(path).is_none()
                && !inherited_paths.contains(*path)
            {
                continue;
            }
            let selected = if value.pointer(path) == inherited.pointer(path) {
                ""
            } else {
                value
                    .pointer(path)
                    .and_then(Value::as_str)
                    .unwrap_or_default()
            };
            let selected = if *field == "speed" && selected == crate::models::CODEX_STANDARD_TIER {
                crate::models::STANDARD_SPEED
            } else {
                selected
            }
            .to_owned();
            match *field {
                "model" => agent.settings.model = selected,
                "reasoning" => agent.settings.reasoning = selected,
                "instructions" => agent.settings.instructions = selected,
                "speed" => agent.settings.speed = selected,
                _ => unreachable!(),
            }
        }
        agent.settings.validate().map_err(Error::bad)?;
        crate::models::validate_speed(&agent.settings.speed)?;
        let mut resolved = serde_json::to_value(&agent)?;
        for (path, field) in mappings(provider) {
            if let Some(value) = value.pointer(path) {
                resolved[*field] = value.clone();
            }
        }
        provider.adapter().validate_agent(&resolved)?;
        if agent.settings != before_settings {
            changed_agent = Some(agent);
        }
    }
    if !save {
        return Ok(output);
    }
    capture_legacy_defaults(db, provider, input.agent_id)?;
    if let Some(agent) = changed_agent {
        crate::agent_grants::save_profile(db, agent, false, &[])?;
    }
    let scope = input.agent_id.map(|id| id.to_string());
    if mapped_profile {
        for (path, _) in mappings(provider) {
            set(&mut overrides, path, None)?;
        }
    }
    let stored_document = if scope.is_some() {
        render(provider, &overrides)?
    } else {
        document.clone()
    };
    db.0.execute(
        "INSERT INTO coding_agent_configurations(scope,provider,agent_id,document,presentation,removed,revision) VALUES(?1,?2,?3,?4,?5,?6,?7) ON CONFLICT(scope,provider) DO UPDATE SET document=excluded.document,presentation=excluded.presentation,removed=excluded.removed,revision=excluded.revision",
        params![scope.as_deref().unwrap_or(""), provider.as_str(), scope, stored_document, document, serde_json::to_string(&removed)?, id()],
    )?;
    db.audit(
        "coding-agent.configuration.saved",
        &json!({ "provider": provider, "agentId": input.agent_id }),
    )?;
    view(db, target)
}

fn native_paths(db: &Db<'_>, provider: Provider, agent_id: AgentId) -> Result<BTreeSet<String>> {
    let defaults = record(db, provider, None)?;
    let own = record(db, provider, Some(agent_id))?;
    let mut configured = paths(&defaults.value);
    configured.extend(paths(&own.value));
    configured.extend(own.removed);
    Ok(configured)
}

/// Preserve only selection defaults, without duplicating instructions or run data.
/// Historical chats have no native snapshot to compare against on their first turn.
fn capture_legacy_defaults(db: &Db<'_>, provider: Provider, agent: Option<AgentId>) -> Result<()> {
    let agents = if let Some(agent) = agent {
        vec![agent.to_string()]
    } else {
        db.0.prepare("SELECT agents.id FROM agents
            LEFT JOIN coding_agent_configurations AS config ON config.scope=agents.id AND config.provider=?
            WHERE config.legacy_baseline IS NULL")?
            .query_map([provider.as_str()], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?
    };
    for agent in agents {
        let agent_id =
            AgentId(uuid::Uuid::parse_str(&agent).map_err(|_| Error::bad("Invalid agent ID."))?);
        let (value, _, own, _, _) = effective(db, provider, Some(agent_id))?;
        if own.legacy_baseline.is_some() {
            continue;
        }
        let mut baseline = json!({});
        for (path, field) in mappings(provider) {
            if *field != "instructions"
                && let Some(value) = value.pointer(path)
            {
                set(&mut baseline, path, Some(value.clone()))?;
            }
        }
        db.0.execute(
            "INSERT INTO coding_agent_configurations(scope,provider,agent_id,document,presentation,removed,revision,legacy_baseline)
             VALUES(?1,?2,?1,?3,?4,?5,?6,?7)
             ON CONFLICT(scope,provider) DO UPDATE SET legacy_baseline=excluded.legacy_baseline
             WHERE coding_agent_configurations.legacy_baseline IS NULL",
            params![agent, provider.as_str(), own.document, own.presentation, serde_json::to_string(&own.removed)?, own.revision, baseline.to_string()],
        )?;
    }
    Ok(())
}

pub(crate) fn snapshot(
    db: &Db<'_>,
    provider: Provider,
    agent_id: &str,
) -> Result<NativeConfigurationSnapshot> {
    let agent_id =
        AgentId(uuid::Uuid::parse_str(agent_id).map_err(|_| Error::bad("Invalid agent ID."))?);
    let result = view(
        db,
        CodingAgentConfigurationInput {
            provider: provider.into(),
            agent_id: Some(agent_id),
        },
    )?;
    let mut configured_paths = native_paths(db, provider, agent_id)?;
    configured_paths.extend(result.customized_paths);
    Ok(NativeConfigurationSnapshot {
        agent_id: Some(agent_id),
        provider: result.provider,
        document: result.document,
        revision: result.revision,
        configured_paths: configured_paths.into_iter().collect(),
    })
}

pub(crate) fn freeze_message(
    db: &Db<'_>,
    snapshot: &mut Value,
    previous: Option<&Value>,
    message: &Value,
) -> Result<()> {
    let provider = Provider::of_run(snapshot)?;
    let agent_id = text(&snapshot["snapshot"]["agent"], "id");
    let frozen = self::snapshot(db, provider, agent_id)?;
    let native = parse(provider, &frozen.document)?;
    let previous_frozen = previous
        .map(|run| &run["snapshot"]["nativeConfiguration"])
        .filter(|configuration| configuration["provider"] == provider.as_str());
    let transferred = previous_frozen
        .and_then(|configuration| configuration["agentId"].as_str())
        .is_some_and(|owner| owner != agent_id);
    let legacy = previous.is_some_and(|run| {
        Provider::of_run(run).ok() == Some(provider)
            && !run["snapshot"]["nativeConfiguration"].is_object()
    });
    let legacy_baseline = if legacy {
        record(db, provider, frozen.agent_id)?.legacy_baseline
    } else {
        None
    };
    let old = previous
        .filter(|run| run["snapshot"]["nativeConfiguration"]["provider"] == provider.as_str())
        .and_then(|run| run["snapshot"]["nativeConfiguration"]["document"].as_str())
        .map(|document| parse(provider, document))
        .transpose()?
        .or(legacy_baseline.clone())
        .unwrap_or_else(|| json!({}));
    let previous_paths = previous
        .filter(|run| run["snapshot"]["nativeConfiguration"]["provider"] == provider.as_str())
        .and_then(|run| run["snapshot"]["nativeConfiguration"]["configuredPaths"].as_array());
    let configured = |path: &str| {
        if legacy {
            return legacy_baseline.is_some() && native.pointer(path) != old.pointer(path);
        }
        frozen
            .configured_paths
            .iter()
            .any(|configured| configured == path)
            || previous_paths.is_some_and(|paths| paths.iter().any(|configured| configured == path))
    };
    let model_changed = !transferred
        && text(message, "model").is_empty()
        && native.get("model") != old.get("model")
        && configured("/model");
    for (path, field) in mappings(provider) {
        if !text(message, field).is_empty()
            || *field == "reasoning" && !text(message, "model").is_empty()
            || transferred && matches!(*field, "model" | "reasoning" | "speed")
        {
            continue;
        }
        let reset_reasoning = *field == "reasoning" && model_changed;
        if (previous.is_none() || configured(path) || reset_reasoning)
            && (reset_reasoning
                || *field == "instructions"
                || previous.is_none()
                || native.pointer(path) != old.pointer(path))
        {
            let value = native.pointer(path).cloned().unwrap_or_else(|| json!(""));
            snapshot["snapshot"]["agent"][*field] =
                if *field == "speed" && value == crate::models::CODEX_STANDARD_TIER {
                    json!(crate::models::STANDARD_SPEED)
                } else {
                    value.clone()
                };
        }
    }
    snapshot["snapshot"]["nativeConfiguration"] = serde_json::to_value(frozen)?;
    Ok(())
}

/// Merge native preferences before the lease's generated tools and permissions.
pub(crate) fn apply(plan: &mut Value, frozen: &Value) -> Result<()> {
    let Some(document) = frozen["document"].as_str() else {
        return Ok(());
    };
    let provider = Provider::parse(text(plan, "provider"))?;
    if frozen["provider"] != provider.as_str() {
        return Err(Error::bad(
            "Saved native configuration does not match the selected coding agent.",
        ));
    }
    let mut native = parse(provider, document)?;
    validate(provider, &native)?;
    for (path, _) in mappings(provider) {
        set(&mut native, path, None)?;
    }
    plan["nativeSettings"] = native.clone();
    plan["nativeConfigurationRevision"] = frozen["revision"].clone();
    let key = match provider {
        Provider::Codex => "codexConfig",
        Provider::Claude => "claudeSettings",
        Provider::Opencode => "opencodeConfig",
    };
    let managed = plan[key].clone();
    if managed.is_object() {
        merge(&mut native, &managed);
    }
    plan[key] = native;
    Ok(())
}

pub(crate) fn codex_arguments(plan: &Value) -> Result<Vec<String>> {
    let mut args = Vec::new();
    let Some(native) = plan["nativeSettings"].as_object() else {
        return Ok(args);
    };
    validate(Provider::Codex, &plan["nativeSettings"])?;
    for (key, value) in native {
        if !key.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_') {
            return Err(Error::bad(
                "Native Codex configuration keys must use letters, digits and underscores.",
            ));
        }
        let value = toml::Value::try_from(value)
            .map_err(|_| Error::bad("Unsupported native Codex value."))?;
        args.extend(["-c".into(), format!("{key}={value}")]);
    }
    Ok(args)
}
