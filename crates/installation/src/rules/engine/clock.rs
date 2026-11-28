//! Cairn's Clock (`Horloge`) hook (ADR-0094): schedules cron and one-shot rules,
//! emits a single catch-up triggering after an interruption, and supports manual
//! immediate runs ("Run now").
use super::*;
use crate::rules::{
    Actor, Author, Viewer,
    storage::{RuleRow, ScheduleRow},
};
use cairn_contracts::{HookRuleDefinition, HookRuleInput, HookRuleTrigger, HookRuleTriggering};

fn rfc3339(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .unwrap_or_default()
        .to_rfc3339()
}

pub(crate) fn next_for_clock(
    trigger: &HookRuleTrigger,
    at: i64,
    existing: Option<ScheduleRow>,
    trigger_changed: bool,
) -> Result<Option<i64>> {
    match trigger {
        HookRuleTrigger::Clock {
            cron: Some(pattern),
            at: None,
            timezone,
        } => {
            if !trigger_changed && let Some(next) = existing.as_ref().and_then(|row| row.next_run) {
                return Ok(Some(next));
            }
            Ok(crate::service::next_occurrences(pattern, timezone, at, 1)?
                .into_iter()
                .next())
        }
        HookRuleTrigger::Clock {
            cron: None,
            at: Some(target),
            ..
        } => {
            if trigger_changed {
                return Ok(Some(*target));
            }
            if existing.as_ref().is_some_and(|row| row.fired_at.is_some()) {
                return Ok(None);
            }
            if let Some(next) = existing.as_ref().and_then(|row| row.next_run) {
                return Ok(Some(next));
            }
            Ok((*target > at).then_some(*target))
        }
        _ => Ok(None),
    }
}

/// Synchronizes `hook_rule_schedules` for a rule after creation, edit or state change.
pub(crate) fn sync_schedule(
    db: &Db<'_>,
    row: &RuleRow,
    definition: &HookRuleDefinition,
    trigger_changed: bool,
) -> Result<Option<i64>> {
    if !matches!(definition.trigger, HookRuleTrigger::Clock { .. }) {
        db.delete_hook_rule_schedule(&row.id)?;
        return Ok(None);
    }

    let existing = db.hook_rule_schedule(&row.id)?;
    let last_scheduled_for = if trigger_changed {
        None
    } else {
        existing.as_ref().and_then(|s| s.last_scheduled_for)
    };
    let fired_at = if trigger_changed {
        None
    } else {
        existing.as_ref().and_then(|s| s.fired_at)
    };

    if row.disabled_at.is_some()
        || row.emergency_paused_at.is_some()
        || row.membership_ended_at.is_some()
        || row.budget_exhausted_at.is_some()
    {
        db.save_hook_rule_schedule(&ScheduleRow {
            rule_id: row.id.clone(),
            next_run: None,
            last_scheduled_for,
            fired_at,
        })?;
        return Ok(None);
    }

    let next_run = next_for_clock(&definition.trigger, now(), existing, trigger_changed)?;
    db.save_hook_rule_schedule(&ScheduleRow {
        rule_id: row.id.clone(),
        next_run,
        last_scheduled_for,
        fired_at,
    })?;
    Ok(next_run)
}

/// Clears `next_run` when a rule stops being eligible (membership ended or budget exhausted).
pub(crate) fn clear_next_run(db: &Db<'_>, rule_id: &str) -> Result<()> {
    if let Some(mut schedule) = db.hook_rule_schedule(rule_id)? {
        schedule.next_run = None;
        db.save_hook_rule_schedule(&schedule)?;
    }
    Ok(())
}

fn clock_event(
    event_id: &str,
    scheduled_at: Option<i64>,
    emitted_at: i64,
    timezone: &str,
    manual: bool,
    depth: u32,
) -> Json {
    json!({
        "id": event_id,
        "hook": "clock",
        "instanceId": Json::Null,
        "payload": {
            "scheduledAt": scheduled_at.map(rfc3339),
            "emittedAt": rfc3339(emitted_at),
            "timezone": timezone,
            "manual": manual,
        },
        "payloadFields": ["scheduledAt", "emittedAt", "timezone", "manual"],
        "receivedAt": emitted_at,
        "cairn": { "depth": depth },
    })
}

