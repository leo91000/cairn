use leo_agent_manager::{
    accounts::{self, KIND},
    claude, claude_process,
    config::{Config, now},
    provider::Provider,
    service::Service,
};
use serde_json::{Value, json};
use std::sync::Arc;
use tempfile::TempDir;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

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

async fn sign_in_until(s: &Service, condition: impl Fn(&Value) -> bool) -> Value {
    tokio::time::timeout(std::time::Duration::from_secs(8), async {
        loop {
            let view = s.accounts.sign_in().await;
            if condition(&view) {
                return view;
            }
            tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        }
    })
    .await
    .unwrap()
}

/// Submits `code` once the sign-in page is ready, and waits for the outcome.
async fn finish_sign_in(s: &Service, code: &str) -> Value {
    let view = sign_in_until(s, |v| v["acceptsCode"] == true).await;
    assert_eq!(view["url"], "https://claude.com/oauth/authorize?fixture=1");
    s.accounts.submit_code(code).await.unwrap();
    sign_in_until(s, |v| v["state"] != "pending").await
}

/// A signed-in Claude account, as if it had completed sign-in.
async fn connected(s: &Arc<Service>) -> String {
    let mut account = s
        .accounts
        .create(s, Provider::Claude, "Personal")
        .await
        .unwrap();
    let id = account["id"].as_str().unwrap().to_owned();
    let home = accounts::claude::account_home(&s.config, &id);
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(
        home.join(".credentials.json"),
        json!({
            "claudeAiOauth": {
                "accessToken": "fixture-access",
                "expiresAt": now()+3600000,
                "scopes": ["user:inference"]
            }
        })
        .to_string(),
    )
    .unwrap();
    account["state"] = "ready".into();
    s.store.put(KIND, account).await.unwrap();
    id
}

async fn view(s: &Service, id: &str) -> Value {
    s.accounts
        .list(s)
        .await
        .unwrap()
        .into_iter()
        .find(|a| a["id"] == id)
        .unwrap()
}

async fn reset_usage_backoff(s: &Service, id: &str) {
    let mut account = s.accounts.get(s, id).await.unwrap();
    account["usage"]["attemptedAt"] = 0.into();
    s.store.put(KIND, account).await.unwrap();
}

