//! Host-owned authorization-code sign-in and refresh for any plugin. Package code
//! sees only the account's API responses; neither it nor a client receives tokens.
use super::{capabilities::target, connections, registry};
use crate::{
    auth::{digest, hex_digest, token},
    config::{id, now},
    error::{Error, Result},
    service::Service,
    validation::text,
};
use cairn_plugin_api::OAuth;
use serde_json::{Map, Value, json};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, Weak},
    time::Duration,
};

const TTL: i64 = 10 * 60 * 1000;

#[derive(Clone)]
struct Flow {
    id: String,
    plugin: String,
    package_digest: String,
    binding: String,
    instance: Option<String>,
    revision: Option<String>,
    settings: Value,
    url: String,
    redirect: String,
    verifier: String,
    client_id: String,
    client_secret: Option<String>,
    scopes: Vec<String>,
    expires: i64,
    state: &'static str,
    credential: Option<Value>,
    message: Option<String>,
}

impl Flow {
    fn public(&self) -> Value {
        json!({
            "id": self.id,
            "pluginId": self.plugin,
            "state": self.state,
            "verificationUri": if self.state == "pending" { self.url.as_str() } else { "" },
            "userCode": "",
            "expiresAt": self.expires,
            "message": self.message,
        })
    }
}

#[derive(Default)]
pub struct Flows {
    flows: Mutex<HashMap<String, Flow>>,
    refresh: Mutex<HashMap<String, Weak<tokio::sync::Mutex<()>>>>,
}

impl Flows {
    pub(crate) fn expire(&self) {
        Self::prune(&mut self.flows.lock().unwrap());
    }

    fn prune(flows: &mut HashMap<String, Flow>) {
        flows.retain(|_, flow| flow.expires > now());
    }

    pub(crate) fn lock(&self, instance: &str) -> Arc<tokio::sync::Mutex<()>> {
        let mut locks = self.refresh.lock().unwrap();
        locks.retain(|_, lock| lock.strong_count() > 0);
        if let Some(lock) = locks.get(instance).and_then(Weak::upgrade) {
            return lock;
        }
        let lock = Arc::new(tokio::sync::Mutex::new(()));
        locks.insert(instance.to_owned(), Arc::downgrade(&lock));
        lock
    }

    pub(crate) fn consume(&self, plugin: &str, flow: &str) {
        let mut flows = self.flows.lock().unwrap();
        if flows.get(flow).is_some_and(|flow| flow.plugin == plugin) {
            flows.remove(flow);
        }
    }

    pub fn status(&self, plugin: &str, flow: &str, binding: &str) -> Result<Value> {
        let mut flows = self.flows.lock().unwrap();
        Self::prune(&mut flows);
        let flow = flows
            .get(flow)
            .filter(|f| f.plugin == plugin && f.binding == hex_digest(binding))
            .ok_or_else(expired)?;
        Ok(flow.public())
    }

    pub fn cancel(&self, plugin: &str, flow: &str, binding: &str) -> Result<Value> {
        self.status(plugin, flow, binding)?;
        self.flows.lock().unwrap().remove(flow);
        Ok(json!({ "cancelled": true }))
    }

    /// A completed sign-in is usable only for the instance and OAuth settings it
    /// started with. A failed save may retry; a successful save consumes it.
    pub fn credential(
        &self,
        plugin: &str,
        flow: &str,
        instance: Option<&str>,
        settings: &Value,
        oauth: &OAuth,
        take: bool,
    ) -> Result<Value> {
        let mut flows = self.flows.lock().unwrap();
        Self::prune(&mut flows);
        let current = flows
            .get(flow)
            .filter(|f| {
                f.plugin == plugin && f.state == "completed" && f.instance.as_deref() == instance
            })
            .ok_or_else(expired)?;
        if current.settings[&oauth.client_id_setting] != settings[&oauth.client_id_setting]
            || oauth
                .scope_settings
                .keys()
                .any(|k| current.settings[k] != settings[k])
        {
            return Err(Error::bad("The sign-in settings changed. Sign in again."));
        }
        let value = current.credential.clone().ok_or_else(expired)?;
        if take {
            flows.remove(flow);
        }
        Ok(value)
    }
}

fn expired() -> Error {
    Error::not_found("The sign-in expired or belongs to another instance. Sign in again.")
}

