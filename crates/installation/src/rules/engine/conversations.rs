//! Conversation keys and the conversation actions of hook rules (ADR-0075, « Clé de
//! conversation », « Valeurs mémorisées », « Conversations démarrées » and « Autorité
//! des règles »).
//!
//! A key belongs to the creator of the rule that bound it: the same key of two creators
//! names two conversations. Every action reaches an existing conversation only by its
//! key, needs its creator's current access to that conversation's agent, and runs in
//! the transaction that records it, so no conversation effect is ever ambiguous. A
//! conversation a rule started records its creator and the external origin; agents
//! never write rules there.
use super::*;
use crate::{
    chats::Conversation, native::policy::Origin, rules::validation::Creator, run_status::RunStatus,
    validation::text,
};
use cairn_contracts::{AgentId, QuestionAnswer, QuestionAnswerInput, QuestionStatus};

pub(crate) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS hook_rule_conversations(
 chat_id TEXT PRIMARY KEY NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
 creator_account_id TEXT NOT NULL CHECK(length(creator_account_id) BETWEEN 1 AND 100),
 key TEXT CHECK(key IS NULL OR length(key) BETWEEN 1 AND 200),
 agent_id TEXT NOT NULL,
 rule_id TEXT NOT NULL,
 author TEXT CHECK(author IS NULL OR length(author) BETWEEN 1 AND 200),
 origin TEXT NOT NULL CHECK(json_valid(origin) AND json_type(origin)='object'),
 run_id TEXT,
 cursor INTEGER NOT NULL DEFAULT 0 CHECK(cursor>=0),
 watch TEXT NOT NULL DEFAULT '{}' CHECK(json_valid(watch) AND json_type(watch)='object' AND length(watch)<=65536),
 cancelled_run TEXT,
 created_at INTEGER NOT NULL CHECK(created_at BETWEEN 0 AND 9007199254740991),
 UNIQUE(creator_account_id,key)
) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS hook_rule_conversation_values(
 chat_id TEXT NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
 name TEXT NOT NULL CHECK(length(name) BETWEEN 1 AND 40),
 value TEXT NOT NULL CHECK(json_valid(value) AND length(CAST(value AS BLOB))<=65536),
 updated_at INTEGER NOT NULL CHECK(updated_at BETWEEN 0 AND 9007199254740991),
 PRIMARY KEY(chat_id,name)
) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS hook_rule_turn_origins(
 message_id TEXT PRIMARY KEY NOT NULL,
 chat_id TEXT NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
 origin TEXT NOT NULL CHECK(json_valid(origin) AND json_type(origin)='object'),
 depth INTEGER NOT NULL CHECK(depth BETWEEN 0 AND 8)
) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS hook_rule_turn_origins_chat ON hook_rule_turn_origins(chat_id);
CREATE TABLE IF NOT EXISTS hook_rule_chain_depths(
 chat_id TEXT PRIMARY KEY NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
 depth INTEGER NOT NULL CHECK(depth BETWEEN 0 AND 8)
) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS hook_rule_task_depths(
 task_id TEXT PRIMARY KEY NOT NULL REFERENCES missions(id) ON DELETE CASCADE,
 depth INTEGER NOT NULL CHECK(depth BETWEEN 0 AND 8)
) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS hook_rule_run_depths(
 run_id TEXT PRIMARY KEY NOT NULL REFERENCES runs(id) ON DELETE CASCADE,
 depth INTEGER NOT NULL CHECK(depth BETWEEN 0 AND 8)
) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS hook_rule_reported_questions(
 question_id TEXT PRIMARY KEY NOT NULL REFERENCES chat_questions(id) ON DELETE CASCADE
) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS hook_rule_author_triggerings(
 rule_id TEXT NOT NULL REFERENCES hook_rules(id) ON DELETE CASCADE,
 author TEXT NOT NULL CHECK(length(author) BETWEEN 1 AND 200),
 triggering_id TEXT NOT NULL,
 at INTEGER NOT NULL CHECK(at BETWEEN 0 AND 9007199254740991),
 PRIMARY KEY(rule_id,author,triggering_id)
) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS hook_rule_author_triggerings_at ON hook_rule_author_triggerings(at);
CREATE TABLE IF NOT EXISTS hook_rule_conversation_counts(
 rule_id TEXT NOT NULL REFERENCES hook_rules(id) ON DELETE CASCADE,
 triggering_id TEXT NOT NULL,
 position INTEGER NOT NULL CHECK(position>=0),
 counted_at INTEGER NOT NULL CHECK(counted_at BETWEEN 0 AND 9007199254740991),
 PRIMARY KEY(rule_id,triggering_id,position)
) WITHOUT ROWID;
";

