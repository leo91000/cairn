//! Run-scoped project access: authorize, prepare a private seed, then import once.
use crate::{
    error::{Error, Result},
    mcp_server::{ToolResult, empty_listing},
    mcps::{grant_key, record::RunGrant},
    run_status::RunStatus,
    service::{Service, covers, run_projects},
    validation::{text, uuid},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    path::Path,
    sync::{Arc, Weak},
    time::Duration,
};
use tokio::sync::Mutex;

#[derive(Default)]
pub struct Projects {
    locks: Mutex<HashMap<String, Weak<Mutex<()>>>>,
}

pub fn catalog(run: &Value) -> Vec<Value> {
    run["snapshot"]["availableProjects"]
        .as_array()
        .cloned()
        .unwrap_or_else(|| run_projects(run))
}

pub fn authorize_in(db: &crate::store::Db<'_>, bearer: &str) -> Result<Value> {
    let denied = || Error::unauthorized("Workspace access expired or was revoked.");
    let grant = db.kv(&grant_key(bearer))?.ok_or_else(denied)?;
    let grant = RunGrant::deserialize(&grant)?;
    let run = db.run(&grant.run_id)?.ok_or_else(denied)?;
    let revoked = grant.message_id != run["chatExecution"]["messageId"]
        || !grant.workspace
        || run["status"] != RunStatus::Running
        || !run["cancelRequestedAt"].is_null();
    if revoked {
        return Err(denied());
    }
    let agent = &run["snapshot"]["agent"];
    let current = db.get("agents", text(agent, "id"))?.ok_or_else(denied)?;
    if !covers(&current, agent) {
        return Err(Error::forbidden("Agent permissions were reduced."));
    }
    Ok(run)
}

pub async fn authorize(s: &Service, bearer: &str) -> Result<Value> {
    let bearer = bearer.to_owned();
    s.store.read(move |db| authorize_in(db, &bearer)).await
}

/// Whether the registered project still matches the one this run was granted.
fn unchanged(current: &Value, granted: &Value) -> bool {
    current["path"] == granted["path"]
        && current["baseBranch"] == granted["baseBranch"]
        && (current["sourceMode"] == "local") == (granted["sourceMode"] == "local")
}

impl Projects {
    /// One open at a time per run and project; entries are dropped once unused.
    async fn lock(&self, key: String) -> Arc<Mutex<()>> {
        let mut locks = self.locks.lock().await;
        locks.retain(|_, lock| lock.strong_count() > 0);
        let lock = locks.get(&key).and_then(Weak::upgrade).unwrap_or_default();
        locks.insert(key, Arc::downgrade(&lock));
        lock
    }

    pub async fn open(&self, s: &Service, bearer: &str, project_id: &str) -> Result<Value> {
        uuid(project_id)?;
        let run = authorize(s, bearer).await?;
        let lock = self
            .lock(format!("{}:{project_id}", text(&run, "id")))
            .await;
        let _guard = lock.lock().await;
        let run = authorize(s, bearer).await?;
        let run_id = text(&run, "id");
        let project = catalog(&run)
            .into_iter()
            .find(|p| p["id"] == project_id)
            .ok_or_else(|| Error::forbidden("This project is not authorized for this run."))?;
        let current = s
            .store
            .get("projects", project_id)
            .await?
            .ok_or_else(|| Error::not_found("Project no longer exists."))?;
        if !unchanged(&current, &project) {
            return Err(Error::conflict(
                "Project configuration changed. Start a new conversation.",
            ));
        }
        let checkpoint = s
            .store
            .kv(&format!("run-checkpoint:{run_id}"))
            .await?
            .unwrap_or_default();
        let prepared = &checkpoint["prepared"];
        let root = Path::new(text(prepared, "projectRoot"));
        if !matches!(prepared["backend"].as_str(), Some("firecracker" | "native"))
            || !root.is_absolute()
        {
            return Err(Error::conflict(
                "The private workspace is not ready. Retry when the run is active.",
            ));
        }
        let attempt = text(&checkpoint, "runnerId");
        uuid(attempt)?;
        let status = format!("Opening {}", text(&project, "name"));
        s.store.event(run_id, "status", &status, None).await?;
        let agent_id = text(&run["snapshot"]["agent"], "id");
        let access = crate::plugins::run_credentials::fetch(s, Some(agent_id), &current).await?;
        let credentials = crate::plugins::run_credentials::RunCredentials {
            fetch: [(project_id.to_owned(), access)].into(),
            ..Default::default()
        };
        let entry =
            crate::execution::project_seed(&run, &project, &s.config, root, &credentials).await?;
        // Recheck the grant after a potentially slow clone and before transferring anything.
        authorize(s, bearer).await?;
        let imported = import(s, &run, attempt, project_id, &entry).await?;
        record_workspace(s, run_id, entry.clone()).await?;
        Ok(json!({
            "projectId": project_id,
            "name": project["name"],
            "path": entry["path"],
            "reused": imported["reused"],
            "revision": entry["revision"],
        }))
    }
}

