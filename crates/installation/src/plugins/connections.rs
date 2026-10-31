//! Configured plugin connections: an owner delegation (agent, project, people, quota)
//! plus plugin settings declared by the manifest. Identical for every plugin.
use super::{forms, registry};
use crate::{
    config::{id, now},
    error::{Error, Result},
    service::Service,
    store::Db,
    validation::{text, uuid},
};
use cairn_plugin_api::Manifest;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

pub use cairn_contracts::{PluginAccessMode as AccessMode, PluginLabel as Label};

fn per_person() -> u32 {
    5
}

/// Approval snapshot at the boundary of the still-unconverted native run domain.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NativeAuthority {
    pub native_principals: Vec<String>,
    pub native_authority_nodes: Vec<String>,
    pub native_provenance_unknown: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub native_parent_run_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub native_parent_access: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub native_plugin_grants: Option<Vec<NativePluginGrant>>,
    pub plugin_owner_account_id: String,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NativePluginGrant {
    pub connection_id: String,
    pub revision: String,
    pub author_id: String,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Connection {
    pub id: String,
    pub plugin_id: String,
    pub name: String,
    pub enabled: bool,
    pub revision: String,
    /// Plugin-specific values validated against the manifest's settings schema.
    pub settings: Value,
    /// Empty when the plugin cannot start conversations.
    pub agent_id: String,
    pub project_id: Option<String>,
    pub granted_access: Option<cairn_contracts::AccessPolicy>,
    /// Native authority committed when the owner selects the answering agent.
    /// Older/unscoped connections carry no native grant until saved by an authorized caller.
    #[serde(default)]
    pub native_authority: Option<NativeAuthority>,
    pub owner_access_id: Option<String>,
    pub access_mode: AccessMode,
    pub user_ids: Vec<String>,
    pub people: Vec<Label>,
    pub max_messages_per_hour: u32,
    /// Hourly limit for each person when anyone can use the connection.
    pub max_per_person_per_hour: u32,
    /// Agents the owner granted for the package's commands and tools.
    pub agent_ids: Vec<String>,
    /// Names of the secret fields saved in the vault (values never leave the host).
    pub secret_keys: Vec<String>,
    pub updated_at: i64,
}

impl Connection {
    /// An instance being edited, before it is saved: only its plugin, settings and the
    /// names of its secrets.
    pub fn draft(id: &str, plugin_id: &str, settings: Value, secret_keys: Vec<String>) -> Self {
        Self {
            id: id.to_owned(),
            plugin_id: plugin_id.to_owned(),
            name: String::new(),
            enabled: false,
            revision: String::new(),
            settings,
            agent_id: String::new(),
            project_id: None,
            granted_access: None,
            native_authority: None,
            owner_access_id: None,
            access_mode: AccessMode::Allowlist,
            user_ids: Vec::new(),
            people: Vec::new(),
            max_messages_per_hour: 0,
            max_per_person_per_hour: per_person(),
            agent_ids: Vec::new(),
            secret_keys,
            updated_at: now(),
        }
    }

    /// Whether this person may use the connection right now.
    pub fn allows(&self, person: &str) -> bool {
        self.access_mode == AccessMode::Everyone || self.user_ids.iter().any(|id| id == person)
    }

    pub fn grants(&self, agent_id: &str) -> bool {
        self.enabled && self.agent_ids.iter().any(|id| id == agent_id)
    }
}

pub fn label(value: &str) -> String {
    value
        .chars()
        .filter(|c| !c.is_control())
        .take(100)
        .collect::<String>()
        .trim()
        .to_owned()
}

/// External identities (Discord snowflakes, Slack IDs, …) are short opaque tokens.
pub fn external_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-' | b'.' | b':'))
}

pub fn credential_key(id: &str) -> String {
    format!("plugin-credential:{id}")
}

/// External delegation stays bound to the owner who set it up. Instances that
/// delegate nothing (no agent answering people) have no owner binding to check.
pub(crate) async fn ensure_owner(s: &Service, connection: &Connection) -> Result<()> {
    if connection.agent_id.is_empty() {
        return Ok(());
    }
    let current = crate::relay::task_author_policy(s).await?;
    let access_id = current
        .as_ref()
        .map(|policy| policy.owner.access_id.as_str());
    if access_id != connection.owner_access_id.as_deref() {
        return Err(Error::forbidden(
            "Plugin delegation no longer belongs to this installation owner.",
        ));
    }
    Ok(())
}

/// Rechecked before every effect: still enabled, same revision, same agent grants.
pub(crate) fn authorize(db: &Db<'_>, expected: &Connection) -> Result<Connection> {
    let denied =
        || Error::forbidden("Plugin connection or agent grant was revoked. Review the connection.");
    let current = db.plugin_instance(&expected.id)?.ok_or_else(denied)?;
    if !current.enabled || current.revision != expected.revision {
        return Err(denied());
    }
    if !current.agent_id.is_empty() {
        let agent = db.agent(&current.agent_id)?.ok_or_else(denied)?;
        if Some(agent.settings.access) != current.granted_access {
            return Err(denied());
        }
    }
    Ok(current)
}

pub(crate) fn check_revision(db: &Db<'_>, expected: &Connection) -> Result<()> {
    let current = db
        .plugin_instance(&expected.id)?
        .ok_or_else(|| Error::forbidden("This plugin instance was removed."))?;
    if current.revision != expected.revision || current.enabled != expected.enabled {
        return Err(Error::conflict(
            "This instance changed during preparation. Reload it and retry.",
        ));
    }
    Ok(())
}

/// The connection's secret, for host-side injection only. Never sent to plugin code.
pub(crate) async fn credential(s: &Service, connection: &Connection) -> Result<Option<String>> {
    super::oauth::access_token(s, connection).await
}

