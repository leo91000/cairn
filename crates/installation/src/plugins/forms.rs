//! Instance forms completed by the package (`instanceForm`): while the owner edits,
//! its `form` handler fills dynamic choices, derives hidden values, reports errors and
//! adds guidance; on save it checks the instance once more. It runs in a one-shot
//! worker with the draft's own credential and secrets, before anything is saved.
use super::{
    connections::{self, Connection, credential_key, secret_values},
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
    pub credential: Option<String>,
    pub secrets: Map<String, Value>,
    pub setup: Option<Box<super::setup::SetupDraft>>,
    pub urls: Option<Value>,
    pub(crate) owner: Option<Arc<super::operations::Owner>>,
}

/// A credential entered during setup; incomplete drafts may omit it.
pub(crate) fn typed_credential(manifest: &Manifest, input: &Value) -> Result<Option<String>> {
    if input.is_null() {
        return Ok(None);
    }
    let token = input
        .as_str()
        .ok_or_else(|| Error::bad("Enter a valid credential."))?
        .trim();
    if token.is_empty() {
        return Ok(None);
    }
    if manifest.credential.is_none()
        || token.len() > 4096
        || token.chars().any(|c| c.is_whitespace() || c.is_control())
    {
        return Err(Error::bad(
            "Enter a valid credential declared by the plugin.",
        ));
    }
    Ok(Some(token.to_owned()))
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
    let payload = json!({
        "values": settings,
        "credential": draft.credential.is_some(),
        "secrets": secret_keys,
        "instance": instance,
        "changed": changed,
        "submit": submit,
        "setup": draft.setup.as_ref().map(|setup| json!({
            "revision": setup.revision,
            "expiresAt": setup.expires_at,
            "urls": draft.urls,
        })),
    });
    let connection = Connection::draft(
        &instance.map_or_else(id, str::to_owned),
        &package.manifest.id,
        settings,
        secret_keys,
    );
    let scope = Scope::Draft(Box::new(connection), Arc::new(draft));
    let result = scope::invoke(s, package, "form", payload, &scope).await?;
    validate_form_result(&package.manifest, &result).map_err(Error::bad_gateway)?;
    Ok(result)
}

/// `POST /api/plugins/{id}/form`: `{ instance?, settings, secrets?, token?, changed? }`
/// → `{ options, values, errors, view }`.
pub async fn form(s: &Service, plugin_id: &str, input: &Value) -> Result<Value> {
    let owner = Arc::new(super::operations::Owner::current(s).await?);
    form_owned(s, plugin_id, input, owner).await
}

pub(crate) async fn form_owned(
    s: &Service,
    plugin_id: &str,
    input: &Value,
    owner: Arc<super::operations::Owner>,
) -> Result<Value> {
    let package = registry::active(s, plugin_id).await?;
    let manifest = &package.manifest;
    if !manifest.instance_form {
        return Err(Error::not_found("This plugin has no instance form."));
    }
    let instance = match input["instance"].as_str() {
        Some(id) => Some(connections::get(s, plugin_id, id).await?.id),
        None => None,
    };
    let token = text(input, "token").trim();
    if token.len() > 4096 || token.chars().any(|c| c.is_whitespace() || c.is_control()) {
        let label = manifest
            .credential
            .as_ref()
            .map_or("credential", |credential| credential.label.as_str());
        return Ok(json!({ "errors": { "credential": format!("Enter a valid {label}.") } }));
    }
    let credential = match (&instance, token.is_empty()) {
        (_, false) => Some(token.to_owned()),
        (Some(id), true) => s
            .vault
            .get(&credential_key(id))
            .await?
            .map(|value| text(&value, "token").to_owned()),
        (None, true) => None,
    };
    let saved = match &instance {
        Some(id) => secret_values(s, id).await?,
        None => Map::new(),
    };
    let draft = Draft {
        credential: credential.filter(|_| manifest.credential.is_some()),
        secrets: secrets(manifest, &input["secrets"], saved),
        setup: None,
        urls: None,
        owner: Some(owner),
    };
    let changed = input["changed"].as_str().filter(|key| {
        *key == "credential" || manifest.settings.iter().any(|field| field["key"] == *key)
    });
    let settings = values(manifest, &input["settings"]);
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
    Ok(json!({
        "options": result.get("options").cloned().unwrap_or_else(|| json!({})),
        "values": result.get("values").cloned().unwrap_or_else(|| json!({})),
        "errors": result.get("errors").cloned().unwrap_or_else(|| json!({})),
        "view": result.get("view").cloned().unwrap_or_else(|| json!([])),
    }))
}

/// Owner-only form for a persisted, still-inert setup draft. Typed values are
/// temporary; saving them requires the draft's separate revision-checked PATCH.
pub(crate) async fn setup_form(
    s: &Service,
    package: &Package,
    setup: super::setup::SetupDraft,
    input: &Value,
) -> Result<Value> {
    if !package.manifest.instance_form {
        return Err(Error::not_found("This plugin has no instance form."));
    }
    let saved = secret_values(s, &setup.id).await?;
    let settings = values(
        &package.manifest,
        input.get("settings").unwrap_or(&setup.settings),
    );
    let changed = input["changed"].as_str().filter(|key| {
        *key == "credential"
            || package
                .manifest
                .settings
                .iter()
                .any(|field| field["key"] == *key)
    });
    let instance = setup.id.clone();
    let urls = setup_urls(s, &package.manifest, &instance, input).await?;
    let credential = match typed_credential(&package.manifest, &input["token"])? {
        Some(token) => Some(token),
        None => s
            .vault
            .get(&credential_key(&instance))
            .await?
            .map(|value| text(&value, "token").to_owned()),
    };
    let draft = Draft {
        credential,
        secrets: secrets(&package.manifest, &input["secrets"], saved),
        setup: Some(Box::new(setup)),
        urls: Some(urls),
        owner: None,
    };
    run(s, package, Some(&instance), settings, draft, changed, false).await
}

pub(crate) async fn setup_urls(
    s: &Service,
    manifest: &Manifest,
    instance: &str,
    input: &Value,
) -> Result<Value> {
    let direct = match input.get("directOrigin").filter(|value| !value.is_null()) {
        None => None,
        Some(Value::String(origin)) => Some(origin.as_str()),
        Some(_) => return Err(Error::bad("Enter an HTTPS installation origin.")),
    };
    let addresses = super::addresses::Addresses::load(s, direct).await?;
    let webhooks = manifest
        .webhooks
        .iter()
        .map(|endpoint| {
            (
                endpoint.name.clone(),
                json!(addresses.webhook(instance, &endpoint.name)),
            )
        })
        .collect::<Map<String, Value>>();
    Ok(json!({"webhooks": webhooks, "oauthCallback": addresses.oauth(instance)}))
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
    let existing = if draft.setup.is_some() {
        Some(instance)
    } else {
        s.store
            .get(connections::KIND, instance)
            .await?
            .map(|_| instance)
    };
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
