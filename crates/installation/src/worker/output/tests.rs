use super::*;
use crate::run_fixture::DbRunFixture;
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
            .task_fixture(
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

    async fn bind_progress(&self) {
        self.service
            .store
            .update_run_boundary(&self.id, json!({ "missionTurnId": "progress-turn" }))
            .await
            .unwrap();
        self.checkpoint
            .update(|saved| {
                saved.runner_id = Some(Some("progress-attempt".into()));
                saved.progress = Some(crate::worker::progress::Progress::launched(
                    "progress-execution".into(),
                    Some("progress-attempt".into()),
                    None,
                    Some("progress-turn".into()),
                    1,
                ));
            })
            .await
            .unwrap();
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
            .read(move |db| db.events_fixture(&id, 0, 1000))
            .await
            .unwrap()
            .into_iter()
            .filter(|event| event["payload"]["item"].is_object() || event["type"] == "diagnostic")
            .collect()
    }
}

#[tokio::test]
async fn runtime_disk_notice_preserves_publication_metadata_and_redacts_all_payloads() {
    let mut fixture = Fixture::new().await;
    let storage = json!({ "mode": "on-demand", "grantId": "retained-grant", "dirtyBytes": 123,
        "published": { "generation": 7, "backupId": "retained-backup" }, "policy": { "cacheMiB": 64 } });
    fixture
        .service
        .store
        .update_run_boundary(&fixture.id, json!({ "storage": storage }))
        .await
        .unwrap();
    fixture
        .event(
            json!({ "type": "runtime.disk_notice", "text": "Pressure sensitive-tool-credential",
        "capacity": { "allowanceBytes": 34359738368u64, "note": "sensitive-tool-credential" } }),
        )
        .await;
    let run = fixture.service.store.run(&fixture.id).await.unwrap();
    let expected = json!({ "allowanceBytes": 34359738368u64, "note": "[redacted]" });
    assert_eq!(run["storage"], storage);
    assert_eq!(run["diskCapacity"], expected);
    // Ordinary storage polling replaces its own authoritative document.
    fixture
        .service
        .store
        .update_run_boundary(
            &fixture.id,
            json!({ "storage": { "mode": "on-demand", "dirtyBytes": 0 } }),
        )
        .await
        .unwrap();
    assert_eq!(
        fixture.service.store.run(&fixture.id).await.unwrap()["diskCapacity"],
        expected
    );
    let id = fixture.id.clone();
    let events = fixture
        .service
        .store
        .read(move |db| db.events_fixture(&id, 0, 100))
        .await
        .unwrap();
    assert!(events.iter().any(|event| event["type"] == "status"));
    assert!(
        !serde_json::to_string(&events)
            .unwrap()
            .contains("sensitive-tool-credential")
    );
}

