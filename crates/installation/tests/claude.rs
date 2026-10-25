mod common;

use cairn_installation::{
    accounts::{self, KIND, Lease},
    claude, claude_process,
    config::{Config, now},
    error::Result,
    provider::Provider,
    service::Service,
};
use common::eventually;
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
        codex_bin: "/nonexistent-codex".into(),
        claude_bin: common::fixture("claude.mjs"),
        opencode_bin: "opencode".into(),
        opencode_go_usage_url: cairn_installation::accounts::opencode::USAGE_URL.into(),
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

/// Presentation must keep the default alias runnable while exposing its known identity,
/// even when the CLI cache or initialize metadata is incomplete.
#[tokio::test]
async fn default_model_labels_survive_missing_metadata_without_pinning() {
    let root = TempDir::new().unwrap();
    let s = Service::new(config(&root)).await.unwrap();
    let directory =
        accounts::claude::account_home(&s.config, "catalog-fixture").join("cache/model-catalog");
    std::fs::create_dir_all(&directory).unwrap();
    let cache = directory.join("fixture.json");
    let resolved = "claude-opus-fixture[1m]";
    for (cached_name, display_name, description, identity, expected) in [
        (
            Some("Opus Fixture"),
            Some("Default (recommended)"),
            Some("Opus description · Details"),
            Some(resolved),
            "Opus Fixture (1M context)",
        ),
        (
            None,
            Some("Default (recommended)"),
            Some("Opus description · Details"),
            Some(resolved),
            "Opus description",
        ),
        (
            Some("Opus Fixture"),
            None,
            None,
            Some(resolved),
            "Opus Fixture (1M context)",
        ),
        (None, None, None, Some(resolved), resolved),
        (None, Some(""), Some(""), Some(resolved), resolved),
        (
            None,
            Some("Default (recommended)"),
            None,
            Some(resolved),
            resolved,
        ),
        (
            None,
            Some("Known human name"),
            None,
            Some(resolved),
            "Known human name",
        ),
        (
            None,
            Some("Default (recommended)"),
            Some("Opus description · Details"),
            None,
            "Opus description",
        ),
        (
            None,
            Some("Default (recommended)"),
            None,
            None,
            "Default (recommended)",
        ),
    ] {
        if let Some(name) = cached_name {
            std::fs::write(
                &cache,
                json!({
                    "fetchedAt": now(),
                    "catalog": { "config": { "models": [{
                        "id": "claude-opus-fixture",
                        "name": name,
                    }] } },
                })
                .to_string(),
            )
            .unwrap();
        } else if cache.exists() {
            std::fs::remove_file(&cache).unwrap();
        }
        s.store
            .set(
                claude::CATALOG,
                json!({
                    "models": [{
                        "model": "default",
                        "resolvedModel": identity,
                        "displayName": display_name,
                        "description": description,
                        "isDefault": true,
                    }],
                    "source": "catalog-fixture",
                    "checkedAt": now(),
                }),
                None,
            )
            .await
            .unwrap();
        let catalog = claude::model_catalog(&s).await.unwrap();
        let row = &catalog["models"][0];
        assert_eq!(
            row["displayName"], expected,
            "cache={cached_name:?}, display={display_name:?}, description={description:?}"
        );
        assert_eq!(row["model"], "default");
        assert_eq!(row["isDefault"], true);
    }
}

