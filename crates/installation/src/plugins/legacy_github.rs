//! GitHub before it became the bundled GitHub package (data compatibility epoch 8).
//! Runs at startup, before agents are normalized, and is a no-op once moved:
//!
//! - the shared connection (the `gh` login in the installation's home) becomes the
//!   "GitHub" instance, granted to every agent that had `access.github`;
//! - each agent's dedicated token (`agent-github:{id}`, stored unencrypted) becomes its
//!   own instance, encrypted in the vault, granted to that agent;
//! - `access.github` leaves agents and the access plugin delegations approved;
//! - GitHub skill sources, projects and waits name the package.
//!
//! Remove once every installation runs epoch 8 or later.
use super::connections::{self, AccessMode, Connection, credential_key};
use crate::{
    config::{MAIN_AGENT_ID, id, now},
    error::Result,
    service::Service,
    validation::text,
};
use serde_json::{Value, json};

const PLUGIN: &str = "github";
const DEDICATED: &str = "agent-github:";

/// The token of the shared connection: `gh` keeps it in `hosts.yml` without a keyring,
/// at the host level (older releases) or under the active user.
fn shared_token(hosts: &str) -> Option<String> {
    let hosts: Value = serde_yaml_ng::from_str(hosts).ok()?;
    let host = &hosts["github.com"];
    let user = text(host, "user");
    [&host["oauth_token"], &host["users"][user]["oauth_token"]]
        .into_iter()
        .filter_map(Value::as_str)
        .map(str::trim)
        .find(|token| !token.is_empty() && !token.chars().any(char::is_whitespace))
        .map(str::to_owned)
}

fn instance(name: &str, agent_ids: Vec<String>) -> Connection {
    Connection {
        id: id(),
        plugin_id: PLUGIN.into(),
        name: name.chars().take(100).collect(),
        enabled: true,
        revision: id(),
        settings: json!({}),
        agent_id: String::new(),
        project_id: None,
        granted_access: Value::Null,
        owner_access_id: None,
        access_mode: AccessMode::Allowlist,
        user_ids: Vec::new(),
        people: Vec::new(),
        max_messages_per_hour: 20,
        max_per_person_per_hour: 5,
        agent_ids,
        secret_keys: Vec::new(),
        updated_at: now(),
    }
}

/// Whether any record still has the earlier GitHub shape.
fn pending(
    agents: &[Value],
    dedicated: &[(String, Value)],
    sources: &[(String, Value)],
    chats: &[Value],
) -> bool {
    agents
        .iter()
        .any(|agent| agent["access"].get("github").is_some())
        || !dedicated.is_empty()
        || sources.iter().any(|(_, record)| record["type"] == "github")
        || chats.iter().any(legacy_wait)
}

fn legacy_wait(chat: &Value) -> bool {
    let condition = &chat["wakeup"]["condition"];
    matches!(
        condition["type"].as_str(),
        Some("github_workflow" | "github_pr_checks")
    ) && condition.get("pluginId").is_none()
}

/// `owner/repo` of a github.com remote, in any spelling Cairn stored.
fn repository(origin: &str) -> Option<String> {
    let https = crate::project_git::https_remote(origin)?;
    let path = https.strip_prefix("https://github.com/")?;
    let path = path.trim_end_matches('/').trim_end_matches(".git");
    let mut parts = path.split('/');
    let (Some(owner), Some(name), None) = (parts.next(), parts.next(), parts.next()) else {
        return None;
    };
    (!owner.is_empty() && !name.is_empty()).then(|| format!("{owner}/{name}"))
}

