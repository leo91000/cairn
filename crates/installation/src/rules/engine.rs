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

mod admissions;

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value as Json, json};
use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex},
    time::Instant,
};

pub(crate) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS hook_rule_admissions(
 rule_id TEXT NOT NULL REFERENCES hook_rules(id) ON DELETE CASCADE,
 event_id TEXT NOT NULL,
 version INTEGER NOT NULL CHECK(version>=1),
 instance_id TEXT NOT NULL REFERENCES plugin_instances(id) ON DELETE CASCADE,
 event TEXT NOT NULL CHECK(json_valid(event) AND json_type(event)='object'),
 fields TEXT NOT NULL CHECK(json_valid(fields) AND json_type(fields)='array'),
 created_at INTEGER NOT NULL,
 CHECK(length(CAST(event AS BLOB))+length(CAST(fields AS BLOB))<=983040),
 PRIMARY KEY(rule_id,event_id)
) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS hook_rule_admissions_order ON hook_rule_admissions(created_at,event_id);
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
 recorded_bytes INTEGER NOT NULL DEFAULT 0 CHECK(recorded_bytes>=0 AND recorded_bytes<=1048576),
 created_at INTEGER NOT NULL CHECK(created_at BETWEEN 0 AND 9007199254740991),
 updated_at INTEGER NOT NULL CHECK(updated_at BETWEEN 0 AND 9007199254740991),
 finished_at INTEGER CHECK(finished_at BETWEEN 0 AND 9007199254740991),
 UNIQUE(rule_id,event_id)
);
CREATE INDEX IF NOT EXISTS hook_rule_triggerings_rule ON hook_rule_triggerings(rule_id,created_at DESC);
CREATE INDEX IF NOT EXISTS hook_rule_triggerings_due ON hook_rule_triggerings(status,next_at);
CREATE INDEX IF NOT EXISTS hook_rule_triggerings_lease ON hook_rule_triggerings(status,lease_until);
CREATE INDEX IF NOT EXISTS hook_rule_triggerings_active ON hook_rule_triggerings(rule_id,version) WHERE status IN ('running','waiting','retrying');
CREATE TABLE IF NOT EXISTS hook_rule_receipts(
 rule_id TEXT NOT NULL REFERENCES hook_rules(id) ON DELETE CASCADE,
 event_id TEXT NOT NULL,
 counted_at INTEGER CHECK(counted_at BETWEEN 0 AND 9007199254740991),
 PRIMARY KEY(rule_id,event_id)
) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS hook_rule_receipts_counted ON hook_rule_receipts(rule_id,counted_at) WHERE counted_at IS NOT NULL;
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
CREATE INDEX IF NOT EXISTS hook_rule_steps_latest ON hook_rule_steps(triggering_id,node_id,position DESC) WHERE status='completed';
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
 count INTEGER NOT NULL CHECK(count>=0),
 not_admitted INTEGER NOT NULL DEFAULT 0 CHECK(not_admitted>=0),
 CHECK(count+not_admitted>=1),
 PRIMARY KEY(rule_id,hour)
) WITHOUT ROWID;
";

/// Version 32 to 33. No triggering exists before.
pub(crate) fn upgrade(connection: &mut Connection) -> Result<()> {
    let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let version: i64 = tx.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if (33..=crate::store::DATABASE_VERSION).contains(&version) {
        return Ok(());
    }
    if version != 32 {
        return Err(Error::bad(
            "Unexpected installation database version during upgrade.",
        ));
    }
    tx.execute_batch(SCHEMA)?;
    tx.pragma_update(None, "user_version", 33)?;
    tx.commit()?;
    Ok(())
}

/// How long intake retries an event whose resources could not be resolved.
const DELIVERY_PATIENCE: i64 = 10 * 60_000;
/// Triggerings waiting at once, per rule.
pub(crate) const MAX_WAITING: i64 = 20;
pub(crate) const MAX_PENDING: i64 = 1000;
pub(crate) const MAX_RECORDED_BYTES: usize = 1024 * 1024;
pub(crate) const MAX_CURSOR_BYTES: usize = 512 * 1024;
pub(crate) const MAX_RULE_VALUES_BYTES: usize = 1024 * 1024;
const ERROR_RESERVE: usize = 64 * 1024;
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
    delivery_retries: Mutex<HashMap<(String, String), (Instant, u32)>>,
    maintenance: Mutex<Option<Instant>>,
    last_resolution: Mutex<HashMap<String, Instant>>,
}

impl Engine {
    fn delivery_waiting(&self, key: &(String, String)) -> bool {
        self.delivery_retries
            .lock()
            .unwrap()
            .get(key)
            .is_some_and(|(at, _)| *at > Instant::now())
    }

    fn defer_delivery(&self, key: (String, String)) {
        let mut retries = self.delivery_retries.lock().unwrap();
        let attempts = retries
            .get(&key)
            .map_or(1, |(_, attempts)| attempts.saturating_add(1));
        let seconds = (2_u64.saturating_pow(attempts.min(6)) * 2).min(60);
        retries.insert(
            key,
            (
                Instant::now() + std::time::Duration::from_secs(seconds),
                attempts,
            ),
        );
    }

    fn clear_delivery(&self, key: &(String, String)) {
        self.delivery_retries.lock().unwrap().remove(key);
    }

    fn maintenance_due(&self) -> bool {
        let mut last = self.maintenance.lock().unwrap();
        if last.is_some_and(|last| last.elapsed() < std::time::Duration::from_secs(60)) {
            return false;
        }
        *last = Some(Instant::now());
        true
    }
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
        let terminal = matches!(status, "completed" | "failed" | "stopped");
        if terminal && t.actions == 0 {
            let removed = self
                .0
                .execute("DELETE FROM hook_rule_triggerings WHERE id=?", [&t.id])?;
            if removed > 0 {
                self.count_quiet(&t.rule_id)?;
                self.prune_hook_triggerings(&t.rule_id)?;
            }
            return Ok(());
        }
        let finished = terminal.then(now);
        let cursor = if terminal {
            Cursor::default()
        } else {
            t.cursor.clone()
        };
        let pending = (!terminal).then_some(t.pending.as_ref()).flatten();
        let cursor = serde_json::to_string(&cursor)?;
        if !terminal && cursor.len() > MAX_CURSOR_BYTES {
            return Err(Error::bad(
                "This triggering's control state exceeds 512 KiB.",
            ));
        }
        self.0
            .prepare_cached("UPDATE hook_rule_triggerings SET status=?2,cursor=?3,pending=?4,in_flight=?5,actions=?6,tool_calls=?7,counted_at=?8,verify=?9,verify_since=?10,next_at=?11,error=?12,updated_at=?13,finished_at=?14,lease_until=CASE WHEN ?2='running' THEN ?13+?15 ELSE NULL END WHERE id=?1")?
            .execute(params![
                t.id,
                status,
                cursor,
                pending.map(serde_json::to_string).transpose()?,
                !terminal && t.in_flight,
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
        if terminal {
            self.0.execute(
                "UPDATE hook_rule_steps SET status='failed',error=?2,finished_at=?3 \
                 WHERE triggering_id=?1 AND status IN ('running','waiting','retrying')",
                params![
                    t.id,
                    error.unwrap_or("The triggering ended before this step completed."),
                    finished
                ],
            )?;
            self.prune_hook_triggerings(&t.rule_id)?;
        }
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

    fn completed_step_values(&self, triggering: &str) -> Result<Map<String, Json>> {
        let mut values = Map::new();
        let mut statement = self.0.prepare_cached(
            "SELECT r.node_id,r.result FROM hook_rule_steps r WHERE r.triggering_id=?1 \
             AND r.status='completed' AND r.result IS NOT NULL AND r.position=( \
                 SELECT max(n.position) FROM hook_rule_steps n WHERE n.triggering_id=r.triggering_id \
                 AND n.node_id=r.node_id AND n.status='completed')",
        )?;
        for row in statement.query_map([triggering], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })? {
            let (node, value) = row?;
            values.insert(node, serde_json::from_str(&value)?);
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
        let input = input.map(Json::to_string);
        let result = result.map(Json::to_string);
        let error = error.map(|error| error.chars().take(4000).collect::<String>());
        let previous = self.0.query_row(
            "SELECT coalesce(length(CAST(input AS BLOB)),0),128+length(CAST(node_id AS BLOB)) \
             +length(CAST(label AS BLOB))+length(CAST(kind AS BLOB)) \
             +coalesce(length(CAST(input AS BLOB)),0)+coalesce(length(CAST(result AS BLOB)),0) \
             +coalesce(length(CAST(error AS BLOB)),0) FROM hook_rule_steps WHERE triggering_id=?1 AND position=?2",
            params![triggering, position], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
        ).optional()?;
        let input_bytes = previous.map_or_else(
            || input.as_ref().map_or(0, String::len),
            |(bytes, _)| bytes as usize,
        );
        let bytes = 128
            + node.id().len()
            + label.len()
            + kind.len()
            + input_bytes
            + result.as_ref().map_or(0, String::len)
            + error.as_ref().map_or(0, String::len);
        let updated = self.0.execute(
            "UPDATE hook_rule_triggerings SET recorded_bytes=recorded_bytes-?2+?3 \
             WHERE id=?1 AND recorded_bytes-?2+?3<=?4",
            params![
                triggering,
                previous.map_or(0, |(_, bytes)| bytes),
                bytes as i64,
                (MAX_RECORDED_BYTES - ERROR_RESERVE) as i64
            ],
        )?;
        if updated == 0 {
            return Err(Error::bad(
                "This triggering exceeded its 1 MiB recorded-data budget. The last action is not retried.",
            ));
        }
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
                input,
                result,
                error,
                attempts,
                at,
                finished,
            ])?;
        Ok(())
    }