/// Version 34 to 35. No conversation is bound to a key before.
pub(crate) fn upgrade(connection: &mut Connection) -> Result<()> {
    let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let version: i64 = tx.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if (35..=crate::store::DATABASE_VERSION).contains(&version) {
        return Ok(());
    }
    if version != 34 {
        return Err(Error::bad(
            "Unexpected installation database version during upgrade.",
        ));
    }
    tx.execute_batch(SCHEMA)?;
    tx.pragma_update(None, "user_version", 35)?;
    tx.commit()?;
    Ok(())
}

/// Links Cairn follows in one chain: a rule's conversation action causes a Cairn hook
/// that triggers a rule, whose conversation action causes another, and so on. A plugin
/// event starts a chain at zero; a person's input starts a new one.
pub(crate) const MAX_CHAIN_DEPTH: u32 = 8;
/// Bytes of the values rules remember on one conversation.
pub(crate) const MAX_CONVERSATION_VALUES_BYTES: usize = 256 * 1024;
/// A conversation key, an author and a remembered name are bounded texts.
const MAX_KEY_CHARS: usize = 200;
/// Distinct authors one triggering's quota conditions may record.
const MAX_QUOTA_AUTHORS: usize = 20;

/// How deep in a chain a triggering stands. Only Cairn writes the `cairn` field of a
/// triggering's event; a plugin's payload lives under `payload` and cannot set it.
pub(super) fn chain_depth(event: &Json) -> u32 {
    event["cairn"]["depth"]
        .as_u64()
        .map_or(0, |depth| u32::try_from(depth).unwrap_or(u32::MAX))
}

/// A conversation key evaluated from a rule value.
pub(crate) fn key(value: &Json) -> std::result::Result<String, String> {
    match value.as_str() {
        Some(key)
            if !key.is_empty()
                && key.chars().count() <= MAX_KEY_CHARS
                && !key.chars().any(char::is_control) =>
        {
            Ok(key.to_owned())
        }
        _ => Err(format!(
            "A conversation key is text of 1 to {MAX_KEY_CHARS} characters."
        )),
    }
}

/// An external author: text, or a number read from the payload. Null means none.
pub(crate) fn author(value: &Json) -> std::result::Result<Option<String>, String> {
    let author = match value {
        Json::Null => return Ok(None),
        Json::String(author) => author.clone(),
        Json::Number(number) => number.to_string(),
        _ => return Err("An author is text or a number.".into()),
    };
    if author.is_empty()
        || author.chars().count() > MAX_KEY_CHARS
        || author.chars().any(char::is_control)
    {
        return Err(format!(
            "An author is text of 1 to {MAX_KEY_CHARS} characters."
        ));
    }
    Ok(Some(author))
}

/// The conversation bound to a key of a creator, while it can be reached: not deleted,
/// and still with the agent it was started with (a merge detaches it).
struct Bound {
    chat: String,
    agent: String,
    author: Option<String>,
    cancelled_run: Option<String>,
}

fn bound(db: &Db<'_>, creator: &str, key: &str) -> Result<Option<Bound>> {
    Ok(db
        .0
        .prepare_cached(
            "SELECT k.chat_id,k.agent_id,k.author,k.cancelled_run FROM hook_rule_conversations k \
             JOIN conversations c ON c.id=k.chat_id WHERE k.creator_account_id=?1 AND k.key=?2 \
             AND c.lifecycle='active' AND c.agent_id=k.agent_id",
        )?
        .query_row(params![creator, key], |row| {
            Ok(Bound {
                chat: row.get(0)?,
                agent: row.get(1)?,
                author: row.get(2)?,
                cancelled_run: row.get(3)?,
            })
        })
        .optional()?)
}

/// What a predicate about a key reads: whether a conversation is bound, and who
/// started it.
pub(super) fn bound_author(
    db: &Db<'_>,
    creator: &str,
    key: &str,
) -> Result<Option<Option<String>>> {
    Ok(bound(db, creator, key)?.map(|bound| bound.author))
}

/// How many other triggerings of a rule an author caused within a window.
pub(super) fn author_triggerings(
    db: &Db<'_>,
    rule: &str,
    author: &str,
    since: i64,
    except: &str,
) -> Result<u32> {
    let count: i64 = db.0.query_row(
        "SELECT count(*) FROM hook_rule_author_triggerings \
         WHERE rule_id=?1 AND author=?2 AND at>?3 AND triggering_id<>?4",
        params![rule, author, since, except],
        |row| row.get(0),
    )?;
    Ok(u32::try_from(count).unwrap_or(u32::MAX))
}

