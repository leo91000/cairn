//! Cairn's Clock (`Horloge`) hook (ADR-0094): schedules cron and one-shot rules,
//! emits a single catch-up triggering after an interruption, and supports manual
//! immediate runs ("Run now").
use super::*;
use crate::rules::{
    Actor, Author, Viewer,
    storage::{RuleRow, ScheduleRow},
};
use cairn_contracts::{HookRuleDefinition, HookRuleInput, HookRuleTrigger, HookRuleTriggering};

#[derive(Default)]
pub(super) struct MembershipCheck {
    in_flight: bool,
    retry_at: Option<Instant>,
}

fn refresh_memberships(service: &Arc<Service>) {
    {
        let mut check = service.rules.clock_memberships.lock().unwrap();
        if check.in_flight || check.retry_at.is_some_and(|at| at > Instant::now()) {
            return;
        }
        check.in_flight = true;
    }

    let service = service.clone();
    tokio::spawn(async move {
        let result = tokio::select! {
            () = service.shutdown.cancelled() => Ok(()),
            result = service.memberships.verify_membership(
                &service,
                crate::memberships::CLOCK_MEMBERSHIP_TTL,
            ) => result,
        };
        if let Err(error) = result {
            tracing::warn!(
                status = error.status,
                "Clock rules await confirmation of migrated memberships"
            );
        }
        let mut check = service.rules.clock_memberships.lock().unwrap();
        check.in_flight = false;
        check.retry_at = Some(Instant::now() + std::time::Duration::from_secs(15));
        service.rules.wake.notify_one();
    });
}

fn rfc3339(ms: i64) -> Result<String> {
    chrono::DateTime::from_timestamp_millis(ms)
        .map(|date| date.to_rfc3339())
        .ok_or_else(|| Error::bad("Invalid Clock date."))
}

