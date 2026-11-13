use crate::{
    error::{Error, Result},
    http::Input,
    service::Service,
};
use serde_json::{Value, json};
use std::sync::Arc;

type Route<'a> = (&'a str, &'a [&'a str]);

fn under(path: &str, prefix: &str) -> bool {
    path == prefix
        || path
            .strip_prefix(prefix)
            .is_some_and(|rest| rest.starts_with('/'))
}

pub async fn dispatch(s: &Arc<Service>, input: &Input) -> Result<Value> {
    crate::agent_access::authorize_request(s, input).await?;
    crate::native::policy::authorize_request(s, input).await?;
    let caller = input
        .identity
        .as_ref()
        .filter(|identity| identity.mcp_scopes.is_none())
        .map(|identity| identity.account_id.clone());
    crate::native::policy::CALLER
        .scope(caller, dispatch_inner(s, input))
        .await
}

async fn dispatch_inner(s: &Arc<Service>, input: &Input) -> Result<Value> {
    if under(&input.path, "/api/nodes") {
        return crate::nodes::admin(s, input).await;
    }
    if under(&input.path, "/api/accounts") {
        return crate::capabilities::accounts::routes(s, input).await;
    }
    if under(&input.path, "/api/plugins") {
        return crate::plugins::routes(s, input).await;
    }
    if under(&input.path, "/api/mcps") {
        return crate::mcp_server::routes(s, input).await;
    }
    if let Some(result) = crate::worker::routes(s, input).await {
        return result;
    }
    let segments = input
        .path
        .trim_start_matches("/api/")
        .split('/')
        .collect::<Vec<_>>();
    let route = (input.method.as_str(), segments.as_slice());
    if let ("GET", ["notification-recipients", chat, question]) = route {
        let recipients =
            crate::notifications::recipients(s, input.identity.as_ref(), chat, question).await?;
        return Ok(serde_json::to_value(recipients)?);
    }
    if let (method @ ("GET" | "PUT" | "DELETE"), ["chats", chat, "share"]) = route {
        let name = match method {
            "GET" => "get_chat_share",
            "PUT" => "set_chat_share",
            _ => "revoke_chat_share",
        };
        let mut args = if method == "PUT" {
            if !input.body.is_object() {
                return Err(Error::bad("Expected sharing options as an object."));
            }
            input.body.clone()
        } else {
            json!({})
        };
        args["chatId"] = (*chat).into();
        return crate::capabilities::chat_operation(name)
            .unwrap()
            .invoke(
                s,
                args,
                crate::capabilities::ResumeContext::http(input)?,
                None,
            )
            .await;
    }
    if let Some(result) = record_routes(s, input, route).await? {
        return Ok(result);
    }
    if let Some(result) = run_routes(s, input, route).await? {
        return Ok(result);
    }
    if let Some(result) = chat_routes(s, input, route).await? {
        return Ok(result);
    }
    if let Some(result) = skill_routes(s, input, route).await? {
        return Ok(result);
    }
    if let Some(result) = settings_routes(s, input, route).await? {
        return Ok(result);
    }
    Err(Error::not_found("Not found"))
}