/// Attributes an acting triggering to the authors its quota conditions examined.
pub(super) fn record_authors(db: &Db<'_>, t: &Triggering) -> Result<()> {
    if t.cursor.authors.len() > MAX_QUOTA_AUTHORS {
        return Err(Error::bad(format!(
            "Quota conditions examined more than {MAX_QUOTA_AUTHORS} authors in one triggering."
        )));
    }
    for author in &t.cursor.authors {
        db.0.prepare_cached(
            "INSERT OR IGNORE INTO hook_rule_author_triggerings VALUES(?1,?2,?3,?4)",
        )?
        .execute(params![t.rule_id, author, t.id, now()])?;
    }
    Ok(())
}

/// Who wrote an entry, as Cairn hooks report it.
pub(super) fn origin_of(origin: &Origin) -> Json {
    match origin {
        Origin::Human(account) | Origin::PublicAccount(account) => {
            json!({ "kind": "person", "accountId": account })
        }
        // `cairn_installation` acts within its run; delegated tasks are agents.
        Origin::Run(_) => json!({ "kind": "installation" }),
        Origin::DelegatedTask(_) => json!({ "kind": "agent" }),
        Origin::Plugin(_) | Origin::Stored(_) | Origin::Unknown => json!({ "kind": "unknown" }),
    }
}

/// Records who wrote an entry of a rule's conversation, so that the Cairn hooks
/// of the turn it starts report it. Unkeyed rule conversations retain their depth
/// too: their agent can write to another conversation through installation access.
///
/// Only a person's entry starts a new chain, and takes back from rules a conversation
/// a rule cancelled. Any other entry stays in the chain of the conversation, and of the
/// run that caused it: an agent's delegated task, or `cairn_installation`.
pub(crate) fn record_input(db: &Db<'_>, chat: &str, message: &str, origin: &Origin) -> Result<()> {
    let person = matches!(origin, Origin::Human(_) | Origin::PublicAccount(_));
    let causing = source_depth(db, origin)?;
    let (depth, tracked): (u32, bool) = db.0.query_row(
        "SELECT CASE WHEN ?2 THEN 0 ELSE max( \
           coalesce((SELECT depth FROM hook_rule_chain_depths WHERE chat_id=?1),0), \
           coalesce((SELECT json_extract(watch,'$.depth') FROM hook_rule_conversations WHERE chat_id=?1),0), \
           ?3) END, \
         EXISTS(SELECT 1 FROM hook_rule_chain_depths WHERE chat_id=?1) OR \
         EXISTS(SELECT 1 FROM hook_rule_conversations WHERE chat_id=?1)",
        params![chat,person,causing], |row| Ok((row.get(0)?,row.get(1)?)),
    )?;
    // Ordinary conversations outside a rule chain need no rule metadata. A
    // delegated or installation-created conversation inherits the source depth
    // even if it has no rule key and consequently no asynchronous watcher.
    if tracked || depth > 0 {
        record_depth(db, chat, depth)?;
        db.0.execute(
            "INSERT INTO hook_rule_turn_origins(message_id,chat_id,origin,depth) VALUES(?1,?2,?3,?4) \
             ON CONFLICT(message_id) DO UPDATE SET origin=excluded.origin,depth=excluded.depth",
            params![message,chat,origin_of(origin).to_string(),depth],
        )?;
    }
    if person {
        release_pause(db, chat)?;
    }
    Ok(())
}

/// The source's depth is independent of native authority and of whether its run
/// belongs to a conversation. Mission admissions freeze it before the caller can
/// accept another person's input or finish its own conversation.
fn source_depth(db: &Db<'_>, origin: &Origin) -> Result<u32> {
    match origin {
        Origin::Run(run) | Origin::DelegatedTask(run) => Ok(db.0.query_row(
            "SELECT max( \
               coalesce((SELECT depth FROM hook_rule_run_depths WHERE run_id=?1),0), \
               coalesce((SELECT max(max(coalesce(d.depth,0),coalesce(json_extract(k.watch,'$.depth'),0))) \
                 FROM conversations c LEFT JOIN hook_rule_chain_depths d ON d.chat_id=c.id \
                 LEFT JOIN hook_rule_conversations k ON k.chat_id=c.id WHERE c.run_id=?1),0))",
            [run], |row| row.get(0),
        )?),
        Origin::Stored(task) => task_depth(db, text(task,"id")),
        _ => Ok(0),
    }
}

fn task_depth(db: &Db<'_>, task: &str) -> Result<u32> {
    Ok(db.0.query_row(
        "SELECT coalesce((SELECT depth FROM hook_rule_task_depths WHERE task_id=?1),0)",
        [task],
        |row| row.get(0),
    )?)
}

