//! Configured plugin connections: an owner delegation (agent, project, people, quota)
//! plus plugin settings declared by the manifest. Identical for every plugin.
use super::{forms, registry};
use crate::{
    config::{id, now},
    error::{Error, Result},
    service::{Service, policy},
    store::Db,
    validation::{text, uuid},
};
use cairn_plugin_api::Manifest;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

pub const KIND: &str = "plugin-connections";

/// Records owned by a connection, removed with it.
pub const OWNED: &[&str] = &[
    "plugin-events",
    "plugin-deliveries",
    "plugin-requests",
    "plugin-activity",
    "plugin-storage",
    "plugin-http-receipts",
    "plugin-timers",
];

#[derive(Clone, Copy, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum AccessMode {
    /// Only listed people; other people's messages are discarded without a trace.
    #[default]
    Allowlist,
    /// Unknown people who reach the plugin create a request the owner can approve.
    Requests,
    /// Anyone who reaches the plugin, e.g. everyone in a public support channel, with a
    /// per-person limit.
    Everyone,
}

fn per_person() -> u32 {
    5
}

/// Display labels chosen by the owner. Never used for authorization.
#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Label {
    pub id: String,
    pub name: String,
}

#[derive(Clone, Deserialize, Serialize)]
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
    pub granted_access: Value,
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
            granted_access: Value::Null,
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
    let value = db.get(KIND, &expected.id)?.ok_or_else(denied)?;
    let current: Connection = serde_json::from_value(value)?;
    if !current.enabled || current.revision != expected.revision {
        return Err(denied());
    }
    if !current.agent_id.is_empty() {
        let agent = db.get("agents", &current.agent_id)?.ok_or_else(denied)?;
        if policy(&agent) != current.granted_access {
            return Err(denied());
        }
    }
    Ok(current)
}

/// The connection's secret, for host-side injection only. Never sent to plugin code.
pub(crate) async fn credential(s: &Service, connection: &Connection) -> Result<Option<String>> {
    super::oauth::access_token(s, connection).await
}

pub(crate) async fn status(s: &Service, connection: &Connection, state: &str) {
    status_with(s, connection, state, None).await;
}

/// The instance's live state with an optional message for the owner, e.g. who the
/// package is signed in as, or why it stopped.
pub(crate) async fn status_with(
    s: &Service,
    connection: &Connection,
    state: &str,
    message: Option<&str>,
) {
    let _ = s
        .store
        .set(
            &format!("plugin-status:{}", connection.id),
            json!({ "state": state, "message": message, "updatedAt": now() }),
            None,
        )
        .await;
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
    pub settings: Map<String, Value>,
    pub secrets: Map<String, Value>,
}