/// Agents, projects, tasks and their scheduling.
async fn record_routes(s: &Arc<Service>, input: &Input, route: Route<'_>) -> Result<Option<Value>> {
    let agent_command = match route {
        ("GET", ["agents", id, "plugin-grants"]) => {
            Some(("get_agent_plugin_grants", json!({ "agentId": id })))
        }
        ("PUT", ["agents", id, "plugin-grants"]) => {
            if input.body["agentId"] != *id {
                return Err(Error::bad("Agent identity does not match the path."));
            }
            Some(("replace_agent_plugin_grants", input.body.clone()))
        }
        ("GET", ["agents"]) => Some(("list_agents", json!({}))),
        ("GET", ["agents", id]) => Some(("get_agent", json!({ "agentId": id }))),
        ("POST", ["agents"]) => Some(("save_agent", input.body.clone())),
        ("PUT", ["agents", id]) | ("POST", ["agents", id, "preview"]) => {
            if input.body["agentId"] != *id {
                return Err(Error::bad("Agent identity does not match the path."));
            }
            Some((
                if route.0 == "PUT" {
                    "replace_agent"
                } else {
                    "preview_agent"
                },
                input.body.clone(),
            ))
        }
        ("POST", ["agents", id, "merge"]) => {
            if input.body["sourceId"] != *id {
                return Err(Error::bad("Agent identity does not match the path."));
            }
            Some(("merge_agents", input.body.clone()))
        }
        ("DELETE", ["agents", id]) => Some(("delete_agent", json!({ "agentId": id }))),
        ("GET", ["agent-avatars"]) => Some(("get_agent_avatar_configuration", json!({}))),
        ("GET", ["agents", id, "accounts"]) => {
            Some(("list_agent_accounts", json!({ "agentId": id })))
        }
        ("POST", ["agents", id, "avatar", "upload"]) => {
            if input.body["agentId"] != *id {
                return Err(Error::bad("Agent identity does not match the path."));
            }
            Some(("set_agent_avatar", input.body.clone()))
        }
        ("POST", ["agents", id, "avatar", "generate"]) => {
            Some(("generate_agent_avatar", json!({ "agentId": id })))
        }
        _ => None,
    };
    if let Some((name, body)) = agent_command {
        let context = crate::capabilities::ResumeContext::http(input)?;
        return Ok(Some(
            crate::capabilities::agent_operation(name)
                .expect("agent operation")
                .invoke(s, body, context)
                .await?,
        ));
    }
    // Giving and removing access to agents stays in the apps (ADR-0075).
    let account_access = match route {
        ("PUT", ["agents", id, "accounts", account]) => Some((true, *id, *account)),
        ("DELETE", ["agents", id, "accounts", account]) => Some((false, *id, *account)),
        _ => None,
    };
    if let Some((grant, agent, account)) = account_access {
        let agent = cairn_contracts::AgentId(
            uuid::Uuid::parse_str(agent).map_err(|_| Error::bad("Invalid agent identity."))?,
        );
        let identity = input.identity.as_ref();
        let output = if grant {
            s.grant_agent_account(identity, agent, account).await?
        } else {
            s.revoke_agent_account(identity, agent, account).await?
        };
        return Ok(Some(serde_json::to_value(output)?));
    }
    if let ("GET", ["usable-agents"]) = route {
        let identity = input
            .identity
            .clone()
            .ok_or_else(|| Error::unauthorized("A verified identity is required."))?;
        let agent_ids = crate::agent_access::usable_now(s, identity).await?;
        return Ok(Some(serde_json::to_value(
            cairn_contracts::UsableAgentsOutput { agent_ids },
        )?));
    }
    let project_command = match route {
        ("GET", ["projects"]) => Some(("list_projects", json!({}))),
        ("GET", ["projects", id]) => Some(("get_project", json!({ "projectId": id }))),
        ("POST", ["projects"]) => Some(("save_project", input.body.clone())),
        ("PUT", ["projects", id]) => {
            if input.body["projectId"] != *id {
                return Err(Error::bad("Project identity does not match the path."));
            }
            Some(("replace_project", input.body.clone()))
        }
        ("DELETE", ["projects", id]) => Some(("delete_project", json!({ "projectId": id }))),
        _ => None,
    };
    if let Some((name, body)) = project_command {
        let context = crate::capabilities::ResumeContext::http(input)?;
        let output = crate::capabilities::project_operation(name)
            .expect("project operation")
            .invoke(s, body, context)
            .await?;
        return Ok(Some(output));
    }
    let mission_command = match route {
        ("GET", ["tasks"]) => Some(("list_tasks", json!({}))),
        ("GET", ["tasks", "activity"]) => Some(("list_task_activity", json!({}))),
        ("GET", ["tasks", id]) => Some(("get_task", json!({ "taskId": id }))),
        ("POST", ["tasks"]) => Some(("create_task", input.body.clone())),
        ("PUT", ["tasks", id]) | ("POST", ["tasks", id, "state"]) => {
            if input.body["taskId"] != *id {
                return Err(Error::bad("Mission identity does not match the path."));
            }
            Some((
                if route.0 == "PUT" {
                    "replace_task"
                } else {
                    "set_task_state"
                },
                input.body.clone(),
            ))
        }
        ("DELETE", ["tasks", id]) => Some(("delete_task", json!({ "taskId": id }))),
        ("POST", ["schedule", "preview"]) => Some(("preview_schedule", input.body.clone())),
        _ => None,
    };
    if let Some((name, body)) = mission_command {
        let operation = crate::capabilities::mission_operation(name).expect("mission operation");
        let identity = input
            .identity
            .as_ref()
            .ok_or_else(|| Error::unauthorized("A verified identity is required."))?;
        let result = if identity.mcp_scopes.is_some() {
            operation
                .invoke(s, body, crate::capabilities::ResumeContext::http(input)?)
                .await?
        } else {
            operation
                .invoke_as(s, body, crate::missions::Actor::Human(identity.clone()))
                .await?
        };
        return Ok(Some(result));
    }
    if let Some(result) = rule_routes(s, input, route).await? {
        return Ok(Some(result));
    }
    let result = match route {
        ("POST", ["projects", "import"]) => {
            crate::capabilities::resource_operation("import_project")
                .unwrap()
                .invoke(
                    s,
                    input.body.clone(),
                    crate::capabilities::ResumeContext::http(input)?,
                )
                .await?
        }
        ("POST", ["task-authors", "refresh"]) => {
            s.synchronize_task_authors().await?;
            json!({ "updated": true })
        }
        ("POST", ["tasks", id, "run"]) => {
            crate::capabilities::run_operation("run_task")
                .unwrap()
                .invoke(
                    s,
                    json!({ "taskId": id }),
                    crate::capabilities::ResumeContext::http(input)?,
                    crate::capabilities::member_account(input.identity.as_ref()),
                )
                .await?
        }
        _ => return Ok(None),
    };
    Ok(Some(result))
}

