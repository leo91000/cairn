//! Content-free observations of the normalized Codex/Claude stream. Silence is
//! an observation, never evidence that a process stalled or permission to stop it.
use crate::{provider::Provider, validation::text};
use serde_json::Value;
use std::{collections::HashMap, time::Duration};
use tokio::time::{Instant, Interval, MissedTickBehavior};

const MAX_ITEMS: usize = 128;
const MAX_ID_BYTES: usize = 256;
const HEARTBEAT: Duration = Duration::from_secs(30);

pub fn heartbeat() -> Interval {
    let mut timer = tokio::time::interval_at(Instant::now() + HEARTBEAT, HEARTBEAT);
    timer.set_missed_tick_behavior(MissedTickBehavior::Skip);
    timer
}

struct Item {
    category: &'static str,
    sequence: u64,
    started: Instant,
}

/// Bounded, in-memory metrics only. Does not alter events or persist another
/// event per token. Item IDs are kept for matching but never logged.
pub struct Activity {
    run_id: String,
    attempt_id: String,
    side: &'static str,
    provider: Provider,
    turn_start_source: &'static str,
    started: Instant,
    last_output: Instant,
    last_event: Instant,
    turn_started: Option<Instant>,
    first_event: Option<u64>,
    first_message: Option<u64>,
    turn: u64,
    sequence: u64,
    events: u64,
    skipped_items: u64,
    items: HashMap<String, Item>,
    questions: HashMap<String, ()>,
}

impl Activity {
    pub fn new(run_id: &str, attempt_id: &str, side: &'static str, provider: Provider) -> Self {
        let now = Instant::now();
        Self {
            run_id: super::identity(run_id).to_owned(),
            attempt_id: super::identity(attempt_id).to_owned(),
            side,
            provider,
            turn_start_source: "none",
            started: now,
            last_output: now,
            last_event: now,
            turn_started: None,
            first_event: None,
            first_message: None,
            turn: 0,
            sequence: 0,
            events: 0,
            skipped_items: 0,
            items: HashMap::new(),
            questions: HashMap::new(),
        }
    }

    /// Stdout transport activity, distinct from stderr and recognized agent events.
    pub fn output(&mut self) {
        self.last_output = Instant::now();
    }

    pub fn observe(&mut self, event: &Value) {
        let now = Instant::now();
        let kind = text(event, "type");
        if !matches!(
            kind,
            "thread.started"
                | "turn.started"
                | "turn.completed"
                | "turn.failed"
                | "error"
                | "item.started"
                | "item.updated"
                | "item.completed"
                | "chat.question"
                | "chat.question.closed"
        ) {
            return;
        }
        self.last_event = now;
        self.events += 1;
        match kind {
            "thread.started" if self.provider == Provider::Claude && self.turn == 0 => {
                // Claude's adapter has no turn.started notification. Initialization
                // is the observable boundary; do not invent a protocol event.
                self.start_turn(now, "thread_initialized");
            }
            "turn.started" => {
                if self.turn_started.is_some() {
                    self.record("turn_replaced", "none", 0, 0);
                }
                self.start_turn(now, "provider_turn");
            }
            "turn.completed" | "turn.failed" | "error" => {
                let outcome = match kind {
                    "turn.completed" => "turn_completed",
                    "turn.failed" => "turn_failed",
                    _ => "error_observed",
                };
                self.record(outcome, "none", 0, 0);
                if kind != "error" {
                    self.turn_started = None;
                    self.items.clear();
                    self.questions.clear();
                }
            }
            "item.started" | "item.updated" | "item.completed" => {
                self.item(event, kind, now);
            }
            "chat.question" => {
                let id = text(&event["question"], "id");
                if event["question"]["blocking"] != false
                    && valid_id(id)
                    && self.questions.len() < MAX_ITEMS
                {
                    self.questions.insert(id.to_owned(), ());
                }
            }
            "chat.question.closed" => {
                let id = text(event, "questionId");
                if id.is_empty() {
                    self.questions.clear();
                } else {
                    self.questions.remove(id);
                }
            }
            _ => {}
        }
    }

