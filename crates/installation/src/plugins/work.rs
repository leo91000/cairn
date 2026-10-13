//! Durable work for delegated connections: admission of external messages, the event
//! queue plugins consume, conversation submission and correlated replies.
use super::connections::{AccessMode, Connection, authorize, ensure_owner, label};
use crate::{
    config::{id, now},
    error::{Error, Result},
    service::Service,
    store::Db,
    validation::text,
};
use rusqlite::OptionalExtension;
use serde_json::{Value, json};

pub const EVENTS: &str = "plugin-events";
pub const DELIVERIES: &str = "plugin-deliveries";

/// A message a transport received for a connection, already scoped by the transport.
pub struct Inbound {
    /// The platform's message ID, used for deduplication and replies.
    pub id: String,
    pub author: super::connections::Label,
    /// Routing information plugins need to answer (channel, bot ID…). Never content.
    pub context: Value,
    /// What the person wrote, stored only for allowed people.
    pub body: Value,
}

fn merge(base: &Value, extra: &Value) -> Value {
    let mut value = base.clone();
    if let (Some(target), Some(source)) = (value.as_object_mut(), extra.as_object()) {
        for (key, item) in source {
            target.insert(key.clone(), item.clone());
        }
    }
    value
}

pub(crate) fn enqueue(
    db: &Db<'_>,
    connection: &Connection,
    name: &str,
    payload: &Value,
    author: Option<&str>,
) -> Result<()> {
    db.put(
        EVENTS,
        &json!({
            "id": id(),
            "connectionId": connection.id,
            "revision": connection.revision,
            "name": name,
            "payload": payload,
            "authorId": author,
            "attempts": 0,
            "createdAt": now(),
            "nextAttemptAt": now(),
        }),
    )?;
    Ok(())
}

/// Fast durable ingress. Messages from people who aren't allowed are discarded before
/// their content is stored; approval mode records only who asked.
pub async fn accept(s: &Service, connection: &Connection, inbound: Inbound) -> Result<bool> {
    // Allowlists only change with a new revision (or shrink), so this snapshot is a
    // superset: strangers are dropped without touching storage or owner authority.
    if connection.access_mode == AccessMode::Allowlist
        && !connection.user_ids.contains(&inbound.author.id)
    {
        return Ok(false);
    }
    ensure_owner(s, connection).await?;
    let expected = connection.clone();
    s.store
        .transaction(move |db| {
            let current = authorize(db, &expected)?;
            let author = json!({ "id": inbound.author.id, "name": label(&inbound.author.name) });
            let base = merge(
                &inbound.context,
                &json!({ "id": inbound.id, "author": author }),
            );
            if !current.user_ids.contains(&inbound.author.id) {
                if current.access_mode == AccessMode::Requests {
                    request(db, &current, &inbound.author, &base)?;
                }
                return Ok(false);
            }
            let dedupe = format!("plugin-seen:{}:{}", current.id, inbound.id);
            if db.kv(&dedupe)?.is_some() {
                return Ok(false);
            }
            let pending = db
                .list(EVENTS)?
                .iter()
                .filter(|value| value["connectionId"] == current.id)
                .count();
            if pending >= 50 {
                return Ok(false);
            }
            enqueue(
                db,
                &current,
                "message",
                &merge(&base, &inbound.body),
                Some(&inbound.author.id),
            )?;
            db.set(&dedupe, &json!(true), Some(now() + 30 * 86_400_000))?;
            Ok(true)
        })
        .await
}

/// One request per person. The plugin hears about it once, without message content.
fn request(
    db: &Db<'_>,
    connection: &Connection,
    author: &super::connections::Label,
    payload: &Value,
) -> Result<()> {
    let requests = db.list("plugin-requests")?;
    if let Some(mut existing) = requests
        .iter()
        .find(|value| value["connectionId"] == connection.id && value["userId"] == author.id)
        .cloned()
    {
        if existing["status"] == "pending" {
            existing["mentions"] = json!(existing["mentions"].as_u64().unwrap_or(1) + 1);
            existing["updatedAt"] = json!(now());
            db.put("plugin-requests", &existing)?;
        }
        return Ok(());
    }
    if requests
        .iter()
        .filter(|value| value["connectionId"] == connection.id)
        .count()
        >= 50
    {
        return Ok(());
    }
    db.put(
        "plugin-requests",
        &json!({
            "id": id(),
            "connectionId": connection.id,
            "userId": author.id,
            "name": label(&author.name),
            "status": "pending",
            "mentions": 1,
            "createdAt": now(),
            "updatedAt": now(),
        }),
    )?;
    enqueue(db, connection, "access_requested", payload, None)?;
    db.audit(
        "plugin.access.requested",
        &json!({ "connectionId": connection.id }),
    )
}