pub async fn migrate(s: &Service) -> Result<()> {
    let agents = s.store.list("agents").await?;
    let dedicated = s.store.keys(DEDICATED).await?;
    let sources = s.store.keys("skill-source:").await?;
    let chats = s.store.list("chats").await?;
    if !pending(&agents, &dedicated, &sources, &chats) {
        return Ok(());
    }
    let hosts = tokio::fs::read_to_string(s.config.home.join(".config/gh/hosts.yml"))
        .await
        .ok();
    let shared_token = hosts.as_deref().and_then(shared_token);

    let shared_access =
        |agent: &Value| agent["id"] == MAIN_AGENT_ID || agent["access"]["github"] != false;
    let shared = shared_token.as_ref().map(|token| {
        let agent_ids = agents
            .iter()
            .filter(|agent| shared_access(agent))
            .map(|agent| text(agent, "id").to_owned())
            .collect();
        (instance("GitHub", agent_ids), token.clone())
    });
    // An agent answering anyone in a channel must hold no plugin grant: its token is
    // kept without one rather than suspending the delegation.
    let public_agents = s
        .store
        .list(connections::KIND)
        .await?
        .into_iter()
        .filter(|connection| connection["accessMode"] == "everyone")
        .map(|connection| text(&connection, "agentId").to_owned())
        .collect::<Vec<_>>();
    let mut created = shared.iter().cloned().collect::<Vec<_>>();
    for (key, token) in &dedicated {
        let agent_id = key.trim_start_matches(DEDICATED);
        let Some(token) = token
            .as_str()
            .map(str::trim)
            .filter(|token| !token.is_empty())
        else {
            continue;
        };
        let Some(agent) = agents.iter().find(|agent| agent["id"] == agent_id) else {
            continue;
        };
        // An agent using the shared connection never used its token: keep it, ungranted.
        let unused = (shared.is_some() && shared_access(agent))
            || public_agents.iter().any(|public| public == agent_id);
        let name = format!("GitHub ({})", text(agent, "name"));
        let grants = if unused {
            Vec::new()
        } else {
            vec![agent_id.to_owned()]
        };
        created.push((instance(&name, grants), token.to_owned()));
    }
    let mut encrypted = Vec::new();
    for (connection, token) in &created {
        let secret = s
            .vault
            .encrypt(&credential_key(&connection.id), &json!({ "token": token }))?;
        encrypted.push((serde_json::to_value(connection)?, secret));
    }
    let shared_id = shared.as_ref().map(|(connection, _)| connection.id.clone());
    let instance_ids = created
        .iter()
        .map(|(connection, _)| connection.id.clone())
        .collect::<Vec<_>>();

    s.store
        .transaction(move |db| {
            for (connection, secret) in &encrypted {
                db.put(connections::KIND, connection)?;
                let key = credential_key(text(connection, "id"));
                db.set(&format!("mcp-secret:{key}"), secret, None)?;
            }
            for mut agent in db.list("agents")? {
                let granted = encrypted
                    .iter()
                    .filter(|(connection, _)| {
                        connection["agentIds"]
                            .as_array()
                            .is_some_and(|ids| ids.iter().any(|id| *id == agent["id"]))
                    })
                    .map(|(connection, _)| connection["id"].clone())
                    .collect::<Vec<_>>();
                let had_setting = agent["access"].get("github").is_some();
                if !had_setting && granted.is_empty() {
                    continue;
                }
                // Materialize the policy first, so a GitHub-only restriction keeps its
                // effect on MCP connections (see `service::policy`).
                agent["access"] = crate::service::policy(&agent);
                let mut plugins = crate::agent_grants::plugins(&agent);
                for id in granted {
                    if let Some(id) = id.as_str()
                        && !plugins.iter().any(|known| known == id)
                    {
                        plugins.push(id.to_owned());
                    }
                }
                agent[crate::agent_grants::PLUGINS] = json!(plugins);
                crate::service::touch(&mut agent);
                db.put("agents", &agent)?;
            }
            // Delegations approved an access that included the setting.
            for mut connection in db.list(connections::KIND)? {
                if let Some(access) = connection["grantedAccess"].as_object_mut()
                    && access.remove("github").is_some()
                {
                    db.put(connections::KIND, &connection)?;
                }
            }
            for (key, _) in db.keys(DEDICATED)? {
                db.delete(&key)?;
            }
            for (key, mut record) in db.keys("skill-source:")? {
                if record["type"] != "github" {
                    continue;
                }
                let repository = text(&record, "repository").to_owned();
                let path = text(&record, "path").to_owned();
                let reference = text(&record, "ref").to_owned();
                let label = if path.is_empty() {
                    repository.clone()
                } else {
                    format!("{repository}/{path}")
                };
                record = json!({
                    "pluginId": PLUGIN,
                    "source": "repository",
                    "instance": shared_id,
                    "location": { "repository": repository, "ref": reference },
                    "folder": path,
                    "version": record["commit"],
                    "label": label,
                    "url": format!("https://github.com/{repository}/tree/{reference}/{path}")
                        .trim_end_matches('/'),
                    "importedAt": record["importedAt"],
                    "revision": record["revision"],
                });
                db.set(&key, &record, None)?;
            }
            for mut project in db.list("projects")? {
                if project["source"].is_object() {
                    continue;
                }
                let Some(repository) = repository(text(&project, "origin")) else {
                    continue;
                };
                project["source"] = json!({
                    "pluginId": PLUGIN,
                    "source": "repositories",
                    "instance": shared_id,
                    "id": repository,
                });
                db.put("projects", &project)?;
            }
            for mut chat in db.list("chats")? {
                if legacy_wait(&chat) {
                    chat["wakeup"]["condition"]["pluginId"] = PLUGIN.into();
                    db.put("chats", &chat)?;
                }
            }
            db.audit(
                "plugin.github.migrated",
                &json!({ "instances": instance_ids }),
            )
        })
        .await?;
    s.plugins.changed.notify_one();
    Ok(())
}

/// Fills the migrated instances' account details (login, commit identity) through the
/// package's own form, once the installation runs. Failures leave them for the owner.
pub async fn complete(s: std::sync::Arc<Service>) {
    let Ok(values) = s.store.list(connections::KIND).await else {
        return;
    };
    for value in values {
        let Ok(connection) = serde_json::from_value::<Connection>(value) else {
            continue;
        };
        if connection.plugin_id != PLUGIN || connection.settings.get("login").is_some() {
            continue;
        }
        if let Err(error) = connections::refresh(&s, &connection).await {
            tracing::info!(
                message = error.message,
                "GitHub instance details not refreshed"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_both_hosts_formats() {
        let flat =
            "github.com:\n    oauth_token: gho_flat\n    git_protocol: https\n    user: octo\n";
        assert_eq!(shared_token(flat).as_deref(), Some("gho_flat"));
        let users = "github.com:\n    users:\n        octo:\n            oauth_token: gho_user\n    git_protocol: https\n    user: octo\n";
        assert_eq!(shared_token(users).as_deref(), Some("gho_user"));
        assert_eq!(shared_token("github.com:\n    user: octo\n"), None);
        assert_eq!(shared_token("not: [yaml"), None);
    }

    #[test]
    fn recognizes_github_remotes() {
        for origin in [
            "https://github.com/Team/Repo.git",
            "github.com:Team/Repo.git",
            "ssh://github.com/Team/Repo.git",
        ] {
            assert_eq!(repository(origin).as_deref(), Some("Team/Repo"), "{origin}");
        }
        assert_eq!(repository("https://gitlab.com/team/repo.git"), None);
        assert_eq!(repository("https://github.com/team"), None);
    }
}
