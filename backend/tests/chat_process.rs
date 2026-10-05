mod common;

use leo_agent_manager::{chat_process, config::Config, skills::atomic_write};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};
use tempfile::TempDir;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

fn config(root: &TempDir) -> Config {
    Config {
        codex_bin: common::fixture("codex.mjs"),
        ..common::config(root.path())
    }
}

fn plan(root: &TempDir, text: &str) -> Value {
    json!({
        "execution": { "messageId": "original-message", "text": text, "recovery": false },
        "instructions": "Test instructions",
        "inputDirectory": root.path().join("inbox"),
        "output": root.path().join("result.md"),
        "cwd": root.path(),
        "model": "",
        "reasoning": "medium",
        "sandbox": "yolo",
        "writableRoots": [],
        "args": [],
    })
}

/// Creates the inbox and the Codex home of a plan, and returns that home.
fn prepare(root: &TempDir) -> PathBuf {
    std::fs::create_dir(root.path().join("inbox")).unwrap();
    let home = root.path().join("codex");
    std::fs::create_dir(&home).unwrap();
    home
}

/// Runs one turn to completion while discarding its events.
async fn run_quietly(config: &Config, home: &Path, plan: Value) {
    let (tx, mut rx) = mpsc::channel(64);
    let output = tokio::spawn(async move { while rx.recv().await.is_some() {} });
    chat_process::run(config, home, plan, tx, CancellationToken::new())
        .await
        .unwrap();
    output.await.unwrap();
}

struct Resident {
    socket: PathBuf,
    stop: CancellationToken,
    task: tokio::task::JoinHandle<leo_agent_manager::error::Result<()>>,
}

impl Resident {
    async fn start(config: Config, home: PathBuf, root: &TempDir) -> Self {
        std::fs::create_dir_all(&config.home).unwrap();
        std::fs::write(home.join("fixture-lifecycle.jsonl"), "").unwrap();
        let socket = root.path().join("resident/codex.sock");
        let stop = CancellationToken::new();
        let endpoint = socket.clone();
        let stopping = stop.clone();
        let task = tokio::spawn(async move {
            chat_process::resident::serve(&config, &home, &endpoint, stopping).await
        });
        common::eventually(
            Duration::from_secs(5),
            Duration::from_millis(10),
            async || {
                chat_process::resident::ready(&socket)
                    .await
                    .ok()
                    .filter(|ready| *ready)
            },
        )
        .await;
        Self { socket, stop, task }
    }

    async fn run(&self, plan: Value) {
        let (events, mut received) = mpsc::channel::<Value>(32);
        let task = tokio::spawn(async move {
            let mut completed = false;
            while let Some(event) = received.recv().await {
                completed |= event["type"] == "turn.completed";
            }
            assert!(completed);
        });
        chat_process::resident::run(&self.socket, plan, events, CancellationToken::new())
            .await
            .unwrap();
        task.await.unwrap();
    }

    async fn stop(self) {
        self.stop.cancel();
        self.task.await.unwrap().unwrap();
    }
}

