//! Host capabilities exposed to plugin scripts. Each call is checked against the
//! manifest (by the host crate), then against the live connection here.
use super::connections::{Connection, authorize, credential, ensure_owner, secret_values};
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
use std::{collections::BTreeMap, sync::Arc, time::Duration};

/// What a handler acts for: its connection and the event being handled.
pub struct Context {
    pub connection: Connection,
    pub manifest: Manifest,
    pub event: Value,
    /// The instance's socket, for its transport worker only.
    pub link: Option<super::socket::Link>,
    /// An instance being edited: its unsaved credential and secrets.
    pub draft: Option<Arc<super::forms::Draft>>,
    /// A slow credential renewal must not let a revoked run start a new request.
    pub resource_grant: Option<Arc<super::grants::Authorization>>,
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
    checked(s, context, method, args)
        .await
        .map_err(|error| Denied(code(&error)))
}

/// A long-lived worker's call, after rechecking the owner and the instance.
pub async fn checked(s: &Service, context: &Context, method: &str, args: Value) -> Result<Value> {
    ensure_owner(s, &context.connection).await?;
    let expected = context.connection.clone();
    s.store.read(move |db| authorize(db, &expected)).await?;
    match method {
        "conversations.submit" => super::work::submit(s, context, &args).await,
        "conversations.check" => super::work::check(s, context, &args).await,
        "conversations.answer" => super::work::answer(s, context, &args).await,
        "conversations.cancel" => super::work::cancel(s, context, &args).await,
        "inbound.accept" | "inbound.deliver" => {
            super::work::inbound(s, context, &args, method == "inbound.deliver").await
        }
        "instance.status" => instance_status(s, context, &args).await,
        _ if method.starts_with("socket.") => super::socket::call(s, context, method, &args).await,
        _ => effect(s, context, method, args).await,
    }
}

/// The package's own view of its connection, shown to the owner.
async fn instance_status(s: &Service, context: &Context, args: &Value) -> Result<Value> {
    let state = text(args, "state");
    let message = text(args, "message");
    if !matches!(state, "connecting" | "connected")
        || message.chars().count() > 200
        || message.chars().any(char::is_control)
    {
        return Err(Error::bad(
            "Report connecting or connected, with a short message.",
        ));
    }
    if state == "connected"
        && let Some(link) = &context.link
    {
        link.send(super::socket::Command::Healthy)?;
    }
    let message = (!message.is_empty()).then_some(message);
    super::connections::status_with(s, &context.connection, state, message).await;
    Ok(Value::Null)
}

