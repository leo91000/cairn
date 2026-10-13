mod common;

use cairn_installation::{
    accounts::{KIND, broker},
    config::Config,
    opencode,
    provider::Provider,
    service::Service,
    validation::text,
};
use serde_json::{Value, json};
use std::time::Duration;
use tempfile::TempDir;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

const KEY: &str = "synthetic-opencode-key-never-a-real-credential";
const MODEL: &str = "opencode/ling-3.1-flash-free";

async fn service(root: &TempDir) -> std::sync::Arc<Service> {
    Service::new(Config {
        opencode_bin: common::fixture("opencode.mjs"),
        ..common::config(root.path())
    })
    .await
    .unwrap()
}

async fn connect(s: &Service) -> Value {
    s.accounts
        .connect_api_key(s, Provider::Opencode, "Zen", "opencode", KEY, None)
        .await
        .unwrap()
}

#[tokio::test]
async fn api_keys_are_private_provider_bound_and_fenced_against_live_replacement() {
    let root = TempDir::new().unwrap();
    let s = service(&root).await;
    let account = connect(&s).await;
    let id = text(&account, "id");
    assert_eq!(account["modelProviderName"], "OpenCode Zen");
    assert!(!account.to_string().contains(KEY));
    assert!(
        !serde_json::to_string(&s.store.list(KIND).await.unwrap())
            .unwrap()
            .contains(KEY)
    );
    assert!(
        s.accounts
            .connect_api_key(&s, Provider::Opencode, "Duplicate", "opencode", KEY, None)
            .await
            .is_err()
    );
    assert!(
        s.accounts
            .connect_api_key(
                &s,
                Provider::Opencode,
                "Zen",
                "openai",
                "other-key",
                Some(id)
            )
            .await
            .is_err()
    );

    let models = s.opencode.models(&s).await.unwrap();
    assert!(models.to_string().contains(MODEL));
    assert!(!models.to_string().contains("openai/fixture-model"));
    let lease = s
        .accounts
        .acquire(&s, "key-fence", Provider::Opencode, MODEL)
        .await
        .unwrap()
        .unwrap();
    let broker = broker::serve(&s, &lease).await.unwrap();
    let credential = broker::request(
        &lease.home.join(broker::SOCKET),
        &broker::Request::default(),
        Duration::from_secs(2),
    )
    .await
    .unwrap();
    assert_eq!(credential["key"], KEY);
    assert!(
        s.accounts
            .connect_api_key(
                &s,
                Provider::Opencode,
                "Zen",
                "opencode",
                "replacement-key",
                Some(id)
            )
            .await
            .is_err()
    );
    drop(broker);
    s.accounts.release(&lease).await.unwrap();
    s.accounts
        .connect_api_key(
            &s,
            Provider::Opencode,
            "Zen",
            "opencode",
            "replacement-key",
            Some(id),
        )
        .await
        .unwrap();
    s.accounts.remove(&s, id).await.unwrap();
    assert!(
        s.vault
            .get(&format!("opencode-account:{id}"))
            .await
            .unwrap()
            .is_none()
    );
}

fn plan(root: &TempDir, body: &str) -> Value {
    json!({
        "sandbox": "yolo",
        "model": MODEL,
        "cwd": root.path(),
        "inputDirectory": root.path().join("inbox"),
        "output": root.path().join("result.md"),
        "instructions": "Be concise.",
        "reasoning": "high",
        "execution": { "messageId": "initial", "text": body, "attachments": [] },
    })
}

async fn run(s: &Service, plan: Value) -> (cairn_installation::error::Result<()>, Vec<Value>) {
    let lease = s
        .accounts
        .acquire(s, "protocol-test", Provider::Opencode, MODEL)
        .await
        .unwrap()
        .unwrap();
    let broker = broker::serve(s, &lease).await.unwrap();
    let mut config = s.config.clone();
    config.home = lease.home.parent().unwrap().to_owned();
    let (tx, mut rx) = mpsc::channel(128);
    let output = tokio::spawn(async move {
        let mut values = Vec::new();
        while let Some(v) = rx.recv().await {
            values.push(v);
        }
        values
    });
    let result = tokio::time::timeout(
        Duration::from_secs(10),
        opencode::run(&config, plan, tx, CancellationToken::new()),
    )
    .await
    .expect("native turn must finish or fail without hanging");
    drop(broker);
    s.accounts.release(&lease).await.unwrap();
    (result, output.await.unwrap())
}