/// Agent-created or edited scheduled work retains its causal chain for later
/// admissions, even if its caller no longer has a current conversation turn.
pub(crate) fn record_task(db: &Db<'_>, task: &str, origin: &Origin) -> Result<()> {
    if matches!(origin, Origin::Human(_) | Origin::PublicAccount(_)) {
        db.0.execute("DELETE FROM hook_rule_task_depths WHERE task_id=?", [task])?;
        return Ok(());
    }
    let depth = source_depth(db, origin)?.max(task_depth(db, task)?);
    if depth > 0 {
        db.0.execute(
            "INSERT INTO hook_rule_task_depths(task_id,depth) VALUES(?1,?2) \
             ON CONFLICT(task_id) DO UPDATE SET depth=excluded.depth",
            params![task, depth],
        )?;
    }
    Ok(())
}

/// Freeze the cause of a mission run in its admission transaction. Repeated
/// admissions of an existing run can narrow neither its original chain nor a
/// chain retained with the scheduled task.
pub(crate) fn record_run(db: &Db<'_>, run: &Json, origin: &Origin) -> Result<()> {
    if run["trigger"] == "chat" {
        return Ok(());
    }
    let person = matches!(origin, Origin::Human(_) | Origin::PublicAccount(_));
    let depth = if person {
        0
    } else {
        source_depth(db, origin)?.max(task_depth(db, text(run, "taskId"))?)
    };
    if depth > 0 {
        db.0.execute(
            "INSERT INTO hook_rule_run_depths(run_id,depth) VALUES(?1,?2) \
             ON CONFLICT(run_id) DO UPDATE SET depth=max(depth,excluded.depth)",
            params![text(run, "id"), depth],
        )?;
    }
    Ok(())
}

/// The watcher may not have observed a queued message yet. Keep the input
/// frontier in its write transaction, independently of event observation.
fn record_depth(db: &Db<'_>, chat: &str, depth: u32) -> Result<()> {
    db.0.execute(
        "INSERT INTO hook_rule_chain_depths(chat_id,depth) VALUES(?1,?2) \
         ON CONFLICT(chat_id) DO UPDATE SET depth=excluded.depth",
        params![chat, depth],
    )?;
    Ok(())
}

/// A person paused, resumed, stopped or wrote: a rule's cancellation no longer accounts
/// for the conversation's pause.
pub(crate) fn release_pause(db: &Db<'_>, chat: &str) -> Result<()> {
    db.0.prepare_cached(
        "UPDATE hook_rule_conversations SET cancelled_run=NULL WHERE chat_id=? AND cancelled_run IS NOT NULL",
    )?
    .execute([chat])?;
    Ok(())
}

/// A rule wrote this entry, one link further in its chain.
fn record_rule_input(
    db: &Db<'_>,
    chat: &str,
    message: &str,
    rule: &RuleRow,
    depth: u32,
) -> Result<()> {
    crate::conversation_writers::record_message(
        db,
        chat,
        message,
        &crate::conversation_writers::EntryWriter::Rule(rule.id.clone()),
    )?;
    let origin = json!({ "kind": "rule", "ruleId": rule.id, "accountId": rule.creator_account_id });
    db.0.prepare_cached(
        "INSERT INTO hook_rule_turn_origins(message_id,chat_id,origin,depth) VALUES(?1,?2,?3,?4) \
         ON CONFLICT(message_id) DO UPDATE SET origin=excluded.origin,depth=excluded.depth",
    )?
    .execute(params![message, chat, origin.to_string(), depth])?;
    record_depth(db, chat, depth)?;
    Ok(())
}

/// A merge moves conversations to another agent: their keys no longer reach them, and
/// what rules remembered on them goes with the keys.
pub(crate) fn detach(db: &Db<'_>, chat: &str) -> Result<()> {
    db.0.execute(
        "UPDATE hook_rule_conversations SET key=NULL,cancelled_run=NULL,watch='{}' WHERE chat_id=?",
        [chat],
    )?;
    db.0.execute(
        "DELETE FROM hook_rule_conversation_values WHERE chat_id=?",
        [chat],
    )?;
    db.0.execute("DELETE FROM hook_rule_turn_origins WHERE chat_id=?", [chat])?;
    Ok(())
}

