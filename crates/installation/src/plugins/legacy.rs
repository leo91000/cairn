//! 1Password service accounts saved before plugin API v2 become instances of the
//! built-in 1Password package: same IDs (agents keep using `--account ID`), names,
//! state, agent grants and tokens. Runs at startup; a no-op once moved.
use super::connections::{self, AccessMode, Connection, secrets_key};
use crate::{
    config::{id, now},
    error::Result,
    service::Service,
    validation::text,
};
use serde_json::{Value, json};

const LEGACY: &str = "onepassword";

fn legacy_secret(id: &str) -> String {
    format!("onepassword:{id}")
}

pub async fn migrate(s: &Service) -> Result<()> {
    let accounts = s.store.list(LEGACY).await?;
    if accounts.is_empty() {
        return Ok(());
    }
    let mut moved = Vec::new();
    for account in accounts {
        let account_id = text(&account, "id").to_owned();
        let token = s
            .vault
            .get(&legacy_secret(&account_id))
            .await?
            .and_then(|credential| credential["token"].as_str().map(str::to_owned));
        let secrets = match &token {
            Some(token) => json!({ "token": token }),
            None => json!({}),
        };
        let encrypted = s
            .vault
            .encrypt(&secrets_key(&account_id), &json!({ "fields": secrets }))?;
        let connection = Connection {
            routing: None,
            id: account_id.clone(),
            plugin_id: "onepassword".into(),
            name: text(&account, "name").to_owned(),
            enabled: account["enabled"] != false,
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
            agent_ids: account["agentIds"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|agent| agent.as_str().map(str::to_owned))
                .collect(),
            secret_keys: if token.is_some() {
                vec!["token".into()]
            } else {
                Vec::new()
            },
            updated_at: now(),
        };
        moved.push((serde_json::to_value(&connection)?, encrypted));
    }
    s.store
        .transaction(move |db| {
            for (connection, encrypted) in &moved {
                let id = text(connection, "id");
                // Records are keyed by ID alone: the account makes way for its instance.
                db.remove(LEGACY, id)?;
                db.delete(&format!("mcp-secret:{}", legacy_secret(id)))?;
                db.put(connections::KIND, connection)?;
                db.set(&format!("mcp-secret:{}", secrets_key(id)), encrypted, None)?;
                db.audit("plugin.onepassword.migrated", &json!({ "id": id }))?;
            }
            Ok(())
        })
        .await?;
    s.plugins.changed.notify_one();
    Ok(())
}
