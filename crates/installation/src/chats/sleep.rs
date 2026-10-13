//! Durable continuation of the same conversation, independent of its native process.
use super::{chat, chat_for_run};
use crate::{
    config::{id, now},
    conversation_lifecycle::{is_active, require_active},
    error::{Error, Result},
    run_status::RunStatus,
    service::Service,
    store::Db,
    validation::text,
};
use serde_json::{Value, json};

pub const INSTRUCTIONS: &str = "When waiting for a remote result before continuing the current task, \
    call cairn_workspace.sleep with a delay in seconds and a concrete reason, then end this turn. \
    Cairn saves the wakeup and resumes this same conversation automatically after the deadline \
    and after the current execution has stopped. Only promise to return after the tool confirms \
    the wakeup. This is a scheduled continuation, not task completion or a request for user input; \
    no outcome report is needed when ending a turn for this wait. Do not use sleep while local \
    background work must keep running: wait for those tasks with the native tools instead. \
    A new user message replaces the scheduled wakeup. Pausing the conversation holds the wakeup; \
    stopping or deleting it cancels it. At wakeup, continue the original authorized task and \
    verify the external result before repeating actions.";

pub fn tool() -> Value {
    json!({
        "name": "sleep",
        "description": "Schedule an automatic continuation of this conversation in 1 to 86400 seconds. \
            The wakeup is durable across process/VM shutdown and installation restart. Call only \
            when waiting for a remote result; local background processes must use native waiting \
            tools. After confirmation, end this turn so execution resources can be released. \
            The continuation runs after this turn succeeds and its execution stops, no earlier \
            than wakeAt, subject to available capacity. A new user message replaces the wakeup; \
            pause holds it, stop/delete cancel it. Repeating the same arguments in this turn \
            returns the same wakeup without extending the delay; different arguments replace it.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "seconds": { "type": "integer", "minimum": 1, "maximum": 86400 },
                "reason": { "type": "string", "minLength": 1, "maxLength": 1000 },
            },
            "required": ["seconds", "reason"],
            "additionalProperties": false,
        },
    })
}

pub async fn schedule(s: &Service, bearer: &str, input: &Value) -> Result<Value> {
    if !jsonschema::is_valid(&tool()["inputSchema"], input)
        || text(input, "reason").trim().is_empty()
    {
        return Err(Error::bad(
            "Provide 1 to 86400 seconds and a concrete wait reason.",
        ));
    }
    // JSON Schema also accepts integral floats such as 1.0. The validated
    // range is small enough to convert them exactly to a deadline duration.
    let seconds = input["seconds"]
        .as_f64()
        .ok_or_else(|| Error::bad("Invalid wait duration."))? as i64;
    let bearer = bearer.to_owned();
    let mut input = crate::run_output::payload(input, &[]);
    input["seconds"] = seconds.into();
    let result = s
        .store
        .transaction(move |db| {
            let run = crate::project_workspaces::authorize_in(db, &bearer)?;
            let mut chat = chat_for_run(db, text(&run, "id"))?.ok_or_else(|| {
                Error::conflict("Scheduled waiting requires an interactive conversation.")
            })?;
            require_active(&chat)?;
            if chat["paused"] == true || has_pending_input(db, &chat)? {
                return Err(Error::conflict(
                    "Handle pending user input before scheduling a wakeup.",
                ));
            }
            let previous = &chat["wakeup"];
            let same_request = previous.is_object()
                && previous["messageId"] == run["chatExecution"]["messageId"]
                && previous["seconds"] == input["seconds"]
                && previous["reason"] == input["reason"];
            if same_request {
                return Ok(previous.clone());
            }
            let created = now();
            let wakeup = json!({
                "id": id(),
                "messageId": run["chatExecution"]["messageId"],
                "seconds": input["seconds"],
                "reason": input["reason"],
                "createdAt": created,
                "wakeAt": created + seconds * 1000,
            });
            chat["wakeup"] = wakeup.clone();
            chat["updatedAt"] = created.into();
            db.put("chats", &chat)?;
            db.event(
                text(&run, "id"),
                "chat.sleep",
                text(&wakeup, "reason"),
                Some(&wakeup),
            )?;
            Ok(wakeup)
        })
        .await?;
    s.worker.notify();
    Ok(result)
}

fn has_pending_input(db: &Db<'_>, chat: &Value) -> Result<bool> {
    Ok(db
        .messages(text(chat, "id"))?
        .iter()
        .any(|message| matches!(text(message, "status"), "queued" | "sending")))
}

/// Rechecked in the launch transaction after async snapshot preparation.
pub(super) fn ready(db: &Db<'_>, chat: &Value, run: Option<&Value>, time: i64) -> Result<bool> {
    let wakeup = &chat["wakeup"];
    let eligible = is_active(chat)
        && chat["paused"] != true
        && chat["sessionRestartRequested"] != true
        && wakeup["wakeAt"]
            .as_i64()
            .is_some_and(|deadline| deadline <= time)
        && run.is_some_and(|run| {
            run["status"] == RunStatus::Succeeded
                && run["cancelRequestedAt"].is_null()
                && run["recoveryPending"] != true
                && run["workspaceCleanedAt"].is_null()
                && run["chatExecution"]["messageId"] == wakeup["messageId"]
        });
    Ok(eligible && !has_pending_input(db, chat)?)
}

pub(super) fn message(
    db: &Db<'_>,
    chat: &Value,
    run: Option<&Value>,
    time: i64,
) -> Result<Option<Value>> {
    if !ready(db, chat, run, time)? {
        return Ok(None);
    }
    Ok(Some(json!({
        "id": chat["wakeup"]["id"],
        "text": format!("Cairn resumed this conversation after the scheduled wait: {}. \
            Continue the original authorized task from the saved conversation and current workspace. \
            Check the external result now. Preserve completed work and verify external effects \
            before repeating any action. The wakeup is not a new user instruction or additional authorization.", text(&chat["wakeup"], "reason")),
        "attachments": [],
        "wakeup": chat["wakeup"],
    })))
}

impl Service {
    pub(crate) async fn chat_cancel_sleep(&self, id: &str) -> Result<()> {
        let id = id.to_owned();
        self.store.write(move |db| cancel(db, &id)).await
    }
}

pub(super) fn cancel(db: &Db<'_>, id: &str) -> Result<()> {
    let mut chat = chat(db, id)?;
    if chat["wakeup"].is_object() {
        chat["wakeup"] = Value::Null;
        db.put("chats", &chat)?;
    }
    Ok(())
}

pub(crate) fn cancel_for_run(db: &Db<'_>, run: &str) -> Result<bool> {
    if let Some(chat) = chat_for_run(db, run)? {
        let waiting = chat["wakeup"].is_object();
        cancel(db, text(&chat, "id"))?;
        return Ok(waiting);
    }
    Ok(false)
}
