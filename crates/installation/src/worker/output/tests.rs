use super::*;
use crate::{
    config::{Config, MAIN_AGENT_ID},
    performance::Activity,
    provider::Provider,
    worker::checkpoint::RunCheckpoint,
};
use serde_json::json;
use std::sync::Arc;
use tempfile::TempDir;
use tokio::io::AsyncWriteExt;

struct Fixture {
    _root: TempDir,
    service: Arc<Service>,
    id: String,
    checkpoint: Checkpoint,
    activity: Activity,
    log_total: usize,
}

impl Fixture {
    async fn new() -> Self {
        let root = TempDir::new().unwrap();
        std::fs::create_dir(root.path().join("home")).unwrap();
        let service = Service::new(Config {
            data_dir: root.path().join("data"),
            home: root.path().join("home"),
            workspace_roots: vec![root.path().to_owned()],
            public_url: "http://localhost:4310".into(),
            host: "127.0.0.1".into(),
            port: 0,
            codex_bin: "unused".into(),
            claude_bin: "unused".into(),
            opencode_bin: "opencode".into(),
            opencode_go_usage_url: crate::accounts::opencode::USAGE_URL.into(),
            concurrency: 1,
            logger: false,
            worker_enabled: false,
            runner_url: String::new(),
        })
        .await
        .unwrap();
        let task = service
            .task(
                json!({
                    "name": "Tool activity test",
                    "prompt": "Fixture only",
                    "worktree": false,
                    "agentId": MAIN_AGENT_ID,
                }),
                None,
            )
            .await
            .unwrap();
        let run = service
            .enqueue(text(&task, "id"), "manual", None)
            .await
            .unwrap();
        let id = text(&run, "id").to_owned();
        let checkpoint = Checkpoint::new(
            service.store.clone(),
            id.clone(),
            RunCheckpoint::fresh(None),
        );
        let activity = Activity::new(&id, "fixture", "host", Provider::Codex);
        Self {
            _root: root,
            service,
            id,
            checkpoint,
            activity,
            log_total: 0,
        }
    }

    async fn output(&mut self, output: Output) {
        record(
            &self.service,
            &output,
            &self.checkpoint,
            false,
            &["sensitive-tool-credential".into()],
            &mut self.log_total,
            &mut self.activity,
        )
        .await
        .unwrap();
    }

    async fn event(&mut self, event: Value) {
        self.output(Output::new(false, event.to_string())).await;
    }

    async fn events(&self) -> Vec<Value> {
        let id = self.id.clone();
        self.service
            .store
            .read(move |db| db.events(&id, 0, 1000))
            .await
            .unwrap()
            .into_iter()
            .filter(|event| event["payload"]["item"].is_object() || event["type"] == "diagnostic")
            .collect()
    }
}

#[tokio::test]
async fn partial_guest_stdout_cannot_hide_controller_failure_after_exhausted_logs() {
    let mut fixture = Fixture::new().await;
    fixture.log_total = LOG_BUDGET;
    let mut stdout = b"{\"type\":\"item.completed\",\"item\":".to_vec();
    stdout.extend(run_output::controller_failure_line(
        "Guest output exceeded the run limit. sensitive-tool-credential",
    ));
    let (sender, mut receiver) = mpsc::channel(32);
    read_output(
        stdout.as_slice(),
        false,
        sender,
        vec!["sensitive-tool-credential".into()],
    )
    .await;
    while let Some(output) = receiver.recv().await {
        fixture.output(output).await;
    }
    let saved = fixture.checkpoint.snapshot().await;
    assert_eq!(
        saved.last_error(),
        "Guest output exceeded the run limit. [redacted]"
    );
    let id = fixture.id.clone();
    let events = fixture
        .service
        .store
        .read(move |db| db.events(&id, 0, 1000))
        .await
        .unwrap();
    let failures = events
        .iter()
        .filter(|event| event["type"] == "runner.failed")
        .collect::<Vec<_>>();
    assert_eq!(failures.len(), 1);
    assert!(
        !failures[0]
            .to_string()
            .contains("sensitive-tool-credential")
    );
}

#[tokio::test]
async fn controller_cleanup_does_not_replace_a_provider_failure_or_its_retry() {
    let mut fixture = Fixture::new().await;
    fixture
        .event(json!({
            "type": "turn.failed",
            "error": { "message": "Provider unavailable", "code": "server_error" },
        }))
        .await;
    fixture
        .event(json!({
            "type": "runner.failed",
            "error": { "message": "Controller cleanup failed" },
        }))
        .await;
    let saved = fixture.checkpoint.snapshot().await;
    assert_eq!(saved.last_error(), "Provider unavailable");
    assert_eq!(
        saved.retry_cause.unwrap().unwrap().kind,
        crate::run_retry::FailureKind::Server
    );
}