/// Optional native qualification: the real Claude Code release answers the control
/// protocol Cairn's model discovery and usage use, and the authentication status
/// account refresh reads, with synthetic credentials and no inference.
/// Run with CLAUDE_NATIVE_BIN set via the credential-isolated backend launcher.
#[tokio::test]
#[ignore = "requires a native Claude Code binary"]
async fn native_cli_lists_its_model_catalog_without_inference() {
    let binary = std::env::var("CLAUDE_NATIVE_BIN").expect("set CLAUDE_NATIVE_BIN");
    let root = TempDir::new().unwrap();
    let s = Service::new(Config {
        claude_bin: binary,
        ..config(&root)
    })
    .await
    .unwrap();
    // Token rotation sends the release's own OAuth client to its token endpoint.
    let executable = std::fs::read(&s.config.claude_bin).unwrap();
    for shipped in [accounts::claude::CLIENT_ID, accounts::claude::TOKEN_URL] {
        let literal = regex::bytes::Regex::new(&regex::escape(shipped)).unwrap();
        assert!(
            literal.is_match(&executable),
            "{shipped} is not in the release"
        );
    }
    let id = connected(&s).await;
    let catalog = claude::model_catalog(&s).await.unwrap();
    assert_eq!(catalog["stale"], false, "{catalog}");
    let models = catalog["models"].as_array().unwrap();
    // Agents without a chosen model run the CLI's default alias.
    assert!(
        models
            .iter()
            .any(|model| model["model"] == "default" && model["isDefault"] == true),
        "{catalog}"
    );
    assert!(
        models.iter().all(|model| {
            !model["model"].as_str().unwrap().is_empty()
                && model["displayName"]
                    .as_str()
                    .is_some_and(|name| !name.is_empty())
        }),
        "{catalog}"
    );
    // Agents can save exactly the effort levels the catalog offers: a level the
    // release adds would be refused, and one it drops would still be passed to it.
    let offered = models
        .iter()
        .flat_map(|model| model["supportedReasoningEfforts"].as_array().unwrap())
        .map(|effort| effort["reasoningEffort"].as_str().unwrap())
        .collect::<BTreeSet<_>>();
    let saved = claude::EFFORTS
        .into_iter()
        .filter(|effort| !effort.is_empty());
    assert_eq!(offered, saved.collect(), "{catalog}");
    // Account usage uses the same protocol. Without a subscription, the CLI answers
    // that no limits are available, in the fields the adapter reads.
    let usage = claude::query_metadata(
        &s,
        &id,
        Some(json!({ "subtype": "get_usage", "skip_behaviors": true })),
    )
    .await
    .unwrap();
    assert!(
        usage.get("rate_limits_available").is_some() && usage.get("rate_limits").is_some(),
        "{usage}"
    );
    // Account refresh reads `claude auth status` JSON, then usage. A changed status
    // format would mark the account as failed.
    s.accounts.refresh(&s, &id).await.unwrap();
    let account = view(&s, &id).await;
    assert_eq!(account["state"], "ready", "{account}");
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

/// Optional native qualification: the real Claude Code release runs a turn through
/// the adapter against a local Messages API, then resumes that session, optionally
/// with another release (CLAUDE_NATIVE_RESUME_BIN), as after an update or a rollback.
/// Run with CLAUDE_NATIVE_BIN set via the credential-isolated backend launcher.
#[tokio::test]
#[ignore = "requires a native Claude Code binary"]
async fn native_cli_executes_and_resumes_a_session_against_a_local_model() {
    use axum::{Router, body::Bytes, routing::post};
    const REPLY: &str = "Native Claude Code local model passed.";
    let binary = std::env::var("CLAUDE_NATIVE_BIN").expect("set CLAUDE_NATIVE_BIN");
    let resume = std::env::var("CLAUDE_NATIVE_RESUME_BIN").unwrap_or_else(|_| binary.clone());
    let requests = Arc::new(std::sync::Mutex::new(Vec::<Value>::new()));
    let recorded = requests.clone();
    let router = Router::new()
        .route(
            "/v1/messages",
            post(move |body: Bytes| async move {
                let request: Value = serde_json::from_slice(&body).unwrap();
                recorded.lock().unwrap().push(request.clone());
                // A streamed Messages API reply, as Server-Sent Events named by their type.
                let body = [
                    json!({
                        "type": "message_start",
                        "message": {
                            "id": "msg_local",
                            "type": "message",
                            "role": "assistant",
                            "model": request["model"],
                            "content": [],
                            "stop_reason": null,
                            "usage": { "input_tokens": 10, "output_tokens": 0 },
                        },
                    }),
                    json!({
                        "type": "content_block_start",
                        "index": 0,
                        "content_block": { "type": "text", "text": "" },
                    }),
                    json!({
                        "type": "content_block_delta",
                        "index": 0,
                        "delta": { "type": "text_delta", "text": REPLY },
                    }),
                    json!({ "type": "content_block_stop", "index": 0 }),
                    json!({
                        "type": "message_delta",
                        "delta": { "stop_reason": "end_turn" },
                        "usage": { "output_tokens": 8 },
                    }),
                    json!({ "type": "message_stop" }),
                ]
                .iter()
                .map(|event| {
                    format!(
                        "event: {}\ndata: {event}\n\n",
                        event["type"].as_str().unwrap()
                    )
                })
                .collect::<String>();
                ([("content-type", "text/event-stream")], body)
            }),
        )
        .route(
            "/v1/messages/count_tokens",
            post(|| async { axum::Json(json!({ "input_tokens": 10 })) }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });

    let root = TempDir::new().unwrap();
    let mut c = setup(&root);
    // The adapter never forwards an API key or endpoint override to subscription runs.
    // Only this test's launcher points the CLI at the local model with a synthetic key.
    let launcher = |name: &str, binary: &str| {
        let path = root.path().join(name);
        std::fs::write(
            &path,
            format!(
                "#!/bin/sh\nexport ANTHROPIC_BASE_URL=http://{address} ANTHROPIC_API_KEY=sk-ant-local-test CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1\nexec '{binary}' \"$@\"\n"
            ),
        )
        .unwrap();
        std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();
        path.to_string_lossy().into_owned()
    };
    c.claude_bin = launcher("claude-first", &binary);
    let mut p = plan(
        &root,
        "Reply with the local model result. Do not call tools.",
    );
    let (result, events) =
        tokio::time::timeout(Duration::from_secs(120), run_to_end(&c, p.clone()))
            .await
            .expect("native execution timed out");
    result.unwrap();
    assert!(completes(&events), "{events:?}");
    assert_eq!(
        std::fs::read_to_string(root.path().join("result.md")).unwrap(),
        REPLY
    );
    let session = events
        .iter()
        .find(|event| event["type"] == "thread.started")
        .unwrap()["thread_id"]
        .as_str()
        .unwrap()
        .to_owned();

    c.claude_bin = launcher("claude-resume", &resume);
    let first_turn = requests.lock().unwrap().len();
    p["sessionId"] = session.clone().into();
    p["execution"]["messageId"] = "follow-up".into();
    p["execution"]["text"] = "Continue in the same session. Do not call tools.".into();
    p["output"] = root
        .path()
        .join("follow-up.md")
        .to_string_lossy()
        .into_owned()
        .into();
    let (result, events) = tokio::time::timeout(Duration::from_secs(120), run_to_end(&c, p))
        .await
        .expect("native resume timed out");
    server.abort();
    result.unwrap();
    assert!(completes(&events), "{events:?}");
    assert!(
        events
            .iter()
            .any(|event| event["type"] == "thread.started" && event["thread_id"] == session)
    );
    assert_eq!(
        std::fs::read_to_string(root.path().join("follow-up.md")).unwrap(),
        REPLY
    );
    // The resuming release sent the stored history rather than starting over.
    assert!(
        requests.lock().unwrap()[first_turn..]
            .iter()
            .any(|request| {
                let messages = request["messages"].to_string();
                messages.contains("Reply with the local model result")
                    && messages.contains(REPLY)
                    && messages.contains("Continue in the same session")
            }),
        "The follow-up request did not include the first turn"
    );
}

#[tokio::test]
async fn managed_native_credentials_use_the_transient_store_and_keep_user_state() {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    let root = TempDir::new().unwrap();
    let c = setup(&root);
    let home = c.home.join(".claude");
    std::fs::remove_file(home.join(".credentials.json")).unwrap();
    std::fs::write(home.join("CLAUDE.md"), "durable user instructions").unwrap();
    let credentials = root.path().join("transient-auth");
    std::fs::create_dir(&credentials).unwrap();
    let listener = tokio::net::UnixListener::bind(credentials.join("cairn-auth.sock")).unwrap();
    let broker = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut stream = BufReader::new(stream);
        let mut request = String::new();
        stream.read_line(&mut request).await.unwrap();
        let snapshot = json!({
            "claudeAiOauth": {
                "accessToken": "synthetic-native-access",
                "refreshToken": "manager-only-refresh",
                "expiresAt": now() + 3_600_000,
                "scopes": ["user:inference"]
            }
        });
        stream
            .get_mut()
            .write_all(format!("{snapshot}\n").as_bytes())
            .await
            .unwrap();
    });

    let mut p = plan(&root, "Inspect the workspace");
    p["claudeManagedAuth"] = true.into();
    p["claudeCredentialsDirectory"] = json!(credentials);
    let (result, events) = run_to_end(&c, p).await;
    result.unwrap();
    broker.await.unwrap();

    assert!(completes(&events));
    assert!(home.join("invocations.jsonl").exists());
    assert_eq!(
        std::fs::read_to_string(home.join("CLAUDE.md")).unwrap(),
        "durable user instructions"
    );
    assert!(!home.join(".credentials.json").exists());
    let snapshot: Value =
        serde_json::from_slice(&std::fs::read(credentials.join(".credentials.json")).unwrap())
            .unwrap();
    assert_eq!(
        snapshot["claudeAiOauth"]["accessToken"],
        "synthetic-native-access"
    );
    assert!(snapshot["claudeAiOauth"]["refreshToken"].is_null());
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
async fn oversized_inbox_fails_before_submitting_messages() {
    let root = TempDir::new().unwrap();
    let c = setup(&root);
    std::fs::write(
        root.path().join("inbox/messages.json"),
        vec![b' '; 2_000_001],
    )
    .unwrap();
    let (result, events) = tokio::time::timeout(
        Duration::from_secs(10),
        run_to_end(&c, plan(&root, "fixture:question")),
    )
    .await
    .unwrap();
    let error = result.unwrap_err();
    assert_eq!(error.status, 400);
    assert_eq!(error.message, "Chat inbox is too large.");
    assert!(!completes(&events));
}

