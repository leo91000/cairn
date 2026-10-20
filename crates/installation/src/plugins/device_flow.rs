//! Device sign-in (RFC 8628) for packages whose credential declares `deviceFlow`. The
//! host asks for a user code, shows it to the owner and polls for the token, which
//! stays in memory until the owner saves an instance with it. Neither the browser nor
//! the script ever receives the token. Pending sign-ins do not survive a restart.
use super::{capabilities::target, registry};
use crate::{
    config::{id, now},
    error::{Error, Result},
    service::Service,
    validation::text,
};
use serde_json::{Value, json};
use std::{collections::HashMap, sync::Mutex, time::Duration};

/// At most this many sign-ins wait at once, across packages.
const MAX_FLOWS: usize = 8;
/// A token not used by a save within this delay is forgotten.
const RETAIN_MS: i64 = 10 * 60 * 1000;

#[derive(Clone)]
struct Flow {
    plugin: String,
    state: &'static str,
    user_code: String,
    verification_uri: String,
    verification_uri_complete: Option<String>,
    expires_at: i64,
    message: Option<String>,
    token: Option<String>,
    updated_at: i64,
}

impl Flow {
    fn public(&self, id: &str) -> Value {
        json!({
            "id": id,
            "pluginId": self.plugin,
            "state": self.state,
            "userCode": self.user_code,
            "verificationUri": self.verification_uri,
            "verificationUriComplete": self.verification_uri_complete,
            "expiresAt": self.expires_at,
            "message": self.message,
        })
    }
}

#[derive(Default)]
pub struct Flows {
    flows: Mutex<HashMap<String, Flow>>,
}

impl Flows {
    fn prune(flows: &mut HashMap<String, Flow>) {
        let time = now();
        flows.retain(|_, flow| match flow.state {
            "pending" => flow.expires_at > time,
            _ => flow.updated_at + RETAIN_MS > time,
        });
    }

    fn update(&self, id: &str, change: impl FnOnce(&mut Flow)) -> bool {
        let mut flows = self.flows.lock().unwrap();
        match flows.get_mut(id) {
            Some(flow) if flow.state == "pending" => {
                change(flow);
                flow.updated_at = now();
                true
            }
            _ => false,
        }
    }

    /// The token of a completed sign-in for `plugin`. `take` forgets it.
    pub fn token(&self, plugin: &str, id: &str, take: bool) -> Result<String> {
        let mut flows = self.flows.lock().unwrap();
        Self::prune(&mut flows);
        let expired = || Error::bad("The sign-in expired. Sign in again.");
        let flow = flows.get(id).ok_or_else(expired)?;
        if flow.plugin != plugin || flow.state != "completed" {
            return Err(expired());
        }
        let token = flow.token.clone().ok_or_else(expired)?;
        if take {
            flows.remove(id);
        }
        Ok(token)
    }
}

fn form(fields: &[(&str, &str)]) -> String {
    fields
        .iter()
        .map(|(key, value)| {
            format!(
                "{}={}",
                url::form_urlencoded::byte_serialize(key.as_bytes()).collect::<String>(),
                url::form_urlencoded::byte_serialize(value.as_bytes()).collect::<String>()
            )
        })
        .collect::<Vec<_>>()
        .join("&")
}