/// The agents of the conversations bound to a creator's keys, which a rule that steers
/// or answers by key may direct.
pub(crate) fn keyed_agents(db: &Db<'_>, creator: &str) -> Result<Vec<String>> {
    Ok(db
        .0
        .prepare_cached(
            "SELECT DISTINCT k.agent_id FROM hook_rule_conversations k JOIN conversations c ON c.id=k.chat_id \
             WHERE k.creator_account_id=? AND k.key IS NOT NULL AND c.lifecycle='active' ORDER BY k.agent_id",
        )?
        .query_map([creator], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?)
}

/// The values rules remembered on a conversation.
pub(super) fn values(db: &Db<'_>, chat: &str) -> Result<Map<String, Json>> {
    let mut values = Map::new();
    let mut statement = db.0.prepare_cached(
        "SELECT name,value FROM hook_rule_conversation_values WHERE chat_id=? ORDER BY name",
    )?;
    for row in statement.query_map([chat], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })? {
        let (name, value) = row?;
        values.insert(name, serde_json::from_str(&value)?);
    }
    Ok(values)
}

/// The native authority a rule's action carries: the authority saved with its version,
/// narrowed to the nodes where its creator still holds native authority (ADR-0050).
fn native_origin(db: &Db<'_>, row: &RuleRow, version: &VersionRow) -> Result<Origin> {
    let mut grant = version.native_grant.clone();
    if grant["nativeProvenanceUnknown"] != false {
        return Ok(Origin::Stored(json!({ "nativeProvenanceUnknown": true })));
    }
    let current =
        crate::native::policy::provenance(db, &Origin::Human(row.creator_account_id.clone()))?;
    let held = current["nativeAuthorityNodes"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let nodes: Vec<Json> = grant["nativeAuthorityNodes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|node| held.contains(node))
        .cloned()
        .collect();
    grant["nativeAuthorityNodes"] = nodes.into();
    Ok(Origin::Stored(grant))
}

/// An action that resolves a native agent or conversation needs both the authority
/// saved with the rule's version and its creator's current native authority.
fn require_native(db: &Db<'_>, version: &VersionRow, agent: &Json, origin: &Origin) -> Result<()> {
    if !crate::native::policy::native(agent) {
        return Ok(());
    }
    if !version.native_authority() {
        return Err(Error::forbidden(
            "This rule version was saved without native authority, so it cannot reach a native agent or conversation. Its creator can save it again from the app.",
        ));
    }
    crate::native::policy::authorize_origin(db, agent, origin)
        .map(|_| ())
        .map_err(|_| {
            Error::forbidden(
                "Its creator no longer holds native authority for this agent, so the rule cannot reach it.",
            )
        })
}

/// The agent a conversation runs as: an automatic profile follows the environment its
/// run was placed in, as native policy does.
fn conversation_agent(db: &Db<'_>, chat: &Conversation) -> Result<Json> {
    let mut agent = db.agent_snapshot(&chat.agent_id)?.unwrap_or_default();
    let run = chat
        .run_id
        .as_deref()
        .map(|run| db.run(run))
        .transpose()?
        .flatten();
    if agent["access"]["backend"] == "auto"
        && let Some(backend @ ("native" | "firecracker")) = run
            .as_ref()
            .and_then(|run| run["snapshot"]["agent"]["access"]["backend"].as_str())
    {
        agent["access"]["backend"] = backend.into();
    }
    Ok(agent)
}

/// Everything an action on a key-bound conversation needs, checked in its transaction.
struct Reached {
    bound: Bound,
    chat: Conversation,
    origin: Origin,
}

fn reach(db: &Db<'_>, row: &RuleRow, version: &VersionRow, key: &str) -> Result<Reached> {
    let bound = bound(db, &row.creator_account_id, key)?
        .ok_or_else(|| Error::not_found(format!("No conversation is bound to the key {key}.")))?;
    let creator = Creator {
        account: row.creator_account_id.clone(),
        owner: row.creator_owner,
    };
    if !creator.uses_agent(db, &bound.agent)? {
        return Err(Error::forbidden(
            "Its creator no longer has access to the agent of this conversation.",
        ));
    }
    let chat = crate::chats::conversation(db, &bound.chat)?;
    crate::conversation_writers::record_entry(
        db,
        &chat.id,
        &crate::conversation_writers::EntryWriter::Rule(row.id.clone()),
    )?;
    let origin = native_origin(db, row, version)?;
    require_native(db, version, &conversation_agent(db, &chat)?, &origin)?;
    Ok(Reached {
        bound,
        chat,
        origin,
    })
}

fn require_depth(depth: u32) -> Result<()> {
    if depth > MAX_CHAIN_DEPTH {
        return Err(Error::conflict(format!(
            "This chain of rules reached its maximum depth: Cairn follows at most {MAX_CHAIN_DEPTH} conversation actions caused by its own hooks."
        )));
    }
    Ok(())
}

/// What one conversation action needs from its triggering.
pub(super) struct Acting<'a> {
    pub row: &'a RuleRow,
    pub version: &'a VersionRow,
    pub event: &'a Json,
    /// The chain depth of the hooks this action causes.
    pub depth: u32,
    /// Runs this process executes, read under the worker's lock when cancelling.
    pub executing: &'a HashSet<String>,
}

fn text_input(input: &Json, field: &str, what: &str) -> Result<String> {
    let value = input[field]
        .as_str()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| Error::bad(format!("The {what} is empty.")))?;
    if value.chars().count() > 50_000 {
        return Err(Error::bad(format!(
            "The {what} has more than 50,000 characters."
        )));
    }
    Ok(value.to_owned())
}

