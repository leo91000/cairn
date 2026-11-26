//! Optional native qualification of side questions: the real Claude Code and Codex
//! releases answer side questions through Cairn's adapters while a turn runs, against a
//! local model. A side question whose answer calls a tool must not run it, a withdrawn one
//! settles silently, the turn completes as without them, and no side question enters the
//! conversation's history (ADR-0093). No credential or external service is used. Run
//! through the credential-isolated backend launcher, for example:
//! `CODEX_NATIVE_BIN=… CLAUDE_NATIVE_BIN=… pnpm test:backend -p cairn-installation --test native_side_questions -- --ignored --test-threads 1`.
mod common;

use axum::{Router, body::Bytes, routing::post};
use cairn_installation::{chat_process, claude_process, config::Config, skills::atomic_write};
use serde_json::{Value, json};
use std::{
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tempfile::TempDir;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// The main turn's request, which the model answers once the side questions settled.
const MAIN_REQUEST: &str = "Do the main work.";
const MAIN_REPLY: &str = "Main work finished.";
const SIDE_REPLY: &str = "Side answer from the local model.";

/// Markers in each side question, which tell the model what to do with it.
const TEXT: &str = "SIDE-TEXT";
const TOOL: &str = "SIDE-TOOL";
const HOLD: &str = "SIDE-HOLD";

const ANSWERED: &str = "a1b2c3d4-0000-4000-8000-0000000000a1";
const TOOL_ID: &str = "a1b2c3d4-0000-4000-8000-0000000000a2";
const WITHDRAWN: &str = "a1b2c3d4-0000-4000-8000-0000000000a3";

/// Which side question a model request answers, if any: the marker in its latest user text.
fn marker(latest_user_text: &str) -> Option<&'static str> {
    [TEXT, TOOL, HOLD]
        .into_iter()
        .find(|marker| latest_user_text.contains(marker))
}

/// Waits until the main turn may finish.
async fn released(release: &AtomicBool) {
    while !release.load(Ordering::SeqCst) {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn sse(events: &[Value]) -> String {
    events
        .iter()
        .map(|event| {
            format!(
                "event: {}\ndata: {event}\n\n",
                event["type"].as_str().unwrap()
            )
        })
        .collect()
}

fn report(name: &str, value: &Value) {
    eprintln!("{name}: {}", serde_json::to_string_pretty(value).unwrap());
    if let Ok(directory) = std::env::var("NATIVE_PROBE_REPORTS") {
        std::fs::write(
            Path::new(&directory).join(format!("{name}.json")),
            serde_json::to_vec_pretty(value).unwrap(),
        )
        .unwrap();
    }
}

fn launcher(root: &Path, name: &str, script: String) -> String {
    let path = root.join(name);
    std::fs::write(&path, script).unwrap();
    std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    path.to_string_lossy().into_owned()
}

fn inbox() -> Vec<Value> {
    [
        (ANSWERED, format!("{TEXT}: why this order?")),
        (TOOL_ID, format!("{TOOL}: check the workspace.")),
        (WITHDRAWN, format!("{HOLD}: take your time.")),
    ]
    .into_iter()
    .map(|(id, text)| {
        json!({ "id": format!("side-question:{id}"), "sideQuestion": { "id": id, "text": text } })
    })
    .collect()
}

/// Delivers the side questions once the adapter offers them, withdraws the held one once
/// the others settled, then lets the main turn finish. Returns every event.
async fn drive(
    root: &Path,
    mut rx: mpsc::Receiver<Value>,
    release: Arc<AtomicBool>,
    timeout: Duration,
) -> Vec<Value> {
    let path = root.join("inbox/messages.json");
    let mut entries = inbox();
    let mut events = Vec::<Value>::new();
    let drained = tokio::time::timeout(timeout, async {
        while let Some(event) = rx.recv().await {
            events.push(event.clone());
            if event["type"] == "chat.side.available" {
                atomic_write(&path, json!(entries).to_string().as_bytes())
                    .await
                    .unwrap();
            }
            let settled = |id: &str| {
                events.iter().any(|event| {
                    event["sideQuestionId"] == id
                        && matches!(
                            event["type"].as_str(),
                            Some("chat.side.answered" | "chat.side.failed")
                        )
                })
            };
            if settled(ANSWERED) && settled(TOOL_ID) && entries.len() == 3 {
                entries.push(json!({
                    "id": format!("withdraw-side-question:{WITHDRAWN}"),
                    "sideQuestionId": WITHDRAWN,
                    "sideQuestionCancelled": true,
                }));
                atomic_write(&path, json!(entries).to_string().as_bytes())
                    .await
                    .unwrap();
                // The adapter polls its inbox four times a second.
                tokio::time::sleep(Duration::from_secs(2)).await;
                release.store(true, Ordering::SeqCst);
            }
        }
    })
    .await;
    // Never leave the scripted model holding the turn after a failure.
    release.store(true, Ordering::SeqCst);
    drained.unwrap_or_else(|_| panic!("native side questions timed out: {events:#?}"));
    events
}

fn settled<'a>(events: &'a [Value], id: &str) -> Option<&'a Value> {
    events.iter().find(|event| {
        event["sideQuestionId"] == id
            && matches!(
                event["type"].as_str(),
                Some("chat.side.answered" | "chat.side.failed")
            )
    })
}

