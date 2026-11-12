//! The rule engine (ADR-0075, « Règles de hook » and « Budget du moteur de règles »).
//!
//! Intake turns each queued plugin hook event into one durable triggering per rule
//! that listens to it and may receive it, pinned to the rule's current version. The
//! executor then walks the tree: before every effect it persists where the triggering
//! stands, so that an interrupted action is never replayed, and it checks the rule's
//! blocks, its creator's membership and its budget again.
use super::{
    evaluate::{self, Variables},
    interpreter::Cursor,
    storage::{RuleRow, VersionRow},
    validation::{self, RESERVED},
};
use crate::{
    config::{id, now},
    error::{Error, Result, required},
    service::Service,
    store::Db,
};
use cairn_contracts::{PluginContributionKind as Kind, RuleAction, RuleNode, RuleValueScope};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value as Json, json};
use std::sync::Arc;

pub(crate) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS hook_rule_triggerings(
 id TEXT PRIMARY KEY NOT NULL,
 rule_id TEXT NOT NULL REFERENCES hook_rules(id) ON DELETE CASCADE,
 version INTEGER NOT NULL CHECK(version>=1),
 event_id TEXT NOT NULL CHECK(length(event_id) BETWEEN 1 AND 300),
 event TEXT NOT NULL CHECK(json_valid(event) AND json_type(event)='object'),
 status TEXT NOT NULL CHECK(status IN ('running','waiting','retrying','completed','failed','stopped')),
 cursor TEXT NOT NULL CHECK(json_valid(cursor) AND json_type(cursor)='object'),
 pending TEXT CHECK(pending IS NULL OR json_valid(pending)),
 in_flight INTEGER NOT NULL CHECK(in_flight IN (0,1)),
 actions INTEGER NOT NULL CHECK(actions>=0),
 tool_calls INTEGER NOT NULL CHECK(tool_calls>=0),
 counted_at INTEGER CHECK(counted_at BETWEEN 0 AND 9007199254740991),
 verify INTEGER NOT NULL CHECK(verify IN (0,1)),
 verify_since INTEGER CHECK(verify_since BETWEEN 0 AND 9007199254740991),
 next_at INTEGER CHECK(next_at BETWEEN 0 AND 9007199254740991),
 lease_until INTEGER CHECK(lease_until BETWEEN 0 AND 9007199254740991),
 error TEXT CHECK(error IS NULL OR length(error)<=4000),
 created_at INTEGER NOT NULL CHECK(created_at BETWEEN 0 AND 9007199254740991),
 updated_at INTEGER NOT NULL CHECK(updated_at BETWEEN 0 AND 9007199254740991),
 finished_at INTEGER CHECK(finished_at BETWEEN 0 AND 9007199254740991),
 UNIQUE(rule_id,event_id)
);
CREATE INDEX IF NOT EXISTS hook_rule_triggerings_rule ON hook_rule_triggerings(rule_id,created_at DESC);
CREATE INDEX IF NOT EXISTS hook_rule_triggerings_due ON hook_rule_triggerings(status,next_at) WHERE status IN ('running','waiting','retrying');
CREATE INDEX IF NOT EXISTS hook_rule_triggerings_counted ON hook_rule_triggerings(rule_id,counted_at) WHERE counted_at IS NOT NULL;
CREATE TABLE IF NOT EXISTS hook_rule_steps(
 triggering_id TEXT NOT NULL REFERENCES hook_rule_triggerings(id) ON DELETE CASCADE,
 position INTEGER NOT NULL CHECK(position>=0),
 node_id TEXT NOT NULL,
 label TEXT NOT NULL,
 kind TEXT NOT NULL,
 status TEXT NOT NULL CHECK(status IN ('running','completed','failed','retrying','waiting')),
 input TEXT CHECK(input IS NULL OR json_valid(input)),
 result TEXT CHECK(result IS NULL OR json_valid(result)),
 error TEXT CHECK(error IS NULL OR length(error)<=4000),
 attempts INTEGER NOT NULL CHECK(attempts>=1),
 started_at INTEGER NOT NULL CHECK(started_at BETWEEN 0 AND 9007199254740991),
 finished_at INTEGER CHECK(finished_at BETWEEN 0 AND 9007199254740991),
 PRIMARY KEY(triggering_id,position)
) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS hook_rule_values(
 rule_id TEXT NOT NULL REFERENCES hook_rules(id) ON DELETE CASCADE,
 name TEXT NOT NULL CHECK(length(name) BETWEEN 1 AND 40),
 value TEXT NOT NULL CHECK(json_valid(value) AND length(CAST(value AS BLOB))<=65536),
 updated_at INTEGER NOT NULL CHECK(updated_at BETWEEN 0 AND 9007199254740991),
 PRIMARY KEY(rule_id,name)
) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS hook_rule_quiet_hours(
 rule_id TEXT NOT NULL REFERENCES hook_rules(id) ON DELETE CASCADE,
 hour INTEGER NOT NULL CHECK(hour>=0),
 count INTEGER NOT NULL CHECK(count>=1),
 PRIMARY KEY(rule_id,hour)
) WITHOUT ROWID;
";

/// Version 31 to 32. No triggering exists before.
pub(crate) fn upgrade(connection: &mut Connection) -> Result<()> {
    let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let version: i64 = tx.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if (32..=crate::store::DATABASE_VERSION).contains(&version) {
        return Ok(());
    }
    if version != 31 {
        return Err(Error::bad(
            "Unexpected installation database version during upgrade.",
        ));
    }
    tx.execute_batch(SCHEMA)?;
    tx.pragma_update(None, "user_version", 32)?;
    tx.commit()?;
    Ok(())
}