/// Starts a conversation for the creator, optionally bound to a key that must be free.
fn start(
    db: &Db<'_>,
    acting: &Acting<'_>,
    agent: &AgentId,
    project: Option<&cairn_contracts::ProjectId>,
    input: &Json,
) -> Result<Json> {
    require_depth(acting.depth)?;
    let (row, version) = (acting.row, acting.version);
    let agent = agent.to_string();
    let creator = Creator {
        account: row.creator_account_id.clone(),
        owner: row.creator_owner,
    };
    if !creator.uses_agent(db, &agent)? {
        return Err(Error::forbidden(
            "Its creator no longer has access to the agent this action starts.",
        ));
    }
    let profile = required(db.agent_snapshot(&agent)?, "The agent no longer exists.")?;
    let origin = native_origin(db, row, version)?;
    require_native(db, version, &profile, &origin)?;
    let prompt = text_input(input, "prompt", "prompt")?;
    let key = input["key"].as_str().map(str::to_owned);
    if let Some(key) = &key {
        if bound(db, &row.creator_account_id, key)?.is_some() {
            return Err(Error::conflict(format!(
                "The key {key} is already bound to a conversation. Steer it instead."
            )));
        }
        // A key whose conversation was deleted or merged away is free again.
        db.0.execute(
            "UPDATE hook_rule_conversations SET key=NULL WHERE creator_account_id=?1 AND key=?2",
            params![row.creator_account_id, key],
        )?;
    }
    let author = input["author"].as_str().map(str::to_owned);

    let chat = crate::chats::create_as_writer(
        db,
        json!({ "agentId": agent, "projectId": project }),
        &origin,
        &crate::conversation_writers::EntryWriter::Rule(row.id.clone()),
    )?;
    let chat = text(&chat, "id").to_owned();
    let external = json!({
        "hook": acting.event["hook"],
        "instanceId": acting.event["instanceId"],
        "eventId": acting.event["id"],
        "author": author,
    });
    db.0.execute(
        "INSERT INTO hook_rule_conversations(chat_id,creator_account_id,key,agent_id,rule_id,author,origin,created_at) \
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
        params![chat, row.creator_account_id, key, agent, row.id, author, external.to_string(), now()],
    )?;
    let message = id();
    crate::chats::send_as_in(db, &chat, json!({ "id": message, "text": prompt }), &origin)?;
    record_rule_input(db, &chat, &message, row, acting.depth)?;
    db.audit(
        "hook_rule.conversation_started",
        &json!({ "ruleId": row.id, "chatId": chat, "key": key }),
    )?;
    Ok(json!({ "conversationId": chat, "messageId": message, "key": key }))
}

/// Sends a message to the conversation of a key, which Cairn delivers after its current
/// turn, as for a person who writes again. A paused conversation stays paused, unless
/// the rule's own cancellation paused it.
fn steer(db: &Db<'_>, acting: &Acting<'_>, input: &Json) -> Result<Json> {
    require_depth(acting.depth)?;
    let key = text_input(input, "key", "conversation key")?;
    let message = text_input(input, "message", "message")?;
    let Reached {
        bound,
        chat,
        origin,
    } = reach(db, acting.row, acting.version, &key)?;
    let run = chat
        .run_id
        .as_deref()
        .map(|run| db.run(run))
        .transpose()?
        .flatten();
    let status = run.as_ref().and_then(RunStatus::of);
    let run_id = run.as_ref().map(|run| text(run, "id").to_owned());
    // A rule's cancellation pauses the conversation like Stop. Any later pause,
    // resumption or entry of a person releases that mark: only the rule's own pause
    // lets it steer a paused conversation.
    let cancelled_by_rule = bound.cancelled_run.is_some() && bound.cancelled_run == run_id;
    if chat.paused && !cancelled_by_rule {
        return Err(Error::conflict(
            "This conversation is paused. Rules can steer it again once a person resumes it.",
        ));
    }
    if cancelled_by_rule && status.is_some_and(RunStatus::is_active) {
        return Err(Error::unavailable(
            "This conversation is still stopping. The rule can steer it once it stopped.",
        ));
    }
    if chat.paused {
        crate::chats::pause_in(db, &chat.id, false)?;
    }
    let id = id();
    crate::chats::send_as_in(db, &chat.id, json!({ "id": id, "text": message }), &origin)?;
    record_rule_input(db, &chat.id, &id, acting.row, acting.depth)?;
    db.0.execute(
        "UPDATE hook_rule_conversations SET cancelled_run=NULL WHERE chat_id=?",
        [&chat.id],
    )?;
    Ok(json!({ "conversationId": chat.id, "messageId": id }))
}