#[tokio::test]
async fn controller_failures_survive_exhausted_logs_without_provider_retry() {
    let mut fixture = Fixture::new().await;
    fixture.log_total = LOG_BUDGET;
    fixture
        .event(json!({
            "type": "runner.failed",
            "error": {
                "message": "Guest output exceeded the run limit. sensitive-tool-credential",
                "httpStatusCode": 500,
            },
        }))
        .await;

    let saved = fixture.checkpoint.snapshot().await;
    assert_eq!(
        saved.last_error(),
        "Guest output exceeded the run limit. [redacted]"
    );
    assert!(saved.retry_cause.as_ref().is_none_or(Option::is_none));
    assert!(!saved.completed());
    let id = fixture.id.clone();
    let events = fixture
        .service
        .store
        .read(move |db| db.events(&id, 0, 1000))
        .await
        .unwrap();
    let failures = events
        .iter()
        .filter(|event| event["type"] == "runner.failed")
        .collect::<Vec<_>>();
    assert_eq!(failures.len(), 1);
    assert!(
        !failures[0]
            .to_string()
            .contains("sensitive-tool-credential")
    );
}

#[tokio::test]
async fn native_retry_notices_survive_log_budget_without_claiming_the_final_failure() {
    let mut fixture = Fixture::new().await;
    fixture.log_total = LOG_BUDGET;
    fixture
        .event(json!({
            "type": "error",
            "willRetry": true,
            "error": {
                "message": "Reconnecting... 2/5 sensitive-tool-credential",
                "codexErrorInfo": "other",
            },
        }))
        .await;
    let saved = fixture.checkpoint.snapshot().await;
    assert!(saved.last_error().is_empty());
    assert!(saved.retry_cause.is_none());

    let id = fixture.id.clone();
    let events = fixture
        .service
        .store
        .read(move |db| db.events(&id, 0, 1000))
        .await
        .unwrap();
    let notice = events
        .iter()
        .find(|event| event["type"] == "error")
        .unwrap();
    assert_eq!(notice["payload"]["willRetry"], true);
    assert!(
        notice["text"]
            .as_str()
            .unwrap()
            .contains("Reconnecting... 2/5")
    );
    assert!(!notice.to_string().contains("sensitive-tool-credential"));

    fixture
        .event(json!({
            "type": "turn.failed",
            "error": {
                "message": "workspace routing discovery timed out",
                "codexErrorInfo": "other",
            },
        }))
        .await;
    let saved = fixture.checkpoint.snapshot().await;
    assert_eq!(saved.last_error(), "workspace routing discovery timed out");
    assert_eq!(
        saved.retry_cause.unwrap().unwrap().kind,
        crate::run_retry::FailureKind::Timeout,
    );
}

#[tokio::test]
async fn tool_calls_remain_recorded_after_diagnostic_budget() {
    let mut fixture = Fixture::new().await;
    fixture.event(json!({
        "type": "item.started",
        "item": { "id": "command", "type": "command_execution", "command": "pnpm check", "status": "in_progress" },
    })).await;
    while fixture.log_total < LOG_BUDGET {
        let remaining = LOG_BUDGET - fixture.log_total;
        fixture
            .output(Output::new(true, "x".repeat(remaining.min(8192))))
            .await;
    }
    assert_eq!(fixture.log_total, LOG_BUDGET);
    for id in ["command", "later"] {
        fixture.event(json!({
            "type": "item.completed",
            "item": { "id": id, "type": "command_execution", "command": "pnpm check", "status": "completed", "exit_code": 2, "duration_ms": 123, "aggregated_output": "Checks failed: invalid configuration." },
        })).await;
    }
    fixture
        .event(json!({
            "type": "item.completed",
            "item": { "id": "message", "type": "agent_message", "text": "Checks failed." },
        }))
        .await;
    let events = fixture.events().await;
    let tools: Vec<_> = events
        .iter()
        .filter(|e| e["payload"]["item"]["type"] == "command_execution")
        .collect();
    assert_eq!(
        tools.len(),
        3,
        "Both the completion and later calls must survive the budget"
    );
    for tool in &tools[1..] {
        assert_eq!(tool["payload"]["item"]["status"], "completed");
        assert_eq!(tool["payload"]["item"]["exit_code"], 2);
        assert_eq!(tool["payload"]["item"]["duration_ms"], 123);
        assert!(tool["payload"]["item"]["details_truncated"].is_null());
        assert_eq!(
            tool["payload"]["item"]["aggregated_output"],
            "Checks failed: invalid configuration."
        );
    }
    assert_eq!(events.last().unwrap()["text"], "Checks failed.");
    assert_eq!(
        events
            .iter()
            .filter(|e| e["type"] == "diagnostic")
            .map(|e| e["text"].as_str().unwrap().len())
            .sum::<usize>(),
        LOG_BUDGET
    );
}

