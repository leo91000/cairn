//! Signed public requests, authenticated by the installation rather than Beacon.
//! Verification always uses the original bytes, before a fresh package worker runs.
use super::{
    connections, registry,
    scope::{self, Scope},
    setup,
};
use crate::{
    config::now,
    error::{Error, Result},
    service::Service,
};
use aws_lc_rs::hmac;
use axum::{
    body::{Body, to_bytes},
    extract::Request,
    http::{HeaderMap, HeaderValue, StatusCode},
    response::Response,
};
use cairn_plugin_api::WebhookSignature;
use serde_json::{Map, Value, json};
use std::{sync::Arc, time::Duration};

/// The JSON worker frame may expand each incoming byte to six escaped bytes.
pub const BODY_BYTES: usize = 128 * 1024;
pub const RESPONSE_BYTES: usize = 64 * 1024;
pub const DEADLINE: Duration = Duration::from_millis(2500);

/// This narrow route authenticates itself; it never grants account permissions.
pub fn public_path(path: &str) -> bool {
    cairn_protocol::plugin_webhook_path(path)
}

fn one_header<'a>(headers: &'a HeaderMap, name: &str) -> Result<&'a str> {
    let mut values = headers.get_all(name).iter();
    let value = values.next().and_then(|value| value.to_str().ok());
    if values.next().is_some() || value.is_none() {
        return Err(Error::unauthorized("Invalid webhook signature."));
    }
    Ok(value.unwrap())
}

fn verify(rule: &WebhookSignature, secret: &str, headers: &HeaderMap, body: &[u8]) -> Result<()> {
    let denied = || Error::unauthorized("Invalid webhook signature.");
    let timestamp = match &rule.timestamp {
        Some(rule) => {
            let raw = one_header(headers, &rule.header)?;
            if raw.is_empty() || raw.len() > 12 || !raw.bytes().all(|byte| byte.is_ascii_digit()) {
                return Err(denied());
            }
            let seconds: i64 = raw.parse().map_err(|_| denied())?;
            if (now() / 1000).abs_diff(seconds) > u64::from(rule.max_age_seconds) {
                return Err(denied());
            }
            raw
        }
        None => "",
    };
    let signature = one_header(headers, &rule.header)?;
    let encoded = signature.strip_prefix(&rule.prefix).ok_or_else(denied)?;
    if encoded.len() != 64 {
        return Err(denied());
    }
    let expected = hex::decode(encoded).map_err(|_| denied())?;
    // Replace only placeholders in the declared template, never text in the body.
    let mut signed = Vec::with_capacity(body.len() + rule.template.len() + timestamp.len());
    let mut remaining = rule.template.as_str();
    while let Some(index) = remaining.find("{{") {
        signed.extend_from_slice(&remaining.as_bytes()[..index]);
        remaining = &remaining[index..];
        let (replacement, length) = if remaining.starts_with("{{body}}") {
            (body, "{{body}}".len())
        } else {
            (timestamp.as_bytes(), "{{timestamp}}".len())
        };
        signed.extend_from_slice(replacement);
        remaining = &remaining[length..];
    }
    signed.extend_from_slice(remaining.as_bytes());
    let key = hmac::Key::new(hmac::HMAC_SHA256, secret.as_bytes());
    hmac::verify(&key, &signed, &expected).map_err(|_| denied())
}

pub async fn http(
    s: &Service,
    instance: &str,
    endpoint: &str,
    request: Request,
) -> Result<Response> {
    if request.method() != "POST" {
        return Err(Error::method_not_allowed("Webhooks require POST."));
    }
    tokio::time::timeout(DEADLINE, receive(s, instance, endpoint, request))
        .await
        .map_err(|_| Error::gateway_timeout("The webhook response timed out."))?
}