/// Stops the current turn of the conversation of a key, like Stop in Cairn. Its end
/// hook belongs to the rule's chain.
fn cancel(db: &Db<'_>, acting: &Acting<'_>, input: &Json) -> Result<Json> {
    require_depth(acting.depth)?;
    let key = text_input(input, "key", "conversation key")?;
    let Reached { bound, chat, .. } = reach(db, acting.row, acting.version, &key)?;
    let active = chat
        .run_id
        .as_deref()
        .map(|run| db.run_state(run))
        .transpose()?
        .flatten()
        .is_some_and(|run| run.status.is_active());
    // Only a pause the rules' own cancellation made lets them steer again: a person's
    // pause or Stop, even one still stopping the turn, stays theirs.
    let rule_owns_pause =
        !chat.paused || (bound.cancelled_run.is_some() && bound.cancelled_run == chat.run_id);
    let Some(run) = chat.run_id.as_deref().filter(|_| active) else {
        // Nothing is working: only a scheduled continuation remains to cancel, and the
        // conversation stays open to later steering.
        if let Some(run) = chat.run_id.as_deref() {
            crate::chats::continuation::cancel_for_run(db, run)?;
        }
        return Ok(json!({ "conversationId": chat.id, "cancelled": false }));
    };
    crate::worker::control::stop_chat_in(db, &chat.id, acting.executing)?;
    db.0.execute(
        "UPDATE hook_rule_conversations SET cancelled_run=CASE WHEN ?4 THEN ?2 END,\
         watch=json_set(watch,'$.depth',max(coalesce(json_extract(watch,'$.depth'),0),?3)) WHERE chat_id=?1",
        params![chat.id, run, acting.depth, rule_owns_pause],
    )?;
    Ok(json!({ "conversationId": chat.id, "runId": run, "cancelled": true }))
}

/// Builds the answer to every field of a question from a text, or from an object by
/// field identifier whose entries are texts or `{selected, comment}`.
fn answers(
    question: &cairn_contracts::ChatQuestion,
    answer: &Json,
) -> Result<std::collections::BTreeMap<String, QuestionAnswer>> {
    let mut answers = std::collections::BTreeMap::new();
    for field in &question.fields {
        let value = match answer {
            Json::String(_) => answer,
            Json::Object(fields) => fields.get(&field.id).ok_or_else(|| {
                Error::bad(format!(
                    "The answer has no entry for the field {}.",
                    field.id
                ))
            })?,
            _ => return Err(Error::bad("An answer is a text, or an object by field.")),
        };
        let answer = match value {
            Json::String(comment) => QuestionAnswer {
                selected: Vec::new(),
                comment: comment.clone(),
            },
            Json::Object(_) => serde_json::from_value(value.clone()).map_err(|_| {
                Error::bad(format!(
                    "The answer to {} is a text or {{selected, comment}}.",
                    field.id
                ))
            })?,
            _ => {
                return Err(Error::bad(format!(
                    "The answer to {} is a text or {{selected, comment}}.",
                    field.id
                )));
            }
        };
        answers.insert(field.id.clone(), answer);
    }
    Ok(answers)
}

/// Answers the oldest pending question of the conversation of a key.
fn answer(db: &Db<'_>, acting: &Acting<'_>, input: &Json) -> Result<Json> {
    require_depth(acting.depth)?;
    let key = text_input(input, "key", "conversation key")?;
    let Reached { chat, origin, .. } = reach(db, acting.row, acting.version, &key)?;
    if chat.paused {
        return Err(Error::conflict(
            "This conversation is paused. Rules can answer it again once a person resumes it.",
        ));
    }
    let question = db
        .questions(&chat.id)?
        .into_iter()
        .filter(|question| {
            question.status == QuestionStatus::Pending
                && chat.run_id.as_deref() == Some(question.run_id.as_str())
        })
        .min_by_key(|question| (question.created_at, question.id.clone()))
        .ok_or_else(|| Error::not_found("The conversation has no pending question."))?;
    let mut reply = QuestionAnswerInput {
        id: id(),
        answers: answers(&question, &input["answer"])?,
    };
    reply.validate().map_err(Error::bad)?;
    crate::chats::questions::answer(db, &chat.id, &question.id, &reply, &origin)?;
    record_rule_input(db, &chat.id, &reply.id, acting.row, acting.depth)?;
    Ok(json!({ "conversationId": chat.id, "questionId": question.id, "messageId": reply.id }))
}