/// Triggerings waiting at once, per rule.
pub(crate) const MAX_WAITING: i64 = 20;
/// Attempts of an action the service confirmed it did not perform.
const MAX_ATTEMPTS: u32 = 5;
/// How long a member's triggering waits for a membership check before failing.
const MEMBERSHIP_PATIENCE: i64 = 7 * 86_400_000;
/// How long the executor holds a triggering it runs.
const LEASE: i64 = 10 * 60_000;
/// Finished triggerings kept per rule, newest first.
const KEPT: i64 = 200;
const TOOL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(90);
/// Triggerings run at once.
const PARALLEL: usize = 8;

/// The engine's wake-up, shared by intake and the executor.
#[derive(Default)]
pub struct Engine {
    pub(crate) wake: tokio::sync::Notify,
}

/// What one action of a triggering did.
#[derive(Clone, Copy)]
struct StepRecord<'a> {
    status: &'a str,
    input: Option<&'a Json>,
    result: Option<&'a Json>,
    error: Option<&'a str>,
}

/// An action whose input is evaluated, persisted before it runs.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Pending {
    node_id: String,
    position: u32,
    attempts: u32,
    input: Json,
}

#[derive(Clone, Debug)]
struct Triggering {
    id: String,
    rule_id: String,
    version: u32,
    event: Json,
    status: String,
    cursor: Cursor,
    pending: Option<Pending>,
    in_flight: bool,
    actions: u32,
    tool_calls: u32,
    counted_at: Option<i64>,
    verify: bool,
    verify_since: Option<i64>,
    created_at: i64,
}

const COLUMNS: &str = "id,rule_id,version,event,status,cursor,pending,in_flight,actions,tool_calls,counted_at,verify,verify_since,created_at";

fn triggering(row: &rusqlite::Row<'_>) -> rusqlite::Result<Triggering> {
    let decode = |index: usize, error: serde_json::Error| {
        rusqlite::Error::FromSqlConversionFailure(
            index,
            rusqlite::types::Type::Text,
            Box::new(error),
        )
    };
    let event: String = row.get(3)?;
    let cursor: String = row.get(5)?;
    let pending: Option<String> = row.get(6)?;
    Ok(Triggering {
        id: row.get(0)?,
        rule_id: row.get(1)?,
        version: row.get(2)?,
        event: serde_json::from_str(&event).map_err(|e| decode(3, e))?,
        status: row.get(4)?,
        cursor: serde_json::from_str(&cursor).map_err(|e| decode(5, e))?,
        pending: pending
            .map(|pending| serde_json::from_str(&pending))
            .transpose()
            .map_err(|e| decode(6, e))?,
        in_flight: row.get(7)?,
        actions: row.get(8)?,
        tool_calls: row.get(9)?,
        counted_at: row.get(10)?,
        verify: row.get(11)?,
        verify_since: row.get(12)?,
        created_at: row.get(13)?,
    })
}

impl Db<'_> {
    fn hook_triggering(&self, id: &str) -> Result<Option<Triggering>> {
        Ok(self
            .0
            .prepare_cached(&format!(
                "SELECT {COLUMNS} FROM hook_rule_triggerings WHERE id=?"
            ))?
            .query_row([id], triggering)
            .optional()?)
    }

    fn save_hook_triggering(
        &self,
        t: &Triggering,
        status: &str,
        next_at: Option<i64>,
        error: Option<&str>,
    ) -> Result<()> {
        let finished = matches!(status, "completed" | "failed" | "stopped").then(now);
        self.0
            .prepare_cached("UPDATE hook_rule_triggerings SET status=?2,cursor=?3,pending=?4,in_flight=?5,actions=?6,tool_calls=?7,counted_at=?8,verify=?9,verify_since=?10,next_at=?11,error=?12,updated_at=?13,finished_at=?14,lease_until=CASE WHEN ?2='running' THEN ?13+?15 ELSE NULL END WHERE id=?1")?
            .execute(params![
                t.id,
                status,
                serde_json::to_string(&t.cursor)?,
                t.pending.as_ref().map(serde_json::to_string).transpose()?,
                t.in_flight,
                t.actions,
                t.tool_calls,
                t.counted_at,
                t.verify,
                t.verify_since,
                next_at,
                error.map(|error| error.chars().take(4000).collect::<String>()),
                now(),
                finished,
                LEASE,
            ])?;
        Ok(())
    }

    fn hook_rule_values(&self, rule: &str) -> Result<Map<String, Json>> {
        let mut values = Map::new();
        let mut statement = self.0.prepare_cached(
            "SELECT name,value FROM hook_rule_values WHERE rule_id=? ORDER BY name",
        )?;
        for row in statement.query_map([rule], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })? {
            let (name, value) = row?;
            values.insert(name, serde_json::from_str(&value)?);
        }
        Ok(values)
    }

    fn record_step(
        &self,
        triggering: &str,
        position: u32,
        node: &RuleNode,
        attempts: u32,
        record: StepRecord<'_>,
    ) -> Result<()> {
        let StepRecord {
            status,
            input,
            result,
            error,
        } = record;
        let (label, kind) = match node {
            RuleNode::Action { label, action, .. } => (label.clone(), action_kind(action)),
            _ => (String::new(), "block"),
        };
        let at = now();
        let finished = matches!(status, "completed" | "failed").then_some(at);
        self.0
            .prepare_cached("INSERT INTO hook_rule_steps VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12) ON CONFLICT(triggering_id,position) DO UPDATE SET status=excluded.status,result=excluded.result,error=excluded.error,attempts=excluded.attempts,finished_at=excluded.finished_at")?
            .execute(params![
                triggering,
                position,
                node.id(),
                label,
                kind,
                status,
                input.map(Json::to_string),
                result.map(Json::to_string),
                error.map(|error| error.chars().take(4000).collect::<String>()),
                attempts,
                at,
                finished,
            ])?;
        Ok(())
    }

    fn count_quiet(&self, rule: &str) -> Result<()> {
        self.0
            .prepare_cached("INSERT INTO hook_rule_quiet_hours VALUES(?1,?2,1) ON CONFLICT(rule_id,hour) DO UPDATE SET count=count+1")?
            .execute(params![rule, now() / 3_600_000])?;
        Ok(())
    }

    /// Keeps the newest finished triggerings of a rule and a week of quiet hours.
    fn prune_hook_triggerings(&self, rule: &str) -> Result<()> {
        self.0.prepare_cached("DELETE FROM hook_rule_triggerings WHERE rule_id=?1 AND status IN ('completed','failed','stopped') AND id NOT IN (SELECT id FROM hook_rule_triggerings WHERE rule_id=?1 ORDER BY created_at DESC LIMIT ?2) AND (counted_at IS NULL OR counted_at<?3)")?
            .execute(params![rule, KEPT, now() - 3_600_000])?;
        self.0
            .prepare_cached("DELETE FROM hook_rule_quiet_hours WHERE rule_id=? AND hour<?")?
            .execute(params![rule, now() / 3_600_000 - 24 * 7])?;
        Ok(())
    }
}

