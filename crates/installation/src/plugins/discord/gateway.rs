use super::{Connection, label, snowflake};
use crate::{
    error::{Error, Result},
    plugins::{
        connections::{Label, status},
        work::{Inbound, accept},
    },
    service::Service,
    validation::text,
};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::time::Duration;
use tokio_tungstenite::{
    connect_async_with_config,
    tungstenite::{Message, protocol::WebSocketConfig},
};
use tokio_util::sync::CancellationToken;

static LAST_IDENTIFY: tokio::sync::Mutex<Option<tokio::time::Instant>> =
    tokio::sync::Mutex::const_new(None);

#[derive(Default)]
pub(super) struct Session {
    id: Option<String>,
    sequence: Option<u64>,
    resume_url: Option<String>,
    bot_id: String,
}

pub(super) async fn run(
    s: &Service,
    connection: &Connection,
    token: &str,
    stop: &CancellationToken,
    session: &mut Session,
) -> Result<()> {
    let url = session
        .resume_url
        .as_deref()
        .unwrap_or("wss://gateway.discord.gg");
    run_at(s, connection, token, stop, session, url.to_owned()).await
}

async fn run_at(
    s: &Service,
    connection: &Connection,
    token: &str,
    stop: &CancellationToken,
    session: &mut Session,
    base: String,
) -> Result<()> {
    // Serialize new sessions conservatively. Waiting precedes Hello, so it cannot
    // delay a connected socket's heartbeat. Resumes do not consume Identify slots.
    let mut identify_slot = if session.id.is_none() || session.sequence.is_none() {
        let slot = LAST_IDENTIFY.lock().await;
        if let Some(last) = *slot {
            tokio::time::sleep_until(last + Duration::from_secs(5)).await;
        }
        Some(slot)
    } else {
        None
    };
    let config = WebSocketConfig::default()
        .max_message_size(Some(1_048_576))
        .max_frame_size(Some(1_048_576));
    let url = format!("{}/?v=10&encoding=json", base.trim_end_matches('/'));
    let (mut socket, _) = tokio::time::timeout(
        Duration::from_secs(15),
        connect_async_with_config(&url, Some(config), false),
    )
    .await
    .map_err(|_| Error::bad_gateway("Discord Gateway timed out."))?
    .map_err(|_| Error::bad_gateway("Discord Gateway unavailable."))?;
    let hello = tokio::time::timeout(Duration::from_secs(15), socket.next())
        .await
        .map_err(|_| Error::bad_gateway("Discord Gateway did not send Hello."))?
        .ok_or_else(|| Error::bad_gateway("Discord Gateway closed."))?
        .map_err(|_| Error::bad_gateway("Discord Gateway interrupted."))?;
    let hello: Value = serde_json::from_str(
        hello
            .to_text()
            .map_err(|_| Error::bad_gateway("Invalid Discord Hello."))?,
    )
    .map_err(|_| Error::bad_gateway("Invalid Discord Hello."))?;
    let interval = hello["d"]["heartbeat_interval"]
        .as_u64()
        .filter(|value| *value >= 100 && *value <= 120_000)
        .filter(|_| hello["op"] == 10)
        .ok_or_else(|| Error::bad_gateway("Invalid Discord heartbeat interval."))?;
    let identify = if let (Some(id), Some(sequence)) = (&session.id, session.sequence) {
        json!({
            "op": 6,
            "d": {
                "token": token,
                "session_id": id,
                "seq": sequence
            }
        })
    } else {
        // Guild messages + guilds. Mention payloads do not require Message Content intent.
        json!({
            "op": 2,
            "d": {
                "token": token,
                "intents": 513,
                "properties": {
                    "os": "linux",
                    "browser": "cairn",
                    "device": "cairn"
                },

            },

        })
    };
    socket
        .send(Message::Text(identify.to_string().into()))
        .await
        .map_err(|_| Error::bad_gateway("Discord identification failed."))?;
    if let Some(mut slot) = identify_slot.take() {
        *slot = Some(tokio::time::Instant::now());
    }
    let heartbeat_interval = Duration::from_millis(interval);
    let jitter = rand::random::<f64>();
    let mut next_heartbeat = tokio::time::Instant::now() + heartbeat_interval.mul_f64(jitter);
    let mut awaiting_ack = false;
    loop {
        tokio::select! {
            () = stop.cancelled() => return Ok(()),
            () = tokio::time::sleep_until(next_heartbeat) => {
                if awaiting_ack { return Err(Error::bad_gateway("Discord heartbeat acknowledgement missed.")); }
                let heartbeat = json!({
                    "op": 1,
                    "d": session.sequence
                });
                socket.send(Message::Text(heartbeat.to_string().into())).await
                    .map_err(|_| Error::bad_gateway("Discord heartbeat failed."))?;
                awaiting_ack = true;
                next_heartbeat = tokio::time::Instant::now() + heartbeat_interval;
            },
            frame = socket.next() => {
                let frame = frame.ok_or_else(|| Error::bad_gateway("Discord Gateway closed."))?
                    .map_err(|_| Error::bad_gateway("Discord Gateway interrupted."))?;
                match frame {
                    Message::Ping(bytes) => {
                        socket.send(Message::Pong(bytes)).await.map_err(|_| Error::bad_gateway("Discord Gateway interrupted."))?;
                        continue;
                    }
                    Message::Close(close) => {
                        let code = close.map(|close| u16::from(close.code));
                        if matches!(code, Some(4004 | 4010 | 4011 | 4012 | 4013 | 4014)) {
                            return Err(Error::forbidden("Discord rejected the bot configuration."));
                        }
                        if matches!(code, Some(4007 | 4009)) { *session = Session::default(); }
                        return Err(Error::bad_gateway("Discord Gateway disconnected."));
                    }
                    Message::Text(_) => {},
                    _ => continue,
                }
                let value: Value = serde_json::from_str(frame.to_text().unwrap_or(""))
                    .map_err(|_| Error::bad_gateway("Invalid Discord event."))?;
                match value["op"].as_u64() {
                    Some(0) => {
                        match value["t"].as_str() {
                            Some("READY") => {
                                if value["d"]["user"]["bot"] != true {
                                    return Err(Error::forbidden("Use a Discord bot token."));
                                }
                                session.id = value["d"]["session_id"].as_str().map(str::to_owned);
                                session.bot_id = value["d"]["user"]["id"].as_str().unwrap_or("").to_owned();
                                session.resume_url = value["d"]["resume_gateway_url"].as_str().filter(|url| safe_resume_url(url)).map(str::to_owned);
                                status(s, connection, "connected").await;
                            }
                            Some("RESUMED") => status(s, connection, "connected").await,
                            Some("MESSAGE_CREATE") if !session.bot_id.is_empty() => {
                                if let Some(inbound) = inbound(connection, &session.bot_id, &value["d"]) {
                                    accept(s, connection, inbound).await?;
                                }
                            }
                            _ => {},
                        }
                        // Advance only after durable admission; replayed messages deduplicate.
                        if let Some(sequence) = value["s"].as_u64() { session.sequence = Some(sequence); }
                    }
                    Some(11) => awaiting_ack = false,
                    Some(1) => {
            socket.send(Message::Text(json!({
                            "op": 1,
                            "d": session.sequence
                        }).to_string().into())).await
                            .map_err(|_| Error::bad_gateway("Discord heartbeat failed."))?;
                    }
                    Some(7) => return Err(Error::bad_gateway("Discord requested reconnect.")),
                    Some(9) => {
                        if value["d"] != true { *session = Session::default(); }
                        return Err(Error::bad_gateway("Discord session expired."));
                    }
                    _ => {},
                }
            },
        }
    }
}