pub async fn flow_credential(
    s: &Service,
    plugin: &str,
    flow: &str,
    instance: Option<&str>,
    settings: &Value,
    take: bool,
) -> Result<Value> {
    let package = registry::active(s, plugin).await?;
    if let Some(oauth) = package
        .manifest
        .credential
        .as_ref()
        .and_then(|c| c.oauth.as_ref())
    {
        {
            let flows = s.plugins.oauth_flows.flows.lock().unwrap();
            let current = flows.get(flow).ok_or_else(expired)?;
            if current.package_digest != package.digest {
                return Err(expired());
            }
        }
        if !take && let Some(instance) = instance {
            let current = connections::get(s, plugin, instance).await?;
            let flows = s.plugins.oauth_flows.flows.lock().unwrap();
            if flows.get(flow).and_then(|f| f.revision.as_ref()) != Some(&current.revision) {
                return Err(expired());
            }
        }
        // Compare only the client and scope fields, applying their defaults as at
        // sign-in. A live form draft can omit other fields the owner just cleared.
        let fields = package
            .manifest
            .settings
            .iter()
            .filter(|field| {
                let key = text(field, "key");
                key == oauth.client_id_setting || oauth.scope_settings.contains_key(key)
            })
            .cloned()
            .collect::<Vec<_>>();
        let supplied = fields
            .iter()
            .filter_map(|field| {
                let key = text(field, "key");
                settings
                    .get(key)
                    .map(|value| (key.to_owned(), value.clone()))
            })
            .collect::<Map<_, _>>();
        let normalized = connections::settings_with(&fields, &Value::Object(supplied))?;
        s.plugins
            .oauth_flows
            .credential(plugin, flow, instance, &normalized, oauth, take)
    } else {
        Ok(json!({ "token": s.plugins.device_flows.token(plugin, flow, take)? }))
    }
}

pub async fn callback_url(s: &Service) -> Result<String> {
    let (origin, installation) = crate::relay::beacon_address(&s.config.data_dir)
        .await?
        .ok_or_else(|| Error::unavailable("Claim this installation before connecting a plugin."))?;
    Ok(format!(
        "{origin}/installations/{installation}/plugins/oauth/callback"
    ))
}

fn setting(settings: &Value, key: &str) -> Result<String> {
    let value = text(settings, key).trim();
    if value.is_empty() || value.len() > 2000 || value.chars().any(char::is_control) {
        return Err(Error::bad(
            "Enter the OAuth client settings before signing in.",
        ));
    }
    Ok(value.to_owned())
}

/// Owner starts a fresh flow. PKCE, client secrets and tokens remain in the host.
pub async fn start(s: &Service, plugin: &str, input: &Value, binding: &str) -> Result<Value> {
    let package = registry::active(s, plugin).await?;
    let oauth = package
        .manifest
        .credential
        .as_ref()
        .and_then(|c| c.oauth.as_ref())
        .ok_or_else(|| Error::not_found("This plugin has no OAuth sign-in."))?;
    let instance = input["instance"].as_str();
    let existing = match instance {
        Some(instance) => Some(connections::get(s, plugin, instance).await?),
        None => None,
    };
    let settings = if input["settings"].is_object() {
        input["settings"].clone()
    } else {
        existing
            .as_ref()
            .map_or_else(|| json!({}), |c| c.settings.clone())
    };
    let settings = connections::settings(&package.manifest, &settings)?;
    let client_id = setting(&settings, &oauth.client_id_setting)?;
    let client_secret = match &oauth.client_secret_setting {
        Some(key) => {
            let saved = match instance {
                Some(id) => connections::secret_values(s, id).await?,
                None => Map::default(),
            };
            let value = input["secrets"][key]
                .as_str()
                .filter(|v| !v.trim().is_empty())
                .or_else(|| saved.get(key).and_then(Value::as_str))
                .ok_or_else(|| Error::bad("Enter the OAuth client secret before signing in."))?;
            if value.len() > 16_384 || value.chars().any(char::is_control) {
                return Err(Error::bad("Invalid OAuth client secret."));
            }
            Some(value.to_owned())
        }
        None => None,
    };
    let mut scopes = oauth.scopes.clone();
    for (key, values) in &oauth.scope_settings {
        if settings[key] == true {
            scopes.extend(values.clone());
        }
    }
    scopes.sort();
    scopes.dedup();
    let verifier = token();
    let state = token();
    let redirect = callback_url(s).await?;
    // Validate without applying test routing: this URL is opened by the owner.
    target(s, &package.manifest, &oauth.authorization_url, "https")?;
    let mut url = url::Url::parse(&oauth.authorization_url)
        .map_err(|_| Error::bad("Invalid authorization URL."))?;
    url.query_pairs_mut().extend_pairs([
        ("response_type", "code"),
        ("client_id", client_id.as_str()),
        ("redirect_uri", redirect.as_str()),
        ("state", state.as_str()),
        ("code_challenge", digest(&verifier).as_str()),
        ("code_challenge_method", "S256"),
        ("scope", scopes.join(" ").as_str()),
    ]);
    url.query_pairs_mut()
        .extend_pairs(oauth.authorization_parameters.iter());
    let flow = Flow {
        id: state.clone(),
        plugin: plugin.to_owned(),
        package_digest: package.digest,
        binding: hex_digest(binding),
        instance: instance.map(str::to_owned),
        revision: existing.map(|c| c.revision),
        settings,
        url: url.into(),
        redirect,
        verifier,
        client_id,
        client_secret,
        scopes,
        expires: now() + TTL,
        state: "pending",
        credential: None,
        message: None,
    };
    let public = flow.public();
    let mut flows = s.plugins.oauth_flows.flows.lock().unwrap();
    Flows::prune(&mut flows);
    if flows.len() >= 16 {
        return Err(Error::too_many_requests("Too many pending sign-ins."));
    }
    flows.insert(state, flow);
    Ok(public)
}

