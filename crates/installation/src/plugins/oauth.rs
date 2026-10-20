//! OAuth code capture and exchange are host operations. Anonymous callbacks can
//! only deposit one code; the initiating account or admitted external person must
//! finish through their own authorized interface.
pub(crate) mod personal;

use super::{
    capabilities, connections, registry,
    requesters::{self, Actor},
    resources,
};
use crate::{
    auth::{digest, hex_digest, token},
    config::{id, now},
    error::{Error, Result},
    service::Service,
    store::Db,
    validation::text,
};
use axum::{
    extract::Request,
    response::{IntoResponse, Response},
};
use cairn_plugin_api::{Manifest, OAuth, OAuthTokenResponse};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::{
    collections::{BTreeMap, HashMap},
    sync::{Arc, Weak},
    time::Duration,
};
use tokio::sync::{Mutex, OwnedMutexGuard};

mod external;
mod setup;

pub(crate) use setup::activate_in as activate_setup_in;
pub(crate) use setup::routes as setup_routes;

pub(crate) use external::{call, remove_event_receipts_in};

const TTL: i64 = 600_000;
const MAX_PENDING: i64 = 128;
const MAX_AUTHORIZATION_RECORDS: i64 = 2048;

const EXPIRED: &str =
    "Authorization expired or changed. Start again from the plugin configuration.";

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct Personal {
    actor: Option<Actor>,
    subject: String,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct SubjectLink {
    actor: Actor,
    authorization: String,
    revision: String,
}

fn subject_key(resource: &str, provider: &str, subject: &str) -> String {
    format!(
        "plugin-oauth-subject:{resource}:{provider}:{}",
        hex_digest(subject)
    )
}

#[derive(Default)]
pub struct Authorizations {
    locks: Mutex<HashMap<String, Weak<Mutex<()>>>>,
}

impl Authorizations {
    async fn lock(&self, key: &str) -> OwnedMutexGuard<()> {
        let lock = {
            let mut locks = self.locks.lock().await;
            locks.retain(|_, lock| lock.strong_count() > 0);
            match locks.get(key).and_then(Weak::upgrade) {
                Some(lock) => lock,
                None => {
                    let lock = Arc::new(Mutex::new(()));
                    locks.insert(key.into(), Arc::downgrade(&lock));
                    lock
                }
            }
        };
        lock.lock_owned().await
    }
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct Pending {
    #[serde(default)]
    approval: Option<registry::Approval>,
    account: String,
    owner_access: String,
    instance: String,
    instance_revision: String,
    resource: String,
    resource_revision: String,
    provider: String,
    digest: String,
    redirect_uri: String,
    client_id: String,
    scopes: Vec<String>,
    verifier: Option<String>,
    expires_at: i64,
    code: Option<String>,
    denied: bool,
    #[serde(default)]
    actor: Option<Actor>,
    #[serde(default)]
    external: Option<requesters::External>,
    #[serde(default)]
    epoch: Option<Value>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    source_event: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    start_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    setup: Option<Box<super::setup::SetupDraft>>,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct Tokens {
    access: String,
    refresh: Option<String>,
    expires_at: Option<i64>,
    scopes: Vec<String>,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct Authorization {
    revision: String,
    owner_access: String,
    instance_revision: String,
    digest: String,
    metadata: Map<String, Value>,
    requested_scopes: Vec<String>,
    tokens: Tokens,
    #[serde(default)]
    refreshing: bool,
    #[serde(default)]
    personal: Option<Personal>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    source_event: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    setup: Option<Box<super::setup::SetupDraft>>,
}

#[derive(Clone)]
pub(crate) struct Reference {
    key: String,
    revision: String,
    owner_access: String,
    plugin: String,
    digest: String,
    actor: Option<Actor>,
    subject_link: Option<(String, Value)>,
}

fn pending_key(state: &str) -> String {
    format!("plugin-oauth-pending:{}", hex_digest(state))
}

fn authorization_key(resource: &str, provider: &str, actor: Option<&Actor>) -> String {
    let base = format!("plugin-oauth:{resource}:{provider}");
    match actor {
        Some(actor) => format!("{base}:personal:{}", hex_digest(&actor.account)),
        None => base,
    }
}

fn external_key(resource: &str, provider: &str, subject: &str) -> String {
    format!(
        "plugin-oauth:{resource}:{provider}:external:{}",
        hex_digest(subject)
    )
}

fn pending_authorization_key(pending: &Pending) -> String {
    match &pending.external {
        Some(person) => external_key(&pending.resource, &pending.provider, &person.subject),
        None => authorization_key(&pending.resource, &pending.provider, pending.actor.as_ref()),
    }
}

fn epoch_key(authorization: &str) -> String {
    stored_key(&format!("{authorization}:epoch"))
}

fn setup_pending_prefix(resource: &str) -> String {
    format!("plugin-oauth-setup-pending:{resource}:")
}

fn setup_pending_index(resource: &str, key: &str) -> String {
    format!("{}{key}", setup_pending_prefix(resource))
}

fn stored_key(key: &str) -> String {
    format!("mcp-secret:{key}")
}

/// Count indexed metadata, without loading encrypted values or remote objects.
fn slots(db: &Db<'_>, prefix: &str) -> Result<i64> {
    Ok(db.0.query_row(
        "SELECT COUNT(*) FROM kv WHERE key>=?1 AND key<?2 AND (expires IS NULL OR expires>?3)",
        rusqlite::params![prefix, format!("{prefix}\u{10ffff}"), now()],
        |row| row.get(0),
    )?)
}

fn reserve(db: &Db<'_>, keys: &[String]) -> Result<()> {
    let mut additional = 0;
    for key in keys {
        if db.kv(key)?.is_none() {
            additional += 1;
        }
    }
    if additional > 0
        && slots(db, &stored_key("plugin-oauth:"))? + additional > MAX_AUTHORIZATION_RECORDS
    {
        return Err(Error::too_many_requests(
            "OAuth authorization capacity reached. Disconnect unused authorizations.",
        ));
    }
    Ok(())
}

fn fence(db: &Db<'_>, key: &str, generation: &Value, expires: Option<i64>) -> Result<()> {
    let key = epoch_key(key);
    reserve(db, std::slice::from_ref(&key))?;
    db.set(&key, generation, expires)
}

fn persist(
    db: &Db<'_>,
    key: &str,
    encrypted: &Value,
    personal: bool,
    expires: Option<i64>,
) -> Result<()> {
    let mut keys = vec![stored_key(key)];
    if personal {
        keys.push(epoch_key(key));
    }
    reserve(db, &keys)?;
    if personal {
        fence(db, key, &json!(id()), None)?;
    }
    db.set(&stored_key(key), encrypted, expires)
}

/// Release the credential before reserving its revocation generation. An existing
/// token must remain disconnectable at capacity, even if its old generation expired.
fn disconnect_authorization(db: &Db<'_>, key: &str) -> Result<()> {
    let connected = db.kv(&stored_key(key))?.is_some();
    let pending = db.kv(&epoch_key(key))?.is_some();
    db.delete(&stored_key(key))?;
    if connected || pending {
        fence(db, key, &json!(id()), Some(now() + TTL))?;
    }
    Ok(())
}

fn expired() -> Error {
    Error::conflict(EXPIRED)
}

fn provider<'a>(manifest: &'a Manifest, name: &str, collection: &str) -> Result<&'a OAuth> {
    manifest
        .oauth
        .iter()
        .find(|p| p.name == name && p.collection == collection)
        .ok_or_else(|| Error::not_found("OAuth provider not found for this resource."))
}

async fn owner(s: &Service, account: &str) -> Result<String> {
    let owner = crate::relay::task_author_policy(s)
        .await?
        .ok_or_else(|| Error::forbidden("The installation has no verified owner."))?
        .owner;
    if owner.account_id != account {
        return Err(Error::forbidden(
            "Only the current installation owner can connect this resource.",
        ));
    }
    Ok(owner.access_id)
}

async fn authority(
    s: &Service,
    provider: &OAuth,
    account: &str,
) -> Result<(String, Option<Actor>)> {
    if provider.personal.is_none() {
        return Ok((owner(s, account).await?, None));
    }
    let policy = crate::relay::task_author_policy(s)
        .await?
        .ok_or_else(|| Error::forbidden("The installation has no verified membership."))?;
    let actor = requesters::account(s, account).await?;
    Ok((policy.owner.access_id, Some(actor)))
}

fn current(db: &Db<'_>, pending: &Pending) -> Result<Value> {
    pending
        .approval
        .as_ref()
        .ok_or_else(expired)?
        .recheck_in(db)?;
    let (instance, resource) = match &pending.setup {
        Some(draft) => {
            if draft.id != pending.instance
                || draft.plugin_id.is_empty()
                || draft.digest != pending.digest
                || draft.owner_account != pending.account
                || draft.owner_access_id != pending.owner_access
                || pending.actor.is_some()
                || pending.external.is_some()
            {
                return Err(expired());
            }
            (
                json!({"revision": draft.revision, "enabled": false}),
                resources::setup_resource_in(db, draft, &pending.resource)?,
            )
        }
        None => (
            db.get(connections::KIND, &pending.instance)?
                .ok_or_else(expired)?,
            db.get(resources::KIND, &pending.resource)?
                .ok_or_else(expired)?,
        ),
    };
    if instance["revision"] != pending.instance_revision
        || resource["revision"] != pending.resource_revision
        || resource["connectionId"] != pending.instance
        || pending.expires_at <= now()
    {
        return Err(expired());
    }
    if let Some(person) = &pending.external {
        if pending.actor.is_some()
            || !pending.account.is_empty()
            || instance["enabled"] != true
            || resource["enabled"] != true
        {
            return Err(expired());
        }
        requesters::external_in(db, person)?;
    }
    if pending.actor.is_some() || pending.external.is_some() {
        let key = pending_authorization_key(pending);
        if db.kv(&epoch_key(&key))? != pending.epoch {
            return Err(expired());
        }
    }
    Ok(resource)
}

async fn redirect(s: &Service, instance: &str, direct: Option<&str>) -> Result<String> {
    Ok(super::addresses::Addresses::load(s, direct)
        .await?
        .oauth(instance))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Start {
    scopes: Vec<String>,
    direct_origin: Option<String>,
}

#[allow(clippy::too_many_arguments)]
pub async fn start(
    s: &Service,
    plugin: &str,
    instance: &str,
    collection: &str,
    resource: &str,
    name: &str,
    account: &str,
    input: &Value,
) -> Result<Value> {
    let connection = connections::get(s, plugin, instance).await?;
    let resource = resources::get(s, plugin, instance, collection, resource).await?;
    let package = registry::active(s, plugin).await?;
    let provider = provider(&package.manifest, name, collection)?;
    let (owner_access, actor) = authority(s, provider, account).await?;
    begin(
        s,
        &package,
        &connection,
        resource,
        provider,
        Initiator {
            account: account.into(),
            owner_access,
            actor,
            external: None,
            source_event: String::new(),
            setup_target: None,
        },
        input,
    )
    .await
}

struct Initiator {
    account: String,
    owner_access: String,
    actor: Option<Actor>,
    external: Option<requesters::External>,
    source_event: String,
    setup_target: Option<Arc<resources::target::Target>>,
}

async fn begin(
    s: &Service,
    package: &registry::Package,
    connection: &connections::Connection,
    resource: resources::Resource,
    provider: &OAuth,
    initiator: Initiator,
    input: &Value,
) -> Result<Value> {
    let Initiator {
        account,
        owner_access,
        actor,
        external,
        source_event,
        setup_target,
    } = initiator;
    let approved = registry::approved(s, &package.manifest.id).await?;
    if approved.package.digest != package.digest {
        return Err(expired());
    }
    let (instance, name) = (connection.id.as_str(), provider.name.as_str());
    if (actor.is_some() || external.is_some())
        && !provider.metadata.iter().any(|(key, pointer)| {
            pointer.rsplit('/').next() == Some("id")
                && resource
                    .settings
                    .get(key)
                    .and_then(Value::as_str)
                    .is_some_and(|value| !value.is_empty())
        })
    {
        return Err(Error::conflict(
            "Connect the resource identity before personal OAuth.",
        ));
    }
    let input: Start = serde_json::from_value(input.clone())
        .map_err(|_| Error::bad("Choose the OAuth scopes to authorize."))?;
    let mut scopes = input.scopes;
    let count = scopes.len();
    scopes.sort();
    scopes.dedup();
    if count != scopes.len()
        || scopes.is_empty()
        || !scopes.iter().all(|scope| provider.scopes.contains(scope))
    {
        return Err(Error::bad(
            "Choose distinct scopes declared by this provider.",
        ));
    }
    let client_id = text(&connection.settings, &provider.client_id).to_owned();
    if client_id.is_empty() || client_id.len() > 500 || client_id.chars().any(char::is_control) {
        return Err(Error::bad("Configure the OAuth client ID first."));
    }
    if let Some(secret) = &provider.client_secret {
        connections::secret_values(s, instance)
            .await?
            .get(secret)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| Error::bad("Configure the OAuth client secret first."))?;
    }
    let state = token();
    let verifier = provider.pkce.then(token);
    let configured_origin = provider
        .redirect_origin_setting
        .as_ref()
        .and_then(|key| connection.settings[key].as_str())
        .filter(|origin| !origin.is_empty());
    let redirect_uri = redirect(
        s,
        instance,
        input.direct_origin.as_deref().or(configured_origin),
    )
    .await?;
    let mut url = url::Url::parse(&provider.authorization_url).map_err(Error::internal)?;
    {
        let mut query = url.query_pairs_mut();
        query
            .append_pair("response_type", "code")
            .append_pair("client_id", &client_id)
            .append_pair("redirect_uri", &redirect_uri)
            .append_pair("state", &state)
            .append_pair(
                &provider.scope_parameter,
                &scopes.join(&provider.scope_separator),
            );
        if let Some(verifier) = &verifier {
            query
                .append_pair("code_challenge_method", "S256")
                .append_pair("code_challenge", &digest(verifier));
        }
    }
    let auth_key = match &external {
        Some(person) => external_key(&resource.id, name, &person.subject),
        None => authorization_key(&resource.id, name, actor.as_ref()),
    };
    let epoch = if actor.is_some() || external.is_some() {
        s.store.kv(&epoch_key(&auth_key)).await?
    } else {
        None
    };
    let start_url = external.as_ref().map(|_| url.to_string());
    let setup = setup_target
        .as_ref()
        .and_then(|target| target.setup_snapshot())
        .cloned()
        .map(Box::new);
    let expires_at = setup
        .as_ref()
        .map_or(now() + TTL, |draft| draft.expires_at.min(now() + TTL));
    let mut pending = Pending {
        approval: Some(approved.snapshot()),
        account,
        owner_access,
        instance: instance.into(),
        instance_revision: connection.revision.clone(),
        resource: resource.id,
        resource_revision: resource.revision,
        provider: name.into(),
        digest: package.digest.clone(),
        redirect_uri: redirect_uri.clone(),
        client_id,
        scopes,
        verifier,
        expires_at,
        code: None,
        denied: false,
        actor,
        external,
        epoch,
        source_event,
        start_url,
        setup,
    };
    let key = pending_key(&state);
    let vault = s.vault.clone();
    s.store
        .transaction(move |db| {
            if let Some(target) = setup_target {
                target.current(db)?;
            }
            current(db, &pending)?;

            let receipt = if let Some(person) = &pending.external {
                let event = db.get(super::work::EVENTS, &pending.source_event)?
                    .ok_or_else(expired)?;
                if event["connectionId"] != pending.instance
                    || event["authorId"] != person.subject
                    || event["routing"] != serde_json::to_value(&person.binding)?
                {
                    return Err(expired());
                }
                let receipt = external::receipt_key(
                    &pending.source_event,
                    &pending.resource,
                    &pending.provider,
                    &person.subject,
                );
                if db.kv(&receipt)?.is_some() {
                    return Err(Error::conflict(
                        "This event already started authorization. Use a new request.",
                    ));
                }
                Some(receipt)
            } else {
                None
            };

            let previous = pending.external.as_ref()
                .map(|person| external::flow_key(
                    &pending.resource, &pending.provider, &person.subject,
                ))
                .map(|index| db.kv(&index))
                .transpose()?
                .flatten()
                .and_then(|value| value["key"].as_str().map(str::to_owned));
            let replacing = match &previous {
                Some(key) => i64::from(db.kv(&stored_key(key))?.is_some()),
                None => 0,
            };
            if slots(db, &stored_key("plugin-oauth-pending:"))? >= MAX_PENDING + replacing {
                return Err(Error::too_many_requests("Too many OAuth requests are waiting for consent. Finish one or wait for expiry."));
            }
            if pending.actor.is_some() || pending.external.is_some() {
                if pending.external.is_some() || pending.epoch.is_none() {
                    pending.epoch = Some(json!(id()));
                }
                let expires = if db.kv(&stored_key(&auth_key))?.is_some() {
                    None
                } else {
                    Some(pending.expires_at)
                };
                fence(db, &auth_key, pending.epoch.as_ref().unwrap(), expires)?;
            }
            if let Some(person) = &pending.external {
                let index = external::flow_key(&pending.resource, &pending.provider, &person.subject);
                if let Some(previous) = previous {
                    db.delete(&stored_key(&previous))?;
                }
                db.set(&index, &json!({
                    "key": key,
                    "event": pending.source_event,
                }), Some(pending.expires_at))?;
            }

            if let Some(receipt) = receipt {
                db.set(
                    &receipt,
                    &json!({
                        "key": key,
                        "scopes": pending.scopes,
                    }),
                    None,
                )?;
            }

            if pending.setup.is_some() {
                db.set(&setup_pending_index(&pending.resource, &key), &json!(key), Some(pending.expires_at))?;
            }
            let encrypted = vault.encrypt(&key, &serde_json::to_value(&pending)?)?;
            db.set(&stored_key(&key), &encrypted, Some(pending.expires_at))
        })
        .await?;
    Ok(json!({
        "state": state,
        "url": url.as_str(),
        "redirectUri": redirect_uri,
        "expiresAt": expires_at,
    }))
}

/// No code exchange, redirect, cookie, active HTML or account authority here.
pub async fn callback(s: &Service, instance: &str, request: Request) -> Result<Response> {
    if request.method() != "GET" {
        return Err(Error::method_not_allowed("OAuth callbacks require GET."));
    }
    let raw = request.uri().query().unwrap_or("");
    if raw.len() > 16_384 {
        return Err(Error::too_large("OAuth callback is too large."));
    }
    let pairs: Vec<_> = url::form_urlencoded::parse(raw.as_bytes())
        .into_owned()
        .collect();
    let mut params = HashMap::new();
    for (key, value) in pairs {
        if params.insert(key, value).is_some() {
            return Err(Error::bad("Duplicate OAuth callback parameter."));
        }
    }
    let state = params
        .get("state")
        .filter(|v| v.len() == 43)
        .ok_or_else(expired)?;
    let key = pending_key(state);
    let vault = s.vault.clone();
    let instance = instance.to_owned();
    s.store
        .transaction(move |db| {
            let value = db.kv(&stored_key(&key))?.ok_or_else(expired)?;
            let mut pending: Pending = serde_json::from_value(vault.decrypt(&key, &value)?)?;
            current(db, &pending)?;
            if pending.instance != instance || pending.code.is_some() || pending.denied {
                return Err(expired());
            }
            let code = params.get("code");
            let denied = params.contains_key("error");
            if denied == code.is_some()
                || code.is_some_and(|code| {
                    code.is_empty() || code.len() > 10_000 || code.chars().any(char::is_control)
                })
            {
                return Err(Error::bad("Invalid OAuth callback."));
            }
            pending.code = code.cloned();
            pending.denied = denied;
            let value = vault.encrypt(&key, &serde_json::to_value(&pending)?)?;
            db.set(&stored_key(&key), &value, Some(pending.expires_at))
        })
        .await?;
    Ok((
        [
            ("content-type", "text/plain; charset=utf-8"),
            ("cache-control", "no-store"),
            ("referrer-policy", "no-referrer"),
            ("x-content-type-options", "nosniff"),
        ],
        "Authorization received. Return to the application to finish.",
    )
        .into_response())
}

async fn exchange<F, Fut>(
    s: &Service,
    manifest: &Manifest,
    provider: &OAuth,
    client_id: &str,
    instance: &str,
    fields: BTreeMap<&str, String>,
    check: F,
) -> Result<Value>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<()>>,
{
    let target = capabilities::target(s, manifest, &provider.token_url, "https")?;
    let mut request = s.plugins.http.post(target).timeout(Duration::from_secs(15));
    let mut fields = fields;
    if let Some(key) = &provider.client_secret {
        let secrets = connections::secret_values(s, instance).await?;
        let secret = secrets
            .get(key)
            .and_then(Value::as_str)
            .filter(|v| !v.is_empty())
            .ok_or_else(|| Error::forbidden("OAuth client secret is unavailable."))?;
        request = request.basic_auth(client_id, Some(secret));
    } else {
        fields.insert("client_id", client_id.into());
    }
    // Client-secret loading is preparation too. Recheck the exact consent or
    // renewal claim after every such await, immediately before provider I/O.
    check().await?;
    let response = request.form(&fields).send().await.map_err(|_| {
        Error::bad_gateway("OAuth token service is unavailable. Start authorization again.")
    })?;
    if !response.status().is_success() {
        return Err(Error::bad_gateway(
            "OAuth token exchange was refused. Start authorization again.",
        ));
    }
    let mut bytes = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk =
            chunk.map_err(|_| Error::bad_gateway("OAuth token response was interrupted."))?;
        if bytes.len() + chunk.len() > 65_536 {
            return Err(Error::too_large("OAuth token response is too large."));
        }
        bytes.extend_from_slice(&chunk);
    }
    let value: Value = serde_json::from_slice(&bytes)
        .map_err(|_| Error::bad_gateway("Invalid OAuth token response."))?;
    if value.get("error").is_some() || value.get("ok").is_some_and(|v| v != true) {
        return Err(Error::bad_gateway(
            "OAuth token exchange was refused. Start authorization again.",
        ));
    }
    Ok(value)
}

fn capture(value: &Value, mapping: &OAuthTokenResponse, requested: &[String]) -> Result<Tokens> {
    let invalid = || Error::bad_gateway("OAuth returned invalid tokens or scopes.");
    let secret = |path: &str| {
        value
            .pointer(path)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty() && s.len() <= 16_384 && !s.chars().any(char::is_control))
            .map(str::to_owned)
            .ok_or_else(invalid)
    };
    let access = secret(&mapping.access_token)?;
    let refresh = mapping
        .refresh_token
        .as_ref()
        .filter(|p| value.pointer(p).is_some_and(|v| !v.is_null()))
        .map(|p| secret(p))
        .transpose()?;
    let expires_at = mapping
        .expires_in
        .as_ref()
        .filter(|p| value.pointer(p).is_some_and(|v| !v.is_null()))
        .map(|p| {
            value
                .pointer(p)
                .and_then(Value::as_i64)
                .filter(|n| (1..=31_536_000).contains(n))
                .map(|n| now() + n * 1000)
                .ok_or_else(invalid)
        })
        .transpose()?;
    let mut scopes = match mapping
        .scope
        .as_ref()
        .and_then(|p| value.pointer(p))
        .filter(|v| !v.is_null())
    {
        Some(Value::String(scopes)) if scopes.len() <= 16_384 => scopes
            .split([',', ' '])
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect::<Vec<_>>(),
        None => requested.to_vec(),
        _ => return Err(invalid()),
    };
    scopes.sort();
    scopes.dedup();
    if scopes.is_empty()
        || scopes.len() > 256
        || !scopes.iter().all(|scope| {
            !scope.is_empty()
                && scope.len() <= 100
                && scope.bytes().all(|byte| {
                    byte.is_ascii_alphanumeric() || matches!(byte, b':' | b'.' | b'_' | b'-')
                })
        })
    {
        return Err(invalid());
    }
    Ok(Tokens {
        access,
        refresh,
        expires_at,
        scopes,
    })
}

