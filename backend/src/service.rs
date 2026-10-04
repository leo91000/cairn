use crate::{
    auth::Auth,
    config::{Config, MAIN_AGENT_ID, id, now},
    error::{Error, Result, required},
    run_status::RunStatus,
    skills::Skills,
    store::{Db, Store, merge},
    validation::{parse, text},
    vault::Vault,
};
use serde_json::{Value, json};
use std::{path::Path, sync::Arc};
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
pub struct Service {
    pub node_maintenance_tasks: Arc<tokio::sync::Mutex<std::collections::HashSet<String>>>,
    pub node_lease_deadlines:
        Arc<tokio::sync::Mutex<std::collections::HashMap<String, tokio::time::Instant>>>,
    pub started: tokio::time::Instant,
    pub node_backup_operation: Arc<crate::nodes::coordination::Coordination>,
    pub node_publication_notify: Arc<tokio::sync::Notify>,
    pub node_backup_lock: Arc<tokio::sync::Mutex<()>>,
    pub shared_block_collection: Arc<tokio::sync::Mutex<()>>,
    pub node_transport: Arc<crate::nodes::transport::Transport>,
    pub avatars: Arc<crate::agent_avatars::AgentAvatars>,
    pub artifacts: Arc<crate::artifacts::Artifacts>,
    pub worker: Arc<crate::worker::Worker>,
    pub mcps: Arc<crate::mcps::Mcps>,
    pub projects: Arc<crate::project_workspaces::Projects>,
    pub accounts: Arc<crate::accounts::Accounts>,
    pub models: Arc<crate::models::Models>,
    pub connections: Arc<crate::connections::Connections>,
    pub notifications: crate::notifications::Notifications,
    pub conversation_storage_lock: Arc<tokio::sync::Mutex<()>>,
    pub attachment_upload: Arc<tokio::sync::Mutex<()>>,
    pub config: Config,
    pub store: Store,
    pub auth: Auth,
    pub vault: Vault,
    pub skills: Skills,
    pub http: reqwest::Client,
    pub disk_http: reqwest::Client,
    pub hot_s3: Arc<crate::object_storage::HotS3>,
    pub shutdown: CancellationToken,
}

impl Service {
    pub async fn new(config: Config) -> Result<Arc<Self>> {
        if config.worker_enabled
            && config.runner_url.is_empty()
            && std::env::var("NODE_ENV").is_ok_and(|v| v == "production")
        {
            return Err(Error::bad(
                "Production execution requires the Firecracker runner. Set RUNNER_URL; \
                    shared host execution is available only in development.",
            ));
        }
        let store = Store::open(&config.data_dir)?;
        let vault = Vault::new(store.clone(), &config.data_dir)?;
        let service = Arc::new(Self {
            node_maintenance_tasks: Arc::default(),
            node_lease_deadlines: Arc::default(),
            started: tokio::time::Instant::now(),
            node_backup_operation: Arc::default(),
            node_publication_notify: Arc::default(),
            node_backup_lock: Arc::default(),
            shared_block_collection: Arc::default(),
            node_transport: Arc::default(),
            avatars: Arc::default(),
            artifacts: Arc::default(),
            worker: Arc::default(),
            mcps: Arc::default(),
            projects: Arc::default(),
            accounts: Arc::default(),
            models: Arc::default(),
            connections: Arc::default(),
            notifications: crate::notifications::Notifications::default(),
            conversation_storage_lock: Arc::default(),
            attachment_upload: Arc::default(),
            auth: Auth::new(store.clone(), config.public_url.clone()),
            skills: Skills {
                config: config.clone(),
            },
            http: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(std::time::Duration::from_secs(20))
                .build()
                .map_err(Error::internal)?,
            hot_s3: Arc::new(crate::object_storage::HotS3::new()),
            // Publications can take minutes while making progress. Their reader
            // bounds individual I/O waits instead of timing out the whole disk.
            disk_http: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .connect_timeout(std::time::Duration::from_secs(20))
                .build()
                .map_err(Error::internal)?,
            config,
            store,
            vault,
            shutdown: CancellationToken::new(),
        });
        service.migrate_agents().await?;
        service.store.transaction(crate::accounts::migrate).await?;
        service.avatars.recover(&service).await?;
        tokio::spawn(crate::artifacts::preview::recover(service.clone()));
        Ok(service)
    }

