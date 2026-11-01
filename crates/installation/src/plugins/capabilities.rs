//! Host capabilities exposed to plugin scripts. Each call is checked against the
//! manifest (by the host crate), then against the live connection here.
use super::connections::{Connection, authorize, ensure_owner, secret_values};
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
use std::{sync::Arc, time::Duration};

/// What a handler acts for: its connection and the event being handled.
pub struct Context {
    pub management: Option<super::management::Authority>,
    pub connection: Connection,
    pub manifest: Manifest,
    /// Approval identity of the exact manifest and source loaded by this worker.
    pub package_digest: Option<String>,
    pub event: Value,
    /// The instance's socket, for its transport worker only.
    pub link: Option<super::socket::Link>,
    /// An instance being edited: its unsaved credential and secrets.
    pub draft: Option<Arc<super::forms::Draft>>,
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
        "inbound.accept" => super::work::inbound(s, context, &args).await,
        "hooks.emit" => super::hooks::emit(s, context, &args).await,
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
    super::connections::status_with(
        s,
        &context.connection,
        if state == "connected" {
            cairn_contracts::PluginState::Connected
        } else {
            cairn_contracts::PluginState::Connecting
        },
        message,
    )
    .await;
    Ok(Value::Null)
}

/// Capabilities that act for an instance rather than for a person: shared by
/// long-lived workers and one-shot handlers once their scope is authorized.
pub async fn effect(s: &Service, context: &Context, method: &str, args: Value) -> Result<Value> {
    match method {
        "storage.get" | "storage.set" | "storage.delete" | "storage.list" => {
            storage(s, context, method, &args).await
        }
        "http.request" => http(s, context, &args).await,
        "files.create" | "files.append" | "files.read" | "files.delete" | "files.composeMime" => {
            if method == "files.composeMime" {
                for attachment in args["attachments"].as_array().into_iter().flatten() {
                    s.plugins
                        .files
                        .authorize(s, context, text(attachment, "id"))
                        .await?;
                }
            } else if method != "files.create" {
                s.plugins
                    .files
                    .authorize(s, context, text(&args, "id"))
                    .await?;
            }
            let result = s.plugins.files.call(&context.connection, method, &args)?;
            if matches!(
                method,
                "files.create" | "files.composeMime" | "files.append"
            ) {
                s.plugins
                    .files
                    .bind(context, &result, method != "files.append")?;
            }
            Ok(result)
        }
        "timers.after" | "timers.cancel" => timers(s, context, method, &args).await,
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
async fn storage(s: &Service, context: &Context, method: &str, args: &Value) -> Result<Value> {
    let connection = context.connection.id.clone();
    let management = context.management.clone();
    let method = method.to_owned();
    let args = args.clone();
    s.store
        .transaction(move |db| {
            if let Some(authority) = management {
                authority.authorize(db)?;
            }
            if method == "storage.list" {
                let prefix = text(&args, "prefix");
                let keys = db
                    .plugin_storage_keys(&connection)?
                    .into_iter()
                    .filter(|key| key.starts_with(prefix))
                    .collect::<Vec<_>>();
                return Ok(json!(keys));
            }
            let key = key(&args)?;
            match method.as_str() {
                "storage.get" => Ok(db.plugin_storage(&connection, &key)?.unwrap_or(Value::Null)),
                "storage.delete" => {
                    db.remove_plugin_storage(&connection, &key)?;
                    Ok(Value::Null)
                }
                _ => {
                    let value = args.get("value").cloned().unwrap_or(Value::Null);
                    if serde_json::to_string(&value)?.len() > 65_536 {
                        return Err(Error::too_large("Stored values are limited to 64 KB."));
                    }
                    if db.plugin_storage(&connection, &key)?.is_none()
                        && db.plugin_storage_keys(&connection)?.len() >= 1000
                    {
                        return Err(Error::too_large(
                            "A connection can keep at most 1000 values.",
                        ));
                    }
                    db.save_plugin_storage(&connection, &key, &value)?;
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
    if *auth == Value::Bool(true) {
        let rule = context
            .manifest
            .credential
            .as_ref()
            .ok_or_else(|| Error::forbidden("This plugin declares no credential."))?;
        let secret = match &context.draft {
            Some(draft) => draft.credential.clone(),
            None => {
                super::oauth::access_token_managed(
                    s,
                    &context.connection,
                    context.management.as_ref(),
                )
                .await?
            }
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
    let secret = secrets(s, context)
        .await?
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| Error::forbidden("This instance has no such secret."))?;
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
    if let Some(spec) = args.get("bodyFile").filter(|value| value.is_object()) {
        if !context.manifest.permits_capability("files") {
            return Err(Error::forbidden("File transfers are not approved."));
        }
        s.plugins
            .files
            .authorize(s, context, text(spec, "id"))
            .await?;
        let bytes = s
            .plugins
            .files
            .bytes(&context.connection, text(spec, "id"))?;
        match text(spec, "format") {
            "base64url" => {
                use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
                let mut body = args.get("json").cloned().unwrap_or_else(|| json!({}));
                let pointer = spec["jsonPointer"].as_str().unwrap_or("/raw");
                if pointer.len() > 200 || !pointer.starts_with('/') {
                    return Err(Error::bad(
                        "Provide a JSON pointer to the encoded file field.",
                    ));
                }
                let (parent, key) = pointer.rsplit_once('/').unwrap();
                let key = key.replace("~1", "/").replace("~0", "~");
                let object = if parent.is_empty() {
                    body.as_object_mut()
                } else {
                    body.pointer_mut(parent).and_then(Value::as_object_mut)
                }
                .ok_or_else(|| Error::bad("The file's JSON parent must be an object."))?;
                object.insert(key, json!(URL_SAFE_NO_PAD.encode(bytes)));
                request = request.json(&body);
            }
            "raw" => request = request.body(bytes),
            "json" => {
                request = request
                    .header("content-type", "application/json")
                    .body(bytes);
            }
            _ => return Err(Error::bad("Choose raw, json or base64url file format.")),
        }
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
    if let Some((header, prefix, secret)) = authorization(s, context, &args["auth"]).await? {
        request = request.header(header, format!("{prefix}{secret}"));
    }
    let file_response = args.get("responseFile").filter(|v| v.is_object());
    if file_response.is_some() && !context.manifest.permits_capability("files") {
        return Err(Error::forbidden("File transfers are not approved."));
    }
    authorize_management(s, context).await?;
    let claim =
        super::idempotency::claim(s, &context.connection, args, context.management.as_ref())
            .await?;
    // Claiming a durable receipt can wait behind another writer. Resolve the
    // live resource ceiling again before a send or a cached result leaves Cairn.
    authorize_management(s, context).await?;
    if let Some(super::idempotency::Claim::Cached(response)) = &claim {
        return Ok(response.clone());
    }
    let response = request
        .send()
        .await
        .map_err(|_| Error::bad_gateway("The service is unavailable."))?;
    let status = response.status().as_u16();
    let mut headers = Map::new();
    for (name, value) in response.headers() {
        let name = name.as_str();
        // Metadata such as rate limits, granted scopes and pagination; never cookies.
        let metadata =
            matches!(name, "retry-after" | "content-type" | "link") || name.starts_with("x-");
        if metadata && let Ok(value) = value.to_str() {
            headers.insert(name.to_owned(), json!(value));
        }
    }
    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| Error::bad_gateway("The response was interrupted."))?;
        let limit = if file_response.is_some() && (200..300).contains(&status) {
            super::files::MAX_BYTES * 4 / 3 + 65_536
        } else {
            1_048_576
        };
        if body.len() + chunk.len() > limit {
            return Err(Error::too_large(
                "The response is too large. Use a transfer file for large payloads.",
            ));
        }
        body.extend_from_slice(&chunk);
    }
    if let Some(spec) = file_response
        && (200..300).contains(&status)
    {
        let media_type = headers
            .get("content-type")
            .and_then(Value::as_str)
            .unwrap_or("application/octet-stream");
        let file = s
            .plugins
            .files
            .response(&context.connection, spec, &body, media_type)?;
        s.plugins.files.bind(context, &file, true)?;
        return Ok(json!({ "status": status, "headers": headers, "file": file }));
    }
    let json = serde_json::from_slice::<Value>(&body).ok();
    let response = json!({
        "status": status,
        "headers": headers,
        "json": json,
        "text": if json.is_none() { json!(String::from_utf8_lossy(&body)) } else { Value::Null },
    });
    if let Some(super::idempotency::Claim::Fresh(id)) = claim {
        super::idempotency::complete(
            s,
            &context.connection,
            &id,
            &response,
            context.management.as_ref(),
        )
        .await?;
    }
    Ok(response)
}

/// Durable timers delivered later as `timer` events.
async fn timers(s: &Service, context: &Context, method: &str, args: &Value) -> Result<Value> {
    let connection = context.connection.clone();
    let management = context.management.clone();
    let method = method.to_owned();
    let args = args.clone();
    s.store
        .transaction(move |db| {
            if let Some(authority) = management {
                authority.authorize(db)?;
            }
            let owned = db.plugin_timers(&connection.id, i64::MAX)?;
            if method == "timers.cancel" {
                if let Some(timer) = owned
                    .iter()
                    .find(|timer| Some(timer.id.as_str()) == args["id"].as_str())
                {
                    db.remove_plugin_timer(&timer.id)?;
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
            db.save_plugin_timer(&super::state::Timer {
                id: timer.clone(),
                instance_id: connection.id.clone(),
                revision: connection.revision.clone(),
                name: name.into(),
                payload,
                at: now() + (seconds * 1000.0) as i64,
            })?;
            Ok(json!(timer))
        })
        .await
}

pub(crate) async fn authorize_management(s: &Service, context: &Context) -> Result<()> {
    if let Some(authority) = &context.management {
        authority.recheck(s).await?;
    }
    Ok(())
}

#[cfg(test)]
mod authorization_tests {
    use super::*;
    use crate::plugin_grant_fixture::{Fixture, contribution, support};
    use cairn_contracts::PluginContributionKind;
    use std::sync::atomic::Ordering;

    #[tokio::test]
    async fn http_rechecks_resource_coverage_after_claiming_its_receipt() {
        let f = Fixture::new().await;
        f.marker("live-parent").await;
        f.set(json!([contribution("tool", "grant_probe_post", support())]))
            .await;
        let package = super::super::registry::active(&f.s, "grant-probe")
            .await
            .unwrap();
        let instance =
            f.s.store
                .plugin_instance(&f.instance)
                .await
                .unwrap()
                .unwrap();
        let snapshot = super::super::grant_authorization::Snapshot::capture(
            &f.s,
            crate::config::MAIN_AGENT_ID,
            None,
            &package,
        )
        .await
        .unwrap();
        let invocation = super::super::grant_authorization::Invocation::prepare(
            &f.s,
            snapshot.clone(),
            instance.clone(),
            PluginContributionKind::Tool,
            "grant_probe_post",
            &json!({ "thread": "support-thread" }),
        )
        .await
        .unwrap();
        let moved = f.moved_parent.clone();
        let preparation =
            crate::capabilities::plugin_packages::Authority::default().with_guard(move |db| {
                // Change only the external service's ancestry at the receipt's
                // transaction boundary; all local revisions stay unchanged.
                if !db.0.is_autocommit() {
                    moved.store(true, Ordering::SeqCst);
                }
                Ok(())
            });
        let management = super::super::management::Authority::agent_action(
            instance.clone(),
            snapshot,
            Some(invocation),
        )
        .with_preparation(Some(preparation));
        let context = Context {
            management: Some(management),
            connection: instance,
            manifest: package.manifest.clone(),
            package_digest: Some(package.digest.clone()),
            event: Value::Null,
            link: None,
            draft: None,
        };
        let result = http(
            &f.s,
            &context,
            &json!({
                "method": "POST",
                "url": "https://service.example.test/upload",
                "idempotencyKey": "resource-move-at-claim",
            }),
        )
        .await;
        assert!(
            f.moved_parent.load(Ordering::SeqCst),
            "receipt did not execute"
        );
        assert_eq!(
            f.uploads.load(Ordering::SeqCst),
            0,
            "HTTP mutation escaped revoked resource coverage"
        );
        assert_eq!(result.unwrap_err().status, 403);
        f.s.shutdown.cancel();
    }

    async fn transfer_context(f: &Fixture, agent: &str, scoped: bool) -> Context {
        let package = super::super::registry::active(&f.s, "grant-probe")
            .await
            .unwrap();
        let connection =
            f.s.store
                .plugin_instance(&f.instance)
                .await
                .unwrap()
                .unwrap();
        let snapshot =
            super::super::grant_authorization::Snapshot::capture(&f.s, agent, None, &package)
                .await
                .unwrap();
        let invocation = if scoped {
            Some(
                super::super::grant_authorization::Invocation::prepare(
                    &f.s,
                    snapshot.clone(),
                    connection.clone(),
                    PluginContributionKind::Tool,
                    "grant_probe_post",
                    &json!({"channel":"support"}),
                )
                .await
                .unwrap(),
            )
        } else {
            None
        };
        Context {
            management: Some(super::super::management::Authority::agent_action(
                connection.clone(),
                snapshot,
                invocation,
            )),
            connection,
            manifest: package.manifest.clone(),
            package_digest: Some(package.digest),
            event: Value::Null,
            link: None,
            draft: None,
        }
    }

    #[tokio::test]
    async fn appends_preserve_raw_uploader_boundaries_even_when_adding_a_contribution_origin() {
        use crate::{agent_fixture::AgentFixture, config::MAIN_AGENT_ID};

        for scoped in [false, true] {
            for original in ["owner", "other_agent", "caller"] {
                let f = Fixture::new().await;
                let context = transfer_context(&f, MAIN_AGENT_ID, false).await;
                let bytes = json!({ "data": "synthetic original bytes", "encoding": "text" });
                let file = match original {
                    "owner" => {
                        f.s.plugins
                            .files
                            .create(
                                &context.connection,
                                "owner.txt",
                                "text/plain",
                                b"synthetic original bytes",
                            )
                            .unwrap()
                    }
                    "other_agent" => {
                        let other =
                            f.s.agent_fixture(json!({ "name": "Other uploader" }), None)
                                .await
                                .unwrap();
                        let agent = text(&other, "id");
                        f.set_for(agent, json!([contribution("instructions", "", json!([]))]))
                            .await;
                        let other = transfer_context(&f, agent, false).await;
                        effect(&f.s, &other, "files.create", bytes).await.unwrap()
                    }
                    _ => effect(&f.s, &context, "files.create", bytes).await.unwrap(),
                };
                let append_context = transfer_context(&f, MAIN_AGENT_ID, scoped).await;
                effect(
                    &f.s,
                    &append_context,
                    "files.append",
                    json!({
                        "id": file["id"], "offset": file["size"], "data": "", "encoding": "text",
                    }),
                )
                .await
                .unwrap();
                let mut contributions = vec![contribution("instructions", "", json!([]))];
                if scoped {
                    contributions.push(contribution("tool", "grant_probe_post", support()));
                }
                f.set(json!(contributions)).await;
                let reader = transfer_context(&f, MAIN_AGENT_ID, false).await;
                let result = effect(&f.s, &reader, "files.read", json!({ "id": file["id"] })).await;
                if original == "caller" {
                    result.expect("a raw uploader lost access to its own bytes");
                    if scoped {
                        f.set(json!([contribution("instructions", "", json!([]))]))
                            .await;
                        let reader = transfer_context(&f, MAIN_AGENT_ID, false).await;
                        assert_eq!(
                            effect(&f.s, &reader, "files.read", json!({ "id": file["id"] }))
                                .await
                                .unwrap_err()
                                .status,
                            403,
                            "own-upload access bypassed the appended contribution's revocation"
                        );
                    }
                } else {
                    assert_eq!(
                        result.unwrap_err().status,
                        403,
                        "an append laundered {original} bytes (scoped={scoped})"
                    );
                }
                f.s.shutdown.cancel();
            }
        }
    }
}
