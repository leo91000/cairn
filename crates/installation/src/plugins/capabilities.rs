//! Host capabilities exposed to plugin scripts. Each call is checked against the
//! manifest (by the host crate), then against the live connection here.
use super::connections::{Connection, authorize, credential};
use crate::{
    config::{id, now},
    error::{Error, Result},
    service::Service,
    validation::text,
};
use cairn_plugin_api::Manifest;
use cairn_plugin_host::Denied;
use futures_util::StreamExt;
use serde_json::{Map, Value, json};
use std::time::Duration;

/// What a handler acts for: its connection and the event being handled.
pub struct Context {
    pub connection: Connection,
    pub manifest: Manifest,
    pub event: Value,
}

/// Maps host errors to the short codes scripts may see. Messages stay in the host.
pub fn code(error: &Error) -> &'static str {
    match error.status {
        400 | 413 => "invalid",
        401 | 403 => "forbidden",
        404 => "not_found",
        409 => "conflict",
        429 => "rate_limited",
        _ => "unavailable",
    }
}

pub async fn call(
    s: &Service,
    context: &Context,
    method: &str,
    args: Value,
) -> std::result::Result<Value, Denied> {
    let result = async {
        let expected = context.connection.clone();
        s.store.read(move |db| authorize(db, &expected)).await?;
        match method {
            "storage.get" | "storage.set" | "storage.delete" | "storage.list" => {
                storage(s, &context.connection, method, &args).await
            }
            "http.request" => http(s, context, &args).await,
            "timers.after" | "timers.cancel" => timers(s, &context.connection, method, &args).await,
            "conversations.submit" => {
                super::work::submit(s, &context.connection, &context.event, &args).await
            }
            _ => Err(Error::forbidden("Unknown plugin capability.")),
        }
    }
    .await;
    result.map_err(|error| Denied(code(&error)))
}

fn key(args: &Value) -> Result<String> {
    let key = text(args, "key");
    if key.is_empty() || key.chars().count() > 200 || key.chars().any(char::is_control) {
        return Err(Error::bad("Invalid storage key."));
    }
    Ok(key.to_owned())
}

/// Small connection-scoped values. Removed with the connection.
async fn storage(
    s: &Service,
    connection: &Connection,
    method: &str,
    args: &Value,
) -> Result<Value> {
    const KIND: &str = "plugin-storage";
    let connection = connection.id.clone();
    let method = method.to_owned();
    let args = args.clone();
    s.store
        .transaction(move |db| {
            let owned = || {
                db.list(KIND).map(|values| {
                    values
                        .into_iter()
                        .filter(|value| value["connectionId"] == connection)
                        .collect::<Vec<_>>()
                })
            };
            if method == "storage.list" {
                let prefix = text(&args, "prefix");
                let mut keys = owned()?
                    .iter()
                    .map(|value| text(value, "key").to_owned())
                    .filter(|key| key.starts_with(prefix))
                    .collect::<Vec<_>>();
                keys.sort();
                return Ok(json!(keys));
            }
            let key = key(&args)?;
            let record_id = format!("storage:{connection}:{}", crate::auth::hex_digest(&key));
            match method.as_str() {
                "storage.get" => Ok(db
                    .get(KIND, &record_id)?
                    .map_or(Value::Null, |value| value["value"].clone())),
                "storage.delete" => {
                    db.remove(KIND, &record_id)?;
                    Ok(Value::Null)
                }
                _ => {
                    let value = args.get("value").cloned().unwrap_or(Value::Null);
                    if serde_json::to_string(&value)?.len() > 65_536 {
                        return Err(Error::too_large("Stored values are limited to 64 KB."));
                    }
                    if db.get(KIND, &record_id)?.is_none() && owned()?.len() >= 1000 {
                        return Err(Error::too_large("A connection can keep at most 1000 values."));
                    }
                    db.put(
                        KIND,
                        &json!({ "id": record_id, "connectionId": connection, "key": key, "value": value, "updatedAt": now() }),
                    )?;
                    Ok(Value::Null)
                }
            }
        })
        .await
}