    fn count_quiet(&self, rule: &str) -> Result<()> {
        self.0
            .prepare_cached("INSERT INTO hook_rule_quiet_hours(rule_id,hour,count) VALUES(?1,?2,1) ON CONFLICT(rule_id,hour) DO UPDATE SET count=count+1")?
            .execute(params![rule, now() / 3_600_000])?;
        Ok(())
    }

    /// Capacity refusal is a per-rule decision, with no payload retained and no
    /// external resolution. Its receipt prevents recounting if another listener defers.
    fn refuse_admission(&self, rule: &str, event: &str) -> Result<bool> {
        let full: bool = self.0.query_row(
            "SELECT ((SELECT count(*) FROM hook_rule_triggerings WHERE rule_id=?1 \
             AND status IN ('running','waiting','retrying')) + \
             (SELECT count(*) FROM hook_rule_admissions a WHERE a.rule_id=?1 AND a.event_id<>?3 \
             AND NOT EXISTS(SELECT 1 FROM hook_rule_receipts r WHERE r.rule_id=a.rule_id AND r.event_id=a.event_id)))>=?2",
            params![rule, MAX_PENDING, event],
            |row| row.get(0),
        )?;
        if !full {
            return Ok(false);
        }
        let recorded = self.0.execute(
            "INSERT OR IGNORE INTO hook_rule_receipts(rule_id,event_id) VALUES(?1,?2)",
            params![rule, event],
        )?;
        if recorded > 0 {
            self.0.execute(
                "INSERT INTO hook_rule_quiet_hours(rule_id,hour,count,not_admitted) VALUES(?1,?2,0,1) \
                 ON CONFLICT(rule_id,hour) DO UPDATE SET not_admitted=not_admitted+1", params![rule, now()/3_600_000],
            )?;
        }
        Ok(true)
    }