/// The transport's scope: a person mentioning this bot in the configured channel.
/// Without the Message Content intent, other messages carry no text anyway.
pub fn inbound(connection: &Connection, bot: &str, message: &Value) -> Option<Inbound> {
    let author = &message["author"];
    let addressed = message["guild_id"] == connection.settings["guildId"]
        && message["channel_id"] == connection.settings["channelId"]
        && author["id"].as_str().is_some_and(snowflake)
        && author["bot"] != true
        && message["webhook_id"].is_null()
        && text(message, "content").len() <= 50_000
        && snowflake(text(message, "id"))
        && message["mentions"]
            .as_array()
            .is_some_and(|mentions| mentions.iter().any(|mention| mention["id"] == bot));
    if !addressed {
        return None;
    }
    let name = author["global_name"]
        .as_str()
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| text(author, "username"));
    Some(Inbound {
        id: text(message, "id").to_owned(),
        author: Label {
            id: text(author, "id").to_owned(),
            name: label(name),
        },
        context: json!({
            "botId": bot,
            "guildId": message["guild_id"],
            "channelId": message["channel_id"],
        }),
        body: json!({ "content": message["content"] }),
    })
}

fn safe_resume_url(value: &str) -> bool {
    url::Url::parse(value).is_ok_and(|url| {
        url.scheme() == "wss"
            && url.username().is_empty()
            && url.password().is_none()
            && url.port().is_none()
            && url
                .host_str()
                .is_some_and(|host| host == "gateway.discord.gg" || host.ends_with(".discord.gg"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;
    use tokio_tungstenite::accept_async;

    #[tokio::test]
    async fn gateway_identifies_heartbeats_persists_mentions_and_retains_resume_state() {
        let (_root, s, connection) = super::super::tests::fixture().await;
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("ws://{}", listener.local_addr().unwrap());
        let upstream = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = accept_async(stream).await.unwrap();
            socket
                .send(Message::Text(
                    json!({
                        "op": 10,
                        "d": {
                            "heartbeat_interval": 200
                        }
                    })
                    .to_string()
                    .into(),
                ))
                .await
                .unwrap();
            let identify = socket.next().await.unwrap().unwrap();
            let identify: Value = serde_json::from_str(identify.to_text().unwrap()).unwrap();
            assert_eq!(identify["op"], 2);
            assert_eq!(identify["d"]["intents"], 513);
            assert_eq!(identify["d"]["token"], "synthetic_discord_token");
            socket
                .send(Message::Text(
                    json!({
                        "op": 0,
                        "s": 1,
                        "t": "READY",
                        "d": {
                            "session_id": "synthetic_session",
                            "user": {
                                "id": "400",
                                "bot": true
                            },
                            "resume_gateway_url": "wss://gateway-us-east1-b.discord.gg"
                        }
                    })
                    .to_string()
                    .into(),
                ))
                .await
                .unwrap();
            socket
                .send(Message::Text(
                    json!({
                        "op": 0,
                        "s": 2,
                        "t": "MESSAGE_CREATE",
                        "d": super::super::tests::message()
                    })
                    .to_string()
                    .into(),
                ))
                .await
                .unwrap();
            // A replayed Gateway dispatch must not create a second inbox record.
            socket
                .send(Message::Text(
                    json!({
                        "op": 0,
                        "s": 3,
                        "t": "MESSAGE_CREATE",
                        "d": super::super::tests::message()
                    })
                    .to_string()
                    .into(),
                ))
                .await
                .unwrap();
            let heartbeat = socket.next().await.unwrap().unwrap();
            let heartbeat: Value = serde_json::from_str(heartbeat.to_text().unwrap()).unwrap();
            assert_eq!(heartbeat["op"], 1);
            socket
                .send(Message::Text(
                    json!({
                        "op": 11,
                        "d": null
                    })
                    .to_string()
                    .into(),
                ))
                .await
                .unwrap();
            socket
                .send(Message::Text(
                    json!({
                        "op": 7,
                        "d": null
                    })
                    .to_string()
                    .into(),
                ))
                .await
                .unwrap();
        });
        let mut session = Session::default();
        let stop = CancellationToken::new();
        let result = tokio::time::timeout(
            Duration::from_secs(3),
            run_at(
                &s,
                &connection,
                "synthetic_discord_token",
                &stop,
                &mut session,
                origin,
            ),
        )
        .await
        .unwrap();
        assert!(result.is_err());
        upstream.await.unwrap();
        assert_eq!(session.sequence, Some(3));
        assert_eq!(session.id.as_deref(), Some("synthetic_session"));
        assert_eq!(s.store.list("plugin-events").await.unwrap().len(), 1);
        assert!(!safe_resume_url("wss://localhost"));
        assert!(!safe_resume_url(
            "wss://gateway.discord.gg.attacker.example"
        ));
    }
}