#[allow(clippy::too_many_arguments)]
pub async fn finish(
    s: &Service,
    plugin: &str,
    instance: &str,
    collection: &str,
    resource: &str,
    name: &str,
    account: &str,
    input: &Value,
) -> Result<Value> {
    let package = registry::active(s, plugin).await?;
    let provider = provider(&package.manifest, name, collection)?.clone();
    let (owner_access, actor) = authority(s, &provider, account).await?;
    let state = text(input, "state");
    if state.len() != 43 {
        return Err(expired());
    }
    let key = pending_key(state);
    let value = s.store.kv(&stored_key(&key)).await?.ok_or_else(expired)?;
    let pending: Pending = serde_json::from_value(s.vault.decrypt(&key, &value)?)?;
    if pending.account != account
        || pending.owner_access != owner_access
        || pending.instance != instance
        || pending.resource != resource
        || pending.provider != name
        || pending.actor != actor
        || pending.external.is_some()
        || pending.setup.is_some()
    {
        return Err(Error::forbidden(
            "This authorization belongs to another requester or resource.",
        ));
    }
    finish_pending(s, &package, &provider, key, value, pending).await
}

async fn check_initiator(s: &Service, provider: &OAuth, pending: &Pending) -> Result<()> {
    if let Some(expected) = &pending.setup {
        if provider.personal.is_some() || pending.actor.is_some() || pending.external.is_some() {
            return Err(expired());
        }
        let draft = super::setup::load(s, &pending.instance).await?;
        if draft.revision != expected.revision
            || draft.owner_account != pending.account
            || draft.owner_access_id != pending.owner_access
        {
            return Err(expired());
        }
        return Ok(());
    }
    if let Some(person) = &pending.external {
        let connection = requesters::external(s, person).await?;
        if connection.owner_access_id.as_deref() != Some(&pending.owner_access)
            || provider.personal.is_none()
        {
            return Err(expired());
        }
    } else if authority(s, provider, &pending.account).await?
        != (pending.owner_access.clone(), pending.actor.clone())
    {
        return Err(expired());
    }
    Ok(())
}

