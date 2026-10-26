//! Instance forms completed by the package (`instanceForm`): while the owner edits,
//! its `form` handler fills dynamic choices, derives hidden values, reports errors and
//! adds guidance; on save it checks the instance once more. It runs in a one-shot
//! worker with the draft's own credential and secrets, before anything is saved.
use super::{
    connections::{self, Connection, secret_values},
    registry::{self, Package},
    scope::{self, Scope},
};
use crate::{
    config::id,
    error::{Error, Result},
    service::Service,
    validation::text,
};
use cairn_plugin_api::{Manifest, validate_form_result};
use serde_json::{Map, Value, json};
use std::sync::Arc;

/// Unsaved values a draft's handler may use through `auth`. Never sent to scripts.
pub struct Draft {
    pub prepared: Vec<Connection>,
    pub management: Option<super::management::Authority>,
    pub credential: Option<String>,
    pub secrets: Map<String, Value>,
}

impl Draft {
    pub(crate) fn authorize(&self, db: &crate::store::Db<'_>) -> Result<()> {
        for expected in &self.prepared {
            connections::check_revision(db, expected)?;
        }
        if let Some(authority) = &self.management {
            authority.authorize(db)?;
        }
        Ok(())
    }
}

/// Settings as typed so far: valid values only, nothing required yet.
fn values(manifest: &Manifest, input: &Value) -> Value {
    let mut output = Map::new();
    for field in manifest
        .settings
        .iter()
        .filter(|field| field["type"] != "secret")
    {
        let key = text(field, "key");
        if let Some(value) = input.get(key).filter(|value| !value.is_null())
            && let Ok(value) = connections::setting(field, value)
        {
            output.insert(key.to_owned(), value);
        }
    }
    Value::Object(output)
}

/// Typed secrets over saved ones, ignoring invalid input.
fn secrets(manifest: &Manifest, input: &Value, saved: Map<String, Value>) -> Map<String, Value> {
    let mut output = saved;
    for key in manifest.secret_settings() {
        if let Some(value) = input[key].as_str().map(str::trim).filter(|value| {
            !value.is_empty() && value.len() <= 16_384 && !value.chars().any(char::is_control)
        }) {
            output.insert(key.to_owned(), json!(value));
        }
    }
    output
}

async fn run(
    s: &Service,
    package: &Package,
    instance: Option<&str>,
    settings: Value,
    draft: Draft,
    changed: Option<&str>,
    submit: bool,
) -> Result<Value> {
    let secret_keys = draft.secrets.keys().cloned().collect::<Vec<_>>();
    let previous = match instance {
        Some(id) => s.store.plugin_instance(id).await?.map(|v| v.settings),
        None => None,
    };
    let payload = json!({
        "previous": previous,
        "values": settings,
        "credential": draft.credential.is_some(),
        "secrets": secret_keys,
        "instance": instance,
        "changed": changed,
        "submit": submit,
    });
    let connection = Connection::draft(
        &instance.map_or_else(id, str::to_owned),
        &package.manifest.id,
        settings,
        secret_keys,
    );
    let management = draft.management.clone();
    let draft = Arc::new(draft);
    let scope = Scope::Draft(Box::new(connection), draft.clone());
    let scope = match management {
        Some(authority) => Scope::Managed(Box::new(scope), authority),
        None => scope,
    };
    let result = scope::invoke(s, package, "form", payload, &scope).await?;
    s.store.read(move |db| draft.authorize(db)).await?;
    validate_form_result(&package.manifest, &result).map_err(Error::bad_gateway)?;
    Ok(result)
}

/// `POST /api/plugins/{id}/form`:
/// `{ instance?, settings, secrets?, token? | credentialFlow?, changed? }`
/// → `{ options, values, errors, view }`.
pub async fn form(
    s: &Service,
    plugin_id: &str,
    input: &cairn_contracts::PluginFormInput,
) -> Result<cairn_contracts::PluginFormResult> {
    form_managed(s, plugin_id, input, None).await
}