/// `conversations.submit`: only while handling a `message` event, on behalf of that
/// event's author. Identity and routing come from ingress, never from script arguments.
pub async fn submit(
    s: &Service,
    expected: &Connection,
    event: &Value,
    args: &Value,
) -> Result<Value> {
    if event["name"] != "message" {
        return Err(Error::forbidden(
            "Conversations start only from a received message.",
        ));
    }
    ensure_owner(s, expected).await?;
    let content = text(args, "text").trim().to_owned();
    let thread = text(args, "thread").to_owned();
    let reply = args.get("reply").cloned().unwrap_or(Value::Null);
    if content.is_empty()
        || content.len() > 50_000
        || thread.chars().count() > 200
        || serde_json::to_string(&reply)?.len() > 4096
    {
        return Err(Error::bad("Invalid conversation message."));
    }
    let expected = expected.clone();
    let event = event.clone();
    let result = s
        .store
        .transaction(move |db| {
            let connection = authorize(db, &expected)?;
            let author = text(&event, "authorId").to_owned();
            if !connection.user_ids.contains(&author) || event["revision"] != connection.revision {
                return Err(Error::forbidden("This person is no longer allowed."));
            }
            let external_id = text(&event["payload"], "id").to_owned();
            let dedupe = format!("plugin-submitted:{}:{external_id}", connection.id);
            if let Some(value) = db.kv(&dedupe)? {
                return Ok(value);
            }
            let hour = now() / 3_600_000;
            let rate_key = format!("plugin-rate:{}:{hour}", connection.id);
            let count = db.kv(&rate_key)?.and_then(|value| value.as_u64()).unwrap_or(0);
            if count >= u64::from(connection.max_messages_per_hour) {
                return Err(Error::too_many_requests("Connection message limit reached."));
            }
            let outstanding = db
                .list(DELIVERIES)?
                .iter()
                .filter(|value| value["connectionId"] == connection.id)
                .count();
            if outstanding >= 20 {
                return Err(Error::too_many_requests("Connection has too much pending work."));
            }
            // The author is always part of the key: a plugin chooses threads, never people.
            let binding_key = format!(
                "plugin-chat:{}:{}:{}",
                connection.id,
                author,
                crate::auth::hex_digest(&thread)
            );
            let bound = db.kv(&binding_key)?.and_then(|value| value.as_str().map(str::to_owned));
            let usable = bound
                .as_deref()
                .and_then(|id| db.get("chats", id).ok().flatten())
                .filter(|chat| {
                    crate::conversation_lifecycle::is_active(chat)
                        && chat["agentId"] == connection.agent_id
                        && chat["projectId"]
                            == serde_json::to_value(&connection.project_id).unwrap_or(Value::Null)
                });
            let chat = match usable {
                Some(chat) => chat,
                None => crate::chats::create_in(
                    db,
                    json!({ "agentId": connection.agent_id, "projectId": connection.project_id }),
                )?,
            };
            let chat_id = text(&chat, "id");
            let message_id = id();
            let queued = crate::chats::send_in(db, chat_id, json!({ "id": message_id, "text": content }))?;
            db.set(&binding_key, &json!(chat_id), None)?;
            db.set(&rate_key, &json!(count + 1), Some((hour + 1) * 3_600_000))?;
            let receipt = json!({ "chatId": chat_id, "messageId": queued["id"] });
            db.set(&dedupe, &receipt, Some(now() + 30 * 86_400_000))?;
            db.put(
                DELIVERIES,
                &json!({
                    "id": message_id,
                    "connectionId": connection.id,
                    "revision": connection.revision,
                    "chatId": chat_id,
                    "userId": author,
                    "reply": reply,
                    "attempts": 0,
                    "createdAt": now(),
                    "nextAttemptAt": now(),
                }),
            )?;
            db.put(
                "plugin-activity",
                &json!({
                    // Record IDs are unique across kinds; link to the delivery separately.
                    "id": id(),
                    "deliveryId": message_id,
                    "connectionId": connection.id,
                    "chatId": chat_id,
                    "userId": author,
                    "name": event["payload"]["author"]["name"],
                    "preview": content.chars().take(200).collect::<String>(),
                    "status": "working",
                    "createdAt": now(),
                    "updatedAt": now(),
                }),
            )?;
            let mut recent = db
                .list("plugin-activity")?
                .into_iter()
                .filter(|value| value["connectionId"] == connection.id)
                .collect::<Vec<_>>();
            recent.sort_by_key(|value| -value["createdAt"].as_i64().unwrap_or(0));
            for stale in recent.iter().skip(30) {
                db.remove("plugin-activity", text(stale, "id"))?;
            }
            db.audit(
                "plugin.message.admitted",
                &json!({ "connectionId": connection.id, "chatId": chat_id, "messageId": message_id }),
            )?;
            Ok(receipt)
        })
        .await?;
    s.worker.notify();
    Ok(result)
}