async fn receive(
    s: &Service,
    instance: &str,
    endpoint: &str,
    request: Request,
) -> Result<Response> {
    if !public_path(request.uri().path()) {
        return Err(Error::not_found("Webhook not found."));
    }
    let saved = s.store.get(connections::KIND, instance).await?;
    let draft = if saved.is_none() {
        Some(setup::lookup(s, instance).await?)
    } else {
        None
    };
    let connection = match saved {
        Some(value) => {
            let connection: connections::Connection = serde_json::from_value(value)?;
            let expected = connection.clone();
            s.store
                .read(move |db| connections::authorize(db, &expected))
                .await?;
            connections::ensure_owner(s, &connection).await?;
            connection
        }
        None => draft.as_ref().unwrap().connection(),
    };
    let approved = registry::approved(s, &connection.plugin_id).await?;
    let package = &approved.package;
    if draft
        .as_ref()
        .is_some_and(|draft| draft.digest != package.digest)
    {
        return Err(Error::not_found("Setup draft is unavailable or expired."));
    }
    let webhook = package
        .manifest
        .webhooks
        .iter()
        .find(|rule| rule.name == endpoint)
        .ok_or_else(|| Error::not_found("Webhook not found."))?;
    let secrets = connections::secret_values(s, instance).await?;
    let secret = secrets
        .get(&webhook.signature.secret)
        .and_then(Value::as_str)
        .filter(|secret| !secret.is_empty())
        .ok_or_else(|| Error::unauthorized("The webhook signing secret is unavailable."))?;
    let (parts, body) = request.into_parts();
    let header_bytes: usize = parts
        .headers
        .iter()
        .map(|(name, value)| name.as_str().len() + value.len())
        .sum();
    if header_bytes > 16_384 {
        return Err(Error::too_large("Webhook headers are limited to 16 KiB."));
    }
    let bytes = to_bytes(body, BODY_BYTES)
        .await
        .map_err(|_| Error::too_large("Webhook bodies are limited to 128 KiB."))?;
    verify(&webhook.signature, secret, &parts.headers, &bytes)?;
    let body =
        std::str::from_utf8(&bytes).map_err(|_| Error::bad("Webhook bodies must be UTF-8."))?;
    let mut headers = Map::new();
    for (name, value) in &parts.headers {
        if !matches!(
            name.as_str(),
            "authorization" | "cookie" | "proxy-authorization"
        ) {
            headers.insert(name.to_string(), json!(value.to_str().unwrap_or_default()));
        }
    }
    if draft.is_some() {
        let challenge: Value = serde_json::from_slice(&bytes)
            .map_err(|_| Error::bad("Setup challenges require JSON."))?;
        if !webhook.setup.as_ref().is_some_and(|setup| {
            setup.selectors.iter().all(|(pointer, expected)| {
                challenge.pointer(pointer).and_then(Value::as_str) == Some(expected)
            })
        }) {
            return Err(Error::conflict(
                "Only declared setup challenges are accepted before activation.",
            ));
        }
    }
    if let Some(expected) = &draft {
        let current = setup::load(s, instance).await?;
        if current.revision != expected.revision {
            return Err(Error::conflict("Setup changed. Retry its challenge."));
        }
    }
    let expected = connection.clone();
    let setup_scope = draft.map(|draft| Scope::Setup(Box::new(draft)));
    let payload = json!({
        "endpoint": endpoint,
        "method": "POST",
        "headers": headers,
        "body": body,
        "instance": scope::summary(&connection),
        "setup": setup_scope.is_some(),
    });
    let scope = setup_scope.unwrap_or_else(|| {
        Scope::Ingress(
            Box::new(connection),
            Arc::new(scope::LocalAuthority::for_package(&approved)),
        )
    });
    // A one-shot handler includes capability RPC futures. Keep its state on the
    // heap so nested public HTTP delivery does not grow the caller's stack.
    let value = Box::pin(scope::invoke(s, package, "webhook", payload, &scope)).await?;
    if !matches!(scope, Scope::Setup(_)) {
        s.store
            .read(move |db| connections::authorize(db, &expected))
            .await?;
    }
    approved.recheck(s).await?;
    response(&value)
}

fn response(value: &Value) -> Result<Response> {
    let status = value["status"]
        .as_u64()
        .filter(|status| (200..600).contains(status))
        .ok_or_else(|| Error::bad_gateway("The webhook returned an invalid status."))?;
    let body = value["body"]
        .as_str()
        .ok_or_else(|| Error::bad_gateway("The webhook response body must be a string."))?;
    if body.len() > RESPONSE_BYTES {
        return Err(Error::bad_gateway("The webhook response exceeds 64 KiB."));
    }
    let mut response = Response::new(Body::from(body.to_owned()));
    *response.status_mut() = StatusCode::from_u16(status as u16).unwrap();
    if let Some(headers) = value["headers"].as_object() {
        for (name, value) in headers {
            if name != "content-type" {
                return Err(Error::bad_gateway(
                    "The webhook returned an unsupported header.",
                ));
            }
            let value = value
                .as_str()
                .filter(|value| value.len() <= 128)
                .and_then(|value| HeaderValue::from_str(value).ok())
                .ok_or_else(|| Error::bad_gateway("The webhook returned an invalid header."))?;
            response.headers_mut().insert("content-type", value);
        }
    }
    response
        .headers_mut()
        .insert("cache-control", HeaderValue::from_static("no-store"));
    Ok(response)
}
