//! Who writes each entry of a conversation, and the instructions and skills its agent
//! follows (ADR-0075, « Autorité des règles » and « Sans migration »).
//!
//! An agent with installation access writes hook rules only for the person of a
//! conversation that this person alone has written, from its creation, on the web,
//! Android or the public MCP. Every check fails closed: a conversation qualifies only
//! when each of its messages carries a matching attestation of that person, so a write
//! path that forgets to record its writer leaves the conversation ineligible. Anything
//! written before this record existed never qualifies.
use crate::{
    chats::{Conversation, Message},
    config::now,
    error::{Error, Result, required},
    native::policy::Origin,
    service::Service,
    store::Db,
    validation::text,
};
use cairn_protocol::TaskAuthorPolicy;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

pub(crate) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS conversation_writers(
 chat_id TEXT PRIMARY KEY NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
 account_id TEXT CHECK(account_id IS NULL OR length(account_id) BETWEEN 1 AND 100),
 agent_id TEXT NOT NULL,
 plugin_instructions TEXT NOT NULL,
 created_at INTEGER NOT NULL CHECK(created_at BETWEEN 0 AND 9007199254740991),
 reached_at INTEGER CHECK(reached_at BETWEEN 0 AND 9007199254740991),
 reached_by TEXT CHECK(reached_by IN ('person','rule','agent','unknown'))
) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS conversation_entry_writers(
 message_id TEXT PRIMARY KEY NOT NULL REFERENCES chat_messages(id) ON DELETE CASCADE,
 chat_id TEXT NOT NULL,
 digest TEXT NOT NULL CHECK(length(digest)=64),
 writer TEXT NOT NULL CHECK(writer IN ('person','rule','agent','unknown')),
 account_id TEXT,
 rule_id TEXT,
 written_at INTEGER NOT NULL CHECK(written_at BETWEEN 0 AND 9007199254740991)
) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS conversation_entry_writers_chat ON conversation_entry_writers(chat_id);
CREATE TABLE IF NOT EXISTS conversation_turn_entries(
 run_id TEXT NOT NULL REFERENCES runs(id) ON DELETE CASCADE,
 message_id TEXT NOT NULL,
 grant_digest TEXT,
 PRIMARY KEY(run_id,message_id)
) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS agent_instruction_writers(
 key INTEGER PRIMARY KEY,
 agent_id TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
 digest TEXT NOT NULL CHECK(length(digest)=64),
 writer TEXT NOT NULL CHECK(writer IN ('person','rule','agent','unknown')),
 account_id TEXT,
 written_at INTEGER NOT NULL CHECK(written_at BETWEEN 0 AND 9007199254740991)
);
CREATE INDEX IF NOT EXISTS agent_instruction_writers_agent ON agent_instruction_writers(agent_id,key);
CREATE TABLE IF NOT EXISTS skill_writers(
 key INTEGER PRIMARY KEY,
 scope TEXT NOT NULL,
 name TEXT NOT NULL,
 revision TEXT NOT NULL,
 writer TEXT NOT NULL CHECK(writer IN ('person','rule','agent','unknown')),
 account_id TEXT,
 written_at INTEGER NOT NULL CHECK(written_at BETWEEN 0 AND 9007199254740991)
);
CREATE INDEX IF NOT EXISTS skill_writers_name ON skill_writers(name,key);
";

/// Version 33 to 34. Nothing written before has a recorded writer, so nothing is
/// converted: earlier conversations, instructions and skills never qualify.
pub(crate) fn upgrade(connection: &mut Connection) -> Result<()> {
    let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    // Recheck under the write lock: another startup may have completed the upgrade.
    let version: i64 = tx.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if (34..=crate::store::DATABASE_VERSION).contains(&version) {
        return Ok(());
    }
    if version != 33 {
        return Err(Error::bad(
            "Unexpected installation database version during upgrade.",
        ));
    }
    tx.execute_batch(SCHEMA)?;
    tx.pragma_update(None, "user_version", 34)?;
    tx.commit()?;
    Ok(())
}

/// Who wrote an entry of a conversation, an agent's instructions or a skill.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum EntryWriter {
    /// A person, from the web, Android or the public MCP they authorized.
    Person(String),
    /// A hook rule, which acts for its creator but is not that person.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "the rule engine records the conversations rules reach"
        )
    )]
    Rule(String),
    /// An agent, `cairn_installation` or a delegated task included.
    Agent,
    /// Anything Cairn cannot attribute.
    Unknown,
}