    async fn migrate_agents(&self) -> Result<()> {
        for mut agent in self.store.list("agents").await? {
            if agent["access"].get("mcps").is_none() || agent["access"].get("nodes").is_none() {
                agent["access"] = policy(&agent);
                self.store.put("agents", agent).await?;
            }
        }
        if self.store.get("agents", MAIN_AGENT_ID).await?.is_none() {
            let mut agent = parse(
                "agent",
                json!({
                    "name": "Main agent",
                    "description": "Your default agent, with access to every registered project, skill, and shared connection.",
                }),
            )?;
            agent["id"] = MAIN_AGENT_ID.into();
            agent["createdAt"] = now().into();
            self.store.put("agents", agent).await?;
        }
        // Migrate the built-in agent's old default once; preserve custom limits and
        // any later choice to explicitly restore a two-hour budget.
        self.store
            .transaction(|db| {
                let key = "migration:main-agent-unlimited";
                if db.kv(key)?.is_some() {
                    return Ok(());
                }
                if let Some(mut agent) = db.get("agents", MAIN_AGENT_ID)?
                    && agent["timeoutMinutes"] == 120
                {
                    agent["timeoutMinutes"] = 0.into();
                    db.put("agents", &agent)?;
                }
                db.set(key, &json!(true), None)
            })
            .await
    }

    pub async fn get(&self, kind: &str, id: &str) -> Result<Value> {
        required(self.store.get(kind, id).await?, "Record not found")
    }

    pub async fn agent(
        self: &Arc<Self>,
        mut input: Value,
        existing_id: Option<&str>,
    ) -> Result<Value> {
        let existing = match existing_id {
            Some(id) => Some(self.get("agents", id).await?),
            None => None,
        };
        if let Some(existing) = &existing {
            inherit_agent_settings(&mut input, existing);
        }
        let mut agent = parse("agent", input)?;
        crate::claude::validate_agent(&agent)?;
        agent["id"] = existing_id.map_or_else(id, str::to_owned).into();
        agent["createdAt"] = existing
            .as_ref()
            .map_or_else(|| now().into(), |v| v["createdAt"].clone());
        let access = policy(&agent);
        validate_access(&agent, &access)?;
        self.require_access_targets(&access).await?;
        let agent = self
            .store
            .transaction(move |db| save_agent(db, agent, &access))
            .await?;
        if existing_id.is_none() && self.avatars.configured(self).await? {
            // Creation remains successful even when portrait scheduling fails.
            return Ok(self
                .avatars
                .generate(self, text(&agent, "id"))
                .await
                .unwrap_or(agent));
        }
        Ok(agent)
    }

    /// Every project, MCP connection and tool permission must reference an existing record.
    async fn require_access_targets(&self, access: &Value) -> Result<()> {
        for kind in ["projects", "mcps"] {
            for value in access[kind].as_array().into_iter().flatten() {
                self.get(kind, value.as_str().unwrap_or("")).await?;
            }
        }
        let Some(tools) = access["mcpTools"].as_object() else {
            return Ok(());
        };
        for key in tools.keys() {
            self.get("mcps", key).await?;
            if !allowed(&access["mcps"], key) {
                return Err(Error::bad(
                    "Tool permissions require access to the MCP connection.",
                ));
            }
        }
        Ok(())
    }

    pub async fn project(&self, input: Value, existing: Option<&str>) -> Result<Value> {
        let mut project = parse("project", input)?;
        let actual = crate::skills::workspace(
            Path::new(text(&project, "path")),
            &self.config.workspace_roots,
        )
        .await?;
        let previous = match existing {
            Some(id) => Some(self.get("projects", id).await?),
            None => None,
        };
        project["origin"] = normalize_origin(&git_origin(&actual).await).into();
        project["path"] = actual.to_string_lossy().into_owned().into();
        project["id"] = existing.map_or_else(id, str::to_owned).into();
        project["createdAt"] = previous.map_or_else(|| now().into(), |v| v["createdAt"].clone());
        self.store.save("projects", project, "project.saved").await
    }