pub(crate) fn action_kind(action: &RuleAction) -> &'static str {
    match action {
        RuleAction::CallTool { .. } => "callTool",
        RuleAction::StartAgent { .. } => "startAgent",
        RuleAction::SteerConversation { .. } => "steerConversation",
        RuleAction::CancelConversation { .. } => "cancelConversation",
        RuleAction::AnswerQuestion { .. } => "answerQuestion",
        RuleAction::StoreValue { .. } => "storeValue",
        RuleAction::Wait { .. } => "wait",
    }
}

/// Stops the triggerings in progress of a rule that was disabled, paused in an
/// emergency, or whose creator left. Called in the transaction that blocks it.
pub(crate) fn stop_rule(db: &Db<'_>, rule: &str, reason: &str) -> Result<()> {
    db.0.prepare_cached("UPDATE hook_rule_triggerings SET status='stopped',error=?2,in_flight=0,lease_until=NULL,next_at=NULL,finished_at=?3,updated_at=?3 WHERE rule_id=?1 AND status IN ('waiting','retrying') OR rule_id=?1 AND status='running' AND in_flight=0 AND lease_until IS NULL")?
        .execute(params![rule, reason, now()])?;
    Ok(())
}

/// Why a rule must not act now, if it must not. An exhausted budget lets the
/// triggerings in progress finish.
fn blocked(db: &Db<'_>, row: &RuleRow, version: &VersionRow) -> Result<Option<String>> {
    if row.disabled_at.is_some() {
        return Ok(Some("The rule was disabled.".into()));
    }
    if row.emergency_paused_at.is_some() {
        return Ok(Some("The owner paused the rule in an emergency.".into()));
    }
    if row.membership_ended_at.is_some() {
        return Ok(Some("The rule's creator left the installation.".into()));
    }
    super::rights_lost(db, row, &version.definition)
}

/// The variables every formula of a triggering reads, besides steps and loop items.
fn base(event: &Json, values: Map<String, Json>, started: i64) -> Variables {
    let mut variables = Variables::default();
    let payload = event["payload"].clone();
    if let Some(fields) = payload.as_object() {
        for (name, value) in fields {
            let identifier = name.len() <= 40
                && name
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
                && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
            if identifier && !RESERVED.contains(&name.as_str()) {
                variables.insert(name, value.clone());
            }
        }
    }
    variables.insert("payload", payload);
    variables.insert(
        "event",
        json!({
            "id": event["id"],
            "hook": event["hook"],
            "instanceId": event["instanceId"],
            "receivedAt": event["receivedAt"],
        }),
    );
    variables.insert("values", Json::Object(values));
    let started = chrono::DateTime::from_timestamp_millis(started).unwrap_or_default();
    variables.insert("now", Json::String(started.to_rfc3339()));
    variables
}