#[tokio::test]
async fn invalid_and_exact_limit_empty_inboxes_do_not_interrupt_a_turn() {
    for bytes in [
        b"invalid json".to_vec(),
        format!("[]{}", " ".repeat(1_999_998)).into_bytes(),
    ] {
        let root = TempDir::new().unwrap();
        let c = setup(&root);
        std::fs::write(root.path().join("inbox/messages.json"), bytes).unwrap();
        let (result, events) = run_to_end(&c, plan(&root, "fixture:slow")).await;
        result.unwrap();
        assert!(completes(&events));
    }
}

#[tokio::test]
async fn fresh_and_resumed_sessions_disable_native_background_tasks() {
    let root = TempDir::new().unwrap();
    let c = setup(&root);
    let mut p = plan(&root, "Inspect the workspace");
    for index in 0..2 {
        if index > 0 {
            p["sessionId"] = RESUMED_SESSION.into();
            p["execution"]["messageId"] = "continued".into();
        }

        let (result, events) = run_to_end(&c, p.clone()).await;
        result.unwrap();
        assert!(completes(&events));
        let invocations =
            std::fs::read_to_string(c.home.join(".claude/invocations.jsonl")).unwrap();
        let invocation: Value =
            serde_json::from_str(invocations.lines().nth(index).unwrap()).unwrap();
        assert_eq!(invocation["backgroundTasksDisabled"], "1");
    }
}

