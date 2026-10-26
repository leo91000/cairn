use crate::{
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
    pub task_author_lock: Arc<tokio::sync::Mutex<()>>,
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
    pub code_mode: Arc<crate::mcp_server::code_mode::CodeMode>,
    pub projects: Arc<crate::project_workspaces::Projects>,
    pub plugins: Arc<crate::plugins::Plugins>,
    pub accounts: Arc<crate::accounts::Accounts>,
    pub models: Arc<crate::models::Models>,
    pub opencode: Arc<crate::opencode::Catalog>,
    pub conversation_storage_lock: Arc<tokio::sync::Mutex<()>>,
    pub attachment_upload: Arc<tokio::sync::Mutex<()>>,
    pub conversation_lists: Arc<crate::live::ConversationLists>,
    pub config: Config,
    pub store: Store,
    pub vault: Vault,
    pub skills: Skills,
    pub http: reqwest::Client,
    pub disk_http: reqwest::Client,
    pub hot_s3: Arc<crate::object_storage::HotS3>,
    pub shutdown: CancellationToken,
}

impl Service {
    pub async fn new(config: Config) -> Result<Arc<Self>> {
        if config.worker_enabled && config.runner_url.is_empty() && !cfg!(debug_assertions) {
            return Err(Error::bad(
                "Release execution requires the Firecracker runner. Set RUNNER_URL; \
                    shared host execution is available only in development.",
            ));
        }
        let store = Store::open(&config.data_dir)?;
        let vault = Vault::new(store.clone(), &config.data_dir)?;
        crate::nodes::connector::record_manager_origin(&config).await?;
        let service = Arc::new(Self {
            task_author_lock: Arc::default(),
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
            code_mode: Arc::default(),
            projects: Arc::default(),
            plugins: Arc::default(),
            accounts: Arc::default(),
            models: Arc::default(),
            opencode: Arc::default(),
            conversation_storage_lock: Arc::default(),
            attachment_upload: Arc::default(),
            conversation_lists: Arc::default(),
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
        service.bootstrap_main_agent().await?;
        service.avatars.recover(&service).await?;
        tokio::spawn(crate::artifacts::preview::recover(service.clone()));
        Ok(service)
    }

    async fn bootstrap_main_agent(&self) -> Result<()> {
        if self.store.agent(MAIN_AGENT_ID).await?.is_none() {
            let settings: cairn_contracts::AgentSettings = serde_json::from_value(json!({
                "name": "Main agent",
                "exclusiveNodes": [], "plugins": [], "access": cairn_contracts::AccessPolicy::default(),
                "description": "Your default agent, with access to every registered project, skill, and shared connection."
            }))?;
            let agent = crate::agents::new_agent(
                settings,
                Some(cairn_contracts::AgentId(
                    uuid::Uuid::parse_str(MAIN_AGENT_ID).unwrap(),
                )),
            );
            self.store
                .transaction(move |db| {
                    db.save_agent(&agent)?;
                    Ok(())
                })
                .await?;
        }
        Ok(())
    }

    pub async fn agent_snapshot(&self, id: &str) -> Result<Value> {
        required(self.store.agent_snapshot(id).await?, "Agent not found")
    }

    pub async fn get(&self, kind: &str, id: &str) -> Result<Value> {
        required(self.store.get(kind, id).await?, "Record not found")
    }

    pub(crate) async fn save_profile<F>(
        self: &Arc<Self>,
        command: crate::agents::SaveProfile,
        editor: AgentEditor,
        authorize: F,
    ) -> Result<cairn_contracts::Agent>
    where
        F: FnOnce(&Db<'_>) -> Result<()> + Send + 'static,
    {
        let (mut agent, expected, creation_id, review) = match command {
            crate::agents::SaveProfile::Create(input) => (
                crate::agents::new_agent(input.settings, input.creation_id),
                None,
                input.creation_id,
                input.review,
            ),
            crate::agents::SaveProfile::Replace(input) => (
                crate::agents::new_agent(input.settings, Some(input.agent_id)),
                Some(input.revision),
                None,
                input.review,
            ),
        };
        agent.settings.validate().map_err(Error::bad)?;
        review.validate().map_err(Error::bad)?;
        if creation_id.is_some_and(|id| id.0.get_version() != Some(uuid::Version::Random)) {
            return Err(Error::bad("creationId must be a random UUID"));
        }
        let snapshot = serde_json::to_value(&agent)?;
        crate::provider::Provider::of_agent(&snapshot)?
            .adapter()
            .validate_agent(&snapshot)?;
        validate_profile(&agent)?;
        // A3 still owns permission target lookup; pass only its typed wire view.
        let access = serde_json::to_value(&agent.settings.access)?;
        self.require_access_targets(&access).await?;
        if editor == AgentEditor::Installation
            && (!review.reapprove_delegations.is_empty() || !review.native_node_reviews.is_empty())
        {
            return Err(Error::forbidden(
                "Only the installation owner can approve native nodes or plugin delegations.",
            ));
        }
        for connection in &review.reapprove_delegations {
            let connection = crate::plugins::connections::load(self, connection).await?;
            crate::plugins::connections::ensure_owner(self, &connection).await?;
        }
        let accepts_agents = self.instances_accepting_agents(&snapshot).await?;
        let creating = expected.is_none();
        let owner_review =
            editor == AgentEditor::Owner && crate::nodes::native_mode::owner().is_some();
        let reapproved = !review.reapprove_delegations.is_empty();
        let (agent, created) = self.store.transaction(move |db| {
            authorize(db)?;
            let current = db.agent(&agent.id.to_string())?;
            if creating {
                if let Some(current) = &current {
                    if creation_id.is_some() { return Ok((current.clone(), false)); }
                    return Err(Error::conflict("Agent already exists."));
                }
                if db.0.query_row("SELECT EXISTS(SELECT 1 FROM records WHERE id=? UNION ALL SELECT 1 FROM projects WHERE id=?)", rusqlite::params![agent.id.to_string(), agent.id.to_string()], |row| row.get::<_, bool>(0))? {
                    return Err(Error::conflict("This creation identity belongs to a different resource."));
                }
            } else {
                let current = required(current.as_ref(), "Agent no longer exists")?;
                if expected.as_deref() != Some(current.revision.as_str()) {
                    return Err(Error::conflict("This agent was changed elsewhere. Reload it before saving."));
                }
                agent.created_at = current.created_at;
                agent.avatar = current.avatar.clone();
            }
            if editor == AgentEditor::Installation {
                let mut before_access = current.as_ref().map_or_else(|| cairn_contracts::AccessPolicy {
                    projects: Some(Vec::new()),
                    skills: Some(Vec::new()),
                    mcps: Some(Vec::new()),
                    nodes: Some(Vec::new()),
                    ..cairn_contracts::AccessPolicy::default()
                }, |profile| profile.settings.access.clone());
                // A tool ceiling only applies to connections retained by the proposed policy.
                // Removing a connection also removes its tool entry, without granting all tools.
                before_access.mcp_tools.retain(|connection, _| {
                    agent.settings.access.mcps.as_ref().is_none_or(|connections| connections.contains(connection))
                });
                let before = json!({ "access": before_access });
                let edited = serde_json::to_value(&agent)?;
                let expands = !covers(&before, &edited) || !node_access_covers(&before, &edited);
                let previous_plugins = current.as_ref().map(|a| a.settings.plugins.as_slice()).unwrap_or_default();
                let previous_exclusive = current.as_ref().map(|a| a.settings.exclusive_nodes.as_slice()).unwrap_or_default();
                if expands || agent.settings.access.backend == "native" && current.as_ref().is_none_or(|a| a.settings.access.backend != "native")
                    || agent.settings.plugins.iter().any(|id| !previous_plugins.contains(id))
                    || agent.settings.exclusive_nodes.iter().any(|id| !previous_exclusive.contains(id)) {
                    return Err(Error::forbidden("Only the installation owner can expand agent permissions."));
                }
                // Owner-managed live grants remain authoritative until A3; profile replacement
                // never revokes them through an installation agent.
                agent.settings.plugins = previous_plugins.to_vec();
                agent.settings.exclusive_nodes = previous_exclusive.to_vec();
                agent.settings.validate().map_err(Error::bad)?;
            }
            let snapshot = serde_json::to_value(&agent)?;
            for project in agent.settings.access.projects.as_ref().into_iter().flatten() {
                required(db.project(project)?, "Project no longer exists")?;
            }
            crate::agent_grants::validate(db, &snapshot, &accepts_agents)?;
            let access = serde_json::to_value(&agent.settings.access)?;
            let agent = crate::agent_grants::save_profile(db, agent, &access, owner_review, &review.native_node_reviews)?;
            let snapshot = serde_json::to_value(&agent)?;
            for connection in &review.reapprove_delegations {
                crate::plugins::connections::reapprove(db, connection, &snapshot)?;
            }
            Ok((agent, creating))
        }).await?;
        if reapproved {
            self.plugins.changed.notify_one();
        }
        if created && self.avatars.configured(self).await? {
            return Ok(self
                .avatars
                .generate(self, &agent.id.to_string())
                .await
                .unwrap_or(agent));
        }
        Ok(agent)
    }

    /// Plugin instances whose package gives agents access. A package that renders
    /// credentials into runs gives each agent one instance: its runs use that one.
    async fn instances_accepting_agents(&self, agent: &Value) -> Result<Vec<String>> {
        let requested = crate::agent_grants::plugins(agent);
        let mut accepting = Vec::new();
        let mut run_credentials = Vec::new();
        for instance in self.store.list(crate::plugins::connections::KIND).await? {
            let id = text(&instance, "id");
            if !requested.iter().any(|requested| requested == id) {
                continue;
            }
            let plugin = text(&instance, "pluginId");
            let Ok(package) = crate::plugins::registry::active(self, plugin).await else {
                continue;
            };
            if package.manifest.agent_access.is_none() {
                continue;
            }
            if crate::plugins::run_credentials::one_per_package(&package.manifest) {
                if run_credentials.contains(&plugin.to_owned()) {
                    return Err(Error::bad(format!(
                        "Grant one {} instance per agent: its runs use that instance's credentials.",
                        package.manifest.name
                    )));
                }
                run_credentials.push(plugin.to_owned());
            }
            accepting.push(id.to_owned());
        }
        Ok(accepting)
    }

    /// What saving an edited agent would interrupt: its queued or running work that
    /// the new access no longer covers, and plugin delegations it would suspend.
    pub(crate) async fn agent_preview<F>(
        &self,
        mut input: cairn_contracts::ReplaceAgentInput,
        authorize: F,
    ) -> Result<cairn_contracts::AgentPreview>
    where
        F: FnOnce(&Db<'_>) -> Result<()> + Send + 'static,
    {
        input.settings.validate().map_err(Error::bad)?;
        input.review.validate().map_err(Error::bad)?;
        let current = required(
            self.store.agent(&input.agent_id.to_string()).await?,
            "Agent not found",
        )?;
        if current.revision != input.revision {
            return Err(Error::conflict(
                "This agent was changed elsewhere. Reload it before saving.",
            ));
        }
        let mut agent = current;
        agent.settings = input.settings;
        validate_profile(&agent)?;
        let access = serde_json::to_value(&agent.settings.access)?;
        self.require_access_targets(&access).await?;
        let agent = serde_json::to_value(&agent)?;
        crate::provider::Provider::of_agent(&agent)?
            .adapter()
            .validate_agent(&agent)?;
        let id = input.agent_id.to_string();
        let owner = crate::relay::task_author_policy(self)
            .await?
            .map(|policy| policy.owner.access_id);
        self.store
            .read(move |db| {
                let tx = rusqlite::Transaction::new_unchecked(
                    db.0,
                    rusqlite::TransactionBehavior::Deferred,
                )?;
                let db = &Db(&tx);
                authorize(db)?;
                let current = required(db.agent(&id)?, "Agent not found")?;
                if current.revision != input.revision {
                    return Err(Error::conflict(
                        "This agent was changed elsewhere. Reload it before saving.",
                    ));
                }
                let mut queued = 0;
                let mut running = 0;
                for run in db.active()? {
                    let snapshot = &run["snapshot"]["agent"];
                    if snapshot["id"] != *id || covers(&agent, snapshot) {
                        continue;
                    }
                    if run["status"] == "queued" {
                        queued += 1;
                    } else {
                        running += 1;
                    }
                }
                let delegations = crate::plugins::connections::delegation_effects(
                    db,
                    &id,
                    &agent,
                    owner.as_deref(),
                )?;
                Ok(cairn_contracts::AgentPreview {
                    runs: cairn_contracts::AgentPreviewRuns { queued, running },
                    delegations: serde_json::from_value(serde_json::to_value(delegations)?)?,
                })
            })
            .await
    }

    /// Every project, MCP connection and tool permission must reference an existing record.
    async fn require_access_targets(&self, access: &Value) -> Result<()> {
        for value in access["projects"].as_array().into_iter().flatten() {
            self.get_project(value.as_str().unwrap_or("")).await?;
        }
        for kind in ["mcps"] {
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

    pub async fn task(&self, input: Value, existing: Option<&str>) -> Result<Value> {
        self.task_as(input, existing, None).await
    }

    pub(crate) async fn save_task(
        &self,
        input: Value,
        existing: Option<&str>,
        author: Option<cairn_protocol::TaskAuthorGrant>,
        bearer: Option<String>,
        reduces_execution: bool,
    ) -> Result<Value> {
        let creation_id = creation_id(&input, existing)?;
        let mut task = parse("task", input)?;
        let agent = self.agent_snapshot(text(&task, "agentId")).await?;
        let origin = crate::native::policy::origin();
        if crate::native::policy::native(&agent) && !reduces_execution {
            crate::native::policy::refresh(self).await?;
        }
        task_projects(&agent, &task, &self.store.projects().await?)?;
        if let Some(cron) = task["cron"].as_str() {
            next_occurrences(cron, text(&task, "timezone"), now(), 1)?;
        }
        task["id"] = existing
            .map(str::to_owned)
            .or(creation_id.clone())
            .unwrap_or_else(id)
            .into();
        task["createdAt"] = if let Some(id) = existing {
            let previous = self.get("tasks", id).await?;
            task["authorId"] = previous["authorId"].clone();
            task["authorAccessId"] = previous["authorAccessId"].clone();
            task["authorRemoved"] = previous["authorRemoved"].clone();
            previous["createdAt"].clone()
        } else {
            if let Some(author) = author {
                task["authorId"] = author.account_id.into();
                task["authorAccessId"] = author.access_id.into();
            }
            now().into()
        };
        let scheduled =
            task["enabled"] == true && task["archived"] != true && task["cron"].is_string();
        if scheduled && task["authorRemoved"] == true {
            return Err(Error::conflict(
                "This author's access ended. Duplicate the task to schedule it again.",
            ));
        }
        task["nextRun"] = if scheduled {
            next_occurrences(text(&task, "cron"), text(&task, "timezone"), now(), 1)?[0].into()
        } else {
            Value::Null
        };
        let existing = existing.map(str::to_owned);
        self.store
            .transaction(move |db| {
                required(
                    db.agent_snapshot(text(&task, "agentId"))?,
                    "Agent no longer exists",
                )?;
                let previous = existing
                    .as_deref()
                    .map(|id| db.get("tasks", id))
                    .transpose()?
                    .flatten();
                let grant = if reduces_execution {
                    required(previous.clone(), "Task not found")?
                } else {
                    crate::native::policy::task_grant(db, &task, previous.as_ref(), &origin)?
                };
                crate::native::policy::apply(&mut task, &grant);
                if let Some(bearer) = bearer {
                    crate::installation_mcp::authorize_task_in(
                        db,
                        &bearer,
                        &task,
                        existing.as_deref(),
                    )?;
                }
                if let Some(previous) = replayed_creation(db, "tasks", creation_id.as_deref())? {
                    return Ok(previous);
                }
                let agent = required(
                    db.agent_snapshot(text(&task, "agentId"))?,
                    "Agent no longer exists",
                )?;
                task_projects(&agent, &task, &db.projects()?)?;
                db.put("tasks", &task)?;
                db.audit("task.saved", &json!({ "id": task["id"] }))?;
                Ok(task)
            })
            .await
    }

    pub async fn remove(&self, kind: &str, id: &str) -> Result<()> {
        self.remove_authorized(kind, id, |_| Ok(())).await
    }

    pub(crate) async fn remove_authorized<F>(
        &self,
        kind: &str,
        id: &str,
        authorize: F,
    ) -> Result<()>
    where
        F: FnOnce(&Db<'_>) -> Result<()> + Send + 'static,
    {
        let (kind, id) = (kind.to_owned(), id.to_owned());
        self.store
            .transaction(move |db| {
                authorize(db)?;
                required(db.get(&kind, &id)?, "Record not found")?;
                ensure_removable(db, &kind, &id)?;
                db.remove(&kind, &id)?;
                db.audit(&format!("{kind}.deleted"), &json!({ "id": id }))
            })
            .await
    }

    pub async fn agent_skills(&self, agent: &Value) -> Result<Vec<Value>> {
        let access = policy(agent);
        let mut available = self.skills.list("global", None).await?;
        for project in self.store.projects().await? {
            if allowed(&access["projects"], &project.id.to_string()) {
                available.extend(
                    self.skills
                        .list(
                            &project.id.to_string(),
                            Some(Path::new(&project.settings.path)),
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
        let agent = self.agent_snapshot(text(&task, "agentId")).await?;
        let all_projects = self.store.projects().await?;
        let projects = task_projects(&agent, &task, &all_projects)?;
        let available_projects =
            task_projects(&agent, &json!({ "projectId": null }), &all_projects)?;
        let project = if projects.len() == 1 {
            Some(projects[0].clone())
        } else {
            None
        };
        let available = self
            .agent_skills(&agent)
            .await?
            .into_iter()
            .filter(|s| {
                s["scope"] == "global"
                    || projects
                        .iter()
                        .any(|p| Some(p.id.to_string().as_str()) == s["scope"].as_str())
            })
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
            // Freezing keeps queued and restarted runs on the files they were created with.
            let (bundle, content) = self.skills.freeze(Path::new(text(skill, "path"))).await?;
            let frozen_metadata = crate::skills::parse(&content)?;
            skills.push(json!({
                "name": skill["name"],
                "description": frozen_metadata["description"],
                "path": skill["path"],
                "content": content,
                "bundle": bundle,
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
            "projectId": project.as_ref().map(|p| p.id),
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
        let run = self.prepare_run(task_id, trigger).await?;
        if trigger == "schedule" {
            let _guard = self.task_author_lock.lock().await;
            self.require_task_author(&run["snapshot"]["task"]).await?;
            return self.queue_run(run, dedupe, None).await;
        }
        self.queue_run(run, dedupe, None).await
    }

    pub(crate) async fn enqueue_from_run(&self, task_id: &str, bearer: &str) -> Result<Value> {
        let run = self.prepare_run(task_id, "mcp").await?;
        self.queue_run(run, None, Some(bearer.to_owned())).await
    }

    async fn prepare_run(&self, task_id: &str, trigger: &str) -> Result<Value> {
        let run = self
            .snapshot(self.get("tasks", task_id).await?, trigger)
            .await?;
        crate::nodes::require_node(&run["snapshot"]["agent"])?;
        Ok(run)
    }

    /// Call under the author lock, after every potentially slow preparation step.
    async fn require_task_author(&self, task: &Value) -> Result<()> {
        let Some(policy) = self.synchronize_task_authors_locked().await? else {
            return Err(Error::unauthorized(
                "Installation is not claimed. Run cairn claim before scheduling work.",
            ));
        };
        let eligible = policy
            .grant(text(task, "authorId"))
            .is_some_and(|grant| task["authorAccessId"] == grant.access_id);

        if !eligible {
            return Err(Error::conflict("The task author's access ended."));
        }
        Ok(())
    }

    async fn queue_run(
        &self,
        run: Value,
        dedupe: Option<String>,
        bearer: Option<String>,
    ) -> Result<Value> {
        let mut run = run;
        let origin = if run["trigger"] == "schedule" {
            crate::native::policy::Origin::Stored(run["snapshot"]["task"].clone())
        } else {
            crate::native::policy::origin()
        };
        if crate::native::policy::native(&run["snapshot"]["agent"]) {
            crate::native::policy::refresh(self).await?;
        }
        let result = self
            .store
            .transaction(move |db| {
                required(
                    db.agent_snapshot(text(&run["snapshot"]["agent"], "id"))?,
                    "Agent no longer exists",
                )?;
                let grant = crate::native::policy::authorize_origin(
                    db,
                    &run["snapshot"]["agent"],
                    &origin,
                )?;
                crate::native::policy::apply(&mut run, &grant);
                if let Some(bearer) = bearer {
                    let caller = crate::installation_mcp::authorize_task_in(
                        db,
                        &bearer,
                        &run["snapshot"]["task"],
                        None,
                    )?;
                    run["requestedBy"] = requester(&caller);
                }
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
        let _guard = self.task_author_lock.lock().await;
        if !self.store.list("tasks").await?.iter().any(schedule_due) {
            return Ok(());
        }

        // No new scheduled work is admitted without the current beacon authority.
        // Already admitted work continues through a temporary beacon outage.
        let authority_wait_reason = match self.synchronize_task_authors_locked().await {
            Ok(Some(_)) => None,
            Ok(None) => Some(
                "Installation is not claimed. Run cairn claim before scheduling work.".to_owned(),
            ),
            Err(error) => Some(error.message),
        };

        if let Some(reason) = authority_wait_reason {
            for task in self
                .store
                .list("tasks")
                .await?
                .iter()
                .filter(|task| schedule_due(task))
            {
                self.schedule_wait(task, Some(&reason)).await?;
            }
            return Ok(());
        }

        for task in self.store.list("tasks").await? {
            if !schedule_due(&task) {
                continue;
            }

            let prepared = self.prepare_run(text(&task, "id"), "schedule").await;
            let admission = match prepared {
                Ok(run) => {
                    if let Err(error) = self.require_task_author(&run["snapshot"]["task"]).await {
                        if error.status != 409 {
                            self.schedule_wait(&task, Some(&error.message)).await?;
                        }
                        continue;
                    }

                    self.schedule_wait(&task, None).await?;
                    let dedupe = Some(format!("{}:{}", text(&task, "id"), task["nextRun"]));
                    self.queue_run(run, dedupe, None).await
                }
                Err(error) => {
                    // The authority recovered; this task's preparation is now the failure.
                    self.schedule_wait(&task, None).await?;
                    Err(error)
                }
            };

            // A skipped occurrence stays on the task until a later one is admitted or the task
            // is saved, so a mission never looks healthy while its schedule silently fails.
            let failure = match &admission {
                Ok(_) => Some(Value::Null),
                Err(error) if error.status != 409 => Some(json!({
                    "message": error.message,
                    "scheduledFor": task["nextRun"],
                    "at": now(),
                })),
                Err(_) => None,
            };

            if let Err(error) = &admission
                && error.status != 409
            {
                let detail = json!({
                    "taskId": task["id"],
                    "error": error.message,
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
                        if let Some(failure) = failure {
                            current["scheduleFailure"] = failure;
                        }
                        db.put("tasks", &current)?;
                    }
                    Ok(())
                })
                .await?;
        }
        Ok(())
    }

    async fn schedule_wait(&self, task: &Value, reason: Option<&str>) -> Result<()> {
        let task_id = text(task, "id").to_owned();
        let reason = reason.map_or(Value::Null, Into::into);
        self.store
            .transaction(move |db| {
                let Some(mut current) = db.get("tasks", &task_id)? else {
                    return Ok(());
                };
                if current["scheduleWaitReason"] == reason {
                    return Ok(());
                }

                current["scheduleWaitReason"] = reason.clone();
                db.put("tasks", &current)?;
                let action = if reason.is_null() {
                    "schedule.resumed"
                } else {
                    "schedule.deferred"
                };
                db.audit(
                    action,
                    &json!({
                        "taskId": task_id,
                        "reason": reason,
                    }),
                )
            })
            .await
    }
}

/// The conversation or mission whose agent started a run. A conversation's runs carry its id as
/// their task, so the requester is named without another lookup.
fn requester(caller: &Value) -> Value {
    if caller["trigger"] == "chat" {
        json!({
            "runId": caller["id"],
            "chatId": caller["taskId"],
        })
    } else {
        json!({
            "runId": caller["id"],
            "taskId": caller["taskId"],
        })
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

pub(crate) async fn git_origin(path: &Path) -> String {
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
pub(crate) fn ensure_removable(db: &Db<'_>, kind: &str, id: &str) -> Result<()> {
    if kind == "agents" && id == MAIN_AGENT_ID {
        return Err(Error::conflict("The main agent cannot be removed."));
    }
    let assigned = |agent: &Value| {
        let projects = &policy(agent)["projects"];
        !projects.is_null() && allowed(projects, id)
    };
    if kind == "projects" && db.agent_snapshots()?.iter().any(assigned) {
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

/// Drops an agent's portrait, plugin grants and node admission reviews.
pub(crate) fn forget_agent(db: &Db<'_>, id: &str) -> Result<()> {
    db.delete(&format!("agent-avatar:{id}"))?;
    crate::nodes::native_mode::forget_agent(db, id)?;
    crate::agent_grants::forget_agent(db, id)
}

/// Who edits an agent: the owner from the app, or an agent through the installation MCP.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum AgentEditor {
    Owner,
    Installation,
}

/// Marks a changed agent record so concurrent editors can detect the change.
pub(crate) fn touch(agent: &mut Value) {
    agent["revision"] = id().into();
}

fn validate_profile(agent: &cairn_contracts::Agent) -> Result<()> {
    crate::models::validate_speed(&agent.settings.speed)?;
    if agent.settings.provider != cairn_contracts::AgentProvider::Codex
        && !agent.settings.speed.is_empty()
    {
        return Err(Error::bad(
            "Speed applies only to Codex agents. Remove it or choose Codex.",
        ));
    }
    let access = &agent.settings.access;
    let restricted = access.projects.is_some()
        || access.skills.is_some()
        || access.mcps.is_some()
        || !access.mcp_tools.is_empty();
    if agent.id.to_string() == MAIN_AGENT_ID && restricted {
        return Err(Error::bad(
            "The main agent always has access to all resources. Create another agent for restricted access.",
        ));
    }
    Ok(())
}

pub fn policy(agent: &Value) -> Value {
    let mut value = json!({
        "projects": null,
        "skills": null,
        "mcps": null,
        "mcpTools": {},
        "installation": false,
        "delegation": false,
        "sandbox": "yolo",
        "nodes": [crate::nodes::LOCAL_NODE_ID],
        "backend": "firecracker",
    });
    merge(&mut value, &agent["access"]);
    value
}

/// Whether the `current` agent still allows everything `granted` allowed. Work started
/// under `granted` continues when access is unchanged or wider, but never with access
/// the owner has since removed. Node grants are excluded: placement follows the current ones.
pub fn covers(current: &Value, granted: &Value) -> bool {
    let current = policy(current);
    let granted = policy(granted);

    if current["backend"] != granted["backend"] {
        return false;
    }
    let scopes = ["projects", "skills", "mcps"]
        .iter()
        .all(|key| within(&granted[*key], &current[*key]));
    // A server without a tool selection allows all of its enabled tools.
    let tools = current["mcpTools"]
        .as_object()
        .into_iter()
        .flatten()
        .all(|(server, selection)| within(&granted["mcpTools"][server], selection));
    let installation = current["installation"] == true || granted["installation"] != true;
    let delegation = current["delegation"] == true || granted["delegation"] != true;
    let sandbox = sandbox_rank(&current["sandbox"]) >= sandbox_rank(&granted["sandbox"]);
    scopes && tools && installation && delegation && sandbox
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
        || a["sandbox"] != "yolo"
        || a["mcpTools"].as_object().is_some_and(|a| !a.is_empty())
}

pub fn task_projects(
    agent: &Value,
    task: &Value,
    projects: &[cairn_contracts::Project],
) -> Result<Vec<cairn_contracts::Project>> {
    let a = policy(agent);
    let available = projects
        .iter()
        .filter(|p| allowed(&a["projects"], &p.id.to_string()))
        .cloned()
        .collect::<Vec<_>>();
    if !task["projectId"].is_null() {
        return available
            .into_iter()
            .find(|p| Some(p.id.to_string().as_str()) == task["projectId"].as_str())
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

pub(crate) fn normalize_origin(remote: &str) -> String {
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

/// A form keeps this identity across an uncertain POST response. It is the normal
/// record UUID, so replay needs no new persistent format or auxiliary key. Updates
/// continue to use the URL identity and cannot change it through the request body.
pub(crate) fn creation_id(input: &Value, existing: Option<&str>) -> Result<Option<String>> {
    if existing.is_some() {
        return Ok(None);
    }
    input
        .get("creationId")
        .map(|value| {
            let value = value
                .as_str()
                .ok_or_else(|| Error::bad("creationId must be a UUID"))?;
            let uuid = uuid::Uuid::parse_str(value)
                .map_err(|_| Error::bad("creationId must be a UUID"))?;
            if uuid.get_version() != Some(uuid::Version::Random) {
                return Err(Error::bad("creationId must be a random UUID"));
            }
            Ok(uuid.to_string())
        })
        .transpose()
}

/// Record IDs share a namespace across kinds. A caller-selected identity must
/// never turn a collision into an upsert of a different resource's JSON.
pub(crate) fn replayed_creation(
    db: &Db<'_>,
    kind: &str,
    creation_id: Option<&str>,
) -> Result<Option<Value>> {
    let Some(id) = creation_id else {
        return Ok(None);
    };
    if db.project(id)?.is_some() || db.agent(id)?.is_some() {
        return Err(Error::conflict(
            "This creation identity belongs to a different resource.",
        ));
    }
    let rows = db.json_rows(
        "SELECT json_object('kind',kind,'record',json(data)) FROM records WHERE id=?",
        [id],
    )?;
    let Some(row) = rows.first() else {
        return Ok(None);
    };
    if row["kind"] != kind {
        return Err(Error::conflict(
            "This creation identity belongs to a different resource.",
        ));
    }
    Ok(Some(row["record"].clone()))
}

fn node_access_covers(current: &Value, edited: &Value) -> bool {
    let current = policy(current);
    let edited = policy(edited);
    current["nodes"].is_null()
        || edited["nodes"].as_array().is_some_and(|nodes| {
            nodes
                .iter()
                .all(|node| allowed(&current["nodes"], node.as_str().unwrap_or("")))
        })
}
