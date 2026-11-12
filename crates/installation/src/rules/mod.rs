//! Hook rules (ADR-0075, « Règles de hook » and « Autorité des règles »): their model,
//! storage, authority and catalog. Running them belongs to the rule engine.
//!
//! A rule acts with its creator's authority. Only its creator edits, enables or
//! disables it; every member sees it and may duplicate it with the rights it uses; its
//! creator or the owner deletes it; only the owner pauses it in an emergency. A rule
//! belongs to a membership and stops for good when that membership ends.
mod capability;
mod catalog;
pub(crate) mod formula;
pub(crate) mod storage;
pub(crate) mod templates;
pub(crate) mod validation;

pub(crate) use capability::{RULE_OPERATIONS, RuleOperation, rule_operation};
pub(crate) use storage::SCHEMA;

use crate::{
    auth::{InstallationIdentity, InstallationRole},
    config::{id, now},
    error::{Error, Result, required},
    native::policy::Origin,
    service::Service,
    store::Db,
};
use cairn_contracts::*;
use cairn_protocol::TaskAuthorPolicy;
use serde_json::json;
use std::collections::BTreeSet;
use storage::{RuleRow, VersionRow};
use validation::{Creator, Instances};

/// Who asks. Constructed by authenticated transport adapters, never from command JSON.
pub(crate) enum Actor {
    /// A person in the web or Android app.
    Person(InstallationIdentity),
    /// The public MCP, for the owner who authorized the client, with its scopes.
    Public(Vec<String>, Option<String>),
    /// An agent with installation access (`cairn_installation`).
    Installation(String, crate::authorization::Authorization),
}

impl Actor {
    pub(crate) fn from_identity(identity: &InstallationIdentity) -> Self {
        match &identity.mcp_scopes {
            Some(scopes) => Self::Public(
                scopes.clone(),
                (identity.role == InstallationRole::Owner).then(|| identity.account_id.clone()),
            ),
            None => Self::Person(identity.clone()),
        }
    }

    pub(crate) fn require_scope(&self, scope: &str) -> Result<()> {
        if let Self::Public(scopes, _) = self
            && !scopes.iter().any(|held| held == scope)
        {
            return Err(Error::forbidden(format!("The {scope} scope is required.")));
        }
        Ok(())
    }

    async fn recheck(&self) -> Result<()> {
        if let Self::Installation(_, authorize) = self {
            authorize.recheck().await?;
        }
        Ok(())
    }

    fn authorize(&self, db: &Db<'_>) -> Result<()> {
        if let Self::Installation(bearer, authorize) = self {
            authorize.authorize(db)?;
            crate::installation_mcp::authorize_in(db, bearer)?;
        }
        Ok(())
    }

    /// The person reading: an agent with installation access reads with the owner's
    /// management authority but never acts as a person.
    fn viewer(&self) -> Viewer {
        match self {
            Self::Person(identity) => Viewer {
                account: Some(identity.account_id.clone()),
                owner: identity.role == InstallationRole::Owner,
            },
            Self::Public(_, account) => Viewer {
                account: account.clone().or_else(crate::native::policy::caller),
                owner: true,
            },
            Self::Installation(..) => Viewer {
                account: None,
                owner: false,
            },
        }
    }
}

/// What the person viewing a rule may see and do.
#[derive(Clone, Debug)]
pub(crate) struct Viewer {
    pub account: Option<String>,
    pub owner: bool,
}

/// The account a write acts for, with its membership and native provenance.
struct Writer {
    creator: Creator,
    access_id: Option<String>,
    source: HookRuleSource,
    conversation: Option<String>,
    origin: Origin,
}

/// Rules are written for a person only in a conversation that meets every provenance
/// condition of ADR-0075. Cairn does not yet record who writes each entry of a
/// conversation, so no conversation qualifies and agents only read rules.
fn installation_writer(_db: &Db<'_>, _bearer: &str) -> Result<Writer> {
    Err(Error::forbidden(
        "An agent writes rules only for the person of a conversation that this person alone has written, from its start. This conversation does not qualify.",
    ))
}

