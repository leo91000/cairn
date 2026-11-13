//! Expiring bearer links. The installation retains content; Beacon only relays it.
use crate::{
    capabilities::ResumeContext,
    config::{id, now},
    error::{Error, Result, required},
    service::Service,
    skills::lifecycle::Authority,
    store::Db,
    validation::uuid,
};
use axum::{
    body::Body,
    extract::Request,
    http::HeaderValue,
    response::{Html, IntoResponse, Response},
};
use cairn_contracts::{
    ChatInput, ChatShare, ConversationLifecycle, SetChatShareInput, TimelineContent,
};
use serde_json::Value;

const MISSING: &str = "Conversation link expired, revoked or unavailable.";

fn active(db: &Db<'_>, chat: &str) -> Result<crate::chats::Conversation> {
    let conversation = required(db.conversation(chat)?, MISSING)?;
    if conversation.lifecycle != ConversationLifecycle::Active {
        return Err(Error::not_found(MISSING));
    }
    Ok(conversation)
}

pub(crate) fn revoke(db: &Db<'_>, chat: &str) -> Result<()> {
    if let Some(share) = db.kv_as::<ChatShare>(&format!("conversation-share:{chat}"))?
        && let Some(token) = share.token
    {
        db.delete(&format!("conversation-link:{token}"))?;
    }
    db.delete(&format!("conversation-share:{chat}"))
}

pub(crate) async fn manage(
    s: &Service,
    name: &str,
    args: Value,
    context: ResumeContext,
) -> Result<Value> {
    // Sharing is an owner/management capability even when merely reading its URL.
    let authority = Authority::new(s, context, "manage", false).await?;
    let (chat, hours) = if name == "set_chat_share" {
        let input: SetChatShareInput =
            serde_json::from_value(args).map_err(|error| Error::bad(error.to_string()))?;
        if !(1..=720).contains(&input.expires_in_hours) {
            return Err(Error::bad("Choose an expiration from 1 to 720 hours."));
        }
        (input.chat_id.to_string(), Some(input.expires_in_hours))
    } else {
        let input: ChatInput =
            serde_json::from_value(args).map_err(|error| Error::bad(error.to_string()))?;
        (input.chat_id.to_string(), None)
    };
    let origin = if hours.is_some() {
        crate::artifacts::sharing::public_origin(s).await?
    } else {
        String::new()
    };
    authority.recheck(s).await?;
    let name = name.to_owned();
    s.store
        .transaction(move |db| {
            authority.authorize(db)?;
            active(db, &chat)?;
            let key = format!("conversation-share:{chat}");
            if name == "get_chat_share" {
                return Ok(serde_json::to_value(
                    db.kv_as::<ChatShare>(&key)?.unwrap_or_default(),
                )?);
            }
            revoke(db, &chat)?;
            let Some(hours) = hours else {
                return Ok(serde_json::to_value(ChatShare::default())?);
            };
            let token = id();
            let expires = now() + i64::from(hours) * 3_600_000;
            let share = ChatShare {
                token: Some(token.clone()),
                public_url: Some(format!("{origin}/conversations/{token}")),
                expires_at: Some(expires),
            };
            db.set(
                &format!("conversation-link:{token}"),
                &Value::String(chat.clone()),
                Some(expires),
            )?;
            db.set_as(&key, &share, Some(expires))?;
            tracing::info!(
                chat_id = chat,
                expires_at = expires,
                "conversation sharing enabled"
            );
            Ok(serde_json::to_value(share)?)
        })
        .await
}

/// The relay's anonymous capability permits only these paths for this token.
pub(crate) fn public_read(path: &str, method: &str, token: &str) -> bool {
    if !matches!(method, "GET" | "HEAD") || uuid(token).is_err() {
        return false;
    }
    matches!(path.split('/').collect::<Vec<_>>().as_slice(),
        ["", "api", "shared-conversations", key] if *key == token)
        || matches!(path.split('/').collect::<Vec<_>>().as_slice(),
        ["", "api", "shared-conversations", key, "artifacts", artifact] if *key == token && uuid(artifact).is_ok())
}

const MESSAGE_BYTES: usize = 64 * 1024;

fn message_text(entry: &cairn_contracts::TimelineEntry) -> Option<(&'static str, &str)> {
    match &entry.content {
        TimelineContent::UserMessage { text, .. } => Some(("User", text)),
        TimelineContent::AgentMessage {
            text, echo: false, ..
        } => Some(("Agent", text)),
        _ => None,
    }
}