#[tokio::test]
async fn speed_checks_before_a_turn_become_conversation_notices() {
    let mut fixture = Fixture::new().await;
    let standard = json!({ "requested": "priority", "applied": "standard" });
    let fast = json!({ "requested": "priority", "confirmed": true });
    let unconfirmed = json!({ "requested": "priority", "unconfirmed": true });
    // Every outcome is stated once per message: a retry repeating it adds nothing, a later
    // attempt getting the speed after all says so, and another message starts over.
    for (message, speed) in [
        ("first", &fast),
        ("second", &standard),
        ("second", &standard),
        ("second", &fast),
        ("third", &unconfirmed),
    ] {
        fixture
            .event(json!({ "type": "turn.speed", "messageId": message, "speed": speed }))
            .await;
    }
    let id = fixture.id.clone();
    let events = fixture
        .service
        .store
        .read(move |db| db.events_fixture(&id, 0, 100))
        .await
        .unwrap();
    let notices = events
        .iter()
        .filter(|event| event["type"] == "status" && event["payload"]["speed"].is_object())
        .map(|event| {
            (
                event["payload"]["messageId"].clone(),
                event["text"].as_str().unwrap().to_owned(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(notices.len(), 4, "{events:#?}");
    assert_eq!(notices[0].0, "first");
    assert!(notices[0].1.contains("Codex lists the selected speed"));
    assert!(notices[0].1.contains("asks for it"));
    assert_eq!(notices[1].0, "second");
    assert!(notices[1].1.contains("runs at Standard speed"));
    assert_eq!(notices[2].0, "second");
    assert!(notices[2].1.contains("now lists the selected speed"));
    assert_eq!(notices[3].0, "third");
    assert!(notices[3].1.contains("may run at Standard speed"));
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
        .read(move |db| db.events_fixture(&id, 0, 1000))
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
async fn interrupted_output_after_completion_records_one_safe_diagnostic() {
    let mut fixture = Fixture::new().await;
    fixture
        .event(json!({ "type": "turn.completed", "usage": {} }))
        .await;
    fixture
        .event(
            json!({ "type": "runner.output_interrupted", "message": "sensitive-tool-credential" }),
        )
        .await;
    fixture
        .event(json!({ "type": "runner.output_interrupted" }))
        .await;
    let saved = fixture.checkpoint.snapshot().await;
    assert!(saved.completed());
    assert_eq!(saved.output_interrupted, Some(true));
    let id = fixture.id.clone();
    let events = fixture
        .service
        .store
        .read(move |db| db.events_fixture(&id, 0, 1000))
        .await
        .unwrap();
    let notices = events
        .iter()
        .filter(|event| text(event, "text").contains("Execution output was interrupted"))
        .collect::<Vec<_>>();
    assert_eq!(notices.len(), 1);
    assert!(!notices[0].to_string().contains("sensitive-tool-credential"));
}

#[tokio::test]
async fn controller_cleanup_does_not_replace_a_provider_failure_or_its_retry() {
    let mut fixture = Fixture::new().await;
    fixture
        .checkpoint
        .update(|saved| saved.prepared = Some(Some(json!({"backend":"native","isolated":true}))))
        .await
        .unwrap();
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
    fixture
        .event(json!({"type":"runner.output_interrupted"}))
        .await;
    let saved = fixture.checkpoint.snapshot().await;
    assert!(
        saved.output_error().is_none(),
        "confirmed provider failure retains its separate retry policy"
    );
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
        .read(move |db| db.events_fixture(&id, 0, 1000))
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
        .read(move |db| db.events_fixture(&id, 0, 1000))
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
async fn engine_reports_survive_log_budget_redacted_and_only_a_reported_limit_exhausts() {
    let mut fixture = Fixture::new().await;
    fixture.log_total = LOG_BUDGET;
    for event in [
        json!({ "type": "engine.retry", "attempt": 1, "maxRetries": 10, "delayMs": 500, "reason": "overloaded" }),
        json!({ "type": "provider.limit", "status": "rejected", "window": "five_hour", "blocked": true }),
        json!({ "type": "engine.notice", "level": "warning", "message": "Hook printed sensitive-tool-credential" }),
        // A long notice is cut once redacted: a secret across the cut leaves no part behind.
        json!({
            "type": "engine.notice",
            "level": "warning",
            "message": format!("{}sensitive-tool-credential", "x".repeat(3_995)),
        }),
        // Notices the conversation does not show stay within the run log's budget.
        json!({ "type": "engine.notice", "level": "info", "message": "Tool Bash running for 30s" }),
    ] {
        fixture.event(event).await;
    }
    // None of them fails the attempt: the engine retries, and the turn reports the limit.
    let saved = fixture.checkpoint.snapshot().await;
    assert!(saved.last_error().is_empty());
    assert!(saved.retry_cause.is_none());

    let id = fixture.id.clone();
    let events = fixture
        .service
        .store
        .read(move |db| db.events_fixture(&id, 0, 1000))
        .await
        .unwrap();
    // The run's own queued status precedes them.
    let events: Vec<_> = events
        .into_iter()
        .filter(|event| event["type"] != "status")
        .collect();
    let kinds: Vec<_> = events.iter().map(|event| text(event, "type")).collect();
    assert_eq!(
        kinds,
        [
            "engine.retry",
            "provider.limit",
            "engine.notice",
            "engine.notice"
        ]
    );
    assert_eq!(events[0]["payload"]["attempt"], 1);
    assert!(!events[2].to_string().contains("sensitive-tool-credential"));
    assert_eq!(events[2]["payload"]["message"], "Hook printed [redacted]");
    let long = text(&events[3]["payload"], "message");
    // It is cut before the marker it would split, and says it was cut.
    assert!(long.chars().count() <= 4_000);
    assert!(long.ends_with("x…"), "{}", &long[3_980..]);
    assert!(!events[3].to_string().contains("sensitive"));

    let limit = json!({
        "type": "turn.failed",
        "error": { "message": "Your allowance is used up.", "code": "usage_limit_reached" },
    });
    assert!(crate::run_output::exhausted(&limit));
    assert!(
        crate::run_retry::Cause::classify(&limit["error"]).is_none(),
        "a usage limit waits for an account, not a retry"
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
        json!({
            "id": "command",
            "type": "command_execution",
            "status": "completed",
            "command": boundary(1024),
            "cwd": boundary(256),
            "error": { "message": boundary(512) },
        }),
        json!({
            "id": "files",
            "type": "file_change",
            "changes": [{ "path": boundary(256), "kind": "update", "diff": "large" }],
        }),
        json!({
            "id": "subagent",
            "type": "subagent",
            "status": "completed",
            "prompt": boundary(4096),
            "result": boundary(4096),
            "activity": boundary(1024),
        }),
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
    fixture
        .service
        .store
        .save_conversation_boundary(chat)
        .await
        .unwrap();
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
async fn side_answers_are_stored_redacted() {
    let mut fixture = Fixture::new().await;
    let chat = question_chat(&fixture).await;
    let (run, key) = (fixture.id.clone(), format!("chat-side-question:{chat}:"));
    let (answered, failed) = (
        "a1b2c3d4-0000-4000-8000-000000000001",
        "a1b2c3d4-0000-4000-8000-000000000002",
    );
    for id in [answered, failed] {
        let question = json!({
            "id": id,
            "chatId": chat,
            "runId": run,
            "text": "What was printed?",
            "status": "pending",
            "createdAt": 1,
        });
        // Asked during the run's current turn, which is still running.
        let open = json!({ "runId": run, "turn": "", "status": "pending" });
        let (key, open_key) = (
            format!("{key}{id}"),
            format!("chat-side-question-open:{chat}:{id}"),
        );
        fixture
            .service
            .store
            .transaction(move |db| {
                db.set(&key, &question, None)?;
                db.set(&open_key, &open, None)
            })
            .await
            .unwrap();
    }
    fixture
        .service
        .store
        .change_run(
            &run,
            crate::runs::RunChanges {
                status: Some(cairn_contracts::RunStatus::Running),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    fixture
        .event(json!({
            "type": "chat.side.answered",
            "sideQuestionId": answered,
            "text": "It printed sensitive-tool-credential.",
        }))
        .await;
    fixture
        .event(json!({
            "type": "chat.side.failed",
            "sideQuestionId": failed,
            "error": "Failed near sensitive-tool-credential",
        }))
        .await;

    let stored = fixture
        .service
        .store
        .read(move |db| db.keys(&key))
        .await
        .unwrap();
    let stored = serde_json::to_string(&stored).unwrap();
    assert!(stored.contains("\"status\":\"answered\""), "{stored}");
    assert!(stored.contains("\"status\":\"failed\""), "{stored}");
    assert!(!stored.contains("sensitive-tool-credential"), "{stored}");
}

#[tokio::test]
async fn common_questions_survive_native_closures_without_becoming_answers() {
    for blocking in [false, true] {
        let mut fixture = Fixture::new().await;
        let chat = question_chat(&fixture).await;
        let question = "a".repeat(64);
        fixture.event(question_event(&question, blocking)).await;
        assert_eq!(
            fixture.service.chat_detail(&chat).await.unwrap()["pendingQuestions"],
            0
        );
        fixture
            .service
            .question_receive(
                &fixture.id,
                question_event(&question, false)["question"].clone(),
            )
            .await
            .unwrap();
        assert_eq!(
            fixture.service.chat_detail(&chat).await.unwrap()["pendingQuestions"],
            1
        );
        fixture
            .event(json!({ "type": "chat.question.closed", "questionId": question }))
            .await;
        // Native events cannot replace or retire a common questionnaire.
        fixture.event(question_event(&question, blocking)).await;
        let detail = fixture.service.chat_detail(&chat).await.unwrap();
        assert_eq!(detail["pendingQuestions"], 1);
        assert_eq!(detail["questions"][0]["status"], "pending");
        assert_eq!(detail["questions"][0]["blocking"], false);
        assert_eq!(detail["messages"], json!([]));
        fixture
            .service
            .question_cancel(&chat, &question)
            .await
            .unwrap();
        assert!(
            fixture
                .service
                .question_answer(
                    &chat,
                    &question,
                    json!({
                        "id": crate::config::id(),
                        "answers": {
                            "scope": { "selected": [], "comment": "Everything" },
                        },
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
            .update_run_boundary(
                &fixture.id,
                json!({
                    "chatExecution": { "messageId": "current" },
                    "outcome": { "status": status, "messageId": message },
                }),
            )
            .await
            .unwrap();
        fixture
            .service
            .question_receive(
                &fixture.id,
                question_event(&"a".repeat(64), false)["question"].clone(),
            )
            .await
            .unwrap();
        fixture.event(json!({ "type": "turn.completed" })).await;
        assert_eq!(
            fixture.service.chat_detail(&chat).await.unwrap()["pendingQuestions"],
            pending
        );
    }
}

#[tokio::test]
async fn releasing_a_session_preserves_common_questions_and_answers_in_delivery() {
    let mut fixture = Fixture::new().await;
    let chat = question_chat(&fixture).await;
    let pending = "a".repeat(64);
    let answering = "b".repeat(64);
    fixture
        .service
        .chat_send(&chat, json!({ "id": crate::config::id(), "text": "Start" }))
        .await
        .unwrap();
    for question in [&pending, &answering] {
        fixture
            .service
            .question_receive(
                &fixture.id,
                question_event(question, false)["question"].clone(),
            )
            .await
            .unwrap();
    }
    let answer = crate::config::id();
    fixture
        .service
        .question_answer(
            &chat,
            &answering,
            json!({
                "id": answer,
                "answers": {
                    "scope": { "selected": [], "comment": "Keep this answer" },
                },
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
    assert_eq!(detail["pendingQuestions"], 1);
    let questions = detail["questions"].as_array().unwrap();
    assert_eq!(
        questions.iter().find(|q| q["id"] == pending).unwrap()["status"],
        "pending"
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

#[tokio::test]
async fn diagnostics_are_plain_text_without_agent_performance_records() {
    let mut fixture = Fixture::new().await;
    let performance = "\x1b[2m2026-10-22T12:08:49.998434Z\x1b[0m \x1b[32m INFO\x1b[0m \x1b[1magent_attempt\x1b[0m\x1b[1m{\x1b[0m\x1b[3mrun_id\x1b[0m\x1b[2m=\x1b[0m\"run\"\x1b[1m}\x1b[0m\x1b[2m:\x1b[0m \x1b[2mcairn_performance\x1b[0m\x1b[2m:\x1b[0m \x1b[3moperation\x1b[0m\x1b[2m=\x1b[0m\"agent_activity\"\n";
    let (reader, mut writer) = tokio::io::duplex(64);
    let (sender, mut receiver) = mpsc::channel(32);
    let reading = tokio::spawn(read_output(reader, true, sender, Vec::new()));
    let writing = tokio::spawn(async move {
        writer.write_all(performance.as_bytes()).await.unwrap();
        writer
            .write_all(b"\x1b[33m WARN\x1b[0m Reconnecting... 2/5\n")
            .await
            .unwrap();
        writer.write_all(performance.as_bytes()).await.unwrap();
        writer.write_all(b"Agent exited").await.unwrap();
    });
    while let Some(output) = receiver.recv().await {
        fixture.output(output).await;
    }
    writing.await.unwrap();
    reading.await.unwrap();

    let diagnostics: String = fixture
        .events()
        .await
        .iter()
        .filter(|event| event["type"] == "diagnostic")
        .map(|event| event["text"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(diagnostics, " WARN Reconnecting... 2/5\nAgent exited");
}

#[tokio::test]
async fn carriage_return_diagnostics_survive_adjacent_performance_records() {
    let performance = "2026-10-22T12:08:49Z INFO cairn_performance: operation=agent_activity";
    for (input, expected) in [
        (format!("disk full\r{performance}\n"), "disk full\r"),
        (format!("{performance}\rdisk full\r"), "disk full\r"),
        (format!("{performance}\r\ndisk full\r"), "disk full\r"),
        (format!("{performance}\r\n"), ""),
    ] {
        let mut fixture = Fixture::new().await;
        let (reader, mut writer) = tokio::io::duplex(1);
        let (sender, mut receiver) = mpsc::channel(32);
        let reading = tokio::spawn(read_output(reader, true, sender, Vec::new()));
        let streamed = input.clone();
        let writing = tokio::spawn(async move {
            writer.write_all(streamed.as_bytes()).await.unwrap();
        });
        while let Some(output) = receiver.recv().await {
            fixture.output(output).await;
        }
        writing.await.unwrap();
        reading.await.unwrap();

        let diagnostics: String = fixture
            .events()
            .await
            .iter()
            .filter(|event| event["type"] == "diagnostic")
            .map(|event| event["text"].as_str().unwrap().to_owned())
            .collect();
        assert_eq!(diagnostics, expected, "input: {input:?}");
    }
}

#[tokio::test]
async fn performance_names_inside_diagnostics_are_retained_and_manager_logs_are_redacted() {
    use tracing::instrument::WithSubscriber;

    // Keep scoped tracing capture independent of other tests' callsite registration.
    let _callsite_registration =
        tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default());
    let mut fixture = Fixture::new().await;
    let log_path = fixture._root.path().join("manager.log");
    let log_file = std::fs::File::create(&log_path).unwrap();
    let subscriber = tracing::Dispatch::new(
        tracing_subscriber::fmt()
            .with_ansi(false)
            .without_time()
            .with_writer(Arc::new(log_file))
            .finish(),
    );
    let input = concat!(
        "WARN native: invalid record cairn_performance: sensitive-tool-credential\n",
        "2026-10-22T12:08:49Z WARN native: invalid record cairn_performance: sensitive-tool-credential\n",
        "2026-10-22T12:08:49Z INFO cairn_performance: operation=agent_activity id=sensitive-tool-credential\n",
    );
    let (sender, mut receiver) = mpsc::channel(32);
    // Match production: the stderr reader has no secrets; the consumer supplies
    // current configured/account redactions, including credentials renewed later.
    read_output(input.as_bytes(), true, sender, Vec::new()).await;
    while let Some(output) = receiver.recv().await {
        let performance = output.kind == OutputKind::Performance;
        let previous_budget = fixture.log_total;
        if performance {
            fixture.log_total = LOG_BUDGET;
        }
        fixture
            .output(output)
            .with_subscriber(subscriber.clone())
            .await;
        if performance {
            assert_eq!(fixture.log_total, LOG_BUDGET);
            fixture.log_total = previous_budget;
        }
    }

    let events = fixture.events().await;
    assert_eq!(events.len(), 1);
    assert_eq!(
        events[0]["text"],
        concat!(
            "WARN native: invalid record cairn_performance: [redacted]\n",
            "2026-10-22T12:08:49Z WARN native: invalid record cairn_performance: [redacted]\n",
        )
    );
    let logs = std::fs::read_to_string(log_path).unwrap();
    assert!(logs.contains("source=\"agent_process\""), "{logs}");
    assert!(
        logs.contains("operation=agent_activity id=[redacted]"),
        "{logs}"
    );
    assert!(!logs.contains("sensitive-tool-credential"), "{logs}");
    assert!(!logs.contains("invalid record"), "{logs}");
}

#[tokio::test]
async fn terminal_styling_and_utf8_survive_long_diagnostic_fragments() {
    let prefix = "x".repeat(8190);
    let link = "https://example.com/".repeat(1000);
    for (input, expected) in [
        (
            format!("{prefix}\x1b[33mWARN\x1b[0m disk full\n"),
            format!("{prefix}WARN disk full\n"),
        ),
        (
            format!("{prefix}\x1b]8;;{link}\x1b\\WARN\x1b]8;;\x1b\\ disk full\n"),
            format!("{prefix}WARN disk full\n"),
        ),
        (
            format!("{prefix}xéWARN disk full\n"),
            format!("{prefix}xéWARN disk full\n"),
        ),
    ] {
        let mut fixture = Fixture::new().await;
        let (sender, mut receiver) = mpsc::channel(32);
        // Slice reads fill the reader's 8192-byte buffer, splitting the CSI,
        // a long OSC body, or a UTF-8 character at that exact boundary.
        read_output(input.as_bytes(), true, sender, Vec::new()).await;
        while let Some(output) = receiver.recv().await {
            fixture.output(output).await;
        }
        let diagnostics: String = fixture
            .events()
            .await
            .iter()
            .filter(|event| event["type"] == "diagnostic")
            .map(|event| event["text"].as_str().unwrap().to_owned())
            .collect();
        assert_eq!(diagnostics, expected);
    }
}

#[cfg(test)]
use crate::mission_fixture::MissionFixture;

#[tokio::test]
async fn context_usage_survives_log_budget_and_keeps_the_latest_snapshot_on_the_run() {
    let mut fixture = Fixture::new().await;
    fixture.log_total = LOG_BUDGET;
    for (tokens, estimated) in [(42_202, false), (6_200, true)] {
        fixture
            .event(json!({
                "type": "context.usage",
                "usage": {
                    "contextTokens": tokens,
                    "contextWindow": 200_000,
                    "cachedTokens": null,
                    "estimated": estimated,
                },
            }))
            .await;
    }

    let id = fixture.id.clone();
    let events = fixture
        .service
        .store
        .read(move |db| db.events_fixture(&id, 0, 1000))
        .await
        .unwrap();
    let usage = events
        .iter()
        .filter(|event| event["type"] == "context.usage")
        .map(|event| event["payload"]["usage"]["contextTokens"].clone())
        .collect::<Vec<_>>();
    assert_eq!(usage, [json!(42_202), json!(6_200)]);

    let run = fixture.service.store.run(&fixture.id).await.unwrap();
    assert_eq!(run["contextUsage"]["contextTokens"], 6_200);
    assert_eq!(run["contextUsage"]["contextWindow"], 200_000);
    assert_eq!(run["contextUsage"]["estimated"], true);

    // A malformed report never replaces the last known occupancy.
    fixture
        .event(json!({ "type": "context.usage", "usage": { "contextTokens": "many" } }))
        .await;
    let run = fixture.service.store.run(&fixture.id).await.unwrap();
    assert_eq!(run["contextUsage"]["contextTokens"], 6_200);
}

#[tokio::test]
async fn failed_event_publication_rolls_back_the_checkpoint_and_does_not_notify() {
    let mut fixture = Fixture::new().await;
    let before = fixture.service.store.revision();
    fixture.service.store.write(|db| {
        db.0.execute_batch("CREATE TRIGGER reject_event BEFORE INSERT ON events BEGIN SELECT RAISE(ABORT,'fixture publication failure'); END;")?;
        Ok(())
    }).await.unwrap();
    let output = Output::new(
        false,
        json!({"type":"turn.completed","usage":{"input_tokens":42}}).to_string(),
    );
    let result = record(
        &fixture.service,
        &output,
        &fixture.checkpoint,
        false,
        &[],
        &mut fixture.log_total,
        &mut fixture.activity,
    )
    .await;
    assert!(result.is_err());
    assert_eq!(fixture.service.store.revision(), before);
    assert!(!fixture.checkpoint.snapshot().await.completed());
    assert!(
        RunCheckpoint::load(&fixture.service.store, &fixture.id)
            .await
            .unwrap()
            .is_none()
    );
    assert!(fixture.service.store.run(&fixture.id).await.unwrap()["usage"].is_null());
    fixture
        .service
        .store
        .write(|db| {
            db.0.execute_batch("DROP TRIGGER reject_event")?;
            Ok(())
        })
        .await
        .unwrap();
    fixture.output(output).await;
    assert!(fixture.checkpoint.snapshot().await.completed());
    assert!(
        RunCheckpoint::load(&fixture.service.store, &fixture.id)
            .await
            .unwrap()
            .unwrap()
            .completed()
    );
    assert_eq!(
        fixture.service.store.run(&fixture.id).await.unwrap()["usage"]["input_tokens"],
        42
    );
}

#[tokio::test]
async fn unknown_engine_events_remain_redacted_data_without_changing_checkpoint_state() {
    let mut fixture = Fixture::new().await;
    fixture
        .event(json!({"type":"engine.future","payload":{"token":"sensitive-tool-credential"}}))
        .await;
    let id = fixture.id.clone();
    let events = fixture
        .service
        .store
        .read(move |db| db.event_page(&id, 0, 100))
        .await
        .unwrap();
    let event = events
        .iter()
        .find(|event| matches!(&event.kind, EventKind::External(kind) if kind == "engine.future"))
        .unwrap();
    let encoded = serde_json::to_string(event).unwrap();
    assert!(!encoded.contains("sensitive-tool-credential"));
    assert!(encoded.contains("[redacted]"));
    assert!(!fixture.checkpoint.snapshot().await.completed());
}

#[tokio::test]
async fn display_redaction_cannot_change_engine_lifecycle_or_session_identity() {
    let mut fixture = Fixture::new().await;
    let frames = [
        json!({"type":"thread.started","thread_id":"session-original"}),
        json!({"type":"context.usage","usage":{"contextTokens":400,"contextWindow":200000}}),
        json!({"type":"item.completed","item":{"id":"reply","type":"agent_message","text":"session private text"}}),
        json!({"type":"turn.completed","usage":{"input_tokens":42}}),
    ];
    for frame in frames {
        let output = Output::new(false, frame.to_string());
        record(
            &fixture.service,
            &output,
            &fixture.checkpoint,
            false,
            &["completed".into(), "text".into(), "session".into()],
            &mut fixture.log_total,
            &mut fixture.activity,
        )
        .await
        .unwrap();
    }
    let run = fixture.service.store.run(&fixture.id).await.unwrap();
    assert_eq!(run["sessionId"], "session-original");
    assert_eq!(run["contextUsage"]["contextTokens"], 400);
    assert_eq!(run["usage"]["input_tokens"], 42);
    let saved = RunCheckpoint::load(&fixture.service.store, &fixture.id)
        .await
        .unwrap()
        .unwrap();
    assert!(saved.completed());
    assert_eq!(saved.last_message(), "[redacted] private [redacted]");
    let id = fixture.id.clone();
    let events = fixture
        .service
        .store
        .read(move |db| db.events_fixture(&id, 0, 100))
        .await
        .unwrap();
    let session = events
        .iter()
        .find(|event| event["payload"]["thread_id"].is_string())
        .unwrap();
    assert_eq!(session["payload"]["thread_id"], "[redacted]-original");
    assert!(
        events
            .iter()
            .any(|event| event["payload"]["type"] == "turn.[redacted]")
    );
}

#[tokio::test]
async fn durable_progress_is_atomic_bound_and_survives_restart_without_a_fresh_clock() {
    use crate::worker::progress::{Phase, Progress};
    let mut fixture = Fixture::new().await;
    fixture
        .service
        .store
        .update_run_boundary(&fixture.id, json!({"missionTurnId":"progress-turn"}))
        .await
        .unwrap();
    fixture
        .checkpoint
        .update(|saved| {
            saved.runner_id = Some(Some("progress-attempt".into()));
            saved.progress = Some(Progress::launched(
                "progress-execution".into(),
                Some("progress-attempt".into()),
                None,
                Some("progress-turn".into()),
                1,
            ));
        })
        .await
        .unwrap();
    fixture
        .output(Output::new(true, "diagnostic only".into()))
        .await;
    let saved = RunCheckpoint::load(&fixture.service.store, &fixture.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(saved.progress.unwrap().advanced_at, None);
    fixture.service.store.write(|db| {
        db.0.execute_batch("CREATE TRIGGER reject_progress_event BEFORE INSERT ON events BEGIN SELECT RAISE(ABORT,'fixture'); END;")?;
        Ok(())
    }).await.unwrap();
    let result = record(
        &fixture.service,
        &Output::new(false, json!({"type":"turn.started"}).to_string()),
        &fixture.checkpoint,
        false,
        &[],
        &mut fixture.log_total,
        &mut fixture.activity,
    )
    .await;
    assert!(result.is_err());
    assert_eq!(
        fixture
            .checkpoint
            .snapshot()
            .await
            .progress
            .unwrap()
            .advanced_at,
        None
    );
    assert_eq!(
        RunCheckpoint::load(&fixture.service.store, &fixture.id)
            .await
            .unwrap()
            .unwrap()
            .progress
            .unwrap()
            .advanced_at,
        None
    );
    fixture
        .service
        .store
        .write(|db| {
            db.0.execute_batch("DROP TRIGGER reject_progress_event;")?;
            Ok(())
        })
        .await
        .unwrap();
    fixture.event(json!({"type":"turn.started"})).await;
    let item = json!({"type":"item.updated","item":{"id":"stable","type":"mcp_tool_call","status":"completed","result":"stable-private"}});
    fixture.event(item.clone()).await;
    fixture
        .checkpoint
        .update(|saved| saved.progress.as_mut().unwrap().advanced_at = Some(2))
        .await
        .unwrap();
    record(
        &fixture.service,
        &Output::new(false, item.to_string()),
        &fixture.checkpoint,
        false,
        &["stable-private".into()],
        &mut fixture.log_total,
        &mut fixture.activity,
    )
    .await
    .unwrap();
    let coverage = fixture.checkpoint.snapshot().await.progress.unwrap();
    assert!(
        !coverage.complete,
        "redaction-only changes cannot certify new work"
    );
    assert_eq!(coverage.advanced_at, Some(2));
    fixture.event(json!({"type":"turn.completed"})).await;
    let saved = RunCheckpoint::load(&fixture.service.store, &fixture.id)
        .await
        .unwrap()
        .unwrap();
    let phase_at = saved.progress.as_ref().unwrap().phase_at;
    assert_eq!(saved.progress.as_ref().unwrap().phase, Phase::Finalizing);
    let restarted = Checkpoint::new(fixture.service.store.clone(), fixture.id.clone(), saved);
    restarted
        .update(|saved| saved.completed = Some(false))
        .await
        .unwrap();
    record(&fixture.service, &Output::new(false, json!({"type":"item.updated","item":{"id":"late","type":"agent_message","text":"late snapshot"}}).to_string()), &restarted, false, &[], &mut fixture.log_total, &mut fixture.activity).await.unwrap();
    restarted.persist().await.unwrap();
    record(
        &fixture.service,
        &Output::new(
            false,
            json!({"type":"runner.finalized","attemptId":"wrong-attempt","physicalFinalized":true})
                .to_string(),
        ),
        &restarted,
        false,
        &[],
        &mut fixture.log_total,
        &mut fixture.activity,
    )
    .await
    .unwrap();
    assert!(
        restarted
            .snapshot()
            .await
            .progress
            .unwrap()
            .finalized_at
            .is_none()
    );
    record(&fixture.service, &Output::new(false, json!({"type":"runner.finalized","attemptId":"progress-attempt","physicalFinalized":true}).to_string()), &restarted, false, &[], &mut fixture.log_total, &mut fixture.activity).await.unwrap();
    let mut saved = RunCheckpoint::load(&fixture.service.store, &fixture.id)
        .await
        .unwrap()
        .unwrap();
    assert!(saved.progress.as_ref().unwrap().finalized_at.is_some());
    assert_eq!(saved.progress.as_ref().unwrap().phase, Phase::Finalizing);
    assert_eq!(saved.progress.as_ref().unwrap().phase_at, phase_at);
    saved.runner_id = Some(Some("replacement".into()));
    saved
        .store(&fixture.service.store, &fixture.id)
        .await
        .unwrap();
    let stale = record(
        &fixture.service,
        &Output::new(false, json!({"type":"turn.started"}).to_string()),
        &restarted,
        false,
        &[],
        &mut fixture.log_total,
        &mut fixture.activity,
    )
    .await;
    assert!(
        stale.is_err(),
        "old execution output must not advance replacement progress"
    );
}

#[tokio::test]
async fn malformed_or_truncated_stdout_retains_monitoring_gap_without_changing_control_policy() {
    let malformed = "{malformed".to_owned();
    let oversized = json!({
        "type": "item.updated",
        "item": {
            "id": "message",
            "type": "agent_message",
            "text": "x".repeat(LINE_LIMIT),
        },
    })
    .to_string();
    for raw in [malformed, oversized] {
        let mut fixture = Fixture::new().await;
        fixture.bind_progress().await;
        fixture.output(Output::new(false, " \t ".into())).await;
        fixture
            .output(Output::new(true, "diagnostic only".into()))
            .await;
        assert!(
            fixture
                .checkpoint
                .snapshot()
                .await
                .progress
                .as_ref()
                .unwrap()
                .complete
        );
        assert_ne!(
            fixture.checkpoint.snapshot().await.output_interrupted,
            Some(true)
        );

        fixture.output(Output::line(raw.as_bytes(), &[])).await;
        assert!(
            !fixture
                .checkpoint
                .snapshot()
                .await
                .progress
                .as_ref()
                .unwrap()
                .complete
        );
        let saved = RunCheckpoint::load(&fixture.service.store, &fixture.id)
            .await
            .unwrap()
            .unwrap();
        assert!(!saved.progress.as_ref().unwrap().complete);
        assert_ne!(saved.output_interrupted, Some(true));

        // Only an explicit trusted relay marker changes the existing control flag.
        fixture
            .event(json!({ "type": "runner.output_interrupted" }))
            .await;
        fixture
            .event(json!({ "type": "runner.output_interrupted" }))
            .await;
        assert_eq!(
            fixture.checkpoint.snapshot().await.output_interrupted,
            Some(true)
        );
        let id = fixture.id.clone();
        let gaps = fixture
            .service
            .store
            .read(move |db| {
                Ok(db
                    .events_fixture(&id, 0, 100)?
                    .iter()
                    .filter(|event| {
                        event["text"]
                            == "Execution output was interrupted; some activity may be missing."
                    })
                    .count())
            })
            .await
            .unwrap();
        assert_eq!(gaps, 1);
    }
}

#[tokio::test]
async fn unsupported_json_degrades_coverage_even_after_log_budget_is_spent() {
    for event in [
        json!({}),
        json!({ "type": "item.started" }),
        json!({ "type": "item.updated", "item": null }),
        json!({ "type": "item.completed", "item": [] }),
        json!({ "type":"item.updated", "item":{"id":"stable","type":"agent_message","text":{"duration_ms":2}} }),
        json!({ "type":"item.updated", "item":{"id":"stable","type":"agent_message","text":"stable","status":{"duration_ms":2}} }),
        json!({ "type":"item.updated", "item":{"id":"stable","type":"agent_message","text":"stable","parent_id":{"duration_ms":2}} }),
        json!({ "type":"item.updated", "item":{"id":"web","type":"web_search","action":{"type":"search","query":{"duration_ms":2}}} }),
        json!({ "type":"item.updated", "item":{"id":"web","type":"web_search","action":{"type":"open_page","url":{"duration_ms":2}}} }),
        json!({ "type":"item.updated", "item":{"id":"web","type":"web_search","action":{"type":"find_in_page","url":"https://example.test","pattern":{"duration_ms":2}}} }),
        json!({ "type":"item.updated", "item":{"id":"web","type":"web_search","action":{"type":"search","query":"unchanged"},"results":[{"duration_ms":2}]} }),
        json!({ "type": "future.provider_wait" }),
        json!({ "type": "item.delta", "item": { "id": "future", "type": "agent_message" } }),
    ] {
        let mut fixture = Fixture::new().await;
        fixture.bind_progress().await;
        fixture.event(json!({ "type": "turn.started" })).await;
        let advanced_at = fixture
            .checkpoint
            .snapshot()
            .await
            .progress
            .as_ref()
            .unwrap()
            .advanced_at;
        fixture.log_total = LOG_BUDGET;
        fixture.event(event).await;
        let saved = RunCheckpoint::load(&fixture.service.store, &fixture.id)
            .await
            .unwrap()
            .unwrap();
        assert!(!saved.progress.as_ref().unwrap().complete);
        assert_eq!(saved.progress.as_ref().unwrap().advanced_at, advanced_at);
        assert_ne!(saved.output_interrupted, Some(true));
    }
}

#[tokio::test]
async fn normal_native_certificate_framing_does_not_report_lost_output() {
    let mut fixture = Fixture::new().await;
    fixture.bind_progress().await;
    let stream = concat!(
        "{\"type\":\"turn.completed\"}\n",
        "\n{\"type\":\"runner.finalized\",\"attemptId\":\"progress-attempt\",\"physicalFinalized\":true}\n"
    );
    let (send, mut receive) = mpsc::channel(8);
    read_output(stream.as_bytes(), false, send, Vec::new()).await;
    while let Some(output) = receive.recv().await {
        fixture.output(output).await;
    }
    let saved = RunCheckpoint::load(&fixture.service.store, &fixture.id)
        .await
        .unwrap()
        .unwrap();
    assert_ne!(saved.output_interrupted, Some(true));
    assert!(saved.progress.as_ref().unwrap().complete);
    assert!(saved.progress.as_ref().unwrap().finalized_at.is_some());
}