#[tokio::test]
async fn native_turn_streams_and_completed_receipt_replays_without_an_engine() {
    let root = TempDir::new().unwrap();
    let s = service(&root).await;
    connect(&s).await;
    let input = plan(&root, "Build the status panel");
    let (result, events) = run(&s, input.clone()).await;
    result.unwrap();
    assert!(
        events
            .iter()
            .any(|e| e["type"] == "chat.delivered" && e["messageId"] == "initial")
    );
    assert!(
        events
            .iter()
            .any(|e| e["type"] == "item.completed" && e["item"]["type"] == "command_execution")
    );
    assert!(events.iter().any(|e| e["type"] == "turn.completed"));
    let answer = tokio::fs::read_to_string(root.path().join("result.md"))
        .await
        .unwrap();
    assert!(answer.contains("compact status panel"));
    assert!(!answer.contains(KEY));
    assert!(
        !events.iter().any(|e| e["item"]["type"] == "agent_message"
            && e["item"]["text"] == "Build the status panel")
    );
    let receipt = tokio::fs::read_to_string(root.path().join("result.opencode-receipt.json"))
        .await
        .unwrap();
    assert!(!receipt.contains(KEY));
    let (tx, mut rx) = mpsc::channel(128);
    let config = Config {
        opencode_bin: "/nonexistent-opencode".into(),
        ..s.config.clone()
    };
    opencode::run(&config, input, tx, CancellationToken::new())
        .await
        .unwrap();
    let mut completed = false;
    while let Some(event) = rx.recv().await {
        completed |= event["type"] == "turn.completed";
    }
    assert!(completed);
}

#[tokio::test]
async fn rejected_credentials_are_masked_and_truncated_responses_fail_without_hanging() {
    for (input, expected) in [
        ("fixture:opencode-error", "authentication failed"),
        ("fixture:opencode-length", "stopped before completing"),
        ("fixture:opencode-continuation", "unsupported continuation"),
    ] {
        let root = TempDir::new().unwrap();
        let s = service(&root).await;
        connect(&s).await;
        let (result, events) = run(&s, plan(&root, input)).await;
        let error = result.unwrap_err().to_string();
        assert!(error.contains(expected), "{error}");
        assert!(!error.contains("Sensitive provider detail"));
        assert!(!serde_json::to_string(&events).unwrap().contains(KEY));
        assert!(
            !serde_json::to_string(&events)
                .unwrap()
                .contains("Continue if you have next steps")
        );
    }
}

#[tokio::test]
async fn cancellation_stops_a_busy_native_turn() {
    let root = TempDir::new().unwrap();
    let s = service(&root).await;
    connect(&s).await;
    let lease = s
        .accounts
        .acquire(&s, "cancel-test", Provider::Opencode, MODEL)
        .await
        .unwrap()
        .unwrap();
    let _broker = broker::serve(&s, &lease).await.unwrap();
    let mut config = s.config.clone();
    config.home = lease.home.parent().unwrap().to_owned();
    let cancel = CancellationToken::new();
    let (tx, mut rx) = mpsc::channel(128);
    let input = plan(&root, "fixture:opencode-hang");
    let stopping = cancel.clone();
    let task = tokio::spawn(async move { opencode::run(&config, input, tx, stopping).await });
    tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(event) = rx.recv().await {
            if event["type"] == "turn.started" {
                break;
            }
        }
    })
    .await
    .unwrap();
    cancel.cancel();
    assert!(
        tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap()
            .is_err()
    );
}

/// Optional pinned-native smoke: real OpenCode, a local model server, synthetic auth.
/// Run with OPENCODE_NATIVE_BIN set via the credential-isolated backend launcher.
#[tokio::test]
#[ignore = "requires the pinned OpenCode 1.19.0 binary"]
async fn pinned_native_cli_executes_against_a_local_model_without_global_plugins() {
    use axum::{Router, http::HeaderMap, routing::post};
    let binary = std::env::var("OPENCODE_NATIVE_BIN").expect("set OPENCODE_NATIVE_BIN");
    let root = TempDir::new().unwrap();
    let s = service(&root).await;
    let account = s
        .accounts
        .connect_api_key(&s, Provider::Opencode, "Local model", "openai", KEY, None)
        .await
        .unwrap();
    // Keep the public API restricted to built-in providers. This test's custom
    // local provider avoids the native OpenAI-specific Responses loader.
    let id = text(&account, "id");
    let mut stored = s.store.get(KIND, id).await.unwrap().unwrap();
    stored["modelProvider"] = "fixture".into();
    s.store.put(KIND, stored).await.unwrap();
    s.vault
        .set(
            &format!("opencode-account:{id}"),
            &json!({ "modelProvider": "fixture", "key": KEY }),
        )
        .await
        .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let router = Router::new().route("/v1/chat/completions", post(|headers: HeaderMap| async move {
        assert_eq!(headers["authorization"], format!("Bearer {KEY}"));
        let chunks = [
            json!({
                "id": "local",
                "object": "chat.completion.chunk",
                "created": 1,
                "model": "fixture-model",
                "choices": [{ "index": 0, "delta": { "role": "assistant", "content": "Native OpenCode local model passed." }, "finish_reason": null }],
            }),
            json!({
                "id": "local",
                "object": "chat.completion.chunk",
                "created": 1,
                "model": "fixture-model",
                "choices": [{ "index": 0, "delta": {}, "finish_reason": "stop" }],
                "usage": { "prompt_tokens": 10, "completion_tokens": 8, "total_tokens": 18 },
            }),
        ];
        let body = format!("data: {}\n\ndata: {}\n\ndata: [DONE]\n\n", chunks[0], chunks[1]);
        ([("content-type", "text/event-stream")], body)
    }));
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let model = "fixture/fixture-model";
    let lease = s
        .accounts
        .acquire(&s, "native-test", Provider::Opencode, model)
        .await
        .unwrap()
        .unwrap();
    let _broker = broker::serve(&s, &lease).await.unwrap();
    let mut config = s.config.clone();
    config.opencode_bin = binary;
    config.home = lease.home.parent().unwrap().to_owned();
    // OpenCode must not load this unrelated global config despite HOME being shared
    // with other command-line tools. A nonexistent plugin makes contamination fail.
    tokio::fs::create_dir_all(config.home.join(".opencode"))
        .await
        .unwrap();
    tokio::fs::write(
        config.home.join(".opencode/opencode.json"),
        json!({ "plugin": ["file:///nonexistent-cairn-global-plugin.mjs"] }).to_string(),
    )
    .await
    .unwrap();
    let mut input = plan(
        &root,
        "Reply with the local model result. Do not call tools.",
    );
    input["model"] = model.into();
    input["reasoning"] = "".into();
    input["opencodeConfig"] = json!({
        "provider": { "fixture": {
            "npm": "@ai-sdk/openai-compatible", "name": "Local test model",
            "options": { "baseURL": format!("http://{address}/v1") },
            "models": { "fixture-model": { "name": "Local test", "limit": { "context": 32000, "output": 4000 } } },
        } },
    });
    let (tx, mut rx) = mpsc::channel(512);
    let output = tokio::spawn(async move {
        let mut values = Vec::new();
        while let Some(v) = rx.recv().await {
            values.push(v);
        }
        values
    });
    let result = tokio::time::timeout(
        Duration::from_secs(60),
        opencode::run(&config, input, tx, CancellationToken::new()),
    )
    .await;
    server.abort();
    result.expect("native execution timed out").unwrap();
    let events = output.await.unwrap();
    assert!(events.iter().any(|e| e["type"] == "turn.completed"));
    assert_eq!(
        tokio::fs::read_to_string(root.path().join("result.md"))
            .await
            .unwrap(),
        "Native OpenCode local model passed."
    );
    assert!(!lease.home.join("data/opencode/auth.json").exists());
}