async fn finish_pending(
    s: &Service,
    package: &registry::Package,
    provider: &OAuth,
    key: String,
    value: Value,
    pending: Pending,
) -> Result<Value> {
    check_initiator(s, provider, &pending).await?;
    let (plugin, instance, collection) = (
        package.manifest.id.as_str(),
        pending.instance.as_str(),
        provider.collection.as_str(),
    );
    if package.digest != pending.digest {
        return Err(expired());
    }
    let setup_target = if pending.setup.is_some() {
        Some(resources::target::Target::setup(s, plugin, instance, &pending.account).await?)
    } else {
        None
    };
    let expected = pending.clone();
    let key_claim = key.clone();
    let claimed = s
        .store
        .transaction(move |db| {
            current(db, &expected)?;
            let stored = db.kv(&stored_key(&key_claim))?.ok_or_else(expired)?;
            // Consume only the callback snapshot we inspected; another callback or
            // concurrent finish cannot replace it while exchange is in flight.
            if stored != value {
                return Err(expired());
            }
            if expected.code.is_none() && !expected.denied {
                return Ok(false);
            }
            db.delete(&stored_key(&key_claim))?;
            if expected.setup.is_some() {
                db.delete(&setup_pending_index(&expected.resource, &key_claim))?;
            }
            Ok(true)
        })
        .await?;
    if !claimed {
        return Ok(json!({ "status": "pending" }));
    }
    if pending.denied {
        return Ok(json!({ "status": "denied" }));
    }
    let mut fields = BTreeMap::from([
        ("grant_type", "authorization_code".into()),
        ("code", pending.code.clone().unwrap()),
        ("redirect_uri", pending.redirect_uri.clone()),
    ]);
    if let Some(verifier) = &pending.verifier {
        fields.insert("code_verifier", verifier.clone());
    }
    let response = exchange(
        s,
        &package.manifest,
        provider,
        &pending.client_id,
        instance,
        fields,
        || async {
            registry::require_digest(s, plugin, &pending.digest).await?;
            check_initiator(s, provider, &pending).await?;
            let expected = pending.clone();
            s.store
                .transaction(move |db| current(db, &expected).map(|_| ()))
                .await
        },
    )
    .await?;
    let tokens = capture(&response, &provider.token, &pending.scopes)?;
    let mut metadata = Map::new();
    for (key, pointer) in &provider.metadata {
        if let Some(value) = response.pointer(pointer).filter(|v| !v.is_null()) {
            let value = value
                .as_str()
                .filter(|v| !v.is_empty() && v.len() <= 256 && !v.chars().any(char::is_control))
                .ok_or_else(|| Error::bad_gateway("OAuth returned invalid resource identity."))?;
            metadata.insert(key.clone(), json!(value));
        }
    }
    if !provider.metadata.is_empty() && metadata.is_empty() {
        return Err(Error::bad_gateway(
            "OAuth returned no declared resource identity.",
        ));
    }
    if provider.personal.is_some()
        && !provider
            .metadata
            .iter()
            .any(|(key, path)| path.rsplit('/').next() == Some("id") && metadata.contains_key(key))
    {
        return Err(Error::bad_gateway(
            "Personal OAuth returned no stable resource identity.",
        ));
    }
    let personal = if let Some(personal) = &provider.personal {
        if pending.actor.is_none() && pending.external.is_none() {
            return Err(expired());
        }
        let subject = response
            .pointer(&personal.subject)
            .and_then(Value::as_str)
            .filter(|value| {
                !value.is_empty() && value.len() <= 256 && !value.chars().any(char::is_control)
            })
            .ok_or_else(|| Error::bad_gateway("OAuth returned no valid personal identity."))?;
        if pending
            .external
            .as_ref()
            .is_some_and(|person| person.subject != subject)
        {
            return Err(Error::forbidden(
                "OAuth proved a different external person.",
            ));
        }
        Some(Personal {
            actor: pending.actor.clone(),
            subject: subject.into(),
        })
    } else {
        None
    };
    check_initiator(s, provider, &pending).await?;
    if registry::active(s, plugin).await?.digest != pending.digest {
        return Err(expired());
    }
    let authorization = Authorization {
        revision: id(),
        owner_access: pending.owner_access.clone(),
        instance_revision: pending.instance_revision.clone(),
        digest: pending.digest.clone(),
        metadata: metadata.clone(),
        requested_scopes: pending.scopes.clone(),
        tokens,
        refreshing: false,
        personal,
        source_event: pending.source_event.clone(),
        setup: pending.setup.clone(),
    };
    let result = summary(&authorization);
    let auth_key = pending_authorization_key(&pending);
    let encrypted = s
        .vault
        .encrypt(&auth_key, &serde_json::to_value(&authorization)?)?;
    let mut schema = package.manifest.clone();
    schema.settings = package
        .manifest
        .resources
        .iter()
        .find(|c| c.name == collection)
        .ok_or_else(|| Error::not_found("OAuth resource collection is unavailable."))?
        .settings
        .clone();
    if pending.setup.is_some() {
        for field in &mut schema.settings {
            field["required"] = json!(false);
        }
    }
    let vault = s.vault.clone();
    let membership = crate::relay::task_author_policy(s)
        .await?
        .ok_or_else(|| Error::forbidden("The installation has no verified membership."))?;
    s.store.transaction(move |db| {
        let owner_current = membership.owner.access_id == pending.owner_access
            && (pending.actor.is_some() || pending.external.is_some()
                || membership.owner.account_id == pending.account);
        let actor_current = pending.actor.as_ref().is_none_or(|actor| {
            membership.grant(&actor.account).is_some_and(|grant| {
                actor.access_id.as_deref() == Some(grant.access_id.as_str())
            })
        });

        if !owner_current || !actor_current {
            return Err(expired());
        }

        if let Some(target) = &setup_target { target.current(db)?; }
        let mut resource = current(db, &pending)?;
        if let Some(personal) = &authorization.personal {
            // Personal authorization proves a person within an already configured
            // resource. It cannot create or change the owner's workspace identity.
            if metadata.iter().any(|(key, value)| resource["settings"].get(key) != Some(value)) {
                return Err(Error::conflict("Personal OAuth belongs to another resource identity."));
            }
            if let Some(previous) = db.kv(&stored_key(&auth_key))? {
                let previous: Authorization = serde_json::from_value(vault.decrypt(&auth_key, &previous)?)?;
                if previous.personal.is_some_and(|p| p.subject != personal.subject) {
                    return Err(Error::conflict("Disconnect before linking another personal identity."));
                }
            }
            if let Some(actor) = &personal.actor {
                let subject_key = subject_key(&pending.resource, &pending.provider, &personal.subject);
                if let Some(previous) = db.kv(&subject_key)? {
                    let previous: SubjectLink = serde_json::from_value(previous)?;
                    if previous.actor != *actor {
                        let still_member = membership.grant(&previous.actor.account)
                            .is_some_and(|grant| previous.actor.access_id.as_deref() == Some(grant.access_id.as_str()));
                        if still_member {
                            return Err(Error::conflict("This external identity is already linked to another installation account."));
                        }
                        // A new proof may replace a removed membership. Its old
                        // callback and token must not remain usable after relinking.
                        fence(db, &previous.authorization, &json!(id()), Some(now() + TTL))?;
                        db.delete(&stored_key(&previous.authorization))?;
                    }
                }
                let link = SubjectLink {actor: actor.clone(), authorization: auth_key.clone(), revision: id()};
                db.set(&subject_key, &serde_json::to_value(link)?, None)?;
            }
            persist(db, &auth_key, &encrypted, true, None)?;
            return Ok(());
        }
        for (key, value) in metadata {
            let previous = resource["settings"].get(&key).and_then(Value::as_str);
            let different_identity = previous.is_some_and(|old| !old.is_empty() && old != value.as_str().unwrap());
            if different_identity {
                return Err(Error::conflict(
                    "The authorized identity does not match this resource. Create another resource.",
                ));
            }
            resource["settings"][key] = value;
        }

        resource["settings"] = connections::settings(&schema, &resource["settings"])
            .map_err(|_| Error::bad_gateway("OAuth returned invalid resource identity."))?;
        if serde_json::to_vec(&resource["settings"])?.len() > 65_536 {
            return Err(Error::bad_gateway("OAuth returned excessive resource metadata."));
        }

        resource["revision"] = json!(id());
        resource["updatedAt"] = json!(now());
        if let Some(target) = setup_target {
            target.put(db, &resource)?;
        } else {
            db.put(resources::KIND, &resource)?;
        }
        persist(db, &auth_key, &encrypted, false, pending.setup.as_ref().map(|draft| draft.expires_at))?;
        Ok(())
    }).await?;
    Ok(result)
}