#[tokio::test]
async fn sign_in_cancellation_failure_retry_identity_and_removal() {
    let root = TempDir::new().unwrap();
    let s = Service::new(config(&root)).await.unwrap();
    let started = s
        .accounts
        .add(&s, Provider::Claude, "Personal")
        .await
        .unwrap();
    assert_eq!(started["provider"], "claude");
    sign_in_until(&s, |v| v["acceptsCode"] == true).await;
    // One sign-in at a time, whichever the coding agent.
    assert!(s.accounts.add(&s, Provider::Codex, "Other").await.is_err());
    s.accounts.cancel(&s).await.unwrap();
    assert!(s.accounts.sign_in().await.is_null());
    assert!(s.accounts.list(&s).await.unwrap().is_empty());
    assert!(s.accounts.submit_code("fixture-code").await.is_err());

    let failed = s
        .accounts
        .add(&s, Provider::Claude, "Personal")
        .await
        .unwrap();
    let id = failed["accountId"].as_str().unwrap().to_owned();
    let failed = finish_sign_in(&s, "wrong").await;
    assert_eq!(failed["state"], "failed");
    assert!(!failed.to_string().contains("never-return"));
    assert_eq!(view(&s, &id).await["status"], "signIn");
    s.accounts.reconnect(&s, &id).await.unwrap();
    assert_eq!(
        finish_sign_in(&s, "fixture-code").await["state"],
        "complete"
    );
    let account = view(&s, &id).await;
    assert_eq!(account["state"], "ready");
    assert_eq!(account["email"], "claude-fixture@example.test");
    assert_eq!(account["plan"], "max");
    assert_eq!(account["usage"]["windows"][0]["usedPercent"], 25.0);
    assert!(account.get("identity").is_none());
    assert!(!account.to_string().contains("never-return"));
    assert!(!account.to_string().contains("refresh"));
    assert!(!s.config.data_dir.join("account-login").join(&id).exists());

    let catalog = claude::model_catalog(&s).await.unwrap();
    assert_eq!(catalog["stale"], false);
    assert_eq!(catalog["models"][0]["model"], "sonnet");
    assert!(
        catalog["models"][0]["supportedReasoningEfforts"]
            .as_array()
            .unwrap()
            .iter()
            .any(|effort| effort["reasoningEffort"] == "high")
    );
    // The default alias shows the model and effort Claude Code actually uses.
    assert_eq!(catalog["models"][0]["defaultReasoningEffort"], "high");
    assert_eq!(catalog["models"][2]["model"], "default");
    assert_eq!(
        catalog["models"][2]["displayName"],
        "Opus Fixture (1M context)"
    );
    assert_eq!(catalog["models"][2]["defaultReasoningEffort"], "medium");
    // A catalog stored before these labels existed.
    s.store
        .set(
            claude::CATALOG,
            json!({
                "models": [{
                    "model": "default",
                    "displayName": "Default (recommended)",
                    "description": "Opus 5.5 with 1M context · Best for everyday tasks",
                    "isDefault": true,
                    "defaultReasoningEffort": "",
                    "supportedReasoningEfforts": []
                }],
                "checkedAt": now(),
                "stale": false,
                "error": ""
            }),
            None,
        )
        .await
        .unwrap();
    let catalog = claude::model_catalog(&s).await.unwrap();
    assert_eq!(
        catalog["models"][0]["displayName"],
        "Opus 5.5 with 1M context"
    );

    // The same Claude identity cannot be connected twice; another one can.
    s.accounts
        .add(&s, Provider::Claude, "Duplicate")
        .await
        .unwrap();
    let duplicate = finish_sign_in(&s, "fixture-code").await;
    assert_eq!(duplicate["state"], "failed");
    assert!(
        duplicate["error"]
            .as_str()
            .unwrap()
            .contains("already connected")
    );
    let second = s
        .accounts
        .reconnect(&s, duplicate["accountId"].as_str().unwrap())
        .await
        .unwrap();
    assert_eq!(
        finish_sign_in(&s, "fixture-code:work@example.test").await["state"],
        "complete"
    );
    let second = second["accountId"].as_str().unwrap().to_owned();
    assert_eq!(view(&s, &second).await["email"], "work@example.test");
    assert_eq!(
        s.accounts
            .records(&s, Provider::Claude)
            .await
            .unwrap()
            .len(),
        2
    );

    s.accounts.remove(&s, &id).await.unwrap();
    assert!(!accounts::claude::account_home(&s.config, &id).exists());
    assert_eq!(
        s.accounts
            .records(&s, Provider::Claude)
            .await
            .unwrap()
            .len(),
        1
    );
}

fn plan(root: &TempDir, prompt: &str) -> Value {
    json!({
        "provider": "claude",
        "execution": {"messageId": "original","text": prompt,"attachments": []},
        "instructions": "Follow the task scope",
        "inputDirectory": root.path().join("inbox"),
        "output": root.path().join("result.md"),
        "cwd": root.path(),
        "model": "sonnet",
        "reasoning": "high",
        "sandbox": "yolo",
        "writableRoots": [root.path()],
        "claudeMcps": {"mcpServers": {}},
        "claudeDeniedTools": []
    })
}

fn setup(root: &TempDir) -> Config {
    let c = config(root);
    std::fs::create_dir_all(c.home.join(".claude")).unwrap();
    std::fs::write(c.home.join(".claude/.credentials.json"), "yes").unwrap();
    std::fs::create_dir(root.path().join("inbox")).unwrap();
    c
}