async fn run_routes(s: &Arc<Service>, input: &Input, route: Route<'_>) -> Result<Option<Value>> {
    let result = match route {
        ("GET", ["runs"]) => {
            let limit = input.number("limit", 40, 1, 100)?;
            let offset = input.number("offset", 0, 0, i64::MAX)?;
            let status = input.query.get("status").cloned();
            let task = input.query.get("taskId").cloned();
            s.store
                .read(move |db| {
                    let runs = db.runs(status.as_deref(), task.as_deref(), limit, offset, false)?;
                    Ok(runs.into())
                })
                .await?
        }
        ("GET", [kind @ ("chats" | "runs"), id, "history"]) => {
            crate::live::history(s, kind, id, input).await?
        }
        ("POST", [kind @ ("chats" | "runs"), id, "event-content"]) => {
            crate::live::event_content(s, kind, id, input).await?
        }
        ("POST", [kind @ ("chats" | "runs"), id, "timeline-content"]) => {
            crate::live::content(s, kind, id, input).await?
        }
        ("GET", ["runs", id]) => s.store.run(id).await?,
        ("POST", ["runs", id, "chat"]) => {
            chat_command(s, input, "convert_run_to_chat", json!({ "runId": id })).await?
        }
        ("GET", ["runs", id, "events"]) => {
            s.store.run(id).await?;
            let after = input.number("after", 0, 0, i64::MAX)?;
            let limit = input.number("limit", 100, 1, 500)?;
            let id = (*id).to_owned();
            s.store
                .read(move |db| Ok(serde_json::to_value(db.event_page(&id, after, limit)?)?))
                .await?
        }
        ("GET", [provider, "models"]) => {
            let provider = crate::provider::Provider::parse(provider)?;
            crate::capabilities::account_operation("list_models")
                .expect("registered model catalog")
                .invoke(
                    s,
                    json!({ "provider": provider }),
                    crate::capabilities::ResumeContext::http(input)?,
                )
                .await?
        }
        _ => return Ok(None),
    };
    Ok(Some(result))
}

