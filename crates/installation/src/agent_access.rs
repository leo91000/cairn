//! Which member accounts may use each agent (ADR-0075, « Accès des comptes aux agents »).
//!
//! The owner uses every agent; a member uses only the agents the owner gave them, and a
//! new agent is the owner's alone. Using an agent means starting, continuing, steering,
//! launching by hand and answering its conversations and tasks, deleting its
//! conversations, and creating or editing its tasks. Pausing, archiving and stopping stay
//! open to everyone, and every member still sees every conversation.
//!
//! An entry belongs to the membership it was given in: reconciling memberships with the
//! Beacon drops the entries of removed or re-invited members. Only the owner gives or
//! removes entries, from the web and Android apps; the public MCP and
//! `cairn_installation` can only read them.
use crate::{
    auth::{InstallationIdentity, InstallationRole},
    chats::storage::Conversation,
    error::{Error, Result, required},
    service::Service,
    store::Db,
    validation::text,
};
use cairn_contracts::{
    AgentAccount, AgentId, ConversationLifecycle, ListAgentAccountsOutput, Mission, MissionSettings,
};
use cairn_protocol::TaskAuthorPolicy;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde_json::{Value, json};
use std::sync::Arc;

pub(crate) const SCHEMA: &str = "
CREATE TABLE agent_account_access(
 agent_id TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
 account_id TEXT NOT NULL CHECK(length(account_id) BETWEEN 1 AND 100),
 access_id TEXT NOT NULL CHECK(length(access_id) BETWEEN 1 AND 300),
 granted_at INTEGER NOT NULL CHECK(granted_at BETWEEN 0 AND 9007199254740991),
 PRIMARY KEY(agent_id,account_id)
) WITHOUT ROWID;
CREATE INDEX agent_account_access_account ON agent_account_access(account_id);
";

/// Agents that existed before account access: the members of the first membership check
/// after the upgrade keep using them. The installation keeps no list of members, so someone
/// invited between the upgrade and that check, which only a Beacon outage can delay before
/// any member reaches the installation again, counts as an existing member.
const ROLLOUT: &str = "agent-access-rollout";

/// Shown on a scheduled task whose author cannot use its agent; it runs again once they can.
pub(crate) const SUSPENDED: &str =
    "Its author no longer has access to this agent. It resumes once the owner gives access again.";

const DENIED: &str =
    "You don't have access to this agent. Ask the installation owner to give you access.";

/// Version 18 to 19. Members at the first membership check keep the existing agents.
pub(crate) fn upgrade(connection: &mut Connection) -> Result<()> {
    let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    // Recheck under the write lock: another startup may have completed the upgrade.
    let version: i64 = tx.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version == 19 {
        return Ok(());
    }
    if version != 18 {
        return Err(Error::bad(
            "Unexpected installation database version during upgrade.",
        ));
    }

    tx.execute_batch(SCHEMA)?;
    let agents = tx
        .prepare("SELECT id FROM agents ORDER BY id")?
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Db(&tx).set(ROLLOUT, &json!({ "agentIds": agents }), None)?;
    tx.pragma_update(None, "user_version", 19)?;
    tx.commit()?;
    Ok(())
}