#[tokio::test]
async fn streaming_tools_receipts_resume_and_no_replay_after_completion() {
    let root = TempDir::new().unwrap();
    let c = setup(&root);
    let p = plan(&root, "Inspect the workspace");
    let (tx, mut rx) = mpsc::channel(64);
    claude_process::run(&c, p.clone(), tx, CancellationToken::new())
        .await
        .unwrap();
    let mut events = Vec::new();
    while let Some(e) = rx.recv().await {
        events.push(e);
    }
    assert!(events.iter().any(|e| e["type"] == "thread.started"));
    assert!(events.iter().any(|e| e["type"] == "item.updated"));
    assert!(
        events
            .iter()
            .any(|e| e["item"]["aggregated_output"] == "fixture")
    );
    assert!(
        events
            .iter()
            .any(|e| e["type"] == "chat.delivered" && e["messageId"] == "original")
    );
    let (tx, mut rx) = mpsc::channel(64);
    claude_process::run(&c, p.clone(), tx, CancellationToken::new())
        .await
        .unwrap();
    while rx.recv().await.is_some() {}
    assert_eq!(
        std::fs::read_to_string(c.home.join(".claude/invocations.jsonl"))
            .unwrap()
            .lines()
            .count(),
        1
    );
    let mut next = p;
    next["execution"]["messageId"] = "next".into();
    next["sessionId"] = "70f5e7a1-8d65-4f5f-a545-af6ee8c0e1ab".into();
    let (tx, mut rx) = mpsc::channel(64);
    claude_process::run(&c, next, tx, CancellationToken::new())
        .await
        .unwrap();
    while rx.recv().await.is_some() {}
    let calls = std::fs::read_to_string(c.home.join(".claude/invocations.jsonl")).unwrap();
    assert!(calls.contains("--resume"));
    assert_eq!(calls.lines().count(), 2);
}

