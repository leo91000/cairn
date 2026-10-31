mod capability;

pub(crate) use capability::{MISSION_OPERATIONS, MissionOperation, mission_operation};

// Missions and scheduled commitments own author observation and admission ordering.
mod storage;

use crate::{
    auth::{InstallationIdentity, InstallationRole},
    config::{id, now},
    error::{Error, Result, required},
    service::{Service, next_occurrences, task_projects},
    store::Db,
};
use cairn_contracts::*;
use cairn_protocol::TaskAuthorPolicy;
use serde_json::{Value, json};
pub(crate) use storage::SCHEMA;

#[derive(Default)]
pub struct Missions {
    authors: tokio::sync::Mutex<()>,
}

pub(crate) enum Actor {
    Human(InstallationIdentity),
    Public,
    Installation(String),
}

impl Actor {
    fn bearer(&self) -> Option<&str> {
        match self {
            Self::Installation(bearer) => Some(bearer),
            _ => None,
        }
    }

    fn authorize(&self, db: &Db<'_>) -> Result<()> {
        if let Some(bearer) = self.bearer() {
            crate::installation_mcp::authorize_in(db, bearer)?;
        }
        Ok(())
    }

    fn require_author(&self, m: &Mission) -> Result<()> {
        if let Self::Human(identity) = self
            && identity.role == InstallationRole::Member
            && m.author_id.as_deref() != Some(identity.account_id.as_str())
        {
            return Err(Error::forbidden(
                "Only the task author or installation owner can change this task. Duplicate it to make your own commitment.",
            ));
        }
        Ok(())
    }
}

pub(crate) enum SaveMission {
    Create(CreateMissionInput),
    Replace(ReplaceMissionInput),
    State(MissionStateInput),
}