/// What both engines must show.
fn check(events: &[Value], workspace: &Path, output: &Path) {
    assert!(
        events.iter().any(|event| event["type"] == "turn.completed"),
        "{events:#?}"
    );
    // Only side-question events carry a side question or its answer: none reaches the
    // conversation's items or the reply.
    let reply = std::fs::read_to_string(output).unwrap();
    assert!(reply.contains(MAIN_REPLY), "{reply}");
    for event in events {
        if event["type"]
            .as_str()
            .unwrap_or_default()
            .starts_with("chat.side.")
        {
            continue;
        }
        let event = event.to_string();
        for marker in [SIDE_REPLY, TEXT, TOOL, HOLD] {
            assert!(
                !event.contains(marker),
                "{marker} reached the conversation: {event}"
            );
        }
    }
    for marker in [SIDE_REPLY, TEXT, TOOL, HOLD] {
        assert!(!reply.contains(marker), "{marker} reached the reply");
    }
    let answered = settled(events, ANSWERED).expect("the text side question did not settle");
    assert_eq!(answered["type"], "chat.side.answered", "{answered}");
    assert!(
        answered["text"].as_str().unwrap().contains(SIDE_REPLY),
        "{answered}"
    );
    assert!(
        settled(events, TOOL_ID).is_some(),
        "the tool side question did not settle"
    );
    // The same call ran in the main turn, so the side question's call was executable.
    assert!(
        workspace.join("main-write").exists(),
        "the main turn's tool call did not run"
    );
    assert!(
        !workspace.join("side-write").exists(),
        "a side question ran a tool"
    );
    assert!(
        !events
            .iter()
            .any(|event| event["sideQuestionId"] == WITHDRAWN),
        "the withdrawn side question settled"
    );
    let completed = events
        .iter()
        .position(|event| event["type"] == "turn.completed")
        .unwrap();
    let closed = events
        .iter()
        .position(|event| event["type"] == "chat.side.closed")
        .expect("side admission never closed");
    assert!(closed < completed);
}

/// The texts of a Responses API request's user messages, newest last.
fn codex_user_texts(request: &Value) -> Vec<String> {
    request["input"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|item| item["type"] == "message" && item["role"] == "user")
        .map(|item| item["content"].to_string())
        .collect()
}

fn codex_reply(item: &Value) -> String {
    let id = format!("resp_{}", uuid::Uuid::new_v4().simple());
    sse(&[
        json!({ "type": "response.created", "response": { "id": id } }),
        json!({ "type": "response.output_item.done", "item": item }),
        json!({
            "type": "response.completed",
            "response": {
                "id": id,
                "usage": {
                    "input_tokens": 10,
                    "input_tokens_details": null,
                    "output_tokens": 5,
                    "output_tokens_details": null,
                    "total_tokens": 15,
                },
            },
        }),
    ])
}

/// Codex's code-mode shell, as its models call it.
fn codex_shell(command: &str) -> Value {
    json!({
        "type": "custom_tool_call",
        "id": format!("ctc_{}", uuid::Uuid::new_v4().simple()),
        "call_id": format!("call_{}", uuid::Uuid::new_v4().simple()),
        "namespace": "functions",
        "name": "exec",
        "input": format!("const result = await tools.exec_command({{ cmd: {} }});\ntext(result);", Value::from(command)),
    })
}

fn codex_message(text: &str) -> Value {
    json!({
        "type": "message",
        "role": "assistant",
        "id": format!("msg_{}", uuid::Uuid::new_v4().simple()),
        "content": [{ "type": "output_text", "text": text }],
    })
}