/// HTTPS to hosts the manifest lists. With `auth = true`, the host adds the
/// connection's credential using the manifest's rule; scripts never see it.
async fn http(s: &Service, context: &Context, args: &Value) -> Result<Value> {
    let invalid = |message: &str| Error::bad(message.to_owned());
    let method = match args["method"].as_str().unwrap_or("GET") {
        "GET" => reqwest::Method::GET,
        "POST" => reqwest::Method::POST,
        "PUT" => reqwest::Method::PUT,
        "PATCH" => reqwest::Method::PATCH,
        "DELETE" => reqwest::Method::DELETE,
        _ => return Err(invalid("Unsupported HTTP method.")),
    };
    let url = url::Url::parse(text(args, "url")).map_err(|_| invalid("Invalid URL."))?;
    let host = url.host_str().unwrap_or("");
    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some()
        || !context.manifest.hosts.iter().any(|allowed| allowed == host)
    {
        return Err(Error::forbidden("This host is not declared by the plugin."));
    }
    let target = match s.plugins.test_origin() {
        Some(origin) => format!(
            "{origin}{}{}",
            url.path(),
            url.query()
                .map(|query| format!("?{query}"))
                .unwrap_or_default()
        ),
        None => url.to_string(),
    };
    let mut request = s
        .plugins
        .http
        .request(method, target)
        .timeout(Duration::from_secs(15));
    if let Some(headers) = args["headers"].as_object() {
        if headers.len() > 10 {
            return Err(invalid("Too many headers."));
        }
        for (name, value) in headers {
            let name = name.to_ascii_lowercase();
            let value = value.as_str().unwrap_or("");
            let allowed = matches!(name.as_str(), "content-type" | "accept" | "user-agent")
                || name.starts_with("x-");
            if !allowed || value.len() > 500 || value.chars().any(char::is_control) {
                return Err(invalid("This header is not allowed."));
            }
            request = request.header(name, value);
        }
    }
    if let Some(body) = args.get("json").filter(|value| !value.is_null()) {
        if serde_json::to_string(body)?.len() > 262_144 {
            return Err(Error::too_large("Request body is too large."));
        }
        request = request.json(body);
    } else if let Some(body) = args["body"].as_str() {
        if body.len() > 262_144 {
            return Err(Error::too_large("Request body is too large."));
        }
        request = request.body(body.to_owned());
    }
    if args["auth"] == true {
        let rule = context
            .manifest
            .credential
            .as_ref()
            .ok_or_else(|| Error::forbidden("This plugin declares no credential."))?;
        let secret = credential(s, &context.connection)
            .await?
            .ok_or_else(|| Error::forbidden("This connection has no credential."))?;
        request = request.header(rule.header.as_str(), format!("{}{secret}", rule.prefix));
    }
    let response = request
        .send()
        .await
        .map_err(|_| Error::bad_gateway("The service is unavailable."))?;
    let status = response.status().as_u16();
    let mut headers = Map::new();
    for (name, value) in response.headers() {
        let name = name.as_str();
        if (name == "retry-after" || name == "content-type" || name.starts_with("x-ratelimit"))
            && let Ok(value) = value.to_str()
        {
            headers.insert(name.to_owned(), json!(value));
        }
    }
    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| Error::bad_gateway("The response was interrupted."))?;
        if body.len() + chunk.len() > 1_048_576 {
            return Err(Error::too_large("The response is larger than 1 MB."));
        }
        body.extend_from_slice(&chunk);
    }
    let json = serde_json::from_slice::<Value>(&body).ok();
    Ok(json!({
        "status": status,
        "headers": headers,
        "json": json,
        "text": if json.is_none() { json!(String::from_utf8_lossy(&body)) } else { Value::Null },
    }))
}

/// Durable timers delivered later as `timer` events.
async fn timers(s: &Service, connection: &Connection, method: &str, args: &Value) -> Result<Value> {
    const KIND: &str = "plugin-timers";
    let connection = connection.clone();
    let method = method.to_owned();
    let args = args.clone();
    s.store
        .transaction(move |db| {
            let owned = db
                .list(KIND)?
                .into_iter()
                .filter(|value| value["connectionId"] == connection.id)
                .collect::<Vec<_>>();
            if method == "timers.cancel" {
                if let Some(timer) = owned.iter().find(|value| value["id"] == args["id"]) {
                    db.remove(KIND, text(timer, "id"))?;
                }
                return Ok(Value::Null);
            }
            let seconds = args["seconds"]
                .as_f64()
                .filter(|value| (1.0..=604_800.0).contains(value))
                .ok_or_else(|| Error::bad("Timers run between 1 second and 7 days later."))?;
            let name = text(&args, "name");
            let payload = args.get("payload").cloned().unwrap_or(Value::Null);
            if name.is_empty()
                || name.len() > 64
                || !name
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-' | b'.'))
                || serde_json::to_string(&payload)?.len() > 4096
            {
                return Err(Error::bad("Invalid timer."));
            }
            if owned.len() >= 100 {
                return Err(Error::too_large(
                    "A connection can schedule at most 100 timers.",
                ));
            }
            let timer = id();
            db.put(
                KIND,
                &json!({
                    "id": timer,
                    "connectionId": connection.id,
                    "revision": connection.revision,
                    "name": name,
                    "payload": payload,
                    "at": now() + (seconds * 1000.0) as i64,
                }),
            )?;
            Ok(json!(timer))
        })
        .await
}
