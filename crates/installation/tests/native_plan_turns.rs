//! Optional native qualification of plan turns: the real coding-agent releases run a
//! plan turn through Cairn's adapters against a local model that requests effects, and
//! the test checks that none of them happened. No credential or external service is
//! used: the model is a local scripted API, and every write targets the test's scratch
//! directory. Run through the credential-isolated backend launcher, for example:
//! `CLAUDE_NATIVE_BIN=… pnpm test:backend -p cairn-installation --test native_plan_turns -- --ignored --nocapture`.
mod common;

use axum::{Router, body::Bytes, routing::post};
use cairn_installation::{claude_process, config::Config};
use serde_json::{Value, json};
use std::{
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tempfile::TempDir;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// The marker of the subagent's task, which tells its requests apart from the main agent's.
const SUBAGENT: &str = "SUBAGENT PROBE";

/// The user request of the probed Codex plan turn.
const PLAN_REQUEST: &str = "Plan the change.";

/// A local listener that counts the connections a sandboxed command manages to open.
async fn probe_listener() -> (u16, Arc<AtomicUsize>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let hits = Arc::new(AtomicUsize::new(0));
    let counted = hits.clone();
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            counted.fetch_add(1, Ordering::SeqCst);
            drop(stream);
        }
    });
    (port, hits)
}

fn tool_results(request: &Value) -> Vec<Value> {
    request["messages"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|message| message["content"].as_array().cloned().unwrap_or_default())
        .filter(|block| block["type"] == "tool_result")
        .collect()
}

/// A streamed Messages API reply, as Server-Sent Events named by their type.
fn streamed(model: &Value, block: &Value, stop: &str) -> String {
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
    [
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
    ]
    .iter()
    .map(|event| {
        format!(
            "event: {}\ndata: {event}\n\n",
            event["type"].as_str().unwrap()
        )
    })
    .collect()
}

/// A tool use ID as long as the model's own: Claude Code binds per-command network
/// lists to it.
fn tool_id(agent: &str, step: usize) -> String {
    format!("toolu_probe_{agent}_{step:02}_00000000000000")
}

