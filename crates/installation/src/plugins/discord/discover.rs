//! Owner-only setup discovery: who the bot is, where it was invited, and where it can reply.
use super::{credential_key, label, snowflake};
use crate::{
    error::{Error, Result},
    service::Service,
    validation::{text, uuid},
};
use serde_json::{Value, json};

const ORIGIN: &str = "https://discord.com/api/v10";
const ADMINISTRATOR: u64 = 1 << 3;
const VIEW_CHANNEL: u64 = 1 << 10;
const SEND_MESSAGES: u64 = 1 << 11;
const READ_MESSAGE_HISTORY: u64 = 1 << 16;
/// Exactly what mention replies need; the invite never requests more.
pub const INVITE_PERMISSIONS: u64 = VIEW_CHANNEL | SEND_MESSAGES | READ_MESSAGE_HISTORY;

pub async fn discover(s: &Service, plugin_id: &str, input: &Value) -> Result<Value> {
    let token = token(s, plugin_id, input).await?;
    discover_at(s, &token, input, ORIGIN).await
}

/// A pasted token is checked before it is saved; editing reuses the vault copy.
async fn token(s: &Service, plugin_id: &str, input: &Value) -> Result<String> {
    let token = text(input, "token").trim();
    if !token.is_empty() {
        if token.len() > 4096 || token.chars().any(|c| c.is_whitespace() || c.is_control()) {
            return Err(Error::bad("Enter a Discord bot token."));
        }
        return Ok(token.to_owned());
    }
    let id = text(input, "connectionId");
    uuid(id)?;
    crate::plugins::connections::get(s, plugin_id, id).await?;
    let saved = s
        .vault
        .get(&credential_key(id))
        .await?
        .ok_or_else(|| Error::bad("Enter a Discord bot token."))?;
    Ok(text(&saved, "token").to_owned())
}

async fn get(s: &Service, origin: &str, token: &str, path: &str) -> Result<Value> {
    let response = s
        .http
        .get(format!("{origin}{path}"))
        .header("user-agent", super::USER_AGENT)
        .header("authorization", format!("Bot {token}"))
        .send()
        .await
        .map_err(|_| Error::bad_gateway("Discord is unavailable."))?;
    match response.status().as_u16() {
        200..=299 => response
            .json()
            .await
            .map_err(|_| Error::bad_gateway("Invalid Discord response.")),
        401 => Err(Error::bad(
            "Discord rejected this token. Reset it in the Developer Portal and paste the new one.",
        )),
        429 => Err(Error::too_many_requests(
            "Discord is rate limiting this bot. Try again in a few seconds.",
        )),
        403 | 404 => Err(Error::bad(
            "The bot can’t see this server. Invite it, then refresh.",
        )),
        _ => Err(Error::bad_gateway("Discord is unavailable.")),
    }
}

fn person(user: &Value) -> Value {
    let name = user["global_name"]
        .as_str()
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| text(user, "username"));
    json!({
        "id": user["id"],
        "name": label(name),
        "username": label(text(user, "username")),
    })
}

