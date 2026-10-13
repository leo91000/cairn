use super::{AccessMode, Connection, authorize, label, snowflake};
use crate::{
    config::{id, now},
    error::{Error, Result},
    service::Service,
    validation::text,
};
use rusqlite::OptionalExtension;
use serde_json::{Value, json};
use std::{path::Path, sync::Arc, time::Duration};
use tokio_util::sync::CancellationToken;

/// Addressed to this bot in its channel by a person. Who may use it is checked against
/// the stored connection inside the admission transaction.
fn addressed(connection: &Connection, bot: &str, message: &Value) -> bool {
    message["guild_id"] == connection.guild_id
        && message["channel_id"] == connection.channel_id
        && message["author"]["id"].as_str().is_some_and(snowflake)
        && message["author"]["bot"] != true
        && message["webhook_id"].is_null()
        && text(message, "content").len() <= 50_000
        && snowflake(text(message, "id"))
        && message["mentions"]
            .as_array()
            .is_some_and(|mentions| mentions.iter().any(|mention| mention["id"] == bot))
}

const WAITING: &str =
    "Thanks! The owner of this bot needs to approve you first. Mention me again once they have.";

/// Fast durable ingress. Messages from people who aren't allowed are discarded before
/// their content is stored; approval mode records only who asked.
pub async fn accept(
    s: &Service,
    connection: &Connection,
    bot: &str,
    message: &Value,
) -> Result<bool> {
    if !addressed(connection, bot, message) {
        return Ok(false);
    }
    // Allowlists only change with a new revision (or shrink), so this snapshot is a
    // superset: strangers are dropped without touching storage or owner authority.
    if connection.access_mode == AccessMode::Allowlist
        && !connection
            .user_ids
            .iter()
            .any(|id| message["author"]["id"] == *id)
    {
        return Ok(false);
    }
    super::ensure_owner(s, connection).await?;
    let expected = connection.clone();
    let author = &message["author"];
    let name = author["global_name"]
        .as_str()
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| text(author, "username"));
    let message = json!({
        "id": message["id"],
        "guild_id": message["guild_id"],
        "channel_id": message["channel_id"],
        "author": {
            "id": author["id"],
            "name": label(name),
            "bot": false
        },
        "content": message["content"],
        "mentions": [{
            "id": bot
        }],

    });
    let bot = bot.to_owned();
    s.store
        .transaction(move |db| {
            let current = authorize(db, &expected)?;
            let user = text(&message["author"], "id").to_owned();
            if !current.user_ids.contains(&user) {
                if current.access_mode == AccessMode::Requests {
                    request(db, &current, &message)?;
                }
                return Ok(false);
            }
            let dedupe = format!(
                "plugin-discord-seen:{}:{}",
                expected.id,
                message["id"].as_str().unwrap_or("")
            );
            if db.kv(&dedupe)?.is_some() {
                return Ok(false);
            }
            let pending = db
                .list("plugin-inbox")?
                .iter()
                .filter(|value| {
                    value["connectionId"] == expected.id && value["status"] == "pending"
                })
                .count();
            if pending >= 20 {
                return Ok(false);
            }
            let record = json!({
                "id": id(),
                "connectionId": expected.id,
                "revision": expected.revision,
                "botId": bot,
                "message": message,
                "status": "pending",
                "createdAt": now(),

            });
            db.put("plugin-inbox", &record)?;
            db.set(&dedupe, &json!(true), Some(now() + 30 * 86_400_000))?;
            Ok(true)
        })
        .await
}

