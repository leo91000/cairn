use super::checkpoint::{Checkpoint, RunCheckpoint};
use crate::{error::Result, run_output, service::Service, validation::text};
use serde_json::Value;
use tokio::{
    io::{AsyncRead, AsyncReadExt},
    sync::mpsc,
    time::Instant,
};

pub(super) struct Output {
    pub diagnostic: bool,
    pub raw: String,
    pub received: Instant,
}

impl Output {
    fn new(diagnostic: bool, raw: String) -> Self {
        Self {
            diagnostic,
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
const ITEM_DETAIL_LIMIT: usize = 16 * 1024;
const LINE_LIMIT: usize = 2_000_000;
// Match the resident Codex transport: supported tool frames must reach the JSON
// parser intact before shortening. Large buffers are released after each frame.
const TOOL_FRAME_LIMIT: usize = 100_000_000;
const MESSAGE_LIMIT: usize = 100_000;
const ERROR_LIMIT: usize = 10_000;

/// Event types that are always recorded. Clients use turn markers to scope reused message IDs.
const CONVERSATION_EVENTS: [&str; 5] = [
    "thread.started",
    "turn.started",
    "turn.completed",
    "turn.failed",
    "error",
];

// Separate pipe readers keep stderr draining even while stdout applies database backpressure.
pub(super) async fn read_output(
    mut reader: impl AsyncRead + Unpin,
    diagnostic: bool,
    events: mpsc::Sender<Output>,
    secrets: Vec<String>,
) {
    let mut bytes = [0; 8192];
    let mut line = Vec::new();
    let mut object_frame = None;
    loop {
        let n = match reader.read(&mut bytes).await {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        if diagnostic {
            let chunk = String::from_utf8_lossy(&bytes[..n]).into_owned();
            if events.send(Output::new(true, chunk)).await.is_err() {
                return;
            }
            continue;
        }
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
    if output.diagnostic {
        record_raw(s, id, "diagnostic", raw, secrets, log_total).await?;
        return Ok(false);
    }
    let Ok(event) = serde_json::from_str::<Value>(raw) else {
        record_raw(s, id, "output", raw, secrets, log_total).await?;
        return Ok(false);
    };
    if event["type"] == "runner.start_rejected" {
        let run = s.store.run(id).await?;
        let first_rejected = !previously_launched
            && run["sessionId"].is_null()
            && checkpoint.read(RunCheckpoint::firecracker).await;
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
        || CONVERSATION_EVENTS.contains(&text(&event, "type"));
    let activity_item = text(&event, "type").starts_with("item.") && event["item"].is_object();
    if conversation || activity_item || *log_total < LOG_BUDGET {
        let abbreviated = !conversation && activity_item && raw.len() > ITEM_DETAIL_LIMIT;
        let stored = if abbreviated {
            run_output::compact_activity(&event, secrets)
        } else {
            run_output::payload(&event, secrets)
        };
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
    if event["type"] == "turn.failed"
        && !failure.is_empty()
        && checkpoint.read(|c| c.last_error().is_empty()).await
    {
        let failure = truncate(&run_output::redact(failure, secrets), ERROR_LIMIT);
        let mut error = run_output::payload(&event["error"], secrets);
        error["message"] = failure.clone().into();
        let cause = crate::run_retry::Cause::classify(&error);
        checkpoint
            .update(|c| {
                c.last_error = Some(Some(failure));
                c.retry_cause = Some(cause);
                c.completed = Some(false);
            })
            .await?;
    }
    if event["type"] == "thread.started" && event["thread_id"].is_string() {
        let patch = serde_json::json!({ "sessionId": event["thread_id"], "resumeAvailable": true });
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
        if !event["usage"].is_null() {
            let patch = serde_json::json!({ "usage": event["usage"] });
            s.store.patch_run(id, patch).await?;
        }
    }
    Ok(())
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

#[cfg(test)]
mod tests;