/// Confirms memberships, then resolves who a write acts for. Callers hold `authors`.
async fn writer(s: &Service, actor: &Actor) -> Result<(Writer, Option<TaskAuthorPolicy>)> {
    let policy = s.missions.observe(s).await?;
    let (account, owner, source, origin) = match actor {
        Actor::Person(identity) => (
            identity.account_id.clone(),
            identity.role == InstallationRole::Owner,
            HookRuleSource::App,
            Origin::Human(identity.account_id.clone()),
        ),
        Actor::Public(_, account) => {
            // A rule created through the public MCP belongs to the owner who
            // authorized the client. Scopes alone grant no native authority.
            let caller = account.clone().or_else(crate::native::policy::caller);
            let account = policy
                .as_ref()
                .map(|policy| policy.owner.account_id.clone())
                .or_else(|| caller.clone())
                .ok_or_else(|| Error::unauthorized("The installation is not claimed."))?;
            let origin = caller
                .filter(|caller| *caller == account)
                .map_or(Origin::Unknown, Origin::Human);
            (account, true, HookRuleSource::PublicMcp, origin)
        }
        Actor::Installation(..) => {
            return Err(Error::forbidden(
                "An agent writes rules only through the installation tools of an eligible conversation.",
            ));
        }
    };
    let access_id = match &policy {
        Some(policy) => Some(
            policy
                .grant(&account)
                .ok_or_else(|| {
                    Error::forbidden("This account no longer has access to the installation.")
                })?
                .access_id
                .clone(),
        ),
        None => None,
    };
    Ok((
        Writer {
            creator: Creator { account, owner },
            access_id,
            source,
            conversation: None,
            origin,
        },
        policy,
    ))
}

/// The instances a definition names, with their active manifests, read before the
/// write transaction. The transaction checks the references again.
async fn instances(s: &Service, definition: &HookRuleDefinition) -> Result<Instances> {
    let mut instances = Instances::default();
    for id in validation::instance_ids(definition) {
        let Some(connection) = s.store.plugin_instance(&id).await? else {
            continue;
        };
        let manifest = crate::plugins::registry::active(s, &connection.plugin_id)
            .await
            .ok()
            .map(|package| package.manifest);
        instances.0.insert(id, (connection, manifest));
    }
    Ok(instances)
}

/// Started agents restricted to native nodes need a fresh native authority observation.
async fn refresh_native(s: &Service, definition: &HookRuleDefinition) -> Result<()> {
    let mut agents = BTreeSet::new();
    validation::visit_actions(&definition.steps, &mut |action| {
        if let RuleAction::StartAgent { agent_id, .. } = action {
            agents.insert(agent_id.to_string());
        }
    });
    for agent in agents {
        if let Some(snapshot) = s.store.agent_snapshot(&agent).await? {
            crate::native::policy::refresh_for(s, &snapshot).await?;
        }
    }
    Ok(())
}

fn refuse(problems: &[HookRuleProblem]) -> Error {
    let first = &problems[0];
    let more = match problems.len() {
        1 => String::new(),
        2 => " (and 1 other problem)".into(),
        count => format!(" (and {} other problems)", count - 1),
    };
    Error::bad(format!("{}{more} [{}]", first.message, first.path))
}

/// Validates and stores a new version, returning the rule row it belongs to.
fn save_version(
    db: &Db<'_>,
    row: &mut RuleRow,
    definition: HookRuleDefinition,
    writer: &Writer,
    instances: &Instances,
) -> Result<()> {
    let native_grant = crate::native::policy::provenance(db, &writer.origin)?;
    let checked = validation::check(
        db,
        &validation::Request {
            definition: &definition,
            creator: &writer.creator,
            instances,
            origin: &writer.origin,
        },
    )?;
    if !checked.problems.is_empty() {
        return Err(refuse(&checked.problems));
    }
    let at = now();
    row.version += 1;
    row.revision = id();
    row.updated_at = at;
    db.save_hook_rule(row)?;
    db.insert_hook_rule_version(
        &VersionRow {
            rule_id: row.id.clone(),
            version: row.version,
            definition,
            saved_by: writer.creator.account.clone(),
            source: writer.source,
            conversation_id: writer.conversation.clone(),
            native_grant,
            saved_at: at,
        },
        &checked.uses,
    )?;
    Ok(())
}

fn current(db: &Db<'_>, id: &str) -> Result<(RuleRow, VersionRow)> {
    let row = required(db.hook_rule(id)?, "Rule not found")?;
    let version = required(
        db.hook_rule_version(id, row.version)?,
        "Rule version not found",
    )?;
    Ok((row, version))
}