    pub async fn task(&self, input: Value, existing: Option<&str>) -> Result<Value> {
        let mut task = parse("task", input)?;
        let agent = self.get("agents", text(&task, "agentId")).await?;
        task_projects(&agent, &task, &self.store.list("projects").await?)?;
        if let Some(cron) = task["cron"].as_str() {
            next_occurrences(cron, text(&task, "timezone"), now(), 1)?;
        }
        task["id"] = existing.map_or_else(id, str::to_owned).into();
        task["createdAt"] = if let Some(id) = existing {
            self.get("tasks", id).await?["createdAt"].clone()
        } else {
            now().into()
        };
        let scheduled =
            task["enabled"] == true && task["archived"] != true && task["cron"].is_string();
        task["nextRun"] = if scheduled {
            next_occurrences(text(&task, "cron"), text(&task, "timezone"), now(), 1)?[0].into()
        } else {
            Value::Null
        };
        self.store.save("tasks", task, "task.saved").await
    }

    pub async fn remove(&self, kind: &str, id: &str) -> Result<()> {
        let (kind, id) = (kind.to_owned(), id.to_owned());
        self.store
            .transaction(move |db| {
                required(db.get(&kind, &id)?, "Record not found")?;
                ensure_removable(db, &kind, &id)?;
                if kind == "agents" {
                    forget_agent(db, &id)?;
                }
                db.remove(&kind, &id)?;
                db.audit(&format!("{kind}.deleted"), &json!({ "id": id }))
            })
            .await
    }

    pub async fn agent_skills(&self, agent: &Value) -> Result<Vec<Value>> {
        let access = policy(agent);
        let mut available = self.skills.list("global", None).await?;
        for project in self.store.list("projects").await? {
            if allowed(&access["projects"], text(&project, "id")) {
                available.extend(
                    self.skills
                        .list(
                            text(&project, "id"),
                            Some(Path::new(text(&project, "path"))),
                        )
                        .await?,
                );
            }
        }
        available.retain(|s| allowed(&access["skills"], &skill_key(s)));
        Ok(available)
    }

    pub async fn snapshot(&self, task: Value, trigger: &str) -> Result<Value> {
        if task["archived"] == true {
            return Err(Error::conflict(
                "Restore this archived task before running it.",
            ));
        }
        let agent = self.get("agents", text(&task, "agentId")).await?;
        let all_projects = self.store.list("projects").await?;
        let projects = task_projects(&agent, &task, &all_projects)?;
        let available_projects =
            task_projects(&agent, &json!({ "projectId": null }), &all_projects)?;
        let project = if projects.len() == 1 {
            projects[0].clone()
        } else {
            Value::Null
        };
        let available = self
            .agent_skills(&agent)
            .await?
            .into_iter()
            .filter(|s| s["scope"] == "global" || projects.iter().any(|p| p["id"] == s["scope"]))
            .collect::<Vec<_>>();
        let keys = task["skills"].as_array().cloned().unwrap_or_else(|| {
            available
                .iter()
                .filter(|s| s["valid"] == true)
                .map(|s| skill_key(s).into())
                .collect()
        });
        let access = policy(&agent);
        let mut skills = Vec::new();
        for key in keys {
            let key = key.as_str().unwrap_or("");
            if !allowed(&access["skills"], key) {
                return Err(Error::bad(format!("Skill outside agent access: {key}")));
            }
            let skill = available
                .iter()
                .find(|s| skill_key(s) == key && s["valid"] == true)
                .ok_or_else(|| Error::bad(format!("Skill unavailable or invalid: {key}")))?;
            skills.push(json!({
                "name": skill["name"],
                "path": skill["path"],
                "content": skill["content"],
            }));
        }
        let snapshot = json!({
            "task": task,
            "agent": agent,
            "project": project,
            "projects": projects,
            "availableProjects": available_projects,
            "skills": skills,
        });
        Ok(json!({
            "id": id(),
            "taskId": task["id"],
            "projectId": project["id"],
            "status": RunStatus::Queued,
            "trigger": trigger,
            "createdAt": now(),
            "startedAt": null,
            "finishedAt": null,
            "summary": "",
            "sessionId": null,
            "workspace": null,
            "usage": null,
            "snapshot": snapshot,
        }))
    }

