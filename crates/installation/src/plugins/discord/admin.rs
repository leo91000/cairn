use super::{AccessMode, Connection, KIND, Label, credential_key, label, snowflake};
use crate::{
    config::{id, now},
    error::{Error, Result},
    service::{Service, policy},
    validation::{text, uuid},
};
use serde_json::{Value, json};

pub async fn save(s: &Service, input: &Value, existing: Option<&str>) -> Result<Value> {
    let owner = crate::relay::task_author_policy(s).await?;
    let name = text(input, "name").trim();
    let guild = text(input, "guildId");
    let channel = text(input, "channelId");
    let agent_id = text(input, "agentId");
    let project = input["projectId"].as_str().map(str::to_owned);
    uuid(agent_id)?;
    if let Some(project) = &project {
        uuid(project)?;
    }
    let users = input["userIds"]
        .as_array()
        .ok_or_else(|| Error::bad("Choose the Discord users allowed to use this bot."))?;
    let access_mode = match input["accessMode"].as_str() {
        None | Some("allowlist") => AccessMode::Allowlist,
        Some("requests") => AccessMode::Requests,
        Some(_) => return Err(Error::bad("Choose who can use this bot.")),
    };
    if name.is_empty()
        || name.chars().count() > 100
        || !snowflake(guild)
        || !snowflake(channel)
        || users.len() > 100
        || users
            .iter()
            .any(|user| !user.as_str().is_some_and(snowflake))
    {
        return Err(Error::bad(
            "Enter a name, Discord server and channel IDs, and valid Discord user IDs.",
        ));
    }
    // Approval mode may start empty: the owner approves the first mention themselves.
    if users.is_empty() && access_mode == AccessMode::Allowlist {
        return Err(Error::bad("Add at least one person who can use this bot."));
    }
    let user_ids = users
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let labels = labels(&input["people"]);
    let people = user_ids
        .iter()
        .map(|id| {
            labels
                .iter()
                .find(|label| label.id == *id)
                .cloned()
                .unwrap_or_else(|| Label {
                    id: id.clone(),
                    name: String::new(),
                })
        })
        .collect();
    let bot = labels_of(&input["bot"]).into_iter().next();
    let enabled = input["enabled"]
        .as_bool()
        .ok_or_else(|| Error::bad("Choose whether the connection is enabled."))?;
    let limit = input["maxMessagesPerHour"]
        .as_u64()
        .filter(|value| *value > 0 && *value <= 100)
        .ok_or_else(|| Error::bad("Choose a message limit between 1 and 100 per hour."))?
        as u32;
    let token = text(input, "token").trim();
    if input.get("token").is_some_and(|value| !value.is_string())
        || token.len() > 4096
        || token.chars().any(|c| c.is_whitespace() || c.is_control())
        || (existing.is_none() && token.is_empty())
    {
        return Err(Error::bad("Enter a Discord bot token."));
    }
    let connection_id = existing.map_or_else(id, str::to_owned);
    uuid(&connection_id)?;
    let encrypted = if token.is_empty() {
        None
    } else {
        Some(s.vault.encrypt(
            &credential_key(&connection_id),
            &json!({
                "token": token
            }),
        )?)
    };
    let agent_id = agent_id.to_owned();
    let mut connection = Connection {
        id: connection_id,
        plugin_id: "discord".into(),
        name: name.to_owned(),
        enabled,
        revision: id(),
        guild_id: guild.into(),
        channel_id: channel.into(),
        user_ids,
        agent_id,
        project_id: project,
        max_messages_per_hour: limit,
        granted_access: Value::Null,
        owner_access_id: owner.map(|policy| policy.owner.access_id),
        updated_at: now(),
        access_mode,
        people,
        bot,
        guild_name: label(text(input, "guildName")),
        channel_name: label(text(input, "channelName")),
    };
    let updating = existing.is_some();
    let result = s.store.transaction(move |db| {
        if updating && db.get(KIND, &connection.id)?.is_none() { return Err(Error::not_found("Plugin connection not found.")); }
        if db.list(KIND)?.len() >= 8 && !updating { return Err(Error::bad("At most eight bot connections can run on this installation.")); }
        let agent = db.get("agents", &connection.agent_id)?.ok_or_else(|| Error::bad("Agent not found."))?;
        let access = policy(&agent);
        let projects = connection.project_id.as_ref().map_or_else(|| json!([]), |id| json!([id]));
        // External users never receive an installation-wide agent or project scope.
        if access["projects"] != projects || access["installation"] == true {
            return Err(Error::bad("Choose a dedicated agent restricted to the selected project (or no projects), with installation access disabled."));
        }
        if let Some(project) = &connection.project_id
            && db.get("projects", project)?.is_none()
        { return Err(Error::bad("Project not found.")); }
        connection.granted_access = access;
        if connection.access_mode == AccessMode::Allowlist {
            for request in db.list("plugin-requests")? {
                if request["connectionId"] == connection.id && request["status"] == "pending" {
                    db.remove("plugin-requests", text(&request, "id"))?;
                }
            }
        }
        if let Some(encrypted) = encrypted { db.set(&format!("mcp-secret:{}", credential_key(&connection.id)), &encrypted, None)?; }
        let value = serde_json::to_value(&connection)?;
        db.put(KIND, &value)?;
        db.audit("plugin.connection.saved", &json!({
            "id": connection.id,
            "plugin": "discord"
        }))?;
        Ok(value)
    }).await?;
    s.plugins.changed.notify_one();
    Ok(result)
}

