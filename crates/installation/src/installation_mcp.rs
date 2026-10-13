//! Opt-in installation tools for an agent, using its revocable execution grant.
//! The owner delegates mission management, not account or resource administration.
use crate::{
    error::{Error, Result},
    mcp_server::{ToolResult, empty_listing},
    mcps::{grant_key, record::RunGrant},
    run_status::RunStatus,
    service::{Service, allowed, covers, policy, run_projects},
    store::Db,
    validation::{parse, text},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;

const TOOLS: &[&str] = &[
    "list_agents",
    "list_projects",
    "list_tasks",
    "create_task",
    "update_task",
    "run_task",
    "cancel_run",
    "list_runs",
    "get_run",
    "read_run_content",
];

pub(crate) async fn authorize(s: &Service, bearer: &str) -> Result<Value> {
    let bearer = bearer.to_owned();
    s.store.read(move |db| authorize_in(db, &bearer)).await
}

fn authorize_in(db: &Db<'_>, bearer: &str) -> Result<Value> {
    let denied = || Error::unauthorized("Installation access expired or was revoked.");
    let grant = db.kv(&grant_key(bearer))?.ok_or_else(denied)?;
    let grant = RunGrant::deserialize(&grant)?;
    let run = db.run(&grant.run_id)?.ok_or_else(denied)?;
    if !grant.installation
        || grant.message_id != run["chatExecution"]["messageId"]
        || run["status"] != RunStatus::Running
        || !run["cancelRequestedAt"].is_null()
    {
        return Err(denied());
    }
    let agent = &run["snapshot"]["agent"];
    let current = db.get("agents", text(agent, "id"))?.ok_or_else(denied)?;
    if policy(agent)["installation"] != true || !agent_allowed(&current, agent) {
        return Err(Error::forbidden("Agent permissions were reduced."));
    }
    Ok(run)
}

/// A mission cannot delegate broader resource, tool, execution or node access.
fn agent_allowed(caller: &Value, target: &Value) -> bool {
    // GitHub credentials and 1Password grants belong to an agent identity, not
    // its access policy. Matching policies alone cannot authorize another agent.
    if caller["id"] != target["id"] {
        return false;
    }
    let current = policy(caller);
    let target_access = policy(target);
    let nodes = current["nodes"].is_null()
        || target_access["nodes"].as_array().is_some_and(|nodes| {
            nodes
                .iter()
                .all(|node| allowed(&current["nodes"], node.as_str().unwrap_or("")))
        });
    covers(caller, target) && nodes
}

fn task_allowed(db: &Db<'_>, caller: &Value, task: &Value) -> Result<bool> {
    let Some(agent) = db.get("agents", text(task, "agentId"))? else {
        return Ok(false);
    };
    Ok(agent_allowed(caller, &agent)
        && (task["projectId"].is_null()
            || allowed(&policy(caller)["projects"], text(task, "projectId"))))
}

pub(crate) fn run_allowed(caller: &Value, run: &Value) -> bool {
    agent_allowed(caller, &run["snapshot"]["agent"])
        && run_projects(run)
            .iter()
            .all(|project| allowed(&policy(caller)["projects"], text(project, "id")))
}

fn require_access(permitted: bool) -> Result<()> {
    if permitted {
        Ok(())
    } else {
        Err(Error::forbidden(
            "This resource is outside the agent's permissions.",
        ))
    }
}

/// Called inside the mutation transaction, after asynchronous author/skill work.
pub(crate) fn authorize_task_in(
    db: &Db<'_>,
    bearer: &str,
    task: &Value,
    existing: Option<&str>,
) -> Result<()> {
    let run = authorize_in(db, bearer)?;
    let caller = &run["snapshot"]["agent"];
    if let Some(id) = existing {
        let previous = db
            .get("tasks", id)?
            .ok_or_else(|| Error::not_found("Task not found"))?;
        require_access(task_allowed(db, caller, &previous)?)?;
    }
    require_access(task_allowed(db, caller, task)?)
}

pub(crate) fn authorize_target_run_in(db: &Db<'_>, bearer: &str, target: &Value) -> Result<()> {
    let run = authorize_in(db, bearer)?;
    require_access(run_allowed(&run["snapshot"]["agent"], target))
}

/// Filter before applying the public pagination offset, so hidden runs neither
/// consume pages nor expose their metadata. Read only one bounded batch at a time.
fn list_runs(db: &Db<'_>, caller: &Value, args: &Value) -> Result<Value> {
    let mut scan = 0;
    let mut skip = args["offset"].as_i64().unwrap();
    let limit = args["limit"].as_i64().unwrap() as usize;
    let mut visible = Vec::new();
    loop {
        let batch = db.runs(args["status"].as_str(), None, 100, scan, false)?;
        let exhausted = batch.len() < 100;
        scan += batch.len() as i64;
        for summary in batch {
            let Some(run) = db.run(text(&summary, "id"))? else {
                continue;
            };
            if !run_allowed(caller, &run) {
                continue;
            }
            if skip > 0 {
                skip -= 1;
            } else {
                visible.push(summary);
                if visible.len() == limit {
                    return Ok(visible.into());
                }
            }
        }
        if exhausted {
            return Ok(visible.into());
        }
    }
}

pub(crate) async fn rpc(
    s: &Arc<Service>,
    bearer: &str,
    method: &str,
    params: &Value,
) -> Result<Value> {
    let run = authorize(s, bearer).await?;
    let agent_id = &run["snapshot"]["agent"]["id"];
    if let Some(listing) = empty_listing(method) {
        return Ok(listing);
    }
    match method {
        "tools/list" => {
            let tools: Vec<_> = crate::mcp_server::catalog()
                .into_iter()
                .filter_map(|mut tool| {
                    if !TOOLS.contains(&text(&tool, "name")) {
                        return None;
                    }
                    // This endpoint uses the run grant, never external OAuth consent.
                    tool.as_object_mut().unwrap().remove("_meta");
                    tool["description"] = format!(
                        "{} Results and mutations are limited to the calling agent and its permitted projects.",
                        text(&tool, "description"),
                    )
                    .into();
                    let properties = match text(&tool, "name") {
                        "create_task" => Some(&mut tool["inputSchema"]["properties"]),
                        "update_task" => Some(&mut tool["inputSchema"]["properties"]["task"]["properties"]),
                        _ => None,
                    };
                    if let Some(properties) = properties {
                        properties["agentId"]["default"] = agent_id.clone();
                        properties["agentId"]["const"] = agent_id.clone();
                    }
                    Some(tool)
                })
                .collect();
            Ok(json!({ "tools": tools }))
        }
        "tools/call" => {
            let name = text(params, "name");
            let operation = async {
                if !TOOLS.contains(&name) {
                    return Err(Error::not_found("Unknown tool"));
                }
                let mut args = params
                    .get("arguments")
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                // The shared public contract defaults to Main. This endpoint
                // schedules the calling agent when no agent was selected.
                let task = match name {
                    "create_task" => Some(&mut args),
                    "update_task" => args.get_mut("task"),
                    _ => None,
                };
                if let Some(task) = task.and_then(Value::as_object_mut) {
                    task.entry("agentId").or_insert_with(|| agent_id.clone());
                }
                let args = parse(&format!("mcp:{name}"), args)?;
                invoke(s, bearer, name, args).await
            }
            .await;
            let result = match operation {
                Ok(value) => ToolResult::success(value.to_string(), json!({ "result": value })),
                Err(error) => ToolResult::failure(&error.message),
            };
            Ok(serde_json::to_value(result)?)
        }
        _ => Err(Error::not_found("Method not found")),
    }
}

async fn invoke(s: &Arc<Service>, bearer: &str, name: &str, args: Value) -> Result<Value> {
    let token = bearer.to_owned();
    let operation = name.to_owned();
    let input = args.clone();
    let listing = s
        .store
        .read(move |db| {
            // Store readers use separate connections from the writer. Keep the
            // authorization and returned data in one consistent SQLite snapshot.
            let transaction = rusqlite::Transaction::new_unchecked(
                db.0,
                rusqlite::TransactionBehavior::Deferred,
            )?;
            let db = &Db(&transaction);
            let run = authorize_in(db, &token)?;
            let caller = &run["snapshot"]["agent"];
            let result = match operation.as_str() {
                "list_agents" => Ok(Some(
                    db.list("agents")?
                        .into_iter()
                        .filter(|agent| agent_allowed(caller, agent))
                        .collect::<Vec<_>>()
                        .into(),
                )),
                "list_projects" => Ok(Some(
                    db.list("projects")?
                        .into_iter()
                        .filter(|project| allowed(&policy(caller)["projects"], text(project, "id")))
                        .collect::<Vec<_>>()
                        .into(),
                )),
                "list_tasks" => {
                    let mut tasks = Vec::new();
                    for task in db.list("tasks")? {
                        if task_allowed(db, caller, &task)? {
                            tasks.push(task);
                        }
                    }
                    Ok(Some(tasks.into()))
                }
                "list_runs" => Ok(Some(list_runs(db, caller, &input)?)),
                "create_task" => {
                    require_access(task_allowed(db, caller, &input)?)?;
                    Ok(None)
                }
                "update_task" | "run_task" => {
                    let id = if operation == "update_task" {
                        "id"
                    } else {
                        "taskId"
                    };
                    let task = db
                        .get("tasks", text(&input, id))?
                        .ok_or_else(|| Error::not_found("Task not found"))?;
                    require_access(task_allowed(db, caller, &task)?)?;
                    if operation == "update_task" {
                        require_access(task_allowed(db, caller, &input["task"])?)?;
                    }
                    Ok(None)
                }
                "get_run" | "read_run_content" | "cancel_run" => {
                    let target = db
                        .run(text(&input, "runId"))?
                        .ok_or_else(|| Error::not_found("Run not found"))?;
                    require_access(run_allowed(caller, &target))?;
                    match operation.as_str() {
                        "get_run" => Ok(Some(crate::mcp_server::run_history(
                            db,
                            text(&input, "runId"),
                            input["after"].as_i64().unwrap(),
                        )?)),
                        "read_run_content" => {
                            Ok(Some(crate::mcp_server::read_run_content(db, &input)?))
                        }
                        _ => Ok(None),
                    }
                }
                _ => Err(Error::not_found("Unknown tool")),
            }?;
            transaction.commit()?;
            Ok(result)
        })
        .await?;
    if let Some(listing) = listing {
        return Ok(listing);
    }
    match name {
        "create_task" => s.task_from_run(args, None, bearer).await,
        "update_task" => {
            s.task_from_run(args["task"].clone(), Some(text(&args, "id")), bearer)
                .await
        }
        "run_task" => s.enqueue_from_run(text(&args, "taskId"), bearer).await,
        "cancel_run" => {
            s.worker
                .cancel_from_run(s, text(&args, "runId"), bearer)
                .await?;
            Ok(json!({ "cancelled": true }))
        }
        _ => Err(Error::not_found("Unknown tool")),
    }
}
