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
    authorize_with_authority(s, bearer)
        .await
        .map(|(run, _)| run)
}

/// Retain both the execution token and semantic delegation proof across effects.
pub(crate) async fn authorize_with_authority(
    s: &Service,
    bearer: &str,
) -> Result<(Value, crate::capabilities::plugin_packages::Authority)> {
    let caller = bearer.to_owned();
    let token = caller.clone();
    let read_token = caller.clone();
    let run = s
        .store
        .read(move |db| authorize_in(db, &read_token))
        .await?;
    let authority = crate::capabilities::plugin_packages::Authority::for_execution(&run)
        .with_guard(move |db| authorize_in(db, &token).map(|_| ()));
    let mut authority = crate::delegation::authorize_async_with(s, &run, authority).await?;
    authority.bearer = Some(caller);
    Ok((run, authority))
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
    crate::delegation::authorize_run(db, &run)?;
    Ok(run)
}

/// Operations that return no content of another conversation, run or task. Any other
/// operation, including one added later, reaches the caller's conversation unless it
/// only targets that conversation or its run.
const OUTSIDE_CONVERSATIONS: &[&str] = &[
    // Hook rules
    "list_hook_rules",
    "get_hook_rule",
    "list_hook_rule_versions",
    "get_hook_rule_catalog",
    "list_hook_rule_templates",
    "validate_hook_rule",
    "create_hook_rule",
    "install_hook_rule_template",
    "replace_hook_rule",
    "set_hook_rule_enabled",
    "set_hook_rule_emergency_pause",
    "lift_hook_rule_budget",
    "duplicate_hook_rule",
    "delete_hook_rule",
    // Agents
    "list_agents",
    "get_agent",
    "get_agent_plugin_grants",
    "replace_agent_plugin_grants",
    "save_agent",
    "replace_agent",
    "preview_agent",
    "merge_agents",
    "delete_agent",
    "set_agent_avatar",
    "generate_agent_avatar",
    "get_agent_avatar_configuration",
    "list_agent_accounts",
    "create_chat",
    // Projects and skills
    "list_projects",
    "get_project",
    "save_project",
    "replace_project",
    "delete_project",
    "import_project",
    "list_source_projects",
    "list_skills",
    "read_skill",
    "save_skill",
    "edit_skill_files",
    "stage_skill_upload",
    "import_skill",
    "delete_skill",
    "export_skill_archive",
    "get_skill_source_status",
    "update_skill_source",
    "forget_skill_source",
    // Models, accounts and settings
    "list_models",
    "list_coding_agent_accounts",
    "get_coding_agent_account_sign_in",
    "list_coding_agent_account_providers",
    "replace_coding_agent_account",
    "order_coding_agent_accounts",
    "refresh_coding_agent_accounts",
    "connect_coding_agent_account",
    "reconnect_coding_agent_account",
    "replace_coding_agent_account_key",
    "submit_coding_agent_account_code",
    "cancel_coding_agent_account_sign_in",
    "delete_coding_agent_account",
    "get_installation_settings",
    "get_coding_agent_configuration",
    "preview_coding_agent_configuration",
    "save_coding_agent_configuration",
    "get_storage_settings",
    "save_storage_settings",
    "check_storage",
    "get_task_storage_limits",
    "save_task_storage_limits",
    "get_chat_plan_settings",
    "save_chat_plan_settings",
    "get_conversation_retention",
    "set_conversation_retention",
    // MCP connections and plugins
    "list_mcps",
    "create_mcp",
    "update_mcp",
    "test_mcp",
    "disconnect_mcp",
    "delete_mcp",
    "begin_mcp_connection",
    "mcp_connection_status",
    "cancel_mcp_connection",
    "manage_plugin",
    "list_plugins",
    "list_plugin_resources",
    "resolve_plugin_resource",
    "install_plugin_package",
    "approve_plugin_package",
    "activate_plugin_package",
    "delete_plugin_package",
    "list_plugin_programs",
    "prepare_plugin_program",
    "get_plugin_operation",
    "get_plugin_resource_operation",
    "cancel_plugin_operation",
    // Nodes
    "list_nodes",
    "list_native_callers",
    "create_node_enrollment",
    "update_node",
    "change_native_node_mode",
    "update_node_storage",
    "revoke_node",
];