/// Capabilities that act for an instance rather than for a person: shared by
/// long-lived workers and one-shot handlers once their scope is authorized.
pub async fn effect(s: &Service, context: &Context, method: &str, args: Value) -> Result<Value> {
    match method {
        "storage.get" | "storage.set" | "storage.delete" | "storage.list" => {
            storage(s, &context.connection, method, &args).await
        }
        "http.request" => http(s, context, &args).await,
        "resources.get" | "resources.list" => {
            super::resources::call(s, context, method, &args).await
        }
        "timers.after" | "timers.cancel" => timers(s, &context.connection, method, &args).await,
        "exec.run" => super::programs::run(s, context, &args).await,
        _ => Err(Error::forbidden("Unknown plugin capability.")),
    }
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

/// Checks a URL against the hosts the plugin declares: HTTPS (or WSS for sockets),
/// default port, no credentials in the URL. Tests may route HTTP to a local server.
pub(crate) fn target(s: &Service, manifest: &Manifest, raw: &str, scheme: &str) -> Result<String> {
    let url = url::Url::parse(raw).map_err(|_| Error::bad("Invalid URL."))?;
    let host = url.host_str().unwrap_or("");
    if url.scheme() != scheme
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some()
        || !manifest.allows_host(host)
    {
        return Err(Error::forbidden("This host is not declared by the plugin."));
    }
    Ok(
        match s.plugins.test_origin().filter(|_| scheme == "https") {
            Some(origin) => format!(
                "{origin}{}{}",
                url.path(),
                url.query()
                    .map(|query| format!("?{query}"))
                    .unwrap_or_default()
            ),
            None => url.to_string(),
        },
    )
}

/// Builds a multipart body from a JSON part and deliverables of the reply being
/// handled. Only files listed in that `reply` event can be attached.
async fn multipart(s: &Service, context: &Context, spec: &Value) -> Result<(String, Vec<u8>)> {
    let allowed = context.event["files"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let boundary = format!("cairn-{}", crate::config::id().replace('-', ""));
    let mut body = Vec::new();
    let mut part = |headers: String, bytes: &[u8]| {
        body.extend_from_slice(format!("--{boundary}\r\n{headers}\r\n\r\n").as_bytes());
        body.extend_from_slice(bytes);
        body.extend_from_slice(b"\r\n");
    };
    let field = |value: &str, fallback: &str| -> Result<String> {
        let name = if value.is_empty() { fallback } else { value };
        if name.len() > 60
            || !name
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'[' | b']' | b'-'))
        {
            return Err(Error::bad("Invalid multipart field."));
        }
        Ok(name.to_owned())
    };
    if let Some(json) = spec.get("json").filter(|value| !value.is_null()) {
        let name = field(text(spec, "jsonField"), "payload_json")?;
        part(
            format!(
                "Content-Disposition: form-data; name=\"{name}\"\r\nContent-Type: application/json"
            ),
            serde_json::to_string(json)?.as_bytes(),
        );
    }
    let files = spec["files"].as_array().cloned().unwrap_or_default();
    if files.len() > 10 {
        return Err(Error::bad("Attach up to ten files."));
    }
    let mut total = 0;
    for (index, file) in files.iter().enumerate() {
        let artifact = text(file, "artifact");
        crate::validation::uuid(artifact)?;
        let listed = allowed
            .iter()
            .find(|item| item["id"] == artifact)
            .ok_or_else(|| Error::forbidden("Only deliverables of this reply can be uploaded."))?;
        let path = s.config.data_dir.join("artifacts").join(artifact);
        let metadata = std::fs::symlink_metadata(&path)?;
        total += metadata.len();
        if !metadata.is_file() || metadata.len() > 25 * 1024 * 1024 || total > 50 * 1024 * 1024 {
            return Err(Error::too_large("Deliverables are too large to upload."));
        }
        let bytes = tokio::fs::read(&path).await?;
        let name = super::connections::label(text(listed, "name")).replace(['"', '\\'], "_");
        let media = listed["mediaType"]
            .as_str()
            .filter(|media| media.len() < 100 && !media.contains(['\r', '\n']))
            .unwrap_or("application/octet-stream");
        part(
            format!(
                "Content-Disposition: form-data; name=\"{}\"; filename=\"{name}\"\r\nContent-Type: {media}",
                field(text(file, "field"), &format!("files[{index}]"))?
            ),
            &bytes,
        );
    }
    body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    Ok((format!("multipart/form-data; boundary={boundary}"), body))
}

