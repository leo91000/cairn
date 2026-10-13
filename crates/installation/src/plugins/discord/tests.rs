use super::*;
use crate::{config::Config, service::Service};
use serde_json::{Value, json};
use std::sync::Arc;
use tempfile::TempDir;

pub(super) async fn fixture() -> (TempDir, Arc<Service>, Connection) {
    let root = TempDir::new().unwrap();
    std::fs::create_dir_all(root.path().join("home")).unwrap();
    let s = Service::new(Config {
        data_dir: root.path().join("data"),
        home: root.path().join("home"),
        workspace_roots: vec![root.path().to_owned()],
        public_url: "http://localhost:4310".into(),
        host: "127.0.0.1".into(),
        port: 0,
        codex_bin: "codex".into(),
        claude_bin: "claude".into(),
        gh_bin: "gh".into(),
        concurrency: 1,
        logger: false,
        worker_enabled: false,
        runner_url: String::new(),
    })
    .await
    .unwrap();
    let agent = s
        .agent(
            json!({
                "name": "Bot",
                "access": {
                    "projects": [],
                    "skills": [],
                    "mcps": [],
                    "github": false
                }
            }),
            None,
        )
        .await
        .unwrap();
    let connection = save(
        &s,
        &json!({
            "name": "Discord",
            "agentId": agent["id"],
            "projectId": null,
            "guildId": "100",
            "channelId": "200",
            "userIds": ["300"],
            "enabled": true,
            "token": "synthetic_discord_token",
            "maxMessagesPerHour": 20,

        }),
        None,
    )
    .await
    .unwrap();
    (root, s, serde_json::from_value(connection).unwrap())
}

pub(super) fn message() -> Value {
    json!({
        "id": "500",
        "guild_id": "100",
        "channel_id": "200",
        "author": {
            "id": "300",
            "bot": false
        },
        "mentions": [{
            "id": "400"
        }],
        "content": "<@400> hello"
    })
}