/// Turns queued hook events into triggerings, then acknowledges them. An event no
/// rule may receive is dropped, so a quiet queue never blocks later events.
async fn intake(s: &Arc<Service>) -> Result<usize> {
    let instances = s
        .store
        .read(|db| {
            Ok(db
                .0
                .prepare_cached(
                    "SELECT DISTINCT instance_id FROM plugin_hook_events ORDER BY instance_id",
                )?
                .query_map([], |row| row.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?)
        })
        .await?;
    let mut created = 0;
    for instance in instances {
        let Some(connection) = s.store.plugin_instance(&instance).await? else {
            continue;
        };
        let Ok(package) = crate::plugins::registry::active(s, &connection.plugin_id).await else {
            continue;
        };
        let (revision, digest) = (connection.revision.clone(), package.digest.clone());
        let queue = instance.clone();
        let events = s
            .store
            .read(move |db| db.plugin_hooks(&queue, &revision, &digest, now()))
            .await?;
        for event in events {
            let (instance, hook) = (instance.clone(), event.name.clone());
            let listeners = s
                .store
                .read(move |db| {
                    let mut listeners = Vec::new();
                    let rules = db.0.prepare_cached("SELECT DISTINCT rule_id FROM hook_rule_uses WHERE kind='hook' AND instance_id=? AND name=?")?
                        .query_map(params![instance, hook], |row| row.get::<_, String>(0))?
                        .collect::<rusqlite::Result<Vec<_>>>()?;
                    for rule in rules {
                        let Some(row) = db.hook_rule(&rule)? else { continue };
                        let Some(version) = db.hook_rule_version(&rule, row.version)? else { continue };
                        if row.budget_exhausted_at.is_some() || blocked(db, &row, &version)?.is_some() {
                            continue;
                        }
                        listeners.push((row, version));
                    }
                    Ok(listeners)
                })
                .await?;
            for (row, version) in listeners {
                let agent = version.definition.execution_agent_id.to_string();
                // Cairn never delivers an event whose payload designates a resource
                // outside the execution agent's hook grant.
                if crate::plugins::grant_authorization::authorize_hook_delivery(
                    s,
                    &agent,
                    &event.instance_id,
                    &event.name,
                    &event.payload,
                )
                .await
                .is_err()
                {
                    continue;
                }
                let record = json!({
                    "id": event.id,
                    "hook": event.name,
                    "instanceId": event.instance_id,
                    "payload": event.payload,
                    "receivedAt": event.created_at,
                });
                let (rule, number, event_id) = (row.id.clone(), version.version, event.id.clone());
                let inserted = s
                    .store
                    .transaction(move |db| {
                        let cursor = serde_json::to_string(&Cursor::default())?;
                        let at = now();
                        Ok(db.0.prepare_cached("INSERT OR IGNORE INTO hook_rule_triggerings(id,rule_id,version,event_id,event,status,cursor,pending,in_flight,actions,tool_calls,counted_at,verify,verify_since,next_at,lease_until,error,created_at,updated_at,finished_at) VALUES(?1,?2,?3,?4,?5,'running',?6,NULL,0,0,0,NULL,1,NULL,NULL,NULL,NULL,?7,?7,NULL)")?
                            .execute(params![id(), rule, number, event_id, record.to_string(), cursor, at])?
                            > 0)
                    })
                    .await?;
                created += usize::from(inserted);
            }
            let (instance, event) = (event.instance_id.clone(), event.id.clone());
            s.store
                .transaction(move |db| db.remove_plugin_hook(&instance, &event))
                .await?;
        }
    }
    Ok(created)
}

/// Claims due triggerings. An action left in flight by an interrupted executor stays
/// ambiguous: its triggering fails rather than replaying it.
async fn claim(s: &Service) -> Result<Vec<String>> {
    s.store
        .transaction(|db| {
            let at = now();
            db.0.prepare_cached("UPDATE hook_rule_triggerings SET status='failed',error='Cairn stopped while an action was running. It is not run again, since it may have taken effect.',in_flight=0,lease_until=NULL,finished_at=?1,updated_at=?1 WHERE status='running' AND in_flight=1 AND lease_until<?1")?
                .execute([at])?;
            let due = db.0.prepare_cached("SELECT id FROM hook_rule_triggerings WHERE (status='running' AND (lease_until IS NULL OR lease_until<?1)) OR (status IN ('waiting','retrying') AND next_at<=?1) ORDER BY created_at LIMIT ?2")?
                .query_map(params![at, PARALLEL as i64], |row| row.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            for id in &due {
                db.0.prepare_cached("UPDATE hook_rule_triggerings SET status='running',next_at=NULL,lease_until=?2 WHERE id=?1")?
                    .execute(params![id, at + LEASE])?;
            }
            Ok(due)
        })
        .await
}

/// One pass: intake, then every due triggering. Returns how many it ran.
pub async fn tick(s: &Arc<Service>) -> Result<usize> {
    intake(s).await?;
    let mut ran = 0;
    loop {
        let due = claim(s).await?;
        if due.is_empty() {
            return Ok(ran);
        }
        ran += due.len();
        let runs = due.iter().map(|id| run(s, id));
        for (id, result) in due.iter().zip(futures_util::future::join_all(runs).await) {
            if let Err(error) = result {
                tracing::warn!(triggering = %id, error = %error, "Rule triggering failed to run");
                let (id, message) = (id.clone(), error.message.clone());
                s.store
                    .transaction(move |db| {
                        if let Some(t) = db.hook_triggering(&id)? {
                            db.save_hook_triggering(&t, "failed", None, Some(&message))?;
                        }
                        Ok(())
                    })
                    .await?;
            }
        }
    }
}

/// Runs the engine until shutdown, waking on new hook events and every two seconds.
pub async fn start(s: Arc<Service>) {
    loop {
        tokio::select! {
            () = s.shutdown.cancelled() => break,
            () = s.rules.wake.notified() => {}
            () = tokio::time::sleep(std::time::Duration::from_secs(2)) => {}
        }
        if let Err(error) = tick(&s).await {
            tracing::warn!(error = %error, "Rule engine pass failed");
        }
    }
}

struct Loaded {
    triggering: Triggering,
    row: RuleRow,
    version: VersionRow,
    values: Map<String, Json>,
}

async fn load(s: &Service, id: &str) -> Result<Option<Loaded>> {
    let id = id.to_owned();
    s.store
        .read(move |db| {
            let Some(triggering) = db.hook_triggering(&id)? else {
                return Ok(None);
            };
            let Some(row) = db.hook_rule(&triggering.rule_id)? else {
                return Ok(None);
            };
            let version = required(
                db.hook_rule_version(&triggering.rule_id, triggering.version)?,
                "Rule version not found",
            )?;
            let values = db.hook_rule_values(&triggering.rule_id)?;
            Ok(Some(Loaded {
                triggering,
                row,
                version,
                values,
            }))
        })
        .await
}

/// Ends a triggering. One without any action is not kept: only its hour is counted.
async fn finish(
    s: &Service,
    t: Triggering,
    status: &'static str,
    error: Option<String>,
) -> Result<()> {
    s.store
        .transaction(move |db| {
            if db.hook_triggering(&t.id)?.is_none() {
                return Ok(());
            }
            if status == "completed" && t.actions == 0 {
                db.0.execute("DELETE FROM hook_rule_triggerings WHERE id=?", [&t.id])?;
                db.count_quiet(&t.rule_id)?;
            } else {
                db.save_hook_triggering(&t, status, None, error.as_deref())?;
            }
            db.prune_hook_triggerings(&t.rule_id)
        })
        .await
}

/// Exceeding an hourly cap suspends the rule and tells its creator. The triggerings
/// already counted keep running.
fn exhaust(db: &Db<'_>, row: &RuleRow, reason: &str) -> Result<()> {
    let mut row = required(db.hook_rule(&row.id)?, "Rule not found")?;
    if row.budget_exhausted_at.is_none() {
        row.budget_exhausted_at = Some(now());
        row.budget_reason = Some(reason.to_owned());
        row.revision = id();
        row.updated_at = now();
        db.save_hook_rule(&row)?;
        db.audit("hook_rule.budget_exhausted", &json!({ "ruleId": row.id }))?;
        let name = db
            .hook_rule_version(&row.id, row.version)?
            .map(|version| version.definition.name)
            .unwrap_or_default();
        crate::notifications::enqueue_rule_alert(
            db,
            &row.creator_account_id,
            "A rule reached its hourly limit",
            &format!("{name} is suspended. Resume it from its page when you are ready."),
        )?;
    }
    Ok(())
}

/// Before a triggering's first effect: counts it against the rule's hourly cap.
fn count(db: &Db<'_>, t: &mut Triggering, row: &RuleRow, version: &VersionRow) -> Result<bool> {
    if t.counted_at.is_some() {
        return Ok(true);
    }
    let cap = i64::from(version.definition.limits.triggerings_per_hour);
    let used: i64 =
        db.0.prepare_cached(
            "SELECT count(*) FROM hook_rule_triggerings WHERE rule_id=? AND counted_at>?",
        )?
        .query_row(params![row.id, now() - 3_600_000], |r| r.get(0))?;
    if used >= cap {
        exhaust(
            db,
            row,
            &format!(
                "More than {cap} triggerings acted within an hour. Its creator or the owner can resume it."
            ),
        )?;
        return Ok(false);
    }
    t.counted_at = Some(now());
    Ok(true)
}

async fn call_tool(
    s: &Service,
    agent: &str,
    instance: &str,
    tool: &str,
    input: &Json,
) -> Result<Json> {
    let connection = required(
        s.store.plugin_instance(instance).await?,
        "This plugin instance no longer exists.",
    )?;
    if !connection.enabled {
        return Err(Error::conflict("This plugin instance is disabled."));
    }
    let package = crate::plugins::registry::active(s, &connection.plugin_id).await?;
    let declaration = package.manifest.tool(tool).ok_or_else(|| {
        Error::bad(format!(
            "{} has no tool named {tool}.",
            package.manifest.name
        ))
    })?;
    let valid = jsonschema::validator_for(&declaration.input_schema)
        .is_ok_and(|validator| validator.is_valid(input));
    if !valid {
        return Err(Error::bad(format!("The input does not fit {tool}.")));
    }
    let profile = required(
        s.store.agent_snapshot(agent).await?,
        "The execution agent no longer exists.",
    )?;
    let snapshot =
        crate::plugins::grant_authorization::Snapshot::capture(s, agent, None, &package).await?;
    let scope = crate::plugins::grant_authorization::contribution_scope(
        s,
        snapshot,
        vec![connection],
        Kind::Tool,
        tool,
        input,
    )
    .await?;
    let payload = json!({
        "tool": tool,
        "input": input,
        "agent": profile,
        "instances": scope.instances(),
    });
    tokio::time::timeout(
        TOOL_TIMEOUT,
        crate::plugins::scope::invoke(s, &package, "tool", payload, &scope),
    )
    .await
    .map_err(|_| Error::unavailable("The tool did not answer in time; it may have taken effect."))?
}

/// A tool says the service did nothing yet and when to try again.
fn retry_after(result: &Json) -> Option<f64> {
    result
        .as_object()
        .filter(|fields| {
            fields.len() <= 2
                && fields
                    .keys()
                    .all(|key| key == "retryAfter" || key == "error")
        })
        .and_then(|fields| fields.get("retryAfter"))
        .and_then(Json::as_f64)
}

fn node<'a>(steps: &'a [RuleNode], id: &str) -> Option<&'a RuleNode> {
    for node in steps {
        if node.id() == id {
            return Some(node);
        }
        let found = match node {
            RuleNode::If {
                then, otherwise, ..
            } => self::node(then, id).or_else(|| self::node(otherwise, id)),
            RuleNode::ForEach { body, .. } => self::node(body, id),
            RuleNode::Action { .. } => None,
        };
        if found.is_some() {
            return found;
        }
    }
    None
}