#[tokio::test]
async fn large_system_instructions_reach_claude_unchanged() {
    let root = TempDir::new().unwrap();
    let c = setup(&root);
    let mut p = plan(&root, "Inspect the workspace");
    let instructions = format!("{}\nKeep accents: é and 漢字.\n", "x".repeat(135_518));
    for (index, instructions) in [instructions, "é".repeat(140_000)].into_iter().enumerate() {
        p["instructions"] = instructions.clone().into();
        if index > 0 {
            p["sessionId"] = RESUMED_SESSION.into();
            p["execution"]["messageId"] = "continued".into();
        }

        let (result, events) = run_to_end(&c, p.clone()).await;
        result.unwrap();
        assert!(completes(&events));
        let invocations =
            std::fs::read_to_string(c.home.join(".claude/invocations.jsonl")).unwrap();
        let invocation: Value =
            serde_json::from_str(invocations.lines().nth(index).unwrap()).unwrap();
        assert_eq!(invocation["instructions"], instructions);
        assert_eq!(invocation["instructionsMode"], 0o600);
        let args = invocation["args"].as_array().unwrap();
        let file = args
            .windows(2)
            .find(|pair| pair[0] == "--append-system-prompt-file")
            .unwrap();
        assert!(!std::path::Path::new(file[1].as_str().unwrap()).exists());
    }
}