pub(crate) async fn status(
    s: &Service,
    connection: &Connection,
    state: cairn_contracts::PluginState,
) {
    status_with(s, connection, state, None).await;
}

pub(crate) async fn status_with(
    s: &Service,
    connection: &Connection,
    state: cairn_contracts::PluginState,
    message: Option<&str>,
) {
    let (id, revision, message) = (
        connection.id.clone(),
        connection.revision.clone(),
        message.map(str::to_owned),
    );
    let _ = s
        .store
        .transaction(move |db| {
            db.set_plugin_status(
                &id,
                &revision,
                &cairn_contracts::PluginStatus { state, message },
            )
        })
        .await;
}

/// Only manifest settings cross this boundary as dynamic data. Saved approval
/// snapshots and owner bindings never enter the owner/agent response.
pub(crate) fn public_view(connection: &Connection) -> Result<cairn_contracts::PluginConnection> {
    Ok(cairn_contracts::PluginConnection {
        id: connection.id.clone(),
        plugin_id: connection.plugin_id.clone(),
        name: connection.name.clone(),
        enabled: connection.enabled,
        revision: connection.revision.clone(),
        settings: serde_json::from_value(connection.settings.clone())?,
        secret_keys: connection.secret_keys.clone(),
        agent_ids: connection.agent_ids.clone(),
        agent_id: connection.agent_id.clone(),
        project_id: connection.project_id.clone(),
        access_mode: connection.access_mode,
        user_ids: connection.user_ids.clone(),
        people: connection.people.clone(),
        max_messages_per_hour: connection.max_messages_per_hour,
        max_per_person_per_hour: connection.max_per_person_per_hour,
        ..Default::default()
    })
}

/// Validates settings against the manifest schema. Unknown keys are rejected; secret
/// fields are saved separately with [`secrets`].
pub fn settings(manifest: &Manifest, input: &Value) -> Result<Value> {
    settings_with(&manifest.settings, input)
}

/// Validates values against declared fields, such as a skill source's location form.
pub fn settings_with(fields: &[Value], input: &Value) -> Result<Value> {
    let empty = Map::new();
    let input = match input {
        Value::Object(map) => map,
        Value::Null => &empty,
        _ => return Err(Error::bad("Plugin settings must be an object.")),
    };
    if let Some(key) = input.keys().find(|key| {
        !fields
            .iter()
            .any(|field| field["key"] == **key && field["type"] != "secret")
    }) {
        return Err(Error::bad(format!("Unknown plugin setting: {key}.")));
    }
    let mut output = Map::new();
    for field in fields.iter().filter(|field| field["type"] != "secret") {
        let key = text(field, "key").to_owned();
        // An empty choice is no choice, e.g. a dynamic channel reset by the package.
        let empty = |value: &&Value| {
            value.is_null() || (text(field, "type") == "select" && value.as_str() == Some(""))
        };
        let value = input
            .get(&key)
            .filter(|value| !empty(value))
            .or_else(|| field.get("default").filter(|value| !value.is_null()));
        let Some(value) = value else {
            if field["required"] == true {
                return Err(Error::bad(format!("Enter {}.", text(field, "title"))));
            }
            continue;
        };
        output.insert(key, setting(field, value)?);
    }
    Ok(Value::Object(output))
}

fn matches_pattern(field: &Value, value: &str) -> bool {
    field["pattern"]
        .as_str()
        .is_none_or(|source| cairn_plugin_api::pattern(source).is_some_and(|re| re.is_match(value)))
}

/// One checked value. Messages name the field, never the submitted value.
pub(crate) fn setting(field: &Value, value: &Value) -> Result<Value> {
    let title = text(field, "title");
    let invalid = || Error::bad(format!("{title} is too long or invalid."));
    let kind = text(field, "type");
    Ok(match kind {
        "string" | "text" | "url" => {
            let default_max = if kind == "string" { 200 } else { 2000 };
            let max = field["maxLength"]
                .as_u64()
                .unwrap_or(default_max)
                .min(10_000) as usize;
            let value = value
                .as_str()
                .ok_or_else(|| Error::bad(format!("{title} must be text.")))?;
            let multiline = kind == "text";
            if value.chars().count() > max
                || value
                    .chars()
                    .any(|c| c.is_control() && !(multiline && matches!(c, '\n' | '\t')))
            {
                return Err(invalid());
            }
            let value = value.trim();
            if field["required"] == true && value.is_empty() {
                return Err(Error::bad(format!("Enter {title}.")));
            }
            if kind == "url"
                && !value.is_empty()
                && !url::Url::parse(value).is_ok_and(|url| matches!(url.scheme(), "https" | "http"))
            {
                return Err(Error::bad(format!("{title} must be a web address.")));
            }
            if !value.is_empty() && !matches_pattern(field, value) {
                return Err(Error::bad(format!("{title} has an invalid format.")));
            }
            json!(value)
        }
        "number" => {
            let number = value
                .as_f64()
                .filter(|number| number.is_finite())
                .ok_or_else(|| Error::bad(format!("{title} must be a number.")))?;
            let low = field["minimum"].as_f64().unwrap_or(f64::MIN);
            let high = field["maximum"].as_f64().unwrap_or(f64::MAX);
            if number < low || number > high {
                return Err(Error::bad(format!("{title} is out of range.")));
            }
            value.clone()
        }
        "boolean" => json!(
            value
                .as_bool()
                .ok_or_else(|| Error::bad(format!("{title} must be on or off.")))?
        ),
        // Dynamic choices come from the package's form; the package checks them on save.
        "select" | "multiselect" if field["dynamic"] == true => {
            let max = field["maxLength"].as_u64().unwrap_or(200).min(10_000) as usize;
            let valid = |choice: &Value| {
                choice.as_str().is_some_and(|choice| {
                    !choice.is_empty()
                        && choice.chars().count() <= max
                        && !choice.chars().any(char::is_control)
                        && matches_pattern(field, choice)
                })
            };
            if kind == "select" {
                if !valid(value) {
                    return Err(Error::bad(format!("Choose {title}.")));
                }
                value.clone()
            } else {
                let choices = value
                    .as_array()
                    .filter(|choices| choices.len() <= 100 && choices.iter().all(valid))
                    .ok_or_else(|| Error::bad(format!("Choose {title}.")))?;
                if field["required"] == true && choices.is_empty() {
                    return Err(Error::bad(format!("Choose {title}.")));
                }
                json!(choices)
            }
        }
        "select" | "multiselect" => {
            let allowed = |choice: &str| {
                field["options"]
                    .as_array()
                    .is_some_and(|options| options.iter().any(|option| option["value"] == choice))
            };
            if kind == "select" {
                let choice = value.as_str().filter(|choice| allowed(choice));
                json!(choice.ok_or_else(|| Error::bad(format!("Choose {title}.")))?)
            } else {
                let choices = value
                    .as_array()
                    .ok_or_else(|| Error::bad(format!("Choose {title}.")))?;
                let mut output = Vec::new();
                for choice in choices {
                    let choice = choice.as_str().filter(|choice| allowed(choice));
                    let choice = choice.ok_or_else(|| Error::bad(format!("Choose {title}.")))?;
                    if !output.contains(&choice) {
                        output.push(choice);
                    }
                }
                if field["required"] == true && output.is_empty() {
                    return Err(Error::bad(format!("Choose {title}.")));
                }
                json!(output)
            }
        }
        _ => return Err(invalid()),
    })
}

