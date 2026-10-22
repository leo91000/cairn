//! Named, durable conditions that resume an existing conversation without keeping its VM active.
//! Cairn provides time waits and run dependencies; plugins granted to the agent add
//! theirs (see `plugins::conditions`).
use super::{chat_for_run, continuation};
use crate::{
    config::{id, now},
    conversation_lifecycle::require_active,
    error::{Error, Result},
    plugins::conditions,
    run_status::RunStatus,
    service::{Service, covers, policy},
    store::Db,
    validation::{text, uuid},
};
use chrono::{DateTime, SecondsFormat, Utc};
use serde_json::{Value, json};
use std::collections::HashSet;

fn run_finished() -> Value {
    json!({
        "type": "object",
        "properties": {
            "type": { "const": "run_finished" },
            "runId": { "type": "string", "format": "uuid" },
        },
        "required": ["type", "runId"],
        "additionalProperties": false,
    })
}

fn delay() -> Value {
    json!({
        "type": "object",
        "description": "Resume after 1 to 86400 seconds. Do not provide timeoutSeconds.",
        "properties": {
            "type": { "const": "delay" },
            "seconds": { "type": "integer", "minimum": 1, "maximum": 86400 },
        },
        "required": ["type", "seconds"],
        "additionalProperties": false,
    })
}

fn at() -> Value {
    json!({
        "type": "object",
        "description": "Resume at an RFC 3339 date with an explicit timezone, at most 24 hours ahead. A past date completes immediately. Do not provide timeoutSeconds.",
        "properties": {
            "type": { "const": "at" },
            "date": { "type": "string", "format": "date-time" },
        },
        "required": ["type", "date"],
        "additionalProperties": false,
    })
}

fn temporal(condition: &Value) -> bool {
    matches!(text(condition, "type"), "delay" | "at")
}

fn date_millis(condition: &Value) -> Result<i64> {
    let date = DateTime::parse_from_rfc3339(text(condition, "date"))
        .map_err(|_| Error::bad("Provide an RFC 3339 date with an explicit timezone."))?;
    // Round up to the scheduler's millisecond precision: never resume before the requested date.
    let extra_millisecond = i64::from(!date.timestamp_subsec_nanos().is_multiple_of(1_000_000));
    Ok(date.timestamp_millis() + extra_millisecond)
}

/// The single wait tool: native time/run conditions and conditions of granted plugins.
pub fn tool(plugin_conditions: &[Value]) -> Value {
    let mut conditions = vec![delay(), at(), run_finished()];
    conditions.extend(plugin_conditions.iter().cloned());
    json!({
        "name": "wait_for",
        "description": "Wait durably for a duration (delay), a date (at), an authorized Cairn run \
            to finish, or a condition provided by a plugin granted to this agent. Time conditions \
            take no timeoutSeconds; external conditions require it (1 to 86400 seconds). \
            If already complete, returns deferred=false and the observation: continue this turn. \
            Otherwise saves a wakeup and returns deferred=true: end this turn so execution resources \
            can be released. Cairn checks the condition without waking the LLM and resumes this \
            conversation when the time or result arrives or the timeout expires, subject to capacity. \
            A time reached is completed, not timed_out. External completion includes failure and \
            cancellation; inspect the result before acting. Never use this for local background \
            processes that need this execution. Run dependencies require installation access; plugin \
            conditions use the agent's plugin grants. One wait is stored per conversation. Identical \
            repeated requests preserve the deadline; a different request replaces the previous wait. \
            Queued follow-ups start after the continuation. A new user message, stop, deletion, \
            terminal turn failure or manual run resume cancels the wait; queue pause holds it.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "condition": { "oneOf": conditions },
                "timeoutSeconds": { "type": "integer", "minimum": 1, "maximum": 86400 },
                "reason": { "type": "string", "minLength": 1, "maxLength": 1000 },
            },
            "required": ["condition", "reason"],
            "allOf": [{
                "if": {
                    "properties": {
                        "condition": { "properties": { "type": { "enum": ["delay", "at"] } } },
                    },
                },
                "then": { "not": { "required": ["timeoutSeconds"] } },
                "else": { "required": ["timeoutSeconds"] },
            }],
            "additionalProperties": false,
        },
    })
}

