//! OAuth code capture and exchange are host operations. Anonymous callbacks can
//! only deposit one code; the initiating account must finish through the account API.
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

const TTL: i64 = 600_000;

const EXPIRED: &str =
    "Authorization expired or changed. Start again from the plugin configuration.";

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct Personal {
    actor: Actor,
    subject: String,
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
    epoch: Option<Value>,
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
}

pub(crate) struct Reference {
    key: String,
    revision: String,
    owner_access: String,
    plugin: String,
    digest: String,
    actor: Option<Actor>,
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

fn epoch_key(authorization: &str) -> String {
    stored_key(&format!("{authorization}:epoch"))
}

fn stored_key(key: &str) -> String {
    format!("mcp-secret:{key}")
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
    let instance = db
        .get(connections::KIND, &pending.instance)?
        .ok_or_else(expired)?;
    let resource = db
        .get(resources::KIND, &pending.resource)?
        .ok_or_else(expired)?;
    if instance["revision"] != pending.instance_revision
        || resource["revision"] != pending.resource_revision
        || resource["connectionId"] != pending.instance
        || pending.expires_at <= now()
    {
        return Err(expired());
    }
    if pending.actor.is_some() {
        let key = authorization_key(&pending.resource, &pending.provider, pending.actor.as_ref());
        if db.kv(&epoch_key(&key))? != pending.epoch {
            return Err(expired());
        }
    }
    Ok(resource)
}

async fn redirect(s: &Service, instance: &str, direct: Option<&str>) -> Result<String> {
    if let Some(origin) = direct {
        let url = url::Url::parse(origin)
            .map_err(|_| Error::bad("Enter an HTTPS installation origin."))?;
        if url.scheme() != "https"
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || origin.trim_end_matches('/') != url.origin().ascii_serialization()
        {
            return Err(Error::bad(
                "Enter an HTTPS installation origin without a path or query.",
            ));
        }
        return Ok(format!(
            "{}/api/plugin-oauth/{instance}/callback",
            url.origin().ascii_serialization()
        ));
    }
    let (origin, installation) = crate::relay::beacon_address(&s.config.data_dir)
        .await?
        .ok_or_else(|| Error::unavailable("Claim this installation before connecting a plugin."))?;
    Ok(format!(
        "{origin}/api/public/installations/{installation}/plugin-oauth/{instance}/callback"
    ))
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
    if actor.is_some()
        && !provider.metadata.keys().any(|key| {
            resource
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
    let redirect_uri = redirect(s, instance, input.direct_origin.as_deref()).await?;
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
    let auth_key = authorization_key(&resource.id, name, actor.as_ref());
    let epoch = if actor.is_some() {
        s.store.kv(&epoch_key(&auth_key)).await?
    } else {
        None
    };
    let pending = Pending {
        account: account.into(),
        owner_access,
        instance: instance.into(),
        instance_revision: connection.revision,
        resource: resource.id,
        resource_revision: resource.revision,
        provider: name.into(),
        digest: package.digest,
        redirect_uri: redirect_uri.clone(),
        client_id,
        scopes,
        verifier,
        expires_at: now() + TTL,
        code: None,
        denied: false,
        actor,
        epoch,
    };
    let key = pending_key(&state);
    let encrypted = s.vault.encrypt(&key, &serde_json::to_value(&pending)?)?;
    s.store
        .transaction(move |db| {
            current(db, &pending)?;
            db.set(&stored_key(&key), &encrypted, Some(pending.expires_at))
        })
        .await?;
    Ok(
        json!({"state": state, "url": url.as_str(), "redirectUri": redirect_uri, "expiresAt": now() + TTL}),
    )
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
        "Authorization received. Return to Cairn to finish.",
    )
        .into_response())
}

async fn exchange(
    s: &Service,
    manifest: &Manifest,
    provider: &OAuth,
    client_id: &str,
    instance: &str,
    fields: BTreeMap<&str, String>,
) -> Result<Value> {
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
    {
        return Err(Error::forbidden(
            "This authorization belongs to another requester or resource.",
        ));
    }
    if package.digest != pending.digest {
        return Err(expired());
    }
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
            Ok(true)
        })
        .await?;
    if !claimed {
        return Ok(json!({"status": "pending"}));
    }
    if pending.denied {
        return Ok(json!({"status": "denied"}));
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
        &provider,
        &pending.client_id,
        instance,
        fields,
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
    let personal = match (&provider.personal, &actor) {
        (Some(personal), Some(actor)) => {
            let subject = response
                .pointer(&personal.subject)
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty() && s.len() <= 256 && !s.chars().any(char::is_control))
                .ok_or_else(|| Error::bad_gateway("OAuth returned no valid personal identity."))?;
            Some(Personal {
                actor: actor.clone(),
                subject: subject.into(),
            })
        }
        (None, None) => None,
        _ => return Err(expired()),
    };
    if authority(s, &provider, account).await? != (pending.owner_access.clone(), actor)
        || registry::active(s, plugin).await?.digest != pending.digest
    {
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
    };
    let result = summary(&authorization);
    let auth_key = authorization_key(resource, name, pending.actor.as_ref());
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
    let vault = s.vault.clone();
    s.store.transaction(move |db| {
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
            db.set(&epoch_key(&auth_key), &json!(id()), None)?;
            db.set(&stored_key(&auth_key), &encrypted, None)?;
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
        db.put(resources::KIND, &resource)?;
        db.set(&stored_key(&auth_key), &encrypted, None)?;
        Ok(())
    }).await?;
    Ok(result)
}

