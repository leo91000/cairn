mod common;

use common::eventually;
use leo_agent_manager::{
    accounts::{self, KIND, Lease},
    claude, claude_process,
    config::{Config, now},
    error::Result,
    provider::Provider,
    service::Service,
};
use serde_json::{Value, json};
use std::{collections::BTreeSet, sync::Arc, time::Duration};
use tempfile::TempDir;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

const FIRST_RUN: &str = "11111111-1111-4111-8111-111111111111";
const SECOND_RUN: &str = "22222222-2222-4222-8222-222222222222";
const RESUMED_SESSION: &str = "70f5e7a1-8d65-4f5f-a545-af6ee8c0e1ab";

/// A service whose Claude Code binary is the repository fixture, without Codex.
fn config(root: &TempDir) -> Config {
    Config {
        setup_token: "test".into(),
        codex_bin: "/nonexistent-codex".into(),
        claude_bin: common::fixture("claude.mjs"),
        ..common::config(root.path())
    }
}

async fn sign_in_until(s: &Service, condition: impl Fn(&Value) -> bool) -> Value {
    eventually(
        Duration::from_secs(8),
        Duration::from_millis(30),
        async || {
            let view = s.accounts.sign_in().await;
            condition(&view).then_some(view)
        },
    )
    .await
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
    let credentials = json!({
        "claudeAiOauth": {
            "accessToken": "fixture-access",
            "expiresAt": now() + 3_600_000,
            "scopes": ["user:inference"],
        },
    });
    std::fs::write(home.join(".credentials.json"), credentials.to_string()).unwrap();
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

/// Makes the next refresh read usage again, regardless of its back-off.
async fn reset_usage_backoff(s: &Service, id: &str) {
    let mut account = s.accounts.get(s, id).await.unwrap();
    account["usage"]["attemptedAt"] = 0.into();
    s.store.put(KIND, account).await.unwrap();
}

/// The catalog read by a signed-in account names the models and efforts Claude
/// Code uses, including for a catalog stored before those labels existed.
async fn assert_signed_in_catalog_describes_the_models(s: &Service) {
    let catalog = claude::model_catalog(s).await.unwrap();
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
                    "supportedReasoningEfforts": [],
                }],
                "checkedAt": now(),
                "stale": false,
                "error": "",
            }),
            None,
        )
        .await
        .unwrap();
    let catalog = claude::model_catalog(s).await.unwrap();
    assert_eq!(
        catalog["models"][0]["displayName"],
        "Opus 5.5 with 1M context"
    );
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

    assert_signed_in_catalog_describes_the_models(&s).await;

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
        "execution": { "messageId": "original", "text": prompt, "attachments": [] },
        "instructions": "Follow the task scope",
        "inputDirectory": root.path().join("inbox"),
        "output": root.path().join("result.md"),
        "cwd": root.path(),
        "model": "sonnet",
        "reasoning": "high",
        "sandbox": "yolo",
        "writableRoots": [root.path()],
        "claudeMcps": { "mcpServers": {} },
        "claudeDeniedTools": [],
    })
}

/// A configuration whose home holds Claude credentials, with an empty inbox.
fn setup(root: &TempDir) -> Config {
    let c = config(root);
    std::fs::create_dir_all(c.home.join(".claude")).unwrap();
    std::fs::write(c.home.join(".claude/.credentials.json"), "yes").unwrap();
    std::fs::create_dir(root.path().join("inbox")).unwrap();
    c
}

/// Runs the Claude process to completion and returns every event it sent.
async fn run_to_end(c: &Config, p: Value) -> (Result<()>, Vec<Value>) {
    let (tx, rx) = mpsc::channel(64);
    let result = claude_process::run(c, p, tx, CancellationToken::new()).await;
    (result, drain(rx).await)
}

async fn drain(mut rx: mpsc::Receiver<Value>) -> Vec<Value> {
    let mut events = Vec::new();
    while let Some(event) = rx.recv().await {
        events.push(event);
    }
    events
}

/// Writes messages the running process reads from its inbox.
fn deliver(root: &TempDir, messages: &Value) {
    std::fs::write(
        root.path().join("inbox/messages.json"),
        messages.to_string(),
    )
    .unwrap();
}

