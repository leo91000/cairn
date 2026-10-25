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
use std::{path::Path, time::Duration};
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
        .acquire(s, "protocol-test", Provider::Opencode, text(&plan, "model"))
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
async fn disk_notice_is_submitted_without_user_delivery_receipts_including_replay() {
    let root = TempDir::new().unwrap();
    let s = service(&root).await;
    connect(&s).await;
    tokio::fs::create_dir_all(root.path().join("inbox"))
        .await
        .unwrap();
    tokio::fs::write(
        root.path().join("inbox/disk-notice.json"),
        json!({ "id": "disk-notice", "operationalNotice": true, "text": "Runtime disk pressure" })
            .to_string(),
    )
    .await
    .unwrap();
    let input = plan(&root, "Build the status panel");
    for _ in 0..2 {
        let (result, events) = run(&s, input.clone()).await;
        result.unwrap();
        assert!(events.iter().any(|event| event["type"] == "turn.completed"));
        assert!(
            !events
                .iter()
                .any(|event| event["type"] == "chat.delivered"
                    && event["messageId"] == "disk-notice")
        );
    }
    let receipt = tokio::fs::read_to_string(root.path().join("result.opencode-receipt.json"))
        .await
        .unwrap();
    assert!(
        !serde_json::from_str::<Value>(&receipt).unwrap()["delivered"]
            .as_array()
            .unwrap()
            .contains(&json!("disk-notice"))
    );
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
        (
            "fixture:opencode-region",
            "OpenCode Go requires Global regions for this model.",
        ),
        (
            "fixture:opencode-unknown-error",
            "could not complete this response",
        ),
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
async fn persisted_usage_errors_do_not_poison_the_replacement_provider() {
    let root = TempDir::new().unwrap();
    let s = service(&root).await;
    connect(&s).await;
    s.accounts
        .connect_api_key(&s, Provider::Opencode, "Go", "opencode-go", "go-key", None)
        .await
        .unwrap();
    let mut first = plan(&root, "fixture:opencode-limit Build the status panel.");
    first["model"] = "opencode-go/ling-3.1-flash-free".into();
    let (result, events) = run(&s, first.clone()).await;
    assert!(result.unwrap_err().message.contains("Usage limit reached"));
    let session = text(
        events
            .iter()
            .find(|event| event["type"] == "thread.started")
            .unwrap(),
        "thread_id",
    )
    .to_owned();
    let native_path = Provider::Opencode
        .driver()
        .home(&s, "protocol-test")
        .join("data/opencode/fixture-sessions.json");
    let mut history: Value =
        serde_json::from_slice(&tokio::fs::read(&native_path).await.unwrap()).unwrap();
    let messages = history[session.as_str()]["messages"].as_array().unwrap();
    assert_eq!(
        messages.last().unwrap()["info"]["error"]["data"]["statusCode"],
        429
    );
    // Across an ID rollover the old error sorts after the replacement's response.
    // Full creation times must decide which assistant response is newest.
    history[session.as_str()]["messages"]
        .as_array_mut()
        .unwrap()
        .last_mut()
        .unwrap()["info"]["id"] = "msg_ffffffffffffabcdefghijklmn".into();
    tokio::fs::write(&native_path, history.to_string())
        .await
        .unwrap();

    // Keep the same native session and receipt, as the worker does on fallback.
    let mut resumed = first;
    resumed["sessionId"] = session.clone().into();
    resumed["model"] = MODEL.into();
    resumed["execution"]["recovery"] = true.into();
    let (result, events) = run(&s, resumed).await;
    result.unwrap();
    assert!(events.iter().any(|event| event["type"] == "turn.completed"));
    let history: Value =
        serde_json::from_slice(&tokio::fs::read(native_path).await.unwrap()).unwrap();
    assert_eq!(
        history[session.as_str()]["lastModel"]["providerID"],
        "opencode"
    );
    assert!(
        tokio::fs::read_to_string(root.path().join("result.md"))
            .await
            .unwrap()
            .contains("compact status panel")
    );
}

async fn read_startup_pid(pid_file: &Path) -> Option<i32> {
    let content = match tokio::fs::read_to_string(pid_file).await {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
        Err(error) => panic!("could not read startup PID: {error}"),
    };

    if content.is_empty() {
        return None;
    }

    let pid = content
        .parse::<i32>()
        .expect("fixture must publish an integer PID");
    assert!(pid > 0, "fixture must publish a positive PID");
    Some(pid)
}

#[tokio::test]
async fn startup_pid_readiness_waits_for_publication() {
    let root = TempDir::new().unwrap();
    let pid_file = root.path().join("startup.pid");
    assert_eq!(read_startup_pid(&pid_file).await, None);

    tokio::fs::write(&pid_file, "").await.unwrap();
    assert_eq!(read_startup_pid(&pid_file).await, None);

    let expected = i32::try_from(std::process::id()).unwrap();
    tokio::fs::write(&pid_file, expected.to_string())
        .await
        .unwrap();
    assert_eq!(read_startup_pid(&pid_file).await, Some(expected));
}

#[tokio::test]
async fn cancelling_during_native_startup_kills_the_waiting_process() {
    let root = TempDir::new().unwrap();
    let s = service(&root).await;
    connect(&s).await;
    let lease = s
        .accounts
        .acquire(&s, "startup-cancel", Provider::Opencode, MODEL)
        .await
        .unwrap()
        .unwrap();
    let _broker = broker::serve(&s, &lease).await.unwrap();
    let mut config = s.config.clone();
    config.home = lease.home.parent().unwrap().to_owned();
    let mut input = plan(&root, "Never reaches the model");
    input["opencodeConfig"] = json!({ "fixtureStartupHang": true });
    let pid_file = lease.home.join("data/opencode/startup.pid");
    // File creation can be observed before the fixture's PID write completes.
    tokio::fs::create_dir_all(pid_file.parent().unwrap())
        .await
        .unwrap();
    tokio::fs::write(&pid_file, "").await.unwrap();
    let stop = CancellationToken::new();
    let cancellation = stop.clone();
    let (tx, _rx) = mpsc::channel(128);
    let task = tokio::spawn(async move { opencode::run(&config, input, tx, cancellation).await });
    let pid = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(pid) = read_startup_pid(&pid_file).await {
                break pid;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    stop.cancel();
    let result = tokio::time::timeout(Duration::from_secs(3), task)
        .await
        .unwrap()
        .unwrap();
    assert!(result.unwrap_err().message.contains("stopped"));
    tokio::time::timeout(Duration::from_secs(3), async {
        while unsafe { libc::kill(pid, 0) } == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("native process survived startup cancellation");
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

/// Optional native qualification: the real OpenCode release reports the provider and
/// model catalog that account connection and the model picker read.
/// Run with OPENCODE_NATIVE_BIN set via the credential-isolated backend launcher.
#[tokio::test]
#[ignore = "requires a native OpenCode binary"]
async fn native_cli_lists_providers_and_models() {
    let binary = std::env::var("OPENCODE_NATIVE_BIN").expect("set OPENCODE_NATIVE_BIN");
    let root = TempDir::new().unwrap();
    let s = Service::new(Config {
        opencode_bin: binary,
        ..common::config(root.path())
    })
    .await
    .unwrap();
    let providers = s.opencode.providers(&s).await.unwrap();
    for provider in ["opencode", "openai"] {
        assert!(
            providers
                .as_array()
                .unwrap()
                .iter()
                .any(|p| p["id"] == provider && !text(p, "name").is_empty()),
            "{providers}"
        );
    }
    // Connecting checks the provider against the same native catalog.
    s.accounts
        .connect_api_key(&s, Provider::Opencode, "OpenAI", "openai", KEY, None)
        .await
        .unwrap();
    let models = s.opencode.models(&s).await.unwrap();
    assert_eq!(models["error"], "", "{models}");
    assert!(!models["models"].as_array().unwrap().is_empty(), "{models}");
}

/// Optional pinned-native smoke: real OpenCode, a local model server, synthetic auth.
/// Run with OPENCODE_NATIVE_BIN set via the credential-isolated backend launcher.
/// OPENCODE_NATIVE_RESUME_BIN resumes the session with another release: a newer
/// one taking over sessions an installation already holds, or an older one after
/// a rollback.
#[tokio::test]
#[ignore = "requires a native OpenCode binary"]
async fn pinned_native_cli_executes_and_resumes_without_global_plugins() {
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
    // Model requests, to check that a resumed session sends its earlier turns.
    let requests = std::sync::Arc::new(std::sync::Mutex::new(Vec::<Value>::new()));
    let recorded = requests.clone();
    let router = Router::new().route("/v1/chat/completions", post(move |headers: HeaderMap, body: axum::body::Bytes| async move {
        assert_eq!(headers["authorization"], format!("Bearer {KEY}"));
        recorded.lock().unwrap().push(serde_json::from_slice(&body).unwrap());
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
        opencode::run(&config, input.clone(), tx, CancellationToken::new()),
    )
    .await;
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

    let session = text(
        events
            .iter()
            .find(|event| event["type"] == "thread.started")
            .unwrap(),
        "thread_id",
    )
    .to_owned();
    if let Ok(binary) = std::env::var("OPENCODE_NATIVE_RESUME_BIN") {
        config.opencode_bin = binary;
    }
    let first_turn = requests.lock().unwrap().len();
    input["sessionId"] = session.clone().into();
    input["execution"]["messageId"] = "follow-up".into();
    input["execution"]["text"] = "Continue in the same session. Do not call tools.".into();
    input["output"] = root
        .path()
        .join("follow-up.md")
        .to_string_lossy()
        .into_owned()
        .into();
    let (tx, mut rx) = mpsc::channel(512);
    let output = tokio::spawn(async move {
        let mut values = Vec::new();
        while let Some(value) = rx.recv().await {
            values.push(value);
        }
        values
    });
    let result = tokio::time::timeout(
        Duration::from_secs(60),
        opencode::run(&config, input, tx, CancellationToken::new()),
    )
    .await;
    server.abort();
    result.expect("native resume timed out").unwrap();
    let events = output.await.unwrap();
    assert!(
        events
            .iter()
            .any(|event| event["type"] == "thread.started" && event["thread_id"] == session)
    );
    assert!(events.iter().any(|event| event["type"] == "turn.completed"));
    assert_eq!(
        tokio::fs::read_to_string(root.path().join("follow-up.md"))
            .await
            .unwrap(),
        "Native OpenCode local model passed."
    );
    // The resuming release loaded the stored history rather than starting over.
    assert!(
        requests.lock().unwrap()[first_turn..]
            .iter()
            .any(|request| {
                let messages = request["messages"].to_string();
                messages.contains("Reply with the local model result")
                    && messages.contains("Native OpenCode local model passed.")
                    && messages.contains("Continue in the same session")
            }),
        "The follow-up request did not include the first turn"
    );
}

#[tokio::test]
async fn withdrawn_questions_reject_the_native_request_without_an_answer() {
    let root = TempDir::new().unwrap();
    let s = service(&root).await;
    connect(&s).await;
    let lease = s
        .accounts
        .acquire(&s, "withdraw-question-test", Provider::Opencode, MODEL)
        .await
        .unwrap()
        .unwrap();
    let _broker = broker::serve(&s, &lease).await.unwrap();
    let mut config = s.config.clone();
    config.home = lease.home.parent().unwrap().to_owned();
    let input = plan(&root, "fixture:opencode-question");
    let (tx, mut rx) = mpsc::channel(128);
    let task =
        tokio::spawn(
            async move { opencode::run(&config, input, tx, CancellationToken::new()).await },
        );
    let mut completed = false;
    tokio::time::timeout(Duration::from_secs(10), async {
        while let Some(event) = rx.recv().await {
            if event["type"] == "chat.question" {
                tokio::fs::create_dir_all(root.path().join("inbox")).await.unwrap();
                tokio::fs::write(root.path().join("inbox/messages.json"), json!([{
                    "id": "withdrawal", "questionId": event["question"]["id"], "questionCancelled": true,
                }]).to_string()).await.unwrap();
            }
            assert!(!(event["type"] == "chat.delivered" && event["messageId"] == "withdrawal"));
            completed |= event["type"] == "turn.completed";
        }
    }).await.unwrap();
    task.await.unwrap().unwrap();
    assert!(completed);
    assert_eq!(
        tokio::fs::read_to_string(root.path().join("result.md"))
            .await
            .unwrap(),
        "Question withdrawn without an answer."
    );
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

#[tokio::test]
async fn key_replacement_keeps_concurrent_account_edits() {
    let root = TempDir::new().unwrap();
    let s = service(&root).await;
    let id = text(&connect(&s).await, "id").to_owned();
    let update_guard = s.accounts.lock(&id).await;
    let mut updated = s.accounts.account(&s, &id).await.unwrap();
    let mut replace = Box::pin(s.accounts.connect_api_key(
        &s,
        Provider::Opencode,
        "Zen",
        "opencode",
        "replacement-key",
        Some(&id),
    ));
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut replace)
            .await
            .is_err(),
        "replacement must wait for the account edit"
    );
    updated.name = "Paused Zen".into();
    updated.enabled = false;
    updated.max_concurrent_runs = Some(1);
    s.store.put(KIND, updated.to_value()).await.unwrap();
    drop(update_guard);

    tokio::time::timeout(Duration::from_secs(5), replace)
        .await
        .unwrap()
        .unwrap();
    let account = s.accounts.account(&s, &id).await.unwrap();
    assert_eq!(account.name, "Paused Zen");
    assert!(!account.enabled);
    assert_eq!(account.max_concurrent_runs, Some(1));
    let credentials = s
        .vault
        .get(&format!("opencode-account:{id}"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(credentials["key"], "replacement-key");
}

#[tokio::test]
async fn connection_order_waits_for_in_flight_account_updates() {
    let root = TempDir::new().unwrap();
    let s = service(&root).await;
    let zen = text(&connect(&s).await, "id").to_owned();
    let go = s
        .accounts
        .connect_api_key(&s, Provider::Opencode, "Go", "opencode-go", "go-key", None)
        .await
        .unwrap();
    let go = text(&go, "id").to_owned();

    // A refresh or edit holds this lock while saving its account snapshot.
    let update_guard = s.accounts.lock(&zen).await;
    let mut updating = s.accounts.account(&s, &zen).await.unwrap();
    let order = [go.clone(), zen.clone()];
    let mut reorder = Box::pin(s.accounts.reorder(&s, Provider::Opencode, &order));
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut reorder)
            .await
            .is_err(),
        "ordering must wait for the account update to finish"
    );
    updating.name = "Updated Zen".into();
    s.store.put(KIND, updating.to_value()).await.unwrap();
    drop(update_guard);

    tokio::time::timeout(Duration::from_secs(5), reorder)
        .await
        .unwrap()
        .unwrap();
    let zen = s.accounts.account(&s, &zen).await.unwrap();
    assert_eq!(zen.name, "Updated Zen");
    assert_eq!(zen.extra["priority"], 1);
    assert_eq!(
        s.accounts.account(&s, &go).await.unwrap().extra["priority"],
        0
    );
}

#[tokio::test]
async fn auto_models_follow_the_connection_order_and_fall_back_when_one_is_exhausted() {
    let root = TempDir::new().unwrap();
    let s = service(&root).await;
    let zen = text(&connect(&s).await, "id").to_owned();
    let go = s
        .accounts
        .connect_api_key(&s, Provider::Opencode, "Go", "opencode-go", "go-key", None)
        .await
        .unwrap();
    let go = text(&go, "id").to_owned();
    assert!(
        s.accounts
            .reorder(&s, Provider::Opencode, std::slice::from_ref(&go))
            .await
            .is_err(),
        "an order must list every account"
    );
    s.accounts
        .reorder(&s, Provider::Opencode, &[go.clone(), zen.clone()])
        .await
        .unwrap();

    let auto = "auto/ling-3-1-flash-free";
    let models = s.opencode.models(&s).await.unwrap();
    let entry = models["models"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["model"] == auto)
        .unwrap();
    assert_eq!(
        entry["routes"][0]["model"],
        "opencode-go/ling-3.1-flash-free"
    );
    assert_eq!(entry["routes"][1]["model"], MODEL);

    let first = s
        .accounts
        .acquire(&s, "auto-first", Provider::Opencode, auto)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first.account_id, go);
    s.accounts.release(&first).await.unwrap();
    s.accounts.exhausted(&s, &go, auto).await.unwrap();
    let next = s
        .accounts
        .acquire(&s, "auto-next", Provider::Opencode, auto)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(next.account_id, zen);
    s.accounts.release(&next).await.unwrap();

    // A pinned route never moves to another provider.
    let pinned = s
        .accounts
        .acquire(
            &s,
            "pinned",
            Provider::Opencode,
            "opencode-go/ling-3.1-flash-free",
        )
        .await;
    assert!(pinned.unwrap_err().is_conflict());
}