/// A local Messages API that plays `main` to the main agent and `subagent` to the
/// subagent it starts, one tool call per request, then ends each conversation.
async fn scripted_model(
    main: Vec<(&'static str, Value)>,
    subagent: Vec<(&'static str, Value)>,
) -> (String, Arc<Mutex<Vec<Value>>>) {
    let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
    let recorded = requests.clone();
    let reply = move |request: Value| -> (&'static str, String) {
        let model = &request["model"];
        if request["stream"] != true {
            // Claude Code's auto-mode classifier, which is not streamed. It approves every
            // action here: the plan boundary must hold even when a model judges an effect
            // harmless.
            let verdict = json!({
                "id": format!("msg_{}", uuid::Uuid::new_v4().simple()),
                "type": "message",
                "role": "assistant",
                "model": model,
                "content": [{ "type": "text", "text": "<severity>3" }],
                "stop_reason": "stop_sequence",
                "stop_sequence": "</severity>",
                "usage": { "input_tokens": 10, "output_tokens": 3 },
            });
            return ("application/json", verdict.to_string());
        }
        let first = request["messages"][0]["content"].to_string();
        let (script, agent) = if first.contains(SUBAGENT) {
            (&subagent, "sub")
        } else if request["tools"].to_string().contains("\"Bash\"") {
            (&main, "main")
        } else {
            // Titles, summaries and other side requests.
            let text = json!({ "type": "text", "text": "ok" });
            return ("text/event-stream", streamed(model, &text, "end_turn"));
        };
        let step = tool_results(&request).len();
        let body = match script.get(step) {
            Some((name, input)) => {
                let call = json!({
                    "type": "tool_use",
                    "id": tool_id(agent, step),
                    "name": name,
                    "input": input,
                });
                streamed(model, &call, "tool_use")
            }
            None => {
                let text = json!({ "type": "text", "text": "Probe finished." });
                streamed(model, &text, "end_turn")
            }
        };
        ("text/event-stream", body)
    };
    let router = Router::new()
        .route(
            "/v1/messages",
            post(move |body: Bytes| {
                let request: Value = serde_json::from_slice(&body).unwrap();
                recorded.lock().unwrap().push(request.clone());
                let (kind, body) = reply(request);
                async move { ([("content-type", kind)], body) }
            }),
        )
        .route(
            "/v1/messages/count_tokens",
            post(|| async { axum::Json(json!({ "input_tokens": 10 })) }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    (format!("http://{address}"), requests)
}

/// Every tool call the model made and what Claude Code answered, by tool use ID.
fn transcript(requests: &[Value]) -> Vec<Value> {
    let mut calls = Vec::<Value>::new();
    for request in requests {
        for result in tool_results(request) {
            let id = result["tool_use_id"].clone();
            if !calls.iter().any(|call| call["id"] == id) {
                calls.push(json!({ "id": id, "isError": result["is_error"], "content": result["content"] }));
            }
        }
    }
    calls
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

#[tokio::test]
#[ignore = "requires a native Claude Code binary"]
async fn native_claude_plan_turn_blocks_writes_network_and_subagent_effects() {
    let binary = std::env::var("CLAUDE_NATIVE_BIN").expect("set CLAUDE_NATIVE_BIN");
    let root = TempDir::new().unwrap();
    let workspace = root.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    std::fs::write(workspace.join("main.rs"), "fn main() {}\n").unwrap();
    let ws = workspace.to_string_lossy().into_owned();
    let (probe, hits) = probe_listener().await;
    let loopback = format!("curl -sS -m 5 http://127.0.0.1:{probe}/");
    // Through the sandbox's proxy: an allowlisted host fails to resolve, a blocked one is refused.
    let proxied =
        "curl -sS -m 10 -o /dev/null -w 'proxy status %{http_code}\\n' http://cairn-probe.invalid/";

    let main = vec![
        (
            "Bash",
            json!({ "command": format!("touch {ws}/bash-write; echo exit=$?"), "description": "write" }),
        ),
        (
            "Bash",
            json!({ "command": format!("{loopback}; echo exit=$?; {proxied}; echo exit=$?; env | grep -i _proxy | sort"), "description": "network" }),
        ),
        (
            "Bash",
            json!({ "command": format!("{proxied}; echo exit=$?; {loopback}; echo exit=$?"), "description": "per-command domains", "allowed_domains": ["cairn-probe.invalid"] }),
        ),
        (
            "Bash",
            json!({ "command": format!("touch {ws}/unsandboxed; echo exit=$?"), "description": "unsandboxed", "dangerouslyDisableSandbox": true }),
        ),
        (
            "Write",
            json!({ "file_path": format!("{ws}/write-tool"), "content": "changed" }),
        ),
        (
            "Edit",
            json!({ "file_path": format!("{ws}/main.rs"), "old_string": "fn main() {}", "new_string": "fn main() { changed() }" }),
        ),
        (
            "Bash",
            json!({ "command": "echo HOME=$HOME; touch \"$HOME/home-write\"; echo home=$?; touch /tmp/cairn-plan-probe; echo tmp=$?; rm -f /tmp/cairn-plan-probe", "description": "outside" }),
        ),
        (
            "Agent",
            json!({ "description": "Probe", "prompt": format!("{SUBAGENT}: change the workspace"), "subagent_type": "general-purpose" }),
        ),
        (
            "ExitPlanMode",
            json!({ "plan": "1. Change main.rs\n2. Run the tests" }),
        ),
    ];
    let subagent = vec![
        (
            "Bash",
            json!({ "command": format!("touch {ws}/subagent-bash; echo exit=$?; {loopback}; echo exit=$?"), "description": "subagent write" }),
        ),
        (
            "Write",
            json!({ "file_path": format!("{ws}/subagent-write"), "content": "changed" }),
        ),
    ];
    let (url, requests) = scripted_model(main, subagent).await;

    // NATIVE_PROBE_PATH can supply sandbox dependencies (bubblewrap, socat) to this run only,
    // and NATIVE_PROBE_DROP_CAPS starts it without inherited capabilities, which bubblewrap
    // refuses when it is not setuid.
    let path = std::env::var("NATIVE_PROBE_PATH")
        .map(|extra| format!("export PATH='{extra}':\"$PATH\"\n"))
        .unwrap_or_default();
    let exec = if std::env::var("NATIVE_PROBE_DROP_CAPS").is_ok() {
        "exec setpriv --inh-caps=-all --ambient-caps=-all --"
    } else {
        "exec"
    };
    let launcher = root.path().join("claude");
    std::fs::write(
        &launcher,
        format!(
            "#!/bin/sh\n{path}export ANTHROPIC_BASE_URL={url} ANTHROPIC_API_KEY=sk-ant-local-test CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1\n{exec} '{binary}' \"$@\"\n"
        ),
    )
    .unwrap();
    std::fs::set_permissions(
        &launcher,
        std::os::unix::fs::PermissionsExt::from_mode(0o755),
    )
    .unwrap();
    let config = Config {
        claude_bin: launcher.to_string_lossy().into_owned(),
        ..common::config(root.path())
    };
    std::fs::create_dir_all(config.home.join(".claude")).unwrap();
    std::fs::create_dir(root.path().join("inbox")).unwrap();

    // As `chat_plan` renders a plan turn of a YOLO agent.
    let plan = json!({
        "provider": "claude",
        "execution": { "messageId": "plan", "text": "Plan the change.", "attachments": [] },
        "instructions": "Follow the task scope",
        "inputDirectory": root.path().join("inbox"),
        "output": root.path().join("result.md"),
        "cwd": workspace,
        "model": "sonnet",
        "reasoning": "high",
        "planning": true,
        "sandbox": "read-only",
        "writableRoots": [workspace],
        "claudeMcps": { "mcpServers": {} },
        "claudePlanTools": [],
    });
    let (tx, mut rx) = mpsc::channel(256);
    let events = tokio::spawn(async move {
        let mut events = Vec::new();
        while let Some(event) = rx.recv().await {
            events.push(event);
        }
        events
    });
    let result = tokio::time::timeout(
        Duration::from_secs(240),
        claude_process::run(&config, plan, tx, CancellationToken::new()),
    )
    .await
    .expect("native plan turn timed out");
    let events = events.await.unwrap();

    let home = config.home.clone();
    let effects = json!({
        "bash-write": workspace.join("bash-write").exists(),
        "unsandboxed": workspace.join("unsandboxed").exists(),
        "write-tool": workspace.join("write-tool").exists(),
        "edited": std::fs::read_to_string(workspace.join("main.rs")).unwrap() != "fn main() {}\n",
        "subagent-bash": workspace.join("subagent-bash").exists(),
        "subagent-write": workspace.join("subagent-write").exists(),
        "home-write": home.join("home-write").exists(),
        "loopback-connections": hits.load(Ordering::SeqCst),
    });
    let requests = requests.lock().unwrap().clone();
    report(
        "claude-plan-turn",
        &json!({
            "binary": binary,
            "result": result.as_ref().map(|()| "completed").map_err(|error| error.message.clone()),
            "effects": effects,
            "toolCalls": transcript(&requests),
            "proposedPlan": events.iter().any(|event| event["item"]["type"] == "proposed_plan"),
            "requests": requests.len(),
            "requestBodies": if std::env::var("NATIVE_PROBE_DUMP").is_ok() { json!(requests) } else { Value::Null },
        }),
    );

    result.unwrap();
    for (effect, happened) in effects.as_object().unwrap() {
        assert!(
            happened == false || happened == 0,
            "{effect} happened during a plan turn"
        );
    }
    assert!(
        transcript(&requests)
            .iter()
            .any(|call| call["id"] == tool_id("sub", 0)),
        "the subagent did not run, so its effects were not exercised"
    );
    // The proxy refuses the unlisted host (403); any other status means it let the
    // request through, as a command's own `allowed_domains` could.
    for call in transcript(&requests) {
        let output = call["content"].to_string();
        for status in output.split("proxy status ").skip(1) {
            assert!(status.starts_with("403"), "{}: {output}", call["id"]);
        }
    }
}

/// One Responses API turn streamed as Server-Sent Events.
fn response_events(items: Vec<Value>) -> String {
    let id = format!("resp_{}", uuid::Uuid::new_v4().simple());
    let mut events = vec![json!({ "type": "response.created", "response": { "id": id } })];
    events.extend(
        items
            .into_iter()
            .map(|item| json!({ "type": "response.output_item.done", "item": item })),
    );
    events.push(json!({
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
    }));
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

fn function_outputs(request: &Value) -> Vec<Value> {
    request["input"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|item| {
            matches!(
                item["type"].as_str(),
                Some("function_call_output" | "custom_tool_call_output")
            )
        })
        .cloned()
        .collect()
}

/// What the scripted Responses API model does at one step of a turn.
#[derive(Clone)]
enum Act {
    /// Runs a shell command through Codex's `exec_command`, from its JavaScript `exec` tool.
    Shell(String),
    /// Starts a sub-agent with this task.
    Spawn(String),
    /// Runs a shell command that asks to leave the sandbox.
    Escalated(String),
    /// Waits inside `exec`, letting a sub-agent work.
    Pause(u64),
}

fn codex_call(act: &Act, step: usize) -> Value {
    let call_id = format!("call_probe_{step}_{}", uuid::Uuid::new_v4().simple());
    let exec = |source: String| {
        json!({
            "type": "custom_tool_call",
            "id": format!("ctc_{}", uuid::Uuid::new_v4().simple()),
            "call_id": call_id,
            "namespace": "functions",
            "name": "exec",
            "input": source,
        })
    };
    match act {
        Act::Shell(command) => exec(format!(
            "const result = await tools.exec_command({{ cmd: {} }});\ntext(result);",
            Value::from(command.as_str())
        )),
        Act::Escalated(command) => exec(format!(
            "const result = await tools.exec_command({{ cmd: {}, sandbox_permissions: \"require_escalated\", justification: \"Probe\" }});\ntext(result);",
            Value::from(command.as_str())
        )),
        Act::Pause(ms) => exec(format!(
            "await new Promise((resolve) => setTimeout(resolve, {ms}));\ntext('paused');"
        )),
        Act::Spawn(task) => json!({
            "type": "function_call",
            "id": format!("fc_{}", uuid::Uuid::new_v4().simple()),
            "call_id": call_id,
            "namespace": "collaboration",
            "name": "spawn_agent",
            "arguments": json!({ "message": task, "task_name": "probe", "fork_turns": "none" }).to_string(),
        }),
    }
}

/// A local Responses API that plays `main` to the conversation's current turn and
/// `subagent` to any sub-agent it starts, one call per request, then ends each turn.
async fn scripted_responses(
    main: Arc<Mutex<Vec<Act>>>,
    subagent: Vec<Act>,
) -> (String, Arc<Mutex<Vec<Value>>>) {
    let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
    let recorded = requests.clone();
    let router = Router::new().route(
        "/v1/responses",
        post(move |body: Bytes| {
            let request: Value = serde_json::from_slice(&body).unwrap();
            recorded.lock().unwrap().push(request.clone());
            let input = request["input"].as_array().cloned().unwrap_or_default();
            let turn_start = input
                .iter()
                .rposition(|item| item["type"] == "message" && item["role"] == "user")
                .unwrap_or(0);
            let current = json!({ "input": input[turn_start..] });
            let step = function_outputs(&current).len();
            // A sub-agent's history holds its task but not the parent's request.
            let history = Value::from(input.clone()).to_string();
            let child = history.contains(SUBAGENT) && !history.contains(PLAN_REQUEST);
            let script = if child {
                subagent.clone()
            } else {
                main.lock().unwrap().clone()
            };
            let item = match script.get(step) {
                Some(act) => codex_call(act, step),
                None => json!({
                    "type": "message",
                    "role": "assistant",
                    "id": format!("msg_{}", uuid::Uuid::new_v4().simple()),
                    "content": [{ "type": "output_text", "text": "Probe finished." }],
                }),
            };
            async move {
                (
                    [("content-type", "text/event-stream")],
                    response_events(vec![item]),
                )
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    (format!("http://{address}/v1"), requests)
}

#[tokio::test]
#[ignore = "requires a native Codex binary"]
async fn native_codex_plan_turn_blocks_writes_network_and_subagent_effects() {
    let binary = std::env::var("CODEX_NATIVE_BIN").expect("set CODEX_NATIVE_BIN");
    let root = TempDir::new().unwrap();
    let workspace = root.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let ws = workspace.to_string_lossy().into_owned();
    let (probe, hits) = probe_listener().await;
    let loopback = format!("curl -sS -m 5 http://127.0.0.1:{probe}/; echo exit=$?");
    let main = Arc::new(Mutex::new(Vec::new()));
    let subagent = vec![
        Act::Shell(format!("touch {ws}/subagent-write; echo exit=$?")),
        Act::Shell(loopback.clone()),
    ];
    let (url, requests) = scripted_responses(main.clone(), subagent).await;

    let home = root.path().join("codex");
    std::fs::create_dir(&home).unwrap();
    std::fs::write(
        home.join("config.toml"),
        format!(
            "model = \"gpt-6-astra\"\nmodel_provider = \"local\"\n\n[model_providers.local]\nname = \"local\"\nbase_url = \"{url}\"\nwire_api = \"responses\"\nenv_key = \"LOCAL_PROBE_KEY\"\n"
        ),
    )
    .unwrap();
    let exec = if std::env::var("NATIVE_PROBE_DROP_CAPS").is_ok() {
        "exec setpriv --inh-caps=-all --ambient-caps=-all --"
    } else {
        "exec"
    };
    // NATIVE_PROBE_PRIVATE_TMP runs Codex with its own /tmp, where its sandbox can bind
    // its sockets, on a node whose /tmp is a bind mount from a directory it cannot reach.
    let exec = if std::env::var("NATIVE_PROBE_PRIVATE_TMP").is_ok() {
        format!("{exec} bwrap --dev-bind / / --tmpfs /tmp --")
    } else {
        exec.to_owned()
    };
    let launcher = root.path().join("codex-launcher");
    std::fs::write(
        &launcher,
        format!("#!/bin/sh\nexport LOCAL_PROBE_KEY=local-test\n{exec} '{binary}' \"$@\"\n"),
    )
    .unwrap();
    std::fs::set_permissions(
        &launcher,
        std::os::unix::fs::PermissionsExt::from_mode(0o755),
    )
    .unwrap();
    let config = Config {
        codex_bin: launcher.to_string_lossy().into_owned(),
        ..common::config(root.path())
    };
    std::fs::create_dir(root.path().join("inbox")).unwrap();
    std::fs::create_dir_all(&config.home).unwrap();

    let run = |plan: Value| {
        let (config, home) = (config.clone(), home.clone());
        async move {
            let (tx, mut rx) = mpsc::channel(256);
            let events = tokio::spawn(async move {
                let mut events = Vec::new();
                while let Some(event) = rx.recv().await {
                    events.push(event);
                }
                events
            });
            let result = tokio::time::timeout(
                Duration::from_secs(180),
                cairn_installation::chat_process::run(
                    &config,
                    &home,
                    plan,
                    tx,
                    CancellationToken::new(),
                ),
            )
            .await
            .expect("native Codex turn timed out");
            (result.map_err(|error| error.message), events.await.unwrap())
        }
    };
    // As `chat_plan` renders the turns of a YOLO agent's conversation: an ordinary turn,
    // a plan turn on the same thread, then the turn that implements the plan.
    let turn = |id: &str, text: &str| {
        let mut plan = json!({
            "execution": { "messageId": id, "text": text, "recovery": false },
            "instructions": "Follow the task scope",
            "inputDirectory": root.path().join("inbox"),
            "output": root.path().join(format!("{id}.md")),
            "cwd": workspace,
            "model": "",
            "reasoning": "medium",
            "sandbox": "yolo",
            "writableRoots": [workspace],
            "args": [],
        });
        plan["execution"]["attachments"] = json!([]);
        plan
    };

    *main.lock().unwrap() = vec![Act::Shell(format!("touch {ws}/first-turn; echo exit=$?"))];
    let (first, events) = run(turn("first", "Start the work.")).await;
    let session = events
        .iter()
        .find(|event| event["type"] == "thread.started")
        .map_or(Value::Null, |event| event["thread_id"].clone());

    *main.lock().unwrap() = vec![
        Act::Shell(format!("touch {ws}/plan-write; echo exit=$?")),
        Act::Shell(loopback.clone()),
        Act::Shell("echo HOME=$HOME; touch \"$HOME/home-write\"; echo home=$?".to_owned()),
        Act::Escalated(format!("touch {ws}/escalated; echo exit=$?")),
        Act::Spawn(format!("{SUBAGENT}: change the workspace")),
        Act::Pause(8000),
    ];
    let mut planning = turn("plan", PLAN_REQUEST);
    planning["planning"] = true.into();
    planning["sandbox"] = "read-only".into();
    planning["sessionId"] = session.clone();
    let plan_start = requests.lock().unwrap().len();
    let (planned, _) = run(planning).await;
    let plan_end = requests.lock().unwrap().len();

    *main.lock().unwrap() = vec![Act::Shell(format!("touch {ws}/after-plan; echo exit=$?"))];
    let mut next = turn("implement", "Implement it.");
    next["afterPlan"] = true.into();
    next["sessionId"] = session;
    let (implemented, _) = run(next).await;

    let requests = requests.lock().unwrap().clone();
    let outputs = requests
        .iter()
        .flat_map(function_outputs)
        .map(|item| json!({ "callId": item["call_id"], "output": item["output"] }))
        .fold(Vec::<Value>::new(), |mut unique, output| {
            if !unique.iter().any(|seen| seen["callId"] == output["callId"]) {
                unique.push(output);
            }
            unique
        });
    let plan_tools = requests[plan_start..plan_end]
        .iter()
        .flat_map(|request| request["input"].as_array().cloned().unwrap_or_default())
        .filter(|item| item["type"] == "additional_tools")
        .flat_map(|item| item["tools"].as_array().cloned().unwrap_or_default())
        .flat_map(|namespace| {
            let name = namespace["name"].as_str().unwrap_or_default().to_owned();
            namespace["tools"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .map(move |tool| format!("{name}.{}", tool["name"].as_str().unwrap_or_default()))
        })
        .collect::<std::collections::BTreeSet<_>>();
    let effects = json!({
        "plan-write": workspace.join("plan-write").exists(),
        "escalated": workspace.join("escalated").exists(),
        "subagent-write": workspace.join("subagent-write").exists(),
        "home-write": config.home.join("home-write").exists(),
        "loopback-connections": hits.load(Ordering::SeqCst),
    });
    let controls = json!({
        "first-turn": workspace.join("first-turn").exists(),
        "after-plan": workspace.join("after-plan").exists(),
    });
    report(
        "codex-plan-turn",
        &json!({
            "binary": binary,
            "turns": { "first": first, "plan": planned, "implement": implemented },
            "effects": effects,
            "controls": controls,
            "planTurnTools": plan_tools,
            "toolOutputs": outputs,
            "requestBodies": if std::env::var("NATIVE_PROBE_DUMP").is_ok() { json!(requests) } else { Value::Null },
        }),
    );

    first.unwrap();
    planned.unwrap();
    implemented.unwrap();
    for (effect, happened) in effects.as_object().unwrap() {
        assert!(
            happened == false || happened == 0,
            "{effect} happened during a plan turn"
        );
    }
    for (control, happened) in controls.as_object().unwrap() {
        assert_eq!(happened, true, "the {control} turn could not write");
    }
}
