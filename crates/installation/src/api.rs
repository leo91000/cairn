use crate::{
    error::{Error, Result},
    http::Input,
    service::Service,
};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

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
    if let Some(result) = crate::reads::routes(s, input).await {
        return result;
    }
    let segments = input
        .path
        .trim_start_matches("/api/")
        .split('/')
        .collect::<Vec<_>>();
    let route = (input.method.as_str(), segments.as_slice());
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

async fn overview(s: &Service) -> Result<Value> {
    let concurrency = s.config.concurrency;
    s.store
        .read(move |db| {
            Ok(json!({
                "counts": db.stats()?,
                "agents": db.agent_snapshots()?.len(),
                "projects": db.projects()?.len(),
                "tasks": db.missions()?,
                "runs": db.runs(None, None, 8, 0, false)?,
                "concurrency": concurrency,
            }))
        })
        .await
}

/// Agents, projects, tasks and their scheduling.
async fn record_routes(s: &Arc<Service>, input: &Input, route: Route<'_>) -> Result<Option<Value>> {
    let agent_command = match route {
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
    let result = match route {
        ("POST", ["projects", "import"]) => {
            static IMPORT: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
            let _guard = IMPORT.try_lock().map_err(|_| {
                Error::conflict("A project import is already running. Retry shortly.")
            })?;
            crate::plugins::projects::import(s, &input.body).await?
        }
        ("GET", ["overview"]) => overview(s).await?,
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
    crate::capabilities::chat_operation(name)
        .expect("registered conversation command")
        .invoke(s, args, crate::capabilities::ResumeContext::http(input)?)
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
            s.question_cancel_authorized(
                id,
                question,
                crate::native::policy::chat_authorization(id),
            )
            .await?
        }
        ("POST", ["chats", id, "questions", question, "answer"]) => {
            s.question_answer(id, question, input.body.clone()).await?
        }
        _ => return Ok(None),
    };
    Ok(Some(result))
}

async fn skill_routes(s: &Arc<Service>, input: &Input, route: Route<'_>) -> Result<Option<Value>> {
    use crate::skills::sources;
    let result = match route {
        ("GET", ["skills"]) => {
            let mut items = s.skills.list("global", None).await?;
            for project in s.store.projects().await? {
                let path = Path::new(&project.settings.path);
                items.extend(s.skills.list(&project.id.to_string(), Some(path)).await?);
            }
            sources::attach(s, &mut items).await?;
            s.attach_skill_usage(&mut items).await?;
            items.into()
        }
        ("POST", ["skills", scope, "import"]) => {
            let project = skill_project(s, scope).await?;
            let result = match input.body["source"].as_str() {
                Some("archive") => {
                    s.skills
                        .import_archive(
                            scope,
                            input.string("upload", 100)?,
                            input.body["path"].as_str(),
                            project.as_deref(),
                        )
                        .await?
                }
                Some("plugin") => {
                    sources::import(s, scope, &input.body, project.as_deref()).await?
                }
                _ => return Err(Error::bad("source: use archive or plugin")),
            };
            if let Some(name) = result["imported"]["name"].as_str() {
                let detail = json!({
                    "scope": scope,
                    "name": name
                });
                s.store.audit("skill.saved", detail).await?;
            }
            result
        }
        ("PUT", ["skills", scope, name]) => {
            let project = skill_project(s, scope).await?;
            let content = input.string("content", 100000)?;
            let result = s.skills.save(name, content, project.as_deref()).await?;
            let detail = json!({
                "scope": scope,
                "name": name
            });
            s.store.audit("skill.saved", detail).await?;
            result
        }
        ("DELETE", ["skills", scope, name]) => {
            let project = skill_project(s, scope).await?;
            delete_skill(s, scope, name, project.as_deref()).await?
        }
        (_, ["skills", scope, _]) => {
            skill_project(s, scope).await?;
            return Err(Error::not_found("Not found"));
        }
        ("GET", ["skills", scope, name, "bundle"]) => {
            let project = skill_project(s, scope).await?;
            let mut bundle = s.skills.bundle(scope, name, project.as_deref()).await?;
            sources::source(s, scope, name, &mut bundle).await?;
            bundle
        }
        ("POST", ["skills", scope, name, "bundle"]) => {
            let project = skill_project(s, scope).await?;
            let base = match &input.body["base"] {
                Value::Null => None,
                Value::String(base) => Some(base.as_str()),
                _ => return Err(Error::bad("base: expected a revision or null")),
            };
            let (changes, uploads) = s.skills.changes(&input.body["changes"]).await?;
            let saved = s
                .skills
                .apply(name, base, &changes, project.as_deref())
                .await?;
            s.skills.discard(&uploads).await;
            let detail = json!({
                "scope": scope,
                "name": name
            });
            s.store.audit("skill.saved", detail).await?;
            let mut bundle = crate::skills::bundle_response(scope, name, &saved);
            sources::source(s, scope, name, &mut bundle).await?;
            bundle
        }
        ("GET", ["skills", scope, name, "files"]) => {
            let project = skill_project(s, scope).await?;
            s.skills.files(name, project.as_deref()).await?.into()
        }
        ("GET", ["skills", scope, name, "file"]) => {
            let file = input
                .query
                .get("path")
                .map(String::as_str)
                .ok_or_else(|| Error::bad("Choose a file path."))?;
            let project = skill_project(s, scope).await?;
            s.skills.read(name, file, project.as_deref()).await?
        }
        ("PUT", ["skills", scope, name, "file"]) => {
            let file = input.string("path", 4096)?;
            let content = input.string("content", 150_000)?;
            let bytes = crate::skills::decode(content, input.body["encoding"].as_str())?;
            let project = skill_project(s, scope).await?;
            let result = if file == "SKILL.md" {
                let content = std::str::from_utf8(&bytes)
                    .map_err(|_| Error::bad("SKILL.md must be UTF-8 text."))?;
                let parsed = crate::skills::metadata(content)?;
                s.skills
                    .write(
                        name,
                        file,
                        bytes,
                        input.body["executable"].as_bool(),
                        project.as_deref(),
                    )
                    .await?;
                serde_json::to_value(parsed)?
            } else {
                let executable = input.body["executable"].as_bool();
                s.skills
                    .write(name, file, bytes, executable, project.as_deref())
                    .await?
            };
            let detail = json!({
                "scope": scope,
                "name": name
            });
            s.store.audit("skill.saved", detail).await?;
            result
        }
        ("GET", ["skills", scope, name, "archive"]) => {
            let project = skill_project(s, scope).await?;
            let offset = input.number("offset", 0, 0, i64::from(u32::MAX))?;
            let revision = input.query.get("revision").map(String::as_str);
            s.skills
                .archive(name, revision, offset as u64, project.as_deref())
                .await?
        }
        ("GET", ["skills", scope, name, "source"]) => {
            let project = skill_project(s, scope).await?;
            sources::status(s, scope, name, project.as_deref()).await?
        }
        ("POST", ["skills", scope, name, "source", "update"]) => {
            let project = skill_project(s, scope).await?;
            sources::update(s, scope, name, &input.body, project.as_deref()).await?
        }
        ("DELETE", ["skills", scope, name, "source"]) => {
            skill_project(s, scope).await?;
            sources::forget(s, scope, name).await?;
            json!({ "detached": true })
        }
        _ => return Ok(None),
    };
    Ok(Some(result))
}