fn render_message(
    html: &mut String,
    entry: &cairn_contracts::TimelineEntry,
    offset: usize,
) -> Result<()> {
    let Some((label, text)) = message_text(entry) else {
        return Ok(());
    };
    if offset > text.len() || !text.is_char_boundary(offset) {
        return Err(Error::bad("Invalid message offset."));
    }
    let end = text.floor_char_boundary(offset.saturating_add(MESSAGE_BYTES).min(text.len()));
    html.push_str(&format!(
        "<article><strong>{label}</strong><pre>{}</pre>",
        escape(&text[offset..end])
    ));
    if end < text.len() {
        let entry_id: String = url::form_urlencoded::byte_serialize(entry.id.as_bytes()).collect();
        html.push_str(&format!(
            "<a href=\"?message={entry_id}&amp;offset={end}\">Continue message</a>"
        ));
    }
    html.push_str("</article>");
    Ok(())
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

pub(crate) async fn http(
    s: &Service,
    token: &str,
    artifact: Option<&str>,
    request: Request,
) -> Result<Response> {
    if !public_read(request.uri().path(), request.method().as_str(), token) {
        return Err(Error::not_found(MISSING));
    }
    let token = token.to_owned();
    let artifact = artifact.map(str::to_owned);
    let query: std::collections::HashMap<String, String> =
        serde_urlencoded::from_str(request.uri().query().unwrap_or(""))
            .map_err(|_| Error::bad("Invalid query."))?;
    let before = query
        .get("before")
        .map(|value| value.parse::<i64>())
        .transpose()
        .map_err(|_| Error::bad("Invalid history cursor."))?
        .unwrap_or(i64::MAX);
    let message = query.get("message").cloned();
    let offset = query
        .get("offset")
        .map(|value| value.parse::<usize>())
        .transpose()
        .map_err(|_| Error::bad("Invalid message offset."))?
        .unwrap_or(0);
    let (html, file) = s.store.read(move |db| {
        let tx = rusqlite::Transaction::new_unchecked(db.0, rusqlite::TransactionBehavior::Deferred)?;
        let db = Db(&tx);
        let chat = required(db.kv_as::<String>(&format!("conversation-link:{token}"))?, MISSING)?;
        let conversation = active(&db, &chat)?;
        let share = required(db.kv_as::<ChatShare>(&format!("conversation-share:{chat}"))?, MISSING)?;
        if share.token.as_deref() != Some(&token) { return Err(Error::not_found(MISSING)); }
        let run = conversation.run_id.as_deref().unwrap_or("");
        if let Some(artifact) = artifact {
            return Ok((String::new(), Some(required(db.artifact(run, &artifact)?, MISSING)?)));
        }
        let mut html = format!("<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><meta name=\"robots\" content=\"noindex,nofollow,noarchive\"><meta name=\"referrer\" content=\"no-referrer\"><title>{}</title><style>body{{font:16px system-ui;margin:0;background:#f6f7f8;color:#20252b}}main{{max-width:850px;margin:auto;padding:24px}}article,aside{{background:white;border:1px solid #ddd;border-radius:12px;padding:20px;margin:16px 0}}pre{{white-space:pre-wrap;overflow-wrap:anywhere;font:inherit}}a{{color:#276258}}small{{color:#58636e}}li{{margin:12px 0}}</style></head><body><main><small>Cairn · Shared conversation · Read only</small><h1>{}</h1><p>This link expires {} UTC. Messages and published files may update until then.</p>", escape(&conversation.title), escape(&conversation.title), chrono::DateTime::from_timestamp_millis(share.expires_at.unwrap_or_default()).map(|time| time.format("%Y-%m-%d %H:%M").to_string()).unwrap_or_default());
        if let Some(message) = message {
            let entry = required(crate::timeline::entry(&db, run, &message)?, MISSING)?;
            if message_text(&entry).is_none() { return Err(Error::not_found(MISSING)); }
            html.push_str("<nav><a href=\"?\">Back to conversation</a></nav>");
            render_message(&mut html, &entry, offset)?;
        } else {
            let (cards, more) = crate::timeline::page(&db, run, before, 100)?;
            if more && let Some(first) = cards.first() {
                html.push_str(&format!("<a href=\"?before={}\">Earlier messages</a>", first.position));
            }
            for card in cards { render_message(&mut html, &card, 0)?; }
        }
        // Include every published version, regardless of its individual visibility.
        html.push_str("<aside><h2>Published files</h2><ul>");
        for item in db.artifacts(Some(run))? {
            let url = format!("{}/artifacts/{}", share.public_url.as_deref().unwrap_or(""), item.id);
            html.push_str(&format!("<li><a href=\"{}\">{} · v{}</a> <small>{} · {} bytes</small> · <a href=\"{}?download=1\">Download</a></li>", escape(&url), escape(&item.title), item.version, escape(&item.name), item.size, escape(&url)));
        }
        html.push_str("</ul></aside></main></body></html>");
        Ok((html, None))
    }).await?;
    let mut response = if let Some(file) = file {
        let mut response = crate::artifacts::serve(s, &file, request).await?;
        response
            .extensions_mut()
            .remove::<crate::http::Uncompressed>();
        response
    } else if request.method() == "HEAD" {
        let mut response = Html(html).into_response();
        *response.body_mut() = Body::empty();
        response
    } else {
        Html(html).into_response()
    };
    for (name, value) in [
        ("cache-control", "no-store"),
        ("x-robots-tag", "noindex, nofollow, noarchive"),
        ("referrer-policy", "no-referrer"),
    ] {
        response
            .headers_mut()
            .insert(name, HeaderValue::from_static(value));
    }
    Ok(response)
}