async fn acquire(s: &Service, run: &str, model: &str) -> Result<Option<Lease>> {
    s.accounts.acquire(s, run, Provider::Claude, model).await
}

fn completes(events: &[Value]) -> bool {
    events.iter().any(|event| event["type"] == "turn.completed")
}

#[tokio::test]
async fn corrupted_session_environment_blocks_launch_until_repaired() {
    let root = TempDir::new().unwrap();
    let c = setup(&root);
    let home = c.home.join(".claude");
    let environment = home.join("session-env").join(RESUMED_SESSION);
    std::fs::create_dir_all(&environment).unwrap();
    let hook = environment.join("sessionstart-hook-0.sh");
    let mut corrupt = b"export PRIVATE=never-return-this-secret\n".to_vec();
    corrupt.extend([0; 100]);
    corrupt.extend(b"export PATH=\"$HOME/.safe-chain/bin:$PATH\"\n");
    std::fs::write(&hook, &corrupt).unwrap();
    let mut p = plan(&root, "Inspect the workspace");
    p["sessionId"] = RESUMED_SESSION.into();

    let error = run_to_end(&c, p.clone()).await.0.unwrap_err();
    assert!(error.message.contains("session environment"));
    assert!(error.message.contains("sessionstart-hook-0.sh"));
    assert!(!error.message.contains("never-return-this-secret"));
    assert!(!home.join("invocations.jsonl").exists());
    assert!(!root.path().join("result.claude-receipt.json").exists());
    assert_eq!(std::fs::read(&hook).unwrap(), corrupt);

    // Repair is explicit: Leo must not guess what a corrupted shell script meant.
    std::fs::write(&hook, "export PATH=\"$HOME/.safe-chain/bin:$PATH\"\n").unwrap();
    let other = home.join("session-env/00000000-0000-4000-8000-000000000001");
    std::fs::create_dir_all(&other).unwrap();
    std::fs::write(other.join("sessionstart-hook-0.sh"), &corrupt).unwrap();
    let (result, events) = run_to_end(&c, p.clone()).await;
    result.unwrap();
    assert!(completes(&events));

    // A completed receipt remains replayable without reopening the environment
    // or starting another process, even if a hook became corrupt afterward.
    std::fs::write(&hook, &corrupt).unwrap();
    run_to_end(&c, p).await.0.unwrap();
    let invocations = std::fs::read_to_string(home.join("invocations.jsonl")).unwrap();
    assert_eq!(invocations.lines().count(), 1);
}

#[tokio::test]
async fn shell_environment_failure_stops_before_success_and_does_not_save_a_receipt() {
    for prompt in [
        "fixture:shell-environment",
        "fixture:shell-environment array-content",
        "fixture:shell-environment monitor-tool",
    ] {
        let root = TempDir::new().unwrap();
        let c = setup(&root);

        let (result, events) = run_to_end(&c, plan(&root, prompt)).await;
        let error = result.unwrap_err();
        assert!(error.message.contains("session environment"));
        assert!(error.message.contains("resume"));
        assert!(!error.message.contains("never-return-this-secret"));
        let mut failed = false;
        for event in &events {
            assert_ne!(event["type"], "turn.completed");
            assert!(!event.to_string().contains("never-return-this-secret"));
            failed |= event["type"] == "item.completed" && event["item"]["status"] == "failed";
        }
        assert!(failed);
        assert!(!root.path().join("result.claude-receipt.json").exists());

        let mut resume = plan(&root, "Continue after repairing the shell environment");
        resume["sessionId"] = RESUMED_SESSION.into();
        run_to_end(&c, resume).await.0.unwrap();
        let calls = std::fs::read_to_string(c.home.join(".claude/invocations.jsonl")).unwrap();
        assert_eq!(calls.lines().count(), 2);
        assert!(calls.lines().last().unwrap().contains("--resume"));
        assert!(root.path().join("result.claude-receipt.json").exists());
    }
}

