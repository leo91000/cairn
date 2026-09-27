use crate::{
    auth::Auth,
    config::{Config, MAIN_AGENT_ID, id, now},
    error::{Error, Result, required},
    skills::Skills,
    store::{Store, merge},
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
    pub node_backup_operation: Arc<tokio::sync::Mutex<()>>,
    pub node_backup_lock: Arc<tokio::sync::Mutex<()>>,
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
    pub retention_lock: Arc<tokio::sync::Mutex<()>>,
    pub attachment_upload: Arc<tokio::sync::Mutex<()>>,
    pub config: Config,
    pub store: Store,
    pub auth: Auth,
    pub vault: Vault,
    pub skills: Skills,
    pub http: reqwest::Client,
    pub shutdown: CancellationToken,
}
impl Service {
    pub async fn new(config: Config) -> Result<Arc<Self>> {
        if config.worker_enabled
            && config.runner_url.is_empty()
            && std::env::var("NODE_ENV").is_ok_and(|v| v == "production")
        {
            return Err(Error::bad(
                "Production execution requires the Firecracker runner. Set RUNNER_URL; shared host execution is available only in development.",
            ));
        }
        let store = Store::open(&config.data_dir)?;
        let vault = Vault::new(store.clone(), &config.data_dir)?;
        let service = Arc::new(Self {
            node_maintenance_tasks: Default::default(),
            node_lease_deadlines: Default::default(),
            started: tokio::time::Instant::now(),
            node_backup_operation: Default::default(),
            node_backup_lock: Default::default(),
            node_transport: Default::default(),
            avatars: Default::default(),
            artifacts: Default::default(),
            worker: Default::default(),
            mcps: Default::default(),
            projects: Default::default(),
            accounts: Default::default(),
            models: Default::default(),
            connections: Default::default(),
            notifications: Default::default(),
            retention_lock: Default::default(),
            attachment_upload: Default::default(),
            auth: Auth::new(store.clone(), config.public_url.clone()),
            skills: Skills {
                config: config.clone(),
            },
            http: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(std::time::Duration::from_secs(20))
                .build()
                .map_err(Error::internal)?,
            config,
            store,
            vault,
            shutdown: CancellationToken::new(),
        });
        for mut agent in service.store.list("agents").await? {
            if agent["access"].get("mcps").is_none() || agent["access"].get("nodes").is_none() {
                agent["access"] = policy(&agent);
                service.store.put("agents", agent).await?;
            }
        }
        if service.store.get("agents", MAIN_AGENT_ID).await?.is_none() {
            let mut agent = parse(
                "agent",
                json!({
                "name":"Main agent","description":"Your default agent, with access to every registered project, skill, and shared connection."}
                ),
            )?;
            agent["id"] = MAIN_AGENT_ID.into();
            agent["createdAt"] = now().into();
            service.store.put("agents", agent).await?;
        }
        // Migrate the built-in agent's old default once; preserve custom limits and
        // any later choice to explicitly restore a two-hour budget.
        service
            .store
            .transaction(|db| {
                let key = "migration:main-agent-unlimited";
                if db.kv(key)?.is_none() {
                    if let Some(mut agent) = db.get("agents", MAIN_AGENT_ID)?
                        && agent["timeoutMinutes"] == 120
                    {
                        agent["timeoutMinutes"] = 0.into();
                        db.put("agents", &agent)?;
                    }
                    db.set(key, &json!(true), None)?;
                }
                Ok(())
            })
            .await?;
        service.store.transaction(crate::accounts::migrate).await?;
        service.avatars.recover(&service).await?;
        tokio::spawn(crate::artifacts::preview::recover(service.clone()));
        Ok(service)
    }
    pub async fn get(&self, kind: &str, id: &str) -> Result<Value> {
        required(self.store.get(kind, id).await?, "Record not found")
    }
    pub async fn agent(
        self: &Arc<Self>,
        mut input: Value,
        existing_id: Option<&str>,
    ) -> Result<Value> {
        let existing = if let Some(id) = existing_id {
            Some(self.get("agents", id).await?)
        } else {
            None
        };
        if input.get("access").is_none()
            && let Some(existing) = &existing
        {
            input["access"] = policy(existing);
        }
        if input.get("provider").is_none()
            && let Some(existing) = &existing
        {
            input["provider"] = crate::provider::Provider::of_agent(existing)
                .as_str()
                .into();
        }
        // Older clients submit the other access axes without knowing about nodes.
        if input["access"].is_object()
            && input["access"].get("nodes").is_none()
            && let Some(existing) = &existing
        {
            input["access"]["nodes"] = policy(existing)["nodes"].clone();
        }
        if input["access"].is_object()
            && input["access"].get("maxResources").is_none()
            && let Some(existing) = &existing
        {
            input["access"]["maxResources"] = policy(existing)["maxResources"].clone();
        }
        let mut agent = parse("agent", input)?;
        crate::claude::validate_agent(&agent)?;
        agent["id"] = existing_id.map(str::to_owned).unwrap_or_else(id).into();
        agent["createdAt"] = existing
            .as_ref()
            .map(|v| v["createdAt"].clone())
            .unwrap_or_else(|| now().into());
        let access = policy(&agent);
        if !access["projects"].is_null() && access["github"] == true {
            return Err(Error::bad(
                "Shared GitHub credentials require access to all projects. Disable the GitHub connection for an agent with selected projects.",
            ));
        }
        if agent["id"] == MAIN_AGENT_ID
            && (!access["projects"].is_null()
                || !access["skills"].is_null()
                || access["github"] != true
                || !access["mcps"].is_null()
                || access["mcpTools"]
                    .as_object()
                    .is_some_and(|v| !v.is_empty()))
        {
            return Err(Error::bad(
                "The main agent always has access to all resources. Create another agent for restricted access.",
            ));
        }
        for kind in ["projects", "mcps"] {
            for value in access[kind].as_array().into_iter().flatten() {
                self.get(kind, value.as_str().unwrap_or("")).await?;
            }
        }
        if let Some(tools) = access["mcpTools"].as_object() {
            for key in tools.keys() {
                self.get("mcps", key).await?;
                if !allowed(&access["mcps"], key) {
                    return Err(Error::bad(
                        "Tool permissions require access to the MCP connection.",
                    ));
                }
            }
        }
        let agent = self
            .store
            .transaction(move |db| {
                // Serialize grants with revocation; a concurrent editor must never
                // restore a grant that the revocation transaction just removed.
                for node in access["nodes"].as_array().into_iter().flatten() {
                    if node != crate::nodes::LOCAL_NODE_ID {
                        let record = required(
                            db.get("nodes", node.as_str().unwrap_or(""))?,
                            "Node not found",
                        )?;
                        if record["revoked"] == true {
                            return Err(Error::bad("A revoked node cannot be authorized."));
                        }
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
                db.audit("agent.saved", &json!({"id":agent["id"]}))?;
                Ok(agent)
            })
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
    pub async fn project(&self, input: Value, existing: Option<&str>) -> Result<Value> {
        let mut project = parse("project", input)?;
        let actual = crate::skills::workspace(
            Path::new(text(&project, "path")),
            &self.config.workspace_roots,
        )
        .await?;
        let previous = if let Some(id) = existing {
            Some(self.get("projects", id).await?)
        } else {
            None
        };
        let args = [
            "-C",
            actual.to_str().unwrap_or(""),
            "config",
            "--get",
            "remote.origin.url",
        ]
        .map(str::to_owned);
        let origin = crate::process::bounded_output(
            crate::process::command("git", &args, &std::env::vars().collect(), None),
            std::time::Duration::from_secs(3),
            8000,
        )
        .await
        .ok()
        .filter(|output| output.success)
        .map(|output| output.stdout.trim().to_owned())
        .unwrap_or_default();
        project["origin"] = normalize_origin(&origin).into();
        project["path"] = actual.to_string_lossy().into_owned().into();
        project["id"] = existing.map(str::to_owned).unwrap_or_else(id).into();
        project["createdAt"] = previous
            .map(|v| v["createdAt"].clone())
            .unwrap_or_else(|| now().into());
        self.store.save("projects", project, "project.saved").await
    }
    pub async fn task(&self, input: Value, existing: Option<&str>) -> Result<Value> {
        let mut task = parse("task", input)?;
        let agent = self.get("agents", text(&task, "agentId")).await?;
        task_projects(&agent, &task, &self.store.list("projects").await?)?;
        if let Some(cron) = task["cron"].as_str() {
            next_occurrences(cron, text(&task, "timezone"), now(), 1)?;
        }
        task["id"] = existing.map(str::to_owned).unwrap_or_else(id).into();
        task["createdAt"] = if let Some(id) = existing {
            self.get("tasks", id).await?["createdAt"].clone()
        } else {
            now().into()
        };
        task["nextRun"] =
            if task["enabled"] == true && task["archived"] != true && task["cron"].is_string() {
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
                if kind == "agents" && id == MAIN_AGENT_ID {
                    return Err(Error::new(409, "The main agent cannot be removed."));
                }
                if kind == "projects"
                    && db.list("agents")?.iter().any(|a| {
                        !policy(a)["projects"].is_null() && allowed(&policy(a)["projects"], &id)
                    })
                {
                    return Err(Error::new(
                        409,
                        "This project is assigned to an agent. Update the agent first.",
                    ));
                }
                if kind != "tasks"
                    && db.list("tasks")?.iter().any(|t| {
                        t[if kind == "agents" {
                            "agentId"
                        } else {
                            "projectId"
                        }] == id
                    })
                {
                    return Err(Error::new(
                        409,
                        "This item is used by a task. Update or remove that task first.",
                    ));
                }
                if db.active()?.iter().any(|r| match kind.as_str() {
                    "tasks" => r["taskId"] == id,
                    "projects" => {
                        run_projects(r).iter().any(|p| p["id"] == id)
                            || r["workspaces"]
                                .as_array()
                                .into_iter()
                                .flatten()
                                .any(|w| w["projectId"] == id)
                    }
                    _ => r["snapshot"]["agent"]["id"] == id,
                }) {
                    return Err(Error::new(
                        409,
                        "This item has active work. Cancel or wait for the run first.",
                    ));
                }
                if kind == "agents" {
                    db.delete(&format!("agent-github:{id}"))?;
                    db.delete(&format!("agent-avatar:{id}"))?;
                    for mut account in db.list("onepassword")? {
                        if let Some(agents) = account["agentIds"].as_array_mut() {
                            agents.retain(|agent| agent != &id);
                        }
                        db.put("onepassword", &account)?;
                    }
                }
                db.remove(&kind, &id)?;
                db.audit(
                    &format!("{kind}.deleted"),
                    &json!({
                    "id":id}
                    ),
                )
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
        available.retain(|s| {
            allowed(
                &access["skills"],
                &format!("{}/{}", text(s, "scope"), text(s, "name")),
            )
        });
        Ok(available)
    }
    pub async fn snapshot(&self, task: Value, trigger: &str) -> Result<Value> {
        if task["archived"] == true {
            return Err(Error::new(
                409,
                "Restore this archived task before running it.",
            ));
        }
        let agent = self.get("agents", text(&task, "agentId")).await?;
        let all_projects = self.store.list("projects").await?;
        let projects = task_projects(&agent, &task, &all_projects)?;
        let available_projects = task_projects(&agent, &json!({"projectId":null}), &all_projects)?;
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
                .map(|s| format!("{}/{}", text(s, "scope"), text(s, "name")).into())
                .collect()
        });
        let mut skills = Vec::new();
        for key in keys {
            let key = key.as_str().unwrap_or("");
            if !allowed(&policy(&agent)["skills"], key) {
                return Err(Error::bad(format!("Skill outside agent access: {key}")));
            }
            let s = available
                .iter()
                .find(|s| {
                    format!("{}/{}", text(s, "scope"), text(s, "name")) == key && s["valid"] == true
                })
                .ok_or_else(|| Error::bad(format!("Skill unavailable or invalid: {key}")))?;
            skills.push(json!({
            "name":s["name"],"path":s["path"],"content":s["content"]}
            ));
        }
        Ok(json!({
        "id":id(),"taskId":task["id"],"projectId":project["id"],"status":"queued","trigger":trigger,"createdAt":now(),"startedAt":null,"finishedAt":null,"summary":"","sessionId":null,"workspace":null,"usage":null,"snapshot":{
        "task":task,"agent":agent,"project":project,"projects":projects,"availableProjects":available_projects,"skills":skills}
        }
        ))
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
                db.audit(
                    "run.queued",
                    &json!({
                    "id":run["id"],"taskId":run["taskId"],"trigger":run["trigger"]}
                    ),
                )?;
                Ok(run)
            })
            .await?;
        self.worker.notify();
        Ok(result)
    }
    pub async fn schedule(&self) -> Result<()> {
        for task in self.store.list("tasks").await? {
            if task["enabled"] != true
                || task["archived"] == true
                || task["cron"].is_null()
                || task["nextRun"].as_i64().is_none_or(|time| time > now())
            {
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
                self.store
                    .audit(
                        "schedule.failed",
                        json!({
                        "taskId":task["id"],"error":error.message}
                        ),
                    )
                    .await?;
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
pub fn policy(agent: &Value) -> Value {
    let mut value = json!({
    "projects":null,"skills":null,"mcps":null,"mcpTools":{
    }
    ,"github":true,"sandbox":"yolo","nodes":[crate::nodes::LOCAL_NODE_ID],"maxResources":null}
    );
    merge(&mut value, &agent["access"]);
    if agent["id"] != MAIN_AGENT_ID
        && agent["access"].is_object()
        && agent["access"].get("mcps").is_none()
        && (!value["projects"].is_null()
            || !value["skills"].is_null()
            || value["github"] != true
            || value["sandbox"] != "yolo")
    {
        value["mcps"] = json!([]);
    }
    value
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