/// One request per person; the first one also answers in Discord so they aren't ignored.
fn request(db: &crate::store::Db<'_>, connection: &Connection, message: &Value) -> Result<()> {
    let user = text(&message["author"], "id");
    let requests = db.list("plugin-requests")?;
    if let Some(mut existing) = requests
        .iter()
        .find(|value| value["connectionId"] == connection.id && value["userId"] == user)
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
            "userId": user,
            "name": message["author"]["name"],
            "status": "pending",
            "mentions": 1,
            "createdAt": now(),
            "updatedAt": now(),
        }),
    )?;
    let outstanding = db
        .list("plugin-deliveries")?
        .iter()
        .filter(|value| value["connectionId"] == connection.id)
        .count();
    if outstanding < 20 {
        db.put(
            "plugin-deliveries",
            &json!({
                "id": id(),
                "connectionId": connection.id,
                "revision": connection.revision,
                "externalMessageId": message["id"],
                "channelId": connection.channel_id,
                "userId": user,
                "notice": WAITING,
                "createdAt": now(),
                "nextAttemptAt": now(),
                "chunk": 0,
            }),
        )?;
    }
    db.audit(
        "plugin.access.requested",
        &json!({ "connectionId": connection.id }),
    )
}

/// Activity is the owner's view of recent exchanges; it keeps the latest per bot.
fn activity(db: &crate::store::Db<'_>, delivery: &str, status: &str) -> Result<()> {
    for mut value in db.list("plugin-activity")? {
        if value["deliveryId"] == delivery {
            value["status"] = json!(status);
            value["updatedAt"] = json!(now());
            db.put("plugin-activity", &value)?;
        }
    }
    Ok(())
}

pub async fn process_pending(
    s: &Service,
    connection: &Connection,
    executable: &Path,
) -> Result<()> {
    let inbox = s.store.list("plugin-inbox").await?;
    for mut record in inbox
        .into_iter()
        .filter(|record| record["connectionId"] == connection.id && record["status"] == "pending")
    {
        if now() < record["nextAttemptAt"].as_i64().unwrap_or(0) {
            continue;
        }
        if record["revision"] != connection.revision {
            remove_record(s, "plugin-inbox", text(&record, "id")).await?;
            continue;
        }
        let input = json!({
            "botId": record["botId"],
            "message": record["message"]
        });
        let result =
            crate::plugins::invoke_discord(s, executable, input, connection, &record["message"])
                .await;
        match result {
            Ok(_) => {
                remove_record(s, "plugin-inbox", text(&record, "id")).await?;
            }
            Err(error) => {
                let attempts = record["attempts"].as_u64().unwrap_or(0);
                if error.status >= 500 && attempts < 5 {
                    record["attempts"] = json!(attempts + 1);
                    record["nextAttemptAt"] = json!(now() + 5000);
                    s.store.put("plugin-inbox", record).await?;
                    continue;
                }
                // Permanent failure rejects this message; the audit contains no content.
                remove_record(s, "plugin-inbox", text(&record, "id")).await?;
                let connection_id = connection.id.clone();
                s.store
                    .transaction(move |db| {
                        db.audit(
                            "plugin.message.rejected",
                            &json!({
                                "connectionId": connection_id
                            }),
                        )
                    })
                    .await?;
                if error.status == 403 {
                    return Err(error);
                }
            }
        }
    }
    Ok(())
}