enum Step {
    Continue,
    Done,
}

/// Runs one claimed triggering until it waits, retries or ends.
async fn run(s: &Arc<Service>, id: &str) -> Result<()> {
    loop {
        match step(s, id).await? {
            Step::Continue => continue,
            Step::Done => return Ok(()),
        }
    }
}

async fn step(s: &Arc<Service>, id: &str) -> Result<Step> {
    let Some(Loaded {
        mut triggering,
        row,
        version,
        values,
    }) = load(s, id).await?
    else {
        return Ok(Step::Done);
    };
    if triggering.status != "running" {
        return Ok(Step::Done);
    }
    let reason = {
        let (row, version) = (row.clone(), version.clone());
        s.store.read(move |db| blocked(db, &row, &version)).await?
    };
    if let Some(reason) = reason {
        finish(s, triggering, "stopped", Some(reason)).await?;
        return Ok(Step::Done);
    }
    let base = base(&triggering.event, values, triggering.created_at);
    let steps = &version.definition.steps;

    // A pending action resumes with the input it was evaluated with.
    let (node, input) = match triggering.pending.clone() {
        Some(pending) => {
            let node = node(steps, &pending.node_id).ok_or_else(|| {
                Error::internal("A pending step is missing from its rule version.")
            })?;
            (node, Some(pending.input))
        }
        None => match triggering.cursor.advance(steps, &base) {
            Ok(Some(node)) => (node, None),
            Ok(None) => {
                finish(s, triggering, "completed", None).await?;
                return Ok(Step::Done);
            }
            Err(error) => {
                finish(s, triggering, "failed", Some(error)).await?;
                return Ok(Step::Done);
            }
        },
    };
    let RuleNode::Action { action, .. } = node else {
        return Err(Error::internal("The interpreter stopped on a block."));
    };

    // A member's triggering acts only on a recently verified membership, and checks it
    // again after each wait.
    if triggering.verify && !row.creator_owner {
        if s.missions
            .verify_membership(s, std::time::Duration::from_secs(60))
            .await
            .is_err()
        {
            let since = *triggering.verify_since.get_or_insert_with(now);
            if now() - since > MEMBERSHIP_PATIENCE {
                finish(
                    s,
                    triggering,
                    "failed",
                    Some("Its creator's membership could not be verified for seven days.".into()),
                )
                .await?;
            } else {
                let at = now() + 60_000;
                s.store
                    .transaction(move |db| {
                        db.save_hook_triggering(
                            &triggering,
                            "waiting",
                            Some(at),
                            Some("Waiting to verify its creator's membership."),
                        )
                    })
                    .await?;
            }
            return Ok(Step::Done);
        }
        // Verification may have just ended the membership.
        let rule = row.id.clone();
        let ended = s
            .store
            .read(move |db| {
                Ok(db
                    .hook_rule(&rule)?
                    .map(|row| row.membership_ended_at.is_some()))
            })
            .await?;
        match ended {
            None => return Ok(Step::Done),
            Some(true) => {
                finish(
                    s,
                    triggering,
                    "stopped",
                    Some("The rule's creator left the installation.".into()),
                )
                .await?;
                return Ok(Step::Done);
            }
            Some(false) => {}
        }
    }
    triggering.verify = false;
    triggering.verify_since = None;

    let variables = triggering.cursor.variables(&base);
    let position = triggering
        .pending
        .as_ref()
        .map_or(triggering.actions, |pending| pending.position);
    let attempts = triggering
        .pending
        .as_ref()
        .map_or(1, |pending| pending.attempts + 1);
    let agent = version.definition.execution_agent_id.to_string();

    match action {
        RuleAction::CallTool {
            instance_id,
            tool,
            input: fields,
        } => {
            let input = match input {
                Some(input) => input,
                None => {
                    let mut evaluated = Map::new();
                    for (key, value) in fields {
                        match evaluate::value(value, &variables) {
                            Ok(value) => {
                                evaluated.insert(key.clone(), value);
                            }
                            Err(error) => {
                                finish(
                                    s,
                                    triggering,
                                    "failed",
                                    Some(format!("{}: {error}", node.id())),
                                )
                                .await?;
                                return Ok(Step::Done);
                            }
                        }
                    }
                    Json::Object(evaluated)
                }
            };
            // Persist the action as in flight before it can take effect.
            let mut prepared = triggering.clone();
            prepared.pending = Some(Pending {
                node_id: node.id().to_owned(),
                position,
                attempts,
                input: input.clone(),
            });
            prepared.in_flight = true;
            if prepared
                .pending
                .as_ref()
                .is_some_and(|pending| pending.attempts == 1)
            {
                prepared.actions += 1;
                prepared.tool_calls += 1;
            }
            let over = prepared.tool_calls > validation::MAX_TOOL_CALLS;
            let (row_copy, version_copy, node_copy, input_copy) =
                (row.clone(), version.clone(), node.clone(), input.clone());
            let go = s
                .store
                .transaction(move |db| {
                    if db.hook_triggering(&prepared.id)?.is_none() {
                        return Ok(None);
                    }
                    if over {
                        db.save_hook_triggering(
                            &prepared,
                            "failed",
                            None,
                            Some(&format!(
                                "This triggering called more than {} tools.",
                                validation::MAX_TOOL_CALLS
                            )),
                        )?;
                        return Ok(None);
                    }
                    if !count(db, &mut prepared, &row_copy, &version_copy)? {
                        db.save_hook_triggering(
                            &prepared,
                            "failed",
                            None,
                            Some("The rule reached its hourly limit of triggerings."),
                        )?;
                        return Ok(None);
                    }
                    db.record_step(
                        &prepared.id,
                        position,
                        &node_copy,
                        attempts,
                        StepRecord {
                            status: "running",
                            input: Some(&input_copy),
                            result: None,
                            error: None,
                        },
                    )?;
                    db.save_hook_triggering(&prepared, "running", None, None)?;
                    Ok(Some(prepared))
                })
                .await?;
            let Some(mut prepared) = go else {
                return Ok(Step::Done);
            };
            let result = call_tool(s, &agent, &instance_id.to_string(), tool, &input).await;
            let node = node.clone();
            s.store
                .transaction(move |db| {
                    if db.hook_triggering(&prepared.id)?.is_none() {
                        return Ok(Step::Done);
                    }
                    prepared.in_flight = false;
                    match result {
                        Ok(result) => {
                            if let Some(seconds) = retry_after(&result) {
                                if attempts >= MAX_ATTEMPTS {
                                    let error = format!(
                                        "The service refused this action {attempts} times."
                                    );
                                    db.record_step(
                                        &prepared.id,
                                        position,
                                        &node,
                                        attempts,
                                        StepRecord {
                                            status: "failed",
                                            input: None,
                                            result: Some(&result),
                                            error: Some(&error),
                                        },
                                    )?;
                                    db.save_hook_triggering(
                                        &prepared,
                                        "failed",
                                        None,
                                        Some(&error),
                                    )?;
                                    return Ok(Step::Done);
                                }
                                let backoff = 2f64.powi(attempts as i32).min(3600.0);
                                let delay = (seconds.max(backoff).min(3600.0) * 1000.0) as i64;
                                db.record_step(
                                    &prepared.id,
                                    position,
                                    &node,
                                    attempts,
                                    StepRecord {
                                        status: "retrying",
                                        input: None,
                                        result: Some(&result),
                                        error: None,
                                    },
                                )?;
                                db.save_hook_triggering(
                                    &prepared,
                                    "retrying",
                                    Some(now() + delay),
                                    None,
                                )?;
                                return Ok(Step::Done);
                            }
                            if serde_json::to_vec(&result)
                                .map_or(true, |bytes| bytes.len() > evaluate::MAX_BYTES)
                            {
                                let error = "The tool's result is larger than 256 KiB.";
                                db.record_step(
                                    &prepared.id,
                                    position,
                                    &node,
                                    attempts,
                                    StepRecord {
                                        status: "failed",
                                        input: None,
                                        result: None,
                                        error: Some(error),
                                    },
                                )?;
                                db.save_hook_triggering(&prepared, "failed", None, Some(error))?;
                                return Ok(Step::Done);
                            }
                            db.record_step(
                                &prepared.id,
                                position,
                                &node,
                                attempts,
                                StepRecord {
                                    status: "completed",
                                    input: None,
                                    result: Some(&result),
                                    error: None,
                                },
                            )?;
                            prepared.cursor.steps.insert(node.id().to_owned(), result);
                            prepared.pending = None;
                            db.save_hook_triggering(&prepared, "running", None, None)?;
                            Ok(Step::Continue)
                        }
                        Err(error) => {
                            db.record_step(
                                &prepared.id,
                                position,
                                &node,
                                attempts,
                                StepRecord {
                                    status: "failed",
                                    input: None,
                                    result: None,
                                    error: Some(&error.message),
                                },
                            )?;
                            db.save_hook_triggering(
                                &prepared,
                                "failed",
                                None,
                                Some(&error.message),
                            )?;
                            Ok(Step::Done)
                        }
                    }
                })
                .await
        }
        RuleAction::StoreValue {
            scope: RuleValueScope::Rule,
            name,
            value,
            ..
        } => {
            let value = match evaluate::value(value, &variables) {
                Ok(value)
                    if serde_json::to_vec(&value).is_ok_and(|bytes| bytes.len() <= 65_536) =>
                {
                    value
                }
                Ok(_) => {
                    finish(
                        s,
                        triggering,
                        "failed",
                        Some(format!(
                            "{}: a remembered value has at most 64 KiB.",
                            node.id()
                        )),
                    )
                    .await?;
                    return Ok(Step::Done);
                }
                Err(error) => {
                    finish(
                        s,
                        triggering,
                        "failed",
                        Some(format!("{}: {error}", node.id())),
                    )
                    .await?;
                    return Ok(Step::Done);
                }
            };
            let (row, version, node, name) =
                (row.clone(), version.clone(), node.clone(), name.clone());
            s.store
                .transaction(move |db| {
                    if db.hook_triggering(&triggering.id)?.is_none() {
                        return Ok(Step::Done);
                    }
                    triggering.actions += 1;
                    if !count(db, &mut triggering, &row, &version)? {
                        db.save_hook_triggering(&triggering, "failed", None, Some("The rule reached its hourly limit of triggerings."))?;
                        return Ok(Step::Done);
                    }
                    db.0.prepare_cached("INSERT INTO hook_rule_values VALUES(?1,?2,?3,?4) ON CONFLICT(rule_id,name) DO UPDATE SET value=excluded.value,updated_at=excluded.updated_at")?
                        .execute(params![row.id, name, value.to_string(), now()])?;
                    db.record_step(&triggering.id, position, &node, 1, StepRecord { status: "completed", input: Some(&value), result: Some(&value), error: None })?;
                    triggering.cursor.steps.insert(node.id().to_owned(), value);
                    db.save_hook_triggering(&triggering, "running", None, None)?;
                    Ok(Step::Continue)
                })
                .await
        }
        RuleAction::Wait { seconds } => {
            let seconds = match evaluate::value(seconds, &variables) {
                Ok(value) => value.as_f64().filter(|seconds| {
                    (1.0..=f64::from(validation::MAX_WAIT_SECONDS)).contains(seconds)
                }),
                Err(error) => {
                    finish(
                        s,
                        triggering,
                        "failed",
                        Some(format!("{}: {error}", node.id())),
                    )
                    .await?;
                    return Ok(Step::Done);
                }
            };
            let Some(seconds) = seconds else {
                finish(
                    s,
                    triggering,
                    "failed",
                    Some(format!("{}: wait from 1 second to 7 days.", node.id())),
                )
                .await?;
                return Ok(Step::Done);
            };
            let (row, version, node) = (row.clone(), version.clone(), node.clone());
            s.store
                .transaction(move |db| {
                    if db.hook_triggering(&triggering.id)?.is_none() {
                        return Ok(Step::Done);
                    }
                    let waiting: i64 = db.0.prepare_cached("SELECT count(*) FROM hook_rule_triggerings WHERE rule_id=? AND status='waiting' AND id<>?")?
                        .query_row(params![row.id, triggering.id], |r| r.get(0))?;
                    if waiting >= MAX_WAITING {
                        db.save_hook_triggering(&triggering, "failed", None, Some(&format!("This rule already has {MAX_WAITING} triggerings waiting.")))?;
                        return Ok(Step::Done);
                    }
                    triggering.actions += 1;
                    if !count(db, &mut triggering, &row, &version)? {
                        db.save_hook_triggering(&triggering, "failed", None, Some("The rule reached its hourly limit of triggerings."))?;
                        return Ok(Step::Done);
                    }
                    let resumes = now() + (seconds * 1000.0) as i64;
                    db.record_step(&triggering.id, position, &node, 1, StepRecord { status: "completed", input: None, result: Some(&json!({ "resumesAt": resumes })), error: None })?;
                    triggering.verify = true;
                    db.save_hook_triggering(&triggering, "waiting", Some(resumes), None)?;
                    Ok(Step::Done)
                })
                .await
        }
        _ => {
            let kind = action_kind(action);
            finish(
                s,
                triggering,
                "failed",
                Some(format!(
                    "{}: the {kind} action needs conversation keys, which are not available yet.",
                    node.id()
                )),
            )
            .await?;
            Ok(Step::Done)
        }
    }
}