/// Check native inputs or validate against the granted plugin's condition schema.
async fn input(s: &Service, bearer: &str, input: &Value) -> Result<Value> {
    let base = json!({
        "type": "object",
        "properties": {
            "condition": { "type": "object" },
            "timeoutSeconds": { "type": "integer", "minimum": 1, "maximum": 86400 },
            "reason": { "type": "string", "minLength": 1, "maxLength": 1000 },
        },
        "required": ["condition", "reason"],
        "additionalProperties": false,
    });
    let invalid = || {
        Error::bad(
            "Provide a supported condition and a concrete reason. Time conditions take no timeoutSeconds; external conditions require 1 to 86400 seconds.",
        )
    };
    if !jsonschema::is_valid(&base, input) || text(input, "reason").trim().is_empty() {
        return Err(invalid());
    }
    let mut input = crate::run_output::payload(input, &[]);
    let run = crate::project_workspaces::authorize(s, bearer).await?;
    if temporal(&input["condition"]) {
        let schema = if input["condition"]["type"] == "delay" {
            delay()
        } else {
            at()
        };
        if input.get("timeoutSeconds").is_some()
            || !jsonschema::is_valid(&schema, &input["condition"])
        {
            return Err(invalid());
        }
        if input["condition"]["type"] == "delay" {
            input["condition"]["seconds"] =
                (input["condition"]["seconds"].as_f64().unwrap() as i64).into();
        } else {
            let deadline = date_millis(&input["condition"])?;
            if deadline > now() + 86_400_000 {
                return Err(Error::bad("The date must be at most 24 hours ahead."));
            }
            input["condition"]["date"] = DateTime::<Utc>::from_timestamp_millis(deadline)
                .ok_or_else(invalid)?
                .to_rfc3339_opts(SecondsFormat::Millis, true)
                .into();
        }
        return Ok(input);
    }
    let Some(timeout) = input["timeoutSeconds"].as_f64() else {
        return Err(invalid());
    };
    input["timeoutSeconds"] = (timeout as i64).into();
    let agent_id = text(&run["snapshot"]["agent"], "id");
    match conditions::normalize(s, agent_id, &input["condition"]).await? {
        Some(condition) => input["condition"] = condition,
        None => {
            if !jsonschema::is_valid(&run_finished(), &input["condition"]) {
                return Err(invalid());
            }
            uuid(text(&input["condition"], "runId"))?;
        }
    }
    Ok(input)
}

fn same_request(previous: &Value, run: &Value, input: &Value) -> bool {
    let condition = &input["condition"];
    let same_condition = if temporal(condition) {
        !previous["condition"].is_object()
            && if previous["timeCondition"].is_object() {
                previous["timeCondition"] == *condition
            } else {
                // Existing sleep records only describe a duration.
                condition["type"] == "delay" && previous["seconds"] == condition["seconds"]
            }
    } else {
        previous["condition"] == *condition && previous["seconds"] == input["timeoutSeconds"]
    };
    previous.is_object()
        && previous["messageId"] == run["chatExecution"]["messageId"]
        && same_condition
        && previous["reason"] == input["reason"]
}

fn time_result(wake_at: i64) -> Value {
    json!({ "state": "completed", "type": "time", "wakeAt": wake_at })
}

fn input_revision(db: &Db<'_>, run: &Value, chat: &Value) -> Result<(i64, Vec<String>)> {
    let acknowledged = db.0.query_row(
        "SELECT COALESCE(MAX(id),0) FROM events WHERE run_id=? AND type='chat.user'",
        [text(run, "id")],
        |row| row.get(0),
    )?;

    // A new queued message cancels a wakeup before producing a chat.user event.
    // Keep submissions in the baseline so an in-flight probe cannot undo that.
    let submitted =
        db.0.prepare("SELECT id FROM chat_messages WHERE chat_id=? ORDER BY id")?
            .query_map([text(chat, "id")], |row| row.get(0))?
            .collect::<rusqlite::Result<Vec<String>>>()?;

    Ok((acknowledged, submitted))
}