async fn post(
    s: &Service,
    manifest: &cairn_plugin_api::Manifest,
    endpoint: &str,
    fields: &[(String, String)],
) -> Result<Value> {
    let endpoint = target(s, manifest, endpoint, "https")?;
    let encoded = serde_urlencoded::to_string(fields).map_err(Error::internal)?;
    let response = s
        .plugins
        .http
        .post(endpoint)
        .header("content-type", "application/x-www-form-urlencoded")
        .body(encoded)
        .timeout(Duration::from_secs(15))
        .send()
        .await
        .map_err(|_| Error::bad_gateway("The sign-in service is unavailable."))?;
    let status = response.status();
    let mut body = Vec::new();
    let mut response = response;
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| Error::bad_gateway("The sign-in response was interrupted."))?
    {
        if body.len() + chunk.len() > 65_536 {
            return Err(Error::bad_gateway("Invalid sign-in response."));
        }
        body.extend_from_slice(&chunk);
    }
    let json: Value = serde_json::from_slice(&body)
        .map_err(|_| Error::bad_gateway("Invalid sign-in response."))?;
    if !status.is_success() || json.get("error").is_some() {
        return Err(Error::unauthorized(
            "The service refused authorization. Reconnect this account.",
        ));
    }
    Ok(json)
}

fn credentials(
    response: &Value,
    flow: &Flow,
    oauth: &OAuth,
    previous: Option<&Value>,
) -> Result<Value> {
    if !text(response, "token_type").eq_ignore_ascii_case("bearer") {
        return Err(Error::bad_gateway(
            "The service returned an unsupported token type.",
        ));
    }
    let access = text(response, "access_token");
    if access.is_empty() || access.len() > 16_384 || access.chars().any(char::is_whitespace) {
        return Err(Error::bad_gateway(
            "The service returned no valid access token.",
        ));
    }
    let refresh = response["refresh_token"]
        .as_str()
        .filter(|v| !v.is_empty())
        .or_else(|| previous.and_then(|p| p["oauth"]["refreshToken"].as_str()));
    if refresh.is_some_and(|v| v.len() > 16_384 || v.chars().any(char::is_whitespace)) {
        return Err(Error::bad_gateway("Invalid refresh token."));
    }
    if oauth.require_refresh_token && refresh.is_none() {
        return Err(Error::bad(
            "The service did not grant offline access. Reconnect and approve consent.",
        ));
    }
    if let Some(granted) = response["scope"].as_str() {
        let granted = granted.split_whitespace().collect::<Vec<_>>();
        if flow
            .scopes
            .iter()
            .any(|scope| !granted.contains(&scope.as_str()))
        {
            return Err(Error::bad(
                "Some requested permissions were declined. Reconnect or turn off those permissions.",
            ));
        }
    }
    Ok(json!({
        "token": access,
        "oauth": {
            "tokenUrl": oauth.token_url, "clientId": flow.client_id,
            "clientSecret": flow.client_secret, "refreshToken": refresh,
            "expiresAt": now() + response["expires_in"].as_i64().unwrap_or(3600).clamp(1, 31_536_000) * 1000,
            "scope": response["scope"].as_str().map_or_else(|| flow.scopes.join(" "), str::to_owned),
        },
    }))
}