fn lifecycle(home: &Path) -> Vec<Value> {
    std::fs::read_to_string(home.join("fixture-lifecycle.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

#[test]
fn manager_chat_plans_keep_mcp_configuration_out_of_process_arguments() {
    let root = TempDir::new().unwrap();
    let run = json!({ "snapshot": { "agent": { "provider": "codex" } } });
    let prepared = json!({ "output": root.path().join("output/result.md") });
    let mcp = json!({
        "args": ["-c", "mcp_servers.fixture={url=\"http://fixture\"}"],
        "codexConfig": { "fixture": { "url": "http://fixture" } },
    });
    let plan = leo_agent_manager::run_output::chat_plan(&run, &prepared, root.path(), &mcp, None);
    assert_eq!(
        plan["args"],
        json!([]),
        "MCP CLI duplication rejects a retained resident"
    );
    assert_eq!(plan["codexConfig"]["mcp_servers"], mcp["codexConfig"]);

    let claude = json!({ "snapshot": { "agent": { "provider": "claude" } } });
    let plan =
        leo_agent_manager::run_output::chat_plan(&claude, &prepared, root.path(), &mcp, None);
    assert_eq!(plan["args"], mcp["args"]);
}

#[tokio::test]
async fn cold_chats_refresh_and_remove_thread_mcp_configuration() {
    let root = TempDir::new().unwrap();
    let home = prepare(&root);
    let config = config(&root);
    for (index, token) in [Some("first-lease"), Some("second-lease"), None]
        .into_iter()
        .enumerate()
    {
        let mut plan = plan(&root, &format!("message {index}"));
        plan["execution"]["messageId"] = format!("message-{index}").into();
        let servers = token.map_or_else(|| json!({}), |token| json!({
            "fixture": { "url": "http://fixture", "http_headers": { "Authorization": token } }
        }));
        plan["codexConfig"] = json!({ "mcp_servers": servers });
        if index > 0 {
            plan["sessionId"] = "fixture-chat".into();
        }
        run_quietly(&config, &home, plan).await;
        assert_eq!(conversation(&home)["fixtureConfig"]["mcp_servers"], servers);
    }
}

#[tokio::test]
async fn resident_reuses_native_process_and_reloads_each_attempts_permissions() {
    let root = TempDir::new().unwrap();
    let home = prepare(&root);
    let service = Resident::start(config(&root), home.clone(), &root).await;
    for (index, token) in [Some("first-lease"), Some("second-lease"), None]
        .into_iter()
        .enumerate()
    {
        let mut plan = plan(&root, &format!("message {index}"));
        plan["execution"]["messageId"] = format!("message-{index}").into();
        let servers = token.map_or_else(|| json!({}), |token| json!({
            "fixture": { "url": "http://fixture", "http_headers": { "Authorization": token } }
        }));
        plan["codexConfig"] = json!({ "mcp_servers": servers });
        if index > 0 {
            plan["sessionId"] = "fixture-chat".into();
            plan["sandbox"] = "workspace-write".into();
        }
        service.run(plan).await;
        let saved = conversation(&home);
        assert_eq!(saved["turns"].as_array().unwrap().len(), index + 1);
        assert_eq!(saved["fixtureConfig"]["mcp_servers"], servers);
    }
    service.stop().await;
    let log = lifecycle(&home);
    assert_eq!(
        log.iter()
            .filter(|entry| entry["method"] == "initialize")
            .count(),
        1
    );
    assert_eq!(
        log.iter()
            .filter(|entry| entry["method"] == "thread/unsubscribe")
            .count(),
        3
    );
    assert!(log.iter().all(|entry| entry["pid"] == log[0]["pid"]));
    let pid = log[0]["pid"].as_i64().unwrap() as i32;
    assert_eq!(
        unsafe { libc::kill(pid, 0) },
        -1,
        "Service shutdown must reap its native process"
    );
}

#[tokio::test]
async fn lost_resident_client_retires_native_instead_of_keeping_unknown_turn() {
    let root = TempDir::new().unwrap();
    let home = prepare(&root);
    let service = Resident::start(config(&root), home.clone(), &root).await;
    let mut plan = plan(&root, "fixture:chat-hang");
    plan["codexConfig"] = json!({ "mcp_servers": {} });
    let (events, mut received) = mpsc::channel(32);
    let endpoint = service.socket.clone();
    let client = tokio::spawn(async move {
        chat_process::resident::run(&endpoint, plan, events, CancellationToken::new()).await
    });
    tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(event) = received.recv().await {
            if event["type"] == "turn.started" {
                break;
            }
        }
    })
    .await
    .unwrap();
    client.abort();
    let _ = client.await;
    assert!(
        tokio::time::timeout(Duration::from_secs(5), service.task)
            .await
            .unwrap()
            .unwrap()
            .is_err()
    );
    let log = lifecycle(&home);
    let pid = log[0]["pid"].as_i64().unwrap() as i32;
    assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
    assert!(!service.socket.exists());
}

#[tokio::test]
async fn resident_rejects_a_concurrent_attempt_without_queuing_it() {
    let root = TempDir::new().unwrap();
    let home = prepare(&root);
    let service = Resident::start(config(&root), home.clone(), &root).await;
    let mut plan = plan(&root, "fixture:chat-hang");
    plan["codexConfig"] = json!({ "mcp_servers": {} });
    let (events, mut received) = mpsc::channel(32);
    let endpoint = service.socket.clone();
    let first_plan = plan.clone();
    let client = tokio::spawn(async move {
        chat_process::resident::run(&endpoint, first_plan, events, CancellationToken::new()).await
    });
    tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(event) = received.recv().await {
            if event["type"] == "turn.started" {
                break;
            }
        }
    })
    .await
    .unwrap();
    let (events, _) = mpsc::channel(32);
    let error = tokio::time::timeout(
        Duration::from_secs(1),
        chat_process::resident::run(&service.socket, plan, events, CancellationToken::new()),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(error.status, 409);
    client.abort();
    let _ = client.await;
    assert!(service.task.await.unwrap().is_err());
    let log = lifecycle(&home);
    assert_eq!(
        log.iter()
            .filter(|entry| entry["method"] == "turn/start")
            .count(),
        1
    );
}