/// Hook rules. Every member reaches them; the rule domain decides who may do what.
async fn rule_routes(s: &Arc<Service>, input: &Input, route: Route<'_>) -> Result<Option<Value>> {
    let query = |field: &str| {
        input
            .query
            .get(field)
            .map_or(Value::Null, |value| Value::String(value.clone()))
    };
    let same = |field: &str, id: &str| {
        if input.body[field] != *id {
            return Err(Error::bad("Rule identity does not match the path."));
        }
        Ok(input.body.clone())
    };
    let (name, body) = match route {
        ("GET", ["hook-rules"]) => (
            "list_hook_rules",
            json!({ "agentId": query("agentId"), "instanceId": query("instanceId") }),
        ),
        ("GET", ["hook-rules", "catalog"]) => (
            "get_hook_rule_catalog",
            json!({ "agentId": query("agentId") }),
        ),
        ("POST", ["hook-rules", "validate"]) => ("validate_hook_rule", input.body.clone()),
        ("GET", ["hook-rules", "templates"]) => (
            "list_hook_rule_templates",
            json!({ "instanceId": query("instanceId") }),
        ),
        ("POST", ["hook-rules", "templates", "install"]) => {
            ("install_hook_rule_template", input.body.clone())
        }
        ("POST", ["hook-rules"]) => ("create_hook_rule", input.body.clone()),
        ("GET", ["hook-rules", id]) => ("get_hook_rule", json!({ "ruleId": id })),
        ("GET", ["hook-rules", id, "versions"]) => {
            ("list_hook_rule_versions", json!({ "ruleId": id }))
        }
        ("PUT", ["hook-rules", id]) => ("replace_hook_rule", same("ruleId", id)?),
        ("POST", ["hook-rules", id, "enabled"]) => ("set_hook_rule_enabled", same("ruleId", id)?),
        ("POST", ["hook-rules", id, "emergency-pause"]) => {
            ("set_hook_rule_emergency_pause", same("ruleId", id)?)
        }
        ("POST", ["hook-rules", id, "budget"]) => ("lift_hook_rule_budget", same("ruleId", id)?),
        ("POST", ["hook-rules", id, "duplicate"]) => ("duplicate_hook_rule", same("ruleId", id)?),
        ("DELETE", ["hook-rules", id]) => ("delete_hook_rule", json!({ "ruleId": id })),
        _ => return Ok(None),
    };
    // Optional filters arrive as nulls; the contracts omit them instead.
    let body = match body {
        Value::Object(mut fields) => {
            fields.retain(|_, value| !value.is_null());
            Value::Object(fields)
        }
        body => body,
    };
    let identity = input
        .identity
        .as_ref()
        .ok_or_else(|| Error::unauthorized("A verified identity is required."))?;
    let operation = crate::rules::rule_operation(name).expect("rule operation");
    Ok(Some(
        operation
            .invoke_as(s, body, crate::rules::Actor::from_identity(identity))
            .await?,
    ))
}

/// Queued messages only, with answers to secret questions redacted.
async fn chat_detail(s: &Service, id: &str) -> Result<Value> {
    let chat_id = id.to_owned();
    let mut detail = s
        .store
        .read(move |db| crate::chats::public_detail(db, &chat_id))
        .await?;
    if let Some(messages) = detail["messages"].as_array_mut() {
        messages.retain(|m| m["status"] != crate::chats::MessageStatus::Delivered);
    }
    Ok(detail)
}

async fn chat_command(s: &Arc<Service>, input: &Input, name: &str, args: Value) -> Result<Value> {
    let member_account = input
        .identity
        .as_ref()
        .filter(|identity| {
            identity.role == crate::auth::InstallationRole::Member && identity.mcp_scopes.is_none()
        })
        .map(|identity| identity.account_id.clone());
    crate::capabilities::chat_operation(name)
        .expect("registered conversation command")
        .invoke(
            s,
            args,
            crate::capabilities::ResumeContext::http(input)?,
            member_account,
        )
        .await
}