impl EntryWriter {
    /// The writer of a request whose native origin is known. Only an authenticated person
    /// is a person; the public MCP of a member carries no person and stays unknown.
    pub(crate) fn of(origin: &Origin) -> Self {
        match origin {
            Origin::Human(account) => Self::Person(account.clone()),
            Origin::Run(_) | Origin::DelegatedTask(_) => Self::Agent,
            Origin::Plugin(_) | Origin::Stored(_) | Origin::Unknown => Self::Unknown,
        }
    }

    /// The writer of a capability call.
    pub(crate) fn of_context(context: &crate::capabilities::ResumeContext) -> Self {
        match context {
            crate::capabilities::ResumeContext::Human(account) => Self::Person(account.clone()),
            // The public MCP acts for the owner who authorized it.
            crate::capabilities::ResumeContext::Public(..) => {
                crate::native::policy::caller().map_or(Self::Unknown, Self::Person)
            }
            crate::capabilities::ResumeContext::Installation(_) => Self::Agent,
        }
    }

    fn columns(&self) -> (&'static str, Option<&str>, Option<&str>) {
        match self {
            Self::Person(account) => ("person", Some(account), None),
            Self::Rule(rule) => ("rule", None, Some(rule)),
            Self::Agent => ("agent", None, None),
            Self::Unknown => ("unknown", None, None),
        }
    }
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// What a person wrote in a message. Delivery state changes it never.
fn entry_digest(message: &Message) -> Result<String> {
    let attachments: Vec<_> = message
        .attachments
        .iter()
        .map(|attachment| json!([attachment.id, attachment.digest]))
        .collect();
    let content = json!({
        "text": message.text,
        "answers": message.answers,
        "attachments": attachments,
        "questionId": message.question_id,
        "mode": message.mode,
        "plan": message.plan,
        "provider": message.provider,
        "model": message.model,
        "reasoning": message.reasoning,
        "speed": message.speed,
    });
    Ok(digest(&serde_json::to_vec(&content)?))
}

/// The plugin instructions an agent follows: which instances grant them, and the
/// package versions those instances run.
fn plugin_instructions(db: &Db<'_>, agent: &str) -> Result<String> {
    let Some(profile) = db.agent(agent)? else {
        return Ok(String::new());
    };
    let mut instances = profile.settings.plugins.clone();
    let selected: Vec<String> = db
        .0
        .prepare_cached("SELECT instance_id FROM plugin_contribution_grants WHERE agent_id=? AND kind='instructions'")?
        .query_map([agent], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    instances.extend(selected);
    instances.sort();
    instances.dedup();
    let mut granted = Vec::new();
    for instance in instances {
        let package: Option<(String, bool, Option<String>, Option<String>)> = db
            .0
            .prepare_cached("SELECT i.plugin_id,i.enabled,p.active_version,v.digest FROM plugin_instances i LEFT JOIN plugin_packages p ON p.id=i.plugin_id LEFT JOIN plugin_versions v ON v.plugin_id=p.id AND v.version=p.active_version WHERE i.id=?")?
            .query_row([&instance], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)))
            .optional()?;
        granted.push(json!([instance, package]));
    }
    if granted.is_empty() {
        return Ok(String::new());
    }
    Ok(digest(&serde_json::to_vec(&granted)?))
}

/// Records who created a conversation. Only a person may later make it eligible.
pub(crate) fn record_creation(
    db: &Db<'_>,
    chat: &Conversation,
    writer: &EntryWriter,
) -> Result<()> {
    let account = match writer {
        EntryWriter::Person(account) => Some(account.as_str()),
        _ => None,
    };
    let reached = account.is_none().then(|| writer.columns().0);
    db.0.prepare_cached(
        "INSERT INTO conversation_writers VALUES(?,?,?,?,?,?,?) ON CONFLICT(chat_id) DO NOTHING",
    )?
    .execute(params![
        chat.id,
        account,
        chat.agent_id,
        plugin_instructions(db, &chat.agent_id)?,
        now(),
        reached.map(|_| now()),
        reached,
    ])?;
    Ok(())
}