/// A model Codex lists, ranked by its catalog: one whose shell runs through `exec`.
fn codex_model(binary: &str, home: &Path) -> String {
    let output = std::process::Command::new(binary)
        .args(["debug", "models"])
        .env("CODEX_HOME", home)
        .output()
        .unwrap();
    let catalog: Value = serde_json::from_slice(&output.stdout).unwrap();
    let mut listed = catalog["models"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|model| model["visibility"] == "list")
        .collect::<Vec<_>>();
    listed.sort_by_key(|model| model["priority"].as_i64());
    listed.first().expect("Codex lists no model")["slug"]
        .as_str()
        .unwrap()
        .to_owned()
}

#[tokio::test]
#[ignore = "requires a native Codex binary"]
async fn native_codex_answers_side_questions_without_tools_or_history() {
    let binary = std::env::var("CODEX_NATIVE_BIN").expect("set CODEX_NATIVE_BIN");
    let root = TempDir::new().unwrap();
    let workspace = root.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let write = |name: &str| format!("touch {}/{name}", workspace.display());

    let (write_side, write_main) = (write("side-write"), write("main-write"));
    let release = Arc::new(AtomicBool::new(false));
    let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
    let (recorded, holding) = (requests.clone(), release.clone());
    let router = Router::new().route(
        "/v1/responses",
        post(move |body: Bytes| {
            let request: Value = serde_json::from_slice(&body).unwrap();
            recorded.lock().unwrap().push(request.clone());
            let latest = codex_user_texts(&request).pop().unwrap_or_default();
            // Codex's own instructions name the output type, so look at the items.
            let started = request["input"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|item| item["type"] == "custom_tool_call_output");
            let holding = holding.clone();
            let (side_write, main_write) = (write_side.clone(), write_main.clone());
            async move {
                let item = match marker(&latest) {
                    Some(TEXT) => codex_message(SIDE_REPLY),
                    Some(TOOL) => codex_shell(&side_write),
                    None if !started => codex_shell(&main_write),
                    _ => {
                        released(&holding).await;
                        codex_message(MAIN_REPLY)
                    }
                };
                ([("content-type", "text/event-stream")], codex_reply(&item))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });

    let home = root.path().join("codex");
    std::fs::create_dir(&home).unwrap();
    let model = codex_model(&binary, &home);
    std::fs::write(
        home.join("config.toml"),
        format!(
            "model = \"{model}\"\nmodel_provider = \"local\"\n\n[model_providers.local]\nname = \"local\"\nbase_url = \"http://{address}/v1\"\nwire_api = \"responses\"\nenv_key = \"LOCAL_PROBE_KEY\"\n"
        ),
    )
    .unwrap();
    let config = Config {
        codex_bin: launcher(
            root.path(),
            "codex-launcher",
            format!("#!/bin/sh\nexport LOCAL_PROBE_KEY=local-test\nexec '{binary}' \"$@\"\n"),
        ),
        ..common::config(root.path())
    };
    std::fs::create_dir(root.path().join("inbox")).unwrap();
    std::fs::create_dir_all(&config.home).unwrap();
    let turn = |id: &str, text: &str| {
        json!({
            "execution": { "messageId": id, "text": text, "recovery": false, "attachments": [] },
            "instructions": "Follow the task scope",
            "inputDirectory": root.path().join("inbox"),
            "output": root.path().join(format!("{id}.md")),
            "cwd": workspace,
            "model": "",
            "reasoning": "medium",
            "sandbox": "yolo",
            "writableRoots": [workspace],
            "args": [],
        })
    };

    let (tx, rx) = mpsc::channel(256);
    let (run_config, run_home, first) = (config.clone(), home.clone(), turn("main", MAIN_REQUEST));
    let task = tokio::spawn(async move {
        chat_process::run(&run_config, &run_home, first, tx, CancellationToken::new()).await
    });
    let events = drive(root.path(), rx, release.clone(), Duration::from_secs(180)).await;
    let result = task.await.unwrap().map_err(|error| error.message);
    let session = events
        .iter()
        .find(|event| event["type"] == "thread.started")
        .map_or(Value::Null, |event| event["thread_id"].clone());

    // The next turn's context holds no side question.
    std::fs::write(root.path().join("inbox/messages.json"), "[]").unwrap();
    let next_start = requests.lock().unwrap().len();
    let mut next = turn("next", "Continue.");
    next["sessionId"] = session;
    let (tx, mut rx) = mpsc::channel(256);
    tokio::spawn(async move { while rx.recv().await.is_some() {} });
    let next_result = tokio::time::timeout(
        Duration::from_secs(120),
        chat_process::run(&config, &home, next, tx, CancellationToken::new()),
    )
    .await
    .expect("the next Codex turn timed out")
    .map_err(|error| error.message);

    let requests = requests.lock().unwrap().clone();
    let samples = requests[..next_start]
        .iter()
        .filter(|request| marker(&codex_user_texts(request).pop().unwrap_or_default()).is_some())
        .collect::<Vec<_>>();
    report(
        "codex-side-questions",
        &json!({
            "binary": binary,
            "result": result,
            "nextResult": next_result,
            "sideEvents": events.iter().filter(|event| event["type"].as_str().unwrap_or_default().starts_with("chat.side")).collect::<Vec<_>>(),
            "sideWrite": workspace.join("side-write").exists(),
            "samples": samples.len(),
            "requestBodies": if std::env::var("NATIVE_PROBE_DUMP").is_ok() { json!(requests) } else { Value::Null },
        }),
    );

    result.unwrap();
    next_result.unwrap();
    check(&events, &workspace, &root.path().join("main.md"));
    let tool = settled(&events, TOOL_ID).unwrap();
    assert_eq!(tool["type"], "chat.side.failed", "{tool}");
    // Each side question saw the running turn's request.
    assert!(samples.len() >= 3, "{}", samples.len());
    for sample in &samples {
        assert!(sample["input"].to_string().contains(MAIN_REQUEST));
    }
    for request in &requests[next_start..] {
        let context = request["input"].to_string();
        for marker in [TEXT, TOOL, HOLD] {
            assert!(!context.contains(marker), "{marker} entered the history");
        }
    }
}

/// The text of the latest user message of a Messages API request.
fn claude_latest_user_text(request: &Value) -> String {
    request["messages"]
        .as_array()
        .into_iter()
        .flatten()
        .rfind(|message| message["role"] == "user")
        .map(|message| message["content"].to_string())
        .unwrap_or_default()
}

fn claude_bash(agent: &str, command: &str) -> Value {
    json!({
        "type": "tool_use",
        "id": format!("toolu_{agent}_probe_00000000000000"),
        "name": "Bash",
        "input": { "command": command, "description": "write" },
    })
}

fn claude_reply(model: &Value, block: &Value, stop: &str) -> String {
    let (start, delta) = if block["type"] == "tool_use" {
        let mut start = block.clone();
        start["input"] = json!({});
        let delta =
            json!({ "type": "input_json_delta", "partial_json": block["input"].to_string() });
        (start, delta)
    } else {
        (
            json!({ "type": "text", "text": "" }),
            json!({ "type": "text_delta", "text": block["text"] }),
        )
    };
    sse(&[
        json!({
            "type": "message_start",
            "message": {
                "id": format!("msg_{}", uuid::Uuid::new_v4().simple()),
                "type": "message",
                "role": "assistant",
                "model": model,
                "content": [],
                "stop_reason": null,
                "usage": { "input_tokens": 10, "output_tokens": 0 },
            },
        }),
        json!({ "type": "content_block_start", "index": 0, "content_block": start }),
        json!({ "type": "content_block_delta", "index": 0, "delta": delta }),
        json!({ "type": "content_block_stop", "index": 0 }),
        json!({
            "type": "message_delta",
            "delta": { "stop_reason": stop },
            "usage": { "output_tokens": 8 },
        }),
        json!({ "type": "message_stop" }),
    ])
}

#[tokio::test]
#[ignore = "requires a native Claude Code binary"]
async fn native_claude_answers_side_questions_without_tools_or_history() {
    let binary = std::env::var("CLAUDE_NATIVE_BIN").expect("set CLAUDE_NATIVE_BIN");
    let root = TempDir::new().unwrap();
    let workspace = root.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let write = |name: &str| format!("touch {}/{name}", workspace.display());

    let (write_side, write_main) = (write("side-write"), write("main-write"));
    let release = Arc::new(AtomicBool::new(false));
    let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
    let (recorded, holding) = (requests.clone(), release.clone());
    let router = Router::new()
        .route(
            "/v1/messages",
            post(move |body: Bytes| {
                let request: Value = serde_json::from_slice(&body).unwrap();
                recorded.lock().unwrap().push(request.clone());
                let holding = holding.clone();
                let (side_write, main_write) = (write_side.clone(), write_main.clone());
                async move {
                    let model = &request["model"];
                    if request["stream"] != true {
                        // A classifier or other unstreamed helper request.
                        let reply = json!({
                            "id": format!("msg_{}", uuid::Uuid::new_v4().simple()),
                            "type": "message",
                            "role": "assistant",
                            "model": model,
                            "content": [{ "type": "text", "text": "ok" }],
                            "stop_reason": "end_turn",
                            "usage": { "input_tokens": 10, "output_tokens": 1 },
                        });
                        return ([("content-type", "application/json")], reply.to_string());
                    }
                    let main = request["tools"].to_string().contains("\"Bash\"");
                    let started = request["messages"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .flat_map(|message| {
                            message["content"].as_array().cloned().unwrap_or_default()
                        })
                        .any(|block| block["type"] == "tool_result");
                    let body = match marker(&claude_latest_user_text(&request)) {
                        Some(TEXT) => claude_reply(
                            model,
                            &json!({ "type": "text", "text": SIDE_REPLY }),
                            "end_turn",
                        ),
                        Some(TOOL) => {
                            claude_reply(model, &claude_bash("side", &side_write), "tool_use")
                        }
                        None if main && !started => {
                            claude_reply(model, &claude_bash("main", &main_write), "tool_use")
                        }
                        Some(_) => {
                            released(&holding).await;
                            claude_reply(
                                model,
                                &json!({ "type": "text", "text": "Late." }),
                                "end_turn",
                            )
                        }
                        None if main => {
                            released(&holding).await;
                            claude_reply(
                                model,
                                &json!({ "type": "text", "text": MAIN_REPLY }),
                                "end_turn",
                            )
                        }
                        // Titles and other helper requests.
                        None => claude_reply(
                            model,
                            &json!({ "type": "text", "text": "ok" }),
                            "end_turn",
                        ),
                    };
                    ([("content-type", "text/event-stream")], body)
                }
            }),
        )
        .route(
            "/v1/messages/count_tokens",
            post(|| async { axum::Json(json!({ "input_tokens": 10 })) }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });

    let config = Config {
        claude_bin: launcher(
            root.path(),
            "claude",
            format!(
                "#!/bin/sh\nexport ANTHROPIC_BASE_URL=http://{address} ANTHROPIC_API_KEY=sk-ant-local-test CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1\nexec '{binary}' \"$@\"\n"
            ),
        ),
        ..common::config(root.path())
    };
    std::fs::create_dir_all(config.home.join(".claude")).unwrap();
    std::fs::create_dir(root.path().join("inbox")).unwrap();
    let plan = json!({
        "provider": "claude",
        "execution": { "messageId": "main", "text": MAIN_REQUEST, "attachments": [] },
        "instructions": "Follow the task scope",
        "inputDirectory": root.path().join("inbox"),
        "output": root.path().join("result.md"),
        "cwd": workspace,
        "model": "sonnet",
        "reasoning": "high",
        "sandbox": "yolo",
        "writableRoots": [workspace],
        "claudeMcps": { "mcpServers": {} },
        "claudeDeniedTools": [],
    });

    let (tx, rx) = mpsc::channel(256);
    let run_config = config.clone();
    let task = tokio::spawn(async move {
        claude_process::run(&run_config, plan, tx, CancellationToken::new()).await
    });
    let events = drive(root.path(), rx, release.clone(), Duration::from_secs(180)).await;
    let result = task.await.unwrap().map_err(|error| error.message);

    // Claude Code's saved session holds no side question.
    let mut sessions = Vec::new();
    let mut directories = vec![config.home.join(".claude")];
    while let Some(directory) = directories.pop() {
        for entry in std::fs::read_dir(directory).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                directories.push(path);
            } else if path
                .extension()
                .is_some_and(|extension| extension == "jsonl")
            {
                sessions.push(path);
            }
        }
    }
    let transcripts = sessions
        .iter()
        .map(|path| std::fs::read_to_string(path).unwrap())
        .collect::<Vec<_>>();
    report(
        "claude-side-questions",
        &json!({
            "binary": binary,
            "result": result,
            "sideEvents": events.iter().filter(|event| event["type"].as_str().unwrap_or_default().starts_with("chat.side")).collect::<Vec<_>>(),
            "sideWrite": workspace.join("side-write").exists(),
            "sessions": sessions,
            "requestBodies": if std::env::var("NATIVE_PROBE_DUMP").is_ok() { json!(requests.lock().unwrap().clone()) } else { Value::Null },
        }),
    );

    result.unwrap();
    check(&events, &workspace, &root.path().join("result.md"));
    assert!(
        transcripts
            .iter()
            .any(|transcript| transcript.contains(MAIN_REQUEST)),
        "no saved session was found"
    );
    for transcript in &transcripts {
        for marker in [TEXT, TOOL, HOLD] {
            assert!(!transcript.contains(marker), "{marker} entered the session");
        }
    }
}