async fn delete_skill(
    s: &Service,
    scope: &str,
    name: &str,
    project: Option<&Path>,
) -> Result<Value> {
    let key = format!("{scope}/{name}");
    let selected = s.store.missions().await?.iter().any(|task| {
        task.settings
            .skills
            .as_ref()
            .is_some_and(|skills| skills.contains(&key))
    });
    if selected {
        return Err(Error::conflict(
            "This skill is selected by a task. Update that task first.",
        ));
    }
    crate::skills::sources::remove(s, scope, name, project).await?;
    let detail = json!({
        "scope": scope,
        "name": name
    });
    s.store.audit("skill.deleted", detail).await?;
    Ok(json!({ "deleted": true }))
}

fn settings(s: &Service) -> Value {
    let commit = std::env::var("APP_COMMIT").unwrap_or_else(|_| "development".into());
    json!({
        "publicUrl": s.config.public_url,
        "workspaceRoots": s.config.workspace_roots,
        "home": s.config.home,
        "concurrency": s.config.concurrency,
        "mcpUrl": format!("{}/mcp", s.config.public_url),
        "version": env!("CARGO_PKG_VERSION"),
        "commit": commit,
        "protocol": "2026-07-28",
        "nativeMcpOauth": true,
    })
}

async fn settings_routes(
    s: &Arc<Service>,
    input: &Input,
    route: Route<'_>,
) -> Result<Option<Value>> {
    let result = match route {
        ("GET", ["settings"]) => settings(s),
        ("GET", ["settings", "conversations"]) => chat_command(s, input, "get_conversation_retention", json!({})).await?,
        ("PUT", ["settings", "conversations"]) => {
            chat_command(s, input, "set_conversation_retention", input.body.clone()).await?
        }
        ("GET", ["settings", "storage"]) => crate::object_storage::settings(s)?,
        ("PUT", ["settings", "storage"]) => {
            crate::object_storage::save_settings(s, &input.body).await?
        }
        ("POST", ["settings", "storage", "check"]) => {
            crate::object_storage::Storage::configured(s)?.probe().await?;
            json!({ "ok": true })
        }
        ("GET", ["audit"]) => {
            s.store
                .read(|db| {
                    let rows = db.json_rows(
                        "SELECT json_object('id',id,'created_at',created_at,'action',action,'detail',detail)
                         FROM audit ORDER BY id DESC LIMIT 100",
                        [],
                    )?;
                    Ok(rows.into())
                })
                .await?
        }
        _ => return Ok(None),
    };
    Ok(Some(result))
}

async fn skill_project(s: &Service, scope: &str) -> Result<Option<PathBuf>> {
    if scope == "global" {
        return Ok(None);
    }
    Ok(Some(PathBuf::from(
        s.get_project(scope).await?.settings.path,
    )))
}