#[tokio::test]
async fn stopping_resident_initialization_reaps_unresponsive_native_process() {
    let root = TempDir::new().unwrap();
    let home = prepare(&root);
    let config = config(&root);
    std::fs::create_dir_all(&config.home).unwrap();
    let control = home.join("fixture-initialize-wait");
    std::fs::write(&control, "").unwrap();
    let socket = root.path().join("resident/codex.sock");
    let endpoint = socket.clone();
    let stop = CancellationToken::new();
    let stopping = stop.clone();
    let service = tokio::spawn(async move {
        chat_process::resident::serve(&config, &home, &endpoint, stopping).await
    });
    let pid: i32 = common::eventually(
        Duration::from_secs(5),
        Duration::from_millis(10),
        async || std::fs::read_to_string(&control).ok()?.parse().ok(),
    )
    .await;
    stop.cancel();
    assert!(
        tokio::time::timeout(Duration::from_secs(5), service)
            .await
            .unwrap()
            .unwrap()
            .is_err()
    );
    assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
    assert!(!socket.exists());
}

#[tokio::test]
async fn failed_readiness_notification_reaps_initialized_native_process() {
    let root = TempDir::new().unwrap();
    let home = prepare(&root);
    let config = config(&root);
    std::fs::create_dir_all(&config.home).unwrap();
    std::fs::write(home.join("fixture-lifecycle.jsonl"), "").unwrap();
    let socket = root.path().join("resident/codex.sock");
    let result = chat_process::resident::serve_with_ready(
        &config,
        &home,
        &socket,
        CancellationToken::new(),
        || {
            Err(leo_agent_manager::error::Error::unavailable(
                "Owner pipe closed.",
            ))
        },
    )
    .await;
    assert_eq!(result.unwrap_err().message, "Owner pipe closed.");
    let log = lifecycle(&home);
    assert_eq!(
        log.iter()
            .filter(|entry| entry["method"] == "initialize")
            .count(),
        1
    );
    let pid = log[0]["pid"].as_i64().unwrap() as i32;
    assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
    assert!(!socket.exists());
}

/// The conversation the Codex fixture persisted in `home`.
fn conversation(home: &Path) -> Value {
    serde_json::from_slice(&std::fs::read(home.join("fixture-conversation.json")).unwrap()).unwrap()
}