impl Missions {
    pub(crate) async fn save(
        &self,
        s: &Service,
        command: SaveMission,
        actor: Actor,
    ) -> Result<Mission> {
        let _authors = self.authors.lock().await;
        let (mission_id, expected, mut settings, creation) = match command {
            SaveMission::Create(mut input) => {
                input.settings.validate().map_err(Error::bad)?;
                if input
                    .creation_id
                    .is_some_and(|id| id.0.get_version() != Some(uuid::Version::Random))
                {
                    return Err(Error::bad("creationId must be a random UUID"));
                }
                (
                    input
                        .creation_id
                        .unwrap_or_else(|| MissionId(uuid::Uuid::new_v4())),
                    None,
                    input.settings,
                    input.creation_id.is_some(),
                )
            }
            SaveMission::Replace(mut input) => {
                input.settings.validate().map_err(Error::bad)?;
                (input.task_id, Some(input.revision), input.settings, false)
            }
            SaveMission::State(input) => {
                let previous = required(
                    s.store.mission(&input.task_id.to_string()).await?,
                    "Mission not found",
                )?;
                let mut settings = previous.settings;
                settings.enabled = input.enabled;
                settings.archived = input.archived;
                (input.task_id, Some(input.revision), settings, false)
            }
        };
        let previous = s.store.mission(&mission_id.to_string()).await?;
        if expected.is_none()
            && creation
            && let Some(previous) = &previous
        {
            actor.require_author(previous)?;
            self.observe(s).await?;
            return s
                .store
                .read(move |db| {
                    actor.authorize(db)?;
                    let current =
                        required(db.mission(&mission_id.to_string())?, "Mission not found")?;
                    actor.require_author(&current)?;
                    Ok(current)
                })
                .await;
        }
        if let Some(expected) = &expected {
            let previous = required(previous.as_ref(), "Mission not found")?;
            if previous.revision != *expected {
                return Err(stale());
            }
        }
        let reduces = previous.as_ref().is_some_and(|previous| {
            let mut unchanged = settings.clone();
            unchanged.enabled = previous.settings.enabled;
            unchanged.archived = previous.settings.archived;
            !settings.enabled
                && (!previous.settings.archived || settings.archived)
                && unchanged == previous.settings
        });
        if !reduces && let Some(previous) = &previous {
            actor.require_author(previous)?;
        }
        let policy = if reduces {
            None
        } else {
            self.observe(s).await?
        };
        let author = policy
            .as_ref()
            .map(|policy| {
                let account = match &actor {
                    Actor::Human(identity) => &identity.account_id,
                    _ => &policy.owner.account_id,
                };
                policy.grant(account).cloned().ok_or_else(|| {
                    Error::forbidden("This account no longer has access to the installation.")
                })
            })
            .transpose()?;
        settings.validate().map_err(Error::bad)?;
        if let Some(cron) = &settings.cron {
            next_occurrences(cron, &settings.timezone, now(), 1)?;
        }
        let agent = s.agent_snapshot(&settings.agent_id.to_string()).await?;
        let origin = crate::native::policy::origin();
        if !reduces {
            crate::native::policy::refresh_for(s, &agent).await?;
        }
        let mut mission = Mission {
            id: mission_id,
            settings,
            revision: id(),
            created_at: now(),
            next_run: None,
            author_id: author.as_ref().map(|a| a.account_id.clone()),
            author_access_id: author.map(|a| a.access_id),
            author_removed: false,
            schedule_wait_reason: None,
            schedule_failure: None,
            native_principals: Vec::new(),
            native_authority_nodes: Vec::new(),
            native_parent_run_id: None,
            native_parent_access: None,
            native_provenance_unknown: true,
            native_plugin_grants: None,
        };
        s.store
            .transaction(move |db| {
                actor.authorize(db)?;
                let current = db.mission(&mission_id.to_string())?;
                if let Some(expected) = expected {
                    let current = required(current.as_ref(), "Mission no longer exists")?;
                    if current.revision != expected {
                        return Err(stale());
                    }
                    if !reduces {
                        actor.require_author(current)?;
                    }
                    mission.author_id = current.author_id.clone();
                    mission.author_access_id = current.author_access_id.clone();
                    mission.author_removed = current.author_removed;
                    mission.created_at = current.created_at;
                } else {
                    if let Some(current) = current.as_ref() {
                        if creation {
                            actor.require_author(current)?;
                            return Ok(current.clone());
                        }
                        return Err(Error::conflict("Mission already exists"));
                    }
                    if db.agent(&mission_id.to_string())?.is_some()
                        || db.project(&mission_id.to_string())?.is_some()
                        || db.0.query_row(
                            "SELECT EXISTS(SELECT 1 FROM records WHERE id=?)",
                            [mission_id.to_string()],
                            |r| r.get::<_, bool>(0),
                        )?
                    {
                        return Err(Error::conflict(
                            "This creation identity belongs to a different resource.",
                        ));
                    }
                }
                let agent = required(
                    db.agent_snapshot(&mission.settings.agent_id.to_string())?,
                    "Agent no longer exists",
                )?;
                // Native provenance is still owned by the native-node domain, not by command JSON.
                let wire = serde_json::to_value(&mission)?;
                task_projects(&agent, &wire, &db.projects()?)?;
                let previous_wire = current.as_ref().map(serde_json::to_value).transpose()?;
                let grant = if reduces {
                    previous_wire.clone().unwrap()
                } else {
                    crate::native::policy::task_grant(db, &wire, previous_wire.as_ref(), &origin)?
                };
                apply_native(&mut mission, grant)?;
                if mission.settings.scheduled() {
                    if mission.author_removed {
                        return Err(Error::conflict(
                            "This author's access ended. Duplicate the task to schedule it again.",
                        ));
                    }
                    mission.next_run = Some(
                        next_occurrences(
                            mission.settings.cron.as_deref().unwrap(),
                            &mission.settings.timezone,
                            now(),
                            1,
                        )?[0],
                    );
                }
                db.save_mission(&mission)?;
                db.audit("task.saved", &json!({"id":mission.id}))?;
                Ok(mission)
            })
            .await
    }

