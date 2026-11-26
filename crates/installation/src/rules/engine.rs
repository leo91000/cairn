//! The rule engine (ADR-0075, « Règles de hook » and « Budget du moteur de règles »).
//!
//! Intake turns each queued plugin hook event into one durable triggering per rule
//! that listens to it and may receive it, pinned to the rule's current version. The
//! executor then walks the tree: before every effect it persists where the triggering
//! stands, so that an interrupted action is never replayed, and it checks the rule's
//! blocks, its creator's membership and its budget again.
use super::{
    evaluate::{self, Variables},
    interpreter::{Cursor, Facts},
    storage::{RuleRow, VersionRow},
    validation::{self, RESERVED},
};
use crate::{
    config::{id, now},
    error::{Error, Result, required},
    service::Service,
    store::Db,
};
use cairn_contracts::{
    PluginContributionKind as Kind, RuleAction, RuleNode, RuleValue, RuleValueScope,
};

mod admissions;
mod cairn_hooks;

pub(crate) use cairn_hooks::record_question;

pub(crate) mod conversations;

use rusqlite::{OptionalExtension, params};
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
 event_bytes INTEGER GENERATED ALWAYS AS(length(CAST(event AS BLOB))+length(CAST(fields AS BLOB))) STORED,
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
 event_bytes INTEGER GENERATED ALWAYS AS(length(CAST(event AS BLOB))) STORED,
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

/// How long intake retries an event whose resources could not be resolved.
const DELIVERY_PATIENCE: i64 = 10 * 60_000;
/// Triggerings waiting at once, per rule.
pub(crate) const MAX_WAITING: i64 = 20;
pub(crate) const MAX_PENDING: i64 = 1000;
/// Event bytes retained by unresolved admissions and active triggerings, per rule.
pub(crate) const MAX_PENDING_EVENT_BYTES: usize = 16 * 1024 * 1024;
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
/// How often one engine pass reads the hook queues again without a wakeup.
const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);

/// The engine's wake-up, shared by intake and the executor.
#[derive(Default)]
pub struct Engine {
    pub(crate) wake: tokio::sync::Notify,
    delivery_retries: Mutex<HashMap<(String, String), (Instant, u32)>>,
    maintenance: Mutex<Option<Instant>>,
    last_resolution: Mutex<HashMap<String, Instant>>,
    last_execution: Mutex<HashMap<String, Instant>>,
}

impl Engine {
    /// Tests only: signal a hook event queued directly, as the plugin transport does.
    #[doc(hidden)]
    pub fn notify_queued(&self) {
        self.wake.notify_one();
    }

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

mod intake;
mod steps;
mod storage;

use intake::*;
pub(crate) use steps::*;

use storage::*;

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

/// Drains due work with independent executors. Intake and waiting triggerings keep
/// progressing while a tool call occupies another executor slot.
pub async fn tick(s: &Arc<Service>) -> Result<usize> {
    let mut tasks = tokio::task::JoinSet::new();
    let mut running = HashMap::new();
    let mut intakes = tokio::task::JoinSet::new();
    let mut deliveries = tokio::task::JoinSet::new();
    let mut resolving = HashSet::new();
    let mut admitting = HashSet::new();
    let mut checked = HashSet::new();
    let mut ran = 0;
    // Queues are read again at an absolute deadline: a continuous flow of completions
    // must not postpone it.
    let mut poll_at = Instant::now() + POLL_INTERVAL;
    loop {
        if Instant::now() >= poll_at {
            checked.clear();
            poll_at = Instant::now() + POLL_INTERVAL;
        }
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
        if let Err(error) = cairn_hooks::observe(s).await {
            tracing::warn!(error = %error, "Cairn hook observation failed");
        }

        let capacity = PARALLEL.saturating_sub(tasks.len());
        if capacity > 0 {
            let excluded = running.values().cloned().collect();
            let due = match claim(s, capacity, &excluded).await {
                Ok(due) => due,
                Err(error) if tasks.is_empty() && intakes.is_empty() && deliveries.is_empty() => {
                    return Err(error);
                }
                Err(error) => {
                    tracing::warn!(error = %error, "Rule claiming failed; existing executors continue");
                    Vec::new()
                }
            };
            for (id, rule) in due {
                ran += 1;
                let service = s.clone();
                let task = tasks.spawn(async move {
                    let result = run(&service, &id).await;
                    (id, result)
                });
                running.insert(task.id(), rule);
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
            () = tokio::time::sleep_until(poll_at.into()) => {}
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
            completed = tasks.join_next_with_id(), if !tasks.is_empty() => {
                if let Some(completed) = &completed {
                    let task_id = match completed { Ok((id, _)) => *id, Err(error) => error.id() };
                    running.remove(&task_id);
                }
                match completed {
                    Some(Ok((_, (id, Err(error))))) => {
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
    // Cairn already paces progress: its triggerings do not count against the cap.
    if t.event["cairn"].is_object() && t.event["hook"] == "progress" {
        t.counted_at = Some(now());
        return Ok(true);
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
    db.0.execute(
        "DELETE FROM hook_rule_conversation_counts WHERE rule_id=?",
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
