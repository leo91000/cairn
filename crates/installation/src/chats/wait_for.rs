//! Named, durable conditions that resume an existing conversation without keeping its VM active.
pub mod github;

use super::{chat_for_run, sleep};
use crate::{
    config::{id, now},
    conversation_lifecycle::require_active,
    error::{Error, Result},
    run_status::RunStatus,
    service::{Service, covers, policy},
    store::Db,
    validation::{text, uuid},
};
use serde_json::{Value, json};
use std::collections::HashSet;

pub fn tool() -> Value {
    json!({
        "name": "wait_for",
        "description": "Wait durably for an authorized Cairn run to finish or a specific GitHub Actions \
            workflow run or the required checks of a pull request to complete. If already complete, returns the observed result: continue this turn. \
            Otherwise saves a wakeup: end this turn so execution resources can be released. Cairn checks \
            the condition without waking the LLM and resumes this conversation when a result is available \
            or the timeout expires, subject to capacity. Completion includes failure and cancellation; \
            inspect the result before taking action. Pin GitHub waits to the project, workflow run ID \
            and full commit SHA; PR checks require the project, PR number and full head SHA. A changed PR head \
            returns superseded; a closed or merged PR returns closed. No configured required checks returns \
            no_required_checks, not a CI success or permission to merge. Required check failures and cancellations \
            wake immediately; success waits for all required checks. Never use this for local background processes that need this VM. \
            Run dependencies require the existing installation grant; GitHub uses the agent's existing connection. \
            One wait is stored per conversation. Identical repeated requests preserve the deadline; \
            a different request replaces the previous wait, including sleep. A new user message, stop, deletion, \
            terminal turn failure or manual run resume cancels the wait; queue pause holds it.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "condition": {
                    "oneOf": [
                        {
                            "type": "object",
                            "properties": {
                                "type": { "const": "run_finished" },
                                "runId": { "type": "string", "format": "uuid" },
                            },
                            "required": ["type", "runId"],
                            "additionalProperties": false,
                        },
                        {
                            "type": "object",
                            "properties": {
                                "type": { "const": "github_pr_checks" },
                                "projectId": { "type": "string", "format": "uuid" },
                                "prNumber": { "type": "integer", "minimum": 1, "maximum": 2147483647 },
                                "headSha": { "type": "string", "pattern": "^[a-fA-F0-9]{40}$" },
                            },
                            "required": ["type", "projectId", "prNumber", "headSha"],
                            "additionalProperties": false,
                        },
                        {
                            "type": "object",
                            "properties": {
                                "type": { "const": "github_workflow" },
                                "projectId": { "type": "string", "format": "uuid" },
                                "runId": { "type": "integer", "minimum": 1, "maximum": 9007199254740991u64 },
                                "headSha": { "type": "string", "pattern": "^[a-fA-F0-9]{40}$" },
                            },
                            "required": ["type", "projectId", "runId", "headSha"],
                            "additionalProperties": false,
                        },
                    ],
                },
                "timeoutSeconds": { "type": "integer", "minimum": 1, "maximum": 86400 },
                "reason": { "type": "string", "minLength": 1, "maxLength": 1000 },
            },
            "required": ["condition", "timeoutSeconds", "reason"],
            "additionalProperties": false,
        },
    })
}

fn input(input: &Value) -> Result<Value> {
    if !jsonschema::is_valid(&tool()["inputSchema"], input)
        || text(input, "reason").trim().is_empty()
    {
        return Err(Error::bad(
            "Provide a supported condition, a concrete reason and a timeout of 1 to 86400 seconds.",
        ));
    }
    let mut input = crate::run_output::payload(input, &[]);
    // JSON Schema accepts integral floats; canonicalize them for idempotency.
    input["timeoutSeconds"] = (input["timeoutSeconds"].as_f64().unwrap() as i64).into();
    let condition = &mut input["condition"];
    if condition["type"] == "run_finished" {
        uuid(text(condition, "runId"))?;
    } else {
        uuid(text(condition, "projectId"))?;
        let id_key = if condition["type"] == "github_pr_checks" {
            "prNumber"
        } else {
            "runId"
        };
        condition[id_key] = (condition[id_key].as_f64().unwrap() as u64).into();
        condition["headSha"] = text(condition, "headSha").to_ascii_lowercase().into();
    }
    Ok(input)
}