/// Asks the run's VM to import the prepared project seed.
async fn import(
    s: &Service,
    run: &Value,
    attempt: &str,
    project_id: &str,
    entry: &Value,
) -> Result<Value> {
    let credential = crate::execution::secret(&s.config.data_dir, "runner-secret").await?;
    let node = crate::nodes::transport::url(s, text(run, "id")).await?;
    let body = json!({
        "runId": run["id"],
        "source": entry["path"],
        "target": entry["path"]
    });
    let response = s
        .http
        .post(format!("{node}/runs/{attempt}/projects/{project_id}"))
        .bearer_auth(credential)
        .json(&body)
        .timeout(Duration::from_secs(300))
        .send()
        .await
        .map_err(|_| {
            Error::unavailable(
                "Project transfer was interrupted. Retry open_project; saved files are preserved.",
            )
        })?;
    if !response.status().is_success() {
        return Err(Error::unavailable(
            "Project could not be opened in this VM. Retry when the run is active.",
        ));
    }
    let response: Value = response.json().await.map_err(Error::internal)?;
    if response["ok"] != true {
        return Err(Error::unavailable("Project import was not acknowledged."));
    }
    Ok(response)
}

/// Adds the project to the run's opened workspaces, once.
async fn record_workspace(s: &Service, run_id: &str, entry: Value) -> Result<()> {
    let id = run_id.to_owned();
    s.store
        .write(move |db| {
            let run = db
                .run(&id)?
                .ok_or_else(|| Error::not_found("Run not found."))?;
            let mut entries = run["workspaces"].as_array().cloned().unwrap_or_default();
            if !entries.iter().any(|w| w["projectId"] == entry["projectId"]) {
                entries.push(entry);
            }
            db.patch_run(&id, &json!({ "workspaces": entries }))?;
            Ok(())
        })
        .await
}

fn open_project_tool() -> Value {
    json!({
        "name": "open_project",
        "description": "Open an authorized project in this conversation's private workspace. Call only when you need its files. Repeated calls reuse existing files and changes. Use the returned path for commands and read its AGENTS.md before editing.",
        "inputSchema": {
            "type": "object",
            "properties": { "projectId": { "type": "string", "format": "uuid" } },
            "required": ["projectId"],
            "additionalProperties": false,
        },
    })
}