/// Merges submitted secret fields with the saved ones. An empty or missing value keeps
/// the saved secret; `null` clears an optional one. Errors never echo values.
pub fn secrets(
    manifest: &Manifest,
    input: &Value,
    saved: &Map<String, Value>,
) -> Result<Map<String, Value>> {
    let empty = Map::new();
    let input = match input {
        Value::Object(map) => map,
        Value::Null => &empty,
        _ => return Err(Error::bad("Plugin secrets must be an object.")),
    };
    let fields = manifest
        .settings
        .iter()
        .filter(|field| field["type"] == "secret")
        .collect::<Vec<_>>();
    if let Some(key) = input
        .keys()
        .find(|key| !fields.iter().any(|field| field["key"] == **key))
    {
        return Err(Error::bad(format!("Unknown plugin secret: {key}.")));
    }
    let mut output = Map::new();
    for field in fields {
        let key = text(field, "key");
        let title = text(field, "title");
        let value = match input.get(key) {
            Some(Value::Null) => None,
            Some(Value::String(value)) if !value.trim().is_empty() => {
                let value = value.trim();
                let max = field["maxLength"].as_u64().unwrap_or(16_384).min(16_384) as usize;
                if value.len() > max
                    || value.chars().any(char::is_control)
                    || !matches_pattern(field, value)
                {
                    return Err(Error::bad(format!("Enter a valid {title}.")));
                }
                Some(json!(value))
            }
            Some(Value::String(_)) | None => saved.get(key).cloned(),
            Some(_) => return Err(Error::bad(format!("Enter a valid {title}."))),
        };
        match value {
            Some(value) => {
                output.insert(key.to_owned(), value);
            }
            None if field["required"] == true => {
                return Err(Error::bad(format!("Enter {title}.")));
            }
            None => {}
        }
    }
    Ok(output)
}

pub fn secrets_key(id: &str) -> String {
    format!("plugin-secrets:{id}")
}

/// The instance's secret fields, for host-side injection only. Never sent to scripts.
pub(crate) async fn secret_values(s: &Service, connection_id: &str) -> Result<Map<String, Value>> {
    Ok(s.vault
        .get(&secrets_key(connection_id))
        .await?
        .and_then(|value| value.get("fields").and_then(Value::as_object).cloned())
        .unwrap_or_default())
}

/// Values of the package's shared setup steps, copied from another of its instances,
/// e.g. one OAuth client for every mailbox. The client names that instance
/// (`sharedFrom`); its secrets stay in the host.
pub(crate) struct Shared {
    pub source: Connection,
    pub settings: Map<String, Value>,
    pub secrets: Map<String, Value>,
}

impl Shared {
    /// Only the manifest-owned settings and secrets are extensible.
    pub fn apply_values(&self, settings: &mut Value, secrets: &mut Value) {
        for (target, values) in [(settings, &self.settings), (secrets, &self.secrets)] {
            if !target.is_object() {
                *target = json!({});
            }
            for (key, value) in values {
                target[key] = value.clone();
            }
        }
    }
}

/// The instance an input reuses, if it names one with `sharedFrom`.
pub(crate) fn shared_source(input: &Value) -> Result<Option<&str>> {
    match input.get("sharedFrom") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(id)) if !id.is_empty() => Ok(Some(id)),
        Some(_) => Err(Error::bad("Choose the instance to reuse.")),
    }
}

/// Only a new instance reuses another one's setup: an edit keeps its own.
pub(crate) async fn shared(
    s: &Service,
    manifest: &Manifest,
    plugin_id: &str,
    input: &Value,
    editing: bool,
) -> Result<Option<Shared>> {
    shared_from(s, manifest, plugin_id, shared_source(input)?, editing).await
}