    pub async fn enqueue(
        &self,
        task_id: &str,
        trigger: &str,
        dedupe: Option<String>,
    ) -> Result<Value> {
        let run = self
            .snapshot(self.get("tasks", task_id).await?, trigger)
            .await?;
        crate::nodes::require_node(&run["snapshot"]["agent"])?;
        let result = self
            .store
            .transaction(move |db| {
                db.add_run(&run, dedupe.as_deref())?;
                db.event(text(&run, "id"), "status", "Queued", None)?;
                let detail = json!({
                    "id": run["id"],
                    "taskId": run["taskId"],
                    "trigger": run["trigger"],
                });
                db.audit("run.queued", &detail)?;
                Ok(run)
            })
            .await?;
        self.worker.notify();
        Ok(result)
    }

    pub async fn schedule(&self) -> Result<()> {
        for task in self.store.list("tasks").await? {
            if !schedule_due(&task) {
                continue;
            }
            if let Err(error) = self
                .enqueue(
                    text(&task, "id"),
                    "schedule",
                    Some(format!("{}:{}", text(&task, "id"), task["nextRun"])),
                )
                .await
                && error.status != 409
            {
                let detail = json!({
                    "taskId": task["id"],
                    "error": error.message
                });
                self.store.audit("schedule.failed", detail).await?;
            }
            let next = next_occurrences(text(&task, "cron"), text(&task, "timezone"), now(), 1)?[0];
            self.store
                .write(move |db| {
                    if let Some(mut current) = db.get("tasks", text(&task, "id"))?
                        && current["nextRun"] == task["nextRun"]
                    {
                        current["nextRun"] = next.into();
                        db.put("tasks", &current)?;
                    }
                    Ok(())
                })
                .await?;
        }
        Ok(())
    }
}

fn schedule_due(task: &Value) -> bool {
    task["enabled"] == true
        && task["archived"] != true
        && !task["cron"].is_null()
        && task["nextRun"].as_i64().is_some_and(|time| time <= now())
}

fn skill_key(skill: &Value) -> String {
    format!("{}/{}", text(skill, "scope"), text(skill, "name"))
}

async fn git_origin(path: &Path) -> String {
    let args = [
        "-C",
        path.to_str().unwrap_or(""),
        "config",
        "--get",
        "remote.origin.url",
    ]
    .map(str::to_owned);
    crate::process::bounded_output(
        crate::process::command("git", &args, &std::env::vars().collect(), None),
        std::time::Duration::from_secs(3),
        8000,
    )
    .await
    .ok()
    .filter(|output| output.success)
    .map(|output| output.stdout.trim().to_owned())
    .unwrap_or_default()
}

fn uses_project(run: &Value, id: &str) -> bool {
    run_projects(run).iter().any(|p| p["id"] == id)
        || run["workspaces"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|w| w["projectId"] == id)
}

/// Refuses to remove a record still referenced by an agent, a task or active work.
fn ensure_removable(db: &Db<'_>, kind: &str, id: &str) -> Result<()> {
    if kind == "agents" && id == MAIN_AGENT_ID {
        return Err(Error::conflict("The main agent cannot be removed."));
    }
    let assigned = |agent: &Value| {
        let projects = &policy(agent)["projects"];
        !projects.is_null() && allowed(projects, id)
    };
    if kind == "projects" && db.list("agents")?.iter().any(assigned) {
        return Err(Error::conflict(
            "This project is assigned to an agent. Update the agent first.",
        ));
    }
    let task_field = if kind == "agents" {
        "agentId"
    } else {
        "projectId"
    };
    if kind != "tasks" && db.list("tasks")?.iter().any(|t| t[task_field] == id) {
        return Err(Error::conflict(
            "This item is used by a task. Update or remove that task first.",
        ));
    }
    let busy = db.active()?.iter().any(|run| match kind {
        "tasks" => run["taskId"] == id,
        "projects" => uses_project(run, id),
        _ => run["snapshot"]["agent"]["id"] == id,
    });
    if busy {
        return Err(Error::conflict(
            "This item has active work. Cancel or wait for the run first.",
        ));
    }
    Ok(())
}

/// Drops an agent's credentials, portrait and 1Password assignments.
fn forget_agent(db: &Db<'_>, id: &str) -> Result<()> {
    db.delete(&format!("agent-github:{id}"))?;
    db.delete(&format!("agent-avatar:{id}"))?;
    for mut account in db.list("onepassword")? {
        if let Some(agents) = account["agentIds"].as_array_mut() {
            agents.retain(|agent| agent != id);
        }
        db.put("onepassword", &account)?;
    }
    Ok(())
}