pub async fn submit(
    s: &Service,
    expected: &Connection,
    message: &Value,
    content: &str,
) -> Result<Value> {
    super::ensure_owner(s, expected).await?;
    let expected = expected.clone();
    let message = message.clone();
    let content = content.trim().to_owned();
    if content.is_empty() || content.len() > 50_000 {
        return Err(Error::bad("Invalid bot message."));
    }
    let result = s
        .store
        .transaction(move |db| {
            let connection = authorize(db, &expected)?;
            // The host identity and routing come from ingress, never from script arguments.
            if message["guild_id"] != connection.guild_id
                || message["channel_id"] != connection.channel_id
                || !connection
                    .user_ids
                    .iter()
                    .any(|id| message["author"]["id"] == *id)
                || message["author"]["bot"] == true
                || !message["webhook_id"].is_null()
            {
                return Err(Error::forbidden("Discord message is not authorized."));
            }
            let external_id = text(&message, "id");
            let dedupe = format!("plugin-discord-submitted:{}:{external_id}", connection.id);
            if let Some(value) = db.kv(&dedupe)? {
                return Ok(value);
            }
            let hour = now() / 3_600_000;
            let rate_key = format!("plugin-discord-rate:{}:{hour}", connection.id);
            let count = db
                .kv(&rate_key)?
                .and_then(|value| value.as_u64())
                .unwrap_or(0);
            if count >= u64::from(connection.max_messages_per_hour) {
                return Err(Error::too_many_requests(
                    "Discord bot message limit reached.",
                ));
            }
            let outstanding = db
                .list("plugin-deliveries")?
                .iter()
                .filter(|value| value["connectionId"] == connection.id)
                .count();
            if outstanding >= 20 {
                return Err(Error::too_many_requests(
                    "Discord bot has too much pending work.",
                ));
            }
            let binding_key = format!(
                "plugin-discord-chat:{}:{}:{}",
                connection.id,
                connection.channel_id,
                text(&message["author"], "id")
            );
            let bound = db
                .kv(&binding_key)?
                .and_then(|value| value.as_str().map(str::to_owned));
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
                    json!({
                        "agentId": connection.agent_id,
                        "projectId": connection.project_id
                    }),
                )?,
            };
            let chat_id = text(&chat, "id");
            let message_id = id();
            let queued = crate::chats::send_in(
                db,
                chat_id,
                json!({
                    "id": message_id,
                    "text": content
                }),
            )?;
            db.set(&binding_key, &json!(chat_id), None)?;
            db.set(&rate_key, &json!(count + 1), Some((hour + 1) * 3_600_000))?;
            let receipt = json!({
                "chatId": chat_id,
                "messageId": queued["id"]
            });
            db.set(&dedupe, &receipt, Some(now() + 30 * 86_400_000))?;
            db.put(
                "plugin-deliveries",
                &json!({
                    "id": message_id,
                    "connectionId": connection.id,
                    "revision": connection.revision,
                    "chatId": chat_id,
                    "externalMessageId": external_id,
                    "channelId": connection.channel_id,
                    "userId": message["author"]["id"],
                    "createdAt": now(),
                    "nextAttemptAt": now(),
                    "chunk": 0,

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
                    "userId": message["author"]["id"],
                    "name": message["author"]["name"],
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
                &json!({
                    "connectionId": connection.id,
                    "chatId": chat_id,
                    "messageId": message_id
                }),
            )?;
            Ok(receipt)
        })
        .await?;
    s.worker.notify();
    Ok(result)
}

