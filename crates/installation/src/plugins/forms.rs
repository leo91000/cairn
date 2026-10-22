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
use cairn_plugin_api::{FormLookup, Manifest, validate_form_fields, validate_lookup_result};
use serde_json::{Map, Value, json};
use std::sync::Arc;

/// Unsaved values a draft's handler may use through `auth`. Never sent to scripts.
pub struct Draft {
    pub management: Option<super::management::Authority>,
    pub credential: Option<String>,
    pub secrets: Map<String, Value>,
    pub setup: Option<Box<super::setup::SetupDraft>>,
    pub urls: Option<Value>,
    pub(crate) owner: Option<Arc<super::operations::Owner>>,
    pub(crate) resource: Option<Arc<super::resources::forms::Form>>,
    pub(crate) root: Option<Arc<super::resources::target::Target>>,
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
        || token.len() > 16384
        || token.chars().any(|c| c.is_whitespace() || c.is_control())
    {
        return Err(Error::bad(
            "Enter a valid credential declared by the plugin.",
        ));
    }
    Ok(Some(token.to_owned()))
}

/// Settings as typed so far: valid values only, nothing required yet.
pub(super) fn partial_values(fields: &[Value], input: &Value) -> Result<Value> {
    let mut output = Map::new();
    for field in fields.iter().filter(|field| field["type"] != "secret") {
        let key = text(field, "key");
        if let Some(value) = input.get(key).filter(|value| !value.is_null())
            && let Ok(value) = connections::setting(field, value)
        {
            output.insert(key.to_owned(), value);
        }
    }
    connections::bounded_settings(Value::Object(output))
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

/// A preview/choice lookup cannot weaken the final submit evaluation.
pub(super) struct Evaluation {
    changed: Option<String>,
    lookup: Option<FormLookup>,
    submit: bool,
}

impl Evaluation {
    pub(super) fn preview(fields: &[Value], globals: &[&str], input: &Value) -> Result<Self> {
        let lookup = FormLookup::parse(fields, input.get("lookup")).map_err(Error::bad)?;
        if lookup.is_some() && !input["changed"].is_null() {
            return Err(Error::bad(
                "A choice lookup is separate from a changed-field evaluation.",
            ));
        }
        let changed = input["changed"]
            .as_str()
            .filter(|key| globals.contains(key) || fields.iter().any(|field| field["key"] == *key))
            .map(str::to_owned);
        Ok(Self {
            changed,
            lookup,
            submit: false,
        })
    }

    pub(super) fn submit() -> Self {
        Self {
            changed: None,
            lookup: None,
            submit: true,
        }
    }

    pub(super) fn finish(
        &self,
        fields: &[Value],
        globals: &[&str],
        mut result: Value,
    ) -> Result<Value> {
        if let Some(lookup) = &self.lookup {
            validate_lookup_result(fields, globals, lookup, &result).map_err(Error::bad_gateway)?;
            if result.is_null() {
                result = json!({});
            }
            // Host-owned echo distinguishes lookups from older hosts' full forms.
            result["lookup"] = serde_json::to_value(lookup)?;
        } else {
            validate_form_fields(fields, globals, &result).map_err(Error::bad_gateway)?;
        }
        Ok(result)
    }

    pub(super) fn fields(&self) -> Value {
        json!({ "changed": self.changed, "lookup": self.lookup, "submit": self.submit })
    }
}

/// Keep optional pagination while preserving ordinary response shapes.
pub(super) fn response(result: &Value) -> Value {
    let mut response = json!({
        "options": result.get("options").cloned().unwrap_or_else(|| json!({})),
        "values": result.get("values").cloned().unwrap_or_else(|| json!({})),
        "errors": result.get("errors").cloned().unwrap_or_else(|| json!({})),
        "view": result.get("view").cloned().unwrap_or_else(|| json!([])),
    });
    for key in ["pages", "lookup"] {
        if let Some(value) = result.get(key) {
            response[key] = value.clone();
        }
    }
    response
}

async fn run(
    s: &Service,
    package: &Package,
    instance: Option<&str>,
    settings: Value,
    draft: Draft,
    evaluation: &Evaluation,
) -> Result<Value> {
    let secret_keys = draft.secrets.keys().cloned().collect::<Vec<_>>();
    let previous = match instance {
        Some(id) => s
            .store
            .get(connections::KIND, id)
            .await?
            .map(|v| v["settings"].clone()),
        None => None,
    };
    let mut payload = json!({
        "previous": previous,
        "values": settings,
        "credential": draft.credential.is_some(),
        "secrets": secret_keys,
        "instance": instance,
        "setup": draft.setup.as_ref().map(|setup| json!({
            "revision": setup.revision,
            "expiresAt": setup.expires_at,
            "urls": draft.urls,
        })),
    });
    payload
        .as_object_mut()
        .unwrap()
        .extend(evaluation.fields().as_object().unwrap().clone());
    let connection = Connection::draft(
        &instance.map_or_else(id, str::to_owned),
        &package.manifest.id,
        settings,
        secret_keys,
    );
    let management = draft.management.clone();
    let scope = Scope::Draft(Box::new(connection), Arc::new(draft));
    let scope = match management {
        Some(authority) => Scope::Managed(Box::new(scope), authority),
        None => scope,
    };
    let result = scope::invoke(s, package, "form", payload, &scope).await?;
    evaluation.finish(&package.manifest.settings, &["credential", "form"], result)
}

/// `POST /api/plugins/{id}/form`:
/// `{ instance?, settings, secrets?, token? | credentialFlow?, changed? }`
/// → `{ options, values, errors, view }`.
pub async fn form(s: &Service, plugin_id: &str, input: &Value) -> Result<Value> {
    let owner = Arc::new(super::operations::Owner::current(s).await?);
    form_owned(s, plugin_id, input, owner).await
}

pub(crate) async fn form_managed(
    s: &Service,
    plugin_id: &str,
    input: &Value,
    management: Option<super::management::Authority>,
) -> Result<Value> {
    let owner = Arc::new(super::operations::Owner::current(s).await?);
    form_authorized(s, plugin_id, input, owner, management).await
}

pub(crate) async fn form_owned(
    s: &Service,
    plugin_id: &str,
    input: &Value,
    owner: Arc<super::operations::Owner>,
) -> Result<Value> {
    form_authorized(s, plugin_id, input, owner, None).await
}

async fn form_authorized(
    s: &Service,
    plugin_id: &str,
    input: &Value,
    owner: Arc<super::operations::Owner>,
    management: Option<super::management::Authority>,
) -> Result<Value> {
    let package = registry::active(s, plugin_id).await?;
    let manifest = &package.manifest;
    if !manifest.instance_form {
        return Err(Error::not_found("This plugin has no instance form."));
    }
    let evaluation = Evaluation::preview(&manifest.settings, &["credential"], input)?;
    let root = match input["instance"].as_str() {
        Some(id) => Some(super::resources::target::Target::active(s, plugin_id, id).await?),
        None => None,
    };
    let instance = root.as_ref().map(|root| root.instance().to_owned());
    let signed_in = match input["credentialFlow"]
        .as_str()
        .filter(|flow| !flow.is_empty())
    {
        Some(flow) => Some(
            super::oauth::flow_credential(
                s,
                plugin_id,
                flow,
                instance.as_deref(),
                &partial_values(&manifest.settings, &input["settings"])?,
                false,
            )
            .await?,
        ),
        None => None,
    };
    let token = signed_in
        .as_ref()
        .and_then(|v| v["token"].as_str())
        .unwrap_or_else(|| text(input, "token"))
        .trim();
    if token.len() > 16_384 || token.chars().any(|c| c.is_whitespace() || c.is_control()) {
        let label = manifest
            .credential
            .as_ref()
            .map_or("credential", |credential| credential.label.as_str());
        return evaluation
            .finish(
                &manifest.settings,
                &["credential", "form"],
                json!({ "errors": { "credential": format!("Enter a valid {label}.") } }),
            )
            .map(|result| response(&result));
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
        management,
        credential: credential.filter(|_| manifest.credential.is_some()),
        secrets: secrets(manifest, &input["secrets"], saved),
        setup: None,
        urls: None,
        owner: Some(owner),
        resource: None,
        root,
    };
    let settings = partial_values(&manifest.settings, &input["settings"])?;
    let result = run(
        s,
        &package,
        instance.as_deref(),
        settings,
        draft,
        &evaluation,
    )
    .await?;
    Ok(response(&result))
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
    let settings = partial_values(
        &package.manifest.settings,
        input.get("settings").unwrap_or(&setup.settings),
    )?;
    let evaluation = Evaluation::preview(&package.manifest.settings, &["credential"], input)?;
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
        management: None,
        credential,
        secrets: secrets(&package.manifest, &input["secrets"], saved),
        setup: Some(Box::new(setup)),
        urls: Some(urls),
        owner: None,
        resource: None,
        root: None,
    };
    run(s, package, Some(&instance), settings, draft, &evaluation).await
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
    let result = run(
        s,
        package,
        existing,
        settings.clone(),
        draft,
        &Evaluation::submit(),
    )
    .await?;
    require_valid(&package.manifest.settings, &result)?;
    let mut merged = settings;
    merge_values(&mut merged, &result);
    connections::settings(&package.manifest, &merged)
}

pub(crate) fn require_valid(settings: &[Value], result: &Value) -> Result<()> {
    if let Some(errors) = result["errors"]
        .as_object()
        .filter(|errors| !errors.is_empty())
    {
        // The form-wide message first, then fields in the order the owner sees them.
        let message = errors
            .get("form")
            .or_else(|| {
                settings
                    .iter()
                    .find_map(|field| errors.get(text(field, "key")))
            })
            .or_else(|| errors.values().next())
            .and_then(Value::as_str)
            .unwrap_or("Check the form.");
        return Err(Error::bad(message));
    }
    Ok(())
}

pub(crate) fn merge_values(settings: &mut Value, result: &Value) {
    if settings.is_null() {
        *settings = json!({});
    }
    if let (Some(target), Some(values)) = (settings.as_object_mut(), result["values"].as_object()) {
        for (key, value) in values {
            if value.is_null() {
                target.remove(key);
            } else {
                target.insert(key.clone(), value.clone());
            }
        }
    }
}