    pub(crate) async fn delete(
        &self,
        s: &Service,
        input: MissionInput,
        actor: Actor,
    ) -> Result<()> {
        if let Actor::Human(identity) = &actor
            && identity.role == InstallationRole::Member
        {
            return Err(Error::forbidden(
                "Only the installation owner can delete missions.",
            ));
        }
        let _authors = self.authors.lock().await;
        s.store
            .transaction(move |db| {
                actor.authorize(db)?;
                let current =
                    required(db.mission(&input.task_id.to_string())?, "Mission not found")?;
                actor.require_author(&current)?;
                db.delete_mission(&input.task_id.to_string())
            })
            .await
    }

    async fn observe(&self, s: &Service) -> Result<Option<TaskAuthorPolicy>> {
        let policy = crate::relay::task_author_policy(s).await?;
        if let Some(policy) = &policy {
            let grants: std::collections::HashMap<_, _> = policy
                .members
                .iter()
                .chain(std::iter::once(&policy.owner))
                .map(|g| (g.account_id.clone(), g.access_id.clone()))
                .collect();
            s.store
                .transaction(move |db| {
                    for mut mission in db.missions()? {
                        let eligible = mission
                            .author_id
                            .as_ref()
                            .and_then(|id| grants.get(id))
                            .is_some_and(|access| {
                                Some(access) == mission.author_access_id.as_ref()
                            });
                        if !eligible && !mission.author_removed {
                            mission.author_removed = true;
                            mission.schedule_wait_reason = None;
                            if mission.settings.cron.is_some() {
                                mission.settings.enabled = false;
                                mission.next_run = None;
                            }
                            mission.revision = id();
                            db.save_mission(&mission)?;
                            db.audit("task.author_reconciled", &json!({"taskId":mission.id}))?;
                        }
                    }
                    Ok(())
                })
                .await?;
        }
        Ok(policy)
    }

    async fn require_author(&self, s: &Service, mission: &Mission) -> Result<()> {
        let policy = self.observe(s).await?.ok_or_else(|| {
            Error::unauthorized(
                "Installation is not claimed. Run cairn claim before scheduling work.",
            )
        })?;
        let eligible = mission
            .author_id
            .as_deref()
            .and_then(|id| policy.grant(id))
            .is_some_and(|grant| Some(&grant.access_id) == mission.author_access_id.as_ref());
        if !eligible || mission.author_removed {
            return Err(Error::conflict("The task author's access ended."));
        }
        Ok(())
    }

    pub(crate) async fn enqueue_scheduled(
        &self,
        s: &Service,
        run: Value,
        dedupe: Option<String>,
    ) -> Result<Value> {
        let _authors = self.authors.lock().await;
        let mission: Mission = serde_json::from_value(run["snapshot"]["task"].clone())?;
        self.require_author(s, &mission).await?;
        s.queue_run(run, dedupe, None).await
    }

    pub(crate) async fn merge_agents_live(
        &self,
        s: &Service,
        input: MergeAgentsInput,
        authorize: crate::authorization::Authorization,
        resource_authority: Option<crate::capabilities::plugin_packages::Authority>,
    ) -> Result<MergeAgentsOutput> {
        authorize.recheck().await?;
        let _authors = self.authors.lock().await;
        if !input.confirm {
            return Err(Error::bad("Confirm the merge."));
        }

        // An active worker can still be settling a terminal run. Keep the same
        // worker/admission ordering as the profile merge while commitments are held.
        let active = s.worker.active.lock().await;
        authorize.recheck().await?;
        let coverage = crate::plugins::grants::check_coverage_authorized(
            s,
            &input.target_id.to_string(),
            &input.source_id.to_string(),
            resource_authority.clone(),
        )
        .await
        .map_err(|error| {
            if error.status == 403 {
                Error::conflict(error.message)
            } else {
                error
            }
        })?;
        authorize.recheck().await?;
        let executing = active.keys().cloned().collect::<Vec<_>>();
        let result = s
            .store
            .transaction(move |db| {
                authorize.authorize(db)?;
                coverage.authorize(db)?;
                if let Some(authority) = &resource_authority {
                    authority.authorize(db)?;
                }
                crate::agent_merge::merge(db, &input, &executing)
            })
            .await?;
        s.worker.notify();
        Ok(result)
    }
}