fn summary(authorization: &Authorization) -> Value {
    let mut result = json!({
        "status": if authorization.refreshing {"reconnect"} else {"authorized"},
        "scopes": authorization.tokens.scopes,
        "requestedScopes": authorization.requested_scopes,
        "expiresAt": authorization.tokens.expires_at,
    });
    if let Some(personal) = &authorization.personal {
        result["subject"] = json!(personal.subject);
    }
    result
}

#[allow(clippy::too_many_arguments)]
pub async fn status(
    s: &Service,
    plugin: &str,
    instance: &str,
    collection: &str,
    resource: &str,
    name: &str,
    account: &str,
) -> Result<Value> {
    let resource = resources::get(s, plugin, instance, collection, resource).await?;
    let connection = connections::get(s, plugin, instance).await?;
    let package = registry::active(s, plugin).await?;
    let provider = provider(&package.manifest, name, collection)?;
    let (_, actor) = authority(s, provider, account).await?;
    let Some(value) = s
        .vault
        .get(&authorization_key(&resource.id, name, actor.as_ref()))
        .await?
    else {
        return Ok(json!({ "status": "disconnected" }));
    };
    let authorization: Authorization = serde_json::from_value(value)?;
    let owner_access = crate::relay::task_author_policy(s)
        .await?
        .map(|policy| policy.owner.access_id);
    Ok(status_summary(
        &authorization,
        &connection,
        &resource,
        &package.digest,
        owner_access.as_deref(),
        actor.as_ref(),
    ))
}

