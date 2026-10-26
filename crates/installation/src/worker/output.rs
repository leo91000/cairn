use super::checkpoint::{Checkpoint, RunCheckpoint};
use crate::{config::now, error::Result, run_output, service::Service, validation::text};
use serde_json::{Value, json};
use std::sync::LazyLock;
use tokio::{
    io::{AsyncRead, AsyncReadExt},
    sync::mpsc,
    time::Instant,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum OutputKind {
    Stdout,
    Diagnostic,
    Performance,
}

pub(super) struct Output {
    pub kind: OutputKind,
    pub raw: String,
    pub received: Instant,
}

impl Output {
    fn new(diagnostic: bool, raw: String) -> Self {
        Self {
            kind: if diagnostic {
                OutputKind::Diagnostic
            } else {
                OutputKind::Stdout
            },
            raw,
            received: Instant::now(),
        }
    }

    fn performance(raw: String) -> Self {
        Self {
            kind: OutputKind::Performance,
            raw,
            received: Instant::now(),
        }
    }

    fn line(bytes: &[u8], secrets: &[String]) -> Self {
        let raw = String::from_utf8_lossy(bytes);
        if raw.len() > LINE_LIMIT
            && let Ok(event) = serde_json::from_str::<Value>(&raw)
            && text(&event, "type").starts_with("item.")
            && event["item"].is_object()
            && event["item"]["type"] != "agent_message"
        {
            return Self::new(
                false,
                run_output::compact_activity(&event, secrets).to_string(),
            );
        }

        Self::new(
            false,
            raw[..raw.floor_char_boundary(raw.len().min(LINE_LIMIT))].to_owned(),
        )
    }
}

/// Separate budget for technical logs; tool activity has only a per-item detail limit.
const LOG_BUDGET: usize = 500_000;
const ITEM_DETAIL_LIMIT: usize = run_output::ACTIVITY_DETAIL_LIMIT;
const LINE_LIMIT: usize = 2_000_000;
const DIAGNOSTIC_FRAGMENT_LIMIT: usize = 8192;

// Match the runner's timestamp, level, optional field-bearing spans and target.
// A target name mentioned in another logger's message is still a diagnostic.
static AGENT_PERFORMANCE_RECORD: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(
        r"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?Z\s+(?:TRACE|DEBUG|INFO|WARN|ERROR)\s+(?:[A-Za-z_][A-Za-z0-9_]*\{[^{}\r\n]*\}:\s*)*cairn_performance: ",
    )
    .unwrap()
});

// Match the resident Codex transport: supported tool frames must reach the JSON
// parser intact before shortening. Large buffers are released after each frame.
const TOOL_FRAME_LIMIT: usize = 100_000_000;
const MESSAGE_LIMIT: usize = 100_000;
const ERROR_LIMIT: usize = 10_000;
/// Claude Code's notices are status lines; a longer one is cut.
const NOTICE_LIMIT: usize = 4_000;

/// Event types that are always recorded. Clients use turn markers to scope reused message IDs,
/// show the latest context usage, and show the engine's retries and usage limits in the
/// conversation.
const CONVERSATION_EVENTS: [&str; 10] = [
    "thread.started",
    "context.usage",
    "turn.started",
    "turn.completed",
    "turn.failed",
    "turn.waiting",
    "error",
    "runner.failed",
    "engine.retry",
    "provider.limit",
];

/// An engine notice the conversation shows. Other notices only reach the run log, within its
/// budget.
fn shown_notice(event: &Value) -> bool {
    event["type"] == "engine.notice"
        && (matches!(text(event, "level"), "warning" | "suggestion") || event["stopsTurn"] == true)
}