/// Writes messages the running process reads from its inbox.
async fn deliver(root: &TempDir, messages: &Value) {
    atomic_write(
        &root.path().join("inbox/messages.json"),
        messages.to_string().as_bytes(),
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn model_and_reasoning_reach_codex_and_model_changes_resolve_the_new_default() {
    let root = TempDir::new().unwrap();
    let home = prepare(&root);
    let config = config(&root);
    for (index, (model, reasoning, expected)) in [
        ("fixture-deep", "ultra", "ultra"),
        ("fixture-fast", "", "low"),
    ]
    .into_iter()
    .enumerate()
    {
        let mut plan = plan(&root, "Check the model setting");
        plan["model"] = model.into();
        plan["reasoning"] = reasoning.into();
        plan["execution"]["messageId"] = format!("message-{index}").into();
        if index > 0 {
            plan["sessionId"] = "fixture-chat".into();
        }
        run_quietly(&config, &home, plan).await;
        let thread = conversation(&home);
        assert_eq!(thread["turns"][index]["model"], model);
        assert_eq!(thread["turns"][index]["effort"], expected);
    }
}

#[tokio::test]
async fn native_in_flight_question_accepts_answer_and_preserves_receipt() {
    let root = TempDir::new().unwrap();
    let home = prepare(&root);
    let config = config(&root);
    let plan = plan(&root, "fixture:question");
    let (tx, mut rx) = mpsc::channel(64);
    let task = tokio::spawn(async move {
        chat_process::run(&config, &home, plan, tx, CancellationToken::new()).await
    });
    let mut answer_receipts = 0;
    let mut completed = false;
    let mut question_id = String::new();
    tokio::time::timeout(Duration::from_secs(10), async {
        while let Some(event) = rx.recv().await {
            match event["type"].as_str().unwrap_or("") {
                "chat.question" => {
                    question_id = event["question"]["id"].as_str().unwrap().to_owned();
                    assert_eq!(event["question"]["blocking"], false);
                    let answer = json!([{
                        "id": "answer-message",
                        "questionId": question_id,
                        "answers": { "direction": ["Gradual rollout"] },
                        "text": "My answer",
                    }]);
                    deliver(&root, &answer).await;
                }
                "chat.delivered" if event["messageId"] == "answer-message" => answer_receipts += 1,
                "turn.completed" => completed = true,
                _ => {}
            }
        }
    })
    .await
    .unwrap();
    task.await.unwrap().unwrap();
    assert_eq!(answer_receipts, 1);
    assert!(completed);
    assert_eq!(question_id.len(), 64);
    assert!(root.path().join("result.md").exists());
}

#[tokio::test]
async fn replay_of_completed_turn_does_not_submit_the_instruction_again() {
    let root = TempDir::new().unwrap();
    let home = prepare(&root);
    let config = config(&root);
    let mut plan = plan(&root, "One instruction");
    for resume in [false, true] {
        if resume {
            plan["sessionId"] = "fixture-chat".into();
            plan["execution"]["recovery"] = true.into();
        }
        run_quietly(&config, &home, plan.clone()).await;
    }
    assert_eq!(conversation(&home)["turns"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn images_and_files_reach_start_and_steer_as_readable_inputs() {
    let root = TempDir::new().unwrap();
    let config = config(&root);
    let home = root.path().join("codex");
    std::fs::create_dir(&home).unwrap();
    let image = json!({ "id": "image-id", "name": "design.png", "kind": "image" });
    let document = json!({ "id": "file-id", "name": "notes.md", "kind": "file" });
    for attachment in [&image, &document] {
        let path = root
            .path()
            .join("inbox/attachments")
            .join(attachment["id"].as_str().unwrap());
        std::fs::create_dir_all(&path).unwrap();
        std::fs::write(
            path.join(attachment["name"].as_str().unwrap()),
            b"fixture content",
        )
        .unwrap();
    }
    let mut plan = plan(&root, "fixture:chat-hang");
    plan["execution"]["attachments"] = json!([image, document]);
    let (tx, mut rx) = mpsc::channel(64);
    let run_home = home.clone();
    let task = tokio::spawn(async move {
        chat_process::run(&config, &run_home, plan, tx, CancellationToken::new()).await
    });
    let steering = json!([{
        "id": "steer-image",
        "text": "finish now",
        "attachments": [image, document],
    }]);
    tokio::time::timeout(Duration::from_secs(10), async {
        while let Some(event) = rx.recv().await {
            if event["type"] == "chat.delivered" && event["messageId"] == "original-message" {
                deliver(&root, &steering).await;
            }
        }
    })
    .await
    .unwrap();
    task.await.unwrap().unwrap();
    let thread = conversation(&home);
    let users = thread["turns"][0]["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| i["type"] == "userMessage")
        .collect::<Vec<_>>();
    assert_eq!(users.len(), 2);
    for user in users {
        let input = user["content"].as_array().unwrap();
        let image = input.iter().find(|i| i["type"] == "localImage").unwrap();
        assert_eq!(
            std::fs::read(image["path"].as_str().unwrap()).unwrap(),
            b"fixture content"
        );
        assert!(
            input
                .iter()
                .any(|i| i["text"].as_str().is_some_and(|s| s.contains("notes.md")))
        );
    }
    let isolated = leo_agent_manager::attachments::input(
        "Review",
        &json!([image, document]),
        Path::new("/run/leo-chat"),
    );
    assert!(
        isolated
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["path"] == "/run/leo-chat/attachments/image-id/design.png")
    );
}