fn status_summary(
    authorization: &Authorization,
    connection: &connections::Connection,
    resource: &resources::Resource,
    digest: &str,
    owner_access: Option<&str>,
    actor: Option<&Actor>,
) -> Value {
    let mut summary = summary(authorization);
    if authorization.setup.is_some()
        || owner_access != Some(authorization.owner_access.as_str())
        || authorization
            .personal
            .as_ref()
            .and_then(|personal| personal.actor.as_ref())
            != actor
        || authorization.instance_revision != connection.revision
        || authorization.digest != digest
        || authorization
            .metadata
            .iter()
            .any(|(key, value)| resource.settings.get(key) != Some(value))
        || (authorization
            .tokens
            .expires_at
            .is_some_and(|expiry| expiry <= now())
            && authorization.tokens.refresh.is_none())
    {
        summary["status"] = json!("reconnect");
    }
    summary
}

pub async fn revoke(
    s: &Service,
    plugin: &str,
    instance: &str,
    collection: &str,
    resource: &str,
    name: &str,
    account: &str,
) -> Result<Value> {
    let package = registry::active(s, plugin).await?;
    let provider = provider(&package.manifest, name, collection)?;
    let (_, actor) = authority(s, provider, account).await?;
    status(s, plugin, instance, collection, resource, name, account).await?;
    let resource = resources::get(s, plugin, instance, collection, resource).await?;
    let key = authorization_key(&resource.id, name, actor.as_ref());
    let vault = s.vault.clone();
    let resource_id = resource.id.clone();
    let provider_name = name.to_owned();
    s.store
        .transaction(move |db| {
            let mut current = db.get(resources::KIND, &resource.id)?.ok_or_else(expired)?;
            if current["revision"] != resource.revision {
                return Err(expired());
            }
            if actor.is_some() {
                if let Some(value) = db.kv(&stored_key(&key))? {
                    let authorization: Authorization =
                        serde_json::from_value(vault.decrypt(&key, &value)?)?;
                    if let Some(personal) = authorization.personal {
                        external::disconnect_subject(
                            db,
                            &resource_id,
                            &provider_name,
                            &personal.subject,
                        )?;
                    }
                }
                return disconnect_authorization(db, &key);
            }
            // Also invalidate exchanges that have already consumed their pending
            // state but have not yet returned from the provider.
            current["revision"] = json!(id());
            current["updatedAt"] = json!(now());
            db.put(resources::KIND, &current)?;
            db.delete(&stored_key(&key))
        })
        .await?;
    Ok(json!({ "status": "disconnected" }))
}