pub async fn remove(s: &Service, connection_id: &str) -> Result<Value> {
    uuid(connection_id)?;
    let connection_id = connection_id.to_owned();
    s.store
        .transaction(move |db| {
            db.remove(KIND, &connection_id)?;
            db.delete(&format!("mcp-secret:{}", credential_key(&connection_id)))?;
            db.delete(&format!("plugin-status:{connection_id}"))?;
            for kind in [
                "plugin-inbox",
                "plugin-deliveries",
                "plugin-requests",
                "plugin-activity",
            ] {
                for value in db.list(kind)? {
                    if value["connectionId"] == connection_id {
                        db.remove(kind, text(&value, "id"))?;
                    }
                }
            }
            db.audit(
                "plugin.connection.deleted",
                &json!({
                    "id": connection_id
                }),
            )?;
            Ok(())
        })
        .await?;
    s.plugins.changed.notify_one();
    Ok(json!({
        "deleted": true
    }))
}

pub async fn test(s: &Service, id: &str) -> Result<Value> {
    uuid(id)?;
    let connection: Connection = serde_json::from_value(s.get(KIND, id).await?)?;
    // This owner-only operation can verify a token before enabling the bot.
    let credential = s
        .vault
        .get(&credential_key(&connection.id))
        .await?
        .ok_or_else(|| Error::bad("Discord token is missing."))?;
    let token = text(&credential, "token");
    let response = s
        .http
        .get("https://discord.com/api/v10/users/@me")
        .header("user-agent", super::USER_AGENT)
        .header("authorization", format!("Bot {token}"))
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
    Ok(json!({
        "ok": true,
        "botId": user["id"],
        "name": user["username"]
    }))
}

fn labels_of(value: &Value) -> Vec<Label> {
    let items = match value {
        Value::Array(items) => items.as_slice(),
        Value::Object(_) => std::slice::from_ref(value),
        _ => &[],
    };
    items
        .iter()
        .filter(|item| item["id"].as_str().is_some_and(snowflake))
        .map(|item| Label {
            id: text(item, "id").to_owned(),
            name: label(text(item, "name")),
        })
        .collect()
}

fn labels(value: &Value) -> Vec<Label> {
    labels_of(value).into_iter().take(100).collect()
}