async fn chat_routes(s: &Arc<Service>, input: &Input, route: Route<'_>) -> Result<Option<Value>> {
    let confirmed = input.body["confirm"] == true;
    let result = match route {
        ("GET", ["chats"]) => {
            let view = input.query.get("view").map_or("active", String::as_str);
            s.chat_list_view(view, crate::reads::account(input)?)
                .await?
                .into()
        }
        ("DELETE", ["chats", id]) => {
            chat_command(
                s,
                input,
                "delete_chat",
                json!({ "chatId": id, "confirm": confirmed }),
            )
            .await?
        }
        ("POST", ["chats", id, "new-session"]) => {
            chat_command(
                s,
                input,
                "restart_chat_session",
                json!({ "chatId": id, "confirm": confirmed }),
            )
            .await?
        }
        ("POST", ["chats", id, "resume"]) => {
            let mut args = if input.body.is_null() {
                json!({})
            } else {
                input.body.clone()
            };
            let Some(fields) = args.as_object_mut() else {
                return Err(Error::bad("Resume input must be an object."));
            };
            if fields
                .get("chatId")
                .is_some_and(|requested| requested.as_str() != Some(id))
            {
                return Err(Error::conflict(
                    "Resume chatId must match the conversation in the request path.",
                ));
            }
            fields.insert("chatId".into(), (*id).into());
            chat_command(s, input, "resume_chat", args).await?
        }
        ("POST", ["chats", id, "compact"]) => {
            chat_command(s, input, "compact_chat", json!({ "chatId": id })).await?
        }
        ("POST", ["chats", id, "restore"]) => {
            chat_command(s, input, "restore_chat", json!({ "chatId": id })).await?
        }
        ("POST", ["chats", id, "pause"]) => {
            chat_command(
                s,
                input,
                "set_chat_paused",
                json!({ "chatId": id, "paused": input.body["paused"] }),
            )
            .await?
        }
        ("POST", ["chats", id, "stop"]) => {
            chat_command(s, input, "stop_chat", json!({ "chatId": id })).await?
        }
        ("POST", ["chats"]) => chat_command(s, input, "create_chat", input.body.clone()).await?,
        ("PUT", ["chats", id]) => {
            let mut args = input.body.clone();
            args["chatId"] = (*id).into();
            chat_command(s, input, "rename_chat", args).await?
        }
        ("GET", ["chats", id]) => chat_detail(s, id).await?,
        ("GET", ["chats", id, "coding-agent"]) => {
            chat_command(
                s,
                input,
                "get_chat_coding_agent_options",
                json!({ "chatId": id }),
            )
            .await?
        }
        ("POST", ["chats", id, "coding-agent"]) => {
            let mut args = input.body.clone();
            args["chatId"] = (*id).into();
            chat_command(s, input, "switch_chat_coding_agent", args).await?
        }
        ("POST", ["chats", id, "messages"]) => {
            let message = input.body.get("message").unwrap_or(&input.body);
            chat_command(
                s,
                input,
                "send_chat_message",
                json!({ "chatId": id, "message": message }),
            )
            .await?
        }
        ("PUT", ["chats", id, "messages", message_id]) => {
            let mut message = input.body.get("message").unwrap_or(&input.body).clone();
            // The existing UI editor submits a displayed message; only command settings
            // enter the shared typed editor, with the path supplying its identity.
            if let Some(fields) = message.as_object_mut() {
                fields.retain(|key, _| {
                    matches!(
                        key.as_str(),
                        "text"
                            | "attachmentIds"
                            | "mode"
                            | "provider"
                            | "model"
                            | "reasoning"
                            | "speed"
                            | "plan"
                    )
                });
            }
            chat_command(
                s,
                input,
                "edit_chat_message",
                json!({ "chatId": id, "messageId": message_id, "message": message }),
            )
            .await?
        }
        ("DELETE", ["chats", id, "messages", message]) => {
            chat_command(
                s,
                input,
                "delete_chat_message",
                json!({ "chatId": id, "messageId": message }),
            )
            .await?
        }
        ("POST", ["chats", id, "questions", question, "cancel"]) => {
            chat_command(
                s,
                input,
                "cancel_chat_question",
                json!({"chatId":id,"questionId":question}),
            )
            .await?
        }
        ("POST", ["chats", id, "questions", question, "answer"]) => {
            chat_command(
                s,
                input,
                "answer_chat_question",
                json!({"chatId":id,"questionId":question,"answer":input.body}),
            )
            .await?
        }
        ("POST", ["chats", id, "side-questions"]) => {
            chat_command(
                s,
                input,
                "ask_chat_side_question",
                json!({ "chatId": id, "question": input.body }),
            )
            .await?
        }
        ("POST", ["chats", id, "side-questions", question, "cancel"]) => {
            chat_command(
                s,
                input,
                "cancel_chat_side_question",
                json!({ "chatId": id, "sideQuestionId": question }),
            )
            .await?
        }
        _ => return Ok(None),
    };
    Ok(Some(result))
}