pub(crate) fn next_for_clock(
    trigger: &HookRuleTrigger,
    at: i64,
    existing: Option<&ScheduleRow>,
    trigger_changed: bool,
) -> Result<Option<i64>> {
    match trigger {
        HookRuleTrigger::Clock {
            cron: Some(pattern),
            at: None,
            timezone,
        } => {
            if !trigger_changed && let Some(next) = existing.and_then(|row| row.next_run) {
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
                return Ok((*target > at).then_some(*target));
            }
            if existing.is_some_and(|row| row.fired_at.is_some()) {
                return Ok(None);
            }
            if let Some(next) = existing.and_then(|row| row.next_run) {
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
    reset_legacy_depth: bool,
) -> Result<Option<i64>> {
    let existing = db.hook_rule_schedule(&row.id)?;
    let mut schedule = existing.unwrap_or_else(|| ScheduleRow {
        rule_id: row.id.clone(),
        next_run: None,
        last_scheduled_for: None,
        fired_at: None,
        legacy_chain_depth: 0,
    });
    if trigger_changed {
        schedule.last_scheduled_for = None;
        schedule.fired_at = None;
    }
    if reset_legacy_depth {
        schedule.legacy_chain_depth = 0;
    }

    let clock = matches!(definition.trigger, HookRuleTrigger::Clock { .. });
    if !clock && schedule.legacy_chain_depth == 0 {
        db.delete_hook_rule_schedule(&row.id)?;
        return Ok(None);
    }
    if !clock
        || row.disabled_at.is_some()
        || row.emergency_paused_at.is_some()
        || row.membership_ended_at.is_some()
        || row.budget_exhausted_at.is_some()
    {
        schedule.next_run = None;
        db.save_hook_rule_schedule(&schedule)?;
        return Ok(None);
    }

    schedule.next_run = next_for_clock(
        &definition.trigger,
        row.updated_at,
        Some(&schedule),
        trigger_changed,
    )?;
    db.save_hook_rule_schedule(&schedule)?;
    Ok(schedule.next_run)
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
) -> Result<Json> {
    Ok(json!({
        "id": event_id,
        "hook": "clock",
        "instanceId": Json::Null,
        "payload": {
            "scheduledAt": scheduled_at.map(rfc3339).transpose()?,
            "emittedAt": rfc3339(emitted_at)?,
            "timezone": timezone,
            "manual": manual,
        },
        "payloadFields": ["scheduledAt", "emittedAt", "timezone", "manual"],
        "receivedAt": emitted_at,
        "cairn": { "depth": depth },
    }))
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
    db.insert_rule_triggering(rule_id, version, event_id, &recorded, at)
}

fn advance_schedule(
    db: &Db<'_>,
    mut schedule: ScheduleRow,
    trigger: &HookRuleTrigger,
    scheduled_for: i64,
    emitted_at: i64,
) -> Result<()> {
    schedule.next_run = match trigger {
        HookRuleTrigger::Clock {
            cron: Some(pattern),
            at: None,
            timezone,
        } => crate::service::next_occurrences(pattern, timezone, emitted_at, 1)?
            .into_iter()
            .next(),
        _ => None,
    };
    schedule.last_scheduled_for = Some(scheduled_for);
    schedule.fired_at = Some(emitted_at);
    db.save_hook_rule_schedule(&schedule)
}

/// Carries a v38 preparation failure that was stored on its mission without a run.
pub(crate) fn record_migrated_failure(
    db: &Db<'_>,
    rule: &str,
    timezone: &str,
    scheduled_for: i64,
    failed_at: i64,
    message: &str,
) -> Result<()> {
    let event_id = format!("clock:{rule}:{scheduled_for}");
    let event = clock_event(
        &event_id,
        Some(scheduled_for),
        failed_at,
        timezone,
        false,
        0,
    )?;
    if let Some(triggering) = insert_triggering(db, rule, 1, &event_id, &event, failed_at)? {
        let error: String = message.chars().take(4000).collect();
        db.0.execute(
            "UPDATE hook_rule_triggerings SET status='failed',error=?2,finished_at=?3 WHERE id=?1",
            params![triggering, error, failed_at],
        )?;
    }
    Ok(())
}

/// Emits one triggering for each due clock rule. After an interruption, missed slots
/// produce a single triggering carrying the missed scheduled time, and the next slot
/// is computed from now (ADR-0094).
pub(super) async fn observe(s: &Arc<Service>) -> Result<bool> {
    let due = s.store.read(|db| db.due_clock_rules(now())).await?;
    if due.is_empty() {
        return Ok(false);
    }
    let confirmed = if due
        .iter()
        .any(|(_, _, needs_memberships)| *needs_memberships)
    {
        match s.memberships.clock_membership_confirmation() {
            Some(confirmed) => confirmed,
            None => {
                refresh_memberships(s);
                false
            }
        }
    } else {
        true
    };
    let mut delivered = false;
    for (rule_id, scheduled_for, needs_memberships) in due {
        if needs_memberships && !confirmed {
            continue;
        }
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
                            schedule,
                            &version.definition.trigger,
                            scheduled,
                            emitted_at,
                        )?;
                        return Ok(false);
                    }
                    let event_id = format!("clock:{rule_id}:{scheduled}");
                    // Starting the migrated conversation adds one step. Preserve the
                    // original mission run's depth, including runs already at the limit.
                    let depth = schedule.legacy_chain_depth.saturating_sub(1);
                    let record = clock_event(
                        &event_id,
                        Some(scheduled),
                        emitted_at,
                        timezone,
                        false,
                        depth,
                    )?;
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
                        schedule,
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
            let depth = if agent_author.is_some() {
                let legacy_depth = db
                    .hook_rule_schedule(&rule_id)?
                    .map_or(0, |schedule| schedule.legacy_chain_depth.saturating_sub(1));
                depth.max(legacy_depth)
            } else {
                depth
            };
            let at = now();
            let event_id = format!("clock-manual:{}", id());
            let record = clock_event(&event_id, None, at, timezone, true, depth)?;
            let Some(triggering_id) =
                insert_triggering(db, &rule_id, version.version, &event_id, &record, at)?
            else {
                // Commit the recorded refusal along with the admission decision.
                return Ok(None);
            };
            db.audit(
                "hook_rule.run_now",
                &json!({ "ruleId": rule_id, "triggeringId": triggering_id }),
            )?;
            steps::get(db, &triggering_id, &viewer).map(Some)
        })
        .await?;
    let triggering = triggering.ok_or_else(|| {
        Error::conflict(
            "This rule has reached its pending capacity. Wait for current triggerings to finish.",
        )
    })?;
    s.rules.wake.notify_one();
    Ok(triggering)
}