#[tokio::test]
async fn native_questions_use_cairn_ids_and_answers_reach_the_original_native_request() {
    let root = TempDir::new().unwrap();
    let s = service(&root).await;
    connect(&s).await;
    let lease = s
        .accounts
        .acquire(&s, "question-test", Provider::Opencode, MODEL)
        .await
        .unwrap()
        .unwrap();
    let _broker = broker::serve(&s, &lease).await.unwrap();
    let mut config = s.config.clone();
    config.home = lease.home.parent().unwrap().to_owned();
    let next_config = config.clone();
    let input = plan(&root, "fixture:opencode-question");
    let (tx, mut rx) = mpsc::channel(128);
    let task =
        tokio::spawn(
            async move { opencode::run(&config, input, tx, CancellationToken::new()).await },
        );
    let mut answered = false;
    let mut session = String::new();
    tokio::time::timeout(Duration::from_secs(10), async {
        while let Some(event) = rx.recv().await {
            if event["type"] == "thread.started" {
                session = text(&event, "thread_id").to_owned();
            }
            if event["type"] == "chat.question" {
                let id = text(&event["question"], "id");
                assert_eq!(id.len(), 64);
                assert!(id.bytes().all(|c| c.is_ascii_hexdigit()));
                tokio::fs::create_dir_all(root.path().join("inbox"))
                    .await
                    .unwrap();
                tokio::fs::write(
                    root.path().join("inbox/messages.json"),
                    json!([{
                        "id": "answer", "questionId": id, "answers": { "0": ["Compact"] },
                        "text": "Compact", "attachments": [],
                    }])
                    .to_string(),
                )
                .await
                .unwrap();
                answered = true;
            }
        }
    })
    .await
    .unwrap();
    task.await.unwrap().unwrap();
    assert!(answered);
    assert_eq!(
        tokio::fs::read_to_string(root.path().join("result.md"))
            .await
            .unwrap(),
        "I will use the Compact layout."
    );

    // Leave the delivered answer on disk, as it can be until the manager's next tick.
    let mut next = plan(&root, "Continue after the question.");
    next["execution"]["messageId"] = "next".into();
    next["sessionId"] = session.into();
    let (tx, mut rx) = mpsc::channel(128);
    let next_task = tokio::spawn(async move {
        opencode::run(&next_config, next, tx, CancellationToken::new()).await
    });
    let mut events = Vec::new();
    tokio::time::timeout(Duration::from_secs(10), async {
        while let Some(event) = rx.recv().await {
            events.push(event);
        }
    })
    .await
    .unwrap();
    next_task.await.unwrap().unwrap();
    assert!(events.iter().any(|e| e["type"] == "turn.completed"));
    assert!(!events.iter().any(|e| e["type"] == "chat.question"));
    assert!(
        !events
            .iter()
            .any(|e| e["type"] == "chat.delivered" && e["messageId"] == "answer")
    );
    assert!(
        tokio::fs::read_to_string(root.path().join("result.md"))
            .await
            .unwrap()
            .contains("compact status panel")
    );
}