/// Remembers a value on the conversation of a key, shared by the rules that reach it.
fn store(db: &Db<'_>, acting: &Acting<'_>, name: &str, input: &Json) -> Result<Json> {
    let key = text_input(input, "key", "conversation key")?;
    let Reached { chat, .. } = reach(db, acting.row, acting.version, &key)?;
    let value = input["value"].clone();
    let encoded = value.to_string();
    if encoded.len() > 65_536 {
        return Err(Error::bad("A remembered value has at most 64 KiB."));
    }
    let others: i64 = db.0.query_row(
        "SELECT coalesce(sum(length(CAST(value AS BLOB))),0) FROM hook_rule_conversation_values WHERE chat_id=?1 AND name<>?2",
        params![chat.id, name],
        |row| row.get(0),
    )?;
    if others as usize + encoded.len() > MAX_CONVERSATION_VALUES_BYTES {
        return Err(Error::bad(
            "The values remembered on this conversation exceed 256 KiB.",
        ));
    }
    db.0.prepare_cached(
        "INSERT INTO hook_rule_conversation_values VALUES(?1,?2,?3,?4) \
         ON CONFLICT(chat_id,name) DO UPDATE SET value=excluded.value,updated_at=excluded.updated_at",
    )?
    .execute(params![chat.id, name, encoded, now()])?;
    Ok(value)
}

/// Runs one conversation action in the caller's transaction.
pub(super) fn effect(
    db: &Db<'_>,
    acting: &Acting<'_>,
    action: &RuleAction,
    input: &Json,
) -> Result<Json> {
    match action {
        RuleAction::StartAgent {
            agent_id,
            project_id,
            ..
        } => start(db, acting, agent_id, project_id.as_ref(), input),
        RuleAction::SteerConversation { .. } => steer(db, acting, input),
        RuleAction::CancelConversation { .. } => cancel(db, acting, input),
        RuleAction::AnswerQuestion { .. } => answer(db, acting, input),
        RuleAction::StoreValue { name, .. } => store(db, acting, name, input),
        _ => Err(Error::internal("Not a conversation action.")),
    }
}

/// Whether an action starts or steers a conversation, which the hourly conversation cap
/// meters.
pub(super) fn meters(action: &RuleAction) -> bool {
    matches!(
        action,
        RuleAction::StartAgent { .. } | RuleAction::SteerConversation { .. }
    )
}

/// Before a conversation is started or steered: the rule's hourly conversation cap.
pub(super) fn count_conversation(db: &Db<'_>, row: &RuleRow, version: &VersionRow) -> Result<bool> {
    let cap = i64::from(version.definition.limits.conversations_per_hour);
    let used: i64 = db.0.query_row(
        "SELECT count(*) FROM hook_rule_conversation_counts WHERE rule_id=?1 AND counted_at>?2",
        params![row.id, now() - 3_600_000],
        |row| row.get(0),
    )?;
    if used >= cap {
        exhaust(
            db,
            row,
            &format!(
                "More than {cap} conversations were started or steered within an hour. Its creator or the owner can resume it."
            ),
        )?;
        return Ok(false);
    }
    Ok(true)
}

pub(super) fn record_conversation(db: &Db<'_>, t: &Triggering, position: u32) -> Result<()> {
    db.0.execute(
        "INSERT OR IGNORE INTO hook_rule_conversation_counts VALUES(?1,?2,?3,?4)",
        params![t.rule_id, t.id, position, now()],
    )?;
    Ok(())
}

/// Refreshes the native authority observation a conversation action checks, before its
/// transaction.
pub(super) async fn prepare(
    s: &Service,
    row: &RuleRow,
    action: &RuleAction,
    input: &Json,
) -> Result<()> {
    if let RuleAction::StartAgent { agent_id, .. } = action {
        if let Some(snapshot) = s.store.agent_snapshot(&agent_id.to_string()).await? {
            crate::native::policy::refresh_for(s, &snapshot).await?;
        }
        return Ok(());
    }
    let Ok(key) = key(&input["key"]) else {
        return Ok(());
    };
    let creator = row.creator_account_id.clone();
    let chat = s
        .store
        .read(move |db| Ok(bound(db, &creator, &key)?.map(|bound| bound.chat)))
        .await?;
    if let Some(chat) = chat {
        crate::native::policy::refresh_chat(s, &chat).await?;
    }
    Ok(())
}