/// Correlates only this bot's admitted message to its completed turn. Never exports tools.
pub async fn reply(s: &Service, expected: &Connection, delivery: &Value) -> Result<Option<String>> {
    super::ensure_owner(s, expected).await?;
    let expected = expected.clone();
    let delivery = delivery.clone();
    s.store.read(move |db| {
        let connection = authorize(db, &expected)?;
        if let Some(notice) = delivery["notice"].as_str() {
            if delivery["revision"] != connection.revision { return Err(Error::forbidden("Discord reply authorization was revoked.")); }
            return Ok(Some(notice.to_owned()));
        }
        if delivery["revision"] != connection.revision
            || !connection.user_ids.iter().any(|id| delivery["userId"] == *id)
        { return Err(Error::forbidden("Discord reply authorization was revoked.")); }
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

pub(super) async fn maintain(
    s: Arc<Service>,
    connection: Connection,
    token: String,
    stop: CancellationToken,
) -> Result<()> {
    let executable = crate::plugins::executable()?;
    let mut owner_check = tokio::time::Instant::now();
    let mut timer = tokio::time::interval(Duration::from_secs(1));
    timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            () = stop.cancelled() => return Ok(()),
            _ = timer.tick() => {},
        }
        if owner_check.elapsed() >= Duration::from_secs(30) {
            super::ensure_owner(&s, &connection).await?;
            owner_check = tokio::time::Instant::now();
        }
        process_pending(&s, &connection, &executable).await?;
        deliver_pending(&s, &connection, &token, "https://discord.com/api/v10").await?;
    }
}

async fn deliver_pending(
    s: &Service,
    connection: &Connection,
    token: &str,
    origin: &str,
) -> Result<()> {
    let cooldown_key = cooldown_key(token);
    let cooldown = s
        .store
        .kv(&cooldown_key)
        .await?
        .and_then(|value| value.as_i64())
        .unwrap_or(0);
    if now() < cooldown {
        return Ok(());
    }
    let deliveries = s.store.list("plugin-deliveries").await?;
    for mut delivery in deliveries
        .into_iter()
        .filter(|value| value["connectionId"] == connection.id)
    {
        if delivery["revision"] != connection.revision
            || now() - delivery["createdAt"].as_i64().unwrap_or(0) > 86_400_000
        {
            finish(s, &delivery, "failed").await?;
            continue;
        }
        if now() < delivery["nextAttemptAt"].as_i64().unwrap_or(0) {
            continue;
        }
        let Some(reply) = reply(s, connection, &delivery).await? else {
            continue;
        };
        let chunks = chunks(&reply);
        let index = delivery["chunk"].as_u64().unwrap_or(0) as usize;
        let Some(content) = chunks.get(index) else {
            finish(s, &delivery, "replied").await?;
            continue;
        };
        let nonce =
            crate::auth::hex_digest(&format!("{}:{index}", text(&delivery, "id")))[..24].to_owned();
        let mut body = json!({
            "content": content,
            "allowed_mentions": {
                "parse": []
            },
            "nonce": nonce,
            "enforce_nonce": true,

        });
        if index == 0 {
            body["message_reference"] = json!({
                "message_id": delivery["externalMessageId"],
                "fail_if_not_exists": false
            });
        }
        let response = s
            .http
            .post(format!(
                "{origin}/channels/{}/messages",
                connection.channel_id
            ))
            .header("authorization", format!("Bot {token}"))
            .header("user-agent", super::USER_AGENT)
            .json(&body)
            .send()
            .await
            .map_err(|_| Error::bad_gateway("Discord reply delivery interrupted."))?;
        if response.status().as_u16() == 429 {
            let limits: Value = response.json().await.unwrap_or(Value::Null);
            let seconds = limits["retry_after"]
                .as_f64()
                .filter(|seconds| seconds.is_finite())
                .unwrap_or(5.0)
                .clamp(1.0, 300.0);
            let retry_at = now() + (seconds * 1000.0).ceil() as i64;
            delivery["nextAttemptAt"] = json!(retry_at);
            // Share the cooldown across connections using this bot token, and
            // retain it across manager restarts. Never persist the token itself.
            s.store
                .set(&cooldown_key, json!(retry_at), Some(retry_at))
                .await?;
            s.store.put("plugin-deliveries", delivery).await?;
            // Discord can rate-limit globally; do not send another reply this pass.
            break;
        }
        if !response.status().is_success() {
            if response.status().is_client_error() {
                return Err(Error::forbidden(
                    "Discord rejected reply delivery. Review bot permissions.",
                ));
            }
            return Err(Error::bad_gateway("Discord reply delivery failed."));
        }
        // The nonce makes immediate retries idempotent within Discord's nonce window.
        if index + 1 == chunks.len() {
            finish(s, &delivery, "replied").await?;
        } else {
            delivery["chunk"] = json!(index + 1);
            s.store.put("plugin-deliveries", delivery).await?;
        }
    }
    Ok(())
}

fn cooldown_key(token: &str) -> String {
    format!("plugin-discord-cooldown:{}", crate::auth::hex_digest(token))
}

fn chunks(text: &str) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut chunk = String::new();
    let mut units = 0;
    for character in text.chars().take(16_000) {
        let length = character.len_utf16();
        if units + length > 1900 {
            chunks.push(std::mem::take(&mut chunk));
            units = 0;
        }
        chunk.push(character);
        units += length;
    }
    if !chunk.is_empty() {
        chunks.push(chunk);
    }
    chunks
}

async fn finish(s: &Service, delivery: &Value, status: &'static str) -> Result<()> {
    let id = text(delivery, "id").to_owned();
    s.store
        .transaction(move |db| {
            db.remove("plugin-deliveries", &id)?;
            activity(db, &id, status)
        })
        .await
}