pub(crate) async fn credential(
    s: &Service,
    context: &capabilities::Context,
    resource: &str,
    name: &str,
) -> Result<(String, Reference)> {
    let resource = resources::reference(s, context, resource).await?;
    let selected = &resource.resource;
    let provider = provider(&context.manifest, name, &selected.collection)?;
    let mut subject_link = None;
    let mut external_subject = None;
    let mut selected_key = None;
    let actor = if provider.personal.is_some() {
        let requester = context
            .resource_grant
            .as_ref()
            .and_then(|grant| grant.requester.as_ref())
            .ok_or_else(|| Error::forbidden("Personal OAuth requires a verified run requester."))?;
        requesters::personal(s, requester).await?;
        match &requester.principal {
            requesters::Principal::Account(actor) => Some(actor.clone()),
            requesters::Principal::External(external) => {
                let selected =
                    external::selection(s, &context.connection, &resource, external, name)
                        .await?
                        .ok_or_else(|| {
                            Error::forbidden(
                                "Connect this external person's OAuth authorization first.",
                            )
                        })?;
                subject_link = selected.link;
                selected_key = Some(selected.key);
                external_subject = Some(external.subject.clone());
                selected.actor
            }
        }
    } else {
        None
    };
    let key = selected_key.unwrap_or_else(|| authorization_key(&selected.id, name, actor.as_ref()));
    let _lock = s.plugins.oauth.lock(&key).await;
    let value = s
        .vault
        .get(&key)
        .await?
        .ok_or_else(|| Error::forbidden("Connect this resource through OAuth first."))?;
    let mut authorization: Authorization = serde_json::from_value(value)?;
    if authorization.setup.is_some() {
        return Err(Error::forbidden(
            "This OAuth authorization has not been activated.",
        ));
    }
    if provider.personal.is_some() != authorization.personal.is_some()
        || authorization
            .personal
            .as_ref()
            .and_then(|personal| personal.actor.as_ref())
            != actor.as_ref()
    {
        return Err(Error::forbidden(
            "This OAuth authorization belongs to another requester.",
        ));
    }
    if let Some(subject) = external_subject
        && !authorization
            .personal
            .as_ref()
            .is_some_and(|p| p.subject == subject)
    {
        return Err(Error::forbidden(
            "OAuth did not prove this external requester.",
        ));
    }
    let owner_access = crate::relay::task_author_policy(s)
        .await?
        .map(|p| p.owner.access_id);
    if owner_access.as_deref() != Some(&authorization.owner_access)
        || authorization.instance_revision != context.connection.revision
        || authorization.digest
            != registry::active(s, &context.connection.plugin_id)
                .await?
                .digest
        || authorization
            .metadata
            .iter()
            .any(|(key, value)| selected.settings.get(key) != Some(value))
    {
        return Err(Error::forbidden(
            "The OAuth authorization changed. Reconnect this resource.",
        ));
    }
    let package = registry::approved(s, &context.connection.plugin_id).await?;
    if package.package.digest != authorization.digest {
        return Err(Error::forbidden("The OAuth package approval changed."));
    }
    let approval = package.snapshot();
    if authorization.refreshing {
        return Err(Error::forbidden(
            "The last OAuth renewal was interrupted. Reconnect this resource.",
        ));
    }
    if let Some(grant) = &context.resource_grant {
        super::grants::recheck(s, grant).await?;
    }
    if authorization
        .tokens
        .expires_at
        .is_some_and(|expiry| expiry <= now() + 60_000)
    {
        let refresh =
            authorization.tokens.refresh.take().ok_or_else(|| {
                Error::forbidden("The OAuth token expired. Reconnect this resource.")
            })?;
        let expected = authorization.revision.clone();
        authorization.revision = id();
        authorization.refreshing = true;
        let encrypted = s
            .vault
            .encrypt(&key, &serde_json::to_value(&authorization)?)?;
        let fence = authorization.revision.clone();
        let vault = s.vault.clone();
        let stored = key.clone();
        s.store
            .transaction(move |db| {
                let value = db.kv(&stored_key(&stored))?.ok_or_else(expired)?;
                if vault.decrypt(&stored, &value)?["revision"] != expected {
                    return Err(expired());
                }
                db.set(&stored_key(&stored), &encrypted, None)
            })
            .await?;
        let check = Reference {
            key: key.clone(),
            revision: fence.clone(),
            owner_access: authorization.owner_access.clone(),
            plugin: context.connection.plugin_id.clone(),
            digest: authorization.digest.clone(),
            actor: actor.clone(),
            subject_link: subject_link.clone(),
        };
        let fields = BTreeMap::from([
            ("grant_type", "refresh_token".into()),
            ("refresh_token", refresh),
        ]);
        let response = exchange(
            s,
            &context.manifest,
            provider,
            text(&context.connection.settings, &provider.client_id),
            &context.connection.id,
            fields,
            || async {
                recheck(s, &check).await?;
                if let Some(grant) = &context.resource_grant {
                    super::grants::recheck(s, grant).await?;
                }
                let connection = context.connection.clone();
                let resource = resource.clone();
                let grant = context.resource_grant.clone();
                let reference = check.clone();
                let approval = approval.clone();
                let vault = s.vault.clone();
                s.store
                    .transaction(move |db| {
                        approval.recheck_in(db)?;
                        resources::recheck_in(db, &connection, &resource)?;
                        if let Some(grant) = &grant {
                            super::grants::recheck_in(db, grant)?;
                        }
                        recheck_in(&vault, db, &reference)
                    })
                    .await
            },
        )
        .await?;
        let mapping = provider
            .refresh_token_response
            .as_ref()
            .unwrap_or(&provider.token);
        let tokens = capture(&response, mapping, &authorization.tokens.scopes)?;
        if tokens.refresh.is_none() {
            return Err(Error::bad_gateway(
                "OAuth did not return the next refresh token. Reconnect this resource.",
            ));
        }
        for (key, pointer) in &provider.metadata {
            if let Some(value) = response.pointer(pointer).filter(|v| !v.is_null())
                && authorization.metadata.get(key) != Some(value)
            {
                return Err(Error::forbidden(
                    "OAuth renewed a different resource identity.",
                ));
            }
        }
        if let (Some(declared), Some(personal)) = (&provider.personal, &authorization.personal)
            && let Some(subject) = response.pointer(&declared.subject).filter(|v| !v.is_null())
            && subject.as_str() != Some(&personal.subject)
        {
            return Err(Error::forbidden(
                "OAuth renewed a different personal identity.",
            ));
        }
        resources::recheck(s, context, &resource).await?;
        recheck(s, &check).await?;
        authorization.tokens = tokens;
        authorization.refreshing = false;
        authorization.revision = id();
        let encrypted = s
            .vault
            .encrypt(&key, &serde_json::to_value(&authorization)?)?;
        let vault = s.vault.clone();
        let stored = key.clone();
        s.store
            .transaction(move |db| {
                let value = db.kv(&stored_key(&stored))?.ok_or_else(expired)?;
                if vault.decrypt(&stored, &value)?["revision"] != fence {
                    return Err(expired());
                }
                db.set(&stored_key(&stored), &encrypted, None)
            })
            .await?;
    }
    resources::recheck(s, context, &resource).await?;
    Ok((
        authorization.tokens.access,
        Reference {
            key,
            revision: authorization.revision,
            owner_access: authorization.owner_access,
            plugin: context.connection.plugin_id.clone(),
            digest: authorization.digest,
            actor,
            subject_link,
        },
    ))
}

