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
            gh_bin: "unused".into(),
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
            &self.id,
            &output,
            &self.checkpoint,
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
                "id": format!("mcp-{failed}"), "type": "mcp_tool_call", "tool": "get_run", "server": "leo", "status": "completed",
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
