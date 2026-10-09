use crate::{
    config::now,
    error::{Error, Result},
    http::Input,
    service::{Service, next_occurrences},
    store::RUN_SUMMARY,
    validation::text,
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
    if under(&input.path, "/api/nodes") {
        return crate::nodes::admin(s, input).await;
    }
    if under(&input.path, "/api/accounts") {
        return crate::accounts::routes(s, input).await;
    }
    if under(&input.path, "/api/onepassword") {
        return crate::onepassword::routes(s, input).await;
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
    if let Some(result) = connection_routes(s, input, route).await? {
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
                "agents": db.list("agents")?.len(),
                "projects": db.list("projects")?.len(),
                "tasks": db.list("tasks")?,
                "runs": db.runs(None, None, 8, 0, false)?,
                "concurrency": concurrency,
            }))
        })
        .await
}

/// Agents, projects, tasks and their scheduling.
async fn record_routes(s: &Arc<Service>, input: &Input, route: Route<'_>) -> Result<Option<Value>> {
    let result = match route {
        ("GET", ["github", "repositories"]) => {
            crate::github_projects::list(s, input.number("page", 1, 1, 10000)?).await?
        }
        ("POST", ["projects", "github"]) => {
            static IMPORT: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
            let _guard = IMPORT.try_lock().map_err(|_| {
                Error::conflict("A GitHub import is already running. Retry shortly.")
            })?;
            crate::github_projects::import(s, input.body.clone()).await?
        }
        ("GET", ["overview"]) => overview(s).await?,
        ("GET", ["agent-avatars"]) => json!({ "configured": s.avatars.configured(s).await? }),
        ("POST", ["agents", id, "avatar", "generate"]) => s.avatars.generate(s, id).await?,
        ("GET", [kind @ ("agents" | "projects" | "tasks")]) => s.store.list(kind).await?.into(),
        ("POST", ["task-authors", "refresh"]) => {
            s.synchronize_task_authors().await?;
            json!({ "updated": true })
        }
        ("POST", [kind @ ("agents" | "projects" | "tasks")]) => save(s, kind, input, None).await?,
        ("PUT", [kind @ ("agents" | "projects" | "tasks"), id]) => {
            save(s, kind, input, Some(id)).await?
        }
        ("DELETE", [kind @ ("agents" | "projects" | "tasks"), id]) => {
            s.remove(kind, id).await?;
            json!({ "deleted": true })
        }
        ("POST", ["tasks", id, "run"]) => s.enqueue(id, "manual", None).await?,
        ("POST", ["schedule", "preview"]) => {
            let occurrences = next_occurrences(
                input.string("cron", 500)?,
                input.string("timezone", 100)?,
                now(),
                3,
            )?;
            json!({ "occurrences": occurrences })
        }
        ("GET", ["tasks", "activity"]) => task_activity(s).await?,
        _ => return Ok(None),
    };
    Ok(Some(result))
}

/// The latest run of every task.
async fn task_activity(s: &Service) -> Result<Value> {
    let query = format!(
        "SELECT {RUN_SUMMARY} FROM runs
         WHERE id IN (
           SELECT (SELECT id FROM runs WHERE task_id=records.id ORDER BY created_at DESC,id DESC LIMIT 1)
           FROM records WHERE kind='tasks'
         )
         ORDER BY created_at DESC,id DESC"
    );
    s.store
        .read(move |db| Ok(db.json_rows(&query, [])?.into()))
        .await
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
        ("GET", ["runs", id, "events"]) => {
            s.store.run(id).await?;
            let after = input.number("after", 0, 0, i64::MAX)?;
            let limit = input.number("limit", 100, 1, 500)?;
            let id = (*id).to_owned();
            s.store
                .read(move |db| Ok(db.events(&id, after, limit)?.into()))
                .await?
        }
        ("POST", ["runs", id, "retry"]) => {
            let run = s.store.run(id).await?;
            s.enqueue(text(&run, "taskId"), "retry", None).await?
        }
        ("GET", ["codex", "models"]) => s.models.list(s).await?,
        _ => return Ok(None),
    };
    Ok(Some(result))
}

/// Queued messages only, with answers to secret questions redacted.
async fn chat_detail(s: &Service, id: &str) -> Result<Value> {
    let mut detail = s.chat_detail(id).await?;
    let private = detail["questions"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|q| {
            q["fields"]
                .as_array()
                .is_some_and(|fields| fields.iter().any(|f| f["secret"] == true))
        })
        .map(|q| q["id"].clone())
        .collect::<Vec<_>>();
    if let Some(messages) = detail["messages"].as_array_mut() {
        messages.retain(|m| m["status"] != crate::chats::MessageStatus::Delivered);
        for message in messages {
            if private.contains(&message["questionId"]) {
                message["text"] = "Private answer".into();
                if let Some(message) = message.as_object_mut() {
                    message.remove("answers");
                }
            }
        }
    }
    detail["error"] = if crate::conversation_lifecycle::is_active(&detail) {
        s.store
            .kv(&format!("chat-error:{id}"))
            .await?
            .unwrap_or(Value::Null)
    } else {
        Value::Null
    };
    Ok(detail)
}