async fn skill_routes(s: &Arc<Service>, input: &Input, route: Route<'_>) -> Result<Option<Value>> {
    let command = match route {
        ("GET", ["skills"]) => Some(("list_skills", json!({ "scope": input.query.get("scope") }))),
        ("POST", ["skills", "import"]) => Some(("import_skill", input.body.clone())),
        ("POST", ["skills", "stage"]) => Some(("stage_skill_upload", input.body.clone())),
        ("POST", ["skills", scope, "import"]) => {
            Some(("import_skill", skill_body(input, scope, None)?))
        }
        ("PUT", ["skills", scope, name]) => {
            Some(("save_skill", skill_body(input, scope, Some(name))?))
        }
        ("DELETE", ["skills", scope, name]) => {
            Some(("delete_skill", json!({"scope": scope, "name": name})))
        }
        ("GET", ["skills", scope, name, "bundle"]) => {
            Some(("read_skill", json!({"scope": scope, "name": name})))
        }
        ("POST", ["skills", scope, name, "bundle"]) => {
            Some(("edit_skill_files", skill_body(input, scope, Some(name))?))
        }
        ("GET", ["skills", scope, name, "file"]) => Some((
            "read_skill",
            json!({"scope": scope, "name": name, "path": input.query.get("path").ok_or_else(|| Error::bad("Choose a file path."))?}),
        )),
        ("GET", ["skills", scope, name, "archive"]) => Some((
            "export_skill_archive",
            json!({"scope": scope, "name": name, "offset": input.number("offset", 0, 0, i64::from(u32::MAX))?, "revision": input.query.get("revision")}),
        )),
        ("GET", ["skills", scope, name, "source"]) => Some((
            "get_skill_source_status",
            json!({"scope": scope, "name": name}),
        )),
        ("POST", ["skills", scope, name, "source", "update"]) => {
            Some(("update_skill_source", skill_body(input, scope, Some(name))?))
        }
        ("DELETE", ["skills", scope, name, "source"]) => {
            Some(("forget_skill_source", json!({"scope": scope, "name": name})))
        }
        _ => None,
    };
    let Some((name, body)) = command else {
        return Ok(None);
    };
    let result = crate::capabilities::resource_operation(name)
        .unwrap()
        .invoke(s, body, crate::capabilities::ResumeContext::http(input)?)
        .await?;
    Ok(Some(result))
}

fn skill_body(input: &Input, scope: &str, name: Option<&str>) -> Result<Value> {
    let mut body = input
        .body
        .as_object()
        .cloned()
        .ok_or_else(|| Error::bad("Expected an object."))?;
    for (field, expected) in [("scope", Some(scope)), ("name", name)] {
        if let Some(expected) = expected {
            if body.get(field).is_some_and(|actual| actual != expected) {
                return Err(Error::bad("Skill identity does not match the path."));
            }
            body.insert(field.into(), expected.into());
        }
    }
    Ok(Value::Object(body))
}

/// Installation settings and personal read state, through their registered capabilities.
async fn settings_routes(
    s: &Arc<Service>,
    input: &Input,
    route: Route<'_>,
) -> Result<Option<Value>> {
    let (name, body) = match route {
        ("POST", ["settings", "coding-agents", "read"]) => {
            ("get_coding_agent_configuration", input.body.clone())
        }
        ("POST", ["settings", "coding-agents", "preview"]) => {
            ("preview_coding_agent_configuration", input.body.clone())
        }
        ("PUT", ["settings", "coding-agents"]) => {
            ("save_coding_agent_configuration", input.body.clone())
        }
        ("GET", ["settings"]) => ("get_installation_settings", json!({})),
        ("GET", ["settings", "chat-plan"]) => ("get_chat_plan_settings", json!({})),
        ("PUT", ["settings", "chat-plan"]) => ("save_chat_plan_settings", input.body.clone()),
        ("GET", ["settings", "task-storage"]) => ("get_task_storage_limits", json!({})),
        ("PUT", ["settings", "task-storage"]) => ("save_task_storage_limits", input.body.clone()),
        ("GET", ["overview"]) => ("get_installation_overview", json!({})),
        ("GET", ["audit"]) => ("list_audit", json!({})),
        ("POST", ["reads"]) => ("mark_read", input.body.clone()),
        ("POST", ["dismissals"]) => ("set_outcome_aside", input.body.clone()),
        ("GET", ["settings", "conversations"]) => {
            return Ok(Some(
                chat_command(s, input, "get_conversation_retention", json!({})).await?,
            ));
        }
        ("PUT", ["settings", "conversations"]) => {
            return Ok(Some(
                chat_command(s, input, "set_conversation_retention", input.body.clone()).await?,
            ));
        }
        _ => return Ok(None),
    };
    let context = crate::capabilities::ResumeContext::http(input)?;
    let result = match crate::capabilities::personal_operation(name) {
        Some(operation) => operation.invoke(s, body, context).await?,
        None => {
            crate::capabilities::settings_operation(name)
                .expect("registered settings capability")
                .invoke(s, body, context)
                .await?
        }
    };
    Ok(Some(result))
}