/// Reading another conversation, run or task through these tools (a delegated task's
/// report included) is an agent entry in the caller's own conversation (ADR-0075): that
/// content now reaches it.
async fn record_reading(s: &Service, run: &Value, name: &str, args: &Value) -> Result<()> {
    if OUTSIDE_CONVERSATIONS.contains(&name) {
        return Ok(());
    }
    let run_id = text(run, "id").to_owned();
    let (chat, target_run) = (
        args["chatId"].as_str().map(str::to_owned),
        args["runId"].as_str().map(str::to_owned),
    );
    s.store
        .transaction(move |db| {
            let Some(own) = db.conversation_for_run(&run_id)? else {
                return Ok(());
            };
            let only_own = (chat.is_some() || target_run.is_some())
                && chat.as_ref().is_none_or(|chat| *chat == own.id)
                && target_run.as_ref().is_none_or(|run| *run == run_id);
            if !only_own {
                crate::conversation_writers::record_entry(
                    db,
                    &own.id,
                    &crate::conversation_writers::EntryWriter::Agent,
                )?;
            }
            Ok(())
        })
        .await
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
pub(crate) async fn management_authorization(
    s: &Service,
    bearer: Option<&str>,
) -> Result<crate::authorization::Authorization> {
    match bearer {
        Some(bearer) => {
            let (_, authority) = authorize_with_authority(s, bearer).await?;
            Ok(crate::authorization::Authorization::installation_management(s, authority, bearer))
        }
        None => Ok(crate::authorization::Authorization::default()),
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

/// A plan turn exposes only operations whose delegated MCP scope is `read`.
fn has_read_scope(operation: &Value) -> bool {
    operation["_meta"]["securitySchemes"]
        .as_array()
        .is_some_and(|schemes| {
            schemes.iter().any(|scheme| {
                let scopes = scheme["scopes"].as_array();
                scheme["type"] == "oauth2"
                    && scopes.is_some_and(|scopes| {
                        scopes.len() == 1 && scopes[0].as_str() == Some("read")
                    })
            })
        })
}

fn allowed_in_turn(run: &Value, operation: &Value) -> bool {
    !crate::chats::planning(run) || has_read_scope(operation)
}

pub(crate) async fn operations(s: &Service, bearer: &str) -> Result<Vec<Value>> {
    let run = authorize(s, bearer).await?;
    let agent_id = &run["snapshot"]["agent"]["id"];
    let tools = crate::mcp_server::operations()
        .into_iter()
        .filter(|tool| allowed_in_turn(&run, tool))
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
    let operations = crate::mcp_server::operations();
    let Some(operation) = operations.iter().find(|tool| text(tool, "name") == name) else {
        return Err(Error::not_found("Unknown operation"));
    };
    if !allowed_in_turn(&run, operation) {
        return Err(Error::forbidden(
            "Only read-scoped installation operations are available during a plan turn.",
        ));
    }
    record_reading(s, &run, name, &args).await?;
    if let Some(op) = crate::capabilities::artifact_operation(name) {
        return op
            .invoke(
                s,
                args,
                crate::capabilities::ResumeContext::Installation(bearer.to_owned()),
            )
            .await;
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
    if let Some(op) = crate::rules::rule_operation(name) {
        return op
            .invoke(
                s,
                args,
                crate::capabilities::ResumeContext::Installation(bearer.to_owned()),
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
                None,
            )
            .await;
    }
    if let Some(op) = crate::capabilities::node_operation(name) {
        // Bound the shared RPC frame while placement can await nested node work.
        return crate::native::policy::PARENT
            .scope(
                crate::validation::text(&run, "id").to_owned(),
                Box::pin(op.invoke(
                    s,
                    args,
                    crate::capabilities::ResumeContext::Installation(bearer.to_owned()),
                )),
            )
            .await;
    }
    if let Some(op) = crate::capabilities::resource_operation(name) {
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
                    None,
                ),
            )
            .await;
    }
    if name == cairn_contracts::RESUME_RUN_ID {
        let input = crate::capabilities::RESUME_RUN.decode(args)?;
        let context = crate::capabilities::ResumeContext::Installation(bearer.to_owned());
        let output = crate::capabilities::RESUME_RUN
            .invoke(s, input, context, None)
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
