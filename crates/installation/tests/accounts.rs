use common::account_fixture::AccountFixtures;
mod common;

use cairn_installation::{
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
    let mut account = serde_json::to_value(
        s.accounts
            .create(&s, Provider::Codex, "Ready")
            .await
            .unwrap(),
    )
    .unwrap();
    account["state"] = "ready".into();
    account["usage"] = json!({
        "allowed": true, "checkedAt": now() - 65_000,
        "windows": [{ "id": "w", "usedPercent": 20, "models": [] }],
        "resets": null
    });
    s.store.save_account_fixture(account.clone()).await.unwrap();
    // The native binary is deliberately unavailable. Usage is still within the
    // existing 90-second validity window, so no synchronous poll is required.
    let lease = s
        .accounts
        .acquire(&s, "fresh-run", Provider::Codex, "gpt-6.1-sol")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(lease.account_id, text(&account, "id"));
    s.accounts.release(&s, &lease).await.unwrap();
    account["usage"]["checkedAt"] = (now() - 100_000).into();
    s.store.save_account_fixture(account).await.unwrap();
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
        let mut account =
            serde_json::to_value(s.accounts.create(&s, provider, name).await.unwrap()).unwrap();
        account["state"] = "ready".into();
        account["usage"] = json!({
            "allowed": true,
            "checkedAt": now(),
            "windows": [{ "id": "w", "usedPercent": used, "models": [] }],
            "resets": null,
        });
        s.store.save_account_fixture(account.clone()).await.unwrap();
        ids.push(text(&account, "id").to_owned());
    }
    let status = |accounts: &[Value], id: &str| {
        accounts.iter().find(|a| a["id"] == id).unwrap()["status"].clone()
    };
    let list = common::account_fixture::account_values(s.accounts.list(&s).await.unwrap());
    assert_eq!(status(&list, &ids[0]), "ready");
    assert_eq!(status(&list, &ids[1]), "next");
    assert_eq!(status(&list, &ids[2]), "next");
    common::account_fixture::update_account_fixture(&s, &ids[1], &json!({ "enabled": false }))
        .await
        .unwrap();
    let list = common::account_fixture::account_values(s.accounts.list(&s).await.unwrap());
    assert_eq!(status(&list, &ids[0]), "next");
    assert_eq!(status(&list, &ids[1]), "paused");
    // OpenCode requires a managed API-key account even before one is connected.
    assert_eq!(
        serde_json::to_value(s.accounts.overview(&s).await.unwrap()).unwrap()["required"],
        json!(["opencode"])
    );
    // Once every Claude account is paused, Claude runs wait for the user.
    common::account_fixture::update_account_fixture(&s, &ids[2], &json!({ "enabled": false }))
        .await
        .unwrap();
    assert_eq!(
        serde_json::to_value(s.accounts.overview(&s).await.unwrap()).unwrap()["required"],
        json!(["claude", "opencode"])
    );
    assert!(
        common::account_fixture::update_account_fixture(&s, &ids[0], &json!({ "name": "" }))
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

#[tokio::test]
async fn sql_leases_fence_concurrent_admission_and_pause_keeps_the_running_lease() {
    use cairn_contracts::AccountSettings;
    use cairn_installation::accounts::{Account, AccountState};
    let root=TempDir::new().unwrap();
    let s=Service::new(config(&root)).await.unwrap();
    s.accounts.initialize(&s).await.unwrap();
    let mut account=Account::new(Provider::Codex,"One slot",AccountState::Ready);
    account.max_concurrent_runs=Some(1);
    account.usage=Some(serde_json::from_value(json!({"checkedAt":now(),"windows":[{"id":"w","usedPercent":10}]})).unwrap());
    s.store.save_coding_account(&account).await.unwrap();
    let (a,b)=tokio::join!(s.accounts.acquire(&s,"concurrent-a",Provider::Codex,"model"),s.accounts.acquire(&s,"concurrent-b",Provider::Codex,"model"));
    assert_eq!([a.is_ok(),b.is_ok()].into_iter().filter(|success|*success).count(),1);
    let lease=a.or(b).unwrap().unwrap();
    s.accounts.update(&s,&account.id,&AccountSettings {name:account.name.clone(),enabled:false,max_concurrent_runs:1}).await.unwrap();
    assert_eq!(s.accounts.active(&s,&account.id).await.unwrap().len(),1);
    assert!(s.accounts.acquire(&s,"paused",Provider::Codex,"model").await.is_err());
    assert!(s.accounts.remove(&s,&account.id).await.is_err());
    s.accounts.release(&s,&lease).await.unwrap();
    assert!(s.accounts.active(&s,&account.id).await.unwrap().is_empty());
    s.accounts.remove(&s,&account.id).await.unwrap();
    assert!(s.store.coding_account(&account.id).await.unwrap().is_none());
}

#[tokio::test]
async fn restart_invalidates_account_leases_and_retains_typed_usage_and_exhaustion() {
    use cairn_installation::accounts::{Account,AccountState};
    let root=TempDir::new().unwrap();
    let s=Service::new(config(&root)).await.unwrap();
    s.accounts.initialize(&s).await.unwrap();
    let mut account=Account::new(Provider::Codex,"Restart",AccountState::Ready);
    account.usage=Some(serde_json::from_value(json!({"checkedAt":now(),"windows":[{"id":"w","usedPercent":12,"models":["model"]}]})).unwrap());
    s.store.save_coding_account(&account).await.unwrap();
    let lease=s.accounts.acquire(&s,"interrupted",Provider::Codex,"model").await.unwrap().unwrap();
    s.accounts.exhausted(&s,&account.id,"model").await.unwrap();
    let saved=s.accounts.account(&s,&account.id).await.unwrap();
    drop(s);
    let restarted=Service::new(config(&root)).await.unwrap();
    restarted.accounts.initialize(&restarted).await.unwrap();
    assert_eq!(restarted.accounts.account(&restarted,&account.id).await.unwrap(),saved);
    assert!(restarted.accounts.active(&restarted,&account.id).await.unwrap().is_empty());
    assert!(restarted.accounts.access(&restarted,&lease,&json!({})).await.is_err());
}

#[tokio::test]
async fn account_settings_replace_rejects_stale_revision_and_never_returns_private_identity() {
    use cairn_installation::{api,auth::{InstallationIdentity,InstallationRole},http::Input};
    let root=TempDir::new().unwrap();
    let s=Service::new(config(&root)).await.unwrap();
    let account=s.accounts.create(&s,Provider::Claude,"Settings").await.unwrap();
    let input=Input {method:"PUT".into(),path:format!("/api/accounts/{}",account.id),body:json!({"revision":account.revision,"settings":{"name":"Renamed","enabled":false,"maxConcurrentRuns":2}}),identity:Some(InstallationIdentity::trusted(InstallationRole::Owner,"owner")),query:Default::default(),headers:Default::default()};
    let replaced=api::dispatch(&s,&input).await.unwrap();
    assert_eq!(replaced["name"],"Renamed");
    assert_eq!(replaced["enabled"],false);
    assert!(replaced.get("identity").is_none());
    assert!(replaced.get("extra").is_none());
    assert_eq!(api::dispatch(&s,&input).await.unwrap_err().status,409);
    assert_eq!(s.accounts.account(&s,&account.id).await.unwrap().name,"Renamed");
}