fn same_request(previous: &Value, run: &Value, input: &Value) -> bool {
    previous.is_object()
        && previous["messageId"] == run["chatExecution"]["messageId"]
        && previous["condition"] == input["condition"]
        && previous["seconds"] == input["timeoutSeconds"]
        && previous["reason"] == input["reason"]
}

fn input_revision(db: &Db<'_>, run: &Value) -> Result<i64> {
    Ok(db.0.query_row(
        "SELECT COALESCE(MAX(id),0) FROM events WHERE run_id=? AND type='chat.user'",
        [text(run, "id")],
        |row| row.get(0),
    )?)
}

fn registering(db: &Db<'_>, bearer: &str, input: &Value) -> Result<(Value, Value)> {
    let run = crate::project_workspaces::authorize_in(db, bearer)?;
    let chat = chat_for_run(db, text(&run, "id"))?
        .ok_or_else(|| Error::conflict("Waiting requires an interactive conversation."))?;
    require_active(&chat)?;
    if chat["paused"] == true || sleep::has_pending_input(db, &chat)? {
        return Err(Error::conflict(
            "Handle pending user input before registering a wait.",
        ));
    }
    access(db, &run, &input["condition"])?;
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
pub(super) fn access(db: &Db<'_>, run: &Value, condition: &Value) -> Result<()> {
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
        "github_workflow" | "github_pr_checks" => {
            github::project(db, run, condition)?;
        }
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
        && (chat["wakeup"].is_object() || sleep::has_pending_input(db, &chat)?)
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
            "finishedAt": target["finishedAt"],
        })
    }))
}

pub async fn schedule(s: &Service, bearer: &str, raw: &Value) -> Result<Value> {
    let input = input(raw)?;
    let token = bearer.to_owned();
    let request = input.clone();
    // Keep the input checks and revision atomic: an acknowledged steering
    // message must not become the baseline of an already-started registration.
    let (run, chat, revision) = s
        .store
        .transaction(move |db| {
            let (run, chat) = registering(db, &token, &request)?;
            let revision = input_revision(db, &run)?;
            Ok((run, chat, revision))
        })
        .await?;
    if same_request(&chat["wakeup"], &run, &input) {
        return Ok(json!({ "deferred": true, "wakeup": chat["wakeup"] }));
    }
    let observed = if github::is_condition(&input["condition"]) {
        github::observe(s, &run, &input["condition"]).await?
    } else {
        None
    };
    let token = bearer.to_owned();
    let result = s
        .store
        .transaction(move |db| {
            let (run, mut chat) = registering(db, &token, &input)?;
            if input_revision(db, &run)? != revision {
                return Err(Error::conflict("User input changed while the condition was checked. Handle it before registering a wait."));
            }
            if same_request(&chat["wakeup"], &run, &input) {
                return Ok(json!({ "deferred": true, "wakeup": chat["wakeup"] }));
            }
            let observed = if input["condition"]["type"] == "run_finished" {
                run_result(db, &input["condition"])?
            } else {
                observed
            };
            if let Some(observed) = observed {
                sleep::cancel(db, text(&chat, "id"))?;
                // Web and Android match this text to show the result on its own card, which
                // names the agent's reason: no wakeup records it.
                let mut announced = observed.clone();
                announced["reason"] = input["reason"].clone();
                db.event(
                    text(&run, "id"),
                    "chat.wait_ready",
                    "Wait condition already completed",
                    Some(&announced),
                )?;
                return Ok(json!({ "deferred": false, "observation": observed }));
            }
            let created = now();
            let seconds = input["timeoutSeconds"].as_i64().unwrap();
            let mut wakeup = json!({
                "id": id(),
                "messageId": run["chatExecution"]["messageId"],
                "seconds": seconds,
                "reason": input["reason"],
                "createdAt": created,
                "wakeAt": created + seconds * 1000,
                "condition": input["condition"],
            });
            if github::is_condition(&input["condition"]) {
                wakeup["nextCheckAt"] = created.into();
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
        return Ok(None);
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