pub(crate) async fn shared_from(
    s: &Service,
    manifest: &Manifest,
    plugin_id: &str,
    source: Option<&str>,
    editing: bool,
) -> Result<Option<Shared>> {
    let Some(source) = source else {
        return Ok(None);
    };
    if editing {
        return Err(Error::bad(
            "Only a new instance can reuse another instance's setup.",
        ));
    }
    let keys = manifest.shared_settings();
    if keys.is_empty() {
        return Err(Error::bad(
            "This plugin has nothing to reuse from another instance.",
        ));
    }
    let source = get(s, plugin_id, source).await?;
    let saved = secret_values(s, &source.id).await?;
    let mut shared = Shared {
        source: source.clone(),
        settings: Map::new(),
        secrets: Map::new(),
    };
    for field in manifest
        .settings
        .iter()
        .filter(|field| keys.contains(&text(field, "key")))
    {
        let key = text(field, "key").to_owned();
        if field["type"] == "secret" {
            let value = saved.get(&key).cloned();
            if value.is_none() && field["required"] == true {
                return Err(Error::bad(format!(
                    "{} has no saved {}. Set it up again.",
                    source.name,
                    text(field, "title")
                )));
            }
            // Reusing an absent optional secret must also replace any typed draft value.
            shared.secrets.insert(key, value.unwrap_or(Value::Null));
        } else {
            shared
                .settings
                .insert(key.clone(), source.settings[&key].clone());
        }
    }
    Ok(Some(shared))
}

/// Agents granted the package's commands and tools on this instance. Off by default.
fn agent_grants(manifest: &Manifest, input: Option<&[String]>) -> Result<Vec<String>> {
    let agents = input.unwrap_or_default();
    if manifest.agent_access.is_none() && !agents.is_empty() {
        return Err(Error::bad("This plugin does not give agents access."));
    }
    if agents.len() > 100 {
        return Err(Error::bad("Select at most 100 agents."));
    }
    let mut ids = Vec::new();
    for agent in agents {
        let agent = agent.as_str();
        uuid(agent)?;
        if !ids.iter().any(|id| id == agent) {
            ids.push(agent.to_owned());
        }
    }
    Ok(ids)
}

pub async fn save(
    s: &Service,
    plugin_id: &str,
    command: cairn_contracts::PluginConfiguration,
    existing: Option<&str>,
) -> Result<cairn_contracts::PluginConnection> {
    Box::pin(save_authorized(
        s,
        plugin_id,
        command,
        existing,
        |_| Ok(()),
        None,
    ))
    .await
}

