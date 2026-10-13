//! Discord's native transport: the Gateway connection and setup discovery. What the
//! bot does with a message (mentions, notices, reply formatting) lives in Luau, in
//! `plugins/discord/main.luau`, behind the generic plugin capabilities.
mod discover;
mod gateway;

use super::connections::{Connection, credential, status};
pub(crate) use super::connections::{credential_key, label};
use crate::{
    error::{Error, Result},
    service::Service,
    validation::text,
};
pub use discover::discover;
pub use gateway::inbound;
use serde_json::{Value, json};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

pub const USER_AGENT: &str = concat!(
    "DiscordBot (https://github.com/leo91000/cairn, ",
    env!("CARGO_PKG_VERSION"),
    ")"
);

pub fn snowflake(value: &str) -> bool {
    !value.is_empty() && value.len() <= 20 && value.bytes().all(|c| c.is_ascii_digit())
}

/// The server and channel are the transport's scope: messages elsewhere never reach
/// storage or the plugin.
pub fn validate_scope(settings: &Value) -> Result<()> {
    if !snowflake(text(settings, "guildId")) || !snowflake(text(settings, "channelId")) {
        return Err(Error::bad("Choose a Discord server and channel."));
    }
    Ok(())
}

/// Owner-only token check for a saved connection, even while it is paused.
pub async fn test(s: &Service, connection: &Connection) -> Result<Value> {
    let saved = s
        .vault
        .get(&credential_key(&connection.id))
        .await?
        .ok_or_else(|| Error::bad("Discord token is missing."))?;
    let response = s
        .http
        .get("https://discord.com/api/v10/users/@me")
        .header("user-agent", USER_AGENT)
        .header("authorization", format!("Bot {}", text(&saved, "token")))
        .send()
        .await
        .map_err(|_| Error::bad_gateway("Discord is unavailable."))?;
    if !response.status().is_success() {
        return Err(Error::bad_gateway("Discord rejected the bot token."));
    }
    let user: Value = response
        .json()
        .await
        .map_err(|_| Error::bad_gateway("Invalid Discord response."))?;
    if user["bot"] != true {
        return Err(Error::bad("Use a bot token, not a user account token."));
    }
    Ok(json!({ "ok": true, "botId": user["id"], "name": user["username"] }))
}

/// Keeps the Gateway session alive until stopped. Returns an error only when Discord
/// or Cairn refuses the connection for good (token reset, revoked grant…).
pub async fn transport(
    s: &Service,
    connection: &Connection,
    stop: &CancellationToken,
) -> Result<()> {
    let mut retry = Duration::from_secs(5);
    let mut session = gateway::Session::default();
    loop {
        let token = credential(s, connection)
            .await?
            .ok_or_else(|| Error::forbidden("Discord token is missing."))?;
        match gateway::run(s, connection, &token, stop, &mut session).await {
            Ok(()) => return Ok(()),
            Err(error) if matches!(error.status, 401 | 403) => return Err(error),
            Err(_) => status(s, connection, "connecting").await,
        }
        tokio::select! {
            () = stop.cancelled() => return Ok(()),
            () = tokio::time::sleep(retry) => {},
        }
        retry = (retry * 2).min(Duration::from_secs(60));
    }
}

#[cfg(test)]
mod tests;
