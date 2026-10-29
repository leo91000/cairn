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
    conversation_lifecycle::is_active,
    error::{Error, Result, required},
    service::Service,
    store::Db,
    validation::text,
};
use cairn_contracts::{AgentAccount, AgentId, ListAgentAccountsOutput, Mission};
use cairn_protocol::TaskAuthorPolicy;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde_json::{Value, json};

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

/// Agents that existed before account access: their members keep using them.
const ROLLOUT: &str = "agent-access-rollout";

/// Shown on a scheduled task whose author cannot use its agent; it runs again once they can.
pub(crate) const SUSPENDED: &str =
    "Its author no longer has access to this agent. It resumes once the owner gives access again.";

const DENIED: &str =
    "You don't have access to this agent. Ask the installation owner to give you access.";

/// Version 13 to 14. Members at the first membership check keep the existing agents.
pub(crate) fn upgrade(connection: &mut Connection) -> Result<()> {
    let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    // Recheck under the write lock: another startup may have completed the upgrade.
    let version: i64 = tx.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version == 14 {
        return Ok(());
    }
    if version != 13 {
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
    tx.pragma_update(None, "user_version", 14)?;
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
    chat: &Value,
) -> Result<bool> {
    if uses_agent(db, identity, text(chat, "agentId"))? {
        return Ok(true);
    }
    let Some(requester) = chat["requestedBy"]["chatId"].as_str() else {
        return Ok(false);
    };
    match db.get("chats", requester)? {
        Some(requesting) if is_active(&requesting) => {
            uses_agent(db, identity, text(&requesting, "agentId"))
        }
        _ => Ok(false),
    }
}

fn require(allowed: bool) -> Result<()> {
    if allowed {
        Ok(())
    } else {
        Err(Error::forbidden(DENIED))
    }
}

fn require_agent(db: &Db<'_>, identity: &InstallationIdentity, agent: &str) -> Result<()> {
    require(uses_agent(db, identity, agent)?)
}

fn require_chat(db: &Db<'_>, identity: &InstallationIdentity, id: &str) -> Result<()> {
    let chat = required(db.get("chats", id)?, "Chat not found")?;
    require(uses_chat(db, identity, &chat)?)
}

/// A conversation's run follows its conversation; a task run follows its agent.
fn require_run(db: &Db<'_>, identity: &InstallationIdentity, id: &str) -> Result<()> {
    if let Some(chat) = crate::chats::chat_for_run(db, id)? {
        return require(uses_chat(db, identity, &chat)?);
    }
    let run = required(db.run(id)?, "Run not found")?;
    require_agent(db, identity, crate::nodes::run_agent(&run))
}

fn require_task(db: &Db<'_>, identity: &InstallationIdentity, id: &str) -> Result<()> {
    let task = required(db.mission(id)?, "Mission not found")?;
    require_agent(db, identity, &task.settings.agent_id.to_string())
}

/// Read-only admission preflight for people, before dispatch, like native authority.
/// Reductions (pause, archive, stop, cancel) and reads stay open to every member.
pub(crate) async fn authorize_request(s: &Service, input: &crate::http::Input) -> Result<()> {
    let Some(identity) = input.identity.clone() else {
        return Ok(());
    };
    if identity.role == InstallationRole::Owner || identity.mcp_scopes.is_some() {
        return Ok(());
    }
    let parts = input
        .path
        .trim_start_matches("/api/")
        .split('/')
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let method = input.method.clone();
    let body = input.body.clone();
    s.store
        .read(move |db| {
            let parts = parts.iter().map(String::as_str).collect::<Vec<_>>();
            match (method.as_str(), parts.as_slice()) {
                ("POST", ["chats"]) => require_agent(
                    db,
                    &identity,
                    body["agentId"]
                        .as_str()
                        .unwrap_or(crate::config::MAIN_AGENT_ID),
                ),
                // Deleting needs the conversation's own agent, even for a delegated task.
                ("DELETE", ["chats", id]) => {
                    let chat = required(db.get("chats", id)?, "Chat not found")?;
                    require_agent(db, &identity, text(&chat, "agentId"))
                }
                ("POST", ["chats", id, "messages"])
                | ("PUT", ["chats", id, "messages", _])
                | ("POST", ["chats", id, "questions", _, "answer" | "cancel"])
                | ("POST", ["chats", id, "new-session" | "compact" | "coding-agent"]) => {
                    require_chat(db, &identity, id)
                }
                ("POST", ["chats", id, "pause"]) if body["paused"] != true => {
                    require_chat(db, &identity, id)
                }
                ("POST", ["runs", id, "resume" | "retry" | "chat"]) => {
                    require_run(db, &identity, id)
                }
                // A missing agent is left to the command's own validation.
                ("POST", ["tasks"]) => match body["settings"]["agentId"].as_str() {
                    Some(agent) => require_agent(db, &identity, agent),
                    None => Ok(()),
                },
                ("PUT", ["tasks", id]) => {
                    require_task(db, &identity, id)?;
                    match body["settings"]["agentId"].as_str() {
                        Some(agent) => require_agent(db, &identity, agent),
                        None => Ok(()),
                    }
                }
                ("POST", ["tasks", id, "state"]) => {
                    let task = required(db.mission(id)?, "Mission not found")?;
                    let reduces = body["enabled"] == false
                        && (body["archived"] == true || !task.settings.archived);
                    if reduces {
                        return Ok(());
                    }
                    require_agent(db, &identity, &task.settings.agent_id.to_string())
                }
                ("POST", ["tasks", id, "run"]) => require_task(db, &identity, id),
                _ => Ok(()),
            }
        })
        .await
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
        // An unclaimed installation has no member to keep the existing agents for.
        return db.delete(ROLLOUT);
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
        let policy = self.missions.observe(self).await?.ok_or_else(|| {
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
                db.0.execute(
                    "INSERT OR IGNORE INTO agent_account_access VALUES(?1,?2,?3,?4)",
                    params![
                        id,
                        member.account_id,
                        member.access_id,
                        crate::config::now()
                    ],
                )?;
                db.audit(
                    "agent.account_access_granted",
                    &json!({ "agentId": id, "accountId": member.account_id }),
                )?;
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
