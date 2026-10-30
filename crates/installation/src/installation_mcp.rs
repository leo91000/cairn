//! Opt-in installation tools for an agent, using its revocable execution grant.
//! The owner delegates the complete public MCP management catalogue.
use crate::{
    error::{Error, Result, required},
    mcp_server::empty_listing,
    mcps::grant_key,
    run_status::RunStatus,
    service::{Service, allowed, covers, policy},
    store::Db,
    validation::{parse, text},
};
use serde_json::{Value, json};
use std::sync::Arc;

pub(crate) async fn authorize(s: &Service, bearer: &str) -> Result<Value> {
    let bearer = bearer.to_owned();
    s.store.read(move |db| authorize_in(db, &bearer)).await
}

pub(crate) fn authorize_in(db: &Db<'_>, bearer: &str) -> Result<Value> {
    let denied = || Error::unauthorized("Installation access expired or was revoked.");
    let grant = db.mcp_run_grant(&grant_key(bearer))?.ok_or_else(denied)?;
    let run = db.run(&grant.run_id)?.ok_or_else(denied)?;
    if !grant.installation
        || grant.message_id.as_deref() != crate::waits::execution_id(&run).as_str()
        || run["status"] != RunStatus::Running
        || !run["cancelRequestedAt"].is_null()
    {
        return Err(denied());
    }
    let agent = &run["snapshot"]["agent"];
    let current = db.agent_snapshot(text(agent, "id"))?.ok_or_else(denied)?;
    if policy(agent)["installation"] != true || !agent_allowed(&current, agent) {
        return Err(Error::forbidden("Agent permissions were reduced."));
    }
    Ok(run)
}