pub(super) async fn discover_at(
    s: &Service,
    token: &str,
    input: &Value,
    origin: &str,
) -> Result<Value> {
    let bot = get(s, origin, token, "/users/@me").await?;
    if bot["bot"] != true {
        return Err(Error::bad("Use a bot token, not a user account token."));
    }
    let bot_id = text(&bot, "id").to_owned();
    // Older applications can have a client ID that differs from the bot user ID.
    let application = get(s, origin, token, "/oauth2/applications/@me")
        .await
        .ok()
        .and_then(|value| value["id"].as_str().map(str::to_owned))
        .filter(|id| snowflake(id))
        .unwrap_or_else(|| bot_id.clone());
    let guilds = get(s, origin, token, "/users/@me/guilds").await?;
    let guilds = guilds
        .as_array()
        .into_iter()
        .flatten()
        .filter(|guild| guild["id"].as_str().is_some_and(snowflake))
        .map(|guild| {
            json!({
                "id": guild["id"],
                "name": label(text(guild, "name")),
            })
        })
        .collect::<Vec<_>>();
    let mut result = json!({
        "bot": person(&bot),
        "applicationId": application,
        "inviteUrl": format!(
            "https://discord.com/oauth2/authorize?client_id={application}&scope=bot&permissions={INVITE_PERMISSIONS}"
        ),
        "guilds": guilds,
    });
    let guild_id = text(input, "guildId");
    if !guild_id.is_empty() {
        if !result["guilds"]
            .as_array()
            .is_some_and(|guilds| guilds.iter().any(|guild| guild["id"] == guild_id))
        {
            return Err(Error::bad(
                "The bot isn’t in this server yet. Invite it, then refresh.",
            ));
        }
        let guild = get(s, origin, token, &format!("/guilds/{guild_id}")).await?;
        let member = get(
            s,
            origin,
            token,
            &format!("/guilds/{guild_id}/members/{bot_id}"),
        )
        .await?;
        let channels = get(s, origin, token, &format!("/guilds/{guild_id}/channels")).await?;
        let mut listed = channels
            .as_array()
            .into_iter()
            .flatten()
            // Text and announcement channels accept bot replies to mentions.
            .filter(|channel| matches!(channel["type"].as_u64(), Some(0 | 5)))
            .map(|channel| {
                let allowed = permissions(&guild, &member, &bot_id, channel);
                let read = allowed & VIEW_CHANNEL != 0;
                json!({
                    "id": channel["id"],
                    "name": label(text(channel, "name")),
                    "position": channel["position"].as_i64().unwrap_or(0),
                    "canRead": read,
                    "canReply": read && allowed & (SEND_MESSAGES | READ_MESSAGE_HISTORY)
                        == SEND_MESSAGES | READ_MESSAGE_HISTORY,
                })
            })
            .collect::<Vec<_>>();
        listed.sort_by_key(|channel| channel["position"].as_i64().unwrap_or(0));
        result["channels"] = json!(listed);
    }
    if let Some(users) = input["userIds"].as_array() {
        let mut people = Vec::new();
        for user in users
            .iter()
            .filter_map(Value::as_str)
            .filter(|id| snowflake(id))
            .take(10)
        {
            // Unknown IDs stay unnamed; saving still validates their format.
            if let Ok(value) = get(s, origin, token, &format!("/users/{user}")).await {
                people.push(person(&value));
            }
        }
        result["users"] = json!(people);
    }
    Ok(result)
}

fn bits(value: &Value) -> u64 {
    value
        .as_str()
        .and_then(|value| value.parse().ok())
        .or_else(|| value.as_u64())
        .unwrap_or(0)
}

