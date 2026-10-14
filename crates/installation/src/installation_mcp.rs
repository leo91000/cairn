//! Opt-in installation tools for an agent, using its revocable execution grant.
//! The owner delegates the complete public MCP management catalogue.
use crate::{
    error::{Error, Result, required},
    mcp_server::{ToolResult, empty_listing},
    mcps::{grant_key, record::RunGrant},
    run_status::RunStatus,
    service::{Service, allowed, covers, policy},
    store::Db,
    validation::{parse, text},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;

pub(crate) async fn authorize(s: &Service, bearer: &str) -> Result<Value> {
    let bearer = bearer.to_owned();
    s.store.read(move |db| authorize_in(db, &bearer)).await
}

pub(crate) fn authorize_in(db: &Db<'_>, bearer: &str) -> Result<Value> {
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

/// Reducing the calling agent's permissions invalidates its active grant.
fn agent_allowed(caller: &Value, target: &Value) -> bool {
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

/// Authorization for management effects after asynchronous preparation.
pub(crate) fn management_authorization(
    bearer: Option<&str>,
) -> impl Fn(&Db<'_>) -> Result<()> + Clone + Send + Sync + 'static {
    let bearer = bearer.map(str::to_owned);
    move |db| {
        if let Some(bearer) = &bearer {
            authorize_in(db, bearer)?;
        }
        Ok(())
    }
}

/// Rechecks the grant and referenced records in the task mutation transaction.
pub(crate) fn authorize_task_in(
    db: &Db<'_>,
    bearer: &str,
    task: &Value,
    existing: Option<&str>,
) -> Result<()> {
    authorize_in(db, bearer)?;

    if let Some(id) = existing {
        required(db.get("tasks", id)?, "Task not found")?;
    }

    required(db.get("agents", text(task, "agentId"))?, "Agent not found")?;

    Ok(())
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
                .map(|mut tool| {
                    // This endpoint uses the run grant, never external OAuth consent.
                    tool.as_object_mut().unwrap().remove("_meta");
                    let properties = match text(&tool, "name") {
                        "create_task" => Some(&mut tool["inputSchema"]["properties"]),
                        "update_task" => {
                            Some(&mut tool["inputSchema"]["properties"]["task"]["properties"])
                        }
                        _ => None,
                    };
                    if let Some(properties) = properties {
                        properties["agentId"]["default"] = agent_id.clone();
                    }
                    tool
                })
                .collect();
            Ok(json!({ "tools": tools }))
        }
        "tools/call" => {
            let name = text(params, "name");
            let operation = async {
                let known = crate::mcp_server::catalog()
                    .iter()
                    .any(|tool| text(tool, "name") == name);
                if !known {
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
            // Authorize database reads in the same snapshot as the returned data.
            let transaction = rusqlite::Transaction::new_unchecked(
                db.0,
                rusqlite::TransactionBehavior::Deferred,
            )?;
            let db = &Db(&transaction);
            authorize_in(db, &token)?;

            let result = match operation.as_str() {
                "list_agents" => Some(db.list("agents")?.into()),
                "list_projects" => Some(db.list("projects")?.into()),
                "list_tasks" => Some(db.list("tasks")?.into()),
                "list_runs" => Some(
                    db.runs(
                        input["status"].as_str(),
                        None,
                        input["limit"].as_i64().unwrap(),
                        input["offset"].as_i64().unwrap(),
                        false,
                    )?
                    .into(),
                ),
                "get_run" => Some(crate::mcp_server::run_history(
                    db,
                    text(&input, "runId"),
                    input["after"].as_i64().unwrap(),
                )?),
                "read_run_content" => Some(crate::mcp_server::read_run_content(db, &input)?),
                _ => None,
            };

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
        _ => crate::mcp_server::invoke(s, name, args, Some(bearer)).await,
    }
}