async fn post(s: &Service, url: &str, body: String) -> Result<Value> {
    let response = s
        .plugins
        .http
        .post(url)
        .header("accept", "application/json")
        .header("content-type", "application/x-www-form-urlencoded")
        .body(body)
        .timeout(Duration::from_secs(15))
        .send()
        .await
        .map_err(|_| Error::bad_gateway("The sign-in service is unavailable."))?;
    let mut bytes = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = futures_util::StreamExt::next(&mut stream).await {
        let chunk = chunk.map_err(|_| Error::bad_gateway("The sign-in service is unavailable."))?;
        if bytes.len() + chunk.len() > 65_536 {
            return Err(Error::bad_gateway("Invalid sign-in response."));
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes).map_err(|_| Error::bad_gateway("Invalid sign-in response."))
}

fn short_text(value: &Value, key: &str, max: usize) -> Option<String> {
    value[key]
        .as_str()
        .filter(|value| {
            !value.is_empty() && value.len() <= max && !value.chars().any(char::is_control)
        })
        .map(str::to_owned)
}

fn web_address(value: Option<String>) -> Option<String> {
    value.filter(|raw| url::Url::parse(raw).is_ok_and(|url| url.scheme() == "https"))
}

/// `POST /api/plugins/{id}/device-flow`: a user code to enter on the service's page.
pub async fn start(s: &std::sync::Arc<Service>, plugin_id: &str) -> Result<Value> {
    let package = registry::active(s, plugin_id).await?;
    let flow = package
        .manifest
        .credential
        .as_ref()
        .and_then(|credential| credential.device_flow.clone())
        .ok_or_else(|| Error::not_found("This plugin has no sign-in."))?;
    {
        let mut flows = s.plugins.device_flows.flows.lock().unwrap();
        Flows::prune(&mut flows);
        if flows
            .values()
            .filter(|flow| flow.state == "pending")
            .count()
            >= MAX_FLOWS
        {
            return Err(Error::too_many_requests(
                "Too many sign-ins are waiting. Retry shortly.",
            ));
        }
    }
    let manifest = &package.manifest;
    let scopes = flow.scopes.join(" ");
    let mut fields = vec![("client_id", flow.client_id.as_str())];
    if !scopes.is_empty() {
        fields.push(("scope", scopes.as_str()));
    }
    let authorization = target(s, manifest, &flow.authorization_url, "https")?;
    let response = post(s, &authorization, form(&fields)).await?;
    let device_code = short_text(&response, "device_code", 512)
        .ok_or_else(|| Error::bad_gateway("The service refused the sign-in."))?;
    let user_code = short_text(&response, "user_code", 64)
        .ok_or_else(|| Error::bad_gateway("The service refused the sign-in."))?;
    let verification_uri = web_address(
        short_text(&response, "verification_uri", 500)
            .or_else(|| short_text(&response, "verification_url", 500)),
    )
    .ok_or_else(|| Error::bad_gateway("Invalid sign-in response."))?;
    let verification_uri_complete =
        web_address(short_text(&response, "verification_uri_complete", 1000));
    let expires_in = response["expires_in"]
        .as_i64()
        .unwrap_or(900)
        .clamp(60, 1800);
    let interval = response["interval"].as_u64().unwrap_or(5).clamp(1, 60);
    let flow_id = id();
    let record = Flow {
        plugin: plugin_id.to_owned(),
        state: "pending",
        user_code,
        verification_uri,
        verification_uri_complete,
        expires_at: now() + expires_in * 1000,
        message: None,
        token: None,
        updated_at: now(),
    };
    let public = record.public(&flow_id);
    {
        // Rechecked as it is kept: sign-ins started concurrently cannot pass the limit.
        let mut flows = s.plugins.device_flows.flows.lock().unwrap();
        if flows
            .values()
            .filter(|flow| flow.state == "pending")
            .count()
            >= MAX_FLOWS
        {
            return Err(Error::too_many_requests(
                "Too many sign-ins are waiting. Retry shortly.",
            ));
        }
        flows.insert(flow_id.clone(), record);
    }
    let token_url = target(s, manifest, &flow.token_url, "https")?;
    let service = s.clone();
    tokio::spawn(async move {
        poll(
            &service,
            &flow_id,
            &token_url,
            &flow.client_id,
            &device_code,
            interval,
        )
        .await;
    });
    s.store
        .audit("plugin.sign_in.started", json!({ "plugin": plugin_id }))
        .await?;
    Ok(public)
}

async fn poll(
    s: &Service,
    flow_id: &str,
    token_url: &str,
    client_id: &str,
    device_code: &str,
    mut interval: u64,
) {
    let flows = &s.plugins.device_flows;
    let body = form(&[
        ("client_id", client_id),
        ("device_code", device_code),
        ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
    ]);
    loop {
        tokio::select! {
            () = s.shutdown.cancelled() => return,
            () = tokio::time::sleep(Duration::from_secs(interval)) => {},
        }
        let expired = {
            let flows = flows.flows.lock().unwrap();
            match flows.get(flow_id) {
                Some(flow) if flow.state == "pending" => flow.expires_at <= now(),
                _ => return,
            }
        };
        if expired {
            flows.update(flow_id, |flow| flow.state = "expired");
            return;
        }
        // Network failures retry until the code expires.
        let Ok(response) = post(s, token_url, body.clone()).await else {
            continue;
        };
        if let Some(token) = short_text(&response, "access_token", 4096)
            .filter(|token| !token.chars().any(char::is_whitespace))
        {
            flows.update(flow_id, |flow| {
                flow.state = "completed";
                flow.token = Some(token);
            });
            return;
        }
        match text(&response, "error") {
            "authorization_pending" => {}
            "slow_down" => interval = (interval + 5).min(60),
            "expired_token" => {
                flows.update(flow_id, |flow| flow.state = "expired");
                return;
            }
            "access_denied" => {
                flows.update(flow_id, |flow| {
                    flow.state = "failed";
                    flow.message = Some("The sign-in was declined.".into());
                });
                return;
            }
            _ => {
                flows.update(flow_id, |flow| {
                    flow.state = "failed";
                    flow.message = Some("The service refused the sign-in.".into());
                });
                return;
            }
        }
    }
}

/// `GET /api/plugins/{id}/device-flow/{flow}`.
pub fn status(s: &Service, plugin_id: &str, flow_id: &str) -> Result<Value> {
    let mut flows = s.plugins.device_flows.flows.lock().unwrap();
    Flows::prune(&mut flows);
    let flow = flows
        .get(flow_id)
        .filter(|flow| flow.plugin == plugin_id)
        .ok_or_else(|| Error::not_found("This sign-in expired. Sign in again."))?;
    Ok(flow.public(flow_id))
}

/// `DELETE /api/plugins/{id}/device-flow/{flow}`: forgets the sign-in and any token.
pub fn cancel(s: &Service, plugin_id: &str, flow_id: &str) -> Value {
    let mut flows = s.plugins.device_flows.flows.lock().unwrap();
    if flows
        .get(flow_id)
        .is_some_and(|flow| flow.plugin == plugin_id)
    {
        flows.remove(flow_id);
    }
    json!({ "cancelled": true })
}