/// Discord's documented permission resolution for one member in one channel.
fn permissions(guild: &Value, member: &Value, user_id: &str, channel: &Value) -> u64 {
    if guild["owner_id"] == user_id {
        return u64::MAX;
    }
    let guild_id = text(guild, "id");
    let member_roles = member["roles"]
        .as_array()
        .map(|roles| roles.iter().filter_map(Value::as_str).collect::<Vec<_>>())
        .unwrap_or_default();
    let mut allowed = 0;
    for role in guild["roles"].as_array().into_iter().flatten() {
        let id = text(role, "id");
        if id == guild_id || member_roles.contains(&id) {
            allowed |= bits(&role["permissions"]);
        }
    }
    if allowed & ADMINISTRATOR != 0 {
        return u64::MAX;
    }
    let overwrites = channel["permission_overwrites"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    if let Some(everyone) = overwrites.iter().find(|item| item["id"] == guild_id) {
        allowed = (allowed & !bits(&everyone["deny"])) | bits(&everyone["allow"]);
    }
    let (mut deny, mut allow) = (0, 0);
    for item in &overwrites {
        if item["type"].as_u64() == Some(0) && member_roles.contains(&text(item, "id")) {
            deny |= bits(&item["deny"]);
            allow |= bits(&item["allow"]);
        }
    }
    allowed = (allowed & !deny) | allow;
    if let Some(own) = overwrites
        .iter()
        .find(|item| item["type"].as_u64() == Some(1) && item["id"] == user_id)
    {
        allowed = (allowed & !bits(&own["deny"])) | bits(&own["allow"]);
    }
    allowed
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Json, Router, extract::Path, http::HeaderMap, routing::get as route};

    fn guild() -> Value {
        json!({
            "id": "100",
            "owner_id": "1",
            "roles": [
                { "id": "100", "permissions": (VIEW_CHANNEL | SEND_MESSAGES | READ_MESSAGE_HISTORY).to_string() },
                { "id": "7", "permissions": "0" }
            ]
        })
    }

    #[test]
    fn channel_permissions_follow_overwrite_precedence() {
        let member = json!({ "roles": ["7"] });
        let open = json!({ "permission_overwrites": [] });
        let hidden = json!({ "permission_overwrites": [
            { "id": "100", "type": 0, "allow": "0", "deny": VIEW_CHANNEL.to_string() }
        ] });
        let role_allowed = json!({ "permission_overwrites": [
            { "id": "100", "type": 0, "allow": "0", "deny": VIEW_CHANNEL.to_string() },
            { "id": "7", "type": 0, "allow": VIEW_CHANNEL.to_string(), "deny": "0" }
        ] });
        let member_denied = json!({ "permission_overwrites": [
            { "id": "7", "type": 0, "allow": VIEW_CHANNEL.to_string(), "deny": "0" },
            { "id": "400", "type": 1, "allow": "0", "deny": SEND_MESSAGES.to_string() }
        ] });
        assert_ne!(
            permissions(&guild(), &member, "400", &open) & VIEW_CHANNEL,
            0
        );
        assert_eq!(
            permissions(&guild(), &member, "400", &hidden) & VIEW_CHANNEL,
            0
        );
        assert_ne!(
            permissions(&guild(), &member, "400", &role_allowed) & VIEW_CHANNEL,
            0
        );
        assert_eq!(
            permissions(&guild(), &member, "400", &member_denied) & SEND_MESSAGES,
            0
        );
        assert_eq!(permissions(&guild(), &member, "1", &hidden), u64::MAX);
        assert_eq!(INVITE_PERMISSIONS, 68_608);
    }

    async fn users(headers: HeaderMap, Path(id): Path<String>) -> Json<Value> {
        assert_eq!(headers["authorization"], "Bot synthetic_discord_token");
        Json(match id.as_str() {
            "@me" => json!({ "id": "400", "username": "cairn", "bot": true }),
            _ => json!({ "id": id, "username": "sarah", "global_name": "Sarah" }),
        })
    }

    #[tokio::test]
    async fn discovery_lists_servers_and_marks_channels_the_bot_can_use() {
        let (_root, s, _) = super::super::tests::fixture().await;
        let app = Router::new()
            .route("/users/{id}", route(users))
            .route("/oauth2/applications/@me", route(|| async { Json(json!({ "id": "401" })) }))
            .route("/users/@me/guilds", route(|| async { Json(json!([{ "id": "100", "name": "Acme" }])) }))
            .route("/guilds/100", route(|| async { Json(guild()) }))
            .route("/guilds/100/members/400", route(|| async { Json(json!({ "roles": [] })) }))
            .route("/guilds/100/channels", route(|| async {
                Json(json!([
                    { "id": "201", "type": 0, "name": "private", "position": 2, "permission_overwrites": [
                        { "id": "100", "type": 0, "allow": "0", "deny": VIEW_CHANNEL.to_string() }
                    ] },
                    { "id": "200", "type": 0, "name": "support", "position": 1, "permission_overwrites": [] },
                    { "id": "202", "type": 2, "name": "voice", "position": 0 }
                ]))
            }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let result = discover_at(
            &s,
            "synthetic_discord_token",
            &json!({ "guildId": "100", "userIds": ["300", "not-an-id"] }),
            &origin,
        )
        .await
        .unwrap();
        assert_eq!(result["bot"]["name"], "cairn");
        assert_eq!(
            result["inviteUrl"],
            "https://discord.com/oauth2/authorize?client_id=401&scope=bot&permissions=68608"
        );
        assert_eq!(result["guilds"][0]["name"], "Acme");
        let channels = result["channels"].as_array().unwrap();
        assert_eq!(channels.len(), 2);
        assert_eq!(channels[0]["name"], "support");
        assert_eq!(channels[0]["canReply"], true);
        assert_eq!(channels[1]["canRead"], false);
        assert_eq!(
            result["users"],
            json!([{ "id": "300", "name": "Sarah", "username": "sarah" }])
        );
        let missing = discover_at(
            &s,
            "synthetic_discord_token",
            &json!({ "guildId": "999" }),
            &origin,
        )
        .await
        .unwrap_err();
        assert_eq!(missing.status, 400);
        server.abort();
    }
}