    /// Keeps the newest finished triggerings of a rule and a week of quiet hours.
    fn prune_hook_triggerings(&self, rule: &str) -> Result<()> {
        self.0.prepare_cached("DELETE FROM hook_rule_triggerings WHERE rule_id=?1 AND status IN ('completed','failed','stopped') AND id NOT IN (SELECT id FROM hook_rule_triggerings WHERE rule_id=?1 ORDER BY created_at DESC,id DESC LIMIT ?2)")?
            .execute(params![rule, KEPT])?;
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
    stop_triggerings(db, rule, None, reason)
}

fn stop_triggerings(db: &Db<'_>, rule: &str, version: Option<u32>, reason: &str) -> Result<()> {
    let at = now();
    db.0.execute(
        "INSERT OR IGNORE INTO hook_rule_receipts(rule_id,event_id) \
         SELECT rule_id,event_id FROM hook_rule_admissions WHERE rule_id=?1 AND (?2 IS NULL OR version=?2)",
        params![rule, version],
    )?;
    db.0.execute(
        "DELETE FROM hook_rule_admissions WHERE rule_id=?1 AND (?2 IS NULL OR version=?2)",
        params![rule, version],
    )?;

    let quiet = db.0.execute(
        "DELETE FROM hook_rule_triggerings WHERE rule_id=?1 AND (?2 IS NULL OR version=?2) \
         AND status IN ('running','waiting','retrying') AND actions=0",
        params![rule, version],
    )?;
    if quiet > 0 {
        db.0.execute(
            "INSERT INTO hook_rule_quiet_hours(rule_id,hour,count) VALUES(?1,?2,?3) \
             ON CONFLICT(rule_id,hour) DO UPDATE SET count=count+excluded.count",
            params![rule, at / 3_600_000, quiet as i64],
        )?;
    }
    db.0.execute(
        "UPDATE hook_rule_steps SET status='failed',error=CASE WHEN status='running' \
         THEN ?2||' The tool may already have taken effect.' ELSE ?2 END,finished_at=?3 \
         WHERE status IN ('running','waiting','retrying') AND triggering_id IN \
         (SELECT id FROM hook_rule_triggerings WHERE rule_id=?1 \
          AND (?4 IS NULL OR version=?4) AND status IN ('running','waiting','retrying'))",
        params![rule, reason, at, version],
    )?;
    db.0.execute(
        "UPDATE hook_rule_triggerings SET status='stopped',error=?2,in_flight=0,lease_until=NULL, \
         next_at=NULL,finished_at=?3,updated_at=?3,pending=NULL,cursor=?5 WHERE rule_id=?1 \
         AND (?4 IS NULL OR version=?4) AND status IN ('running','waiting','retrying')",
        params![
            rule,
            reason,
            at,
            version,
            serde_json::to_string(&Cursor::default())?
        ],
    )?;
    db.prune_hook_triggerings(rule)
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
fn payload_identifier(name: &str) -> bool {
    name.len() <= 40
        && name
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        && !RESERVED.contains(&name)
}

fn base(event: &Json, values: Map<String, Json>, started: i64) -> Variables {
    let mut variables = Variables::default();
    let payload = event["payload"].clone();
    if let Some(fields) = event["payloadFields"].as_array() {
        for name in fields
            .iter()
            .filter_map(Json::as_str)
            .filter(|name| payload_identifier(name))
        {
            variables.insert(name, Json::Null);
        }
    }
    if let Some(fields) = payload.as_object() {
        for (name, value) in fields {
            if payload_identifier(name) {
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

fn eligible_listener(db: &Db<'_>, rule: &str) -> Result<Option<(RuleRow, VersionRow)>> {
    let Some(row) = db.hook_rule(rule)? else {
        return Ok(None);
    };
    let Some(version) = db.hook_rule_version(rule, row.version)? else {
        return Ok(None);
    };
    if row.budget_exhausted_at.is_some() || blocked(db, &row, &version)?.is_some() {
        return Ok(None);
    }
    Ok(Some((row, version)))
}

/// Turns queued hook events into triggerings, then acknowledges them. An event no
/// rule may receive is dropped, so a quiet queue never blocks later events.
enum Delivery {
    Deferred,
    Handled,
}

fn hook_record(event: &crate::plugins::hooks::HookEvent, fields: &[String]) -> Json {
    json!({
        "id": event.id,
        "hook": event.name,
        "instanceId": event.instance_id,
        "payload": event.payload,
        "receivedAt": event.created_at,
        "payloadFields": fields,
    })
}

async fn deliver_hook(
    s: &Arc<Service>,
    event: &crate::plugins::hooks::HookEvent,
    fields: &[String],
    row: RuleRow,
    version: VersionRow,
) -> Result<Delivery> {
    let key = (event.id.clone(), row.id.clone());
    if s.rules.delivery_waiting(&key) {
        return Ok(if now() - event.created_at < DELIVERY_PATIENCE {
            Delivery::Deferred
        } else {
            Delivery::Handled
        });
    }
    let full = s
        .store
        .transaction({
            let (rule, event) = (row.id.clone(), event.id.clone());
            move |db| db.refuse_admission(&rule, &event)
        })
        .await?;
    if full {
        return Ok(Delivery::Handled);
    }
    let agent = version.definition.execution_agent_id.to_string();
    // Cairn never delivers an event whose payload designates a resource
    // outside the execution agent's hook grant. A resolution that failed for
    // another reason is tried again for a while.
    let delivery = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        crate::plugins::grant_authorization::authorize_hook_delivery(
            s,
            &agent,
            &event.instance_id,
            &event.name,
            &event.payload,
        ),
    )
    .await
    .unwrap_or_else(|_| Err(Error::unavailable("Hook resource resolution timed out.")));
    match delivery {
        Ok(()) => {}
        Err(error)
            if !matches!(error.status, 400 | 403 | 404)
                && now() - event.created_at < DELIVERY_PATIENCE =>
        {
            s.rules.defer_delivery(key);
            return Ok(Delivery::Deferred);
        }
        Err(_) => {
            s.rules.clear_delivery(&key);
            return Ok(Delivery::Handled);
        }
    }
    s.rules.clear_delivery(&key);
    let record = hook_record(event, fields);
    let (rule, number, event_id) = (row.id.clone(), version.version, event.id.clone());
    let inserted = s
    .store
    .transaction(move |db| {
        if !admissions::eligible(db, &rule, &event_id, number)? {
            return Ok(None);
        }
        if db.refuse_admission(&rule, &event_id)? { return Ok(None) }
        let recorded = record.to_string();
        let recorded_bytes = recorded.len() + 256;
        if recorded_bytes > MAX_RECORDED_BYTES - ERROR_RESERVE {
            return Err(Error::bad("The event exceeds the recorded-data budget."));
        }
        let received = db.0.execute(
            "INSERT OR IGNORE INTO hook_rule_receipts(rule_id,event_id) VALUES(?1,?2)",
            params![rule, event_id],
        )?;
        if received == 0 {
            return Ok(Some(false));
        }
        let cursor = serde_json::to_string(&Cursor::default())?;
        let at = now();
        let insert_sql = "INSERT OR IGNORE INTO hook_rule_triggerings( \
            id,rule_id,version,event_id,event,status,cursor,pending,in_flight, \
            actions,tool_calls,counted_at,verify,verify_since,next_at,lease_until, \
            error,created_at,updated_at,finished_at,recorded_bytes) \
            VALUES(?1,?2,?3,?4,?5,'running',?6,NULL,0,0,0,NULL,0,NULL,NULL,NULL,NULL,?7,?7,NULL,?8)";
        Ok(Some(db.0.prepare_cached(insert_sql)?
            .execute(params![id(), rule, number, event_id, recorded, cursor, at, recorded_bytes as i64])?
            > 0))
    })
    .await?;
    let _ = inserted;
    Ok(Delivery::Handled)
}

/// Copies each shared event to bounded, private admissions before acknowledging it.
/// No resource resolver can hold the instance's transport queue here.
async fn intake_instance(s: &Arc<Service>, instance: String) -> Result<usize> {
    let Some(connection) = s.store.plugin_instance(&instance).await? else {
        return Ok(0);
    };
    let Ok(package) = crate::plugins::registry::active(s, &connection.plugin_id).await else {
        return Ok(0);
    };
    let (revision, digest, queue) = (
        connection.revision.clone(),
        package.digest.clone(),
        instance,
    );
    let events = s
        .store
        .read(move |db| db.plugin_hooks(&queue, &revision, &digest, now()))
        .await?;
    let mut copied = 0;
    for event in events {
        let fields = package
            .manifest
            .hook(&event.name)
            .and_then(|hook| hook.payload_schema.get("properties"))
            .and_then(Json::as_object)
            .map(|fields| fields.keys().cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        copied += s
            .store
            .transaction(move |db| admissions::fanout(db, &event, &fields))
            .await?;
    }
    Ok(copied)
}

/// Queue discovery is read-only and never waits for a plugin resource resolver.
async fn queued_instances(s: &Service) -> Result<Vec<String>> {
    s.store
        .read(|db| {
            Ok(db
                .0
                .prepare_cached(
                    "SELECT DISTINCT instance_id FROM plugin_hook_events ORDER BY instance_id",
                )?
                .query_map([], |row| row.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?)
        })
        .await
}

/// Claims due triggerings. An action left in flight by an interrupted executor stays
/// ambiguous: its triggering fails rather than replaying it.
async fn claim(s: &Service, capacity: usize) -> Result<Vec<String>> {
    let due = s.store.read(|db| {
        Ok(db.0.query_row(
            "SELECT EXISTS(SELECT 1 FROM hook_rule_triggerings WHERE (status='running' \
             AND (lease_until IS NULL OR lease_until<?1)) OR (status IN ('waiting','retrying') AND next_at<=?1))",
            [now()], |row| row.get::<_, bool>(0),
        )?)
    }).await?;
    if !due {
        return Ok(Vec::new());
    }
    s.store
        .transaction(move |db| {
            let at = now();
            let interrupted = "Cairn stopped while this action was running. It is not run again, \
                since it may have taken effect.";
            db.0.prepare_cached(
                "UPDATE hook_rule_steps SET status='failed',error=?2,finished_at=?1 \
                 WHERE status='running' AND triggering_id IN (SELECT id FROM hook_rule_triggerings \
                 WHERE status='running' AND in_flight=1 AND lease_until<?1)",
            )?
            .execute(params![at, interrupted])?;
            db.0.prepare_cached(
                "UPDATE hook_rule_triggerings SET status='failed',error=?2,in_flight=0,\
                 lease_until=NULL,finished_at=?1,updated_at=?1,pending=NULL,cursor=?3 \
                 WHERE status='running' AND in_flight=1 AND lease_until<?1",
            )?
            .execute(params![at, interrupted, serde_json::to_string(&Cursor::default())?])?;
            // Member checks may wait on Beacon. Leave half of the executors
            // available to owner rules, even if older member work remains due.
            let members_running: i64 = db.0.query_row(
                "SELECT count(*) FROM hook_rule_triggerings t JOIN hook_rules r ON r.id=t.rule_id \
                 WHERE r.creator_owner=0 AND t.status='running' AND t.lease_until>=?1",
                [at], |row| row.get(0),
            )?;
            let member_capacity = (PARALLEL / 2).saturating_sub(members_running as usize);
            let mut due = Vec::new();
            for (owner, limit) in [(false, member_capacity), (true, capacity)] {
                let limit = limit.min(capacity.saturating_sub(due.len()));
                if limit == 0 {
                    continue;
                }
                let mut statement = db.0.prepare_cached(
                    "SELECT t.id FROM hook_rule_triggerings t JOIN hook_rules r ON r.id=t.rule_id \
                     WHERE r.creator_owner=?3 AND ((t.status='running' AND \
                     (t.lease_until IS NULL OR t.lease_until<?1)) OR (t.status IN ('waiting','retrying') \
                     AND t.next_at<=?1)) ORDER BY t.created_at,t.id LIMIT ?2",
                )?;
                due.extend(statement.query_map(params![at, limit as i64, owner], |row| row.get::<_, String>(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()?);
            }
            for id in &due {
                db.0.prepare_cached("UPDATE hook_rule_triggerings SET status='running',next_at=NULL,lease_until=?2 WHERE id=?1")?
                    .execute(params![id, at + LEASE])?;
            }
            Ok(due)
        })
        .await
}

fn sweep_rule(db: &Db<'_>, rule: &str) -> Result<()> {
    let Some(row) = db.hook_rule(rule)? else {
        return Ok(());
    };
    if let Some(version) = db.hook_rule_version(rule, row.version)? {
        if let Some(reason) = blocked(db, &row, &version)? {
            stop_rule(db, rule, &reason)?;
        }
    } else {
        stop_rule(db, rule, "The rule version no longer exists.")?;
    }
    let versions =
        db.0.prepare_cached(
            "SELECT version FROM hook_rule_triggerings WHERE rule_id=?1 \
         AND status IN ('running','waiting','retrying') \
         UNION SELECT version FROM hook_rule_admissions WHERE rule_id=?1",
        )?
        .query_map([rule], |row| row.get::<_, u32>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for number in versions {
        let reason = match db.hook_rule_version(rule, number)? {
            Some(version) => blocked(db, &row, &version)?,
            None => Some("The triggering's rule version no longer exists.".into()),
        };
        if let Some(reason) = reason {
            stop_triggerings(db, rule, Some(number), &reason)?;
        }
    }
    Ok(())
}

/// Stops the waiting triggerings of rules that became blocked in the meantime, such as
/// a lost right, rather than letting them resume once the right returns.
async fn sweep(s: &Service) -> Result<()> {
    let maintenance = s.rules.maintenance_due();
    let (active, retained, expired) = s.store.read(move |db| {
        let active = db.0.prepare_cached(
            "SELECT rule_id FROM hook_rule_triggerings WHERE status IN ('running','waiting','retrying') \
             UNION SELECT rule_id FROM hook_rule_admissions",
        )?.query_map([], |row| row.get::<_, String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
        let retained = if maintenance {
            db.0.prepare_cached("SELECT rule_id FROM hook_rule_triggerings UNION SELECT rule_id FROM hook_rule_quiet_hours")?
                .query_map([], |row| row.get::<_, String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?
        } else { Vec::new() };
        let expired = maintenance && db.0.query_row(
            "SELECT EXISTS(SELECT 1 FROM hook_rule_receipts WHERE counted_at IS NULL OR counted_at<=?1)",
            [now() - 3_600_000], |row| row.get::<_, bool>(0),
        )?;
        Ok((active, retained, expired))
    }).await?;
    if active.is_empty() && retained.is_empty() && !expired {
        return Ok(());
    }
    s.store
        .transaction(move |db| {
            for rule in active {
                if let Err(error) = sweep_rule(db, &rule) {
                    tracing::warn!(rule = %rule, error = %error, "Rule state is unavailable");
                    stop_rule(db, &rule, "The rule state could not be loaded.")?;
                }
            }
            for rule in retained {
                db.prune_hook_triggerings(&rule)?;
            }
            if !maintenance {
                return Ok(());
            }
            // Delivery receipts outlive quiet triggerings while another listener is
            // deferred. Counted receipts also meter the hour independently of history.
            db.0.execute(
                "DELETE FROM hook_rule_receipts WHERE (counted_at IS NULL OR counted_at<=?1) \
                 AND NOT EXISTS(SELECT 1 FROM plugin_hook_events e WHERE e.id=hook_rule_receipts.event_id) \
                 AND NOT EXISTS(SELECT 1 FROM hook_rule_admissions a \
                     WHERE a.rule_id=hook_rule_receipts.rule_id AND a.event_id=hook_rule_receipts.event_id) \
                 AND NOT EXISTS(SELECT 1 FROM hook_rule_triggerings t \
                     WHERE t.rule_id=hook_rule_receipts.rule_id AND t.event_id=hook_rule_receipts.event_id \
                     AND t.status IN ('running','waiting','retrying'))",
                [now() - 3_600_000],
            )?;
            Ok(())
        })
        .await
}

/// Drains due work with independent executors. Intake and waiting triggerings keep
/// progressing while a tool call occupies another executor slot.
pub async fn tick(s: &Arc<Service>) -> Result<usize> {
    let mut tasks = tokio::task::JoinSet::new();
    let mut intakes = tokio::task::JoinSet::new();
    let mut deliveries = tokio::task::JoinSet::new();
    let mut resolving = HashSet::new();
    let mut admitting = HashSet::new();
    let mut checked = HashSet::new();
    let mut ran = 0;
    loop {
        match queued_instances(s).await {
            Ok(instances) => {
                for instance in instances {
                    if intakes.len() >= PARALLEL {
                        break;
                    }
                    if admitting.contains(&instance) || !checked.insert(instance.clone()) {
                        continue;
                    }
                    admitting.insert(instance.clone());
                    let service = s.clone();
                    intakes.spawn(async move {
                        let result = intake_instance(&service, instance.clone()).await;
                        (instance, result)
                    });
                }
            }
            Err(error) => tracing::warn!(error = %error, "Rule queue discovery failed"),
        }
        match admissions::pending(s, &resolving).await {
            Ok(pending) => {
                for (rule, event) in pending
                    .into_iter()
                    .take(PARALLEL.saturating_sub(deliveries.len()))
                {
                    resolving.insert(rule.clone());
                    s.rules
                        .last_resolution
                        .lock()
                        .unwrap()
                        .insert(rule.clone(), Instant::now());
                    let service = s.clone();
                    deliveries.spawn(async move {
                        let result = admissions::deliver(&service, &rule, &event).await;
                        if result.is_err() {
                            service.rules.defer_delivery((event, rule.clone()));
                        }
                        (rule, result)
                    });
                }
            }
            Err(error) => tracing::warn!(error = %error, "Rule admission discovery failed"),
        }
        if let Err(error) = sweep(s).await {
            tracing::warn!(error = %error, "Rule history sweep failed");
        }

        let capacity = PARALLEL.saturating_sub(tasks.len());
        if capacity > 0 {
            let due = match claim(s, capacity).await {
                Ok(due) => due,
                Err(error) if tasks.is_empty() && intakes.is_empty() && deliveries.is_empty() => {
                    return Err(error);
                }
                Err(error) => {
                    tracing::warn!(error = %error, "Rule claiming failed; existing executors continue");
                    Vec::new()
                }
            };
            for id in due {
                ran += 1;
                let service = s.clone();
                tasks.spawn(async move {
                    let result = run(&service, &id).await;
                    (id, result)
                });
            }
        }

        if tasks.is_empty() && intakes.is_empty() && deliveries.is_empty() {
            return Ok(ran);
        }

        tokio::select! {
            () = s.shutdown.cancelled() => {
                tasks.abort_all();
                intakes.abort_all();
                deliveries.abort_all();
                return Ok(ran);
            }
            () = tokio::time::sleep(std::time::Duration::from_secs(2)) => { checked.clear(); }
            () = s.rules.wake.notified() => { checked.clear(); }
            completed = intakes.join_next(), if !intakes.is_empty() => {
                match completed {
                    Some(Ok((instance, result))) => {
                        admitting.remove(&instance);
                        if let Err(error) = result {
                            tracing::warn!(instance = %instance, error = %error, "Rule hook intake failed");
                        }
                    }
                    Some(Err(error)) => tracing::warn!(error = %error, "Rule intake stopped"),
                    None => {}
                }
            }
            completed = deliveries.join_next(), if !deliveries.is_empty() => {
                match completed {
                    Some(Ok((rule, result))) => {
                        resolving.remove(&rule);
                        if let Err(error) = result {
                            tracing::warn!(rule = %rule, error = %error, "Rule admission failed");
                        }
                    }
                    Some(Err(error)) => tracing::warn!(error = %error, "Rule admission stopped"),
                    None => {}
                }
            }
            completed = tasks.join_next(), if !tasks.is_empty() => {
                match completed {
                    Some(Ok((id, Err(error)))) => {
                        tracing::warn!(triggering = %id, error = %error, "Rule triggering failed to run");
                        let failed = id.clone();
                        let saved = s.store
                            .transaction(move |db| {
                                if let Some(t) = db.hook_triggering(&id)?
                                    && t.status == "running"
                                {
                                    db.save_hook_triggering(&t, "failed", None, Some(&error.message))?;
                                }
                                Ok(())
                            })
                            .await;
                        if let Err(error) = saved {
                            tracing::warn!(triggering = %failed, error = %error, "Rule failure could not be recorded");
                        }
                    }
                    Some(Err(error)) => tracing::warn!(error = %error, "Rule executor stopped"),
                    _ => {}
                }
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
            let Some(mut triggering) = db.hook_triggering(&id)? else {
                return Ok(None);
            };
            triggering.cursor.steps = db.completed_step_values(&id)?;
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
            let Some(current) = db.hook_triggering(&t.id)? else {
                return Ok(());
            };
            if current.status != "running" {
                return Ok(());
            }
            db.save_hook_triggering(&t, status, None, error.as_deref())?;
            Ok(())
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
            &row.id,
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
    let current = required(db.hook_rule(&row.id)?, "Rule not found")?;
    if current.budget_exhausted_at.is_some() {
        return Ok(false);
    }
    let cap = i64::from(version.definition.limits.triggerings_per_hour);
    let used: i64 = db
        .0
        .prepare_cached("SELECT count(*) FROM hook_rule_receipts WHERE rule_id=? AND counted_at>?")?
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
    let at = now();
    db.0.execute(
        "INSERT INTO hook_rule_receipts(rule_id,event_id,counted_at) \
         SELECT rule_id,event_id,?2 FROM hook_rule_triggerings WHERE id=?1 \
         ON CONFLICT(rule_id,event_id) DO UPDATE SET counted_at=excluded.counted_at",
        params![t.id, at],
    )?;
    t.counted_at = Some(at);
    Ok(true)
}

/// A deliberate budget lift gives subsequent triggerings a fresh window. Running
/// triggerings retain their counted marker and do not consume the new window again.
pub(crate) fn reset_budget(db: &Db<'_>, rule: &str) -> Result<()> {
    db.0.execute(
        "UPDATE hook_rule_receipts SET counted_at=NULL WHERE rule_id=?",
        [rule],
    )?;
    Ok(())
}

fn waiting_count(db: &Db<'_>, rule: &str, except: &str) -> Result<i64> {
    Ok(db.0.query_row(
        "SELECT count(*) FROM hook_rule_triggerings WHERE rule_id=?1 \
         AND (status='retrying' OR (status='waiting' AND verify_since IS NULL)) AND id<>?2",
        params![rule, except],
        |row| row.get(0),
    )?)
}

/// This path is only for preparation failures, before invocation can have effects.
fn pre_call_failure(error: Error) -> Result<Json> {
    if matches!(error.status, 408 | 429) || (error.status >= 500 && error.status != 502) {
        Ok(json!({ "retryAfter": 0, "error": error.message }))
    } else {
        Err(error)
    }
}

async fn call_tool(
    s: &Service,
    triggering: &str,
    agent: &str,
    instance: &str,
    tool: &str,
    input: &Json,
) -> Result<Json> {
    let prepared: Result<_> = async {
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
            crate::plugins::grant_authorization::Snapshot::capture(s, agent, None, &package)
                .await?;
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
            "agent": { "id": agent, "name": crate::validation::text(&profile, "name") },
            "instances": scope.instances(),
        });
        let triggering = triggering.to_owned();
        let allowed = s
            .store
            .transaction(move |db| {
                let Some(current) = db.hook_triggering(&triggering)? else {
                    return Ok(false);
                };
                if !current.in_flight || guard(db, &current)? {
                    return Ok(false);
                }
                Ok(db
                    .0
                    .query_row(
                        "SELECT lease_until>?2 FROM hook_rule_triggerings WHERE id=?1",
                        params![triggering, now()],
                        |row| row.get::<_, Option<bool>>(0),
                    )?
                    .unwrap_or(false))
            })
            .await?;
        if !allowed {
            return Err(Error::conflict(
                "This triggering stopped before the tool could be called.",
            ));
        }
        Ok((package, scope, payload))
    }
    .await;
    let (package, scope, payload) = match prepared {
        Ok(prepared) => prepared,
        Err(error) => return pre_call_failure(error),
    };
    tokio::time::timeout(
        TOOL_TIMEOUT,
        crate::plugins::scope::invoke(s, &package, "tool", payload, &scope),
    )
    .await
    .map_err(|_| Error::unavailable("The tool did not answer in time; it may have taken effect."))?
}

#[derive(Debug, PartialEq)]
enum ToolOutcome<'a> {
    Success,
    Retry(f64),
    Failed(&'a str),
}

fn retry_delay(value: &Json) -> Option<f64> {
    let seconds = value.as_f64().or_else(|| {
        let text = value.as_str()?.trim();
        text.parse::<f64>().ok().or_else(|| {
            chrono::DateTime::parse_from_rfc2822(text)
                .ok()
                .map(|date| ((date.timestamp_millis() - now()) as f64 / 1000.0).max(0.0))
        })
    })?;
    (seconds.is_finite() && seconds >= 0.0).then_some(seconds)
}

/// The legacy explicit no-effect reply and the bundled plugins' `ok` convention.
/// A status code alone never proves a mutation was not performed.
fn tool_outcome(result: &Json) -> ToolOutcome<'_> {
    let Some(fields) = result.as_object() else {
        return ToolOutcome::Success;
    };
    let legacy = fields.contains_key("retryAfter")
        && fields.len() <= 2
        && fields
            .keys()
            .all(|key| key == "retryAfter" || key == "error");
    if legacy {
        return retry_delay(&result["retryAfter"]).map_or(
            ToolOutcome::Failed("The plugin returned an invalid retry delay."),
            ToolOutcome::Retry,
        );
    }
    if result["ok"] != false {
        return ToolOutcome::Success;
    }
    if result["retryable"] == true {
        return match fields.get("retryAfter").filter(|delay| !delay.is_null()) {
            Some(delay) => retry_delay(delay).map_or(
                ToolOutcome::Failed("The plugin returned an invalid retry delay."),
                ToolOutcome::Retry,
            ),
            None => ToolOutcome::Retry(0.0),
        };
    }
    ToolOutcome::Failed(
        result["message"]
            .as_str()
            .or_else(|| result["error"].as_str())
            .unwrap_or("The plugin reported a failure."),
    )
}

fn retry_after(result: &Json) -> Option<f64> {
    match tool_outcome(result) {
        ToolOutcome::Retry(delay) => Some(delay),
        _ => None,
    }
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

/// What the interpreter decided before an effect, computed off the async runtime.
enum Plan {
    Done,
    Fail(String),
    Act(Box<Acting>),
}

/// An action and its evaluated input, with the cursor standing after it.
struct Acting {
    node: RuleNode,
    input: Json,
    cursor: Cursor,
}

/// How long the pure part of one step may take: walking blocks and evaluating values.
const EVALUATION: std::time::Duration = std::time::Duration::from_secs(10);

fn actions(nodes: &[RuleNode], ids: &mut Vec<String>) {
    for node in nodes {
        match node {
            RuleNode::If {
                then, otherwise, ..
            } => {
                actions(then, ids);
                actions(otherwise, ids);
            }
            RuleNode::ForEach { body, .. } => actions(body, ids),
            RuleNode::Action { id, .. } => ids.push(id.clone()),
        }
    }
}

/// Walks to the next action and evaluates its input. A pending action keeps the input
/// it was evaluated with.
fn plan(
    definition: &cairn_contracts::HookRuleDefinition,
    mut cursor: Cursor,
    pending: Option<Pending>,
    mut base: Variables,
) -> Plan {
    if let Err(error) = evaluate::check_deadline() {
        return Plan::Fail(error);
    }
    let steps = &definition.steps;
    if let Some(pending) = pending {
        return match node(steps, &pending.node_id) {
            Some(node) => Plan::Act(Box::new(Acting {
                node: node.clone(),
                input: pending.input,
                cursor,
            })),
            None => Plan::Fail("A pending step is missing from its rule version.".into()),
        };
    }
    let mut ids = Vec::new();
    actions(steps, &mut ids);
    base.insert("steps", cursor.steps(&ids));
    let evaluator = match evaluate::Evaluator::new(&base) {
        Ok(evaluator) => evaluator,
        Err(error) => return Plan::Fail(error),
    };
    let node = match cursor.advance(steps, &evaluator) {
        Ok(Some(node)) => node,
        Ok(None) => return Plan::Done,
        Err(error) => return Plan::Fail(error),
    };
    let RuleNode::Action { action, .. } = node else {
        return Plan::Fail("The interpreter stopped on a block.".into());
    };
    let items = cursor.items();
    let input = match action {
        RuleAction::CallTool { input, .. } => input
            .iter()
            .map(|(key, value)| Ok((key.clone(), evaluator.value(value, &items)?)))
            .collect::<std::result::Result<Map<_, _>, String>>()
            .map(Json::Object),
        RuleAction::StoreValue {
            scope: RuleValueScope::Rule,
            value,
            ..
        } => evaluator.value(value, &items),
        RuleAction::Wait { seconds } => evaluator.value(seconds, &items),
        _ => Err(format!(
            "the {} action needs conversation keys, which are not available yet.",
            action_kind(action)
        )),
    };
    match input {
        Ok(input)
            if serde_json::to_vec(&input)
                .map_or(true, |bytes| bytes.len() > evaluate::MAX_BYTES) =>
        {
            Plan::Fail(format!("{}: the action input exceeds 256 KiB.", node.id()))
        }
        Ok(input) => Plan::Act(Box::new(Acting {
            node: node.clone(),
            input,
            cursor,
        })),
        Err(error) => Plan::Fail(format!("{}: {error}", node.id())),
    }
}

/// Stops a triggering whose rule must not act anymore, in the transaction that would
/// record its next effect. Returns whether it stopped.
fn guard(db: &Db<'_>, t: &Triggering) -> Result<bool> {
    let Some(current) = db.hook_triggering(&t.id)? else {
        return Ok(true);
    };
    if current.status != "running" {
        return Ok(true);
    }
    let Some(row) = db.hook_rule(&t.rule_id)? else {
        return Ok(true);
    };
    for number in [row.version, t.version] {
        let version = required(
            db.hook_rule_version(&t.rule_id, number)?,
            "Rule version not found",
        )?;
        if let Some(reason) = blocked(db, &row, &version)? {
            db.save_hook_triggering(&current, "stopped", None, Some(&reason))?;
            return Ok(true);
        }
    }
    Ok(false)
}

/// A late answer can complete the step's history, but never revive a triggering
/// stopped by its creator, the owner, lost rights or an expired executor lease.
fn record_late_tool_result(
    db: &Db<'_>,
    t: &Triggering,
    node: &RuleNode,
    position: u32,
    attempts: u32,
    answer: &Result<Json>,
) -> Result<()> {
    let record = match answer {
        Ok(result)
            if serde_json::to_vec(result)
                .map_or(true, |bytes| bytes.len() > evaluate::MAX_BYTES) =>
        {
            StepRecord {
                status: "failed",
                input: None,
                result: None,
                error: Some("The tool's result is larger than 256 KiB."),
            }
        }
        Ok(result) if retry_after(result).is_some() => StepRecord {
            status: "failed",
            input: None,
            result: Some(result),
            error: Some("The triggering stopped before this action could be retried."),
        },
        Ok(result) if matches!(tool_outcome(result), ToolOutcome::Failed(_)) => {
            let ToolOutcome::Failed(error) = tool_outcome(result) else {
                unreachable!()
            };
            StepRecord {
                status: "failed",
                input: None,
                result: Some(result),
                error: Some(error),
            }
        }
        Ok(result) => StepRecord {
            status: "completed",
            input: None,
            result: Some(result),
            error: None,
        },
        Err(error) => StepRecord {
            status: "failed",
            input: None,
            result: None,
            error: Some(&error.message),
        },
    };
    db.record_step(&t.id, position, node, attempts, record)?;
    db.0.execute(
        "UPDATE hook_rule_triggerings SET in_flight=0,pending=NULL,updated_at=?2 WHERE id=?1",
        params![t.id, now()],
    )?;
    Ok(())
}

/// Fails a triggering before an effect, without recording one.
fn refuse(db: &Db<'_>, t: &Triggering, error: &str) -> Result<Step> {
    db.save_hook_triggering(t, "failed", None, Some(error))?;
    Ok(Step::Done)
}

async fn step(s: &Arc<Service>, id: &str) -> Result<Step> {
    let resumed = Instant::now();
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
    let (definition, cursor, pending) = (
        version.definition.clone(),
        triggering.cursor.clone(),
        triggering.pending.clone(),
    );
    let deadline = std::time::Instant::now() + EVALUATION;
    let planned = tokio::time::timeout(
        EVALUATION,
        tokio::task::spawn_blocking(move || {
            evaluate::with_deadline(deadline, || plan(&definition, cursor, pending, base))
        }),
    )
    .await;
    let (node, input, cursor) = match planned {
        Err(_) => {
            let error = "Evaluating this step took more than 10 seconds.";
            finish(s, triggering, "failed", Some(error.into())).await?;
            return Ok(Step::Done);
        }
        Ok(Err(error)) => return Err(Error::internal(error)),
        Ok(Ok(Plan::Done)) => {
            finish(s, triggering, "completed", None).await?;
            return Ok(Step::Done);
        }
        Ok(Ok(Plan::Fail(error))) => {
            finish(s, triggering, "failed", Some(error)).await?;
            return Ok(Step::Done);
        }
        Ok(Ok(Plan::Act(acting))) => {
            let Acting {
                node,
                input,
                cursor,
            } = *acting;
            (node, input, cursor)
        }
    };
    // A member's triggering acts only on a recently verified membership, and checks it
    // again after each wait. The cursor stays where it is until it does.
    if !row.creator_owner {
        let verified = if triggering.verify {
            s.missions.verify_membership_since(s, resumed).await
        } else {
            s.missions
                .verify_membership(s, std::time::Duration::from_secs(60))
                .await
        };
        if verified.is_err() {
            triggering.verify = true;
            let since = *triggering.verify_since.get_or_insert_with(now);
            if now() - since > MEMBERSHIP_PATIENCE {
                let error = "Its creator's membership could not be verified for seven days.";
                finish(s, triggering, "failed", Some(error.into())).await?;
            } else {
                let at = now() + 60_000;
                let note = "Waiting to verify its creator's membership.";
                s.store
                    .transaction(move |db| {
                        if guard(db, &triggering)? {
                            return Ok(());
                        }
                        db.save_hook_triggering(&triggering, "waiting", Some(at), Some(note))
                    })
                    .await?;
            }
            return Ok(Step::Done);
        }
        triggering.verify = false;
        triggering.verify_since = None;
    }

    let RuleNode::Action { action, .. } = &node else {
        return Err(Error::internal("The interpreter stopped on a block."));
    };
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
            instance_id, tool, ..
        } => {
            // Persist the action as in flight before it can take effect.
            let mut prepared = triggering.clone();
            prepared.cursor = cursor;
            prepared.pending = Some(Pending {
                node_id: node.id().to_owned(),
                position,
                attempts,
                input: input.clone(),
            });
            prepared.in_flight = true;
            if attempts == 1 {
                prepared.actions += 1;
            }
            prepared.tool_calls += 1;
            let (record, recorded_input) = (node.clone(), input.clone());
            let go = s
                .store
                .transaction(move |db| {
                    if db.hook_triggering(&triggering.id)?.is_none() || guard(db, &triggering)? {
                        return Ok(None);
                    }
                    if prepared.tool_calls > validation::MAX_TOOL_CALLS {
                        let error = format!(
                            "This triggering called more than {} tools.",
                            validation::MAX_TOOL_CALLS
                        );
                        refuse(db, &triggering, &error)?;
                        return Ok(None);
                    }
                    if !count(db, &mut prepared, &row, &version)? {
                        refuse(
                            db,
                            &triggering,
                            "The rule reached its hourly limit of triggerings.",
                        )?;
                        return Ok(None);
                    }
                    let running = StepRecord {
                        status: "running",
                        input: Some(&recorded_input),
                        result: None,
                        error: None,
                    };
                    db.record_step(&prepared.id, position, &record, attempts, running)?;
                    db.save_hook_triggering(&prepared, "running", None, None)?;
                    Ok(Some(prepared))
                })
                .await?;
            let Some(mut prepared) = go else {
                return Ok(Step::Done);
            };
            let result = call_tool(
                s,
                &prepared.id,
                &agent,
                &instance_id.to_string(),
                tool,
                &input,
            )
            .await;
            s.store
                .transaction(move |db| {
                    let Some(current) = db.hook_triggering(&prepared.id)? else {
                        return Ok(Step::Done);
                    };
                    if current.status != "running" || !current.in_flight {
                        record_late_tool_result(db, &current, &node, position, attempts, &result)?;
                        return Ok(Step::Done);
                    }
                    prepared.in_flight = false;
                    let result = match result {
                        Ok(result) => result,
                        Err(error) => {
                            let failed = StepRecord {
                                status: "failed",
                                input: None,
                                result: None,
                                error: Some(&error.message),
                            };
                            db.record_step(&prepared.id, position, &node, attempts, failed)?;
                            db.save_hook_triggering(
                                &prepared,
                                "failed",
                                None,
                                Some(&error.message),
                            )?;
                            return Ok(Step::Done);
                        }
                    };
                    if serde_json::to_vec(&result)
                        .map_or(true, |bytes| bytes.len() > evaluate::MAX_BYTES)
                    {
                        let error = "The tool's result is larger than 256 KiB.";
                        let failed = StepRecord {
                            status: "failed",
                            input: None,
                            result: None,
                            error: Some(error),
                        };
                        db.record_step(&prepared.id, position, &node, attempts, failed)?;
                        db.save_hook_triggering(&prepared, "failed", None, Some(error))?;
                        return Ok(Step::Done);
                    }
                    if let ToolOutcome::Failed(error) = tool_outcome(&result) {
                        let failed = StepRecord {
                            status: "failed",
                            input: None,
                            result: Some(&result),
                            error: Some(error),
                        };
                        db.record_step(&prepared.id, position, &node, attempts, failed)?;
                        db.save_hook_triggering(&prepared, "failed", None, Some(error))?;
                        return Ok(Step::Done);
                    }
                    if let Some(seconds) = retry_after(&result) {
                        if attempts >= MAX_ATTEMPTS {
                            let error =
                                format!("The service refused this action {attempts} times.");
                            let failed = StepRecord {
                                status: "failed",
                                input: None,
                                result: Some(&result),
                                error: Some(&error),
                            };
                            db.record_step(&prepared.id, position, &node, attempts, failed)?;
                            db.save_hook_triggering(&prepared, "failed", None, Some(&error))?;
                            return Ok(Step::Done);
                        }
                        let backoff = 2f64.powi(attempts as i32).min(3600.0);
                        let at = now();
                        let delay = (seconds.max(backoff) * 1000.0).ceil();
                        if !delay.is_finite()
                            || delay > f64::from(validation::MAX_WAIT_SECONDS) * 1000.0
                        {
                            let error = "The service requested a retry more than seven days away.";
                            let failed = StepRecord {
                                status: "failed",
                                input: None,
                                result: Some(&result),
                                error: Some(error),
                            };
                            db.record_step(&prepared.id, position, &node, attempts, failed)?;
                            db.save_hook_triggering(&prepared, "failed", None, Some(error))?;
                            return Ok(Step::Done);
                        }
                        if waiting_count(db, &prepared.rule_id, &prepared.id)? >= MAX_WAITING {
                            let error = "This rule already has 20 triggerings waiting.";
                            let failed = StepRecord {
                                status: "failed",
                                input: None,
                                result: Some(&result),
                                error: Some(error),
                            };
                            db.record_step(&prepared.id, position, &node, attempts, failed)?;
                            db.save_hook_triggering(&prepared, "failed", None, Some(error))?;
                            return Ok(Step::Done);
                        }
                        prepared.verify = true;
                        let resumes_at = at + delay as i64;
                        let retrying = StepRecord {
                            status: "retrying",
                            input: None,
                            result: Some(&result),
                            error: None,
                        };
                        db.record_step(&prepared.id, position, &node, attempts, retrying)?;
                        db.save_hook_triggering(&prepared, "retrying", Some(resumes_at), None)?;
                        return Ok(Step::Done);
                    }
                    let completed = StepRecord {
                        status: "completed",
                        input: None,
                        result: Some(&result),
                        error: None,
                    };
                    db.record_step(&prepared.id, position, &node, attempts, completed)?;
                    prepared.cursor.steps.insert(node.id().to_owned(), result);
                    prepared.pending = None;
                    db.save_hook_triggering(&prepared, "running", None, None)?;
                    Ok(Step::Continue)
                })
                .await
        }
        RuleAction::StoreValue { name, .. } => {
            if serde_json::to_vec(&input).map_or(true, |bytes| bytes.len() > 65_536) {
                let error = format!("{}: a remembered value has at most 64 KiB.", node.id());
                finish(s, triggering, "failed", Some(error)).await?;
                return Ok(Step::Done);
            }
            let name = name.clone();
            s.store
                .transaction(move |db| {
                    if db.hook_triggering(&triggering.id)?.is_none() || guard(db, &triggering)? {
                        return Ok(Step::Done);
                    }
                    let state_bytes: i64 = db.0.query_row(
                        "SELECT coalesce(sum(length(CAST(value AS BLOB))),0) FROM hook_rule_values WHERE rule_id=?1 AND name<>?2",
                        params![row.id, name], |row| row.get(0),
                    )?;
                    if state_bytes as usize + input.to_string().len() > MAX_RULE_VALUES_BYTES {
                        return refuse(db, &triggering, "This rule's remembered values exceed 1 MiB.");
                    }
                    let mut stored = triggering.clone();
                    stored.cursor = cursor;
                    stored.actions += 1;
                    if !count(db, &mut stored, &row, &version)? {
                        return refuse(db, &triggering, "The rule reached its hourly limit of triggerings.");
                    }
                    db.0.prepare_cached(
                        "INSERT INTO hook_rule_values VALUES(?1,?2,?3,?4) \
                         ON CONFLICT(rule_id,name) DO UPDATE SET value=excluded.value,updated_at=excluded.updated_at",
                    )?
                    .execute(params![row.id, name, input.to_string(), now()])?;
                    let completed = StepRecord {
                        status: "completed",
                        input: Some(&input),
                        result: Some(&input),
                        error: None,
                    };
                    db.record_step(&stored.id, position, &node, 1, completed)?;
                    stored.cursor.steps.insert(node.id().to_owned(), input.clone());
                    db.save_hook_triggering(&stored, "running", None, None)?;
                    Ok(Step::Continue)
                })
                .await
        }
        RuleAction::Wait { .. } => {
            let seconds = input.as_f64().filter(|seconds| {
                (1.0..=f64::from(validation::MAX_WAIT_SECONDS)).contains(seconds)
            });
            let Some(seconds) = seconds else {
                let error = format!("{}: wait from 1 second to 7 days.", node.id());
                finish(s, triggering, "failed", Some(error)).await?;
                return Ok(Step::Done);
            };
            s.store
                .transaction(move |db| {
                    if db.hook_triggering(&triggering.id)?.is_none() || guard(db, &triggering)? {
                        return Ok(Step::Done);
                    }
                    let waiting = waiting_count(db, &row.id, &triggering.id)?;
                    if waiting >= MAX_WAITING {
                        let error =
                            format!("This rule already has {MAX_WAITING} triggerings waiting.");
                        return refuse(db, &triggering, &error);
                    }
                    let mut waiting = triggering.clone();
                    waiting.cursor = cursor;
                    waiting.actions += 1;
                    if !count(db, &mut waiting, &row, &version)? {
                        return refuse(
                            db,
                            &triggering,
                            "The rule reached its hourly limit of triggerings.",
                        );
                    }
                    let resumes = now() + (seconds * 1000.0) as i64;
                    let result = json!({ "resumesAt": resumes });
                    let completed = StepRecord {
                        status: "completed",
                        input: None,
                        result: Some(&result),
                        error: None,
                    };
                    db.record_step(&waiting.id, position, &node, 1, completed)?;
                    waiting.cursor.steps.insert(node.id().to_owned(), result);
                    waiting.verify = true;
                    db.save_hook_triggering(&waiting, "waiting", Some(resumes), None)?;
                    Ok(Step::Done)
                })
                .await
        }
        _ => Err(Error::internal(
            "The planner returned an unsupported action.",
        )),
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

const PAGE: usize = 50;

/// Details follow the execution agent of the version a triggering ran with.
fn visible_for(db: &Db<'_>, row: &RuleRow, version: u32, viewer: &super::Viewer) -> Result<bool> {
    let version = required(
        db.hook_rule_version(&row.id, version)?,
        "Rule version not found",
    )?;
    details_visible(db, row, &version.definition, viewer)
}

pub(crate) fn list(
    db: &Db<'_>,
    input: &cairn_contracts::ListHookRuleTriggeringsInput,
    viewer: &super::Viewer,
) -> Result<cairn_contracts::ListHookRuleTriggeringsOutput> {
    let rule = input.rule_id.to_string();
    let row = required(db.hook_rule(&rule)?, "Rule not found")?;
    // The cursor is the creation time and identity of the last triggering shown.
    let (before, after_id) = match input.cursor.as_deref() {
        Some(cursor) => {
            let (at, id) = cursor
                .split_once(':')
                .ok_or_else(|| Error::bad("Invalid triggering cursor."))?;
            let at = at
                .parse::<i64>()
                .map_err(|_| Error::bad("Invalid triggering cursor."))?;
            (at, id.to_owned())
        }
        None => (i64::MAX, String::new()),
    };
    let ids =
        db.0.prepare_cached(
            "SELECT id,created_at,version FROM hook_rule_triggerings WHERE rule_id=?1 \
             AND (created_at<?2 OR (created_at=?2 AND id<?3)) \
             ORDER BY created_at DESC,id DESC LIMIT ?4",
        )?
        .query_map(params![rule, before, after_id, PAGE as i64 + 1], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, u32>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let next_cursor = (ids.len() > PAGE).then(|| {
        let (id, at, _) = &ids[PAGE - 1];
        format!("{at}:{id}")
    });
    let mut visibility = std::collections::BTreeMap::new();
    let mut triggerings = Vec::new();
    for (id, _, version) in ids.into_iter().take(PAGE) {
        let visible = match visibility.get(&version) {
            Some(visible) => *visible,
            None => {
                let visible = visible_for(db, &row, version, viewer)?;
                visibility.insert(version, visible);
                visible
            }
        };
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
        next_cursor,
        not_admitted: Some(db.0.query_row(
            "SELECT coalesce(sum(not_admitted),0) FROM hook_rule_quiet_hours WHERE rule_id=? AND hour>=?",
            params![rule, now()/3_600_000 - 23], |row| row.get::<_, u32>(0),
        )?),
    })
}

pub(crate) fn get(
    db: &Db<'_>,
    id: &str,
    viewer: &super::Viewer,
) -> Result<cairn_contracts::HookRuleTriggering> {
    let t = required(db.hook_triggering(id)?, "Triggering not found")?;
    let row = required(db.hook_rule(&t.rule_id)?, "Rule not found")?;
    let visible = visible_for(db, &row, t.version, viewer)?;
    view(db, id, visible, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn member_work_leaves_executor_capacity_for_owner_rules() {
        let root = tempfile::tempdir().unwrap();
        let service = Service::new(crate::common::config(root.path()))
            .await
            .unwrap();
        service.store.transaction(|db| {
            for (rule, owner) in [("member", false), ("owner", true)] {
                db.0.execute(
                    "INSERT INTO hook_rules(id,creator_account_id,creator_owner,revision,version,created_at,updated_at) \
                     VALUES(?1,?1,?2,'revision',1,1,1)", params![rule, owner],
                )?;
            }
            for index in 0..50 {
                db.0.execute(
                    "INSERT INTO hook_rule_triggerings(id,rule_id,version,event_id,event,status,cursor,in_flight,actions,tool_calls,verify,created_at,updated_at) \
                     VALUES(?1,'member',1,?1,'{}','running','{}',0,0,0,1,1,1)",
                    [format!("member-{index}")],
                )?;
            }
            Ok(())
        }).await.unwrap();
        let first = claim(&service, PARALLEL).await.unwrap();
        assert_eq!(first.len(), PARALLEL / 2);
        service.store.transaction(|db| {
            db.0.execute(
                "INSERT INTO hook_rule_triggerings(id,rule_id,version,event_id,event,status,cursor,in_flight,actions,tool_calls,verify,created_at,updated_at) \
                 VALUES('owner-triggering','owner',1,'owner-event','{}','running','{}',0,0,0,0,2,2)", [],
            )?;
            Ok(())
        }).await.unwrap();
        assert_eq!(
            claim(&service, PARALLEL - first.len()).await.unwrap(),
            ["owner-triggering"]
        );
        // More old member work cannot take the reserved slots on a later pass.
        assert!(claim(&service, PARALLEL).await.unwrap().is_empty());
    }

    #[test]
    fn only_explicit_safe_replies_or_known_pre_call_refusals_are_retryable() {
        for status in [408, 429, 500, 503, 504] {
            let reply = pre_call_failure(Error::new(status, "not invoked")).unwrap();
            assert_eq!(tool_outcome(&reply), ToolOutcome::Retry(0.0));
        }
        for status in [400, 401, 403, 404, 409, 502] {
            assert!(pre_call_failure(Error::new(status, "permanent refusal")).is_err());
        }
        assert_eq!(
            tool_outcome(&json!({"ok": false, "status": 429, "retryable": false})),
            ToolOutcome::Failed("The plugin reported a failure.")
        );
        assert_eq!(
            tool_outcome(&json!({"ok": true, "error": "domain data"})),
            ToolOutcome::Success
        );
        assert_eq!(
            tool_outcome(&json!({"retryAfter": -1})),
            ToolOutcome::Failed("The plugin returned an invalid retry delay.")
        );
    }

    #[test]
    fn retry_after_accepts_numeric_headers_and_http_dates() {
        assert_eq!(retry_delay(&json!("3600")), Some(3600.0));
        assert_eq!(
            retry_delay(&json!("Sun, 06 Nov 1994 08:49:37 GMT")),
            Some(0.0)
        );
        assert_eq!(retry_delay(&json!("not a date")), None);
        assert_eq!(retry_delay(&json!("NaN")), None);
        assert_eq!(retry_delay(&json!("inf")), None);
    }
}