/// Marks a conversation as reached by a writer. Anyone other than the person who
/// created it makes it ineligible for good. The rule engine calls this for each
/// conversation a rule starts, steers, answers or otherwise reaches.
pub(crate) fn record_entry(db: &Db<'_>, conversation: &str, writer: &EntryWriter) -> Result<()> {
    let creator: Option<Option<String>> =
        db.0.prepare_cached("SELECT account_id FROM conversation_writers WHERE chat_id=?")?
            .query_row([conversation], |row| row.get(0))
            .optional()?;
    let Some(creator) = creator else {
        // Unrecorded conversations never qualify; there is nothing to mark.
        return Ok(());
    };
    if let (Some(creator), EntryWriter::Person(account)) = (&creator, writer)
        && creator == account
    {
        return Ok(());
    }
    db.0.prepare_cached(
        "UPDATE conversation_writers SET reached_at=?,reached_by=? WHERE chat_id=? AND reached_at IS NULL",
    )?
    .execute(params![now(), writer.columns().0, conversation])?;
    Ok(())
}

/// Attests who wrote a message as it is now stored: sent, edited or answered.
pub(crate) fn record_message(
    db: &Db<'_>,
    conversation: &str,
    message: &str,
    writer: &EntryWriter,
) -> Result<()> {
    let stored = required(
        db.conversation_message(conversation, message)?,
        "Message not found",
    )?;
    let (kind, account, rule) = writer.columns();
    db.0.prepare_cached("INSERT INTO conversation_entry_writers VALUES(?,?,?,?,?,?,?) ON CONFLICT(message_id) DO UPDATE SET chat_id=excluded.chat_id,digest=excluded.digest,writer=excluded.writer,account_id=excluded.account_id,rule_id=excluded.rule_id,written_at=excluded.written_at")?
        .execute(params![message, conversation, entry_digest(&stored)?, kind, account, rule, now()])?;
    record_entry(db, conversation, writer)
}

/// A message starts a turn of the conversation's run.
pub(crate) fn record_turn(db: &Db<'_>, run: &str, message: &str) -> Result<()> {
    db.0.prepare_cached(
        "INSERT INTO conversation_turn_entries VALUES(?,?,NULL) ON CONFLICT(run_id,message_id) DO UPDATE SET grant_digest=NULL",
    )?
    .execute(params![run, message])?;
    Ok(())
}

/// Binds the first launch grant of a turn that a message started. A relaunch of the
/// same turn (a resume or a recovery) gets another grant, which no entry launched.
pub(crate) fn bind_launch(
    db: &Db<'_>,
    grant: &str,
    run: &str,
    message: Option<&str>,
) -> Result<()> {
    let Some(message) = message else {
        return Ok(());
    };
    db.0.prepare_cached(
        "UPDATE conversation_turn_entries SET grant_digest=? WHERE run_id=? AND message_id=? AND grant_digest IS NULL",
    )?
    .execute(params![grant, run, message])?;
    Ok(())
}

/// Records who wrote an agent's instructions when they change. A change that no
/// recorded write explains is attributed to no one first.
pub(crate) fn record_instructions(
    db: &Db<'_>,
    agent: &str,
    before: &str,
    after: &str,
    writer: &EntryWriter,
) -> Result<()> {
    if before == after {
        return Ok(());
    }
    let latest: Option<String> = db
        .0
        .prepare_cached("SELECT digest FROM agent_instruction_writers WHERE agent_id=? ORDER BY key DESC LIMIT 1")?
        .query_row([agent], |row| row.get(0))
        .optional()?;
    let previous = digest(before.as_bytes());
    if latest.as_ref().is_some_and(|latest| *latest != previous) {
        insert_instructions(db, agent, &previous, &EntryWriter::Unknown)?;
    }
    insert_instructions(db, agent, &digest(after.as_bytes()), writer)
}

fn insert_instructions(db: &Db<'_>, agent: &str, digest: &str, writer: &EntryWriter) -> Result<()> {
    let (kind, account, _) = writer.columns();
    db.0.prepare_cached("INSERT INTO agent_instruction_writers(agent_id,digest,writer,account_id,written_at) VALUES(?,?,?,?,?)")?
        .execute(params![agent, digest, kind, account, now()])?;
    Ok(())
}