pub(crate) async fn save_authorized<F>(
    s: &Service,
    plugin_id: &str,
    command: cairn_contracts::PluginConfiguration,
    existing: Option<&str>,
    authorize_management: F,
    management: Option<super::management::Authority>,
) -> Result<cairn_contracts::PluginConnection>
where
    F: Fn(&Db<'_>) -> Result<()> + Send + Sync + 'static,
{
    let package = registry::active(s, plugin_id).await?;
    let manifest = package.manifest.clone();
    let source = command.shared_from.map(|id| id.to_string());
    let shared = shared_from(
        s,
        &manifest,
        plugin_id,
        source.as_deref(),
        existing.is_some(),
    )
    .await?;
    let mut settings_input = serde_json::to_value(&command.settings)?;
    let mut secrets_input = command
        .secrets
        .as_ref()
        .map(serde_json::to_value)
        .transpose()?
        .unwrap_or_else(|| json!({}));
    if let Some(shared) = &shared {
        shared.apply_values(&mut settings_input, &mut secrets_input);
    }
    let mut prepared = shared
        .as_ref()
        .map(|shared| shared.source.clone())
        .into_iter()
        .collect::<Vec<_>>();
    let expected_revision = match existing {
        Some(instance) => {
            let current = get(s, plugin_id, instance).await?;
            let revision = command
                .revision
                .as_ref()
                .ok_or_else(|| Error::bad("Supply the instance revision before replacing it."))?;
            if revision != &current.revision {
                return Err(Error::conflict(
                    "This instance changed. Reload it before saving.",
                ));
            }
            prepared.push(current.clone());
            Some(current.revision)
        }
        None => None,
    };
    let owner = crate::relay::task_author_policy(s).await?;
    let origin = crate::native::policy::origin();
    let name = command.name.trim();
    if name.is_empty() || name.chars().count() > 100 {
        return Err(Error::bad("Enter a connection name."));
    }
    let mut settings = settings(&manifest, &settings_input)?;
    let delegates = manifest.permits_capability("conversations.submit");
    let agent_id = if delegates {
        command.agent_id.as_str()
    } else {
        ""
    };
    let project = command.project_id.clone().filter(|_| delegates);
    if delegates {
        uuid(agent_id)?;
        crate::native::policy::refresh_agent(s, agent_id).await?;
    }
    if let Some(project) = &project {
        uuid(project)?;
    }
    let access_mode = command.access_mode;
    if !delegates && access_mode == AccessMode::Everyone {
        return Err(Error::bad("Choose who can use this connection."));
    }
    let users = &command.user_ids;
    if users.len() > 100 || users.iter().any(|user| !external_id(user)) {
        return Err(Error::bad("Enter valid user IDs, at most 100."));
    }
    // Approval mode may start empty: the owner approves the first request themselves.
    if delegates && users.is_empty() && access_mode == AccessMode::Allowlist {
        return Err(Error::bad(
            "Add at least one person who can use this connection.",
        ));
    }
    let mut user_ids = Vec::new();
    for user in users {
        if !user_ids.contains(user) {
            user_ids.push(user.clone());
        }
    }
    let people = user_ids
        .iter()
        .map(|id| {
            command
                .people
                .iter()
                .find(|person| person.id == *id)
                .map_or_else(
                    || Label {
                        id: id.clone(),
                        name: String::new(),
                    },
                    |person| Label {
                        id: id.clone(),
                        name: label(&person.name),
                    },
                )
        })
        .collect();
    let enabled = command.enabled;
    let limit = command.max_messages_per_hour;
    let per_person = command.max_per_person_per_hour;
    if !(1..=100).contains(&limit) || !(1..=50).contains(&per_person) {
        return Err(Error::bad("Choose valid hourly message limits."));
    }
    // A device sign-in the owner completed supplies the credential instead of a paste.
    let flow = command
        .credential_flow
        .as_deref()
        .filter(|flow| !flow.is_empty());
    let flow_lock = flow.map(|flow| s.plugins.oauth_flows.lock(&format!("flow:{flow}")));
    let _flow_guard = match &flow_lock {
        Some(lock) => Some(lock.lock().await),
        None => None,
    };
    let signed_record = match flow {
        Some(flow) => Some(
            super::oauth::flow_credential(s, plugin_id, flow, existing, &settings, false).await?,
        ),
        None => None,
    };
    let token = signed_record
        .as_ref()
        .and_then(|v| v["token"].as_str())
        .unwrap_or_else(|| command.token.as_deref().unwrap_or_default())
        .trim();
    if token.len() > 16_384
        || token.chars().any(|c| c.is_whitespace() || c.is_control())
        || (existing.is_none() && token.is_empty() && manifest.credential.is_some())
    {
        let label = manifest
            .credential
            .as_ref()
            .map_or("credential", |c| c.label.as_str());
        return Err(Error::bad(format!("Enter the {label}.")));
    }
    if manifest
        .credential
        .as_ref()
        .is_some_and(|c| c.oauth.is_some())
        && signed_record.is_none()
        && !token.is_empty()
    {
        return Err(Error::bad(
            "Connect this account through OAuth; pasted access tokens are not accepted.",
        ));
    }
    let connection_id = existing.map_or_else(id, str::to_owned);
    uuid(&connection_id)?;
    // Existing grants require a revision-guarded profile command. Setup may
    // grant the newly created instance atomically, before any editor can hold it.
    if existing.is_some() && command.agent_ids.is_some() {
        return Err(Error::bad(
            "Edit plugin grants from the agent page with its current revision.",
        ));
    }
    let keep_agents = existing.is_some();
    let agent_ids = agent_grants(&manifest, command.agent_ids.as_deref())?;
    let saved = match existing {
        Some(_) => secret_values(s, &connection_id).await?,
        None => Map::new(),
    };
    let secret_fields = secrets(&manifest, &secrets_input, &saved)?;
    if let Some(record) = &signed_record
        && let Some(key) = manifest
            .credential
            .as_ref()
            .and_then(|c| c.oauth.as_ref())
            .and_then(|o| o.client_secret_setting.as_ref())
        && record["oauth"]["clientSecret"] != secret_fields.get(key).cloned().unwrap_or(Value::Null)
    {
        return Err(Error::bad(
            "The client secret changed after sign-in. Sign in again.",
        ));
    }
    let secret_keys = secret_fields.keys().cloned().collect::<Vec<_>>();
    if manifest.instance_form {
        let credential = match (token.is_empty(), existing) {
            (false, _) => Some(token.to_owned()),
            (true, Some(id)) => {
                super::oauth::draft_token(s, &get(s, plugin_id, id).await?, management.as_ref())
                    .await?
            }
            (true, None) => None,
        };
        let draft = forms::Draft {
            prepared: prepared.clone(),
            management: management.clone(),
            credential,
            secrets: secret_fields.clone(),
        };
        settings = forms::submit(s, &package, &connection_id, settings, draft).await?;
    }
    if signed_record.is_none()
        && let Some(oauth) = manifest.credential.as_ref().and_then(|c| c.oauth.as_ref())
    {
        let previous = match existing {
            Some(id) => Some(get(s, plugin_id, id).await?),
            None => None,
        };
        if let Some(previous) = previous {
            let client_changed =
                previous.settings[&oauth.client_id_setting] != settings[&oauth.client_id_setting];
            let added_scope = oauth
                .scope_settings
                .keys()
                .any(|key| settings[key] == true && previous.settings[key] != true);
            let secret_changed = oauth
                .client_secret_setting
                .as_ref()
                .is_some_and(|key| secret_fields.get(key) != saved.get(key));
            if client_changed || added_scope || secret_changed {
                return Err(Error::bad(
                    "The OAuth client or permissions changed. Sign in again before saving.",
                ));
            }
        }
    }
    if let Some(flow) = flow {
        super::oauth::flow_credential(s, plugin_id, flow, existing, &settings, false).await?;
    }
    let encrypted_secrets = if manifest.secret_settings().next().is_some() {
        Some(s.vault.encrypt(
            &secrets_key(&connection_id),
            &json!({ "fields": secret_fields }),
        )?)
    } else {
        None
    };
    let encrypted = if token.is_empty() {
        None
    } else {
        Some(
            s.vault.encrypt(
                &credential_key(&connection_id),
                &signed_record
                    .clone()
                    .unwrap_or_else(|| json!({ "token": token })),
            )?,
        )
    };
    let mut connection = Connection {
        id: connection_id,
        plugin_id: plugin_id.to_owned(),
        name: name.to_owned(),
        enabled,
        revision: id(),
        settings,
        agent_id: agent_id.to_owned(),
        project_id: project,
        granted_access: None,
        native_authority: None,
        owner_access_id: owner.as_ref().map(|policy| policy.owner.access_id.clone()),
        access_mode: if delegates {
            access_mode
        } else {
            AccessMode::Allowlist
        },
        user_ids: if delegates { user_ids } else { Vec::new() },
        people: if delegates { people } else { Vec::new() },
        max_messages_per_hour: if delegates { limit } else { 20 },
        max_per_person_per_hour: if delegates { per_person } else { 5 },
        agent_ids,
        secret_keys,
        updated_at: now(),
    };
    let updating = existing.is_some();
    if let Some(authority) = &management {
        authority.recheck(s).await?;
    }
    let result = s
        .store
        .transaction(move |db| {
            authorize_management(db)?;
            for expected in &prepared {
                check_revision(db, expected)?;
            }
            if let Some(current) = db.plugin_instance(&connection.id)? {
                if expected_revision
                    .as_ref()
                    .is_some_and(|revision| current.revision != *revision)
                {
                    return Err(Error::conflict(
                        "This instance changed while it was being checked. Reload it and retry.",
                    ));
                }
                if current.plugin_id != connection.plugin_id {
                    return Err(Error::not_found("Plugin connection not found."));
                }
            } else if updating {
                return Err(Error::not_found("Plugin connection not found."));
            }
            if !updating && db.plugin_instances()?.len() >= 64 {
                return Err(Error::bad(
                    "At most 64 plugin instances can exist on this installation.",
                ));
            }
            for agent in &connection.agent_ids {
                if db.agent_snapshot(agent)?.is_none() {
                    return Err(Error::bad("A selected agent no longer exists."));
                }
            }
            if !connection.agent_id.is_empty() {
                let agent = db
                    .agent(&connection.agent_id)?
                    .ok_or_else(|| Error::bad("Agent not found."))?;
                let access = agent.settings.access.clone();
                delegation_rules(db, &connection, &access)?;
                connection.granted_access = Some(access);
                connection.native_authority =
                    native_authority(db, &connection, &serde_json::to_value(&agent)?, &origin)?;
            }
            if connection.access_mode != AccessMode::Requests {
                for request in db.plugin_requests()? {
                    if request.instance_id == connection.id
                        && request.status == super::state::RequestStatus::Pending
                    {
                        db.remove_plugin_request(&request.id)?;
                    }
                }
            }
            if let Some(encrypted) = encrypted {
                db.set(
                    &format!("mcp-secret:{}", credential_key(&connection.id)),
                    &encrypted,
                    None,
                )?;
            }
            if let Some(encrypted) = encrypted_secrets {
                db.set(
                    &format!("mcp-secret:{}", secrets_key(&connection.id)),
                    &encrypted,
                    None,
                )?;
            }
            db.save_plugin_instance(&connection)?;
            if !keep_agents {
                crate::agent_grants::set_instance_agents(
                    db,
                    &connection.id,
                    &connection.agent_ids,
                )?;
            } else if let Some(current) = db.plugin_instance(&connection.id)? {
                connection.agent_ids = current.agent_ids;
            }
            let value = public_view(&connection)?;
            db.audit(
                "plugin.connection.saved",
                &json!({ "id": connection.id, "plugin": connection.plugin_id }),
            )?;
            Ok(value)
        })
        .await?;
    if let Some(flow) = flow {
        if manifest
            .credential
            .as_ref()
            .is_some_and(|c| c.oauth.is_some())
        {
            s.plugins.oauth_flows.consume(plugin_id, flow);
        } else {
            let _ = s.plugins.device_flows.token(plugin_id, flow, true);
        }
    }
    s.plugins.program_caches.forget(&result.id);
    s.plugins.changed.notify_one();
    Ok(result)
}

/// Whether the delegation's project exists and its agent can open it. The owner chooses
/// how much the agent may do: any agent can answer, whoever it is delegated to.
fn delegation_rules(
    db: &Db<'_>,
    connection: &Connection,
    access: &cairn_contracts::AccessPolicy,
) -> Result<()> {
    let Some(project) = &connection.project_id else {
        return Ok(());
    };
    if db.project(project)?.is_none() {
        return Err(Error::bad("Project not found."));
    }
    let reachable = access
        .projects
        .as_ref()
        .is_none_or(|projects| projects.contains(project));
    if !reachable {
        return Err(Error::bad("This agent can’t open the selected project."));
    }
    Ok(())
}

/// Loads an instance by ID, whatever its plugin.
pub(crate) async fn load(s: &Service, connection_id: &str) -> Result<Connection> {
    uuid(connection_id)?;
    s.store
        .plugin_instance(connection_id)
        .await?
        .ok_or_else(|| Error::not_found("Plugin connection not found."))
}

/// Approves the agent's saved access for a delegation that answers as this agent,
/// as saving the instance again would. Runs in the agent's save transaction.
fn native_authority(
    db: &Db<'_>,
    connection: &Connection,
    agent: &Value,
    origin: &crate::native::policy::Origin,
) -> Result<Option<NativeAuthority>> {
    if !crate::native::policy::supports(agent, "native") {
        return Ok(None);
    }
    let mut grant = crate::native::policy::authorize_origin(db, agent, origin)?;
    if !crate::native::policy::native(agent)
        && (grant["nativeProvenanceUnknown"] != false
            || grant["nativeAuthorityNodes"]
                .as_array()
                .is_none_or(Vec::is_empty))
    {
        return Ok(None);
    }
    let authority = db.kv("native-authority")?.unwrap_or_default();
    let owner_access = connection
        .owner_access_id
        .as_deref()
        .ok_or_else(|| Error::forbidden("The bot has no current owner delegation."))?;
    let owner = authority["accounts"]
        .as_object()
        .into_iter()
        .flatten()
        .find(|(_, account)| account["accessId"].as_str() == Some(owner_access))
        .map(|(id, _)| id.clone());
    let Some(owner) = owner else {
        if !crate::native::policy::native(agent) {
            return Ok(None);
        }
        return Err(Error::forbidden(
            "The bot's owner has no current native authority.",
        ));
    };
    grant["pluginOwnerAccountId"] = owner.into();
    Ok(Some(serde_json::from_value(grant)?))
}

pub(crate) fn reapprove(
    db: &Db<'_>,
    connection_id: &str,
    agent: &cairn_contracts::Agent,
    origin: &crate::native::policy::Origin,
) -> Result<()> {
    let mut connection = db
        .plugin_instance(connection_id)?
        .ok_or_else(|| Error::bad("A plugin delegation to reapprove no longer exists."))?;
    if connection.agent_id != agent.id.to_string() {
        return Err(Error::bad(
            "This plugin delegation answers as another agent.",
        ));
    }
    let access = agent.settings.access.clone();
    delegation_rules(db, &connection, &access).map_err(|error| {
        Error::bad(format!(
            "{} cannot answer as this agent with the new access: {error}",
            connection.name
        ))
    })?;
    connection.granted_access = Some(access);
    connection.native_authority =
        native_authority(db, &connection, &serde_json::to_value(agent)?, origin)?;
    connection.revision = id();
    connection.updated_at = now();
    db.save_plugin_instance(&connection)?;
    db.audit(
        "plugin.connection.reapproved",
        &json!({ "id": connection.id, "agentId": connection.agent_id }),
    )
}

/// Delegations answering as an agent and how an edited agent would affect them,
/// including `alreadySuspended` when the saved agent no longer matches the approval:
/// `suspended` when its access no longer matches the approved one or no longer meets
/// the delegation's rules, such as reaching its project, and the reason it could not
/// be reapproved, if any. A delegation bound to a previous installation owner already
/// stopped answering and only its instance screen can rebind it, so it counts as
/// already suspended and cannot be reapproved here.
pub(crate) fn delegation_effects(
    db: &Db<'_>,
    agent_id: &str,
    agent: &Value,
    owner: Option<&str>,
) -> Result<Value> {
    let profile: cairn_contracts::Agent = serde_json::from_value(agent.clone())?;
    let access = profile.settings.access;
    let saved = db.agent(agent_id)?;
    let mut effects = Vec::new();
    for connection in db.plugin_instances()? {
        if connection.agent_id != agent_id {
            continue;
        }
        let rules = delegation_rules(db, &connection, &access)
            .err()
            .map(|error| error.to_string());
        let previous_owner = connection.owner_access_id.as_deref() != owner;
        // Suspended before this edit, so reapproving would also approve earlier changes.
        let already_suspended = previous_owner
            || saved.as_ref().is_none_or(|saved| {
                Some(&saved.settings.access) != connection.granted_access.as_ref()
            });
        effects.push(json!({
            "id": connection.id,
            "pluginId": connection.plugin_id,
            "name": connection.name,
            "enabled": connection.enabled,
            "suspended": rules.is_some()
                || Some(&access) != connection.granted_access.as_ref(),
            "blocked": if previous_owner {
                Some("It belongs to a previous installation owner. Save it again from its instance screen.".to_owned())
            } else {
                rules
            },
            "alreadySuspended": already_suspended,
        }));
    }
    Ok(effects.into())
}

/// Loads a connection and checks it belongs to the plugin named in the route.
pub async fn get(s: &Service, plugin_id: &str, connection_id: &str) -> Result<Connection> {
    uuid(connection_id)?;
    let connection = load(s, connection_id).await?;
    if connection.plugin_id != plugin_id {
        return Err(Error::not_found("Plugin connection not found."));
    }
    Ok(connection)
}

pub async fn remove(s: &Service, plugin_id: &str, connection_id: &str) -> Result<Value> {
    remove_authorized(s, plugin_id, connection_id, |_| Ok(())).await
}

pub(crate) async fn remove_authorized<F>(
    s: &Service,
    plugin_id: &str,
    connection_id: &str,
    authorize_management: F,
) -> Result<Value>
where
    F: Fn(&Db<'_>) -> Result<()> + Send + Sync + 'static,
{
    get(s, plugin_id, connection_id).await?;
    let forgotten_instance = connection_id.to_owned();
    let connection_id = connection_id.to_owned();
    s.store
        .transaction(move |db| {
            authorize_management(db)?;
            crate::agent_grants::forget_resource(db, crate::agent_grants::PLUGINS, &connection_id)?;
            db.remove_plugin_instance(&connection_id)?;
            db.delete(&format!("mcp-secret:{}", credential_key(&connection_id)))?;
            db.delete(&format!("mcp-secret:{}", secrets_key(&connection_id)))?;
            db.audit("plugin.connection.deleted", &json!({ "id": connection_id }))
        })
        .await?;
    s.plugins.files.forget(&forgotten_instance);
    s.plugins.program_caches.forget(&forgotten_instance);
    s.plugins.changed.notify_one();
    Ok(json!({ "deleted": true }))
}

/// Owner view: configuration, live state, usage and recent exchanges. Never secrets.
pub async fn overview(s: &Service) -> Result<Vec<cairn_contracts::PluginConnection>> {
    let hour = now() / 3_600_000;
    s.store
        .read(move |db| {
            let requests = db.plugin_requests()?;
            let activity = db.plugin_activity()?;
            let statuses = db.plugin_statuses()?;
            let mut result = Vec::new();
            for connection in db.plugin_instances()? {
                let mut view = public_view(&connection)?;
                view.status = statuses.get(&connection.id).cloned().unwrap_or_default();
                view.usage.hour = db
                    .kv(&format!("plugin-rate:{}:{hour}", connection.id))?
                    .and_then(|value| value.as_u64())
                    .unwrap_or(0)
                    .min(u32::MAX.into()) as u32;
                view.requests = requests
                    .iter()
                    .filter(|value| {
                        value.instance_id == connection.id
                            && value.status == super::state::RequestStatus::Pending
                    })
                    .map(|value| cairn_contracts::PluginRequest {
                        id: value.id.clone(),
                        user_id: value.user_id.clone(),
                        name: value.name.clone(),
                        mentions: value.mentions,
                        updated_at: value.updated_at,
                    })
                    .collect();
                view.requests
                    .sort_by_key(|request| std::cmp::Reverse(request.updated_at));
                view.activity = activity
                    .iter()
                    .filter(|value| value.instance_id == connection.id)
                    .map(|value| cairn_contracts::PluginActivity {
                        id: value.id.clone(),
                        chat_id: value.chat_id.clone(),
                        user_id: value.user_id.clone(),
                        name: value.name.clone(),
                        preview: value.preview.clone(),
                        status: value.status,
                        created_at: value.created_at,
                    })
                    .collect();
                view.activity
                    .sort_by_key(|activity| std::cmp::Reverse(activity.created_at));
                view.activity.truncate(8);
                result.push(view);
            }
            Ok(result)
        })
        .await
}

/// Pausing keeps the configuration and credential; the worker stops within seconds.
pub async fn set_enabled(
    s: &Service,
    plugin_id: &str,
    id: &str,
    input: &cairn_contracts::PluginEnabledInput,
) -> Result<cairn_contracts::PluginConnection> {
    set_enabled_authorized(s, plugin_id, id, input, |_| Ok(())).await
}

pub(crate) async fn set_enabled_authorized<F>(
    s: &Service,
    plugin_id: &str,
    id: &str,
    input: &cairn_contracts::PluginEnabledInput,
    authorize_management: F,
) -> Result<cairn_contracts::PluginConnection>
where
    F: Fn(&Db<'_>) -> Result<()> + Send + Sync + 'static,
{
    get(s, plugin_id, id).await?;
    let enabled = input.enabled;
    let oauth = if enabled {
        registry::active(s, plugin_id)
            .await?
            .manifest
            .credential
            .as_ref()
            .is_some_and(|c| c.oauth.is_some())
    } else {
        false
    };
    let id = id.to_owned();
    let value = s
        .store
        .transaction(move |db| {
            authorize_management(db)?;
            if enabled
                && oauth
                && db
                    .kv(&format!("mcp-secret:{}", credential_key(&id)))?
                    .is_none()
            {
                return Err(Error::bad("Reconnect this account before enabling it."));
            }
            let mut connection = db
                .plugin_instance(&id)?
                .ok_or_else(|| Error::not_found("Plugin connection not found."))?;
            if connection.enabled != enabled {
                connection.revision = crate::config::id();
            }
            connection.enabled = enabled;
            connection.updated_at = now();
            db.save_plugin_instance(&connection)?;
            db.audit(
                "plugin.connection.enabled",
                &json!({ "id": id, "enabled": enabled }),
            )?;
            public_view(&connection)
        })
        .await?;
    s.plugins.program_caches.forget(&value.id);
    s.plugins.changed.notify_one();
    Ok(value)
}

/// Approve, deny or remove one person. Admission rereads this list for every
/// message, so changes apply immediately without restarting the worker.
pub async fn people(
    s: &Service,
    plugin_id: &str,
    connection_id: &str,
    user_id: &str,
    action: cairn_contracts::PluginPersonAction,
) -> Result<cairn_contracts::PluginConnection> {
    get(s, plugin_id, connection_id).await?;
    if !external_id(user_id) {
        return Err(Error::bad("Invalid person."));
    }
    let (connection_id, user_id, action) = (connection_id.to_owned(), user_id.to_owned(), action);
    s.store
        .transaction(move |db| {
            let mut connection = db
                .plugin_instance(&connection_id)?
                .ok_or_else(|| Error::not_found("Plugin connection not found."))?;
            let request = db
                .plugin_requests()?
                .into_iter()
                .find(|value| value.instance_id == connection_id && value.user_id == user_id);
            match action {
                cairn_contracts::PluginPersonAction::Approve => {
                    let request =
                        request.ok_or_else(|| Error::not_found("Access request not found."))?;
                    if !connection.user_ids.contains(&user_id) {
                        if connection.user_ids.len() >= 100 {
                            return Err(Error::bad("At most 100 people can use one connection."));
                        }
                        connection.user_ids.push(user_id.clone());
                        connection.people.push(Label {
                            id: user_id.clone(),
                            name: label(&request.name),
                        });
                    }
                    db.remove_plugin_request(&request.id)?;
                }
                cairn_contracts::PluginPersonAction::Deny => {
                    // A denied request stays recorded so repeated attempts stay silent.
                    let mut request =
                        request.ok_or_else(|| Error::not_found("Access request not found."))?;
                    request.status = super::state::RequestStatus::Denied;
                    request.updated_at = now();
                    db.save_plugin_request(&request)?;
                }
                cairn_contracts::PluginPersonAction::Remove => {
                    connection.user_ids.retain(|id| *id != user_id);
                    connection.people.retain(|person| person.id != user_id);
                    if let Some(request) = request {
                        db.remove_plugin_request(&request.id)?;
                    }
                }
            }
            connection.updated_at = now();
            db.save_plugin_instance(&connection)?;
            let value = public_view(&connection)?;
            db.audit(
                "plugin.connection.people",
                &json!({ "id": connection_id, "action": action }),
            )?;
            Ok(value)
        })
        .await
}