    fn start_turn(&mut self, now: Instant, source: &'static str) {
        self.items.clear();
        self.questions.clear();
        self.turn += 1;
        self.turn_started = Some(now);
        self.turn_start_source = source;
        self.first_event = None;
        self.first_message = None;
        self.record("turn_started", "none", 0, 0);
    }

    fn item(&mut self, event: &Value, kind: &str, now: Instant) {
        let item = &event["item"];
        let category = category(text(item, "type"));
        if let Some(started) = self.turn_started {
            if self.first_event.is_none() {
                self.first_event = Some(ms(now - started));
                self.record("first_activity", category, 0, self.first_event.unwrap());
            }
            if category == "message"
                && !text(item, "text").is_empty()
                && self.first_message.is_none()
            {
                self.first_message = Some(ms(now - started));
                self.record("first_message", category, 0, self.first_message.unwrap());
            }
        }
        let id = text(item, "id");
        if !valid_id(id) {
            return;
        }
        if kind == "item.completed" {
            if let Some(active) = self.items.remove(id) {
                self.record(
                    "item_completed",
                    active.category,
                    active.sequence,
                    ms(now - active.started),
                );
            } else {
                // Some adapters expose only the completion (including restored
                // items). Keep the native duration distinct from an observed one.
                tracing::info!(
                    target: "leo_performance",
                    operation = "agent_activity",
                    run_id = self.run_id,
                    attempt_id = self.attempt_id,
                    side = self.side,
                    event = "item_completed_without_start",
                    category,
                    turn = self.turn,
                    provider_elapsed_ms = item["duration_ms"].as_u64(),
                );
            }
            return;
        }
        // Updates and duplicate starts must not reset the duration of an open item.
        if kind != "item.started" || self.items.contains_key(id) {
            return;
        }
        if self.items.len() >= MAX_ITEMS {
            self.skipped_items += 1;
            return;
        }
        self.sequence += 1;
        self.items.insert(
            id.to_owned(),
            Item {
                category,
                sequence: self.sequence,
                started: now,
            },
        );
        self.record("item_started", category, self.sequence, 0);
    }

    pub fn heartbeat(&self, phase: &'static str) {
        self.record("heartbeat", phase, 0, 0);
    }

    pub fn finish(&self) {
        self.record("stream_closed", "none", 0, 0);
    }

    fn state(&self) -> &'static str {
        if !self.questions.is_empty() {
            return "waiting_for_user";
        }
        if self.items.values().any(|i| {
            matches!(
                i.category,
                "command" | "mcp" | "file_change" | "web" | "collaboration"
            )
        }) {
            return "tools_open";
        }
        if self.turn_started.is_some() {
            return "waiting_for_agent_event";
        }
        if self.turn == 0 {
            "startup"
        } else {
            "between_turns"
        }
    }

    fn record(
        &self,
        event: &'static str,
        category: &'static str,
        item_sequence: u64,
        elapsed_ms: u64,
    ) {
        let now = Instant::now();
        let oldest = self.items.values().min_by_key(|item| item.started);
        tracing::info!(
            target: "leo_performance",
            operation = "agent_activity",
            run_id = self.run_id,
            attempt_id = self.attempt_id,
            side = self.side,
            event,
            category,
            item_sequence,
            elapsed_ms,
            state = self.state(),
            turn = self.turn,
            turn_start_source = self.turn_start_source,
            attempt_ms = ms(now - self.started),
            turn_ms = self.turn_started.map(|started| ms(now - started)),
            first_activity_ms = self.first_event,
            first_message_ms = self.first_message,
            output_quiet_ms = ms(now - self.last_output),
            event_quiet_ms = ms(now - self.last_event),
            events = self.events,
            open_items = self.items.len(),
            oldest_category = oldest.map_or("none", |item| item.category),
            oldest_item_ms = oldest.map(|item| ms(now - item.started)),
            open_questions = self.questions.len(),
            skipped_items = self.skipped_items,
        );
    }
}

fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= MAX_ID_BYTES
}

fn ms(duration: Duration) -> u64 {
    duration.as_millis().try_into().unwrap_or(u64::MAX)
}

fn category(kind: &str) -> &'static str {
    match kind {
        "command_execution" => "command",
        "mcp_tool_call" => "mcp",
        "file_change" => "file_change",
        "web_search" => "web",
        "collab_tool_call" => "collaboration",
        "reasoning" => "reasoning",
        "agent_message" => "message",
        _ => "other",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::{
        io::Write,
        sync::{Arc, Mutex},
    };

    const RUN: &str = "11111111-1111-4111-8111-111111111111";
    const ATTEMPT: &str = "22222222-2222-4222-8222-222222222222";

    #[derive(Clone, Default)]
    struct Logs(Arc<Mutex<Vec<u8>>>);

    impl Write for Logs {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl Logs {
        fn subscriber(&self) -> impl tracing::Subscriber + Send + Sync + 'static {
            let writer = self.clone();
            tracing_subscriber::fmt()
                .json()
                .without_time()
                .with_writer(move || writer.clone())
                .finish()
        }

        fn events(&self) -> Vec<Value> {
            let bytes = self.0.lock().unwrap();
            String::from_utf8_lossy(&bytes)
                .lines()
                .map(|line| serde_json::from_str::<Value>(line).unwrap()["fields"].clone())
                .collect()
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_silent_open_tool_reports_its_age_without_restarting_or_logging_content() {
        let logs = Logs::default();
        let _subscriber = tracing::subscriber::set_default(logs.subscriber());
        let mut activity = Activity::new(RUN, ATTEMPT, "adapter", Provider::Codex);
        activity.observe(&json!({"type":"turn.started"}));
        tokio::time::advance(Duration::from_secs(5)).await;
        let tool = json!({
            "type": "item.started",
            "item": {
                "id": "secret-tool-id",
                "type": "command_execution",
                "command": "secret-command",
                "arguments": {"token":"secret-token"},
                "text": "secret-output",
            },
        });
        activity.observe(&tool);
        tokio::time::advance(Duration::from_secs(60)).await;
        activity.observe(&tool); // duplicate starts preserve the original clock
        activity.heartbeat("receive_agent_event");
        let events = logs.events();
        let heartbeat = events.last().unwrap();
        assert_eq!(heartbeat["state"], "tools_open");
        assert_eq!(heartbeat["oldest_item_ms"], 60_000);
        assert_eq!(heartbeat["first_activity_ms"], 5_000);
        assert_eq!(heartbeat["run_id"], RUN);
        assert_eq!(heartbeat["attempt_id"], ATTEMPT);
        assert_eq!(heartbeat["open_items"], 1);
        let serialized = serde_json::to_string(&events).unwrap();
        assert!(!serialized.contains("secret-"));
        activity.observe(&json!({"type":"item.completed","item":{"id":"secret-tool-id","type":"command_execution"}}));
        assert_eq!(logs.events().last().unwrap()["elapsed_ms"], 60_000);
        assert_eq!(activity.state(), "waiting_for_agent_event");
    }

    #[tokio::test(start_paused = true)]
    async fn streaming_deltas_are_quiet_in_logs_and_new_turns_reset_first_message_and_open_items() {
        let logs = Logs::default();
        let _subscriber = tracing::subscriber::set_default(logs.subscriber());
        let mut activity = Activity::new(RUN, ATTEMPT, "worker", Provider::Codex);
        activity.observe(&json!({"type":"turn.started"}));
        tokio::time::advance(Duration::from_secs(2)).await;
        activity.observe(
            &json!({"type":"item.started","item":{"id":"msg","type":"agent_message","text":""}}),
        );
        tokio::time::advance(Duration::from_secs(3)).await;
        let delta = json!({"type":"item.updated","item":{"id":"msg","type":"agent_message","text":"private prompt and response"}});
        activity.observe(&delta);
        let count = logs.events().len();
        for _ in 0..10_000 {
            activity.observe(&delta);
        }
        assert_eq!(
            logs.events().len(),
            count,
            "no telemetry line per token delta"
        );
        assert_eq!(activity.first_message, Some(5_000));
        activity.observe(&json!({"type":"chat.question","question":{"id":"q"}}));
        assert_eq!(activity.state(), "waiting_for_user");
        activity.observe(&json!({"type":"chat.question.closed"}));
        assert_eq!(activity.state(), "waiting_for_agent_event");
        activity.observe(&json!({"type":"turn.started"}));
        assert!(activity.items.is_empty());
        assert_eq!(activity.first_message, None);
        assert_eq!(activity.first_event, None);
        activity.observe(&json!({"type":"turn.failed","error":{"message":"secret-error"}}));
        assert_eq!(activity.state(), "between_turns");
        assert!(
            !serde_json::to_string(&logs.events())
                .unwrap()
                .contains("secret-error")
        );
    }

    #[test]
    fn malformed_ids_and_unclosed_items_cannot_grow_memory_or_log_untrusted_categories() {
        let mut activity = Activity::new(
            "secret-invalid-run",
            "secret-invalid-attempt",
            "worker",
            Provider::Codex,
        );
        assert_eq!(activity.run_id, "unknown");
        assert_eq!(activity.attempt_id, "unknown");
        for id in 0..500 {
            activity.observe(&json!({"type":"item.started","item":{"id":id.to_string(),"type":"secret-category"}}));
        }
        assert_eq!(activity.items.len(), MAX_ITEMS);
        assert_eq!(activity.skipped_items, 500 - MAX_ITEMS as u64);
        assert!(activity.items.values().all(|item| item.category == "other"));
        activity.observe(&json!({"type":"item.started","item":{"id":"x".repeat(MAX_ID_BYTES+1),"type":"command_execution"}}));
        assert_eq!(activity.items.len(), MAX_ITEMS);
    }

    #[tokio::test(start_paused = true)]
    async fn claude_uses_an_explicitly_labeled_initialization_boundary_without_a_protocol_turn_start()
     {
        let logs = Logs::default();
        let _subscriber = tracing::subscriber::set_default(logs.subscriber());
        let mut activity = Activity::new(RUN, ATTEMPT, "adapter", Provider::Claude);
        activity.observe(&json!({"type":"thread.started"}));
        tokio::time::advance(Duration::from_secs(7)).await;
        activity.observe(&json!({"type":"item.completed","item":{"id":"reply","type":"agent_message","text":"private response"}}));
        activity.heartbeat("receive_agent_event");
        let events = logs.events();
        let heartbeat = events.last().unwrap();
        assert_eq!(heartbeat["turn_start_source"], "thread_initialized");
        assert_eq!(heartbeat["first_message_ms"], 7_000);
        assert_eq!(heartbeat["state"], "waiting_for_agent_event");
        assert_eq!(heartbeat["turn"], 1);
        activity.observe(&json!({"type":"turn.completed"}));
        assert_eq!(activity.state(), "between_turns");
    }

    #[tokio::test(start_paused = true)]
    async fn a_slow_output_write_still_reports_progress_and_completes_without_a_timeout() {
        let logs = Logs::default();
        let _subscriber = tracing::subscriber::set_default(logs.subscriber());
        let result = super::super::wait("stdout_write", async {
            tokio::time::sleep(Duration::from_secs(65)).await;
            42
        })
        .await;
        assert_eq!(result, 42);
        let events = logs.events();
        assert_eq!(events.len(), 3);
        assert_eq!(events[0]["event"], "waiting");
        assert_eq!(events[0]["elapsed_ms"], 30_000);
        assert_eq!(events[1]["elapsed_ms"], 60_000);
        assert_eq!(events[2]["event"], "completed");
        assert_eq!(events[2]["elapsed_ms"], 65_000);
    }
}
