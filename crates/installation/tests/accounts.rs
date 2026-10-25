mod common;

use cairn_installation::{
    accounts::KIND,
    config::{Config, now},
    provider::Provider,
    service::Service,
    validation::text,
};
use serde_json::{Value, json};
use tempfile::TempDir;

#[tokio::test]
async fn valid_available_usage_does_not_wait_for_due_account_refresh() {
    let root = TempDir::new().unwrap();
    let s = Service::new(config(&root)).await.unwrap();
    s.accounts.initialize(&s).await.unwrap();
    let mut account = s
        .accounts
        .create(&s, Provider::Codex, "Ready")
        .await
        .unwrap();
    account["state"] = "ready".into();
    account["usage"] = json!({
        "allowed": true, "checkedAt": now() - 65_000,
        "windows": [{ "id": "w", "usedPercent": 20, "models": [] }],
        "resets": null
    });
    s.store.put(KIND, account.clone()).await.unwrap();
    // The native binary is deliberately unavailable. Usage is still within the
    // existing 90-second validity window, so no synchronous poll is required.
    let lease = s
        .accounts
        .acquire(&s, "fresh-run", Provider::Codex, "gpt-6.1-sol")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(lease.account_id, text(&account, "id"));
    s.accounts.release(&lease).await.unwrap();
    account["usage"]["checkedAt"] = (now() - 100_000).into();
    s.store.put(KIND, account).await.unwrap();
    assert!(
        s.accounts
            .acquire(&s, "stale-run", Provider::Codex, "gpt-6.1-sol")
            .await
            .is_err()
    );
}

fn config(root: &TempDir) -> Config {
    Config {
        codex_bin: "/nonexistent-codex".into(),
        claude_bin: common::fixture("claude.mjs"),
        opencode_bin: "opencode".into(),
        opencode_go_usage_url: cairn_installation::accounts::opencode::USAGE_URL.into(),
        ..common::config(root.path())
    }
}

/// Writes the Codex pool, runs and Claude login of versions before the unified pool.
#[tokio::test]
async fn each_coding_agent_has_its_own_next_account_and_paused_accounts_are_skipped() {
    let root = TempDir::new().unwrap();
    let s = Service::new(config(&root)).await.unwrap();
    s.accounts.initialize(&s).await.unwrap();
    let mut ids = Vec::new();
    for (provider, name, used) in [
        (Provider::Codex, "Codex low", 90),
        (Provider::Codex, "Codex high", 10),
        (Provider::Claude, "Claude", 50),
    ] {
        let mut account = s.accounts.create(&s, provider, name).await.unwrap();
        account["state"] = "ready".into();
        account["usage"] = json!({
            "allowed": true,
            "checkedAt": now(),
            "windows": [{ "id": "w", "usedPercent": used, "models": [] }],
            "resets": null,
        });
        s.store.put(KIND, account.clone()).await.unwrap();
        ids.push(text(&account, "id").to_owned());
    }
    let status = |accounts: &[Value], id: &str| {
        accounts.iter().find(|a| a["id"] == id).unwrap()["status"].clone()
    };
    let list = s.accounts.list(&s).await.unwrap();
    assert_eq!(status(&list, &ids[0]), "ready");
    assert_eq!(status(&list, &ids[1]), "next");
    assert_eq!(status(&list, &ids[2]), "next");
    s.accounts
        .update(&s, &ids[1], &json!({ "enabled": false }))
        .await
        .unwrap();
    let list = s.accounts.list(&s).await.unwrap();
    assert_eq!(status(&list, &ids[0]), "next");
    assert_eq!(status(&list, &ids[1]), "paused");
    // OpenCode requires a managed API-key account even before one is connected.
    assert_eq!(
        s.accounts.overview(&s).await.unwrap()["required"],
        json!(["opencode"])
    );
    // Once every Claude account is paused, Claude runs wait for the user.
    s.accounts
        .update(&s, &ids[2], &json!({ "enabled": false }))
        .await
        .unwrap();
    assert_eq!(
        s.accounts.overview(&s).await.unwrap()["required"],
        json!(["claude", "opencode"])
    );
    assert!(
        s.accounts
            .update(&s, &ids[0], &json!({ "name": "" }))
            .await
            .is_err()
    );
    assert!(
        s.accounts
            .create(&s, Provider::Claude, &"x".repeat(101))
            .await
            .is_err()
    );
}
