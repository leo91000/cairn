use super::checkpoint::Checkpoint;
use crate::{error::Result, run_output, service::Service, validation::text};
use serde_json::Value;
use tokio::{
    io::{AsyncRead, AsyncReadExt},
    sync::mpsc,
};

/// Output recorded per run before tool and diagnostic events are dropped.
const OUTPUT_BUDGET: usize = 5_000_000;
const LINE_LIMIT: usize = 2_000_000;
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
    events: mpsc::Sender<(bool, String)>,
) {
    let mut bytes = [0; 8192];
    let mut line = Vec::new();
    loop {
        let n = match reader.read(&mut bytes).await {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        if diagnostic {
            let chunk = String::from_utf8_lossy(&bytes[..n]).into_owned();
            if events.send((true, chunk)).await.is_err() {
                return;
            }
            continue;
        }
        for byte in &bytes[..n] {
            if *byte != b'\n' {
                if line.len() < LINE_LIMIT {
                    line.push(*byte);
                }
                continue;
            }
            let complete = String::from_utf8_lossy(&line).into_owned();
            if events.send((false, complete)).await.is_err() {
                return;
            }
            line.clear();
        }
    }
    if !line.is_empty() {
        // The receiver only disappears once the run stopped listening.
        let _ = events
            .send((diagnostic, String::from_utf8_lossy(&line).into_owned()))
            .await;
    }
}

/// Records one output line and returns whether it reports exhausted usage.
pub(super) async fn record(
    s: &Service,
    id: &str,
    raw: &str,
    diagnostic: bool,
    checkpoint: &Checkpoint,
    secrets: &[String],
    total: &mut usize,
) -> Result<bool> {
    if diagnostic {
        record_raw(s, id, "diagnostic", raw, secrets, total).await?;
        return Ok(false);
    }
    let Ok(event) = serde_json::from_str::<Value>(raw) else {
        record_raw(s, id, "output", raw, secrets, total).await?;
        return Ok(false);
    };
    if handle_chat_control(s, id, &event).await? {
        return Ok(false);
    }
    let exhausted = run_output::exhausted(&event);
    apply_event(s, id, &event, checkpoint, secrets).await?;
    // Tool/diagnostic volume must never hide the conversation or its outcome.
    let conversation = event["item"]["type"] == "agent_message"
        || CONVERSATION_EVENTS.contains(&text(&event, "type"));
    if conversation || *total < OUTPUT_BUDGET {
        if !conversation {
            *total += raw.len();
        }
        store_event(s, id, raw, &event, secrets).await?;
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
    total: &mut usize,
) -> Result<()> {
    if *total >= OUTPUT_BUDGET {
        return Ok(());
    }
    *total += raw.len();
    s.store
        .event(id, kind, &run_output::redact(raw, secrets), None)
        .await
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
        checkpoint
            .update(|c| c.last_error = Some(Some(failure)))
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

async fn store_event(
    s: &Service,
    id: &str,
    raw: &str,
    event: &Value,
    secrets: &[String],
) -> Result<()> {
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
    s.store
        .event(
            id,
            kind,
            &run_output::redact(&value, secrets),
            Some(run_output::payload(event, secrets)),
        )
        .await
}

fn truncate(value: &str, limit: usize) -> String {
    value.chars().take(limit).collect()
}