/// The broad rights of an agent that strangers could reach through a rule.
pub(crate) fn exposure(db: &Db<'_>, agent: &Agent) -> Result<Vec<&'static str>> {
    let access = &agent.settings.access;
    let mut rights = Vec::new();
    if !db
        .agent_plugin_grants(&agent.id.to_string())?
        .grants
        .is_empty()
    {
        rights.push("plugin rights");
    }
    if access.mcps.as_ref().is_none_or(|mcps| !mcps.is_empty()) {
        rights.push("MCP connections");
    }
    if access
        .projects
        .as_ref()
        .is_none_or(|projects| projects.len() > 1)
    {
        rights.push("several projects");
    }
    if access.sandbox != "read-only" {
        rights.push("file writes and commands");
    }
    if access.delegation {
        rights.push("delegation");
    }
    if access.backend != "firecracker" {
        rights.push("native execution");
    }
    if access.installation {
        rights.push("installation access");
    }
    Ok(rights)
}

fn agents_used(definition: &HookRuleDefinition) -> Vec<String> {
    let mut agents = vec![definition.execution_agent_id.to_string()];
    validation::visit_actions(&definition.steps, &mut |action| {
        if let RuleAction::StartAgent { agent_id, .. } = action
            && !agents.contains(&agent_id.to_string())
        {
            agents.push(agent_id.to_string());
        }
    });
    agents
}

fn creator(row: &RuleRow) -> Creator {
    Creator {
        account: row.creator_account_id.clone(),
        owner: row.creator_owner,
    }
}

/// Why the rule lost its rights, if it did. This lifts by itself when the right returns.
pub(crate) fn rights_lost(
    db: &Db<'_>,
    row: &RuleRow,
    definition: &HookRuleDefinition,
) -> Result<Option<String>> {
    let creator = creator(row);
    for agent in agents_used(definition) {
        let Some(profile) = db.agent(&agent)? else {
            return Ok(Some(
                "An agent this rule uses was deleted or merged. Its creator can choose another one."
                    .into(),
            ));
        };
        if !creator.uses_agent(db, &agent)? {
            return Ok(Some(format!(
                "Its creator no longer has access to {}.",
                profile.settings.name
            )));
        }
    }
    if let HookRuleTrigger::Plugin { instance_id, hook } = &definition.trigger
        && db
            .plugin_contribution(
                &definition.execution_agent_id.to_string(),
                &instance_id.to_string(),
                PluginContributionKind::Hook,
                hook,
            )?
            .is_none()
    {
        return Ok(Some(format!(
            "The execution agent no longer holds the hook {hook}."
        )));
    }
    Ok(None)
}

fn blocks(
    db: &Db<'_>,
    row: &RuleRow,
    definition: &HookRuleDefinition,
) -> Result<Vec<HookRuleBlock>> {
    let mut blocks = Vec::new();
    if let Some(at) = row.disabled_at {
        blocks.push(HookRuleBlock {
            kind: HookRuleBlockKind::Disabled,
            since: Some(at),
            reason: "Disabled. Its creator can enable it.".into(),
        });
    }
    if let Some(at) = row.emergency_paused_at {
        blocks.push(HookRuleBlock {
            kind: HookRuleBlockKind::EmergencyPaused,
            since: Some(at),
            reason: "Paused by the installation owner, who alone can resume it.".into(),
        });
    }
    if let Some(at) = row.budget_exhausted_at {
        blocks.push(HookRuleBlock {
            kind: HookRuleBlockKind::BudgetExhausted,
            since: Some(at),
            reason: row.budget_reason.clone().unwrap_or_else(|| {
                "An hourly cap was exceeded. Its creator or the owner can resume it.".into()
            }),
        });
    }
    if let Some(reason) = rights_lost(db, row, definition)? {
        blocks.push(HookRuleBlock {
            kind: HookRuleBlockKind::RightsLost,
            since: None,
            reason,
        });
    }
    if let Some(at) = row.membership_ended_at {
        blocks.push(HookRuleBlock {
            kind: HookRuleBlockKind::MembershipEnded,
            since: Some(at),
            reason: "Its creator left the installation. Duplicate it to use it again.".into(),
        });
    }
    Ok(blocks)
}

fn allowed(
    db: &Db<'_>,
    row: &RuleRow,
    definition: &HookRuleDefinition,
    viewer: &Viewer,
) -> Result<HookRuleAllowed> {
    let Some(account) = &viewer.account else {
        // Agents read with management authority and act through their own tools.
        return Ok(HookRuleAllowed {
            view_details: true,
            ..HookRuleAllowed::default()
        });
    };
    let mine = *account == row.creator_account_id;
    let ended = row.membership_ended_at.is_some();
    let person = Creator {
        account: account.clone(),
        owner: viewer.owner,
    };
    let mut duplicate = true;
    for agent in agents_used(definition) {
        duplicate &= person.uses_agent(db, &agent)?;
    }
    let execution = definition.execution_agent_id.to_string();
    Ok(HookRuleAllowed {
        edit: mine && !ended,
        enable: mine && !ended && row.disabled_at.is_some(),
        disable: mine && row.disabled_at.is_none(),
        duplicate,
        delete: mine || viewer.owner,
        emergency_pause: viewer.owner,
        lift_budget: (mine || viewer.owner) && row.budget_exhausted_at.is_some(),
        view_details: mine || viewer.owner || person.uses_agent(db, &execution)?,
    })
}

fn uuid(value: &str) -> Result<uuid::Uuid> {
    uuid::Uuid::parse_str(value).map_err(|_| Error::bad("Invalid saved rule identifier."))
}

pub(crate) fn view(db: &Db<'_>, id: &str, viewer: &Viewer) -> Result<HookRule> {
    let (row, version) = current(db, id)?;
    let definition = version.definition.clone();
    Ok(HookRule {
        id: HookRuleId(uuid(&row.id)?),
        creator_account_id: row.creator_account_id.clone(),
        revision: row.revision.clone(),
        version: row.version,
        blocks: blocks(db, &row, &definition)?,
        allowed: allowed(db, &row, &definition, viewer)?,
        warnings: validation::warnings(db, &definition)?,
        native_authority: version.native_authority(),
        source: version.source,
        conversation_id: version
            .conversation_id
            .as_deref()
            .and_then(|id| uuid::Uuid::parse_str(id).ok()),
        created_at: row.created_at,
        updated_at: row.updated_at,
        definition,
    })
}

fn summary(db: &Db<'_>, row: &RuleRow, viewer: &Viewer) -> Result<HookRuleSummary> {
    let version = required(
        db.hook_rule_version(&row.id, row.version)?,
        "Rule version not found",
    )?;
    let definition = &version.definition;
    let mut started_agent_ids = Vec::new();
    let mut tool_instance_ids = Vec::new();
    validation::visit_actions(&definition.steps, &mut |action| match action {
        RuleAction::StartAgent { agent_id, .. } if !started_agent_ids.contains(agent_id) => {
            started_agent_ids.push(*agent_id);
        }
        RuleAction::CallTool { instance_id, .. } if !tool_instance_ids.contains(instance_id) => {
            tool_instance_ids.push(*instance_id);
        }
        _ => {}
    });
    Ok(HookRuleSummary {
        id: HookRuleId(uuid(&row.id)?),
        name: definition.name.clone(),
        description: definition.description.clone(),
        creator_account_id: row.creator_account_id.clone(),
        revision: row.revision.clone(),
        version: row.version,
        trigger: definition.trigger.clone(),
        execution_agent_id: definition.execution_agent_id,
        started_agent_ids,
        tool_instance_ids,
        blocks: blocks(db, row, definition)?,
        allowed: allowed(db, row, definition, viewer)?,
        updated_at: row.updated_at,
    })
}

fn new_row(id: String, writer: &Writer, disabled: bool) -> RuleRow {
    let at = now();
    RuleRow {
        id,
        creator_account_id: writer.creator.account.clone(),
        creator_access_id: writer.access_id.clone(),
        creator_owner: writer.creator.owner,
        revision: String::new(),
        version: 0,
        disabled_at: disabled.then_some(at),
        emergency_paused_at: None,
        budget_exhausted_at: None,
        budget_reason: None,
        membership_ended_at: None,
        created_at: at,
        updated_at: at,
    }
}

fn creation_id(requested: Option<HookRuleId>) -> Result<String> {
    match requested {
        Some(id) if id.0.get_version() != Some(uuid::Version::Random) => {
            Err(Error::bad("creationId must be a random UUID"))
        }
        Some(id) => Ok(id.to_string()),
        None => Ok(uuid::Uuid::new_v4().to_string()),
    }
}

/// The same identity must not already name another kind of record.
fn fresh_identity(db: &Db<'_>, id: &str) -> Result<()> {
    if db.agent(id)?.is_some() || db.project(id)?.is_some() || db.mission(id)?.is_some() {
        return Err(Error::conflict(
            "This creation identity belongs to a different resource.",
        ));
    }
    Ok(())
}

pub(crate) async fn create(
    s: &Service,
    input: CreateHookRuleInput,
    actor: Actor,
) -> Result<HookRule> {
    actor.require_scope("manage")?;
    actor.recheck().await?;
    let rule = creation_id(input.creation_id)?;
    let instances = instances(s, &input.definition).await?;
    refresh_native(s, &input.definition).await?;
    let _authors = s.missions.authors.lock().await;
    actor.recheck().await?;
    let (writer, _) = match &actor {
        Actor::Installation(bearer, _) => {
            let bearer = bearer.clone();
            (
                s.store
                    .read(move |db| installation_writer(db, &bearer))
                    .await?,
                None,
            )
        }
        _ => writer(s, &actor).await?,
    };
    let viewer = Viewer {
        account: Some(writer.creator.account.clone()),
        owner: writer.creator.owner,
    };
    s.store
        .transaction(move |db| {
            actor.authorize(db)?;
            if let Actor::Installation(bearer, _) = &actor {
                installation_writer(db, bearer)?;
            }
            if let Some(existing) = db.hook_rule(&rule)? {
                if existing.creator_account_id != writer.creator.account {
                    return Err(Error::conflict(
                        "This creation identity belongs to another rule.",
                    ));
                }
                return view(db, &rule, &viewer);
            }
            fresh_identity(db, &rule)?;
            let mut row = new_row(rule.clone(), &writer, !input.enabled);
            save_version(db, &mut row, input.definition, &writer, &instances)?;
            db.audit(
                "hook_rule.created",
                &json!({ "ruleId": row.id, "creator": row.creator_account_id }),
            )?;
            view(db, &rule, &viewer)
        })
        .await
}

pub(crate) async fn replace(
    s: &Service,
    input: ReplaceHookRuleInput,
    actor: Actor,
) -> Result<HookRule> {
    actor.require_scope("manage")?;
    actor.recheck().await?;
    let instances = instances(s, &input.definition).await?;
    refresh_native(s, &input.definition).await?;
    let _authors = s.missions.authors.lock().await;
    actor.recheck().await?;
    let (writer, _) = writer(s, &actor).await?;
    let viewer = Viewer {
        account: Some(writer.creator.account.clone()),
        owner: writer.creator.owner,
    };
    let rule = input.rule_id.to_string();
    s.store
        .transaction(move |db| {
            actor.authorize(db)?;
            let mut row = required(db.hook_rule(&rule)?, "Rule not found")?;
            if row.creator_account_id != writer.creator.account {
                return Err(Error::forbidden(
                    "Only its creator can change this rule. Duplicate it to make your own.",
                ));
            }
            if row.membership_ended_at.is_some() {
                return Err(Error::conflict(
                    "This rule's membership ended. Duplicate it to use it again.",
                ));
            }
            if row.revision != input.revision {
                return Err(Error::conflict(
                    "This rule changed. Reload it before saving.",
                ));
            }
            save_version(db, &mut row, input.definition, &writer, &instances)?;
            db.audit(
                "hook_rule.replaced",
                &json!({ "ruleId": row.id, "version": row.version }),
            )?;
            view(db, &rule, &viewer)
        })
        .await
}

/// State changes need no fresh membership: they only reduce what the rule does, or
/// lift a block its own holder may lift.
async fn change_state<F>(s: &Service, rule: HookRuleId, actor: Actor, change: F) -> Result<HookRule>
where
    F: FnOnce(&Db<'_>, &mut RuleRow, &Viewer) -> Result<&'static str> + Send + 'static,
{
    actor.require_scope("manage")?;
    actor.recheck().await?;
    if matches!(actor, Actor::Installation(..)) {
        return Err(Error::forbidden(
            "An agent changes rules only for the person of an eligible conversation.",
        ));
    }
    let viewer = actor.viewer();
    let rule = rule.to_string();
    s.store
        .transaction(move |db| {
            actor.authorize(db)?;
            let mut row = required(db.hook_rule(&rule)?, "Rule not found")?;
            let action = change(db, &mut row, &viewer)?;
            row.revision = id();
            row.updated_at = now();
            db.save_hook_rule(&row)?;
            db.audit(action, &json!({ "ruleId": row.id }))?;
            view(db, &rule, &viewer)
        })
        .await
}

fn mine(row: &RuleRow, viewer: &Viewer) -> bool {
    viewer.account.as_deref() == Some(row.creator_account_id.as_str())
}

pub(crate) async fn set_enabled(
    s: &Service,
    input: SetHookRuleEnabledInput,
    actor: Actor,
) -> Result<HookRule> {
    change_state(s, input.rule_id, actor, move |_, row, viewer| {
        if !mine(row, viewer) {
            return Err(Error::forbidden(
                "Only its creator can enable or disable this rule. The owner can pause it in an emergency.",
            ));
        }
        if input.enabled {
            if row.membership_ended_at.is_some() {
                return Err(Error::conflict(
                    "This rule's membership ended. Duplicate it to use it again.",
                ));
            }
            row.disabled_at = None;
            Ok("hook_rule.enabled")
        } else {
            row.disabled_at.get_or_insert_with(now);
            Ok("hook_rule.disabled")
        }
    })
    .await
}

pub(crate) async fn set_emergency_pause(
    s: &Service,
    input: SetHookRuleEmergencyPauseInput,
    actor: Actor,
) -> Result<HookRule> {
    change_state(s, input.rule_id, actor, move |_, row, viewer| {
        if !viewer.owner {
            return Err(Error::forbidden(
                "Only the installation owner can pause a rule in an emergency.",
            ));
        }
        if input.paused {
            row.emergency_paused_at.get_or_insert_with(now);
            Ok("hook_rule.emergency_paused")
        } else {
            row.emergency_paused_at = None;
            Ok("hook_rule.emergency_resumed")
        }
    })
    .await
}

pub(crate) async fn lift_budget(
    s: &Service,
    input: HookRuleInput,
    actor: Actor,
) -> Result<HookRule> {
    change_state(s, input.rule_id, actor, move |_, row, viewer| {
        if !mine(row, viewer) && !viewer.owner {
            return Err(Error::forbidden(
                "Only its creator or the installation owner can resume this rule's budget.",
            ));
        }
        row.budget_exhausted_at = None;
        row.budget_reason = None;
        Ok("hook_rule.budget_resumed")
    })
    .await
}

pub(crate) async fn duplicate(
    s: &Service,
    input: DuplicateHookRuleInput,
    actor: Actor,
) -> Result<HookRule> {
    actor.require_scope("manage")?;
    actor.recheck().await?;
    let copy = creation_id(input.creation_id)?;
    let source = input.rule_id.to_string();
    let source_id = source.clone();
    let definition = s
        .store
        .read(move |db| Ok(current(db, &source_id)?.1.definition))
        .await?;
    let instances = instances(s, &definition).await?;
    refresh_native(s, &definition).await?;
    let _authors = s.missions.authors.lock().await;
    actor.recheck().await?;
    let (mut writer, _) = writer(s, &actor).await?;
    writer.source = HookRuleSource::Duplicate;
    let viewer = Viewer {
        account: Some(writer.creator.account.clone()),
        owner: writer.creator.owner,
    };
    s.store
        .transaction(move |db| {
            actor.authorize(db)?;
            if let Some(existing) = db.hook_rule(&copy)? {
                if existing.creator_account_id != writer.creator.account {
                    return Err(Error::conflict(
                        "This creation identity belongs to another rule.",
                    ));
                }
                return view(db, &copy, &viewer);
            }
            fresh_identity(db, &copy)?;
            let (_, version) = current(db, &source)?;
            for agent in agents_used(&version.definition) {
                if !writer.creator.uses_agent(db, &agent)? {
                    return Err(Error::forbidden(
                        "You can duplicate a rule only with access to every agent it uses.",
                    ));
                }
            }
            // A duplicate starts disabled and belongs to whoever duplicates it.
            let mut row = new_row(copy.clone(), &writer, true);
            save_version(db, &mut row, version.definition, &writer, &instances)?;
            db.audit(
                "hook_rule.duplicated",
                &json!({ "ruleId": row.id, "sourceId": source }),
            )?;
            view(db, &copy, &viewer)
        })
        .await
}

pub(crate) async fn delete(s: &Service, input: HookRuleInput, actor: Actor) -> Result<()> {
    actor.require_scope("manage")?;
    actor.recheck().await?;
    if matches!(actor, Actor::Installation(..)) {
        return Err(Error::forbidden(
            "An agent deletes rules only for the person of an eligible conversation.",
        ));
    }
    let viewer = actor.viewer();
    let rule = input.rule_id.to_string();
    s.store
        .transaction(move |db| {
            actor.authorize(db)?;
            let row = required(db.hook_rule(&rule)?, "Rule not found")?;
            if !mine(&row, &viewer) && !viewer.owner {
                return Err(Error::forbidden(
                    "Only its creator or the installation owner can delete this rule.",
                ));
            }
            db.delete_hook_rule(&rule)?;
            db.audit("hook_rule.deleted", &json!({ "ruleId": rule }))
        })
        .await
}

pub(crate) async fn get(s: &Service, input: HookRuleInput, actor: Actor) -> Result<HookRule> {
    actor.require_scope("read")?;
    actor.recheck().await?;
    let viewer = actor.viewer();
    s.store
        .read(move |db| {
            actor.authorize(db)?;
            view(db, &input.rule_id.to_string(), &viewer)
        })
        .await
}

/// Every member sees every rule and its state.
pub(crate) async fn list(
    s: &Service,
    input: ListHookRulesInput,
    actor: Actor,
) -> Result<ListHookRulesOutput> {
    actor.require_scope("read")?;
    actor.recheck().await?;
    let viewer = actor.viewer();
    s.store
        .read(move |db| {
            actor.authorize(db)?;
            let filtered = (input.agent_id.is_some() || input.instance_id.is_some()).then(|| {
                db.hook_rules_using(
                    input.agent_id.map(|id| id.to_string()).as_deref(),
                    input.instance_id.map(|id| id.to_string()).as_deref(),
                )
            });
            let filtered = filtered.transpose()?;
            let mut rules = Vec::new();
            for row in db.hook_rules()? {
                if filtered.as_ref().is_some_and(|ids| !ids.contains(&row.id)) {
                    continue;
                }
                rules.push(summary(db, &row, &viewer)?);
            }
            Ok(ListHookRulesOutput { rules })
        })
        .await
}

pub(crate) async fn versions(
    s: &Service,
    input: HookRuleInput,
    actor: Actor,
) -> Result<ListHookRuleVersionsOutput> {
    actor.require_scope("read")?;
    actor.recheck().await?;
    s.store
        .read(move |db| {
            actor.authorize(db)?;
            let rule = input.rule_id.to_string();
            required(db.hook_rule(&rule)?, "Rule not found")?;
            let versions = db
                .hook_rule_versions(&rule)?
                .into_iter()
                .map(|version| HookRuleVersion {
                    version: version.version,
                    saved_by_account_id: version.saved_by.clone(),
                    source: version.source,
                    conversation_id: version
                        .conversation_id
                        .as_deref()
                        .and_then(|id| uuid::Uuid::parse_str(id).ok()),
                    native_authority: version.native_authority(),
                    saved_at: version.saved_at,
                    definition: version.definition,
                })
                .collect();
            Ok(ListHookRuleVersionsOutput { versions })
        })
        .await
}

/// Checks a definition as its saving would, without writing. The editor uses it while
/// the creator edits.
pub(crate) async fn validate(
    s: &Service,
    input: ValidateHookRuleInput,
    actor: Actor,
) -> Result<ValidateHookRuleOutput> {
    actor.require_scope("read")?;
    actor.recheck().await?;
    let instances = instances(s, &input.definition).await?;
    let viewer = actor.viewer();
    s.store
        .read(move |db| {
            actor.authorize(db)?;
            let creator = match (&input.rule_id, &viewer.account) {
                (Some(rule), _) if db.hook_rule(&rule.to_string())?.is_some() => creator(
                    &required(db.hook_rule(&rule.to_string())?, "Rule not found")?,
                ),
                (_, Some(account)) => Creator {
                    account: account.clone(),
                    owner: viewer.owner,
                },
                // An agent's eventual writes act for a person; check with the owner's reach.
                (_, None) => Creator {
                    account: String::new(),
                    owner: true,
                },
            };
            let origin = viewer
                .account
                .clone()
                .map_or(Origin::Unknown, Origin::Human);
            let checked = validation::check(
                db,
                &validation::Request {
                    definition: &input.definition,
                    creator: &creator,
                    instances: &instances,
                    origin: &origin,
                },
            )?;
            Ok(ValidateHookRuleOutput {
                problems: checked.problems,
                warnings: checked.warnings,
            })
        })
        .await
}

/// The agents a person may designate: the owner, public clients and agents reach all.
async fn usable_agents(s: &Service, actor: &Actor) -> Result<Vec<AgentId>> {
    match actor {
        Actor::Person(identity) => crate::agent_access::usable_now(s, identity.clone()).await,
        _ => Ok(s
            .store
            .agents()
            .await?
            .into_iter()
            .map(|agent| agent.id)
            .collect()),
    }
}

pub(crate) async fn catalog(
    s: &Service,
    input: GetHookRuleCatalogInput,
    actor: Actor,
) -> Result<HookRuleCatalog> {
    actor.require_scope("read")?;
    actor.recheck().await?;
    let usable = usable_agents(s, &actor).await?;
    let catalog = catalog::build(s, usable, input.agent_id).await?;
    actor.recheck().await?;
    s.store.read(move |db| actor.authorize(db)).await?;
    Ok(catalog)
}

/// The rule templates of the instances on which an agent the caller may use holds a
/// grant, like the catalog's hooks and tools.
pub(crate) async fn list_templates(
    s: &Service,
    input: ListHookRuleTemplatesInput,
    actor: Actor,
) -> Result<ListHookRuleTemplatesOutput> {
    actor.require_scope("read")?;
    actor.recheck().await?;
    let usable = usable_agents(s, &actor)
        .await?
        .into_iter()
        .map(|agent| agent.to_string())
        .collect::<Vec<_>>();
    let (instances, granted) = s
        .store
        .read(move |db| {
            let mut granted = BTreeSet::new();
            for agent in &usable {
                for grant in db.agent_plugin_grants(agent)?.grants {
                    granted.insert(grant.instance_id.to_string());
                }
            }
            Ok((db.plugin_instances()?, granted))
        })
        .await?;
    let mut templates = Vec::new();
    for instance in instances {
        if !granted.contains(&instance.id)
            || input
                .instance_id
                .is_some_and(|only| only.to_string() != instance.id)
        {
            continue;
        }
        let Ok(package) = crate::plugins::registry::active(s, &instance.plugin_id).await else {
            continue;
        };
        let Ok(instance_id) = uuid::Uuid::parse_str(&instance.id).map(PluginInstanceId) else {
            continue;
        };
        for template in package.manifest.rule_templates {
            templates.push(HookRuleTemplate {
                instance_id,
                instance_name: instance.name.clone(),
                plugin_id: instance.plugin_id.clone(),
                plugin_version: package.manifest.version.clone(),
                template,
            });
        }
    }
    actor.recheck().await?;
    s.store.read(move |db| actor.authorize(db)).await?;
    Ok(ListHookRuleTemplatesOutput { templates })
}

/// Creates one ordinary rule per template rule, all or none, through the same checks as
/// [`create`]. The rules belong to the installer and keep no link to the template.
pub(crate) async fn install_template(
    s: &Service,
    input: InstallHookRuleTemplateInput,
    actor: Actor,
) -> Result<InstallHookRuleTemplateOutput> {
    actor.require_scope("manage")?;
    actor.recheck().await?;
    let creation = creation_id(input.creation_id)?;
    let instance = input.instance_id.to_string();
    let connection = required(
        s.store.plugin_instance(&instance).await?,
        "Plugin instance not found",
    )?;
    let package = crate::plugins::registry::active(s, &connection.plugin_id).await?;
    let template = package
        .manifest
        .rule_template(&input.template)
        .cloned()
        .ok_or_else(|| Error::not_found("This plugin offers no such rule template."))?;
    let values = templates::values(&template, &input.parameters)?;
    let definitions = templates::instantiate(&package.manifest, &template, &instance, &values)
        .map_err(Error::bad)?;
    let mut known = Instances::default();
    for definition in &definitions {
        known.0.extend(instances(s, definition).await?.0);
        refresh_native(s, definition).await?;
    }
    // Retrying with the same creation identity finds the same rules.
    let ids = (0..definitions.len())
        .map(|index| {
            use sha2::{Digest, Sha256};
            let digest = Sha256::digest(format!("hook-rule-template\n{creation}\n{index}"));
            let mut bytes = [0; 16];
            bytes.copy_from_slice(&digest[..16]);
            uuid::Builder::from_random_bytes(bytes)
                .into_uuid()
                .to_string()
        })
        .collect::<Vec<_>>();

    let _authors = s.missions.authors.lock().await;
    actor.recheck().await?;
    let (mut writer, _) = match &actor {
        Actor::Installation(bearer, _) => {
            let bearer = bearer.clone();
            (
                s.store
                    .read(move |db| installation_writer(db, &bearer))
                    .await?,
                None,
            )
        }
        _ => writer(s, &actor).await?,
    };
    writer.source = HookRuleSource::Template;
    let viewer = Viewer {
        account: Some(writer.creator.account.clone()),
        owner: writer.creator.owner,
    };
    s.store
        .transaction(move |db| {
            actor.authorize(db)?;
            if let Actor::Installation(bearer, _) = &actor {
                installation_writer(db, bearer)?;
            }
            if let Some(existing) = db.hook_rule(&ids[0])? {
                if existing.creator_account_id != writer.creator.account {
                    return Err(Error::conflict(
                        "This creation identity belongs to another rule.",
                    ));
                }
                let rules = ids
                    .iter()
                    .map(|id| view(db, id, &viewer))
                    .collect::<Result<_>>()?;
                return Ok(InstallHookRuleTemplateOutput { rules });
            }
            templates::authorize_agents(db, &writer.creator, &template, &values)?;
            for (index, (id, definition)) in ids.iter().zip(definitions).enumerate() {
                fresh_identity(db, id)?;
                let mut row = new_row(id.clone(), &writer, !input.enabled);
                save_version(db, &mut row, definition, &writer, &known).map_err(|mut error| {
                    error.message = format!("Rule {}: {}", index + 1, error.message);
                    error
                })?;
            }
            db.audit(
                "hook_rule.template_installed",
                &json!({
                    "instanceId": instance,
                    "template": template.name,
                    "ruleIds": ids,
                    "creator": writer.creator.account,
                }),
            )?;
            let rules = ids
                .iter()
                .map(|id| view(db, id, &viewer))
                .collect::<Result<_>>()?;
            Ok(InstallHookRuleTemplateOutput { rules })
        })
        .await
}

/// Marks the rules of ended memberships, in the transaction that observes them.
/// Re-inviting their creator never restores them.
pub(crate) fn reconcile(db: &Db<'_>, policy: &TaskAuthorPolicy) -> Result<()> {
    for mut row in db.hook_rules()? {
        if row.membership_ended_at.is_some() {
            continue;
        }
        let current = policy.grant(&row.creator_account_id).is_some_and(|grant| {
            row.creator_access_id
                .as_ref()
                .is_none_or(|access| *access == grant.access_id)
        });
        if !current {
            row.membership_ended_at = Some(now());
            row.revision = id();
            row.updated_at = now();
            db.save_hook_rule(&row)?;
            db.audit("hook_rule.membership_ended", &json!({ "ruleId": row.id }))?;
        }
    }
    Ok(())
}

/// The hooks and tools that rules of current memberships use on an execution agent,
/// with the resources the agent holds now: a reduction keeps covering them.
pub(crate) fn grant_uses(
    db: &Db<'_>,
    agent: &str,
    except_account: Option<&str>,
) -> Result<Vec<crate::plugins::grants::RuleGrantUse>> {
    let mut uses = Vec::new();
    for (account, usage) in db.hook_rule_grant_uses(agent)? {
        if except_account == Some(account.as_str()) {
            continue;
        }
        let kind = if usage.kind == "hook" {
            PluginContributionKind::Hook
        } else {
            PluginContributionKind::Tool
        };
        let Some(held) = db.plugin_contribution(agent, &usage.instance_id, kind, &usage.name)?
        else {
            continue;
        };
        let Ok(instance) = uuid::Uuid::parse_str(&usage.instance_id) else {
            continue;
        };
        uses.push(crate::plugins::grants::RuleGrantUse {
            instance_id: PluginInstanceId(instance),
            contribution: held,
        });
    }
    Ok(uses)
}