pub(crate) async fn form_managed(
    s: &Service,
    plugin_id: &str,
    command: &cairn_contracts::PluginFormInput,
    management: Option<super::management::Authority>,
) -> Result<cairn_contracts::PluginFormResult> {
    let package = registry::active(s, plugin_id).await?;
    let manifest = &package.manifest;
    if !manifest.instance_form {
        return Err(Error::not_found("This plugin has no instance form."));
    }
    let source = command.shared_from.map(|id| id.to_string());
    let shared = connections::shared_from(
        s,
        manifest,
        plugin_id,
        source.as_deref(),
        command.instance.is_some(),
    )
    .await?;
    let mut settings_input = serde_json::to_value(&command.settings)?;
    let mut secrets_input = serde_json::to_value(&command.secrets)?;
    if let Some(shared) = &shared {
        shared.apply_values(&mut settings_input, &mut secrets_input);
    }
    let mut prepared = shared
        .as_ref()
        .map(|shared| shared.source.clone())
        .into_iter()
        .collect::<Vec<_>>();
    let requested_instance = command.instance.map(|id| id.to_string());
    let instance = match requested_instance.as_deref() {
        Some(id) => {
            let current = connections::get(s, plugin_id, id).await?;
            if command
                .revision
                .as_ref()
                .is_some_and(|revision| *revision != current.revision)
            {
                return Err(Error::conflict(
                    "This instance changed. Reload it before checking the form.",
                ));
            }
            prepared.push(current);
            Some(id.to_owned())
        }
        None => None,
    };
    let signed_in = match command
        .credential_flow
        .as_deref()
        .filter(|flow| !flow.is_empty())
    {
        Some(flow) => Some(
            super::oauth::flow_credential(
                s,
                plugin_id,
                flow,
                instance.as_deref(),
                &values(manifest, &settings_input),
                false,
            )
            .await?,
        ),
        None => None,
    };
    let token = signed_in
        .as_ref()
        .and_then(|v| v["token"].as_str())
        .unwrap_or_else(|| command.token.as_deref().unwrap_or_default())
        .trim();
    if token.len() > 16_384 || token.chars().any(|c| c.is_whitespace() || c.is_control()) {
        let label = manifest
            .credential
            .as_ref()
            .map_or("credential", |credential| credential.label.as_str());
        return Ok(cairn_contracts::PluginFormResult {
            errors: std::collections::BTreeMap::from([(
                "credential".into(),
                format!("Enter a valid {label}."),
            )]),
            ..Default::default()
        });
    }
    let credential = match (&instance, token.is_empty()) {
        (_, false) => Some(token.to_owned()),
        (Some(id), true) => {
            super::oauth::draft_token(
                s,
                &connections::get(s, plugin_id, id).await?,
                management.as_ref(),
            )
            .await?
        }
        (None, true) => None,
    };
    let saved = match &instance {
        Some(id) => secret_values(s, id).await?,
        None => Map::new(),
    };
    let draft = Draft {
        prepared,
        management,
        credential: credential.filter(|_| manifest.credential.is_some()),
        secrets: secrets(manifest, &secrets_input, saved),
    };
    let changed = command.changed.as_deref().filter(|key| {
        *key == "credential" || manifest.settings.iter().any(|field| field["key"] == *key)
    });
    let settings = values(manifest, &settings_input);
    let result = run(
        s,
        &package,
        instance.as_deref(),
        settings,
        draft,
        changed,
        false,
    )
    .await?;
    Ok(serde_json::from_value(json!({
        "options": result.get("options").cloned().unwrap_or_else(|| json!({})),
        "values": result.get("values").cloned().unwrap_or_else(|| json!({})),
        "errors": result.get("errors").cloned().unwrap_or_else(|| json!({})),
        "view": result.get("view").cloned().unwrap_or_else(|| json!([])),
    }))?)
}

/// The package's last word before an instance is saved: derived values are applied
/// and checked again, and any error refuses the save with the package's message.
pub(crate) async fn submit(
    s: &Service,
    package: &Package,
    instance: &str,
    settings: Value,
    draft: Draft,
) -> Result<Value> {
    let existing = s.store.plugin_instance(instance).await?.map(|_| instance);
    let result = run(s, package, existing, settings.clone(), draft, None, true).await?;
    if let Some(errors) = result["errors"]
        .as_object()
        .filter(|errors| !errors.is_empty())
    {
        // The form-wide message first, then fields in the order the owner sees them.
        let message = errors
            .get("form")
            .or_else(|| {
                package
                    .manifest
                    .settings
                    .iter()
                    .find_map(|field| errors.get(text(field, "key")))
            })
            .or_else(|| errors.values().next())
            .and_then(Value::as_str)
            .unwrap_or("Check the form.");
        return Err(Error::bad(message));
    }
    let mut merged = settings;
    if let (Some(target), Some(values)) = (merged.as_object_mut(), result["values"].as_object()) {
        for (key, value) in values {
            if value.is_null() {
                target.remove(key);
            } else {
                target.insert(key.clone(), value.clone());
            }
        }
    }
    connections::settings(&package.manifest, &merged)
}