fn registering(db: &Db<'_>, bearer: &str, input: &Value) -> Result<(Value, Value)> {
    let run = crate::project_workspaces::authorize_in(db, bearer)?;
    let chat = chat_for_run(db, text(&run, "id"))?
        .ok_or_else(|| Error::conflict("Waiting requires an interactive conversation."))?;
    require_active(&chat)?;
    if chat["paused"] == true || continuation::has_unhandled_input(db, &chat)? {
        return Err(Error::conflict(
            "Handle pending user input before registering a wait.",
        ));
    }
    if !temporal(&input["condition"]) {
        access(db, &run, &input["condition"])?;
    }
    if input["condition"]["type"] == "run_finished" {
        crate::installation_mcp::authorize_in(db, bearer)?;
    }
    if input["condition"]["type"] == "run_finished"
        && run_result(db, &input["condition"])?.is_none()
    {
        check_dependency(db, text(&run, "id"), text(&input["condition"], "runId"))?;
    }
    Ok((run, chat))
}

fn check_dependency(db: &Db<'_>, caller: &str, target: &str) -> Result<()> {
    let mut target = target.to_owned();
    let mut visited = HashSet::new();
    for _ in 0..32 {
        if target == caller || !visited.insert(target.clone()) {
            return Err(Error::conflict(
                "This wait would create a dependency cycle.",
            ));
        }
        let Some(chat) = chat_for_run(db, &target)? else {
            return Ok(());
        };
        let next = &chat["wakeup"]["condition"];
        if next["type"] != "run_finished" {
            return Ok(());
        }
        target = text(next, "runId").to_owned();
    }
    Err(Error::bad("Wait dependency chains are limited to 32 runs."))
}

/// Rechecks the durable authorization after the turn's live MCP grant is released.
pub(crate) fn access(db: &Db<'_>, run: &Value, condition: &Value) -> Result<()> {
    let agent = &run["snapshot"]["agent"];
    let current = db
        .get("agents", text(agent, "id"))?
        .ok_or_else(|| Error::forbidden("The waiting agent no longer exists."))?;
    if !covers(&current, agent) {
        return Err(Error::forbidden("Agent permissions were reduced."));
    }
    match text(condition, "type") {
        "run_finished" => {
            if policy(agent)["installation"] != true {
                return Err(Error::forbidden(
                    "Waiting for another run requires installation access.",
                ));
            }
            if condition["runId"] == run["id"] {
                return Err(Error::bad("A conversation cannot wait for its own run."));
            }
            db.run(text(condition, "runId"))?
                .ok_or_else(|| Error::not_found("The target run no longer exists."))?;
        }
        _ if conditions::is_condition(condition) => conditions::access(db, run, condition)?,
        _ => return Err(Error::bad("Unsupported wait condition.")),
    }
    Ok(())
}

fn run_result(db: &Db<'_>, condition: &Value) -> Result<Option<Value>> {
    let target = db
        .run(text(condition, "runId"))?
        .ok_or_else(|| Error::not_found("The target run no longer exists."))?;
    let status =
        RunStatus::of(&target).ok_or_else(|| Error::bad_gateway("Unknown target run state."))?;
    if status == RunStatus::Succeeded
        && target["trigger"] == "chat"
        && let Some(chat) = chat_for_run(db, text(&target, "id"))?
        && (chat["wakeup"].is_object() || continuation::has_pending_input(db, &chat)?)
    {
        return Ok(None);
    }
    Ok((!status.is_active()).then(|| {
        json!({
            "state": "completed",
            "type": "run_finished",
            "runId": target["id"],
            "status": status,
            "outcomeStatus": target["outcome"]["status"],
            "waitingOn": target["outcome"]["waitingOn"],
            "finishedAt": target["finishedAt"],
        })
    }))
}