fn insert_triggering(
    db: &Db<'_>,
    rule_id: &str,
    version: u32,
    event_id: &str,
    record: &Json,
    at: i64,
) -> Result<Option<String>> {
    let recorded = record.to_string();
    let recorded_bytes = recorded.len() + 256;
    if recorded_bytes > MAX_RECORDED_BYTES - ERROR_RESERVE {
        db.record_admission_refusal(rule_id, event_id)?;
        return Ok(None);
    }
    if db.refuse_admission(rule_id, event_id, recorded.len())? {
        return Ok(None);
    }
    let received = db.0.execute(
        "INSERT OR IGNORE INTO hook_rule_receipts(rule_id,event_id) VALUES(?1,?2)",
        params![rule_id, event_id],
    )?;
    if received == 0 {
        return Ok(None);
    }
    let triggering_id = id();
    db.0.prepare_cached(
        "INSERT OR IGNORE INTO hook_rule_triggerings( \
         id,rule_id,version,event_id,event,status,cursor,pending,in_flight, \
         actions,tool_calls,counted_at,verify,verify_since,next_at,lease_until, \
         error,created_at,updated_at,finished_at,recorded_bytes) \
         VALUES(?1,?2,?3,?4,?5,'running',?6,NULL,0,0,0,NULL,0,NULL,NULL,NULL,NULL,?7,?7,NULL,?8)",
    )?
    .execute(params![
        triggering_id,
        rule_id,
        version,
        event_id,
        recorded,
        serde_json::to_string(&Cursor::default())?,
        at,
        recorded_bytes as i64
    ])?;
    Ok(Some(triggering_id))
}

fn advance_schedule(
    db: &Db<'_>,
    rule_id: &str,
    trigger: &HookRuleTrigger,
    scheduled_for: i64,
    emitted_at: i64,
) -> Result<()> {
    let next_run = match trigger {
        HookRuleTrigger::Clock {
            cron: Some(pattern),
            at: None,
            timezone,
        } => crate::service::next_occurrences(pattern, timezone, emitted_at, 1)?
            .into_iter()
            .next(),
        _ => None,
    };
    db.save_hook_rule_schedule(&ScheduleRow {
        rule_id: rule_id.to_owned(),
        next_run,
        last_scheduled_for: Some(scheduled_for),
        fired_at: Some(emitted_at),
    })
}

/// Emits one triggering for each due clock rule. After an interruption, missed slots
/// produce a single triggering carrying the missed scheduled time, and the next slot
/// is computed from now (ADR-0094).
pub(super) async fn observe(s: &Service) -> Result<bool> {
    let due = s.store.read(|db| db.due_clock_rules(now())).await?;
    if due.is_empty() {
        return Ok(false);
    }
    let mut delivered = false;
    for (rule_id, scheduled_for) in due {
        let emitted = s
            .store
            .transaction({
                let rule_id = rule_id.clone();
                move |db| {
                    let Some(schedule) = db.hook_rule_schedule(&rule_id)? else {
                        return Ok(false);
                    };
                    let emitted_at = now();
                    if schedule.next_run.is_none_or(|next| next > emitted_at) {
                        return Ok(false);
                    }
                    let scheduled = schedule.next_run.unwrap_or(scheduled_for);
                    let Some(row) = db.hook_rule(&rule_id)? else {
                        return Ok(false);
                    };
                    let Some(version) = db.hook_rule_version(&rule_id, row.version)? else {
                        return Ok(false);
                    };
                    let HookRuleTrigger::Clock { timezone, .. } = &version.definition.trigger
                    else {
                        db.delete_hook_rule_schedule(&rule_id)?;
                        return Ok(false);
                    };
                    if row.disabled_at.is_some()
                        || row.emergency_paused_at.is_some()
                        || row.membership_ended_at.is_some()
                        || row.budget_exhausted_at.is_some()
                    {
                        clear_next_run(db, &rule_id)?;
                        return Ok(false);
                    }
                    if blocked(db, &row, &version)?.is_some() {
                        // Rights are temporarily lost: advance past the missed slot so the
                        // rule does not spin on every tick until its rights return.
                        advance_schedule(
                            db,
                            &rule_id,
                            &version.definition.trigger,
                            scheduled,
                            emitted_at,
                        )?;
                        return Ok(false);
                    }
                    let event_id = format!("clock:{rule_id}:{scheduled}");
                    let record =
                        clock_event(&event_id, Some(scheduled), emitted_at, timezone, false, 0);
                    let inserted = insert_triggering(
                        db,
                        &rule_id,
                        version.version,
                        &event_id,
                        &record,
                        emitted_at,
                    )?
                    .is_some();
                    advance_schedule(
                        db,
                        &rule_id,
                        &version.definition.trigger,
                        scheduled,
                        emitted_at,
                    )?;
                    Ok(inserted)
                }
            })
            .await;
        match emitted {
            Ok(inserted) => delivered |= inserted,
            Err(error) => {
                tracing::warn!(rule = %rule_id, error = %error, "Clock rule could not be triggered");
            }
        }
    }
    Ok(delivered)
}