#[tokio::test]
async fn interrupted_resume_uses_a_fresh_wire_id_and_preserves_chat_receipts() {
    for prompt in [
        "fixture:resume-dedup",
        "fixture:resume-dedup fixture:result-ack",
    ] {
        let root = TempDir::new().unwrap();
        let c = setup(&root);
        let mut p = plan(&root, prompt);
        let stop = CancellationToken::new();
        let (tx, mut rx) = mpsc::channel(64);
        let first = tokio::spawn({
            let c = c.clone();
            let p = p.clone();
            let stop = stop.clone();
            async move { claude_process::run(&c, p, tx, stop).await }
        });
        tokio::time::timeout(std::time::Duration::from_secs(8), async {
            while let Some(event) = rx.recv().await {
                if event["type"] == "chat.delivered" {
                    assert_eq!(event["messageId"], "original");
                    stop.cancel();
                }
            }
        })
        .await
        .unwrap();
        assert!(first.await.unwrap().is_err());
        assert!(!root.path().join("result.claude-receipt.json").exists());

        p["sessionId"] = "70f5e7a1-8d65-4f5f-a545-af6ee8c0e1ab".into();
        let (tx, mut rx) = mpsc::channel(64);
        tokio::time::timeout(
            std::time::Duration::from_secs(8),
            claude_process::run(&c, p, tx, CancellationToken::new()),
        )
        .await
        .expect("replayed UUID must not leave the continuation waiting")
        .unwrap();
        let mut completed = false;
        while let Some(event) = rx.recv().await {
            if event["type"] == "chat.delivered" {
                assert_eq!(event["messageId"], "original");
            }
            completed |= event["type"] == "turn.completed";
        }
        assert!(completed);
        let inputs = std::fs::read_to_string(c.home.join(".claude/user-messages.jsonl"))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(inputs.len(), 2);
        assert_eq!(inputs[0]["uuid"], "original");
        assert_ne!(inputs[1]["uuid"], inputs[0]["uuid"]);
        let receipt: Value = serde_json::from_slice(
            &std::fs::read(root.path().join("result.claude-receipt.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(receipt["messageId"], "original");
        assert_eq!(receipt["delivered"], json!(["original"]));
    }
}

#[tokio::test]
async fn reasoning_before_text_keeps_one_message_per_block() {
    let root = TempDir::new().unwrap();
    let c = setup(&root);
    let (tx, mut rx) = mpsc::channel(64);
    claude_process::run(
        &c,
        plan(&root, "Inspect fixture:thinking"),
        tx,
        CancellationToken::new(),
    )
    .await
    .unwrap();
    let mut events = Vec::new();
    while let Some(e) = rx.recv().await {
        events.push(e);
    }
    let ids = |kind: &str| {
        events
            .iter()
            .filter(|e| e["item"]["type"] == kind)
            .map(|e| e["item"]["id"].as_str().unwrap().to_owned())
            .collect::<std::collections::BTreeSet<_>>()
    };
    // Streamed deltas and the per-block assistant events must describe the same items.
    let texts = ids("agent_message");
    let reasoning = ids("reasoning");
    assert_eq!(texts.len(), 1, "{events:#?}");
    assert_eq!(reasoning.len(), 1, "{events:#?}");
    assert!(texts.is_disjoint(&reasoning));
    assert!(events.iter().any(|e| e["type"] == "item.completed"
        && e["item"]["type"] == "agent_message"
        && e["item"]["text"] == "Claude fixture completed"));
}

#[tokio::test]
async fn question_answers_use_control_protocol() {
    let root = TempDir::new().unwrap();
    let c = setup(&root);
    let p = plan(&root, "fixture:question");
    let (tx, mut rx) = mpsc::channel(64);
    let task =
        tokio::spawn(async move { claude_process::run(&c, p, tx, CancellationToken::new()).await });
    tokio::time::timeout(std::time::Duration::from_secs(8), async {
        while let Some(e) = rx.recv().await {
            if e["type"] == "chat.question" {
                assert_eq!(e["question"]["id"].as_str().unwrap().len(), 64);
                std::fs::write(
                    root.path().join("inbox/messages.json"),
                    json!([{
                        "id": "answer",
                        "questionId": e["question"]["id"],
                        "answers": {"0": ["Small change"]}
                    }])
                    .to_string(),
                )
                .unwrap();
            }
        }
    })
    .await
    .unwrap();
    task.await.unwrap().unwrap();
    let response: Value = serde_json::from_slice(
        &std::fs::read(root.path().join("home/.claude/question-response.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        response["response"]["response"]["updatedInput"]["answers"]["Which approach?"],
        "Small change"
    );
}

#[tokio::test]
async fn cancellation_and_provider_errors_do_not_complete_the_turn() {
    for prompt in ["fixture:hang", "fixture:fail", "fixture:background"] {
        let root = TempDir::new().unwrap();
        let c = setup(&root);
        let p = plan(&root, prompt);
        let cancel = CancellationToken::new();
        let stop = cancel.clone();
        let (tx, mut rx) = mpsc::channel(64);
        let task = tokio::spawn(async move { claude_process::run(&c, p, tx, stop).await });
        let timer = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            timer.cancel();
        });
        while let Some(e) = rx.recv().await {
            assert_ne!(e["type"], "turn.completed");
        }
        assert!(task.await.unwrap().is_err());
        assert!(!root.path().join("result.md").exists());
    }
}

#[test]
fn provider_defaults_and_sandbox_arguments_preserve_boundaries() {
    assert_eq!(Provider::of_agent(&json!({})), Provider::Codex);
    assert!(claude::validate_agent(&json!({"provider":"claude","reasoning":"ultra"})).is_err());
    let root = TempDir::new().unwrap();
    let mut p = plan(&root, "test");
    p["sandbox"] = "workspace-write".into();
    let a = claude_process::args(&p);
    assert!(!a.contains(&"--dangerously-skip-permissions".into()));
    assert!(a.iter().any(|s| s.contains("failIfUnavailable")));
    assert!(a.contains(&"--strict-mcp-config".into()));
}

#[tokio::test]
async fn steering_waits_for_both_responses_and_acknowledges_each_message() {
    let root = TempDir::new().unwrap();
    let c = setup(&root);
    let p = plan(&root, "fixture:slow");
    let (tx, mut rx) = mpsc::channel(64);
    let task =
        tokio::spawn(async move { claude_process::run(&c, p, tx, CancellationToken::new()).await });
    let mut delivered = Vec::new();
    let mut responses = 0;
    tokio::time::timeout(std::time::Duration::from_secs(15), async {
        while let Some(e) = rx.recv().await {
            if e["type"] == "chat.delivered" {
                delivered.push(e["messageId"].clone());
                if e["messageId"] == "original" {
                    std::fs::write(
                        root.path().join("inbox/messages.json"),
                        json!([{"id": "steering","text": "fixture:slow additional instruction","attachments": []}]).to_string(),
                    )
                    .unwrap();
                }
            }
            if e["type"] == "item.completed" && e["item"]["type"] == "agent_message" {
                responses += 1;
            }
            if e["type"] == "turn.completed" {
                assert_eq!(responses,2);
            }
        }
    })
    .await
    .unwrap();
    task.await.unwrap().unwrap();
    assert_eq!(delivered, vec![json!("original"), json!("steering")]);
}

#[tokio::test]
async fn restored_background_results_cannot_complete_an_undelivered_prompt_or_receipt() {
    let root = TempDir::new().unwrap();
    let c = setup(&root);
    std::fs::write(
        root.path().join("result.claude-receipt.json"),
        json!({"messageId": "original","delivered": [],"text": ""}).to_string(),
    )
    .unwrap();
    let events = run_events(&c, plan(&root, "fixture:startup-result")).await;
    assert!(
        events
            .iter()
            .any(|e| e["type"] == "chat.delivered" && e["messageId"] == "original")
    );
    assert_eq!(
        std::fs::read_to_string(root.path().join("result.md")).unwrap(),
        "Actual requested response"
    );
}

async fn run_events(c: &Config, p: Value) -> Vec<Value> {
    let (tx, mut rx) = mpsc::channel(128);
    tokio::time::timeout(
        std::time::Duration::from_secs(8),
        claude_process::run(c, p, tx, CancellationToken::new()),
    )
    .await
    .unwrap()
    .unwrap();
    let mut events = Vec::new();
    while let Some(e) = rx.recv().await {
        events.push(e);
    }
    assert_eq!(
        events
            .iter()
            .filter(|e| e["type"] == "turn.completed")
            .count(),
        1
    );
    events
}

#[tokio::test]
async fn background_work_and_its_followup_finish_before_the_run_completes() {
    let root = TempDir::new().unwrap();
    let c = setup(&root);
    run_events(&c, plan(&root, "fixture:background")).await;
    assert_eq!(
        std::fs::read_to_string(root.path().join("result.md")).unwrap(),
        "Build checked and task finished"
    );
    let receipt: Value = serde_json::from_slice(
        &std::fs::read(root.path().join("result.claude-receipt.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(receipt["text"], "Build checked and task finished");
}

#[tokio::test]
async fn ambient_watchers_do_not_keep_the_run_alive() {
    for prompt in [
        "fixture:background-ambient",
        "fixture:background-ambient-flip",
    ] {
        let root = TempDir::new().unwrap();
        let c = setup(&root);
        run_events(&c, plan(&root, prompt)).await;
    }
}

#[tokio::test]
async fn correlated_result_acknowledges_a_prompt_without_a_user_replay() {
    let root = TempDir::new().unwrap();
    let c = setup(&root);
    let events = run_events(&c, plan(&root, "fixture:result-ack")).await;
    assert_eq!(
        events
            .iter()
            .filter(|e| e["type"] == "chat.delivered" && e["messageId"] == "original")
            .count(),
        1
    );
}

#[tokio::test]
async fn merged_prompts_need_only_one_correlated_result() {
    let root = TempDir::new().unwrap();
    let c = setup(&root);
    std::fs::write(
        root.path().join("inbox/messages.json"),
        json!([{"id": "steering","text": "fixture:batch second prompt","attachments": []}])
            .to_string(),
    )
    .unwrap();
    let events = run_events(&c, plan(&root, "fixture:batch first prompt")).await;
    assert_eq!(
        events
            .iter()
            .filter(|e| e["type"] == "chat.delivered")
            .count(),
        2
    );
    assert_eq!(
        std::fs::read_to_string(root.path().join("result.md")).unwrap(),
        "Both prompts processed"
    );
}

#[tokio::test]
async fn usage_is_sanitized_cached_and_preserved_on_failure_without_changing_connection() {
    let root = TempDir::new().unwrap();
    let s = Service::new(config(&root)).await.unwrap();
    let id = connected(&s).await;
    let home = accounts::claude::account_home(&s.config, &id);
    s.accounts.refresh(&s, &id).await.unwrap();
    let first = view(&s, &id).await;
    assert_eq!(first["state"], "ready");
    assert_eq!(first["status"], "next");
    assert_eq!(first["remainingPercent"], 40.0);
    assert_eq!(first["usage"]["windows"][0]["usedPercent"], 25.0);
    assert_eq!(first["usage"]["windows"][0]["resetsAt"], 1893499200i64);
    assert_eq!(first["usage"]["windows"][2]["label"], "Weekly · Sonnet");
    assert_eq!(first["usage"]["windows"][2]["models"], json!(["sonnet"]));
    assert_eq!(first["stale"], false);
    assert!(!first.to_string().contains("never-return"));
    assert!(
        !home.join("user-messages.jsonl").exists(),
        "Quota reads must not submit a prompt"
    );
    let requests = || {
        std::fs::read_to_string(home.join("usage-requests.jsonl"))
            .unwrap()
            .lines()
            .count()
    };
    // Usage is read at most every five minutes, even when the account is refreshed again.
    s.accounts.refresh(&s, &id).await.unwrap();
    assert_eq!(view(&s, &id).await["usage"], first["usage"]);
    assert_eq!(requests(), 1);
    assert!(
        std::fs::read_to_string(home.join("usage-requests.jsonl"))
            .unwrap()
            .contains("\"skip_behaviors\":true")
    );

    // A failed read keeps dated values, backs off, and does not disconnect.
    reset_usage_backoff(&s, &id).await;
    std::fs::write(home.join("fixture-usage.json"), "{\"fixtureError\":true}").unwrap();
    s.accounts.refresh(&s, &id).await.unwrap();
    let failed = view(&s, &id).await;
    assert_eq!(failed["state"], "ready");
    assert_eq!(failed["usage"]["windows"], first["usage"]["windows"]);
    assert_eq!(failed["usage"]["checkedAt"], first["usage"]["checkedAt"]);
    assert!(failed["usage"]["error"].is_string());
    assert!(!failed.to_string().contains("never-return"));
    s.accounts.refresh(&s, &id).await.unwrap();
    assert_eq!(requests(), 2);

    // Usage is read while the account runs: only the manager rotates its credentials.
    reset_usage_backoff(&s, &id).await;
    std::fs::remove_file(home.join("fixture-usage.json")).unwrap();
    let lease = s
        .accounts
        .acquire(
            &s,
            "11111111-1111-4111-8111-111111111111",
            Provider::Claude,
            "sonnet",
        )
        .await
        .unwrap()
        .unwrap();
    s.accounts.refresh(&s, &id).await.unwrap();
    let running = view(&s, &id).await;
    assert!(running["usage"]["error"].is_null());
    assert_eq!(
        running["activeRunIds"],
        json!(["11111111-1111-4111-8111-111111111111"])
    );
    assert_eq!(requests(), 3);
    s.accounts.release(&lease).await.unwrap();

    s.accounts.remove(&s, &id).await.unwrap();
    assert!(s.accounts.list(&s).await.unwrap().is_empty());
}

#[tokio::test]
async fn an_exhausted_account_returns_once_usage_shows_capacity_again() {
    let root = TempDir::new().unwrap();
    let s = Service::new(config(&root)).await.unwrap();
    let id = connected(&s).await;
    let home = accounts::claude::account_home(&s.config, &id);
    let read = async |used: u32| {
        let payload = json!({
            "rate_limits_available": true,
            "rate_limits": {
                "five_hour": {"utilization": used,"resets_at": "2030-01-01T12:00:00Z"},
                "seven_day": {"utilization": 60}
            }
        });
        std::fs::write(home.join("fixture-usage.json"), payload.to_string()).unwrap();
        reset_usage_backoff(&s, &id).await;
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        s.accounts.refresh(&s, &id).await.unwrap();
        view(&s, &id).await["status"].clone()
    };
    read(90).await;
    s.accounts.exhausted(&s, &id, "sonnet").await.unwrap();
    assert_eq!(view(&s, &id).await["status"], "waiting");
    // Usage that has not come down yet keeps the account waiting.
    assert_eq!(read(100).await, "waiting");
    let run = "11111111-1111-4111-8111-111111111111";
    assert!(
        s.accounts
            .acquire(&s, run, Provider::Claude, "sonnet")
            .await
            .is_err()
    );
    assert_eq!(read(0).await, "next");
    let lease = s
        .accounts
        .acquire(&s, run, Provider::Claude, "sonnet")
        .await
        .unwrap()
        .unwrap();
    s.accounts.release(&lease).await.unwrap();

    // Without usage to compare, the account is tried again after a while.
    std::fs::write(home.join("fixture-usage.json"), "{\"fixtureError\":true}").unwrap();
    s.accounts.exhausted(&s, &id, "sonnet").await.unwrap();
    let mut account = s.accounts.get(&s, &id).await.unwrap();
    account["usage"] = Value::Null;
    account["exhausted"]["usage"] = Value::Null;
    s.store.put(KIND, account).await.unwrap();
    s.accounts.refresh(&s, &id).await.unwrap();
    assert_eq!(view(&s, &id).await["status"], "waiting");
    let mut account = s.accounts.get(&s, &id).await.unwrap();
    account["exhausted"]["at"] = (now() - 16 * 60_000).into();
    s.store.put(KIND, account).await.unwrap();
    s.accounts.refresh(&s, &id).await.unwrap();
    assert_eq!(view(&s, &id).await["status"], "next");
}

#[tokio::test]
async fn opus_windows_limit_every_model_that_runs_opus() {
    let root = TempDir::new().unwrap();
    let s = Service::new(config(&root)).await.unwrap();
    let id = connected(&s).await;
    let home = accounts::claude::account_home(&s.config, &id);
    s.store
        .set(claude::CATALOG, json!({
            "models": [{"model": "opus"},{"model": "default","resolvedModel": "claude-opus-fixture[1m]"},{"model": "sonnet","resolvedModel": "claude-sonnet-fixture"}]
        }), None)
        .await
        .unwrap();
    let payload = json!({
        "rate_limits_available": true,
        "rate_limits": {
            "five_hour": {"utilization": 10},
            "seven_day": {"utilization": 20},
            "seven_day_opus": {"utilization": 100}
        }
    });
    std::fs::write(home.join("fixture-usage.json"), payload.to_string()).unwrap();
    s.accounts.refresh(&s, &id).await.unwrap();
    let run = "11111111-1111-4111-8111-111111111111";
    for model in ["opus", "default"] {
        assert!(
            s.accounts
                .acquire(&s, run, Provider::Claude, model)
                .await
                .unwrap_err()
                .message
                .contains("available usage"),
            "{model}"
        );
    }
    let lease = s
        .accounts
        .acquire(&s, run, Provider::Claude, "sonnet")
        .await
        .unwrap()
        .unwrap();
    s.accounts.release(&lease).await.unwrap();
}

#[tokio::test]
async fn usage_handles_partial_invalid_and_unavailable_windows() {
    let root = TempDir::new().unwrap();
    let s = Service::new(config(&root)).await.unwrap();
    let id = connected(&s).await;
    let home = accounts::claude::account_home(&s.config, &id);
    let payload = json!({
        "rate_limits_available": true,
        "rate_limits": {
            "five_hour": {"utilization": 0,"resets_at": null},
            "seven_day": {"utilization": 105,"resets_at": "invalid"},
            "seven_day_opus": {"utilization": null},
            "seven_day_sonnet": {"utilization": -1},
            "model_scoped": [{
                "display_name": "Fable",
                "utilization": 42,
                "resets_at": "2030-01-07T12:00:00Z",
                "secret": "never-return"
            }],
            "unknown_secret": "never-return"
        }
    });
    std::fs::write(home.join("fixture-usage.json"), payload.to_string()).unwrap();
    s.accounts.refresh(&s, &id).await.unwrap();
    let account = view(&s, &id).await;
    let windows = account["usage"]["windows"].as_array().unwrap();
    assert_eq!(windows.len(), 3);
    assert_eq!(windows[0]["usedPercent"], 0.0);
    assert_eq!(windows[1]["usedPercent"], 105.0);
    assert!(windows[1]["resetsAt"].is_null());
    assert_eq!(windows[2]["label"], "Weekly · Fable");
    assert!(!account.to_string().contains("never-return"));
    // The weekly window is spent, so the account waits for its reset.
    assert_eq!(account["status"], "waiting");
    assert!(
        s.accounts
            .acquire(
                &s,
                "11111111-1111-4111-8111-111111111111",
                Provider::Claude,
                ""
            )
            .await
            .unwrap_err()
            .message
            .contains("available usage")
    );

    for payload in [
        json!({"rate_limits_available": false,"rate_limits": null}),
        json!({"rate_limits_available": true,"rate_limits": {"five_hour": {"utilization": "25"}}}),
    ] {
        let mut account = s.accounts.get(&s, &id).await.unwrap();
        account["usage"] = Value::Null;
        s.store.put(KIND, account).await.unwrap();
        std::fs::write(home.join("fixture-usage.json"), payload.to_string()).unwrap();
        s.accounts.refresh(&s, &id).await.unwrap();
        let account = view(&s, &id).await;
        assert_eq!(account["state"], "ready");
        assert_eq!(account["usage"]["windows"], json!([]));
        assert!(account["usage"]["checkedAt"].is_null());
        assert_eq!(account["stale"], true);
        // Unknown Claude usage never blocks runs.
        assert_eq!(account["status"], "next");
    }
}

#[tokio::test]
async fn parallel_runs_are_validated_and_lowering_them_keeps_current_runs() {
    let root = TempDir::new().unwrap();
    let s = Service::new(config(&root)).await.unwrap();
    let id = connected(&s).await;
    assert_eq!(view(&s, &id).await["maxConcurrentRuns"], 4);
    for limit in [json!(0), json!(-1), json!(1.5), json!("4"), Value::Null] {
        assert!(
            s.accounts
                .update(&s, &id, &json!({"maxConcurrentRuns":limit}))
                .await
                .is_err()
        );
    }
    let leases = [
        "11111111-1111-4111-8111-111111111111",
        "22222222-2222-4222-8222-222222222222",
    ];
    let mut held = Vec::new();
    for run in leases {
        held.push(
            s.accounts
                .acquire(&s, run, Provider::Claude, "sonnet")
                .await
                .unwrap()
                .unwrap(),
        );
    }
    let updated = s
        .accounts
        .update(&s, &id, &json!({"maxConcurrentRuns": 1}))
        .await
        .unwrap();
    assert_eq!(updated["maxConcurrentRuns"], 1);
    assert_eq!(updated["status"], "full");
    assert_eq!(updated["activeRunIds"].as_array().unwrap().len(), 2);
    assert!(
        s.accounts
            .acquire(
                &s,
                "33333333-3333-4333-8333-333333333333",
                Provider::Claude,
                ""
            )
            .await
            .unwrap_err()
            .message
            .contains("free Claude Code account slot")
    );
    for lease in &held {
        s.accounts.release(lease).await.unwrap();
    }
    assert!(!updated.to_string().contains("refreshToken"));
}