/// Whether a viewer sees a triggering's event, actions and errors.
fn details_visible(
    db: &Db<'_>,
    row: &RuleRow,
    definition: &cairn_contracts::HookRuleDefinition,
    viewer: &super::Viewer,
) -> Result<bool> {
    let Some(account) = &viewer.account else {
        return Ok(true);
    };
    if viewer.owner || *account == row.creator_account_id {
        return Ok(true);
    }
    super::validation::Creator {
        account: account.clone(),
        owner: false,
    }
    .uses_agent(db, &definition.execution_agent_id.to_string())
}

fn view(
    db: &Db<'_>,
    id: &str,
    visible: bool,
    detail: bool,
) -> Result<cairn_contracts::HookRuleTriggering> {
    let (t, status, updated_at, finished_at, next_at, error): (Triggering, String, i64, Option<i64>, Option<i64>, Option<String>) =
        db.0.prepare_cached(&format!("SELECT {COLUMNS},status,updated_at,finished_at,next_at,error FROM hook_rule_triggerings WHERE id=?"))?
            .query_row([id], |row| Ok((triggering(row)?, row.get(14)?, row.get(15)?, row.get(16)?, row.get(17)?, row.get(18)?)))?;
    let status = serde_json::from_value(Json::String(status))?;
    let event = (visible && detail).then(|| cairn_contracts::HookRuleTriggeringEvent {
        hook: t.event["hook"].as_str().unwrap_or_default().to_owned(),
        instance_id: t.event["instanceId"]
            .as_str()
            .and_then(|id| uuid::Uuid::parse_str(id).ok())
            .map(cairn_contracts::PluginInstanceId),
        payload: t.event["payload"].clone(),
        received_at: t.event["receivedAt"].as_i64().unwrap_or_default(),
    });
    let steps = if visible && detail {
        let mut statement = db.0.prepare_cached("SELECT node_id,label,kind,status,input,result,error,attempts,started_at,finished_at FROM hook_rule_steps WHERE triggering_id=? ORDER BY position")?;
        let rows = statement.query_map([id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, Option<String>>(6)?,
                row.get::<_, u32>(7)?,
                row.get::<_, i64>(8)?,
                row.get::<_, Option<i64>>(9)?,
            ))
        })?;
        let mut steps = Vec::new();
        for row in rows {
            let (
                node_id,
                label,
                kind,
                status,
                input,
                result,
                error,
                attempts,
                started_at,
                finished_at,
            ) = row?;
            steps.push(cairn_contracts::HookRuleStep {
                node_id,
                label,
                kind,
                status: serde_json::from_value(Json::String(status))?,
                input: input
                    .map(|input| serde_json::from_str(&input))
                    .transpose()?,
                result: result
                    .map(|result| serde_json::from_str(&result))
                    .transpose()?,
                error,
                attempts,
                started_at,
                finished_at,
            });
        }
        Some(steps)
    } else {
        None
    };
    Ok(cairn_contracts::HookRuleTriggering {
        id: uuid::Uuid::parse_str(&t.id)
            .map_err(|_| Error::bad("Invalid triggering identifier."))?,
        rule_id: cairn_contracts::HookRuleId(
            uuid::Uuid::parse_str(&t.rule_id)
                .map_err(|_| Error::bad("Invalid rule identifier."))?,
        ),
        version: t.version,
        status,
        actions: t.actions,
        created_at: t.created_at,
        updated_at,
        finished_at,
        resumes_at: next_at,
        details_visible: visible,
        error: if visible { error } else { None },
        event,
        steps,
    })
}