impl Shared {
    /// The input with the shared values in place of any typed ones.
    pub fn apply(&self, input: &Value) -> Value {
        let mut output = input.clone();
        for (field, values) in [("settings", &self.settings), ("secrets", &self.secrets)] {
            if !output[field].is_object() {
                output[field] = json!({});
            }
            for (key, value) in values {
                output[field][key] = value.clone();
            }
        }
        output
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
    let Some(source) = shared_source(input)? else {
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
fn agent_grants(manifest: &Manifest, input: &Value) -> Result<Vec<String>> {
    let agents = match input {
        Value::Null => return Ok(Vec::new()),
        Value::Array(agents) => agents,
        _ => {
            return Err(Error::bad(
                "Select the agents allowed to use this instance.",
            ));
        }
    };
    if manifest.agent_access.is_none() && !agents.is_empty() {
        return Err(Error::bad("This plugin does not give agents access."));
    }
    if agents.len() > 100 {
        return Err(Error::bad("Select at most 100 agents."));
    }
    let mut ids = Vec::new();
    for agent in agents {
        let agent = agent.as_str().unwrap_or("");
        uuid(agent)?;
        if !ids.iter().any(|id| id == agent) {
            ids.push(agent.to_owned());
        }
    }
    Ok(ids)
}

fn labels(value: &Value) -> Vec<Label> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter(|item| item["id"].as_str().is_some_and(external_id))
        .take(100)
        .map(|item| Label {
            id: text(item, "id").to_owned(),
            name: label(text(item, "name")),
        })
        .collect()
}

pub async fn save(
    s: &Service,
    plugin_id: &str,
    input: &Value,
    existing: Option<&str>,
) -> Result<Value> {
    save_authorized(s, plugin_id, input, existing, |_| Ok(()), None).await
}

pub(crate) async fn save_authorized<F>(
    s: &Service,
    plugin_id: &str,
    input: &Value,
    existing: Option<&str>,
    authorize_management: F,
    management: Option<super::management::Authority>,
) -> Result<Value>
where
    F: Fn(&Db<'_>) -> Result<()> + Send + Sync + 'static,
{
    let package = registry::active(s, plugin_id).await?;
    let manifest = package.manifest.clone();
    let shared_input = shared(s, &manifest, plugin_id, input, existing.is_some())
        .await?
        .map(|shared| shared.apply(input));
    let input = shared_input.as_ref().unwrap_or(input);
    let expected_revision = match existing {
        Some(id) => Some(get(s, plugin_id, id).await?.revision),
        None => None,
    };
    let owner = crate::relay::task_author_policy(s).await?;
    let name = text(input, "name").trim();
    if name.is_empty() || name.chars().count() > 100 {
        return Err(Error::bad("Enter a connection name."));
    }
    let mut settings = settings(&manifest, &input["settings"])?;
    let delegates = manifest.permits_capability("conversations.submit");
    let agent_id = if delegates {
        text(input, "agentId")
    } else {
        ""
    };
    let project = input["projectId"]
        .as_str()
        .filter(|_| delegates)
        .map(str::to_owned);
    if delegates {
        uuid(agent_id)?;
    }
    if let Some(project) = &project {
        uuid(project)?;
    }
    let users = input["userIds"].as_array().cloned().unwrap_or_default();
    let access_mode = match input["accessMode"].as_str() {
        None | Some("allowlist") => AccessMode::Allowlist,
        Some("requests") => AccessMode::Requests,
        Some("everyone") if delegates => AccessMode::Everyone,
        Some(_) => return Err(Error::bad("Choose who can use this connection.")),
    };
    if users.len() > 100
        || users
            .iter()
            .any(|user| !user.as_str().is_some_and(external_id))
    {
        return Err(Error::bad("Enter valid user IDs, at most 100."));
    }
    // Approval mode may start empty: the owner approves the first request themselves.
    if delegates && users.is_empty() && access_mode == AccessMode::Allowlist {
        return Err(Error::bad(
            "Add at least one person who can use this connection.",
        ));
    }
    let user_ids = users
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let given = labels(&input["people"]);
    let people = user_ids
        .iter()
        .map(|id| {
            given
                .iter()
                .find(|label| label.id == *id)
                .cloned()
                .unwrap_or_else(|| Label {
                    id: id.clone(),
                    name: String::new(),
                })
        })
        .collect();
    let enabled = input["enabled"]
        .as_bool()
        .ok_or_else(|| Error::bad("Choose whether the connection is enabled."))?;
    let limit = input["maxMessagesPerHour"]
        .as_u64()
        .filter(|value| (1..=100).contains(value))
        .unwrap_or(20) as u32;
    let per_person = input["maxPerPersonPerHour"]
        .as_u64()
        .filter(|value| (1..=50).contains(value))
        .unwrap_or(5) as u32;
    // A device sign-in the owner completed supplies the credential instead of a paste.
    let flow = input["credentialFlow"]
        .as_str()
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
        .unwrap_or_else(|| text(input, "token"))
        .trim();
    if input.get("token").is_some_and(|value| !value.is_string())
        || token.len() > 16_384
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
    // Agents are granted from their own page; clients that omit the list keep it.
    let keep_agents = existing.is_some() && input.get("agentIds").is_none();
    let agent_ids = agent_grants(&manifest, &input["agentIds"])?;
    let saved = match existing {
        Some(_) => secret_values(s, &connection_id).await?,
        None => Map::new(),
    };
    let secret_fields = secrets(&manifest, &input["secrets"], &saved)?;
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
            management,
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
        granted_access: Value::Null,
        owner_access_id: owner.map(|policy| policy.owner.access_id),
        access_mode,
        user_ids,
        people,
        max_messages_per_hour: limit,
        max_per_person_per_hour: per_person,
        agent_ids,
        secret_keys,
        updated_at: now(),
    };
    let updating = existing.is_some();
    let one_per_agent = super::run_credentials::one_per_package(&manifest);
    let package_name = manifest.name.clone();
    let result = s
        .store
        .transaction(move |db| {
            authorize_management(db)?;
            if let Some(current) = db.get(KIND, &connection.id)? {
                if expected_revision.as_ref().is_some_and(|revision| current["revision"] != *revision) {
                    return Err(Error::conflict("This instance changed while it was being checked. Reload it and retry."));
                }
                if current["pluginId"] != connection.plugin_id {
                    return Err(Error::not_found("Plugin connection not found."));
                }
            } else if updating {
                return Err(Error::not_found("Plugin connection not found."));
            }
            if !updating && db.list(KIND)?.len() >= 64 {
                return Err(Error::bad(
                    "At most 64 plugin instances can exist on this installation.",
                ));
            }
            for agent in &connection.agent_ids {
                if db.get("agents", agent)?.is_none() {
                    return Err(Error::bad("A selected agent no longer exists."));
                }
            }
            // Runs receive the credentials of one instance per package.
            if one_per_agent && !keep_agents {
                let held = db.list(KIND)?.into_iter().any(|other| {
                    other["pluginId"] == connection.plugin_id
                        && other["id"] != connection.id.as_str()
                        && other["agentIds"].as_array().is_some_and(|ids| {
                            ids.iter()
                                .any(|id| connection.agent_ids.iter().any(|agent| id == agent))
                        })
                });
                if held {
                    return Err(Error::bad(format!(
                        "Grant one {package_name} instance per agent: its runs use that instance's credentials."
                    )));
                }
            }
            if !connection.agent_id.is_empty() {
                let agent = db
                    .get("agents", &connection.agent_id)?
                    .ok_or_else(|| Error::bad("Agent not found."))?;
                let access = policy(&agent);
                delegation_rules(db, &connection, &access)?;
                connection.granted_access = access;
            }
            if connection.access_mode != AccessMode::Requests {
                for request in db.list("plugin-requests")? {
                    if request["connectionId"] == connection.id && request["status"] == "pending" {
                        db.remove("plugin-requests", text(&request, "id"))?;
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
            if !keep_agents {
                crate::agent_grants::set_instance_agents(
                    db,
                    &connection.id,
                    &connection.agent_ids,
                )?;
            } else if let Some(current) = db.get(KIND, &connection.id)? {
                connection.agent_ids =
                    serde_json::from_value(current["agentIds"].clone()).unwrap_or_default();
            }
            let value = serde_json::to_value(&connection)?;
            db.put(KIND, &value)?;
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
    s.plugins.changed.notify_one();
    Ok(result)
}

/// Whether the delegation's project exists and its agent can open it. The owner chooses
/// how much the agent may do: any agent can answer, whoever it is delegated to.
fn delegation_rules(db: &Db<'_>, connection: &Connection, access: &Value) -> Result<()> {
    let Some(project) = &connection.project_id else {
        return Ok(());
    };
    if db.get("projects", project)?.is_none() {
        return Err(Error::bad("Project not found."));
    }
    let reachable = access["projects"]
        .as_array()
        .is_none_or(|projects| projects.iter().any(|id| id == project.as_str()));
    if !reachable {
        return Err(Error::bad("This agent can’t open the selected project."));
    }
    Ok(())
}

/// Loads an instance by ID, whatever its plugin.
pub(crate) async fn load(s: &Service, connection_id: &str) -> Result<Connection> {
    uuid(connection_id)?;
    let value = s.get(KIND, connection_id).await?;
    Ok(serde_json::from_value(value)?)
}

/// Approves the agent's saved access for a delegation that answers as this agent,
/// as saving the instance again would. Runs in the agent's save transaction.
pub(crate) fn reapprove(db: &Db<'_>, connection_id: &str, agent: &Value) -> Result<()> {
    let value = db
        .get(KIND, connection_id)?
        .ok_or_else(|| Error::bad("A plugin delegation to reapprove no longer exists."))?;
    let mut connection: Connection = serde_json::from_value(value)?;
    if connection.agent_id != text(agent, "id") {
        return Err(Error::bad(
            "This plugin delegation answers as another agent.",
        ));
    }
    let access = policy(agent);
    delegation_rules(db, &connection, &access).map_err(|error| {
        Error::bad(format!(
            "{} cannot answer as this agent with the new access: {error}",
            connection.name
        ))
    })?;
    connection.granted_access = access;
    connection.revision = id();
    connection.updated_at = now();
    db.put(KIND, &serde_json::to_value(&connection)?)?;
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
    let access = policy(agent);
    let saved = db.get("agents", agent_id)?;
    let mut effects = Vec::new();
    for value in db.list(KIND)? {
        if value["agentId"] != agent_id {
            continue;
        }
        let connection: Connection = serde_json::from_value(value)?;
        let rules = delegation_rules(db, &connection, &access)
            .err()
            .map(|error| error.to_string());
        let previous_owner = connection.owner_access_id.as_deref() != owner;
        // Suspended before this edit, so reapproving would also approve earlier changes.
        let already_suspended = previous_owner
            || saved
                .as_ref()
                .is_none_or(|saved| policy(saved) != connection.granted_access);
        effects.push(json!({
            "id": connection.id,
            "pluginId": connection.plugin_id,
            "name": connection.name,
            "enabled": connection.enabled,
            "suspended": rules.is_some()
                || access != connection.granted_access,
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
    let value = s.get(KIND, connection_id).await?;
    if value["pluginId"] != plugin_id {
        return Err(Error::not_found("Plugin connection not found."));
    }
    Ok(serde_json::from_value(value)?)
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
            db.remove(KIND, &connection_id)?;
            crate::agent_grants::forget_resource(db, crate::agent_grants::PLUGINS, &connection_id)?;
            db.delete(&format!("mcp-secret:{}", credential_key(&connection_id)))?;
            db.delete(&format!("mcp-secret:{}", secrets_key(&connection_id)))?;
            db.delete(&format!("plugin-status:{connection_id}"))?;
            for kind in OWNED {
                for value in db.list(kind)? {
                    if value["connectionId"] == connection_id {
                        db.remove(kind, text(&value, "id"))?;
                    }
                }
            }
            db.audit("plugin.connection.deleted", &json!({ "id": connection_id }))
        })
        .await?;
    s.plugins.files.forget(&forgotten_instance);
    s.plugins.changed.notify_one();
    Ok(json!({ "deleted": true }))
}

/// Owner view: configuration, live state, usage and recent exchanges. Never secrets.
pub async fn overview(s: &Service) -> Result<Vec<Value>> {
    let hour = now() / 3_600_000;
    s.store
        .read(move |db| {
            let requests = db.list("plugin-requests")?;
            let activity = db.list("plugin-activity")?;
            let mut connections = db.list(KIND)?;
            for connection in &mut connections {
                let id = text(connection, "id").to_owned();
                connection["status"] = db
                    .kv(&format!("plugin-status:{id}"))?
                    .unwrap_or(json!({ "state": "disconnected" }));
                connection["usage"] = json!({
                    "hour": db
                        .kv(&format!("plugin-rate:{id}:{hour}"))?
                        .and_then(|value| value.as_u64())
                        .unwrap_or(0)
                });
                let mut pending = requests
                    .iter()
                    .filter(|value| value["connectionId"] == id && value["status"] == "pending")
                    .cloned()
                    .collect::<Vec<_>>();
                pending.sort_by_key(|value| -value["updatedAt"].as_i64().unwrap_or(0));
                connection["requests"] = json!(pending);
                let mut recent = activity
                    .iter()
                    .filter(|value| value["connectionId"] == id)
                    .cloned()
                    .collect::<Vec<_>>();
                recent.sort_by_key(|value| -value["createdAt"].as_i64().unwrap_or(0));
                recent.truncate(8);
                connection["activity"] = json!(recent);
            }
            Ok(connections)
        })
        .await
}

/// Pausing keeps the configuration and credential; the worker stops within seconds.
pub async fn set_enabled(s: &Service, plugin_id: &str, id: &str, input: &Value) -> Result<Value> {
    set_enabled_authorized(s, plugin_id, id, input, |_| Ok(())).await
}

pub(crate) async fn set_enabled_authorized<F>(
    s: &Service,
    plugin_id: &str,
    id: &str,
    input: &Value,
    authorize_management: F,
) -> Result<Value>
where
    F: Fn(&Db<'_>) -> Result<()> + Send + Sync + 'static,
{
    get(s, plugin_id, id).await?;
    let enabled = input["enabled"]
        .as_bool()
        .ok_or_else(|| Error::bad("Choose whether the connection accepts work."))?;
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
            let mut value = db
                .get(KIND, &id)?
                .ok_or_else(|| Error::not_found("Plugin connection not found."))?;
            value["enabled"] = json!(enabled);
            value["updatedAt"] = json!(now());
            db.put(KIND, &value)?;
            db.audit(
                "plugin.connection.enabled",
                &json!({ "id": id, "enabled": enabled }),
            )?;
            Ok(value)
        })
        .await?;
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
    action: &str,
) -> Result<Value> {
    get(s, plugin_id, connection_id).await?;
    if !external_id(user_id) || !matches!(action, "approve" | "deny" | "remove") {
        return Err(Error::bad("Invalid person."));
    }
    let (connection_id, user_id, action) = (
        connection_id.to_owned(),
        user_id.to_owned(),
        action.to_owned(),
    );
    s.store
        .transaction(move |db| {
            let mut connection: Connection = serde_json::from_value(
                db.get(KIND, &connection_id)?
                    .ok_or_else(|| Error::not_found("Plugin connection not found."))?,
            )?;
            let request = db
                .list("plugin-requests")?
                .into_iter()
                .find(|value| value["connectionId"] == connection_id && value["userId"] == user_id);
            match action.as_str() {
                "approve" => {
                    let request =
                        request.ok_or_else(|| Error::not_found("Access request not found."))?;
                    if !connection.user_ids.contains(&user_id) {
                        if connection.user_ids.len() >= 100 {
                            return Err(Error::bad("At most 100 people can use one connection."));
                        }
                        connection.user_ids.push(user_id.clone());
                        connection.people.push(Label {
                            id: user_id.clone(),
                            name: label(text(&request, "name")),
                        });
                    }
                    db.remove("plugin-requests", text(&request, "id"))?;
                }
                "deny" => {
                    // A denied request stays recorded so repeated attempts stay silent.
                    let mut request =
                        request.ok_or_else(|| Error::not_found("Access request not found."))?;
                    request["status"] = json!("denied");
                    request["updatedAt"] = json!(now());
                    db.put("plugin-requests", &request)?;
                }
                _ => {
                    connection.user_ids.retain(|id| *id != user_id);
                    connection.people.retain(|person| person.id != user_id);
                    if let Some(request) = request {
                        db.remove("plugin-requests", text(&request, "id"))?;
                    }
                }
            }
            connection.updated_at = now();
            let value = serde_json::to_value(&connection)?;
            db.put(KIND, &value)?;
            db.audit(
                "plugin.connection.people",
                &json!({ "id": connection_id, "action": action }),
            )?;
            Ok(value)
        })
        .await
}
