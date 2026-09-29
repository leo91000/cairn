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
        setup_token: String::new(),
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
