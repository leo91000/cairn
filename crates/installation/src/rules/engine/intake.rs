use super::*;

pub(super) fn eligible_listener(db: &Db<'_>, rule: &str) -> Result<Option<(RuleRow, VersionRow)>> {
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
pub(super) enum Delivery {
    Deferred,
    Handled,
}

pub(super) fn hook_record(event: &crate::plugins::hooks::HookEvent, fields: &[String]) -> Json {
    json!({
        "id": event.id,
        "hook": event.name,
        "instanceId": event.instance_id,
        "payload": event.payload,
        "receivedAt": event.created_at,
        "payloadFields": fields,
    })
}

pub(super) async fn deliver_hook(
    s: &Arc<Service>,
    event: &crate::plugins::hooks::HookEvent,
    fields: &[String],
    row: RuleRow,
    version: VersionRow,
    admitted_at: i64,
) -> Result<Delivery> {
    let key = (event.id.clone(), row.id.clone());
    if s.rules.delivery_waiting(&key) {
        return Ok(if now() - admitted_at < DELIVERY_PATIENCE {
            Delivery::Deferred
        } else {
            Delivery::Handled
        });
    }
    let record = hook_record(event, fields);
    let incoming_bytes = record.to_string().len();
    let full = s
        .store
        .transaction({
            let (rule, event) = (row.id.clone(), event.id.clone());
            move |db| db.refuse_admission(&rule, &event, incoming_bytes)
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
                && now() - admitted_at < DELIVERY_PATIENCE =>
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
    let (rule, number, event_id) = (row.id.clone(), version.version, event.id.clone());
    s.store
        .transaction(move |db| {
            if !admissions::eligible(db, &rule, &event_id, number)? {
                return Ok(());
            }
            if db.refuse_admission(&rule, &event_id, incoming_bytes)? {
                return Ok(());
            }
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
                return Ok(());
            }
            db.insert_rule_triggering(&rule, number, &event_id, &recorded, now())?;
            Ok(())
        })
        .await?;
    Ok(Delivery::Handled)
}

/// Copies each shared event to bounded, private admissions before acknowledging it.
/// No resource resolver can hold the instance's transport queue here.
pub(super) async fn intake_instance(s: &Arc<Service>, instance: String) -> Result<usize> {
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
pub(super) async fn queued_instances(s: &Service) -> Result<Vec<String>> {
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
pub(super) async fn claim(
    s: &Service,
    capacity: usize,
    excluded: &HashSet<String>,
) -> Result<Vec<(String, String)>> {
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
    let excluded = excluded.clone();
    let order = s.rules.last_execution.lock().unwrap().clone();
    let (due, existing) = s.store
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
            let members_running: i64 = db.0.query_row(
                "SELECT count(*) FROM hook_rule_triggerings t JOIN hook_rules r ON r.id=t.rule_id \
                 WHERE r.creator_owner=0 AND t.status='running' AND t.lease_until>=?1",
                [at], |row| row.get(0),
            )?;
            let mut member_capacity = (PARALLEL / 2).saturating_sub(members_running as usize);
            // One oldest due triggering per rule, excluding any leased or locally
            // running rule. Least recently served rules get the next free slot.
            let mut candidates = db.0.prepare_cached(
                "SELECT t.id,t.rule_id,r.creator_owner FROM hook_rules r JOIN hook_rule_triggerings t ON t.id=( \
                   SELECT d.id FROM hook_rule_triggerings d WHERE d.rule_id=r.id AND ( \
                     (d.status='running' AND (d.lease_until IS NULL OR d.lease_until<?1)) OR \
                     (d.status IN ('waiting','retrying') AND d.next_at<=?1)) ORDER BY d.created_at,d.id LIMIT 1) \
                 WHERE NOT EXISTS(SELECT 1 FROM hook_rule_triggerings active WHERE active.rule_id=r.id \
                   AND active.status='running' AND active.lease_until>=?1) ORDER BY t.created_at,t.id",
            )?.query_map([at], |row| Ok((row.get::<_, String>(0)?,row.get::<_, String>(1)?,row.get::<_, bool>(2)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            candidates.retain(|(_, rule, _)| !excluded.contains(rule));
            candidates.sort_by_key(|(_, rule, _)| order.get(rule).copied());
            let mut due = Vec::new();
            for (id, rule, owner) in candidates {
                if due.len() >= capacity { break }
                if !owner {
                    if member_capacity == 0 { continue }
                    member_capacity -= 1;
                }
                db.0.execute(
                    "UPDATE hook_rule_triggerings SET status='running',next_at=NULL,lease_until=?2 WHERE id=?1",
                    params![id, at + LEASE],
                )?;
                due.push((id, rule));
            }
            let existing = db.0.prepare_cached("SELECT id FROM hook_rules")?
                .query_map([], |row| row.get::<_, String>(0))?.collect::<rusqlite::Result<HashSet<_>>>()?;
            Ok((due, existing))
        }).await?;
    let mut order = s.rules.last_execution.lock().unwrap();
    order.retain(|rule, _| existing.contains(rule));
    for (_, rule) in &due {
        order.insert(rule.clone(), Instant::now());
    }
    Ok(due)
}

pub(super) fn sweep_rule(db: &Db<'_>, rule: &str) -> Result<()> {
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
pub(super) async fn sweep(s: &Service) -> Result<()> {
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
            // Quotas look back at most a week; the conversation cap an hour.
            db.0.execute(
                "DELETE FROM hook_rule_author_triggerings WHERE at<=?",
                [now() - 7 * 86_400_000],
            )?;
            db.0.execute(
                "DELETE FROM hook_rule_conversation_counts WHERE counted_at<=?",
                [now() - 3_600_000],
            )?;
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