async fn remove_record(s: &Service, kind: &'static str, id: &str) -> Result<()> {
    let id = id.to_owned();
    s.store.transaction(move |db| db.remove(kind, &id)).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Json, Router,
        extract::State,
        http::{HeaderMap, StatusCode},
        response::IntoResponse,
        routing::post,
    };
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::sync::Mutex;

    #[derive(Default)]
    struct Discord {
        attempts: AtomicUsize,
        bodies: Mutex<Vec<Value>>,
    }

    async fn receive(
        State(discord): State<Arc<Discord>>,
        headers: HeaderMap,
        Json(body): Json<Value>,
    ) -> axum::response::Response {
        assert_eq!(headers["authorization"], "Bot synthetic_discord_token");
        assert_eq!(headers["user-agent"], super::super::USER_AGENT);
        discord.bodies.lock().await.push(body);
        if discord.attempts.fetch_add(1, Ordering::SeqCst) == 0 {
            return (
                StatusCode::TOO_MANY_REQUESTS,
                Json(json!({
                    "retry_after": 1,
                    "global": true
                })),
            )
                .into_response();
        }
        Json(json!({
            "id": "600"
        }))
        .into_response()
    }

    #[tokio::test]
    async fn delivery_retries_rate_limits_with_same_nonce_and_disables_mentions() {
        let (_root, s, connection) = super::super::tests::fixture().await;
        let receipt = submit(&s, &connection, &super::super::tests::message(), "hello")
            .await
            .unwrap();
        let chat_id = text(&receipt, "chatId").to_owned();
        let run_id = id();
        let run = json!({
            "id": run_id,
            "taskId": chat_id,
            "projectId": "",
            "createdAt": now(),
            "status": "succeeded"
        });
        let message_id = receipt["messageId"].clone();
        s.store
            .transaction(move |db| {
                db.add_run(&run, None)?;
                let mut chat = db.get("chats", &chat_id)?.unwrap();
                chat["runId"] = run["id"].clone();
                db.put("chats", &chat)?;
                db.event(
                    &run_id,
                    "chat.user",
                    "hello",
                    Some(&json!({
                        "messageId": message_id
                    })),
                )?;
                db.event(
                    &run_id,
                    "item.completed",
                    "@everyone hello",
                    Some(&json!({
                        "item": {
                            "type": "agent_message",
                            "text": "@everyone hello"
                        }
                    })),
                )?;
                db.event(&run_id, "turn.completed", "", None)
            })
            .await
            .unwrap();
        let mock = Arc::new(Discord::default());
        let app = Router::new()
            .route("/channels/200/messages", post(receive))
            .with_state(mock.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        deliver_pending(&s, &connection, "synthetic_discord_token", &origin)
            .await
            .unwrap();
        assert_eq!(mock.attempts.load(Ordering::SeqCst), 1);
        deliver_pending(&s, &connection, "synthetic_discord_token", &origin)
            .await
            .unwrap();
        assert_eq!(mock.attempts.load(Ordering::SeqCst), 1);
        let mut delivery = s.store.list("plugin-deliveries").await.unwrap().remove(0);
        assert!(delivery["nextAttemptAt"].as_i64().unwrap() > now());
        // Advance this persisted retry deadline without a wall-clock sleep.
        delivery["nextAttemptAt"] = json!(now());
        s.store.put("plugin-deliveries", delivery).await.unwrap();
        s.store
            .set(&cooldown_key("synthetic_discord_token"), json!(now()), None)
            .await
            .unwrap();
        deliver_pending(&s, &connection, "synthetic_discord_token", &origin)
            .await
            .unwrap();
        assert!(s.store.list("plugin-deliveries").await.unwrap().is_empty());
        assert_eq!(
            s.store.list("plugin-activity").await.unwrap()[0]["status"],
            "replied"
        );
        let bodies = mock.bodies.lock().await;
        assert_eq!(bodies.len(), 2);
        assert_eq!(bodies[0]["nonce"], bodies[1]["nonce"]);
        assert_eq!(bodies[1]["allowed_mentions"]["parse"], json!([]));
        assert_eq!(bodies[1]["enforce_nonce"], true);
        server.abort();
    }

    #[test]
    fn discord_chunks_respect_utf16_limits() {
        let reply = "😀".repeat(2000);
        let parts = chunks(&reply);
        assert_eq!(parts.concat(), reply);
        assert!(parts.iter().all(|part| part.encode_utf16().count() <= 1900));
    }
}
