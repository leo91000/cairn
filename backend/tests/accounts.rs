use leo_agent_manager::{
    accounts::{self, KIND},
    config::{Config, now},
    provider::Provider,
    service::Service,
    store::Store,
    validation::text,
};
use serde_json::{Value, json};
use tempfile::TempDir;

fn config(root: &TempDir) -> Config {
    serde_json::from_value(json!({
        "dataDir": root.path().join("data"),
        "home": root.path().join("home"),
        "workspaceRoots": [root.path()],
        "publicUrl": "http://localhost:4310",
        "host": "127.0.0.1",
        "port": 0,
        "setupToken": "test",
        "codexBin": "/nonexistent-codex",
        "claudeBin": std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../tests/fixtures/claude.mjs"),
        "ghBin": "gh",
        "concurrency": 1,
        "logger": false,
        "workerEnabled": false,
        "runnerUrl": ""
    })).unwrap()
}

#[tokio::test]
async fn codex_accounts_and_the_single_claude_login_migrate_into_one_pool() {
    let root = TempDir::new().unwrap();
    let c = config(&root);
    let codex = "11111111-1111-4111-8111-111111111111";
    let session = "70f5e7a1-8d65-4f5f-a545-af6ee8c0e1ab";
    {
        // State written by earlier versions.
        let store = Store::open(&c.data_dir).unwrap();
        let limits = json!({
            "ordinaryUsageAllowed": true,
            "rateLimits": {
                "limitId": "codex",
                "primary": {"usedPercent": 20,"windowDurationMins": 300,"resetsAt": 1},
                "secondary": {"usedPercent": 70,"windowDurationMins": 10080,"resetsAt": 2}
            },
            "rateLimitResetCredits": {"availableCount": 3,"credits": []}
        });
        store
            .put(
                "codexAccounts",
                json!({
                    "id": codex,
                    "name": "Work",
                    "enabled": true,
                    "email": "work@example.test",
                    "plan": "plus",
                    "identity": "fingerprint",
                    "createdAt": 1,
                    "checkedAt": now(),
                    "state": "ready",
                    "error": "",
                    "limits": limits,
                    "lastUsedAt": null,
                    "exhausted": {"at": 1,"model": "","limits": limits},
                    "maxConcurrentRuns": 2
                }),
            )
            .await
            .unwrap();
        store
            .write(move |db| {
                db.add_run(
                    &json!({
                        "id": "codex-run",
                        "taskId": "a",
                        "status": "succeeded",
                        "createdAt": 1,
                        "codexAccountId": codex,
                        "codexAccountName": "Work",
                        "codexAuthMode": "external",
                        "snapshot": {"agent": {}}
                    }),
                    None,
                )?;
                db.add_run(
                    &json!({
                        "id": "claude-run",
                        "taskId": "b",
                        "status": "succeeded",
                        "createdAt": 2,
                        "sessionId": session,
                        "isolated": false,
                        "snapshot": {"agent": {"provider": "claude"}}
                    }),
                    None,
                )?;
                Ok(())
            })
            .await
            .unwrap();
        store
            .set(
                "claude-status",
                json!({"connected": true,"email": "old@example.test","subscriptionType": "pro"}),
                None,
            )
            .await
            .unwrap();
        store
            .set("claude-concurrency", json!(3), None)
            .await
            .unwrap();
        store
            .set("claude-usage", json!({"windows": []}), None)
            .await
            .unwrap();
    }
    let legacy = c.data_dir.join("claude");
    std::fs::create_dir_all(legacy.join("projects/-data-project")).unwrap();
    std::fs::write(legacy.join(".credentials.json"), json!({
        "claudeAiOauth": {"accessToken": "old","refreshToken": "old-refresh","expiresAt": now()+3600000}
    }).to_string()).unwrap();
    std::fs::write(legacy.join(".claude.json"), "{}").unwrap();
    // The old serialized credential transfer was interrupted.
    std::fs::write(legacy.join("sync-required"), "claude-run").unwrap();
    std::fs::write(
        legacy.join(format!("projects/-data-project/{session}.jsonl")),
        "{\"type\":\"user\"}\n",
    )
    .unwrap();

    let s = Service::new(c).await.unwrap();
    s.accounts.initialize(&s).await.unwrap();
    let migrated = s.accounts.get(&s, codex).await.unwrap();
    assert_eq!(migrated["provider"], "codex");
    assert!(migrated.get("limits").is_none());
    assert_eq!(migrated["usage"]["windows"][1]["usedPercent"], 70);
    assert_eq!(migrated["usage"]["resets"]["available"], 3);
    assert_eq!(
        migrated["exhausted"]["usage"]["windows"][0]["usedPercent"],
        20
    );
    assert!(s.store.list("codexAccounts").await.unwrap().is_empty());
    let run = s.store.run("codex-run").await.unwrap();
    assert_eq!(run["accountId"], codex);
    assert_eq!(run["accountName"], "Work");
    for field in ["codexAccountId", "codexAccountName", "codexAuthMode"] {
        assert!(run.get(field).is_none(), "{field}");
    }

    let claude = s.accounts.records(&s, Provider::Claude).await.unwrap();
    assert_eq!(claude.len(), 1);
    let claude = &claude[0];
    assert_eq!(claude["email"], "old@example.test");
    assert_eq!(claude["plan"], "pro");
    assert_eq!(claude["maxConcurrentRuns"], 3);
    assert_eq!(claude["state"], "error");
    let home = accounts::claude::account_home(&s.config, text(claude, "id"));
    assert!(home.join(".credentials.json").exists());
    assert!(!home.join("sync-required").exists());
    assert!(!legacy.exists());
    for key in ["claude-status", "claude-usage", "claude-concurrency"] {
        assert!(s.store.kv(key).await.unwrap().is_none(), "{key}");
    }
    // A local run keeps its session: it now lives with the run.
    assert!(
        s.config
            .data_dir
            .join("runs/claude-run/home/.claude/projects/-data-project")
            .join(format!("{session}.jsonl"))
            .exists()
    );
    // Migrating again changes nothing.
    let again = Service::new(s.config.clone()).await.unwrap();
    again.accounts.initialize(&again).await.unwrap();
    assert_eq!(again.store.list(KIND).await.unwrap().len(), 2);
}

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
            "windows": [{"id": "w","usedPercent": used,"models": []}],
            "resets": null
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
        .update(&s, &ids[1], &json!({"enabled": false}))
        .await
        .unwrap();
    let list = s.accounts.list(&s).await.unwrap();
    assert_eq!(status(&list, &ids[0]), "next");
    assert_eq!(status(&list, &ids[1]), "paused");
    assert_eq!(
        s.accounts.overview(&s).await.unwrap()["required"],
        json!([])
    );
    // Once every Claude account is paused, Claude runs wait for the user.
    s.accounts
        .update(&s, &ids[2], &json!({"enabled": false}))
        .await
        .unwrap();
    assert_eq!(
        s.accounts.overview(&s).await.unwrap()["required"],
        json!(["claude"])
    );
    assert!(
        s.accounts
            .update(&s, &ids[0], &json!({"name":""}))
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