pub async fn callback(s: &Service, parameters: &Value, binding: &str) -> Result<Value> {
    let state = text(parameters, "state");
    let flow = {
        let mut flows = s.plugins.oauth_flows.flows.lock().unwrap();
        Flows::prune(&mut flows);
        let flow = flows
            .get_mut(state)
            .filter(|f| f.state == "pending" && f.binding == hex_digest(binding))
            .ok_or_else(expired)?;
        // Reserve the one-shot callback before any network operation.
        flow.state = "exchanging";
        flow.clone()
    };
    let result = async {
        let package = registry::active(s, &flow.plugin).await?;
        if package.digest != flow.package_digest {
            return Err(expired());
        }
        if let Some(instance) = &flow.instance {
            let current = connections::get(s, &flow.plugin, instance).await?;
            if Some(&current.revision) != flow.revision.as_ref() {
                return Err(expired());
            }
        }
        if parameters.get("error").is_some() {
            return Err(Error::bad("Sign-in was declined. Try again when ready."));
        }
        let code = text(parameters, "code");
        if code.is_empty() || code.len() > 10_000 {
            return Err(Error::bad("Invalid authorization response."));
        }
        let oauth = package
            .manifest
            .credential
            .as_ref()
            .and_then(|c| c.oauth.as_ref())
            .ok_or_else(expired)?;
        let mut fields = vec![
            ("grant_type".into(), "authorization_code".into()),
            ("code".into(), code.to_owned()),
            ("client_id".into(), flow.client_id.clone()),
            ("redirect_uri".into(), flow.redirect.clone()),
            ("code_verifier".into(), flow.verifier.clone()),
        ];
        if let Some(secret) = &flow.client_secret {
            fields.push(("client_secret".into(), secret.clone()));
        }
        let response = post(s, &package.manifest, &oauth.token_url, &fields).await?;
        credentials(&response, &flow, oauth, None)
    }
    .await;
    let mut flows = s.plugins.oauth_flows.flows.lock().unwrap();
    let Some(current) = flows.get_mut(state) else {
        return Err(expired());
    };
    current.state = if result.is_ok() {
        "completed"
    } else {
        "failed"
    };
    current.verifier.clear();
    current.credential = result.as_ref().ok().cloned();
    current.message = result.as_ref().err().map(ToString::to_string);
    result?;
    Ok(json!({ "pluginId": flow.plugin, "flowId": flow.id, "state": "completed" }))
}

/// Refresh once per instance, preserving rotated refresh tokens and refusing to
/// overwrite an edited, removed, disabled or newly revoked instance after I/O.
fn authorize_token(
    db: &crate::store::Db<'_>,
    expected: &connections::Connection,
    editing: bool,
) -> Result<()> {
    if expected.enabled && !editing {
        connections::authorize(db, expected)?;
        return Ok(());
    }
    let current = db
        .get(connections::KIND, &expected.id)?
        .ok_or_else(expired)?;
    if current["revision"] != expected.revision || (expected.enabled && current["enabled"] != true)
    {
        return Err(Error::forbidden(
            "This account changed or was disabled. Retry the operation.",
        ));
    }
    Ok(())
}

pub async fn access_token(
    s: &Service,
    connection: &connections::Connection,
) -> Result<Option<String>> {
    access_token_managed(s, connection, None).await
}

pub(crate) async fn access_token_managed(
    s: &Service,
    connection: &connections::Connection,
    management: Option<&super::management::Authority>,
) -> Result<Option<String>> {
    access_token_with(s, connection, management, false).await
}

pub(crate) async fn draft_token(
    s: &Service,
    connection: &connections::Connection,
    management: Option<&super::management::Authority>,
) -> Result<Option<String>> {
    access_token_with(s, connection, management, true).await
}