impl Db<'_> {
    pub(crate) fn agent_accounts(&self, agent: &str) -> Result<Vec<AgentAccount>> {
        Ok(self
            .0
            .prepare_cached(
                "SELECT account_id,granted_at FROM agent_account_access WHERE agent_id=? ORDER BY granted_at,account_id",
            )?
            .query_map([agent], |row| {
                Ok(AgentAccount {
                    account_id: row.get(0)?,
                    granted_at: row.get(1)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?)
    }

    /// Whether a member's current membership may use the agent. The owner holds no entry.
    pub(crate) fn member_uses_agent(&self, account: &str, agent: &str) -> Result<bool> {
        Ok(self
            .0
            .prepare_cached("SELECT 1 FROM agent_account_access WHERE agent_id=? AND account_id=?")?
            .query_row([agent, account], |_| Ok(()))
            .optional()?
            .is_some())
    }

    fn membership_uses_agent(&self, account: &str, access: &str, agent: &str) -> Result<bool> {
        Ok(self
            .0
            .prepare_cached(
                "SELECT 1 FROM agent_account_access WHERE agent_id=? AND account_id=? AND access_id=?",
            )?
            .query_row([agent, account, access], |_| Ok(()))
            .optional()?
            .is_some())
    }

    fn member_agents(&self, account: &str) -> Result<Vec<String>> {
        Ok(self
            .0
            .prepare_cached(
                "SELECT agent_id FROM agent_account_access WHERE account_id=? ORDER BY agent_id",
            )?
            .query_map([account], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?)
    }
}

/// Whether this identity may use the agent. Only people are restricted: the public MCP
/// acts for the owner, and agents with installation access check nothing more here.
pub(crate) fn uses_agent(
    db: &Db<'_>,
    identity: &InstallationIdentity,
    agent: &str,
) -> Result<bool> {
    if identity.role == InstallationRole::Owner || identity.mcp_scopes.is_some() {
        return Ok(true);
    }
    db.member_uses_agent(&identity.account_id, agent)
}

/// A conversation is directed through its agent. A delegated task may also be directed
/// through the agent of its requesting conversation, until that conversation is deleted.
pub(crate) fn uses_chat(
    db: &Db<'_>,
    identity: &InstallationIdentity,
    chat: &Conversation,
) -> Result<bool> {
    if uses_agent(db, identity, &chat.agent_id)? {
        return Ok(true);
    }
    match requesting(db, chat)? {
        Some(requesting) => uses_agent(db, identity, &requesting.agent_id),
        None => Ok(false),
    }
}

/// The conversation that delegated this one, while it is not deleted.
fn requesting(db: &Db<'_>, chat: &Conversation) -> Result<Option<Conversation>> {
    let Some(requester) = &chat.requested_by else {
        return Ok(None);
    };
    Ok(db
        .conversation(&requester.chat_id)?
        .filter(|requesting| requesting.lifecycle == ConversationLifecycle::Active))
}

/// The members who may direct a conversation, by the same rule as [`uses_chat`].
pub(crate) fn chat_members(db: &Db<'_>, chat: &Conversation) -> Result<Vec<String>> {
    let mut agents = vec![chat.agent_id.clone()];
    if let Some(requesting) = requesting(db, chat)? {
        agents.push(requesting.agent_id);
    }
    let mut members = Vec::new();
    for agent in agents {
        members.extend(
            db.agent_accounts(&agent)?
                .into_iter()
                .map(|entry| entry.account_id),
        );
    }
    members.sort_unstable();
    members.dedup();
    Ok(members)
}

/// Allowed requests report whether a member's entry allowed them.
fn require(allowed: bool, member: bool) -> Result<bool> {
    if allowed {
        Ok(member)
    } else {
        Err(Error::forbidden(DENIED))
    }
}

fn member(identity: &InstallationIdentity) -> bool {
    identity.role == InstallationRole::Member && identity.mcp_scopes.is_none()
}

fn require_agent(db: &Db<'_>, identity: &InstallationIdentity, agent: &str) -> Result<bool> {
    require(uses_agent(db, identity, agent)?, member(identity))
}

fn require_chat(db: &Db<'_>, identity: &InstallationIdentity, id: &str) -> Result<bool> {
    let chat = required(db.conversation(id)?, "Chat not found")?;
    require(uses_chat(db, identity, &chat)?, member(identity))
}

/// A conversation's run follows its conversation; a task run follows its agent.
fn require_run(db: &Db<'_>, identity: &InstallationIdentity, id: &str) -> Result<bool> {
    if let Some(chat) = db.conversation_for_run(id)? {
        return require(uses_chat(db, identity, &chat)?, member(identity));
    }
    let run = required(db.run(id)?, "Run not found")?;
    // An earlier run of a conversation still follows the conversation.
    if let Some(chat) = db.conversation(text(&run, "taskId"))? {
        return require(uses_chat(db, identity, &chat)?, member(identity));
    }
    require_agent(db, identity, crate::nodes::run_agent(&run))
}

fn require_task(db: &Db<'_>, identity: &InstallationIdentity, id: &str) -> Result<bool> {
    let task = required(db.mission(id)?, "Mission not found")?;
    require_agent(db, identity, &task.settings.agent_id.to_string())
}

/// Read-only admission preflight for people, before dispatch, like native authority.
/// Reductions (pause, archive, stop, cancel) and reads stay open to every member.
/// Access allowed by a member's entry relies on a recently verified membership, so an
/// entry from an ended membership never serves a re-invited member for long. As with
/// native authority, a change committed between this check and the command can still let
/// that one request through.
pub(crate) async fn authorize_request(s: &Service, input: &crate::http::Input) -> Result<()> {
    let Some(identity) = input.identity.clone() else {
        return Ok(());
    };
    if matches!(input.method.as_str(), "GET" | "HEAD") {
        return Ok(());
    }
    let request = Arc::new((input.method.clone(), input.path.clone(), input.body.clone()));
    authorize(s, identity, move |db, identity| {
        let (method, path, body) = &*request;
        gate(db, identity, method, path, body)
    })
    .await
}

/// Files are uploaded for a conversation's next message: only those who may direct it.
pub(crate) async fn authorize_upload(
    s: &Service,
    identity: Option<&InstallationIdentity>,
    chat: &str,
) -> Result<()> {
    let Some(identity) = identity.cloned() else {
        return Ok(());
    };
    let chat = chat.to_owned();
    authorize(s, identity, move |db, identity| {
        require_chat(db, identity, &chat)
    })
    .await
}

/// Runs a check that reports whether a member's entry allowed the request, first
/// confirming the membership again when the last check is too old.
async fn authorize<F>(s: &Service, identity: InstallationIdentity, check: F) -> Result<()>
where
    F: Fn(&Db<'_>, &InstallationIdentity) -> Result<bool> + Send + Sync + 'static,
{
    if identity.role == InstallationRole::Owner || identity.mcp_scopes.is_some() {
        return Ok(());
    }
    let check = Arc::new(check);
    let run = || {
        let (identity, check) = (identity.clone(), check.clone());
        s.store.read(move |db| check(db, &identity))
    };
    if !run().await? || s.missions.membership_verified_within(MEMBERSHIP_AGE) {
        return Ok(());
    }

    // A failed check fails closed: these requests direct an agent.
    if let Err(error) = s.missions.verify_membership(s, MEMBERSHIP_AGE).await {
        tracing::warn!(status = error.status, "Agent access could not be confirmed");
        return Err(Error::unavailable(
            "Your access to this agent could not be confirmed right now. Try again in a moment.",
        ));
    }
    run().await.map(|_| ())
}

/// The agents a person may use now. A member's list first confirms their membership when
/// the last check is too old, so an ended membership's entries soon stop showing; when
/// the Beacon cannot answer, the list still shows them and directing requests fail closed.
pub(crate) async fn usable_now(
    s: &Service,
    identity: InstallationIdentity,
) -> Result<Vec<AgentId>> {
    if member(&identity) {
        confirm_memberships(s).await;
    }
    s.store.read(move |db| usable(db, &identity)).await
}

/// Confirms memberships when the last check is too old, dropping ended memberships' entries
/// and those of an earlier membership of a re-invited member. When the Beacon cannot answer,
/// the entries stay as they are.
pub(crate) async fn confirm_memberships(s: &Service) {
    if !s.missions.membership_verified_within(MEMBERSHIP_AGE)
        && let Err(error) = s.missions.verify_membership(s, MEMBERSHIP_AGE).await
    {
        tracing::warn!(status = error.status, "Agent access could not be confirmed");
    }
}

/// How long a membership check vouches for a member's entries.
const MEMBERSHIP_AGE: std::time::Duration = std::time::Duration::from_secs(30);

/// Whether the request needs access to an agent, and then that the identity holds it.
/// Returns true when a member's entry allowed it.
fn gate(
    db: &Db<'_>,
    identity: &InstallationIdentity,
    method: &str,
    path: &str,
    body: &Value,
) -> Result<bool> {
    let parts = path
        .trim_start_matches("/api/")
        .split('/')
        .collect::<Vec<_>>();
    match (method, parts.as_slice()) {
        ("POST", ["chats"]) => require_agent(
            db,
            identity,
            body["agentId"]
                .as_str()
                .unwrap_or(crate::config::MAIN_AGENT_ID),
        ),
        // Deleting, and restoring what was deleted, needs the conversation's own agent, even
        // for a delegated task.
        ("DELETE", ["chats", id]) | ("POST", ["chats", id, "restore"]) => {
            let chat = required(db.conversation(id)?, "Chat not found")?;
            require_agent(db, identity, &chat.agent_id)
        }
        ("POST", ["chats", id, "messages"])
        | ("PUT", ["chats", id, "messages", _])
        | ("POST", ["chats", id, "questions", _, "answer" | "cancel"])
        | ("POST", ["chats", id, "new-session" | "compact" | "coding-agent"]) => {
            require_chat(db, identity, id)
        }
        ("POST", ["chats", id, "pause"]) if body["paused"] != true => {
            require_chat(db, identity, id)
        }
        // A retry starts the task again as it is now, on its current agent.
        ("POST", ["runs", id, "retry"]) => {
            let run = required(db.run(id)?, "Run not found")?;
            match db.mission(text(&run, "taskId"))? {
                Some(task) => require_agent(db, identity, &task.settings.agent_id.to_string()),
                None => require_run(db, identity, id),
            }
        }
        ("POST", ["runs", id, "resume" | "chat"]) => require_run(db, identity, id),
        // A missing agent is left to the command's own validation.
        ("POST", ["tasks"]) => match body["settings"]["agentId"].as_str() {
            Some(agent) => require_agent(db, identity, agent),
            None => Ok(false),
        },
        // Members edit only their own tasks; like a rule, a suspended task can move to an
        // agent its author may use. Pausing or archiving it stays open to everyone.
        ("PUT", ["tasks", id]) => match body["settings"]["agentId"].as_str() {
            Some(_) if reduces_task(db, id, &body["settings"])? => Ok(false),
            Some(agent) => require_agent(db, identity, agent),
            None => Ok(false),
        },
        ("POST", ["tasks", id, "state"]) => {
            let task = required(db.mission(id)?, "Mission not found")?;
            let reduces =
                body["enabled"] == false && (body["archived"] == true || !task.settings.archived);
            if reduces {
                return Ok(false);
            }
            require_agent(db, identity, &task.settings.agent_id.to_string())
        }
        ("POST", ["tasks", id, "run"]) => require_task(db, identity, id),
        _ => Ok(false),
    }
}

/// The same reduction as a task save: only pausing, or archiving, and nothing else.
fn reduces_task(db: &Db<'_>, id: &str, settings: &Value) -> Result<bool> {
    let Some(current) = db.mission(id)? else {
        return Ok(false);
    };
    let Ok(mut settings) = serde_json::from_value::<MissionSettings>(settings.clone()) else {
        return Ok(false);
    };
    if settings.validate().is_err() {
        return Ok(false);
    }
    let mut unchanged = settings.clone();
    unchanged.enabled = current.settings.enabled;
    unchanged.archived = current.settings.archived;
    Ok(!settings.enabled
        && (!current.settings.archived || settings.archived)
        && unchanged == current.settings)
}

/// A scheduled task runs only while its author may use its agent. Tasks without a
/// recorded author predate authorship and belong to the owner.
pub(crate) fn suspended(db: &Db<'_>, task: &Mission, policy: &TaskAuthorPolicy) -> Result<bool> {
    let (Some(author), Some(access)) = (&task.author_id, &task.author_access_id) else {
        return Ok(false);
    };
    if *author == policy.owner.account_id {
        return Ok(false);
    }
    Ok(!db.membership_uses_agent(author, access, &task.settings.agent_id.to_string())?)
}

/// Show or clear the suspension of the scheduled tasks of one author, or of every author.
fn mark_suspensions(
    db: &Db<'_>,
    author: Option<&str>,
    policy: Option<&TaskAuthorPolicy>,
) -> Result<()> {
    for mut task in db.missions()? {
        if task.author_removed
            || task.settings.cron.is_none()
            || author.is_some_and(|author| task.author_id.as_deref() != Some(author))
        {
            continue;
        }
        let blocked = match policy {
            Some(policy) => suspended(db, &task, policy)?,
            // Only a member, never the owner, holds or loses an entry.
            None => match (&task.author_id, &task.author_access_id) {
                (Some(author), Some(access)) => {
                    !db.membership_uses_agent(author, access, &task.settings.agent_id.to_string())?
                }
                _ => false,
            },
        };
        let shown = task.schedule_wait_reason.as_deref() == Some(SUSPENDED);
        if blocked == shown {
            continue;
        }
        task.schedule_wait_reason = blocked.then(|| SUSPENDED.to_owned());
        db.save_mission(&task)?;
        db.audit(
            if blocked {
                "task.agent_access_suspended"
            } else {
                "task.agent_access_resumed"
            },
            &json!({ "taskId": task.id }),
        )?;
    }
    Ok(())
}

/// Called with every membership observation: drops entries of memberships that ended
/// (including a removal followed by a new invitation), then completes the rollout.
pub(crate) fn reconcile(db: &Db<'_>, policy: Option<&TaskAuthorPolicy>) -> Result<()> {
    let Some(policy) = policy else {
        // Only an installation without a relay identity observes no policy. The rollout
        // waits for a real observation: right after a claim it has no member yet and is
        // consumed harmlessly, while a claimed installation never loses it. Unreadable
        // identities fail instead.
        return Ok(());
    };
    let entries =
        db.0.prepare("SELECT agent_id,account_id,access_id FROM agent_account_access")?
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
    for (agent, account, access) in entries {
        let current = policy
            .members
            .iter()
            .any(|member| member.account_id == account && member.access_id == access);
        if !current {
            db.0.execute(
                "DELETE FROM agent_account_access WHERE agent_id=?1 AND account_id=?2",
                params![agent, account],
            )?;
            db.audit(
                "agent.account_access_ended",
                &json!({ "agentId": agent, "accountId": account }),
            )?;
        }
    }

    if let Some(rollout) = db.kv(ROLLOUT)? {
        for agent in rollout["agentIds"].as_array().into_iter().flatten() {
            let Some(agent) = agent.as_str() else {
                continue;
            };
            if db.agent(agent)?.is_none() {
                continue;
            }
            for member in &policy.members {
                db.0.execute(
                    "INSERT OR IGNORE INTO agent_account_access VALUES(?1,?2,?3,?4)",
                    params![
                        agent,
                        member.account_id,
                        member.access_id,
                        crate::config::now()
                    ],
                )?;
            }
        }
        db.delete(ROLLOUT)?;
        db.audit("agent.account_access_rollout", &json!({}))?;
    }
    mark_suspensions(db, None, Some(policy))
}

pub(crate) fn list(db: &Db<'_>, agent: AgentId) -> Result<ListAgentAccountsOutput> {
    let id = agent.to_string();
    required(db.agent(&id)?, "Agent not found")?;
    Ok(ListAgentAccountsOutput {
        agent_id: agent,
        accounts: db.agent_accounts(&id)?,
    })
}

/// Agents the caller may use, for choosing an agent and showing what they may direct.
pub(crate) fn usable(db: &Db<'_>, identity: &InstallationIdentity) -> Result<Vec<AgentId>> {
    let ids = if identity.role == InstallationRole::Owner || identity.mcp_scopes.is_some() {
        db.agents()?
            .into_iter()
            .map(|agent| agent.id.to_string())
            .collect()
    } else {
        db.member_agents(&identity.account_id)?
    };
    ids.iter()
        .map(|id| {
            uuid::Uuid::parse_str(id)
                .map(AgentId)
                .map_err(|_| Error::internal("Invalid agent identity."))
        })
        .collect()
}

/// Only the owner, from the apps: never the public MCP or an agent tool.
fn require_owner(identity: Option<&InstallationIdentity>) -> Result<()> {
    match identity {
        Some(identity)
            if identity.role == InstallationRole::Owner && identity.mcp_scopes.is_none() =>
        {
            Ok(())
        }
        _ => Err(Error::forbidden(
            "Only the installation owner gives or removes access to agents, from the apps.",
        )),
    }
}

impl Service {
    pub(crate) async fn grant_agent_account(
        &self,
        identity: Option<&InstallationIdentity>,
        agent: AgentId,
        account: &str,
    ) -> Result<ListAgentAccountsOutput> {
        require_owner(identity)?;
        crate::validation::uuid(&agent.to_string())?;
        if account.is_empty() || account.len() > 100 {
            return Err(Error::bad("Choose a member of the installation."));
        }

        // The entry belongs to the membership the Beacon reports now.
        let _authors = self.missions.authors.lock().await;
        let observed = self.missions.observe(self).await.map_err(|error| {
            tracing::warn!(status = error.status, "Members could not be confirmed");
            Error::unavailable(
                "The installation's members could not be confirmed right now. Try again in a moment.",
            )
        })?;
        let policy = observed.ok_or_else(|| {
            Error::conflict("Claim the installation before sharing agents with members.")
        })?;
        if policy.owner.account_id == account {
            return Err(Error::bad("The owner can always use every agent."));
        }
        let member = policy
            .members
            .iter()
            .find(|member| member.account_id == account)
            .cloned()
            .ok_or_else(|| Error::not_found("This account is not a member of the installation."))?;
        self.store
            .transaction(move |db| {
                let id = agent.to_string();
                required(db.agent(&id)?, "Agent not found")?;
                // Observing reconciled ended memberships under the same lock.
                let granted = db.0.execute(
                    "INSERT OR IGNORE INTO agent_account_access VALUES(?1,?2,?3,?4)",
                    params![
                        id,
                        member.account_id,
                        member.access_id,
                        crate::config::now()
                    ],
                )?;
                if granted > 0 {
                    db.audit(
                        "agent.account_access_granted",
                        &json!({ "agentId": id, "accountId": member.account_id }),
                    )?;
                }
                mark_suspensions(db, Some(&member.account_id), Some(&policy))?;
                list(db, agent)
            })
            .await
    }

    pub(crate) async fn revoke_agent_account(
        &self,
        identity: Option<&InstallationIdentity>,
        agent: AgentId,
        account: &str,
    ) -> Result<ListAgentAccountsOutput> {
        require_owner(identity)?;
        let account = account.to_owned();
        let _authors = self.missions.authors.lock().await;
        self.store
            .transaction(move |db| {
                let id = agent.to_string();
                required(db.agent(&id)?, "Agent not found")?;
                let removed = db.0.execute(
                    "DELETE FROM agent_account_access WHERE agent_id=?1 AND account_id=?2",
                    params![id, account],
                )?;
                if removed > 0 {
                    db.audit(
                        "agent.account_access_revoked",
                        &json!({ "agentId": id, "accountId": account }),
                    )?;
                    mark_suspensions(db, Some(&account), None)?;
                }
                list(db, agent)
            })
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{agent_fixture::AgentFixture, relay_fixture};

    const MEMBER: &str = "member-account";

    /// A member's request straight to the installation API, without the fixture relay's
    /// own membership refresh, so only the installation decides when to confirm them.
    async fn direct(s: &Arc<Service>, body: Value) -> std::result::Result<Value, (u16, String)> {
        Box::pin(crate::api::dispatch(
            s,
            &crate::http::Input {
                method: "POST".into(),
                path: "/api/chats".into(),
                query: std::collections::HashMap::default(),
                headers: axum::http::HeaderMap::default(),
                body,
                identity: Some(InstallationIdentity::trusted(
                    InstallationRole::Member,
                    MEMBER,
                )),
            },
        ))
        .await
        .map_err(|error| (error.status, error.message))
    }

    async fn service(root: &tempfile::TempDir) -> Arc<Service> {
        Service::new(crate::config::Config {
            data_dir: root.path().join("data"),
            home: root.path().join("home"),
            workspace_roots: vec![root.path().to_owned()],
            public_url: "http://localhost:4310".into(),
            host: "127.0.0.1".into(),
            port: 0,
            codex_bin: "codex".into(),
            claude_bin: "claude".into(),
            opencode_bin: "opencode".into(),
            opencode_go_usage_url: crate::accounts::opencode::USAGE_URL.into(),
            concurrency: 1,
            logger: false,
            worker_enabled: false,
            runner_url: String::new(),
        })
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn questions_reach_the_members_who_may_answer_through_either_agent() {
        let root = tempfile::tempdir().unwrap();
        let s = service(&root).await;
        relay_fixture::claimed(&s).await.unwrap();
        let reviewer = s
            .agent_fixture(json!({ "name": "Reviewer" }), None)
            .await
            .unwrap();
        let reviewer = text(&reviewer, "id").to_owned();
        relay_fixture::allow_member(&s, MEMBER, &reviewer).await;
        relay_fixture::allow_member(&s, "main-member", crate::config::MAIN_AGENT_ID).await;

        let requesting =
            json!({ "id": crate::config::id(), "agentId": reviewer, "lifecycle": "active" });
        let delegated = json!({
            "id": crate::config::id(),
            "agentId": crate::config::MAIN_AGENT_ID,
            "lifecycle": "active",
            "requestedBy": { "runId": crate::config::id(), "chatId": requesting["id"] },
        });
        let members = |requesting: Value, delegated: Value| {
            let s = s.clone();
            async move {
                s.store
                    .transaction(move |db| {
                        db.save_conversation(&serde_json::from_value(requesting)?)?;
                        chat_members(db, &serde_json::from_value(delegated)?)
                    })
                    .await
                    .unwrap()
            }
        };

        // The delegated task's own agent, and its requesting conversation's while it exists.
        assert_eq!(
            members(requesting.clone(), delegated.clone()).await,
            vec!["main-member".to_owned(), MEMBER.to_owned()]
        );
        let mut deleted = requesting;
        deleted["id"] = crate::config::id().into();
        deleted["lifecycle"] = "trash".into();
        let mut orphan = delegated;
        orphan["requestedBy"]["chatId"] = deleted["id"].clone();
        assert_eq!(
            members(deleted, orphan).await,
            vec!["main-member".to_owned()]
        );
    }

    #[tokio::test]
    async fn a_stale_membership_check_is_repeated_and_fails_closed_without_the_beacon() {
        let root = tempfile::tempdir().unwrap();
        let s = service(&root).await;
        std::fs::create_dir_all(&s.config.home).unwrap();
        relay_fixture::claimed(&s).await.unwrap();
        let reviewer = s
            .agent_fixture(json!({ "name": "Reviewer" }), None)
            .await
            .unwrap();
        let reviewer = text(&reviewer, "id").to_owned();
        relay_fixture::allow_member(&s, MEMBER, &reviewer).await;
        let start = json!({ "agentId": reviewer });

        // While the Beacon cannot confirm the membership, the entry allows nothing.
        relay_fixture::authority_unavailable(&s, true).await;
        s.missions.expire_membership_check();
        let (status, message) = direct(&s, start.clone()).await.unwrap_err();
        assert_eq!(status, 503);
        assert!(message.contains("could not be confirmed"), "{message}");
        relay_fixture::authority_unavailable(&s, false).await;
        direct(&s, start.clone()).await.unwrap();

        // A recent check vouches for the entry; an expired one drops an ended membership.
        relay_fixture::remove_member(&s, MEMBER).await;
        relay_fixture::add_member(&s, MEMBER, "membership-2").await;
        direct(&s, start.clone()).await.unwrap();
        s.missions.expire_membership_check();
        assert_eq!(direct(&s, start).await.unwrap_err().0, 403);
    }

    #[tokio::test]
    async fn the_owner_is_told_plainly_when_members_cannot_be_confirmed() {
        let root = tempfile::tempdir().unwrap();
        let s = service(&root).await;
        relay_fixture::claimed(&s).await.unwrap();
        relay_fixture::add_member(&s, MEMBER, "membership-1").await;
        let reviewer = s
            .agent_fixture(json!({ "name": "Reviewer" }), None)
            .await
            .unwrap();
        let reviewer = AgentId(text(&reviewer, "id").parse().unwrap());
        let owner = InstallationIdentity::trusted(InstallationRole::Owner, "fixture-owner");

        relay_fixture::authority_unavailable(&s, true).await;
        let error = s
            .grant_agent_account(Some(&owner), reviewer, MEMBER)
            .await
            .unwrap_err();
        assert_eq!(error.status, 503);
        assert!(
            error.message.contains("could not be confirmed right now"),
            "{}",
            error.message
        );
        relay_fixture::authority_unavailable(&s, false).await;
        s.grant_agent_account(Some(&owner), reviewer, MEMBER)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn the_owner_never_sees_an_earlier_membership_s_entry_as_access() {
        let root = tempfile::tempdir().unwrap();
        let s = service(&root).await;
        relay_fixture::claimed(&s).await.unwrap();
        let reviewer = s
            .agent_fixture(json!({ "name": "Reviewer" }), None)
            .await
            .unwrap();
        let reviewer = text(&reviewer, "id").to_owned();
        relay_fixture::allow_member(&s, MEMBER, &reviewer).await;
        let listed = || {
            let (s, reviewer) = (s.clone(), reviewer.clone());
            async move {
                let output = Box::pin(crate::api::dispatch(
                    &s,
                    &crate::http::Input {
                        method: "GET".into(),
                        path: format!("/api/agents/{reviewer}/accounts"),
                        query: std::collections::HashMap::default(),
                        headers: axum::http::HeaderMap::default(),
                        body: Value::Null,
                        identity: Some(InstallationIdentity::trusted(
                            InstallationRole::Owner,
                            "fixture-owner",
                        )),
                    },
                ))
                .await
                .unwrap();
                output["accounts"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|account| text(account, "accountId").to_owned())
                    .collect::<Vec<_>>()
            }
        };
        assert_eq!(listed().await, vec![MEMBER.to_owned()]);

        // Removed and invited again: the entry belonged to the earlier membership.
        relay_fixture::remove_member(&s, MEMBER).await;
        relay_fixture::add_member(&s, MEMBER, "membership-2").await;
        s.missions.expire_membership_check();
        assert!(listed().await.is_empty());
    }

    #[tokio::test]
    async fn a_request_does_not_wait_on_a_lock_holder_that_checks_no_membership() {
        let root = tempfile::tempdir().unwrap();
        let s = service(&root).await;
        relay_fixture::claimed(&s).await.unwrap();
        s.missions.expire_membership_check();

        // A save or an idle scheduling pass holds the lock briefly and checks nothing.
        let authors = s.missions.authors.lock().await;
        let waiting = {
            let s = s.clone();
            tokio::spawn(async move {
                s.missions
                    .verify_membership(&s, std::time::Duration::from_secs(30))
                    .await
            })
        };
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        drop(authors);
        tokio::time::timeout(std::time::Duration::from_secs(5), waiting)
            .await
            .expect("the request must take the lock once it is free")
            .unwrap()
            .unwrap();
        assert!(
            s.missions
                .membership_verified_within(std::time::Duration::from_secs(30))
        );
    }

    #[tokio::test]
    async fn a_request_fails_closed_behind_a_long_lock_holder() {
        let root = tempfile::tempdir().unwrap();
        let s = service(&root).await;
        relay_fixture::claimed(&s).await.unwrap();
        s.missions.expire_membership_check();

        // A long merge or scheduling pass keeps the lock and checks no membership.
        let authors = s.missions.authors.lock().await;
        let error = tokio::time::timeout(
            std::time::Duration::from_secs(20),
            s.missions
                .verify_membership(&s, std::time::Duration::from_secs(30)),
        )
        .await
        .expect("the request must not wait for the holder's whole work")
        .unwrap_err();
        assert_eq!(error.status, 503);
        drop(authors);
    }

    #[tokio::test]
    async fn a_request_waits_only_for_the_lock_holders_own_membership_check() {
        let root = tempfile::tempdir().unwrap();
        let s = service(&root).await;
        relay_fixture::claimed(&s).await.unwrap();
        s.missions.expire_membership_check();

        // A long task admission holds the lock and confirms the memberships once.
        let authors = s.missions.authors.lock().await;
        let waiting = {
            let s = s.clone();
            tokio::spawn(async move {
                s.missions
                    .verify_membership(&s, std::time::Duration::from_secs(30))
                    .await
            })
        };
        tokio::task::yield_now().await;
        s.missions.observe(&s).await.unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(5), waiting)
            .await
            .expect("the request must not wait for the rest of the admission")
            .unwrap()
            .unwrap();
        drop(authors);
    }
}