/// Records who wrote a skill revision through Cairn's skill tools. A skill imported
/// from a source counts as written by whoever imported it.
pub(crate) fn record_skill(
    db: &Db<'_>,
    scope: &str,
    name: &str,
    revision: &str,
    writer: &EntryWriter,
) -> Result<()> {
    let (kind, account, _) = writer.columns();
    db.0.prepare_cached("INSERT INTO skill_writers(scope,name,revision,writer,account_id,written_at) VALUES(?,?,?,?,?,?)")?
        .execute(params![scope, name, revision, kind, account, now()])?;
    Ok(())
}

/// Records the revision a skill tool left on disk.
pub(crate) async fn record_skill_write(
    s: &Service,
    scope: &str,
    name: &str,
    project: Option<&Path>,
    writer: &EntryWriter,
) -> Result<()> {
    let bundle = s.skills.bundle(scope, name, project).await?;
    let revision = text(&bundle, "revision").to_owned();
    let (scope, name, writer) = (scope.to_owned(), name.to_owned(), writer.clone());
    s.store
        .transaction(move |db| record_skill(db, &scope, &name, &revision, &writer))
        .await
}

/// The person an agent writes rules for, in an eligible conversation.
#[derive(Clone, Debug)]
pub(crate) struct Eligible {
    pub account: String,
    pub owner: bool,
    pub access_id: Option<String>,
    pub conversation: String,
}

/// What eligibility needs from outside the database, observed before a write
/// transaction that checks it again.
pub(crate) struct Eligibility {
    policy: Option<TaskAuthorPolicy>,
    global_skills: Option<PathBuf>,
}

impl Eligibility {
    pub(crate) async fn observe(s: &Service) -> Result<Self> {
        let policy = s.missions.observe(s).await?;
        let global_skills = s.skills.root(None).await.ok();
        Ok(Self {
            policy,
            global_skills,
        })
    }

    #[cfg(test)]
    fn of(policy: Option<TaskAuthorPolicy>, global_skills: Option<PathBuf>) -> Self {
        Self {
            policy,
            global_skills,
        }
    }

    /// Whether the run holding this bearer may write rules, and for whom.
    pub(crate) fn check(&self, db: &Db<'_>, bearer: &str) -> Result<Eligible> {
        let run = crate::installation_mcp::authorize_in(db, bearer)?;
        let grant = crate::mcps::grant_key(bearer);
        eligible_run(db, &run, &grant, self)
    }
}

fn refused(reason: &str) -> Error {
    Error::forbidden(format!(
        "An agent writes rules only for the person of a conversation that this person alone has written, from its start. {reason}"
    ))
}

struct InstructionWrite {
    digest: String,
    writer: String,
    account: Option<String>,
    written_at: i64,
}

impl InstructionWrite {
    fn by(&self, account: &str) -> bool {
        self.writer == "person" && self.account.as_deref() == Some(account)
    }
}