pub async fn schedule(s: &Service, bearer: &str, raw: &Value) -> Result<Value> {
    let input = input(s, bearer, raw).await?;
    let token = bearer.to_owned();
    let request = input.clone();
    // Keep the input checks and revision atomic: an acknowledged steering
    // message must not become the baseline of an already-started registration.
    let (run, chat, revision) = s
        .store
        .transaction(move |db| {
            let (run, chat) = registering(db, &token, &request)?;
            let revision = input_revision(db, &run, &chat)?;
            Ok((run, chat, revision))
        })
        .await?;
    if same_request(&chat["wakeup"], &run, &input) {
        return Ok(json!({ "deferred": true, "wakeup": chat["wakeup"] }));
    }
    let (observed, display) = if conditions::is_condition(&input["condition"]) {
        let observed = conditions::observe(s, &run, &input["condition"]).await?;
        // A condition that cannot be observed is refused rather than awaited.
        if let Some(result) = &observed.result
            && result["state"] == "unavailable"
        {
            return Err(Error::conflict(text(result, "message").to_owned()));
        }
        (observed.result, observed.display)
    } else {
        (None, None)
    };
    let token = bearer.to_owned();
    let result = s
        .store
        .transaction(move |db| {
            let (run, mut chat) = registering(db, &token, &input)?;
            if input_revision(db, &run, &chat)? != revision {
                return Err(Error::conflict("User input changed while the condition was checked. Handle it before registering a wait."));
            }
            if same_request(&chat["wakeup"], &run, &input) {
                return Ok(json!({ "deferred": true, "wakeup": chat["wakeup"] }));
            }
            let created = now();
            let condition = &input["condition"];
            let deadline = if condition["type"] == "at" {
                date_millis(condition)?
            } else {
                let seconds = if condition["type"] == "delay" {
                    condition["seconds"].as_i64().unwrap()
                } else {
                    input["timeoutSeconds"].as_i64().unwrap()
                };
                created + seconds * 1000
            };
            let observed = if condition["type"] == "at" && deadline <= created {
                Some(time_result(deadline))
            } else if condition["type"] == "run_finished" {
                run_result(db, condition)?
            } else {
                observed
            };
            if let Some(observed) = observed {
                continuation::cancel(db, text(&chat, "id"))?;
                // An immediate result has its own card; retain the text for older clients.
                // No wakeup records the reason when the condition is already complete.
                let mut announced = observed.clone();
                announced["reason"] = input["reason"].clone();
                announced["immediate"] = true.into();
                db.event(
                    text(&run, "id"),
                    "chat.wait_ready",
                    "Wait condition already completed",
                    Some(&announced),
                )?;
                return Ok(json!({ "deferred": false, "observation": observed }));
            }
            // Time waits keep the existing timer record (no condition), readable by older binaries.
            let seconds = (deadline - created + 999) / 1000;
            let mut wakeup = json!({
                "id": id(),
                "messageId": run["chatExecution"]["messageId"],
                "seconds": seconds,
                "reason": input["reason"],
                "createdAt": created,
                "wakeAt": deadline,
            });
            if temporal(condition) {
                // Additive request identity; older binaries still schedule this as a plain timer.
                wakeup["timeCondition"] = condition.clone();
            } else {
                wakeup["condition"] = condition.clone();
            }
            if conditions::is_condition(&input["condition"]) {
                wakeup["nextCheckAt"] = created.into();
                if let Some(display) = &display {
                    wakeup["display"] = display.clone();
                }
            }
            chat["wakeup"] = wakeup.clone();
            crate::nodes::exclusive::release_before_wait(db, text(&run, "id"))?;
            chat["updatedAt"] = created.into();
            db.put("chats", &chat)?;
            db.event(
                text(&run, "id"),
                "chat.wait",
                text(&wakeup, "reason"),
                Some(&wakeup),
            )?;
            Ok(json!({ "deferred": true, "wakeup": wakeup }))
        })
        .await?;
    s.worker.notify();
    Ok(result)
}

/// A current state, not a new instruction or permission to act on the target.
pub(super) fn observation(
    db: &Db<'_>,
    chat: &Value,
    run: &Value,
    time: i64,
) -> Result<Option<Value>> {
    let wakeup = &chat["wakeup"];
    let condition = &wakeup["condition"];
    if !condition.is_object() {
        return Ok(wakeup["wakeAt"]
            .as_i64()
            .filter(|deadline| *deadline <= time)
            .map(time_result));
    }
    if let Err(error) = access(db, run, condition) {
        return Ok(Some(
            json!({ "state": "unavailable", "message": error.message }),
        ));
    }
    if condition["type"] == "run_finished" {
        match run_result(db, condition) {
            Ok(Some(result)) => return Ok(Some(result)),
            Ok(None) => {}
            Err(error) => {
                return Ok(Some(
                    json!({ "state": "unavailable", "message": error.message }),
                ));
            }
        }
    } else if wakeup["observation"].is_object() {
        return Ok(Some(wakeup["observation"].clone()));
    }
    Ok(wakeup["wakeAt"]
        .as_i64()
        .is_some_and(|deadline| deadline <= time)
        .then(|| json!({ "state": "timed_out", "condition": condition })))
}
