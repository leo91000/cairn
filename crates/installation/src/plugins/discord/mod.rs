//! Discord is an owner-delegated ingress, never an installation-wide reader.
mod admin;
mod delivery;
mod discover;
mod gateway;

use crate::{
    error::{Error, Result},
    service::{Service, policy},
    store::Db,
    validation::text,
};
pub use admin::{overview, people, remove, save, set_enabled, test};
pub use delivery::{accept, process_pending, reply, submit};
pub use discover::discover;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{collections::HashMap, sync::Arc, time::Duration};
use tokio_util::sync::CancellationToken;

pub const KIND: &str = "plugin-connections";

pub const USER_AGENT: &str = concat!(
    "DiscordBot (https://github.com/leo91000/cairn, ",
    env!("CARGO_PKG_VERSION"),
    ")"
);

#[derive(Clone, Copy, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum AccessMode {
    /// Only listed users; other mentions are discarded without a trace.
    #[default]
    Allowlist,
    /// Unknown users who mention the bot create a request the owner can approve.
    Requests,
}

/// Display labels chosen by the owner. Never used for authorization.
#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Label {
    pub id: String,
    pub name: String,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Connection {
    pub id: String,
    pub plugin_id: String,
    pub name: String,
    pub enabled: bool,
    pub revision: String,
    pub guild_id: String,
    pub channel_id: String,
    pub user_ids: Vec<String>,
    pub agent_id: String,
    pub project_id: Option<String>,
    pub max_messages_per_hour: u32,
    pub granted_access: Value,
    pub owner_access_id: Option<String>,
    pub updated_at: i64,
    #[serde(default)]
    pub access_mode: AccessMode,
    #[serde(default)]
    pub people: Vec<Label>,
    #[serde(default)]
    pub bot: Option<Label>,
    #[serde(default)]
    pub guild_name: String,
    #[serde(default)]
    pub channel_name: String,
}

pub fn snowflake(value: &str) -> bool {
    !value.is_empty() && value.len() <= 20 && value.bytes().all(|c| c.is_ascii_digit())
}

pub(crate) fn label(value: &str) -> String {
    value
        .chars()
        .filter(|c| !c.is_control())
        .take(100)
        .collect::<String>()
        .trim()
        .to_owned()
}

fn credential_key(id: &str) -> String {
    format!("plugin-discord:{id}")
}

pub(crate) async fn ensure_owner(s: &Service, connection: &Connection) -> Result<()> {
    let current = crate::relay::task_author_policy(s).await?;
    let access_id = current
        .as_ref()
        .map(|policy| policy.owner.access_id.as_str());
    if access_id != connection.owner_access_id.as_deref() {
        return Err(Error::forbidden(
            "Discord delegation no longer belongs to this installation owner.",
        ));
    }
    Ok(())
}

pub(crate) fn authorize(db: &Db<'_>, expected: &Connection) -> Result<Connection> {
    let denied = || {
        Error::forbidden(
            "Discord connection or agent grant was revoked. Review the connection settings.",
        )
    };
    let value = db.get(KIND, &expected.id)?.ok_or_else(denied)?;
    let current: Connection = serde_json::from_value(value)?;
    if !current.enabled || current.revision != expected.revision {
        return Err(denied());
    }
    let agent = db.get("agents", &current.agent_id)?.ok_or_else(denied)?;
    if policy(&agent) != current.granted_access {
        return Err(denied());
    }
    Ok(current)
}

pub(crate) async fn credential(s: &Service, connection: &Connection) -> Result<String> {
    ensure_owner(s, connection).await?;
    let expected = connection.clone();
    s.store.read(move |db| authorize(db, &expected)).await?;
    let value = s
        .vault
        .get(&credential_key(&connection.id))
        .await?
        .ok_or_else(|| Error::forbidden("Discord token is missing."))?;
    Ok(text(&value, "token").to_owned())
}

pub async fn maintain(s: Arc<Service>) {
    let mut active: HashMap<String, (String, CancellationToken, tokio::task::JoinHandle<()>)> =
        HashMap::new();
    let mut timer = tokio::time::interval(Duration::from_secs(2));
    timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            () = s.shutdown.cancelled() => break,
            () = s.plugins.changed.notified() => {},
            _ = timer.tick() => {},
        }
        let connections = match s.store.list(KIND).await {
            Ok(values) => values
                .into_iter()
                .filter_map(|value| serde_json::from_value::<Connection>(value).ok())
                .filter(|value| value.enabled && value.plugin_id == "discord")
                .collect::<Vec<_>>(),
            Err(_) => continue,
        };
        let stale = active
            .iter()
            .filter(|(id, (revision, _, _))| {
                !connections
                    .iter()
                    .any(|value| value.id == **id && value.revision == *revision)
            })
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();
        for id in stale {
            if let Some((_, stop, task)) = active.remove(&id) {
                stop.cancel();
                task.abort();
                let _ = task.await;
            }
        }
        for connection in connections {
            if active.contains_key(&connection.id) {
                continue;
            }
            let stop = s.shutdown.child_token();
            let task = tokio::spawn(run(s.clone(), connection.clone(), stop.clone()));
            active.insert(connection.id, (connection.revision, stop, task));
        }
    }
    for (_, (_, stop, task)) in active {
        stop.cancel();
        task.abort();
        let _ = task.await;
    }
}

async fn status(s: &Service, connection: &Connection, state: &str) {
    let key = format!("plugin-status:{}", connection.id);
    let _ = s
        .store
        .set(
            &key,
            serde_json::json!({
                "state": state,
                "updatedAt": crate::config::now()
            }),
            None,
        )
        .await;
}

async fn run(s: Arc<Service>, connection: Connection, stop: CancellationToken) {
    let mut retry = Duration::from_secs(5);
    let mut session = gateway::Session::default();
    loop {
        if stop.is_cancelled() {
            break;
        }
        status(&s, &connection, "connecting").await;
        let operation = async {
            let token = credential(&s, &connection).await?;
            let delivery =
                delivery::maintain(s.clone(), connection.clone(), token.clone(), stop.clone());
            let socket = gateway::run(&s, &connection, &token, &stop, &mut session);
            tokio::try_join!(socket, delivery).map(|_| ())
        };
        tokio::select! {
            () = stop.cancelled() => break,
            result = operation => {
                if let Err(error) = result {
                    let state = if matches!(error.status, 401 | 403) { "needs_review" } else { "disconnected" };
                    status(&s, &connection, state).await;
                    if matches!(error.status, 401 | 403) { break; }
                }
            },
        }
        tokio::select! {
            () = stop.cancelled() => break,
            () = tokio::time::sleep(retry) => {},
        }
        retry = (retry * 2).min(Duration::from_secs(60));
    }
}

#[cfg(test)]
mod tests;