/// Owner view: configuration, live state, usage and the latest exchanges. Never the token.
pub async fn overview(s: &Service) -> Result<Vec<Value>> {
    let hour = now() / 3_600_000;
    s.store
        .read(move |db| {
            let requests = db.list("plugin-requests")?;
            let activity = db.list("plugin-activity")?;
            let mut connections = db.list(KIND)?;
            for connection in &mut connections {
                let id = text(connection, "id").to_owned();
                connection["status"] = db
                    .kv(&format!("plugin-status:{id}"))?
                    .unwrap_or(json!({ "state": "disconnected" }));
                connection["usage"] = json!({
                    "hour": db
                        .kv(&format!("plugin-discord-rate:{id}:{hour}"))?
                        .and_then(|value| value.as_u64())
                        .unwrap_or(0)
                });
                let mut pending = requests
                    .iter()
                    .filter(|value| value["connectionId"] == id && value["status"] == "pending")
                    .cloned()
                    .collect::<Vec<_>>();
                pending.sort_by_key(|value| -value["updatedAt"].as_i64().unwrap_or(0));
                connection["requests"] = json!(pending);
                let mut recent = activity
                    .iter()
                    .filter(|value| value["connectionId"] == id)
                    .cloned()
                    .collect::<Vec<_>>();
                recent.sort_by_key(|value| -value["createdAt"].as_i64().unwrap_or(0));
                recent.truncate(8);
                connection["activity"] = json!(recent);
            }
            Ok(connections)
        })
        .await
}

/// Pausing keeps the configuration and token; the gateway stops within seconds.
pub async fn set_enabled(s: &Service, connection_id: &str, input: &Value) -> Result<Value> {
    uuid(connection_id)?;
    let enabled = input["enabled"]
        .as_bool()
        .ok_or_else(|| Error::bad("Choose whether the bot accepts messages."))?;
    let connection_id = connection_id.to_owned();
    let value = s
        .store
        .transaction(move |db| {
            let mut value = db
                .get(KIND, &connection_id)?
                .ok_or_else(|| Error::not_found("Plugin connection not found."))?;
            value["enabled"] = json!(enabled);
            value["updatedAt"] = json!(now());
            db.put(KIND, &value)?;
            db.audit(
                "plugin.connection.enabled",
                &json!({ "id": connection_id, "enabled": enabled }),
            )?;
            Ok(value)
        })
        .await?;
    s.plugins.changed.notify_one();
    Ok(value)
}

/// Approve, deny or remove one Discord user. Admission rereads this list for every
/// message, so changes apply immediately without restarting the gateway session.
pub async fn people(
    s: &Service,
    connection_id: &str,
    user_id: &str,
    action: &str,
) -> Result<Value> {
    uuid(connection_id)?;
    if !snowflake(user_id) || !matches!(action, "approve" | "deny" | "remove") {
        return Err(Error::bad("Invalid Discord user."));
    }
    let (connection_id, user_id, action) = (
        connection_id.to_owned(),
        user_id.to_owned(),
        action.to_owned(),
    );
    s.store
        .transaction(move |db| {
            let mut connection: Connection = serde_json::from_value(
                db.get(KIND, &connection_id)?
                    .ok_or_else(|| Error::not_found("Plugin connection not found."))?,
            )?;
            let request = db
                .list("plugin-requests")?
                .into_iter()
                .find(|value| value["connectionId"] == connection_id && value["userId"] == user_id);
            match action.as_str() {
                "approve" => {
                    let request =
                        request.ok_or_else(|| Error::not_found("Access request not found."))?;
                    if !connection.user_ids.contains(&user_id) {
                        if connection.user_ids.len() >= 100 {
                            return Err(Error::bad("At most 100 people can use one bot."));
                        }
                        connection.user_ids.push(user_id.clone());
                        connection.people.push(Label {
                            id: user_id.clone(),
                            name: label(text(&request, "name")),
                        });
                    }
                    db.remove("plugin-requests", text(&request, "id"))?;
                }
                "deny" => {
                    // A denied request stays recorded so repeated mentions stay silent.
                    let mut request =
                        request.ok_or_else(|| Error::not_found("Access request not found."))?;
                    request["status"] = json!("denied");
                    request["updatedAt"] = json!(now());
                    db.put("plugin-requests", &request)?;
                }
                _ => {
                    connection.user_ids.retain(|id| *id != user_id);
                    connection.people.retain(|person| person.id != user_id);
                    if let Some(request) = request {
                        db.remove("plugin-requests", text(&request, "id"))?;
                    }
                }
            }
            connection.updated_at = now();
            let value = serde_json::to_value(&connection)?;
            db.put(KIND, &value)?;
            db.audit(
                "plugin.connection.people",
                &json!({ "id": connection_id, "action": action }),
            )?;
            Ok(value)
        })
        .await
}
