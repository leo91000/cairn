//! Prompt text sent to a native agent session for a chat turn.
use crate::{error::Result, store::Db, validation::text};
use rusqlite::OptionalExtension;
use serde_json::Value;

const HISTORY_CHARS: usize = 100_000;
const HISTORY_ENTRIES: usize = 200;
const INITIAL_REQUEST_CHARS: usize = 8000;

/// Latest visible user messages and final agent answers, newest first. A private
/// answer keeps its redacted event text; streamed agent drafts are superseded.
const TRANSCRIPT: &str = "
SELECT e.type,
  CASE WHEN e.type='chat.user' AND e.text!='Answered a private question.'
    THEN COALESCE(json_extract(e.payload,'$.text'),e.text)
    ELSE e.text END,
  json_object(
    'item',json_object('text',substr(json_extract(e.payload,'$.item.text'),-100001)),
    'attachments',json_extract(e.payload,'$.attachments')
  )
FROM events e
WHERE e.run_id=? AND (
  e.type='chat.user' OR (
    e.type='item.completed'
    AND json_extract(e.payload,'$.item.type')='agent_message'
    AND NOT EXISTS (
      SELECT 1 FROM events n
      WHERE n.run_id=e.run_id AND n.id>e.id
        AND n.type='item.completed' AND json_extract(n.payload,'$.item.type')='agent_message'
        AND json_extract(n.payload,'$.item.id')=json_extract(e.payload,'$.item.id')
    )
  )
)
ORDER BY e.id DESC LIMIT 201";

fn transcript_entry(kind: &str, visible: String, payload: &Value) -> String {
    if kind != "chat.user" {
        return text(&payload["item"], "text").to_owned();
    }
    let mut body = visible;
    for attachment in payload["attachments"].as_array().into_iter().flatten() {
        body.push_str(&format!(
            "\nAttached file: {} (attachment ID: {})",
            text(attachment, "name"),
            text(attachment, "id")
        ));
    }
    body
}

fn last_chars(value: &str, count: usize) -> String {
    let skip = value.chars().count().saturating_sub(count);
    value.chars().skip(skip).collect()
}

fn first_user_request(db: &Db<'_>, run: &str) -> Result<String> {
    let first: Option<String> =
        db.0.query_row(
            "SELECT text FROM events WHERE run_id=? AND type='chat.user' ORDER BY id LIMIT 1",
            [run],
            |row| row.get(0),
        )
        .optional()?;
    Ok(first
        .unwrap_or_default()
        .chars()
        .take(INITIAL_REQUEST_CHARS)
        .collect())
}