async fn access_token_with(
    s: &Service,
    connection: &connections::Connection,
    management: Option<&super::management::Authority>,
    editing: bool,
) -> Result<Option<String>> {
    let lock = s.plugins.oauth_flows.lock(&connection.id);
    let _guard = lock.lock().await;
    if !editing {
        connections::ensure_owner(s, connection).await?;
    }
    let expected = connection.clone();
    let initial_authority = management.cloned();
    s.store
        .read(move |db| {
            if let Some(authority) = initial_authority {
                authority.authorize(db)?;
            }
            authorize_token(db, &expected, editing)
        })
        .await?;
    let key = connections::credential_key(&connection.id);
    let Some(saved) = s.vault.get(&key).await? else {
        return Ok(None);
    };
    if saved.get("oauth").is_none()
        || saved["oauth"]["expiresAt"]
            .as_i64()
            .is_some_and(|expires| expires > now() + 60_000)
    {
        return Ok(saved["token"].as_str().map(str::to_owned));
    }
    let package = registry::active(s, &connection.plugin_id).await?;
    let oauth = package
        .manifest
        .credential
        .as_ref()
        .and_then(|c| c.oauth.as_ref())
        .ok_or_else(expired)?;
    let metadata = &saved["oauth"];
    if metadata["tokenUrl"] != oauth.token_url {
        return Err(expired());
    }
    let refresh = metadata["refreshToken"]
        .as_str()
        .filter(|v| !v.is_empty())
        .ok_or_else(|| Error::unauthorized("This account needs to be reconnected."))?;
    let mut fields = vec![
        ("grant_type".into(), "refresh_token".into()),
        ("refresh_token".into(), refresh.to_owned()),
        ("client_id".into(), text(metadata, "clientId").to_owned()),
    ];
    if let Some(secret) = metadata["clientSecret"].as_str() {
        fields.push(("client_secret".into(), secret.to_owned()));
    }
    let response = post(s, &package.manifest, &oauth.token_url, &fields).await?;
    let flow = Flow {
        id: id(),
        plugin: connection.plugin_id.clone(),
        package_digest: package.digest,
        binding: String::new(),
        instance: Some(connection.id.clone()),
        revision: Some(connection.revision.clone()),
        settings: connection.settings.clone(),
        url: String::new(),
        redirect: String::new(),
        verifier: String::new(),
        client_id: text(metadata, "clientId").to_owned(),
        client_secret: metadata["clientSecret"].as_str().map(str::to_owned),
        scopes: text(metadata, "scope")
            .split_whitespace()
            .map(str::to_owned)
            .collect(),
        expires: 0,
        state: "completed",
        credential: None,
        message: None,
    };
    let refreshed = credentials(&response, &flow, oauth, Some(&saved))?;
    let encrypted = s.vault.encrypt(&key, &refreshed)?;
    let expected = connection.clone();
    let management = management.cloned();
    s.store
        .transaction(move |db| {
            if let Some(authority) = management {
                authority.authorize(db)?;
            }
            authorize_token(db, &expected, editing)?;
            db.set(&format!("mcp-secret:{key}"), &encrypted, None)
        })
        .await?;
    Ok(refreshed["token"].as_str().map(str::to_owned))
}

pub(crate) async fn disconnect<F>(
    s: &Service,
    plugin: &str,
    instance: &str,
    authorize: F,
) -> Result<Value>
where
    F: Fn(&crate::store::Db<'_>) -> Result<()> + Send + Sync + 'static,
{
    let lock = s.plugins.oauth_flows.lock(instance);
    let _guard = lock.lock().await;
    let package = registry::active(s, plugin).await?;
    let oauth = package
        .manifest
        .credential
        .as_ref()
        .and_then(|c| c.oauth.as_ref())
        .ok_or_else(|| Error::bad("This plugin has no OAuth account."))?;
    let connection = connections::get(s, plugin, instance).await?;
    let key = connections::credential_key(instance);
    let saved = s.vault.get(&key).await?;
    let expected = connection.clone();
    s.store
        .transaction(move |db| {
            authorize(db)?;
            let mut current = db
                .get(connections::KIND, &expected.id)?
                .ok_or_else(expired)?;
            if current["revision"] != expected.revision {
                return Err(Error::conflict(
                    "This account changed. Reload it before disconnecting.",
                ));
            }
            current["enabled"] = json!(false);
            current["revision"] = json!(id());
            current["updatedAt"] = json!(now());
            db.put(connections::KIND, &current)?;
            db.delete(&format!(
                "mcp-secret:{}",
                connections::credential_key(&expected.id)
            ))?;
            db.set(
                &format!("plugin-status:{}", expected.id),
                &json!({ "state": "disconnected" }),
                None,
            )?;
            db.audit(
                "plugin.oauth.disconnected",
                &json!({ "instance": expected.id, "plugin": expected.plugin_id }),
            )
        })
        .await?;
    s.plugins.changed.notify_one();
    s.plugins.files.forget(instance);
    let mut revoked = None;
    if let (Some(endpoint), Some(saved)) = (&oauth.revocation_url, saved) {
        let token = saved["oauth"]["refreshToken"]
            .as_str()
            .or_else(|| saved["token"].as_str());
        if let Some(token) = token {
            let endpoint = target(s, &package.manifest, endpoint, "https")?;
            let body = serde_urlencoded::to_string([("token", token)]).map_err(Error::internal)?;
            revoked = Some(
                s.plugins
                    .http
                    .post(endpoint)
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(body)
                    .timeout(Duration::from_secs(15))
                    .send()
                    .await
                    .is_ok_and(|r| r.status().is_success()),
            );
        }
    }
    Ok(
        json!({ "disconnected": true, "revoked": revoked, "message": if revoked == Some(false) {
        "Disconnected locally. The service could not confirm revocation; remove the app from your provider's connected applications if needed."
    } else { "Account disconnected. Reconnect before enabling it again." } }),
    )
}