async fn chat_routes(s: &Arc<Service>, input: &Input, route: Route<'_>) -> Result<Option<Value>> {
    let confirmed = input.body["confirm"] == true;
    let result = match route {
        ("GET", ["chats"]) => {
            let view = input.query.get("view").map_or("active", String::as_str);
            s.chat_list_view(view).await?.into()
        }
        ("DELETE", ["chats", id]) => s.chat_trash(id, confirmed).await?,
        ("POST", ["chats", id, "new-session"]) => s.chat_new_session(id, confirmed).await?,
        ("POST", ["chats", id, "restore"]) => s.chat_restore(id).await?,
        ("POST", ["chats"]) => s.chat_create(input.body.clone()).await?,
        ("GET", ["chats", id]) => chat_detail(s, id).await?,
        ("POST", ["chats", id, "messages"]) => s.chat_send(id, input.body.clone()).await?,
        ("PUT", ["chats", id, "messages", message]) => {
            s.chat_edit(id, message, Some(input.body.clone())).await?
        }
        ("DELETE", ["chats", id, "messages", message]) => s.chat_edit(id, message, None).await?,
        ("POST", ["chats", id, "questions", question, "answer"]) => {
            s.question_answer(id, question, input.body.clone()).await?
        }
        _ => return Ok(None),
    };
    Ok(Some(result))
}

async fn skill_routes(s: &Arc<Service>, input: &Input, route: Route<'_>) -> Result<Option<Value>> {
    let result = match route {
        ("GET", ["skills"]) => {
            let mut items = s.skills.list("global", None).await?;
            for project in s.store.list("projects").await? {
                let path = Path::new(text(&project, "path"));
                items.extend(s.skills.list(text(&project, "id"), Some(path)).await?);
            }
            items.into()
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
        ("GET", ["skills", scope, name, "files"]) => {
            let project = skill_project(s, scope).await?;
            s.skills.files(name, project.as_deref()).await?.into()
        }
        (method @ ("GET" | "PUT"), ["skills", scope, name, "file"]) => {
            let file = if method == "GET" {
                input
                    .query
                    .get("path")
                    .map(String::as_str)
                    .ok_or_else(|| Error::bad("Choose a file path."))?
            } else {
                input.string("path", 4096)?
            };
            let content = if method == "PUT" {
                Some(input.string("content", 100000)?)
            } else {
                None
            };
            let project = skill_project(s, scope).await?;
            s.skills
                .file(name, file, content, project.as_deref())
                .await?
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
    let selected = s.store.list("tasks").await?.iter().any(|task| {
        task["skills"]
            .as_array()
            .is_some_and(|skills| skills.iter().any(|skill| skill == &key))
    });
    if selected {
        return Err(Error::conflict(
            "This skill is selected by a task. Update that task first.",
        ));
    }
    s.skills.remove(name, project).await?;
    let detail = json!({
        "scope": scope,
        "name": name
    });
    s.store.audit("skill.deleted", detail).await?;
    Ok(json!({ "deleted": true }))
}

async fn connection_routes(
    s: &Arc<Service>,
    input: &Input,
    route: Route<'_>,
) -> Result<Option<Value>> {
    let result = match route {
        ("GET", ["connections"]) => {
            let refresh = input.query.get("refresh").is_some_and(|s| s == "true");
            s.connections.status(s, refresh).await?
        }
        ("GET", ["claude", "models"]) => crate::claude::model_catalog(s).await?,
        ("GET", ["connections", "login"]) => s.connections.flow().await,
        ("POST", ["connections", "login"]) => {
            if input.string("provider", 20)? != "github" {
                return Err(Error::bad("Unknown connection provider."));
            }
            s.connections.start(s).await?
        }
        ("DELETE", ["connections", "login"]) => {
            s.connections.cancel().await;
            json!({ "cancelled": true })
        }
        ("GET", ["agents", id, "github-token"]) => {
            s.get("agents", id).await?;
            let configured = s.store.kv(&format!("agent-github:{id}")).await?.is_some();
            json!({ "configured": configured })
        }
        ("PUT", ["agents", id, "github-token"]) => set_github_token(s, id, input).await?,
        _ => return Ok(None),
    };
    Ok(Some(result))
}

async fn set_github_token(s: &Service, id: &str, input: &Input) -> Result<Value> {
    s.get("agents", id).await?;
    let token = input.string("token", 500)?.trim();
    let key = format!("agent-github:{id}");
    if token.is_empty() {
        s.store.delete(&key).await?;
    } else {
        s.store.set(&key, token.into(), None).await?;
    }
    let detail = json!({
        "agentId": id,
        "provider": "github"
    });
    s.store.audit("agent.connection.updated", detail).await?;
    Ok(json!({ "configured": !token.is_empty() }))
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

async fn save(s: &Arc<Service>, kind: &str, input: &Input, id: Option<&str>) -> Result<Value> {
    match kind {
        "agents" => s.agent(input.body.clone(), id).await,
        "projects" => s.project(input.body.clone(), id).await,
        _ => {
            s.task_as(input.body.clone(), id, input.identity.as_ref())
                .await
        }
    }
}

async fn skill_project(s: &Service, scope: &str) -> Result<Option<PathBuf>> {
    if scope == "global" {
        return Ok(None);
    }
    Ok(Some(PathBuf::from(text(
        &s.get("projects", scope).await?,
        "path",
    ))))
}