/// Older clients omit settings they do not know about; keep the saved ones.
fn inherit_agent_settings(input: &mut Value, existing: &Value) {
    if input.get("access").is_none() {
        input["access"] = policy(existing);
    }
    if input.get("provider").is_none() {
        input["provider"] = crate::provider::Provider::of_agent(existing)
            .as_str()
            .into();
    }
    if !input["access"].is_object() {
        return;
    }
    for key in ["nodes"] {
        if input["access"].get(key).is_none() {
            input["access"][key] = policy(existing)[key].clone();
        }
    }
}

fn restricted(access: &Value) -> bool {
    !access["projects"].is_null()
        || !access["skills"].is_null()
        || access["github"] != true
        || !access["mcps"].is_null()
        || access["mcpTools"]
            .as_object()
            .is_some_and(|tools| !tools.is_empty())
}

fn validate_access(agent: &Value, access: &Value) -> Result<()> {
    if !access["projects"].is_null() && access["github"] == true {
        return Err(Error::bad(
            "Shared GitHub credentials require access to all projects. Disable the GitHub connection for an agent with selected projects.",
        ));
    }
    if agent["id"] == MAIN_AGENT_ID && restricted(access) {
        return Err(Error::bad(
            "The main agent always has access to all resources. Create another agent for restricted access.",
        ));
    }
    Ok(())
}

fn save_agent(db: &Db<'_>, mut agent: Value, access: &Value) -> Result<Value> {
    // Serialize grants with revocation; a concurrent editor must never
    // restore a grant that the revocation transaction just removed.
    for node in access["nodes"].as_array().into_iter().flatten() {
        if node == crate::nodes::LOCAL_NODE_ID {
            continue;
        }
        let record = required(
            db.get("nodes", node.as_str().unwrap_or(""))?,
            "Node not found",
        )?;
        if record["revoked"] == true {
            return Err(Error::bad("A revoked node cannot be authorized."));
        }
    }
    // Read the latest portrait inside the save transaction: editing an agent
    // must not overwrite an upload or background generation that just finished.
    if let Some(existing) = db.get("agents", text(&agent, "id"))?
        && let Some(avatar) = existing.get("avatar")
    {
        agent["avatar"] = avatar.clone();
    }
    db.put("agents", &agent)?;
    db.audit("agent.saved", &json!({ "id": agent["id"] }))?;
    Ok(agent)
}

pub fn policy(agent: &Value) -> Value {
    let mut value = json!({
        "projects": null,
        "skills": null,
        "mcps": null,
        "mcpTools": {},
        "github": true,
        "sandbox": "yolo",
        "nodes": [crate::nodes::LOCAL_NODE_ID],
    });
    merge(&mut value, &agent["access"]);
    // Agents restricted before MCP permissions existed get no MCP connection.
    let legacy_restricted = agent["id"] != MAIN_AGENT_ID
        && agent["access"].is_object()
        && agent["access"].get("mcps").is_none()
        && (!value["projects"].is_null()
            || !value["skills"].is_null()
            || value["github"] != true
            || value["sandbox"] != "yolo");
    if legacy_restricted {
        value["mcps"] = json!([]);
    }
    value
}

/// Whether the `current` agent still allows everything `granted` allowed. Work started
/// under `granted` continues when access is unchanged or wider, but never with access
/// the owner has since removed. Settings older versions saved (such as `maxResources`)
/// do not count, and node grants are excluded: placement follows the current ones.
pub fn covers(current: &Value, granted: &Value) -> bool {
    let current = policy(current);
    let granted = policy(granted);

    let scopes = ["projects", "skills", "mcps"]
        .iter()
        .all(|key| within(&granted[*key], &current[*key]));
    // A server without a tool selection allows all of its enabled tools.
    let tools = current["mcpTools"]
        .as_object()
        .into_iter()
        .flatten()
        .all(|(server, selection)| within(&granted["mcpTools"][server], selection));
    let github = current["github"] == true || granted["github"] != true;
    let sandbox = sandbox_rank(&current["sandbox"]) >= sandbox_rank(&granted["sandbox"]);
    scopes && tools && github && sandbox
}