pub(crate) async fn recheck(s: &Service, reference: &Reference) -> Result<()> {
    if registry::active(s, &reference.plugin).await?.digest != reference.digest {
        return Err(Error::forbidden("The OAuth package approval changed."));
    }
    let owner_access = crate::relay::task_author_policy(s)
        .await?
        .map(|p| p.owner.access_id);
    if owner_access.as_deref() != Some(&reference.owner_access) {
        return Err(Error::forbidden(
            "OAuth no longer belongs to the current owner.",
        ));
    }
    if let Some(actor) = &reference.actor
        && requesters::account(s, &actor.account).await? != *actor
    {
        return Err(Error::forbidden(
            "The personal OAuth requester's access changed.",
        ));
    }
    let reference = reference.clone();
    let vault = s.vault.clone();
    s.store
        .transaction(move |db| recheck_in(&vault, db, &reference))
        .await
}

pub(crate) fn recheck_in(
    vault: &crate::vault::Vault,
    db: &Db<'_>,
    reference: &Reference,
) -> Result<()> {
    if let Some((key, value)) = &reference.subject_link
        && db.kv(key)?.as_ref() != Some(value)
    {
        return Err(Error::forbidden(
            "The external identity was unlinked during the request.",
        ));
    }
    if !vault
        .get_in(db, &reference.key)?
        .is_some_and(|value| value["revision"] == reference.revision)
    {
        return Err(Error::forbidden(
            "OAuth was revoked or replaced during the request.",
        ));
    }
    Ok(())
}

pub(crate) fn remove_resource(db: &Db<'_>, resource: &str) -> Result<()> {
    for (index, key) in db.keys(&setup_pending_prefix(resource))? {
        if let Some(key) = key.as_str() {
            db.delete(&stored_key(key))?;
        }
        db.delete(&index)?;
    }
    // Provider names are bounded, but their definitions may have been removed in
    // a package update. Remove the vault prefix rather than just current providers.
    for (key, _) in db.keys(&stored_key(&format!("plugin-oauth:{resource}:")))? {
        db.delete(&key)?;
    }
    for (key, _) in db.keys(&format!("plugin-oauth-subject:{resource}:"))? {
        db.delete(&key)?;
    }
    for (key, value) in db.keys(&format!("plugin-oauth-external-flow:{resource}:"))? {
        if let Some(pending) = value["key"].as_str() {
            db.delete(&stored_key(pending))?;
        }
        db.delete(&key)?;
    }
    Ok(())
}