/// The instance's credential (`auth = true`), or one of its secret fields
/// (`auth = { secret, header?, prefix? }`), with the header it goes in. Values never
/// reach scripts. Instances being edited use their unsaved values.
pub(crate) async fn authorization(
    s: &Service,
    context: &Context,
    auth: &Value,
) -> Result<Option<(String, String, String)>> {
    if auth.get("oauth").is_some() {
        return Err(Error::forbidden(
            "OAuth credentials require a saved resource HTTP request.",
        ));
    }
    if *auth == Value::Bool(true) {
        let rule = context
            .manifest
            .credential
            .as_ref()
            .ok_or_else(|| Error::forbidden("This plugin declares no credential."))?;
        let secret = match &context.draft {
            Some(draft) => draft.credential.clone(),
            None => credential(s, &context.connection).await?,
        }
        .ok_or_else(|| Error::forbidden("This connection has no credential."))?;
        return Ok(Some((rule.header.clone(), rule.prefix.clone(), secret)));
    }
    let Some(rule) = auth.as_object() else {
        return Ok(None);
    };
    let header = rule
        .get("header")
        .and_then(Value::as_str)
        .unwrap_or("authorization");
    let prefix = rule.get("prefix").and_then(Value::as_str).unwrap_or("");
    let valid_header = !header.is_empty()
        && header.len() <= 40
        && header
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
        && !matches!(
            header,
            "host" | "content-length" | "transfer-encoding" | "cookie"
        );
    if !valid_header
        || prefix.len() > 20
        || !prefix
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b' ')
    {
        return Err(Error::bad("Invalid credential header."));
    }
    let key = rule.get("secret").and_then(Value::as_str).unwrap_or("");
    let secret = if let Some(resource) = rule.get("resource") {
        if context.draft.is_some() {
            return Err(Error::forbidden(
                "Resource credentials are only available to saved HTTP requests.",
            ));
        }
        let resource = resource
            .as_str()
            .ok_or_else(|| Error::bad("Name a resource."))?;
        super::resources::credential(s, context, resource, key).await?
    } else {
        secrets(s, context)
            .await?
            .get(key)
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| Error::forbidden("This instance has no such secret."))?
    };
    Ok(Some((header.to_owned(), prefix.to_owned(), secret)))
}

/// The instance's secret fields: saved, or being edited.
pub(crate) async fn secrets(s: &Service, context: &Context) -> Result<Map<String, Value>> {
    match &context.draft {
        Some(draft) => Ok(draft.secrets.clone()),
        None => secret_values(s, &context.connection.id).await,
    }
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
    let target = target(s, &context.manifest, text(args, "url"), "https")?;
    let resource = match args["auth"].get("resource") {
        Some(value) => {
            Some(super::resources::reference(s, context, value.as_str().unwrap_or_default()).await?)
        }
        None => None,
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
    if let Some(form) = args.get("form").filter(|value| !value.is_null()) {
        if !form
            .as_object()
            .is_some_and(|form| form.len() <= 50 && form.values().all(Value::is_string))
            || ["json", "body", "multipart"]
                .iter()
                .any(|key| args.get(*key).is_some_and(|value| !value.is_null()))
        {
            return Err(invalid(
                "Use form text fields without another request body.",
            ));
        }
        let fields: BTreeMap<String, String> = serde_json::from_value(form.clone())?;
        let encoded = serde_urlencoded::to_string(&fields).map_err(Error::internal)?;
        if encoded.len() > 262_144 {
            return Err(Error::too_large("Request body is too large."));
        }
        request = request.form(&fields);
    } else if let Some(spec) = args.get("multipart").filter(|value| value.is_object()) {
        let (kind, body) = multipart(s, context, spec).await?;
        request = request.header("content-type", kind).body(body);
    } else if let Some(body) = args.get("json").filter(|value| !value.is_null()) {
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
    let oauth = if let Some(name) = args["auth"].get("oauth") {
        if context.draft.is_some()
            || !context.manifest.permits_capability("oauth")
            || args["auth"].as_object().is_none_or(|auth| auth.len() != 2)
        {
            return Err(Error::forbidden(
                "Use an OAuth provider and resource on a saved instance.",
            ));
        }
        let (token, reference) = super::oauth::credential(
            s,
            context,
            text(&args["auth"], "resource"),
            name.as_str().unwrap_or(""),
        )
        .await?;
        request = request.bearer_auth(token);
        Some(reference)
    } else {
        None
    };
    if oauth.is_none()
        && let Some((header, prefix, secret)) = authorization(s, context, &args["auth"]).await?
    {
        request = request.header(header, format!("{prefix}{secret}"));
    }
    if let Some(grant) = &context.resource_grant {
        super::grants::recheck(s, grant).await?;
    }
    if let Some(oauth) = &oauth {
        super::oauth::recheck(s, oauth).await?;
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
    if let Some(resource) = &resource {
        super::resources::recheck(s, context, resource).await?;
    }
    if let Some(oauth) = &oauth {
        super::oauth::recheck(s, oauth).await?;
    }
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