#[tokio::test]
async fn many_small_tool_results_keep_all_details_without_a_cumulative_budget() {
    let mut fixture = Fixture::new().await;
    let output = "x".repeat(8192);
    for index in 0..80 {
        fixture
            .event(json!({
                "type": "item.completed",
                "item": {
                    "id": format!("command-{index}"),
                    "type": "command_execution",
                    "command": "pnpm check",
                    "status": "completed",
                    "exit_code": 0,
                    "aggregated_output": output,
                },
            }))
            .await;
    }
    let events = fixture.events().await;
    assert_eq!(events.len(), 80);
    for (index, event) in events.iter().enumerate() {
        let item = &event["payload"]["item"];
        assert_eq!(item["id"], format!("command-{index}"));
        assert_eq!(item["aggregated_output"], output);
        assert!(item["details_truncated"].is_null());
    }
    assert!(
        events
            .iter()
            .map(|event| serde_json::to_string(&event["payload"]).unwrap().len())
            .sum::<usize>()
            > LOG_BUDGET
    );
    assert_eq!(
        fixture.log_total, 0,
        "Tool calls must not consume the technical log budget"
    );
}

#[tokio::test]
async fn large_mcp_results_keep_their_success_or_error_and_small_results_keep_details() {
    let mut fixture = Fixture::new().await;
    for failed in [false, true] {
        fixture.event(json!({
            "type": "item.completed",
            "item": {
                "id": format!("mcp-{failed}"), "type": "mcp_tool_call", "tool": "get_run", "server": "cairn", "status": "completed",
                "arguments": { "access_token": "sensitive-tool-credential" },
                "result": { "isError": failed, "structuredContent": { "rows": "😀".repeat(100_000) }, "content": [{ "type": "text", "text": "sensitive-tool-credential" }] },
            },
        })).await;
    }
    let small = json!({
        "type": "item.completed",
        "item": { "id": "small", "type": "mcp_tool_call", "tool": "echo", "arguments": { "access_token": "private" }, "result": { "content": [{ "type": "text", "text": "OK" }] } },
    });
    fixture.event(small.clone()).await;
    let events = fixture.events().await;
    for (event, failed) in events[..2].iter().zip([false, true]) {
        let item = &event["payload"]["item"];
        assert_eq!(item["tool"], "get_run");
        assert_eq!(item["result"]["isError"], failed);
        assert_eq!(item["details_truncated"], true);
        assert!(serde_json::to_string(event).unwrap().len() < ITEM_DETAIL_LIMIT);
        assert!(
            !serde_json::to_string(event)
                .unwrap()
                .contains("sensitive-tool-credential")
        );
    }
    let mut expected = small;
    expected["item"]["arguments"]["access_token"] = "[redacted]".into();
    assert_eq!(events[2]["payload"], expected);
    assert!(
        fixture.log_total == 0,
        "Tool calls must not consume the technical log budget"
    );
}

#[tokio::test]
async fn transport_previews_do_not_cut_secrets_before_manager_redaction() {
    let mut fixture = Fixture::new().await;
    let boundary = |limit: usize| {
        format!(
            "{}sensitive-tool-credential{}",
            "x".repeat(limit - 9),
            "😀".repeat(10_000)
        )
    };
    for item in [
        json!({ "id": "command", "type": "command_execution", "status": "completed", "command": boundary(1024), "cwd": boundary(256), "error": { "message": boundary(512) } }),
        json!({ "id": "files", "type": "file_change", "changes": [{ "path": boundary(256), "kind": "update", "diff": "large" }] }),
        json!({ "id": "subagent", "type": "subagent", "status": "completed", "prompt": boundary(4096), "result": boundary(4096), "activity": boundary(1024) }),
    ] {
        let event = json!({ "type": "item.completed", "item": item });
        let transported = serde_json::from_slice(&run_output::event_line(&event)).unwrap();
        fixture.event(transported).await;
    }
    let events = fixture.events().await;
    assert_eq!(events.len(), 3);
    for event in events {
        assert!(
            !event.to_string().contains("sensitive"),
            "Secret fragment in transport preview"
        );
        assert_eq!(event["payload"]["item"]["details_truncated"], true);
    }
}