const PAGE: i64 = 50;

pub(crate) fn list(
    db: &Db<'_>,
    input: &cairn_contracts::ListHookRuleTriggeringsInput,
    viewer: &super::Viewer,
) -> Result<cairn_contracts::ListHookRuleTriggeringsOutput> {
    let rule = input.rule_id.to_string();
    let row = required(db.hook_rule(&rule)?, "Rule not found")?;
    let version = required(
        db.hook_rule_version(&rule, row.version)?,
        "Rule version not found",
    )?;
    let visible = details_visible(db, &row, &version.definition, viewer)?;
    let ids = db.0.prepare_cached("SELECT id,created_at FROM hook_rule_triggerings WHERE rule_id=? AND created_at<? ORDER BY created_at DESC,id DESC LIMIT ?")?
        .query_map(params![rule, input.before.unwrap_or(i64::MAX), PAGE + 1], |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let next_before = (ids.len() as i64 > PAGE).then(|| ids[PAGE as usize - 1].1);
    let mut triggerings = Vec::new();
    for (id, _) in ids.into_iter().take(PAGE as usize) {
        triggerings.push(view(db, &id, visible, false)?);
    }
    let without_effect: i64 =
        db.0.prepare_cached(
            "SELECT coalesce(sum(count),0) FROM hook_rule_quiet_hours WHERE rule_id=? AND hour>=?",
        )?
        .query_row(params![rule, now() / 3_600_000 - 23], |row| row.get(0))?;
    Ok(cairn_contracts::ListHookRuleTriggeringsOutput {
        triggerings,
        without_effect: u32::try_from(without_effect).unwrap_or(u32::MAX),
        next_before,
    })
}

pub(crate) fn get(
    db: &Db<'_>,
    id: &str,
    viewer: &super::Viewer,
) -> Result<cairn_contracts::HookRuleTriggering> {
    let t = required(db.hook_triggering(id)?, "Triggering not found")?;
    let row = required(db.hook_rule(&t.rule_id)?, "Rule not found")?;
    let version = required(
        db.hook_rule_version(&t.rule_id, row.version)?,
        "Rule version not found",
    )?;
    let visible = details_visible(db, &row, &version.definition, viewer)?;
    view(db, id, visible, true)
}