/// Reducing the calling agent's permissions invalidates its active grant.
pub(crate) fn agent_allowed(caller: &Value, target: &Value) -> bool {
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

/// Rechecks the grant and referenced records in the task mutation transaction, and returns the
/// calling run.
pub(crate) fn authorize_task_in(
    db: &Db<'_>,
    bearer: &str,
    task: &Value,
    existing: Option<&str>,
) -> Result<Value> {
    let caller = authorize_in(db, bearer)?;

    if let Some(id) = existing {
        required(db.mission(id)?, "Task not found")?;
    }

    required(db.agent_snapshot(text(task, "agentId"))?, "Agent not found")?;

    Ok(caller)
}

pub(crate) async fn rpc(
    s: &Arc<Service>,
    bearer: &str,
    method: &str,
    params: &Value,
) -> Result<Value> {
    authorize(s, bearer).await?;
    if let Some(listing) = empty_listing(method) {
        return Ok(listing);
    }
    match method {
        "tools/list" => {
            let tools: Vec<_> = crate::mcp_server::catalog()
                .into_iter()
                .map(|mut tool| {
                    tool.as_object_mut().unwrap().remove("_meta");
                    tool
                })
                .collect();
            Ok(json!({ "tools": tools }))
        }
        "tools/call" => {
            crate::mcp_server::code_mode::call(
                s,
                crate::mcp_server::code_mode::Authorization::Installation(bearer.to_owned()),
                text(params, "name"),
                params
                    .get("arguments")
                    .cloned()
                    .unwrap_or_else(|| json!({})),
            )
            .await
        }
        _ => Err(Error::not_found("Method not found")),
    }
}

pub(crate) async fn operations(s: &Service, bearer: &str) -> Result<Vec<Value>> {
    let run = authorize(s, bearer).await?;
    let agent_id = &run["snapshot"]["agent"]["id"];
    let tools = crate::mcp_server::operations()
        .into_iter()
        .map(|mut tool| {
            tool.as_object_mut().unwrap().remove("_meta");
            if tool["name"] == "create_task" {
                let settings = &mut tool["inputSchema"]["definitions"]["MissionSettings"];
                settings["properties"]["agentId"]["default"] = agent_id.clone();
                if let Some(required) = settings["required"].as_array_mut() {
                    required.retain(|field| field != "agentId");
                }
            }
            let properties = match text(&tool, "name") {
                "create_chat" => Some(&mut tool["inputSchema"]["properties"]),
                _ => None,
            };
            if let Some(properties) = properties {
                properties["agentId"]["default"] = agent_id.clone();
            }
            tool
        })
        .collect();
    Ok(tools)
}

pub(crate) async fn operation(
    s: &Arc<Service>,
    bearer: &str,
    name: &str,
    mut args: Value,
) -> Result<Value> {
    let run = authorize(s, bearer).await?;
    let known = crate::mcp_server::operations()
        .iter()
        .any(|tool| text(tool, "name") == name);
    if !known {
        return Err(Error::not_found("Unknown operation"));
    }
    if let Some(operation) = crate::capabilities::plugin_package_operation(name) {
        return operation
            .invoke(
                s,
                args,
                crate::capabilities::ResumeContext::Installation(bearer.to_owned()),
            )
            .await;
    }
    if name == "manage_plugin" {
        return crate::capabilities::MANAGE_PLUGIN
            .invoke(
                s,
                args,
                crate::capabilities::ResumeContext::Installation(bearer.to_owned()),
            )
            .await;
    }
    if let Some(op) = crate::capabilities::mission_operation(name) {
        if name == "create_task"
            && let Some(settings) = args.get_mut("settings").and_then(Value::as_object_mut)
        {
            settings
                .entry("agentId")
                .or_insert_with(|| run["snapshot"]["agent"]["id"].clone());
        }
        let origin = crate::validation::text(&run, "id").to_owned();
        return crate::native::policy::PARENT
            .scope(
                origin,
                op.invoke(
                    s,
                    args,
                    crate::capabilities::ResumeContext::Installation(bearer.to_owned()),
                ),
            )
            .await;
    }
    if let Some(op) = crate::capabilities::mcp_operation(name) {
        return op
            .invoke(
                s,
                args,
                crate::capabilities::ResumeContext::Installation(bearer.to_owned()),
                None,
            )
            .await;
    }
    if let Some(op) = crate::capabilities::agent_operation(name) {
        return op
            .invoke(
                s,
                args,
                crate::capabilities::ResumeContext::Installation(bearer.to_owned()),
            )
            .await;
    }
    if let Some(op) = crate::capabilities::account_operation(name) {
        return op
            .invoke(
                s,
                args,
                crate::capabilities::ResumeContext::Installation(bearer.to_owned()),
            )
            .await;
    }
    if let Some(op) = crate::capabilities::personal_operation(name) {
        return op
            .invoke(
                s,
                args,
                crate::capabilities::ResumeContext::Installation(bearer.to_owned()),
            )
            .await;
    }
    if let Some(op) = crate::capabilities::settings_operation(name) {
        return op
            .invoke(
                s,
                args,
                crate::capabilities::ResumeContext::Installation(bearer.to_owned()),
            )
            .await;
    }
    if name == "create_chat"
        && let Some(input) = args.as_object_mut()
    {
        input
            .entry("agentId")
            .or_insert_with(|| run["snapshot"]["agent"]["id"].clone());
    }
    if let Some(op) = crate::capabilities::chat_operation(name) {
        return op
            .invoke(
                s,
                args,
                crate::capabilities::ResumeContext::Installation(bearer.to_owned()),
            )
            .await;
    }
    if let Some(op) = crate::capabilities::node_operation(name) {
        return op
            .invoke(
                s,
                args,
                crate::capabilities::ResumeContext::Installation(bearer.to_owned()),
            )
            .await;
    }
    if let Some(op) = crate::capabilities::project_operation(name) {
        return op
            .invoke(
                s,
                args,
                crate::capabilities::ResumeContext::Installation(bearer.to_owned()),
            )
            .await;
    }
    if let Some(op) = crate::capabilities::run_operation(name) {
        let origin = crate::validation::text(&run, "id").to_owned();
        return crate::native::policy::PARENT
            .scope(
                origin,
                op.invoke(
                    s,
                    args,
                    crate::capabilities::ResumeContext::Installation(bearer.to_owned()),
                ),
            )
            .await;
    }
    if name == cairn_contracts::RESUME_RUN_ID {
        let input = crate::capabilities::RESUME_RUN.decode(args)?;
        let context = crate::capabilities::ResumeContext::Installation(bearer.to_owned());
        let output = crate::capabilities::RESUME_RUN
            .invoke(s, input, context)
            .await?;
        return Ok(serde_json::to_value(output)?);
    }
    let input = match name {
        "create_chat" => Some(&mut args),
        _ => None,
    };
    if let Some(input) = input.and_then(Value::as_object_mut) {
        input
            .entry("agentId")
            .or_insert_with(|| run["snapshot"]["agent"]["id"].clone());
    }
    let args = parse(&format!("mcp:{name}"), args)?;
    invoke(s, bearer, name, args).await
}

async fn invoke(s: &Arc<Service>, bearer: &str, name: &str, args: Value) -> Result<Value> {
    let run = authorize(s, bearer).await?;
    crate::native::policy::PARENT
        .scope(
            text(&run, "id").to_owned(),
            invoke_inner(s, bearer, name, args),
        )
        .await
}

async fn invoke_inner(s: &Arc<Service>, bearer: &str, name: &str, args: Value) -> Result<Value> {
    crate::mcp_server::invoke(s, name, args, Some(bearer)).await
}