#[tokio::test]
async fn launch_errors_report_the_os_failure_without_exposing_instructions() {
    let root = TempDir::new().unwrap();
    let mut c = setup(&root);
    c.claude_bin = root
        .path()
        .join("missing-claude")
        .to_string_lossy()
        .into_owned();
    let mut p = plan(&root, "Inspect the workspace");
    p["instructions"] = "never-return-this-secret".into();

    let error = run_to_end(&c, p).await.0.unwrap_err();
    assert!(error.message.contains("Unable to start Claude Code"));
    assert!(error.message.contains("os error 2"));
    assert!(!error.message.contains("never-return-this-secret"));
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

    // Repair is explicit: Cairn must not guess what a corrupted shell script meant.
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
async fn todo_writes_show_as_the_agent_plan() {
    let root = TempDir::new().unwrap();
    let c = setup(&root);
    let (result, events) = run_to_end(&c, plan(&root, "fixture:plan")).await;
    result.unwrap();

    let plans: Vec<_> = events
        .iter()
        .filter(|e| e["item"]["type"] == "todo_list")
        .map(|e| {
            (
                e["type"].as_str().unwrap(),
                e["item"]["id"].as_str().unwrap(),
            )
        })
        .collect();
    // Each list shows once Claude Code accepts it.
    assert_eq!(
        plans,
        vec![("item.completed", "todo-1"), ("item.completed", "todo-2")]
    );
    let last = events
        .iter()
        .rfind(|e| e["item"]["type"] == "todo_list")
        .unwrap();
    assert_eq!(last["item"]["status"], "completed");
    assert_eq!(last["item"]["items"][1]["status"], "in_progress");
    assert_eq!(
        last["item"]["items"][1]["active_text"],
        "Adding the migration"
    );
    // The tool result only acknowledges the list.
    assert!(last["item"].get("result").is_none());
    assert!(!events.iter().any(|e| e["item"]["tool"] == "TodoWrite"));
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
async fn withdrawn_questions_deny_the_native_request_without_an_answer() {
    let root = TempDir::new().unwrap();
    let c = setup(&root);
    let p = plan(&root, "fixture:question");
    let (tx, mut rx) = mpsc::channel(64);
    let task =
        tokio::spawn(async move { claude_process::run(&c, p, tx, CancellationToken::new()).await });
    let mut completed = false;
    tokio::time::timeout(Duration::from_secs(8), async {
        while let Some(event) = rx.recv().await {
            if event["type"] == "chat.question" {
                deliver(&root, &json!([{
                    "id": "withdrawal", "questionId": event["question"]["id"], "questionCancelled": true,
                }]));
            }
            completed |= event["type"] == "turn.completed";
        }
    }).await.unwrap();
    task.await.unwrap().unwrap();
    assert!(completed);
    let response: Value = serde_json::from_slice(
        &std::fs::read(root.path().join("home/.claude/question-response.json")).unwrap(),
    )
    .unwrap();
    let decision = &response["response"]["response"];
    assert_eq!(decision["behavior"], "deny");
    assert!(decision["updatedInput"].is_null());
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
    assert_eq!(Provider::of_agent(&json!({})).unwrap(), Provider::Codex);
    assert!(
        claude::validate_agent(&json!({ "provider": "claude", "reasoning": "ultra" })).is_err()
    );
    let root = TempDir::new().unwrap();
    let mut p = plan(&root, "test");
    p["sandbox"] = "workspace-write".into();
    let a = claude_process::args(&p, &root.path().join("instructions.txt"));
    assert!(!a.contains(&"--dangerously-skip-permissions".into()));
    assert!(a.iter().any(|s| s.contains("failIfUnavailable")));
    assert!(a.contains(&"--strict-mcp-config".into()));
}

#[tokio::test]
async fn disk_notice_reaches_engine_without_user_receipts_including_replay() {
    let root = TempDir::new().unwrap();
    let c = setup(&root);
    let input = plan(&root, "fixture:slow");
    std::fs::write(
        root.path().join("inbox/disk-notice.json"),
        json!({ "id": "disk-notice", "operationalNotice": true,
            "text": "fixture:slow Runtime disk pressure" })
        .to_string(),
    )
    .unwrap();
    let (result, events) = run_to_end(&c, input.clone()).await;
    result.unwrap();
    assert!(completes(&events));
    let inputs = std::fs::read_to_string(c.home.join(".claude/user-messages.jsonl")).unwrap();
    assert!(inputs.contains("Runtime disk pressure"));
    for round in [events, run_to_end(&c, input).await.1] {
        assert!(
            !round
                .iter()
                .any(|event| event["type"] == "chat.delivered"
                    && event["messageId"] == "disk-notice")
        );
        assert!(
            round
                .iter()
                .any(|event| event["type"] == "chat.delivered" && event["messageId"] == "original")
        );
    }
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
async fn stopped_background_tasks_do_not_wait_for_a_notification_that_never_arrives() {
    for prompt in [
        "fixture:stopped-background",
        "fixture:stopped-background snapshot-first",
        "fixture:stopped-background legacy-id",
    ] {
        let root = TempDir::new().unwrap();
        let c = setup(&root);
        let events = run_events(&c, plan(&root, prompt)).await;
        assert!(events.iter().any(|event| event["type"] == "turn.waiting"));
        assert_eq!(
            std::fs::read_to_string(root.path().join("result.md")).unwrap(),
            "Stopped all background work"
        );
        let receipt: Value = serde_json::from_slice(
            &std::fs::read(root.path().join("result.claude-receipt.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(receipt["delivered"], json!(["original"]));
        assert_eq!(receipt["text"], "Stopped all background work");
    }
}

#[tokio::test]
async fn failed_stops_and_unrelated_results_still_wait_for_background_followups() {
    for prompt in [
        "fixture:stopped-background failed-stop",
        "fixture:stopped-background unrelated-tool",
    ] {
        let root = TempDir::new().unwrap();
        let c = setup(&root);
        run_events(&c, plan(&root, prompt)).await;
        assert_eq!(
            std::fs::read_to_string(root.path().join("result.md")).unwrap(),
            "Background followup handled"
        );
    }
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
async fn subagents_report_their_own_steps_without_replacing_the_reply() {
    let root = TempDir::new().unwrap();
    let c = setup(&root);
    let events = run_events(&c, plan(&root, "fixture:subagents")).await;
    let subagent = |id: &str| {
        events
            .iter()
            .filter(|e| e["item"]["type"] == "subagent" && e["item"]["id"] == id)
            .map(|e| (e["type"].as_str().unwrap(), e["item"].clone()))
            .collect::<Vec<_>>()
    };

    let count = subagent("agent-count");
    assert_eq!(count[0].0, "item.started", "{count:#?}");
    assert_eq!(count[0].1["name"], "Count lines");
    assert_eq!(count[0].1["agent_type"], "general-purpose");
    assert_eq!(count[0].1["prompt"], "Count lines in the workspace.");
    assert!(
        count
            .iter()
            .any(|(_, item)| item["activity"] == "Running wc -l")
    );
    let (kind, last) = count.last().unwrap();
    assert_eq!(*kind, "item.completed");
    assert_eq!(last["status"], "completed");
    assert_eq!(last["task_id"], "task-agent-count");
    // The report drops Claude Code's hand-back framing and keeps the final usage.
    assert_eq!(last["result"], "Total: 3 lines.");
    assert_eq!(last["tokens"], 1510);
    assert_eq!(last["duration_ms"], 1150);

    // A background subagent outlives its tool call and settles with its notification.
    let list = subagent("agent-list");
    let launched = list
        .iter()
        .find(|(kind, _)| *kind == "item.completed")
        .unwrap();
    assert_eq!(launched.1["status"], "in_progress");
    assert_eq!(launched.1["background"], true);
    let (kind, last) = list.last().unwrap();
    assert_eq!(*kind, "item.updated");
    assert_eq!(last["status"], "completed");
    assert_eq!(last["result"], "Files: one.txt, two.txt");

    let children: Vec<_> = events
        .iter()
        .filter(|e| e["item"]["parent_id"].is_string())
        .map(|e| {
            (
                e["item"]["parent_id"].as_str().unwrap(),
                e["item"]["type"].as_str().unwrap(),
            )
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    assert_eq!(
        children,
        vec![
            ("agent-count", "command_execution"),
            ("agent-count", "subagent_message"),
            ("agent-list", "command_execution"),
            ("agent-list", "subagent_message"),
        ]
    );
    let replies: Vec<_> = events
        .iter()
        .filter(|e| e["item"]["type"] == "agent_message" && e["type"] == "item.completed")
        .map(|e| e["item"]["text"].as_str().unwrap())
        .collect();
    assert_eq!(
        replies,
        vec![
            "Counted while the listing continues",
            "Both subagents reported back"
        ]
    );
    assert_eq!(
        std::fs::read_to_string(root.path().join("result.md")).unwrap(),
        "Both subagents reported back"
    );
}

#[tokio::test]
async fn a_subagent_tool_result_preserves_its_stopped_notification() {
    let root = TempDir::new().unwrap();
    let c = setup(&root);
    let events = run_events(&c, plan(&root, "fixture:subagents-stopped")).await;
    let last = events
        .iter()
        .rfind(|event| event["item"]["id"] == "agent-count")
        .unwrap();
    assert_eq!(last["item"]["status"], "stopped");
    assert_eq!(last["item"]["result"], "Total: 3 lines.");
}

#[tokio::test]
async fn task_updates_during_a_streamed_response_do_not_announce_an_idle_agent() {
    let root = TempDir::new().unwrap();
    let c = setup(&root);
    let events = run_events(&c, plan(&root, "fixture:background-stream")).await;
    let waits: Vec<_> = events
        .iter()
        .filter(|event| event["type"] == "turn.waiting")
        .map(|event| event["tasks"].clone())
        .collect();
    assert_eq!(
        waits,
        vec![json!([{ "id": "build", "description": "Wait for build" }])]
    );
    assert!(events.iter().any(|event| {
        event["type"] == "item.updated" && event["item"]["text"] == "Checking progress"
    }));
}

#[tokio::test]
async fn engine_retries_limits_and_notices_are_reported_without_ending_the_turn() {
    let root = TempDir::new().unwrap();
    let c = setup(&root);
    let started = now();
    let events = run_events(&c, plan(&root, "fixture:engine-retry")).await;
    let of = |kind: &str| {
        events
            .iter()
            .filter(|event| event["type"] == kind)
            .collect::<Vec<_>>()
    };

    // A retry the engine makes itself is not a failure: the turn completes with its reply.
    let retries = of("engine.retry");
    assert_eq!(retries.len(), 2, "{events:#?}");
    assert_eq!(retries[0]["attempt"], 1);
    assert_eq!(retries[0]["maxRetries"], 10);
    assert_eq!(retries[0]["delayMs"], 600);
    assert_eq!(retries[0]["status"], 529);
    assert_eq!(retries[0]["reason"], "overloaded");
    assert!(retries[0]["retryAt"].as_i64().unwrap() >= started + 600);
    assert!(retries[0].get("noResponse").is_none());
    assert!(retries[1]["status"].is_null());
    assert_eq!(
        retries[1]["noResponse"],
        json!({ "waitedMs": 30000, "retryWaitMs": 60000 })
    );
    assert!(of("turn.failed").is_empty());
    assert_eq!(
        std::fs::read_to_string(root.path().join("result.md")).unwrap(),
        "Answered after the retries"
    );

    // Only changes of the usage limit are shown: the warning once, then its end.
    let limits: Vec<_> = of("provider.limit")
        .into_iter()
        .map(|event| (event["status"].clone(), event["resetsAt"].clone()))
        .collect();
    assert_eq!(
        limits,
        vec![
            (json!("allowed_warning"), json!(1_893_499_200_000_i64)),
            (json!("allowed"), json!(1_893_517_200_000_i64)),
        ]
    );
    assert_eq!(of("provider.limit")[0]["window"], "five_hour");
    assert_eq!(of("provider.limit")[0]["blocked"], false);

    // Blank notices and frames this adapter does not know are ignored.
    let notices = of("engine.notice");
    assert_eq!(notices.len(), 1);
    assert_eq!(notices[0]["level"], "warning");
    assert_eq!(notices[0]["message"], "Context is almost full.");
    let known = [
        "thread.started",
        "chat.delivered",
        "engine.retry",
        "provider.limit",
        "engine.notice",
        "item.started",
        "item.updated",
        "item.completed",
        "chat.question.closed",
        "turn.completed",
    ];
    for event in &events {
        assert!(known.contains(&kind(event)), "{event}");
    }
}

fn kind(event: &Value) -> &str {
    event["type"].as_str().unwrap_or_default()
}

#[tokio::test]
async fn a_usage_limit_that_stops_requests_fails_the_turn_with_its_reset() {
    let root = TempDir::new().unwrap();
    let c = setup(&root);
    let (result, events) = run_to_end(&c, plan(&root, "fixture:usage-limit")).await;
    assert!(result.is_err());
    assert!(!completes(&events));
    let limit = events
        .iter()
        .find(|event| event["type"] == "provider.limit")
        .unwrap();
    assert_eq!(limit["status"], "rejected");
    assert_eq!(limit["blocked"], true);

    // The engine said why; its wording does not need to be recognized.
    let failure = events
        .iter()
        .find(|event| event["type"] == "turn.failed")
        .unwrap();
    assert_eq!(
        failure["error"],
        json!({
            "message": "Your usage allowance is used up for now.",
            "code": "usage_limit_reached",
            "window": "five_hour",
            "resetsAt": 1_893_499_200_000_i64,
        })
    );
    assert!(cairn_installation::run_output::exhausted(failure));
    assert!(!root.path().join("result.claude-receipt.json").exists());

    // A limit that no longer stops requests does not explain a later failure: Claude Code
    // answered after it (on another model), or its reset time passed.
    for scenario in [
        "fixture:usage-limit answered",
        "fixture:usage-limit reset-passed",
    ] {
        let root = TempDir::new().unwrap();
        let c = setup(&root);
        let (result, events) = run_to_end(&c, plan(&root, scenario)).await;
        assert!(result.is_err(), "{scenario}");
        assert!(
            events
                .iter()
                .filter(|event| event["type"] == "turn.failed")
                .all(|failure| failure["error"]["code"] != "usage_limit_reached"
                    && !cairn_installation::run_output::exhausted(failure)),
            "{scenario}: {events:?}"
        );
    }

    // Extra usage takes over: the limit is shown, and the turn goes on.
    let root = TempDir::new().unwrap();
    let c = setup(&root);
    let events = run_events(&c, plan(&root, "fixture:usage-limit extra-usage")).await;
    let limit = events
        .iter()
        .find(|event| event["type"] == "provider.limit")
        .unwrap();
    assert_eq!(limit["blocked"], false);
    assert_eq!(limit["usingExtraUsage"], true);
    assert_eq!(
        std::fs::read_to_string(root.path().join("result.md")).unwrap(),
        "Finished on extra usage"
    );
}

#[tokio::test]
async fn nested_and_resumed_subagents_keep_their_identity_and_the_parent_reply() {
    let root = TempDir::new().unwrap();
    let c = setup(&root);
    let events = run_events(&c, plan(&root, "fixture:subagents-resumed")).await;
    let updates = |id: &str| {
        events
            .iter()
            .filter(|event| event["item"]["type"] == "subagent" && event["item"]["id"] == id)
            .map(|event| event["item"].clone())
            .collect::<Vec<_>>()
    };

    let nested = updates("agent-nested");
    let nested = nested.last().unwrap();
    assert_eq!(nested["parent_id"], "agent-review");
    assert_eq!(nested["parent_task_id"], "task-review");
    assert_eq!(nested["status"], "completed");

    // The first run reports back, then the resumed run reopens the same subagent.
    let review = updates("agent-review");
    let first = review
        .iter()
        .find(|item| item["status"] == "completed")
        .unwrap();
    assert_eq!(first["run_id"], "run-1");
    assert_eq!(first["result"], "Looks good.");
    assert!(first.get("resumed").is_none());
    let reopened = review.iter().find(|item| item["resumed"] == true).unwrap();
    assert_eq!(reopened["status"], "in_progress");
    assert_eq!(reopened["run_id"], "run-2");
    let last = review.last().unwrap();
    assert_eq!(last["status"], "completed");
    assert_eq!(last["task_id"], "task-review");
    assert_eq!(last["run_id"], "run-2");
    assert_eq!(last["result"], "Docs are fine too.");
    assert_eq!(last["tokens"], 700);
    // A late frame of the first run changes nothing.
    assert!(
        review
            .iter()
            .all(|item| item["activity"] != "Late progress of the first run")
    );

    // The resumed subagent's message is one of its steps, never the agent's reply.
    let steps: Vec<_> = events
        .iter()
        .filter(|event| event["item"]["type"] == "subagent_message")
        .map(|event| {
            (
                event["item"]["parent_id"].as_str().unwrap(),
                event["item"]["text"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        steps,
        vec![
            ("agent-nested", "Tests pass."),
            ("agent-review", "Looks good."),
            ("agent-review", "Docs are fine too."),
        ]
    );
    let replies: Vec<_> = events
        .iter()
        .filter(|event| {
            event["item"]["type"] == "agent_message" && event["type"] == "item.completed"
        })
        .map(|event| event["item"]["text"].as_str().unwrap())
        .collect();
    assert_eq!(replies, vec!["Review resumed and finished"]);
    assert_eq!(
        std::fs::read_to_string(root.path().join("result.md")).unwrap(),
        "Review resumed and finished"
    );
}

#[tokio::test]
async fn a_subagent_resumed_by_a_later_execution_shows_as_resumed() {
    let root = TempDir::new().unwrap();
    let c = setup(&root);
    let events = run_events(&c, plan(&root, "fixture:resumed-later")).await;
    let subagents: Vec<_> = events
        .iter()
        .filter(|event| event["item"]["type"] == "subagent")
        .collect();
    // Only the subagent the agent resumed: an agent its workflow starts, or a housekeeping agent
    // Claude Code starts itself, is not one.
    assert!(
        subagents
            .iter()
            .all(|event| event["item"]["id"] == "task-old")
    );
    let started = subagents.first().unwrap();
    assert_eq!(started["type"], "item.started");
    assert_eq!(started["item"]["resumed"], true);
    assert_eq!(started["item"]["task_id"], "task-old");
    assert_eq!(started["item"]["run_id"], "run-3");
    assert_eq!(started["item"]["name"], "Review the change");
    let last = &subagents.last().unwrap()["item"];
    assert_eq!(last["status"], "completed");
    assert_eq!(last["result"], "Picked up again.");

    // Its message is one of its steps, never the agent's reply.
    let step = events
        .iter()
        .find(|event| event["item"]["type"] == "subagent_message")
        .unwrap();
    assert_eq!(step["item"]["parent_id"], "task-old");
    assert_eq!(
        std::fs::read_to_string(root.path().join("result.md")).unwrap(),
        "Resumed in a later turn"
    );

    // Claude Code repeats a running tool's progress notice; it is reported once.
    let notices: Vec<_> = events
        .iter()
        .filter(|event| event["type"] == "engine.notice")
        .collect();
    assert_eq!(notices.len(), 1);
    assert_eq!(notices[0]["message"], "Tool SendMessage running for 5s…");
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
async fn auto_keeps_saved_models_the_catalog_no_longer_lists_within_their_family_windows() {
    let root = TempDir::new().unwrap();
    let s = Service::new(config(&root)).await.unwrap();
    // A newer Claude Code catalog lists "opus" but no longer "opus[1m]", which agents
    // saved under an earlier release still run.
    s.store
        .set(
            claude::CATALOG,
            json!({
                "models": [
                    { "model": "opus" },
                    { "model": "sonnet", "resolvedModel": "claude-sonnet-fixture" },
                ],
            }),
            None,
        )
        .await
        .unwrap();
    let read = async |id: &str, opus: u32| {
        let home = accounts::claude::account_home(&s.config, id);
        let payload = json!({
            "rate_limits_available": true,
            "rate_limits": {
                "five_hour": { "utilization": 10 },
                "seven_day": { "utilization": 20 },
                "seven_day_opus": { "utilization": opus },
            },
        });
        std::fs::write(home.join("fixture-usage.json"), payload.to_string()).unwrap();
        s.accounts.refresh(&s, id).await.unwrap();
    };
    let spent = connected(&s).await;
    read(&spent, 100).await;
    for model in ["opus[1m]", "claude-opus-4-6[1m]"] {
        let error = acquire(&s, FIRST_RUN, model).await.unwrap_err();
        assert!(error.message.contains("available usage"), "{model}");
    }
    // Other families keep the account's capacity.
    for model in ["sonnet[1m]", "claude-haiku-4-5"] {
        let lease = acquire(&s, FIRST_RUN, model).await.unwrap().unwrap();
        assert_eq!(lease.account_id, spent, "{model}");
        s.accounts.release(&lease).await.unwrap();
    }

    // Auto picks the account with Opus capacity left; other families still prefer the
    // account with the most capacity.
    let open = connected(&s).await;
    read(&open, 50).await;
    let mut account = s.accounts.get(&s, &open).await.unwrap();
    account["usage"]["windows"][0]["usedPercent"] = 80.into();
    s.store.put(KIND, account).await.unwrap();
    let lease = acquire(&s, FIRST_RUN, "opus[1m]").await.unwrap().unwrap();
    assert_eq!(lease.account_id, open);
    s.accounts.release(&lease).await.unwrap();
    let lease = acquire(&s, FIRST_RUN, "sonnet").await.unwrap().unwrap();
    assert_eq!(lease.account_id, spent);
    s.accounts.release(&lease).await.unwrap();

    // An exhaustion recorded for the old alias clears once Opus capacity returns.
    s.accounts.exhausted(&s, &open, "opus[1m]").await.unwrap();
    reset_usage_backoff(&s, &open).await;
    tokio::time::sleep(Duration::from_millis(5)).await;
    read(&open, 100).await;
    assert_eq!(view(&s, &open).await["status"], "waiting");
    reset_usage_backoff(&s, &open).await;
    tokio::time::sleep(Duration::from_millis(5)).await;
    read(&open, 40).await;
    assert_eq!(view(&s, &open).await["status"], "next");
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