/// Triggers a clock rule immediately without waiting for its next scheduled occurrence.
pub(crate) async fn run_now(
    s: &Service,
    input: HookRuleInput,
    actor: Actor,
) -> Result<HookRuleTriggering> {
    actor.require_scope("run")?;
    actor.recheck().await?;
    let _authors = match &actor {
        Actor::Installation(..) => Some(s.memberships.authors.lock().await),
        _ => None,
    };
    let agent_author = match &actor {
        Actor::Installation(bearer, _) => Some((
            bearer.clone(),
            Author::Agent(
                bearer.clone(),
                crate::conversation_writers::Eligibility::observe(s).await?,
            ),
        )),
        _ => None,
    };
    let viewer = actor.viewer();
    let rule_id = input.rule_id.to_string();
    let triggering = s
        .store
        .transaction(move |db| {
            actor.authorize(db)?;
            let (viewer, depth) = match &agent_author {
                Some((bearer, author)) => {
                    let writer = author.writer(db)?;
                    author.unblocked(db, &writer)?;
                    let depth = crate::installation_mcp::authorize_in(db, bearer)
                        .ok()
                        .and_then(|run| run["id"].as_str().map(str::to_owned))
                        .map(|run_id| conversations::source_depth(db, Some(&run_id)))
                        .transpose()?
                        .unwrap_or(0);
                    (
                        Viewer {
                            account: Some(writer.creator.account),
                            owner: false,
                        },
                        depth,
                    )
                }
                None => (viewer, 0),
            };
            let row = required(db.hook_rule(&rule_id)?, "Rule not found")?;
            let version = required(
                db.hook_rule_version(&rule_id, row.version)?,
                "Rule version not found",
            )?;
            let HookRuleTrigger::Clock { timezone, .. } = &version.definition.trigger else {
                return Err(Error::bad("Only clock rules can be started with Run now."));
            };
            if viewer.account.as_deref() != Some(row.creator_account_id.as_str()) {
                return Err(Error::forbidden(
                    "Only its creator can run this rule now. Duplicate it to make your own.",
                ));
            }
            if row.budget_exhausted_at.is_some() {
                return Err(Error::conflict(
                    row.budget_reason
                        .as_deref()
                        .unwrap_or("This rule reached its hourly limit."),
                ));
            }
            if let Some(reason) = blocked(db, &row, &version)? {
                return Err(Error::conflict(reason));
            }
            let at = now();
            let event_id = format!("clock-manual:{}", id());
            let record = clock_event(&event_id, None, at, timezone, true, depth);
            let triggering_id =
                insert_triggering(db, &rule_id, version.version, &event_id, &record, at)?
                    .ok_or_else(|| {
                        Error::conflict(
                            "This rule has reached its pending capacity. Wait for current triggerings to finish.",
                        )
                    })?;
            db.audit(
                "hook_rule.run_now",
                &json!({ "ruleId": rule_id, "triggeringId": triggering_id }),
            )?;
            steps::get(db, &triggering_id, &viewer)
        })
        .await?;
    s.rules.wake.notify_one();
    Ok(triggering)
}