/// Correlates only this admitted message to its completed turn. Never exports tools,
/// reasoning or unrelated turns.
pub async fn reply(s: &Service, expected: &Connection, delivery: &Value) -> Result<Option<String>> {
    ensure_owner(s, expected).await?;
    let expected = expected.clone();
    let delivery = delivery.clone();
    s.store.read(move |db| {
        let connection = authorize(db, &expected)?;
        if delivery["revision"] != connection.revision
            || !connection.user_ids.iter().any(|id| delivery["userId"] == *id)
        { return Err(Error::forbidden("Reply authorization was revoked.")); }
        let chat = db.get("chats", text(&delivery, "chatId"))?.ok_or_else(|| Error::not_found("Conversation no longer exists."))?;
        if !crate::conversation_lifecycle::is_active(&chat) { return Err(Error::forbidden("Conversation is no longer active.")); }
        let run_id = text(&chat, "runId");
        if run_id.is_empty() { return Ok(None); }
        let delivered: Option<i64> = db.0.query_row(
            "SELECT id FROM events WHERE run_id=?1 AND type='chat.user' AND json_extract(payload,'$.messageId')=?2 ORDER BY id LIMIT 1",
            rusqlite::params![run_id, delivery["id"].as_str()], |row| row.get(0),
        ).optional()?;
        let Some(delivered) = delivered else { return Ok(None); };
        let completed: Option<i64> = db.0.query_row(
            "SELECT id FROM events WHERE run_id=?1 AND type='turn.completed' AND id>?2 ORDER BY id LIMIT 1",
            rusqlite::params![run_id, delivered], |row| row.get(0),
        ).optional()?;
        if let Some(completed) = completed {
            let final_text: Option<String> = db.0.query_row(
                "SELECT json_extract(payload,'$.item.text') FROM events WHERE run_id=?1 AND id>?2 AND id<?3 AND json_extract(payload,'$.item.type')='agent_message' ORDER BY id DESC LIMIT 1",
                rusqlite::params![run_id, delivered, completed], |row| row.get(0),
            ).optional()?.flatten();
            return Ok(Some(final_text.filter(|value| !value.is_empty()).unwrap_or_else(|| "The agent finished without a text reply. Open the conversation in Cairn.".into())));
        }
        let run = db.run(run_id)?;
        if run.as_ref().is_some_and(|run| matches!(run["status"].as_str(), Some("failed" | "cancelled"))) {
            return Ok(Some("The agent could not finish this request. Review the conversation in Cairn.".into()));
        }
        Ok(None)
    }).await
}

/// Removes a delivery and records how its exchange ended.
pub async fn finish(s: &Service, delivery: &Value, status: &'static str) -> Result<()> {
    let id = text(delivery, "id").to_owned();
    s.store
        .transaction(move |db| {
            db.remove(DELIVERIES, &id)?;
            for mut value in db.list("plugin-activity")? {
                if value["deliveryId"] == id {
                    value["status"] = json!(status);
                    value["updatedAt"] = json!(now());
                    db.put("plugin-activity", &value)?;
                }
            }
            Ok(())
        })
        .await
}