#[tokio::test]
async fn command_previews_redact_secrets_before_cutting_and_file_changes_stay_bounded() {
    let mut fixture = Fixture::new().await;
    fixture.event(json!({
        "type": "item.completed",
        "item": {
            "id": "long-command", "type": "command_execution", "exit_code": 0,
            "command": format!("{}sensitive-tool-credential{}", "x".repeat(1015), "😀".repeat(10_000)),
            "aggregated_output": "sensitive-tool-credential",
        },
    })).await;
    let changes: Vec<_> = (0..100)
        .map(|index| {
            json!({
                "path": format!("src/{index}.rs"), "kind": "update", "diff": "x".repeat(1000),
            })
        })
        .collect();
    fixture.event(json!({ "type": "item.completed", "item": { "id": "files", "type": "file_change", "changes": changes } })).await;
    let events = fixture.events().await;
    let command = events[0]["payload"]["item"]["command"].as_str().unwrap();
    assert!(command.len() < 1030);
    assert!(!command.contains("sensitive"));
    assert_eq!(events[0]["payload"]["item"]["exit_code"], 0);
    let files = &events[1]["payload"]["item"];
    assert_eq!(files["changes"].as_array().unwrap().len(), 16);
    assert_eq!(files["changes"][0]["path"], "src/0.rs");
    assert_eq!(files["details_truncated"], true);
    assert!(serde_json::to_string(files).unwrap().len() < ITEM_DETAIL_LIMIT);
}

#[tokio::test]
async fn oversized_tool_frames_are_recorded_without_losing_the_following_event() {
    let mut fixture = Fixture::new().await;
    let large = json!({
        "type": "item.completed",
        "item": {
            "id": "large-frame", "type": "mcp_tool_call", "tool": "get_run", "status": "completed",
            "result": { "isError": true, "content": [{ "type": "text", "text": "x".repeat(LINE_LIMIT + 4096) }] },
        },
    });
    let following = json!({
        "type": "item.completed",
        "item": { "id": "following", "type": "command_execution", "command": "pnpm check", "exit_code": 0 },
    });
    let large_command = json!({
        "type": "item.completed",
        "item": {
            "id": "large-command", "type": "command_execution", "exit_code": 42,
            "command": format!("{}sensitive-tool-credential{}", "x".repeat(1015), "😀".repeat(10_000)),
            "aggregated_output": "x".repeat(LINE_LIMIT + 4096),
        },
    });
    let (reader, mut writer) = tokio::io::duplex(8192);
    let (sender, mut receiver) = mpsc::channel(32);
    let reading = tokio::spawn(read_output(
        reader,
        false,
        sender,
        vec!["sensitive-tool-credential".into()],
    ));
    let writing = tokio::spawn(async move {
        writer
            .write_all(large.to_string().as_bytes())
            .await
            .unwrap();
        writer.write_all(b"\n").await.unwrap();
        writer
            .write_all(large_command.to_string().as_bytes())
            .await
            .unwrap();
        writer.write_all(b"\n").await.unwrap();
        writer
            .write_all(following.to_string().as_bytes())
            .await
            .unwrap();
    });
    while let Some(output) = receiver.recv().await {
        fixture.output(output).await;
    }
    writing.await.unwrap();
    reading.await.unwrap();
    let events = fixture.events().await;
    assert_eq!(
        events.len(),
        3,
        "An oversized JSON frame must remain a tool call"
    );
    assert_eq!(events[0]["payload"]["item"]["id"], "large-frame");
    assert_eq!(events[0]["payload"]["item"]["result"]["isError"], true);
    assert_eq!(events[0]["payload"]["item"]["details_truncated"], true);
    assert_eq!(events[1]["payload"]["item"]["id"], "large-command");
    assert_eq!(events[1]["payload"]["item"]["exit_code"], 42);
    assert!(
        !events[1]["payload"]["item"]["command"]
            .as_str()
            .unwrap()
            .contains("sensitive")
    );
    assert_eq!(events[2]["payload"]["item"]["id"], "following");
    assert_eq!(fixture.log_total, 0);
}