async fn call_tool(s: &Service, bearer: &str, name: &str, args: &Value) -> Result<Value> {
    let summary = Value::to_string;
    match name {
        "acquire_exclusive" => ToolResult::from_result(
            crate::nodes::exclusive::acquire(s, bearer, args).await,
            summary,
        ),
        "release_exclusive" => ToolResult::from_result(
            crate::nodes::exclusive::release(s, bearer, args).await,
            summary,
        ),
        "exclusive_status" => {
            ToolResult::from_result(crate::nodes::exclusive::status(s, bearer).await, summary)
        }
        "renew_exclusive" => ToolResult::from_result(
            crate::nodes::exclusive::renew(s, bearer, args).await,
            summary,
        ),
        "wait_for" => ToolResult::from_result(
            crate::chats::wait_for::schedule(s, bearer, args).await,
            |result| {
                if result["deferred"] == true {
                    "Wait saved. End this turn; Cairn will resume when the scheduled time arrives, the condition resolves or the timeout expires, subject to capacity.".into()
                } else {
                    "Condition already completed. Inspect the observation and continue this turn; no wakeup was scheduled.".into()
                }
            },
        ),

        "list_nodes" => {
            let run = authorize(s, bearer).await?;
            ToolResult::from_result(crate::nodes::moves::list(s, &run).await, summary)
        }
        "move_to_node" => {
            let run = authorize(s, bearer).await?;
            let result = crate::nodes::moves::request_by_agent(s, &run, args).await;
            ToolResult::from_result(result, summary)
        }
        "pending_questions" | "withdraw_question" => ToolResult::from_result(
            crate::chats::questions::for_agent(
                s,
                bearer,
                (name == "withdraw_question").then_some(args),
            )
            .await,
            summary,
        ),
        "report_outcome" => {
            ToolResult::from_result(crate::outcome::report(s, bearer, args).await, |_| {
                "Outcome saved.".into()
            })
        }
        "set_artifact_visibility" => ToolResult::from_result(
            crate::artifacts::sharing::for_agent(s, bearer, args).await,
            |result| {
                format!(
                    "Artifact {}. {}",
                    text(result, "visibility"),
                    text(result, "publicUrl")
                )
            },
        ),
        "publish_artifact" => {
            ToolResult::from_result(s.artifacts.publish(s, bearer, args).await, |result| {
                let url = result["publicUrl"]
                    .as_str()
                    .unwrap_or_else(|| text(result, "url"));
                format!("Published {}: {url}", text(result, "title"))
            })
        }
        "open_project" => ToolResult::from_result(
            s.projects.open(s, bearer, text(args, "projectId")).await,
            |result| {
                format!(
                    "{} is ready at {}",
                    text(result, "name"),
                    text(result, "path")
                )
            },
        ),
        _ => match crate::plugins::agents::call_tool(s, bearer, name, args).await {
            Ok(Some(value)) => ToolResult::from_result(Ok(value), summary),
            Ok(None) => Err(Error::not_found("Unknown workspace operation.")),
            Err(error) if error.status == 401 => Err(error),
            Err(error) => ToolResult::from_result(Err(error), summary),
        },
    }
}

pub async fn rpc(s: &Service, bearer: &str, method: &str, params: &Value) -> Result<Value> {
    if method == "tools/call" {
        return call_tool(s, bearer, text(params, "name"), &params["arguments"]).await;
    }
    if let Some(listing) = empty_listing(method) {
        return Ok(listing);
    }
    if method != "tools/list" {
        return Err(Error::not_found("Unknown workspace operation."));
    }
    // Conditions of granted plugins; listing without a live run offers Cairn's own.
    let conditions = match authorize(s, bearer).await {
        Ok(run) => {
            let agent_id = text(&run["snapshot"]["agent"], "id");
            crate::plugins::conditions::schemas(s, agent_id)
                .await
                .unwrap_or_default()
        }
        Err(_) => Vec::new(),
    };
    let mut tools = vec![
        open_project_tool(),
        crate::artifacts::tool(),
        crate::nodes::moves::list_tool(),
        crate::nodes::moves::tool(),
        crate::artifacts::sharing::tool(),
        crate::outcome::tool(),
        crate::chats::questions::list_tool(),
        crate::chats::questions::cancel_tool(),
        crate::chats::wait_for::tool(&conditions),
        crate::nodes::exclusive::acquire_tool(),
        crate::nodes::exclusive::release_tool(),
        crate::nodes::exclusive::status_tool(),
        crate::nodes::exclusive::renew_tool(),
    ];
    // Package tools never shadow Cairn's own.
    // Listing stays available without a live run; calls authorize on their own.
    let granted = crate::plugins::agents::tools(s, bearer)
        .await
        .unwrap_or_default();
    for tool in granted {
        if !tools
            .iter()
            .any(|existing| existing["name"] == tool["name"])
        {
            tools.push(tool);
        }
    }
    Ok(json!({ "tools": tools }))
}