fn summary(authorization: &Authorization) -> Value {
    let mut result = json!({
        "status": if authorization.refreshing {"reconnect"} else {"authorized"},
        "scopes": authorization.tokens.scopes, "requestedScopes": authorization.requested_scopes,
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
        return Ok(json!({"status": "disconnected"}));
    };
    let authorization: Authorization = serde_json::from_value(value)?;
    let owner_access = crate::relay::task_author_policy(s)
        .await?
        .map(|policy| policy.owner.access_id);
    let mut summary = summary(&authorization);
    if owner_access.as_deref() != Some(&authorization.owner_access)
        || authorization
            .personal
            .as_ref()
            .map(|personal| &personal.actor)
            != actor.as_ref()
        || authorization.instance_revision != connection.revision
        || authorization.digest != package.digest
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
    Ok(summary)
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
    s.store
        .transaction(move |db| {
            let mut current = db.get(resources::KIND, &resource.id)?.ok_or_else(expired)?;
            if current["revision"] != resource.revision {
                return Err(expired());
            }
            if actor.is_some() {
                // A per-person generation fences code exchanges without changing
                // the shared resource or anyone else's authorization.
                db.set(&epoch_key(&key), &json!(id()), None)?;
                return db.delete(&stored_key(&key));
            }
            // Also invalidate exchanges that have already consumed their pending
            // state but have not yet returned from the provider.
            current["revision"] = json!(id());
            current["updatedAt"] = json!(now());
            db.put(resources::KIND, &current)?;
            db.delete(&stored_key(&key))
        })
        .await?;
    Ok(json!({"status": "disconnected"}))
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
    let actor = if provider.personal.is_some() {
        let requester = context
            .resource_grant
            .as_ref()
            .and_then(|grant| grant.requester.as_ref())
            .ok_or_else(|| Error::forbidden("Personal OAuth requires a verified run requester."))?;
        requesters::recheck(s, requester).await?;
        Some(requester.actor.clone())
    } else {
        None
    };
    let key = authorization_key(&selected.id, name, actor.as_ref());
    let _lock = s.plugins.oauth.lock(&key).await;
    let value = s
        .vault
        .get(&key)
        .await?
        .ok_or_else(|| Error::forbidden("Connect this resource through OAuth first."))?;
    let mut authorization: Authorization = serde_json::from_value(value)?;
    if authorization
        .personal
        .as_ref()
        .map(|personal| &personal.actor)
        != actor.as_ref()
    {
        return Err(Error::forbidden(
            "This OAuth authorization belongs to another requester.",
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
        resources::recheck(s, context, &resource).await?;
        if let Some(grant) = &context.resource_grant {
            super::grants::recheck(s, grant).await?;
        }
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
        let check = Reference {
            key: key.clone(),
            revision: fence.clone(),
            owner_access: authorization.owner_access.clone(),
            plugin: context.connection.plugin_id.clone(),
            digest: authorization.digest.clone(),
            actor: actor.clone(),
        };
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
    let current = s.vault.get(&reference.key).await?;
    if !current.is_some_and(|value| value["revision"] == reference.revision) {
        return Err(Error::forbidden(
            "OAuth was revoked or replaced during the request.",
        ));
    }
    Ok(())
}

pub(crate) fn remove_resource(db: &Db<'_>, resource: &str) -> Result<()> {
    // Provider names are bounded, but their definitions may have been removed in
    // a package update. Remove the vault prefix rather than just current providers.
    for (key, _) in db.keys(&stored_key(&format!("plugin-oauth:{resource}:")))? {
        db.delete(&key)?;
    }
    Ok(())
}