#[tokio::test]
async fn ordinary_tool_errors_and_read_content_do_not_abort_claude() {
    for prompt in [
        "fixture:shell-environment ordinary-error",
        "fixture:shell-environment read-tool",
        "fixture:shell-environment successful-tool",
    ] {
        let root = TempDir::new().unwrap();
        let c = setup(&root);
        let (result, events) = run_to_end(&c, plan(&root, prompt)).await;
        result.unwrap();
        assert!(completes(&events));
    }
}

#[tokio::test]
async fn streaming_tools_receipts_resume_and_no_replay_after_completion() {
    let root = TempDir::new().unwrap();
    let c = setup(&root);
    let p = plan(&root, "Inspect the workspace");
    let (result, events) = run_to_end(&c, p.clone()).await;
    result.unwrap();
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
    run_to_end(&c, p.clone()).await.0.unwrap();
    let invocations = || std::fs::read_to_string(c.home.join(".claude/invocations.jsonl")).unwrap();
    assert_eq!(invocations().lines().count(), 1);
    let mut next = p;
    next["execution"]["messageId"] = "next".into();
    next["sessionId"] = RESUMED_SESSION.into();
    run_to_end(&c, next).await.0.unwrap();
    let calls = invocations();
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
        tokio::time::timeout(Duration::from_secs(8), async {
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

        p["sessionId"] = RESUMED_SESSION.into();
        let (tx, mut rx) = mpsc::channel(64);
        tokio::time::timeout(
            Duration::from_secs(8),
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
    let (result, events) = run_to_end(&c, plan(&root, "Inspect fixture:thinking")).await;
    result.unwrap();
    let ids = |kind: &str| {
        events
            .iter()
            .filter(|e| e["item"]["type"] == kind)
            .map(|e| e["item"]["id"].as_str().unwrap().to_owned())
            .collect::<BTreeSet<_>>()
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
    tokio::time::timeout(Duration::from_secs(8), async {
        while let Some(event) = rx.recv().await {
            if event["type"] != "chat.question" {
                continue;
            }
            let question = &event["question"]["id"];
            assert_eq!(question.as_str().unwrap().len(), 64);
            deliver(
                &root,
                &json!([{
                    "id": "answer",
                    "questionId": question,
                    "answers": { "0": ["Small change"] },
                }]),
            );
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
            tokio::time::sleep(Duration::from_millis(500)).await;
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
    assert!(
        claude::validate_agent(&json!({ "provider": "claude", "reasoning": "ultra" })).is_err()
    );
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
    tokio::time::timeout(Duration::from_secs(15), async {
        while let Some(event) = rx.recv().await {
            if event["type"] == "chat.delivered" {
                delivered.push(event["messageId"].clone());
                if event["messageId"] == "original" {
                    deliver(
                        &root,
                        &json!([{
                            "id": "steering",
                            "text": "fixture:slow additional instruction",
                            "attachments": [],
                        }]),
                    );
                }
            }
            if event["type"] == "item.completed" && event["item"]["type"] == "agent_message" {
                responses += 1;
            }
            if event["type"] == "turn.completed" {
                assert_eq!(responses, 2);
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
        json!({ "messageId": "original", "delivered": [], "text": "" }).to_string(),
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

/// Runs one turn within eight seconds and checks that it completed exactly once.
async fn run_events(c: &Config, p: Value) -> Vec<Value> {
    let (tx, rx) = mpsc::channel(128);
    tokio::time::timeout(
        Duration::from_secs(8),
        claude_process::run(c, p, tx, CancellationToken::new()),
    )
    .await
    .unwrap()
    .unwrap();
    let events = drain(rx).await;
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
    let events = run_events(&c, plan(&root, "fixture:background")).await;
    let waits: Vec<_> = events
        .iter()
        .filter(|e| e["type"] == "turn.waiting")
        .map(|e| e["tasks"].clone())
        .collect();
    // Idle while the build runs, then resuming once it finishes.
    assert_eq!(
        waits,
        vec![
            json!([{ "id": "build", "description": "Wait for build" }]),
            json!([]),
        ]
    );
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
        let events = run_events(&c, plan(&root, prompt)).await;
        assert!(!events.iter().any(|e| e["type"] == "turn.waiting"));
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
    deliver(
        &root,
        &json!([{ "id": "steering", "text": "fixture:batch second prompt", "attachments": [] }]),
    );
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
    let lease = acquire(&s, FIRST_RUN, "sonnet").await.unwrap().unwrap();
    s.accounts.refresh(&s, &id).await.unwrap();
    let running = view(&s, &id).await;
    assert!(running["usage"]["error"].is_null());
    assert_eq!(running["activeRunIds"], json!([FIRST_RUN]));
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
                "five_hour": { "utilization": used, "resets_at": "2030-01-01T12:00:00Z" },
                "seven_day": { "utilization": 60 },
            },
        });
        std::fs::write(home.join("fixture-usage.json"), payload.to_string()).unwrap();
        reset_usage_backoff(&s, &id).await;
        tokio::time::sleep(Duration::from_millis(5)).await;
        s.accounts.refresh(&s, &id).await.unwrap();
        view(&s, &id).await["status"].clone()
    };
    read(90).await;
    s.accounts.exhausted(&s, &id, "sonnet").await.unwrap();
    assert_eq!(view(&s, &id).await["status"], "waiting");
    // Usage that has not come down yet keeps the account waiting.
    assert_eq!(read(100).await, "waiting");
    assert!(acquire(&s, FIRST_RUN, "sonnet").await.is_err());
    assert_eq!(read(0).await, "next");
    let lease = acquire(&s, FIRST_RUN, "sonnet").await.unwrap().unwrap();
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
        .set(
            claude::CATALOG,
            json!({
                "models": [
                    { "model": "opus" },
                    { "model": "default", "resolvedModel": "claude-opus-fixture[1m]" },
                    { "model": "sonnet", "resolvedModel": "claude-sonnet-fixture" },
                ],
            }),
            None,
        )
        .await
        .unwrap();
    let payload = json!({
        "rate_limits_available": true,
        "rate_limits": {
            "five_hour": { "utilization": 10 },
            "seven_day": { "utilization": 20 },
            "seven_day_opus": { "utilization": 100 },
        },
    });
    std::fs::write(home.join("fixture-usage.json"), payload.to_string()).unwrap();
    s.accounts.refresh(&s, &id).await.unwrap();
    for model in ["opus", "default"] {
        assert!(
            acquire(&s, FIRST_RUN, model)
                .await
                .unwrap_err()
                .message
                .contains("available usage"),
            "{model}"
        );
    }
    let lease = acquire(&s, FIRST_RUN, "sonnet").await.unwrap().unwrap();
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
            "five_hour": { "utilization": 0, "resets_at": null },
            "seven_day": { "utilization": 105, "resets_at": "invalid" },
            "seven_day_opus": { "utilization": null },
            "seven_day_sonnet": { "utilization": -1 },
            "model_scoped": [{
                "display_name": "Fable",
                "utilization": 42,
                "resets_at": "2030-01-07T12:00:00Z",
                "secret": "never-return",
            }],
            "unknown_secret": "never-return",
        },
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
        acquire(&s, FIRST_RUN, "")
            .await
            .unwrap_err()
            .message
            .contains("available usage")
    );

    for payload in [
        json!({ "rate_limits_available": false, "rate_limits": null }),
        json!({
            "rate_limits_available": true,
            "rate_limits": { "five_hour": { "utilization": "25" } },
        }),
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
                .update(&s, &id, &json!({ "maxConcurrentRuns": limit }))
                .await
                .is_err()
        );
    }
    let mut held = Vec::new();
    for run in [FIRST_RUN, SECOND_RUN] {
        held.push(acquire(&s, run, "sonnet").await.unwrap().unwrap());
    }
    let updated = s
        .accounts
        .update(&s, &id, &json!({ "maxConcurrentRuns": 1 }))
        .await
        .unwrap();
    assert_eq!(updated["maxConcurrentRuns"], 1);
    assert_eq!(updated["status"], "full");
    assert_eq!(updated["activeRunIds"].as_array().unwrap().len(), 2);
    assert!(
        acquire(&s, "33333333-3333-4333-8333-333333333333", "")
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