/// Whether a granted scope stays within the current one; `null` means everything.
fn within(granted: &Value, current: &Value) -> bool {
    if current.is_null() {
        return true;
    }
    let (Some(granted), Some(current)) = (granted.as_array(), current.as_array()) else {
        return false;
    };
    granted.iter().all(|item| current.contains(item))
}

/// Sandboxes from the most restrictive to the most permissive.
fn sandbox_rank(sandbox: &Value) -> u8 {
    match sandbox.as_str() {
        Some("yolo") => 2,
        Some("workspace-write") => 1,
        _ => 0,
    }
}

pub fn allowed(scope: &Value, id: &str) -> bool {
    scope.is_null() || scope.as_array().is_some_and(|a| a.iter().any(|v| v == id))
}

pub fn isolated(agent: &Value) -> bool {
    let a = policy(agent);
    !a["projects"].is_null()
        || !a["skills"].is_null()
        || !a["mcps"].is_null()
        || a["github"] != true
        || a["sandbox"] != "yolo"
        || a["mcpTools"].as_object().is_some_and(|a| !a.is_empty())
}

pub fn task_projects(agent: &Value, task: &Value, projects: &[Value]) -> Result<Vec<Value>> {
    let a = policy(agent);
    let available = projects
        .iter()
        .filter(|p| allowed(&a["projects"], text(p, "id")))
        .cloned()
        .collect::<Vec<_>>();
    if !task["projectId"].is_null() {
        return available
            .into_iter()
            .find(|p| p["id"] == task["projectId"])
            .map(|p| vec![p])
            .ok_or_else(|| Error::bad("This project is unavailable to the selected agent."));
    }
    Ok(available)
}

pub fn run_projects(run: &Value) -> Vec<Value> {
    run["snapshot"]["projects"]
        .as_array()
        .cloned()
        .unwrap_or_else(|| {
            if run["snapshot"]["project"].is_object() {
                vec![run["snapshot"]["project"].clone()]
            } else {
                vec![]
            }
        })
}

pub fn next_occurrences(pattern: &str, zone: &str, time: i64, count: usize) -> Result<Vec<i64>> {
    use chrono::{Offset, TimeZone};
    use std::str::FromStr;
    if pattern.split_whitespace().count() != 5 {
        return Err(Error::bad(
            "Enter a valid five-field cron expression and IANA timezone.",
        ));
    }
    let zone = chrono_tz::Tz::from_str(zone)
        .map_err(|_| Error::bad("Enter a valid five-field cron expression and IANA timezone."))?;
    let cron = croner::Cron::from_str(pattern)
        .map_err(|_| Error::bad("Enter a valid five-field cron expression and IANA timezone."))?;
    let mut current = zone
        .timestamp_millis_opt(time)
        .single()
        .ok_or_else(|| Error::bad("Invalid schedule time"))?;
    let mut result = Vec::new();
    for _ in 0..count {
        let mut next = cron
            .find_next_occurrence(&current, false)
            .map_err(|_| Error::bad("No future schedule occurrence."))?;
        // cron-parser preserves the scheduled minute when a fixed wall-clock
        // time falls in the spring gap. Croner clamps it to the gap's end.
        // Resolve that missing time using the offset immediately before the gap.
        if !cron.is_time_matching(&next).map_err(Error::internal)? {
            let before = next - chrono::Duration::hours(3);
            let offset = before.offset().fix();
            let shifted = cron
                .find_next_occurrence(&current.with_timezone(&offset), false)
                .map_err(Error::internal)?
                .with_timezone(&zone);
            if shifted >= next && shifted - next < chrono::Duration::hours(3) {
                next = shifted;
            }
        }
        current = next;
        result.push(current.timestamp_millis());
    }
    Ok(result)
}

fn normalize_origin(remote: &str) -> String {
    if let Ok(mut url) = url::Url::parse(remote)
        && ["http", "https", "ssh", "git"].contains(&url.scheme())
    {
        let _ = url.set_username("");
        let _ = url.set_password(None);
        url.set_query(None);
        url.set_fragment(None);
        return url.to_string();
    }
    if let Some((host, path)) = remote.split_once(':') {
        let host = host.rsplit('@').next().unwrap_or(host);
        if !host.is_empty() && !path.is_empty() && !host.contains('/') {
            return format!("{host}:{path}");
        }
    }
    String::new()
}