fn writes(db: &Db<'_>, sql: &str, key: &str) -> Result<Vec<InstructionWrite>> {
    Ok(db
        .0
        .prepare_cached(sql)?
        .query_map([key], |row| {
            Ok(InstructionWrite {
                digest: row.get(0)?,
                writer: row.get(1)?,
                account: row.get(2)?,
                written_at: row.get(3)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?)
}

/// How a conversation started, and whether another writer reached it since.
struct Creation {
    person: Option<String>,
    agent: String,
    plugins: String,
    created_at: i64,
    reached_at: Option<i64>,
}

fn eligible_run(
    db: &Db<'_>,
    run: &Value,
    grant: &str,
    eligibility: &Eligibility,
) -> Result<Eligible> {
    // Tasks (scheduled, manual or delegated) and missions never qualify.
    let run_id = text(run, "id");
    let chat = db
        .conversation_for_run(run_id)?
        .filter(|chat| run["trigger"] == "chat" && chat.requested_by.is_none())
        .ok_or_else(|| refused("This run is not a conversation."))?;

    let created = db
        .0
        .prepare_cached("SELECT account_id,agent_id,plugin_instructions,created_at,reached_at FROM conversation_writers WHERE chat_id=?")?
        .query_row([&chat.id], |row| {
            Ok(Creation {
                person: row.get(0)?,
                agent: row.get(1)?,
                plugins: row.get(2)?,
                created_at: row.get(3)?,
                reached_at: row.get(4)?,
            })
        })
        .optional()?;
    let Some(Creation {
        person: Some(person),
        agent,
        plugins,
        created_at,
        reached_at,
    }) = created
    else {
        return Err(refused(
            "Cairn did not record that a person started this conversation.",
        ));
    };
    if reached_at.is_some() {
        return Err(refused("Another writer reached this conversation."));
    }
    // A conversation moved to another agent never qualifies again.
    if chat.agent_id != agent || text(&run["snapshot"]["agent"], "id") != agent {
        return Err(refused("This conversation moved to another agent."));
    }

    // Every entry is attested to the person, as it is stored now.
    for message in db.conversation_messages(&chat.id)? {
        let attested: Option<(String, String, Option<String>)> = db
            .0
            .prepare_cached("SELECT digest,writer,account_id FROM conversation_entry_writers WHERE message_id=? AND chat_id=?")?
            .query_row(params![message.id, chat.id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .optional()?;
        let by_person = attested.is_some_and(|(digest, writer, account)| {
            writer == "person"
                && account.as_deref() == Some(person.as_str())
                && entry_digest(&message).is_ok_and(|current| current == digest)
        });
        if !by_person {
            return Err(refused(
                "A message of this conversation was not written by its person.",
            ));
        }
    }

    // The turn was launched by the person's entry, not resumed, woken or recovered.
    let launched_by = run["chatExecution"]["messageId"].as_str().unwrap_or("");
    let bound: Option<Option<String>> =
        db.0.prepare_cached(
            "SELECT grant_digest FROM conversation_turn_entries WHERE run_id=? AND message_id=?",
        )?
        .query_row(params![run_id, launched_by], |row| row.get(0))
        .optional()?;
    let launched = bound.flatten().is_some_and(|bound| bound == grant)
        && run["chatExecution"]["operation"].is_null()
        && db.conversation_message(&chat.id, launched_by)?.is_some();
    if !launched {
        return Err(refused("This person's message did not launch this turn."));
    }

    // The person is still the owner, or a member with access to the agent.
    let policy = eligibility
        .policy
        .as_ref()
        .ok_or_else(|| refused("The installation has no confirmed accounts."))?;
    let membership = policy
        .grant(&person)
        .ok_or_else(|| refused("This person no longer has access to the installation."))?;
    let owner = policy.owner.account_id == person;
    if !owner && !db.member_uses_agent(&person, &agent)? {
        return Err(refused("This person no longer has access to this agent."));
    }

    instructions_written_by(db, run, &agent, &person, created_at)?;
    skills_written_by(db, run, &person, created_at, eligibility)?;
    // Plugin instructions count as written by the owner who granted them.
    if !owner && plugin_instructions(db, &agent)? != plugins {
        return Err(refused(
            "The owner changed this agent's plugin instructions since the conversation started.",
        ));
    }

    Ok(Eligible {
        account: person,
        owner,
        access_id: Some(membership.access_id.clone()),
        conversation: chat.id,
    })
}

/// The agent's instructions were written by a person, never by an agent, and only the
/// person changed them since the conversation started.
fn instructions_written_by(
    db: &Db<'_>,
    run: &Value,
    agent: &str,
    person: &str,
    since: i64,
) -> Result<()> {
    let history = writes(
        db,
        "SELECT digest,writer,account_id,written_at FROM agent_instruction_writers WHERE agent_id=? ORDER BY key",
        agent,
    )?;
    if history
        .iter()
        .any(|write| write.written_at >= since && !write.by(person))
    {
        return Err(refused(
            "Someone else changed this agent's instructions since the conversation started.",
        ));
    }
    let current = db
        .agent(agent)?
        .map(|profile| profile.settings.instructions);
    let followed = text(&run["snapshot"]["agent"], "instructions").to_owned();
    if let (Some(latest), Some(current)) = (history.last(), &current)
        && latest.digest != digest(current.as_bytes())
    {
        return Err(refused(
            "Cairn did not record who wrote this agent's instructions.",
        ));
    }
    for instructions in current.iter().chain([&followed]) {
        if instructions.is_empty() {
            continue;
        }
        let written = digest(instructions.as_bytes());
        let writer = history.iter().rev().find(|write| write.digest == written);
        match writer {
            Some(write) if write.writer == "person" => {}
            Some(_) => {
                return Err(refused("This agent follows instructions an agent wrote."));
            }
            None => {
                return Err(refused(
                    "Cairn did not record who wrote this agent's instructions.",
                ));
            }
        }
    }
    Ok(())
}

/// The skills this turn follows were written by people through Cairn, never by an
/// agent, and only the person changed them since the conversation started. A project
/// skill Cairn never wrote is repository content, an accepted residual risk.
fn skills_written_by(
    db: &Db<'_>,
    run: &Value,
    person: &str,
    since: i64,
    eligibility: &Eligibility,
) -> Result<()> {
    let skills = run["snapshot"]["skills"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    for skill in &skills {
        let name = text(skill, "name");
        let revision = text(skill, "bundle");
        let global = eligibility.global_skills.as_ref().is_none_or(|root| {
            Path::new(text(skill, "path"))
                .parent()
                .and_then(Path::parent)
                == Some(root.as_path())
        });
        let history = writes(
            db,
            "SELECT revision,writer,account_id,written_at FROM skill_writers WHERE name=? ORDER BY key",
            name,
        )?;
        if history
            .iter()
            .any(|write| write.written_at >= since && !write.by(person))
        {
            return Err(refused(&format!(
                "Someone else changed the skill {name} since the conversation started."
            )));
        }
        match history.iter().rev().find(|write| write.digest == revision) {
            Some(write) if write.writer == "person" => {}
            Some(_) => {
                return Err(refused(&format!(
                    "This agent follows the skill {name}, which an agent wrote."
                )));
            }
            None if global || revision.is_empty() => {
                return Err(refused(&format!(
                    "Cairn did not record who wrote the skill {name}."
                )));
            }
            None => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Store;

    const TABLES: [&str; 5] = [
        "agent_instruction_writers",
        "conversation_entry_writers",
        "conversation_turn_entries",
        "conversation_writers",
        "skill_writers",
    ];

    fn tables(connection: &Connection) -> Vec<String> {
        let names = TABLES.map(|name| format!("'{name}'")).join(",");
        connection
            .prepare(&format!(
                "SELECT name FROM sqlite_master WHERE type='table' AND name IN ({names}) ORDER BY name"
            ))
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    }

    #[test]
    fn version_33_upgrades_once_and_refuses_unexpected_versions() {
        let root = tempfile::tempdir().unwrap();
        drop(Store::open(root.path()).unwrap());
        let database = root.path().join("manager.db");
        let connection = Connection::open(&database).unwrap();
        assert_eq!(tables(&connection), TABLES);

        let dropped = TABLES.map(|name| format!("DROP TABLE {name};")).join("");
        connection
            .execute_batch(&format!("{dropped} PRAGMA user_version=33;"))
            .unwrap();
        assert!(tables(&connection).is_empty());
        drop(connection);

        drop(Store::open(root.path()).unwrap());
        let mut connection = Connection::open(&database).unwrap();
        assert_eq!(tables(&connection), TABLES);
        let version: i64 = connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, 34);
        // Reopening, or a concurrent startup, changes nothing.
        upgrade(&mut connection).unwrap();
        drop(Store::open(root.path()).unwrap());
        assert_eq!(tables(&connection), TABLES);

        connection.pragma_update(None, "user_version", 30).unwrap();
        assert!(upgrade(&mut connection).is_err());
    }

    fn conversation(db: &Db<'_>, id: &str, writer: &EntryWriter) -> Conversation {
        db.save_conversation_boundary(&json!({
            "id": id,
            "agentId": crate::config::MAIN_AGENT_ID,
        }))
        .unwrap();
        let chat = db.conversation(id).unwrap().unwrap();
        record_creation(db, &chat, writer).unwrap();
        chat
    }

    fn reached(db: &Db<'_>, id: &str) -> Option<String> {
        db.0.query_row(
            "SELECT reached_by FROM conversation_writers WHERE chat_id=?",
            [id],
            |row| row.get(0),
        )
        .unwrap()
    }

    #[tokio::test]
    async fn only_the_person_who_started_a_conversation_keeps_it_unreached() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path()).unwrap();
        store
            .transaction(|db| {
                let person = EntryWriter::Person("person".into());
                conversation(db, "mine", &person);
                record_entry(db, "mine", &person)?;
                assert_eq!(reached(db, "mine"), None);

                // A rule, an agent or another person reaches it for good.
                for (id, writer) in [
                    ("rule", EntryWriter::Rule("rule-id".into())),
                    ("agent", EntryWriter::Agent),
                    ("other", EntryWriter::Person("other".into())),
                    ("unknown", EntryWriter::Unknown),
                ] {
                    conversation(db, id, &person);
                    record_entry(db, id, &writer)?;
                    record_entry(db, id, &person)?;
                    assert_eq!(reached(db, id).as_deref(), Some(writer.columns().0));
                }

                // Created by anyone but a person, it starts reached.
                conversation(db, "started-by-agent", &EntryWriter::Agent);
                assert_eq!(reached(db, "started-by-agent").as_deref(), Some("agent"));

                // A conversation Cairn never recorded stays unrecorded.
                db.save_conversation_boundary(&json!({
                    "id": "before",
                    "agentId": crate::config::MAIN_AGENT_ID,
                }))?;
                record_entry(db, "before", &EntryWriter::Agent)?;
                let recorded: bool = db.0.query_row(
                    "SELECT EXISTS(SELECT 1 FROM conversation_writers WHERE chat_id='before')",
                    [],
                    |row| row.get(0),
                )?;
                assert!(!recorded);
                Ok(())
            })
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn an_unexplained_instruction_change_is_attributed_to_no_one() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path()).unwrap();
        store
            .transaction(|db| {
                let agent = crate::config::MAIN_AGENT_ID;
                let settings = serde_json::from_value(json!({
                    "name": "Main",
                    "access": cairn_contracts::AccessPolicy::default(),
                    "exclusiveNodes": [],
                    "plugins": [],
                }))?;
                db.save_agent(&crate::agents::new_agent(
                    settings,
                    Some(cairn_contracts::AgentId(uuid::Uuid::parse_str(agent).unwrap())),
                ))?;
                let person = EntryWriter::Person("person".into());
                record_instructions(db, agent, "", "First", &person)?;
                record_instructions(db, agent, "First", "First", &EntryWriter::Agent)?;
                // Something changed "First" into "Second" without a record.
                record_instructions(db, agent, "Second", "Third", &person)?;
                let history = writes(
                    db,
                    "SELECT digest,writer,account_id,written_at FROM agent_instruction_writers WHERE agent_id=? ORDER BY key",
                    agent,
                )?;
                let writers: Vec<_> = history
                    .iter()
                    .map(|write| (write.digest.clone(), write.writer.as_str()))
                    .collect();
                assert_eq!(
                    writers,
                    [
                        (digest(b"First"), "person"),
                        (digest(b"Second"), "unknown"),
                        (digest(b"Third"), "person"),
                    ]
                );
                Ok(())
            })
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn skills_qualify_only_when_people_wrote_them_through_cairn() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path()).unwrap();
        let global = PathBuf::from("/home/cairn/.agents/skills");
        let eligibility = Eligibility::of(None, Some(global.clone()));
        let run = |name: &str, scope: &Path| {
            json!({ "snapshot": { "skills": [{
                "name": name,
                "bundle": format!("{name}-revision"),
                "path": scope.join(name).join("SKILL.md"),
            }] } })
        };
        let project = PathBuf::from("/workspace/project/.agents/skills");
        store
            .transaction(move |db| {
                let since = now();
                let person = EntryWriter::Person("person".into());
                // A global skill must have a recorded writer; a project skill Cairn
                // never wrote is repository content.
                assert!(
                    skills_written_by(db, &run("global", &global), "person", since, &eligibility)
                        .is_err()
                );
                assert!(
                    skills_written_by(
                        db,
                        &run("repository", &project),
                        "person",
                        since,
                        &eligibility
                    )
                    .is_ok()
                );

                record_skill(db, "global", "global", "global-revision", &person)?;
                assert!(
                    skills_written_by(db, &run("global", &global), "person", since, &eligibility)
                        .is_ok()
                );

                // Written by an agent, at any time, whatever its scope.
                record_skill(
                    db,
                    "project",
                    "written",
                    "written-revision",
                    &EntryWriter::Agent,
                )?;
                assert!(
                    skills_written_by(db, &run("written", &project), "person", 0, &eligibility)
                        .is_err()
                );

                // Changed by someone else since the conversation started.
                record_skill(
                    db,
                    "global",
                    "global",
                    "global-revision",
                    &EntryWriter::Person("other".into()),
                )?;
                assert!(
                    skills_written_by(db, &run("global", &global), "person", since, &eligibility)
                        .is_err()
                );
                Ok(())
            })
            .await
            .unwrap();
    }
}