async fn question_chat(fixture: &Fixture) -> String {
    let mut chat = fixture.service.chat_create(json!({})).await.unwrap();
    chat["runId"] = fixture.id.clone().into();
    let id = text(&chat, "id").to_owned();
    fixture.service.store.put("chats", chat).await.unwrap();
    id
}

fn question_event(id: &str, blocking: bool) -> Value {
    json!({
        "type": "chat.question",
        "question": {
            "id": id,
            "blocking": blocking,
            "fields": [{ "id": "scope", "title": "Which scope?" }],
        },
    })
}

#[tokio::test]
async fn closed_questions_leave_the_pending_badge_without_becoming_answers() {
    for blocking in [false, true] {
        let mut fixture = Fixture::new().await;
        let chat = question_chat(&fixture).await;
        let question = "a".repeat(64);
        fixture.event(question_event(&question, blocking)).await;
        assert_eq!(
            fixture.service.chat_detail(&chat).await.unwrap()["pendingQuestions"],
            1
        );
        fixture
            .event(json!({ "type": "chat.question.closed", "questionId": question }))
            .await;
        // A duplicate event must not reopen a withdrawn question.
        fixture.event(question_event(&question, blocking)).await;
        let detail = fixture.service.chat_detail(&chat).await.unwrap();
        assert_eq!(detail["pendingQuestions"], 0);
        assert_eq!(detail["questions"][0]["status"], "cancelled");
        assert_eq!(detail["questions"][0]["blocking"], false);
        assert_eq!(detail["messages"], json!([]));
        assert!(
            fixture
                .service
                .question_answer(
                    &chat,
                    &question,
                    json!({
                        "id": crate::config::id(), "answers": { "scope": ["Everything"] },
                    })
                )
                .await
                .is_err()
        );
    }
}

#[tokio::test]
async fn a_completed_outcome_retires_questions_but_input_needed_and_old_reports_do_not() {
    for (status, message, pending) in [
        ("completed", "current", 0),
        ("needs_input", "current", 1),
        ("blocked", "current", 1),
        ("completed", "previous", 1),
    ] {
        let mut fixture = Fixture::new().await;
        let chat = question_chat(&fixture).await;
        fixture
            .service
            .store
            .patch_run(
                &fixture.id,
                json!({
                    "chatExecution": { "messageId": "current" },
                    "outcome": { "status": status, "messageId": message },
                }),
            )
            .await
            .unwrap();
        fixture.event(question_event(&"a".repeat(64), false)).await;
        fixture.event(json!({ "type": "turn.completed" })).await;
        assert_eq!(
            fixture.service.chat_detail(&chat).await.unwrap()["pendingQuestions"],
            pending
        );
    }
}

#[tokio::test]
async fn releasing_a_session_preserves_answers_in_delivery() {
    let mut fixture = Fixture::new().await;
    let chat = question_chat(&fixture).await;
    let pending = "a".repeat(64);
    let answering = "b".repeat(64);
    fixture
        .service
        .chat_send(&chat, json!({ "id": crate::config::id(), "text": "Start" }))
        .await
        .unwrap();
    fixture.event(question_event(&pending, true)).await;
    fixture.event(question_event(&answering, true)).await;
    let answer = crate::config::id();
    fixture
        .service
        .question_answer(
            &chat,
            &answering,
            json!({
                "id": answer, "answers": { "scope": ["Keep this answer"] },
            }),
        )
        .await
        .unwrap();
    assert!(
        fixture
            .service
            .question_cancel(&chat, &answering)
            .await
            .is_err()
    );
    fixture
        .event(json!({ "type": "chat.question.closed" }))
        .await;
    let detail = fixture.service.chat_detail(&chat).await.unwrap();
    assert_eq!(detail["pendingQuestions"], 0);
    let questions = detail["questions"].as_array().unwrap();
    assert_eq!(
        questions.iter().find(|q| q["id"] == pending).unwrap()["status"],
        "cancelled"
    );
    assert_eq!(
        questions.iter().find(|q| q["id"] == answering).unwrap()["status"],
        "answering"
    );
    fixture
        .service
        .chat_acknowledge(&fixture.id, &answer)
        .await
        .unwrap();
    let detail = fixture.service.chat_detail(&chat).await.unwrap();
    assert_eq!(
        detail["questions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|q| q["id"] == answering)
            .unwrap()["status"],
        "answered"
    );
}