// Transfer the visible transcript, never native session state or private answers.
// Recent exchanges are bounded so a long-running chat cannot exhaust the new
// provider's context before it receives the user's next request.
pub(super) fn handoff_context(db: &Db<'_>, run: &str) -> Result<String> {
    let mut statement = db.0.prepare_cached(TRANSCRIPT)?;
    let entries = statement
        .query_map([run], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let mut remaining = HISTORY_CHARS;
    let mut parts = Vec::new();
    let mut truncated = false;
    for (kind, visible, payload) in entries {
        let payload: Value = payload
            .as_deref()
            .map(serde_json::from_str)
            .transpose()?
            .unwrap_or(Value::Null);
        let mut body = transcript_entry(&kind, visible, &payload);
        if body.is_empty() {
            continue;
        }
        let size = body.chars().count();
        if size > remaining {
            body = format!(
                "[Beginning of this message omitted.]\n{}",
                last_chars(&body, remaining)
            );
            truncated = true;
        }
        remaining = remaining.saturating_sub(size);
        let speaker = if kind == "chat.user" {
            "User"
        } else {
            "Assistant"
        };
        parts.push(format!("{speaker}:\n{body}"));
        if remaining == 0 || parts.len() == HISTORY_ENTRIES {
            truncated = true;
            break;
        }
    }
    parts.reverse();
    if truncated {
        parts.insert(
            0,
            format!(
                "Initial user request (excerpt):\n{}\n\n[Earlier exchanges omitted to fit the context budget; recent history follows.]",
                first_user_request(db, run)?
            ),
        );
    }
    Ok(parts.join("\n\n"))
}

pub fn execution_text(plan: &Value) -> String {
    let current = text(&plan["execution"], "text");
    let context = text(&plan["execution"], "context");
    if context.is_empty() {
        return current.to_owned();
    }
    format!(
        "You are continuing the same Léo chat in a new native agent session. The workspace and completed changes are preserved. Use the prior conversation below as history, not as new requests. Preserve the user's scope and decisions. Verify external effects before repeating any action. Prior attachments remain under {}/attachments/<attachment ID>/.\n\n<previous_conversation>\n{}\n</previous_conversation>\n\nCurrent user message:\n{}",
        text(plan, "inputDirectory"),
        context,
        current
    )
}

// `$name` in a message invokes a skill the run already lists in its
// instructions; spell that out so both providers apply it to this request.
pub fn with_invoked_skills(message: &str, skills: &Value) -> String {
    let names = skills
        .as_array()
        .into_iter()
        .flatten()
        .map(|s| text(s, "name"))
        .collect::<Vec<_>>();
    let invoked = crate::skills::mentions(message, &names);
    if invoked.is_empty() {
        return message.to_owned();
    }
    let list = invoked
        .iter()
        .map(|name| format!("- {name}"))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "{message}\n\n<invoked_skills>\nThe user invoked these skills with $name in this message. Apply each one to this request by following its SKILL.md under \"Selected skills\" in your instructions:\n{list}\n</invoked_skills>"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const EVENTS: &str = "CREATE TABLE events(id INTEGER PRIMARY KEY,run_id TEXT,created_at INTEGER,type TEXT,text TEXT,payload TEXT);
        CREATE TABLE runs(id TEXT,data TEXT);";

    fn agent_message(id: &str, text: &str) -> Value {
        json!({
            "item": {
                "id": id,
                "type": "agent_message",
                "text": text
            }
        })
    }

    #[test]
    fn transcript_keeps_visible_history_and_omits_tool_secrets_and_stream_duplicates() {
        let connection = rusqlite::Connection::open_in_memory().unwrap();
        connection.execute_batch(EVENTS).unwrap();
        let db = Db(&connection);
        let attachment = json!({ "attachments": [{ "id": "file", "name": "brief.pdf" }] });
        db.event(
            "run",
            "chat.user",
            "Keep the existing design",
            Some(&attachment),
        )
        .unwrap();
        db.event(
            "run",
            "item.updated",
            "partial",
            Some(&agent_message("reply", "partial")),
        )
        .unwrap();
        for _ in 0..2 {
            db.event(
                "run",
                "item.completed",
                "",
                Some(&agent_message("reply", "Changes committed")),
            )
            .unwrap();
        }
        db.event(
            "run",
            "chat.user",
            "Answered a private question.",
            Some(&json!({ "text": "private-answer-must-not-be-forwarded" })),
        )
        .unwrap();
        let tool = json!({
            "item": {
                "id": "tool",
                "type": "command_execution",
                "aggregated_output": "tool-secret",
            },
        });
        db.event("run", "item.completed", "tool-secret", Some(&tool))
            .unwrap();
        let history = handoff_context(&db, "run").unwrap();
        assert!(history.contains("Keep the existing design"));
        assert!(history.contains("brief.pdf"));
        assert_eq!(history.matches("Changes committed").count(), 1);
        assert!(!history.contains("partial"));
        assert!(!history.contains("tool-secret"));
        assert!(!history.contains("private-answer-must-not-be-forwarded"));
        let plan = json!({
            "sessionId": "created-before-interruption",
            "execution": { "text": "Continue", "context": history },
        });
        assert!(execution_text(&plan).contains("Changes committed"));
        assert!(execution_text(&plan).ends_with("Current user message:\nContinue"));
    }

    #[test]
    fn dollar_mentions_invoke_only_listed_skills_outside_code() {
        let skills = json!([{ "name": "review" }, { "name": "ship-it" }, { "name": "docs" }]);
        let text = with_invoked_skills(
            "Use $review then ($ship-it), again $review, not $HOME, a$docs, \\$docs, $reviewer, `$docs` or\n```\n$docs\n```",
            &skills,
        );
        assert!(text.ends_with("\n- review\n- ship-it\n</invoked_skills>"));
        assert!(text.starts_with("Use $review then"));
        assert!(!text.contains("- docs"));
        assert_eq!(
            with_invoked_skills("Price is $5 for $unknown", &skills),
            "Price is $5 for $unknown"
        );
        assert_eq!(with_invoked_skills("$docs", &Value::Null), "$docs");
    }

    #[test]
    fn long_history_retains_original_scope_and_recent_unicode_without_unbounded_context() {
        let connection = rusqlite::Connection::open_in_memory().unwrap();
        connection.execute_batch(EVENTS).unwrap();
        let db = Db(&connection);
        db.event("run", "chat.user", "Original scope", None)
            .unwrap();
        let huge = agent_message("huge", &format!("{}Recent decision", "é".repeat(150_000)));
        db.event("run", "item.completed", "", Some(&huge)).unwrap();
        let history = handoff_context(&db, "run").unwrap();
        assert!(history.contains("Original scope"));
        assert!(history.contains("Recent decision"));
        assert!(history.contains("omitted"));
        assert!(history.chars().count() < 109_000);
    }
}