fn stale() -> Error {
    Error::conflict("This mission was changed elsewhere. Reload it before saving.")
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct NativeGrant {
    native_principals: Vec<String>,
    native_authority_nodes: Vec<String>,
    native_parent_run_id: Option<String>,
    native_parent_access: Option<String>,
    native_provenance_unknown: bool,
    native_plugin_grants: Option<Vec<MissionPluginGrant>>,
}

fn apply_native(m: &mut Mission, value: Value) -> Result<()> {
    let grant: NativeGrant = serde_json::from_value(value)?;
    m.native_principals = grant.native_principals;
    m.native_authority_nodes = grant.native_authority_nodes;
    m.native_parent_run_id = grant.native_parent_run_id;
    m.native_parent_access = grant.native_parent_access;
    m.native_provenance_unknown = grant.native_provenance_unknown;
    m.native_plugin_grants = grant.native_plugin_grants;
    Ok(())
}

impl Service {
    pub async fn synchronize_task_authors(&self) -> Result<()> {
        let _authors = self.missions.authors.lock().await;
        self.missions.observe(self).await?;
        Ok(())
    }

    pub async fn schedule(&self) -> Result<()> {
        let _authors = self.missions.authors.lock().await;
        if self.store.due_missions(now()).await?.is_empty() {
            return Ok(());
        }
        let wait = match self.missions.observe(self).await {
            Ok(Some(_)) => None,
            Ok(None) => Some(
                "Installation is not claimed. Run cairn claim before scheduling work.".to_owned(),
            ),
            Err(error) => Some(error.message),
        };
        if let Some(reason) = wait {
            for mission in self.store.due_missions(now()).await? {
                self.schedule_wait(&mission, Some(reason.clone())).await?;
            }
            return Ok(());
        }
        for mission in self.store.due_missions(now()).await? {
            let prepared = self.prepare_run(&mission.id.to_string(), "schedule").await;
            let admission = match prepared {
                Ok(run) => {
                    if let Err(error) = self.missions.require_author(self, &mission).await {
                        if error.status != 409 {
                            self.schedule_wait(&mission, Some(error.message)).await?;
                        }
                        continue;
                    }
                    self.schedule_wait(&mission, None).await?;
                    let dedupe = Some(format!("{}:{}", mission.id, mission.next_run.unwrap()));
                    self.queue_run(run, dedupe, None).await
                }
                Err(error) => {
                    self.schedule_wait(&mission, None).await?;
                    Err(error)
                }
            };
            let failure = match &admission {
                Ok(_) => Some(None),
                Err(error) if error.status != 409 => Some(Some(ScheduleFailure {
                    message: error.message.clone(),
                    scheduled_for: mission.next_run.unwrap(),
                    at: now(),
                })),
                Err(_) => None,
            };
            if let Err(error) = &admission
                && error.status != 409
            {
                self.store
                    .audit(
                        "schedule.failed",
                        json!({"taskId":mission.id,"error":error.message}),
                    )
                    .await?;
            }
            let next = next_occurrences(
                mission.settings.cron.as_deref().unwrap(),
                &mission.settings.timezone,
                now(),
                1,
            )?[0];
            self.store
                .transaction(move |db| {
                    if let Some(mut current) = db.mission(&mission.id.to_string())?
                        && current.next_run == mission.next_run
                    {
                        current.next_run = Some(next);
                        if let Some(failure) = failure {
                            current.schedule_failure = failure;
                        }
                        db.save_mission(&current)?;
                    }
                    Ok(())
                })
                .await?;
        }
        Ok(())
    }

    async fn schedule_wait(&self, mission: &Mission, reason: Option<String>) -> Result<()> {
        let id = mission.id;
        self.store
            .transaction(move |db| {
                if let Some(mut current) = db.mission(&id.to_string())?
                    && current.schedule_wait_reason != reason
                {
                    current.schedule_wait_reason = reason.clone();
                    db.save_mission(&current)?;
                    db.audit(
                        if reason.is_none() {
                            "schedule.resumed"
                        } else {
                            "schedule.deferred"
                        },
                        &json!({"taskId":id,"reason":reason}),
                    )?;
                }
                Ok(())
            })
            .await
    }
}