// Separate pipe readers keep stderr draining even while stdout applies database backpressure.
pub(super) async fn read_output(
    mut reader: impl AsyncRead + Unpin,
    diagnostic: bool,
    events: mpsc::Sender<Output>,
    secrets: Vec<String>,
) {
    if diagnostic {
        return read_diagnostics(reader, events).await;
    }

    let mut bytes = [0; 8192];
    let mut line = Vec::new();
    let mut object_frame = None;
    loop {
        let n = match reader.read(&mut bytes).await {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        for byte in &bytes[..n] {
            if *byte != b'\n' {
                if object_frame.is_none() && !byte.is_ascii_whitespace() {
                    object_frame = Some(*byte == b'{');
                }
                let limit = if object_frame == Some(true) {
                    TOOL_FRAME_LIMIT
                } else {
                    LINE_LIMIT
                };
                if line.len() < limit {
                    line.push(*byte);
                }
                continue;
            }
            if events.send(Output::line(&line, &secrets)).await.is_err() {
                return;
            }
            if line.capacity() > LINE_LIMIT {
                line = Vec::new();
            } else {
                line.clear();
            }
            object_frame = None;
        }
    }
    if !line.is_empty() {
        // The receiver only disappears once the run stopped listening.
        let _ = events.send(Output::line(&line, &secrets)).await;
    }
}

/// Forwards agent stderr by whole lines, so styling and performance records are
/// recognized intact. A long fragment without a line end is still forwarded.
async fn read_diagnostics(mut reader: impl AsyncRead + Unpin, events: mpsc::Sender<Output>) {
    let mut bytes = [0; 8192];
    let mut pending = Vec::new();
    let mut styling = run_output::TerminalStyling::default();
    loop {
        let n = match reader.read(&mut bytes).await {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        styling.push(&bytes[..n], &mut pending);

        // Keep a trailing CR until the next byte, so a split CRLF stays together.
        let complete = match pending.iter().enumerate().rfind(|(index, byte)| {
            **byte == b'\n' || (**byte == b'\r' && index + 1 < pending.len())
        }) {
            Some((end, _)) => end + 1,
            None if pending.len() >= DIAGNOSTIC_FRAGMENT_LIMIT => {
                // Keep an incomplete UTF-8 character for the next read as well.
                match std::str::from_utf8(&pending) {
                    Err(error) if error.error_len().is_none() => error.valid_up_to(),
                    _ => pending.len(),
                }
            }
            None => continue,
        };
        let lines: Vec<u8> = pending.drain(..complete).collect();
        if !forward_diagnostics(&lines, &events).await {
            return;
        }
    }
    if !pending.is_empty() {
        // The receiver only disappears once the run stopped listening.
        forward_diagnostics(&pending, &events).await;
    }
}

/// Sends readable diagnostics to the run. The agent process's own performance
/// records belong to the manager log, like the worker's.
async fn forward_diagnostics(bytes: &[u8], events: &mpsc::Sender<Output>) -> bool {
    let text = String::from_utf8_lossy(bytes);
    let mut diagnostics = String::new();
    let mut lines = text.split_inclusive(['\r', '\n']).peekable();
    while let Some(line) = lines.next() {
        if AGENT_PERFORMANCE_RECORD.is_match(line) {
            // CRLF is one terminator; do not leave its LF as an empty diagnostic.
            if line.ends_with('\r') && lines.peek() == Some(&"\n") {
                lines.next();
            }
            if events
                .send(Output::performance(line.trim_end().to_owned()))
                .await
                .is_err()
            {
                return false;
            }
        } else {
            diagnostics.push_str(line);
        }
    }

    diagnostics.is_empty() || events.send(Output::new(true, diagnostics)).await.is_ok()
}

/// Records one output line and returns whether it reports exhausted usage.
pub(super) async fn record(
    s: &Service,
    output: &Output,
    checkpoint: &Checkpoint,
    previously_launched: bool,
    secrets: &[String],
    log_total: &mut usize,
    activity: &mut crate::performance::Activity,
) -> Result<bool> {
    let id = checkpoint.id.as_str();
    let raw = output.raw.as_str();
    if output.kind == OutputKind::Performance {
        // The consumer refreshes configured/account redactions before every output.
        // Performance records never consume the run's persisted log budget.
        let record = run_output::redact(raw, secrets);
        tracing::info!(target: "cairn_performance", source = "agent_process", record = %record);
        return Ok(false);
    }
    if output.kind == OutputKind::Diagnostic {
        record_raw(s, id, "diagnostic", raw, secrets, log_total).await?;
        return Ok(false);
    }
    let Ok(event) = serde_json::from_str::<Value>(raw) else {
        record_raw(s, id, "output", raw, secrets, log_total).await?;
        return Ok(false);
    };
    if event["type"] == "runtime.disk_notice" {
        let event = run_output::payload(&event, secrets);
        let message = text(&event, "text").to_owned();
        // Capacity is runtime diagnostic state, separate from the controller's
        // authoritative storage document refreshed by the publication monitor.
        s.store
            .patch_run(id, serde_json::json!({ "diskCapacity": event["capacity"] }))
            .await?;
        s.store.event(id, "status", &message, Some(event)).await?;
        return Ok(false);
    }
    if event["type"] == "turn.speed" {
        let event = run_output::payload(&event, secrets);
        let (message, outcome) = speed_notice(s, id, &event).await?;
        if let Some(message) = message {
            s.store.event(id, "status", message, Some(event)).await?;
        }
        // Recorded once its notice is stored, so a retry after a failure still shows it.
        s.store
            .set(
                &format!("turn-speed:{id}"),
                outcome,
                Some(now() + 30 * 86_400_000),
            )
            .await?;
        return Ok(false);
    }
    if event["type"] == "runner.start_rejected" {
        let run = s.store.run(id).await?;
        let first_rejected = !previously_launched
            && run["sessionId"].is_null()
            && checkpoint.read(RunCheckpoint::managed).await;
        if first_rejected && run["chatExecution"].is_object() {
            let mut execution = run["chatExecution"].clone();
            execution["recovery"] = false.into();
            s.store
                .patch_run(id, serde_json::json!({ "chatExecution": execution }))
                .await?;
        }

        let message = truncate(
            &run_output::redact(text(&event, "message"), secrets),
            ERROR_LIMIT,
        );
        checkpoint
            .update(|c| {
                c.start_rejected = Some(true);
                c.last_error = Some(Some(message));
                if first_rejected {
                    // Persist retry intent before fencing can fail. Settlement
                    // and every subsequent launch still require a successful fence.
                    c.launched = Some(false);
                }
            })
            .await?;
        return Ok(false);
    }

    activity.observe(&event);
    if handle_chat_control(s, id, &event).await? {
        return Ok(false);
    }
    let exhausted = run_output::exhausted(&event);
    apply_event(s, id, &event, checkpoint, secrets).await?;
    // Every activity item has its own detail limit, regardless of earlier output.
    let conversation = event["item"]["type"] == "agent_message"
        || CONVERSATION_EVENTS.contains(&text(&event, "type"))
        || shown_notice(&event);
    let activity_item = text(&event, "type").starts_with("item.") && event["item"].is_object();
    if conversation || activity_item || *log_total < LOG_BUDGET {
        let abbreviated = !conversation && activity_item && raw.len() > ITEM_DETAIL_LIMIT;
        let mut stored = if abbreviated {
            run_output::compact_activity(&event, secrets)
        } else {
            run_output::payload(&event, secrets)
        };
        // Cut once redacted, so a cut cannot expose part of a secret.
        if let Some(message) = stored["message"]
            .as_str()
            .filter(|_| stored["type"] == "engine.notice")
        {
            stored["message"] = cut_notice(message).into();
        }
        let serialized = serde_json::to_string(&stored)?;
        if !conversation && !activity_item {
            *log_total = log_total.saturating_add(serialized.len());
        }
        store_event(s, id, &serialized, &stored).await?;
    }
    if event["type"] == "turn.completed" {
        // Title failures cannot change the outcome of the user's turn.
        let _ = crate::chat_titles::enqueue(s, id).await;
    }
    Ok(exhausted)
}

async fn record_raw(
    s: &Service,
    id: &str,
    kind: &str,
    raw: &str,
    secrets: &[String],
    log_total: &mut usize,
) -> Result<()> {
    if *log_total >= LOG_BUDGET {
        return Ok(());
    }
    let redacted = run_output::redact(raw, secrets);
    let remaining = LOG_BUDGET.saturating_sub(*log_total);
    let recorded = &redacted[..redacted.floor_char_boundary(remaining.min(redacted.len()))];
    *log_total += recorded.len();
    if recorded.len() < redacted.len() {
        *log_total = LOG_BUDGET;
    }
    if recorded.is_empty() {
        return Ok(());
    }
    s.store.event(id, kind, recorded, None).await
}

/// Handles chat protocol messages, which are not run output.
async fn handle_chat_control(s: &Service, id: &str, event: &Value) -> Result<bool> {
    match text(event, "type") {
        "chat.question" => s.question_receive(id, event["question"].clone()).await?,
        "chat.question.closed" => {
            s.question_release(id, event["questionId"].as_str()).await?;
        }
        "chat.delivered" => {
            if let Some(message) = event["messageId"].as_str() {
                s.chat_acknowledge(id, message).await?;
            }
        }
        _ => return Ok(false),
    }
    Ok(true)
}

/// Updates the checkpoint and run from an agent event.
async fn apply_event(
    s: &Service,
    id: &str,
    event: &Value,
    checkpoint: &Checkpoint,
    secrets: &[String],
) -> Result<()> {
    let failure = text(&event["error"], "message");
    let provider_failure = event["type"] == "turn.failed";
    let controller_failure = event["type"] == "runner.failed";
    if (provider_failure || controller_failure)
        && !failure.is_empty()
        && checkpoint.read(|c| c.last_error().is_empty()).await
    {
        let failure = truncate(&run_output::redact(failure, secrets), ERROR_LIMIT);
        let mut error = run_output::payload(&event["error"], secrets);
        error["message"] = failure.clone().into();
        let cause = if provider_failure {
            crate::run_retry::Cause::classify(&error)
        } else {
            None
        };
        checkpoint
            .update(|c| {
                c.last_error = Some(Some(failure));
                c.retry_cause = Some(cause);
                if provider_failure {
                    c.completed = Some(false);
                }
            })
            .await?;
    }
    if event["type"] == "thread.started" && event["thread_id"].is_string() {
        let patch = serde_json::json!({ "sessionId": event["thread_id"], "resumeAvailable": true });
        s.store.patch_run(id, patch).await?;
    }
    // The latest occupancy of the native session, shown before its history loads.
    if event["type"] == "context.usage" && event["usage"]["contextTokens"].is_u64() {
        let patch = serde_json::json!({ "contextUsage": event["usage"] });
        s.store.patch_run(id, patch).await?;
    }
    if event["item"]["type"] == "agent_message" && event["item"]["text"].is_string() {
        let message = truncate(
            &run_output::redact(text(&event["item"], "text"), secrets),
            MESSAGE_LIMIT,
        );
        checkpoint
            .remember(|c| c.last_message = Some(message))
            .await;
    }
    if event["type"] == "turn.completed" {
        checkpoint.update(|c| c.completed = Some(true)).await?;
        let run = s.store.run(id).await?;
        let outcome = &run["outcome"];
        if outcome["status"] == "completed"
            && outcome["messageId"].is_string()
            && outcome["messageId"] == *crate::waits::execution_id(&run)
        {
            s.question_release(id, None).await?;
        }
        if !event["usage"].is_null() {
            let patch = serde_json::json!({ "usage": event["usage"] });
            s.store.patch_run(id, patch).await?;
        }
    }
    Ok(())
}

/// The notice a turn's speed check needs: every faster turn states how it runs, since the picker
/// and queue judge from cached catalogs. Each attempt of a turn checks again, on whichever account
/// it runs: only a change of outcome for the message is shown, so retries do not repeat a notice
/// and a later attempt that gets the speed after all says so.
async fn speed_notice(
    s: &Service,
    id: &str,
    event: &Value,
) -> Result<(Option<&'static str>, Value)> {
    let speed = &event["speed"];
    let outcome = if speed["confirmed"] == true {
        "confirmed"
    } else if text(speed, "applied") == crate::models::STANDARD_SPEED {
        "standard"
    } else {
        "unconfirmed"
    };
    let key = format!("turn-speed:{id}");
    let earlier = s
        .store
        .kv(&key)
        .await?
        .filter(|earlier| earlier["messageId"] == event["messageId"])
        .map(|earlier| text(&earlier, "outcome").to_owned());
    let record = json!({ "messageId": event["messageId"], "outcome": outcome });

    let message = match (outcome, earlier.as_deref()) {
        (outcome, Some(earlier)) if outcome == earlier => None,
        ("confirmed", None) => Some(
            "Codex lists the selected speed for this model here, so this turn asks for it. The account’s plan decides whether it applies.",
        ),
        ("confirmed", Some(_)) => Some(
            "Codex now lists the selected speed for this model here, so this turn asks for it. The account’s plan decides whether it applies.",
        ),
        ("standard", _) => Some(
            "Codex doesn’t list the selected speed for this model here, so this turn runs at Standard speed.",
        ),
        _ => Some(
            "The selected speed couldn’t be confirmed for this account and model. This turn asks for it but may run at Standard speed.",
        ),
    };
    Ok((message, record))
}

async fn store_event(s: &Service, id: &str, raw: &str, event: &Value) -> Result<()> {
    let value = event["item"]
        .get("text")
        .or_else(|| event["item"].get("aggregated_output"))
        .or_else(|| event["error"].get("message"));
    let value = value.map_or_else(
        || raw.to_owned(),
        |v| v.as_str().map_or_else(|| v.to_string(), str::to_owned),
    );
    let kind = match text(event, "type") {
        "" => "output",
        kind => kind,
    };
    s.store.event(id, kind, &value, Some(event.clone())).await
}

fn truncate(value: &str, limit: usize) -> String {
    value.chars().take(limit).collect()
}

/// A redacted notice cut to its limit, never inside a redaction marker, marked as cut.
fn cut_notice(message: &str) -> String {
    const MARKER: &str = "[redacted]";
    if message.chars().count() <= NOTICE_LIMIT {
        return message.to_owned();
    }
    let mut cut = truncate(message, NOTICE_LIMIT - 1);
    if let Some(start) = cut.rfind('[')
        && message[start..].starts_with(MARKER)
        && start + MARKER.len() > cut.len()
    {
        cut.truncate(start);
    }
    cut.push('…');
    cut
}

#[cfg(test)]
mod tests;
