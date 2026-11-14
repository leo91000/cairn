//! Private resource-resolution admissions. The transport queue is acknowledged
//! atomically with fanout; these rows never appear in rule history before authority
//! is resolved. One slow rule cannot occupy another rule's transport queue slots.
use super::*;
use crate::plugins::hooks::HookEvent;

pub(super) fn fanout(db: &Db<'_>, event: &HookEvent, fields: &[String]) -> Result<usize> {
    let listeners_sql = "SELECT DISTINCT rule_id FROM hook_rule_uses u \
        WHERE kind='hook' AND instance_id=?1 AND name=?2 \
        AND NOT EXISTS(SELECT 1 FROM hook_rule_receipts r WHERE r.rule_id=u.rule_id AND r.event_id=?3) \
        AND NOT EXISTS(SELECT 1 FROM hook_rule_admissions a WHERE a.rule_id=u.rule_id AND a.event_id=?3)";
    let rules =
        db.0.prepare_cached(listeners_sql)?
            .query_map(params![event.instance_id, event.name, event.id], |row| {
                row.get::<_, String>(0)
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
    let event_json = serde_json::to_string(event)?;
    let fields_json = serde_json::to_string(fields)?;
    let fits = event_json.len() + fields_json.len() <= MAX_RECORDED_BYTES - ERROR_RESERVE
        && hook_record(event, fields).to_string().len() + 256 <= MAX_RECORDED_BYTES - ERROR_RESERVE;
    let mut copied = 0;
    for rule in rules {
        let listener = match eligible_listener(db, &rule) {
            Ok(listener) => listener,
            Err(error) => {
                tracing::warn!(rule = %rule, error = %error, "Rule listener is unavailable");
                continue;
            }
        };
        let Some((_, version)) = listener else {
            continue;
        };
        if db.refuse_admission(&rule, &event.id)? {
            continue;
        }
        if !fits {
            continue;
        }
        copied += db.0.execute(
            "INSERT OR IGNORE INTO hook_rule_admissions(rule_id,event_id,version,instance_id,event,fields,created_at) \
             VALUES(?1,?2,?3,?4,?5,?6,?7)",
            params![rule, event.id, version.version, event.instance_id, event_json, fields_json, event.created_at],
        )?;
    }
    db.remove_plugin_hook(&event.instance_id, &event.id)?;
    Ok(copied)
}

/// Source approval and both rule versions are rechecked in the effect transaction.
pub(super) fn eligible(db: &Db<'_>, rule: &str, event: &str, version: u32) -> Result<bool> {
    let source: Option<(String, String, String)> = db.0.query_row(
        "SELECT instance_id,json_extract(event,'$.revision'),json_extract(event,'$.packageDigest') \
         FROM hook_rule_admissions WHERE rule_id=?1 AND event_id=?2",
        params![rule, event], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    ).optional()?;
    let Some((instance, revision, digest)) = source else {
        return Ok(false);
    };
    let Some(connection) = db.plugin_instance(&instance)? else {
        return Ok(false);
    };
    if connection.revision != revision {
        return Ok(false);
    }
    if let Some(package) = crate::plugins::registry::builtin(&connection.plugin_id) {
        if package.digest != digest {
            return Ok(false);
        }
    } else {
        let package = db.plugin_package(&connection.plugin_id)?;
        let approved = package
            .active
            .as_ref()
            .and_then(|active| package.approvals.get(active));
        if approved != Some(&digest) {
            return Ok(false);
        }
    }
    let Some((row, _)) = eligible_listener(db, rule)? else {
        return Ok(false);
    };
    let Some(version) = db.hook_rule_version(rule, version)? else {
        return Ok(false);
    };
    Ok(blocked(db, &row, &version)?.is_none())
}

/// Choose one due admission per rule, favoring rules least recently given a slot.
/// Transient resolver failures back off that rule, including newly queued events.
pub(super) async fn pending(
    s: &Service,
    resolving: &HashSet<String>,
) -> Result<Vec<(String, String)>> {
    let rows = s.store.read(|db| {
        Ok(db.0.prepare_cached(
            "SELECT rule_id,event_id,created_at FROM hook_rule_admissions ORDER BY created_at,event_id,rule_id",
        )?.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, i64>(2)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }).await?;
    let keys = rows
        .iter()
        .map(|(rule, event, _)| (event.clone(), rule.clone()))
        .collect::<HashSet<_>>();
    let rules = rows
        .iter()
        .map(|(rule, _, _)| rule.clone())
        .collect::<HashSet<_>>();
    let mut retries = s.rules.delivery_retries.lock().unwrap();
    retries.retain(|key, _| keys.contains(key));
    let backoff = retries
        .iter()
        .filter(|(_, (until, _))| *until > Instant::now())
        .map(|((_, rule), _)| rule.as_str())
        .collect::<HashSet<_>>();
    let mut selected = HashSet::new();
    let mut pending = rows
        .iter()
        .filter(|(rule, _, at)| {
            !resolving.contains(rule)
                && (!backoff.contains(rule.as_str()) || now() - at >= DELIVERY_PATIENCE)
                && selected.insert(rule.clone())
        })
        .map(|(rule, event, _)| (rule.clone(), event.clone()))
        .collect::<Vec<_>>();
    let mut order = s.rules.last_resolution.lock().unwrap();
    order.retain(|rule, _| rules.contains(rule));
    pending.sort_by_key(|(rule, _)| order.get(rule).copied());
    Ok(pending)
}

fn discard(db: &Db<'_>, rule: &str, event: &str) -> Result<()> {
    db.0.execute(
        "INSERT OR IGNORE INTO hook_rule_receipts(rule_id,event_id) \
         SELECT rule_id,event_id FROM hook_rule_admissions WHERE rule_id=?1 AND event_id=?2",
        params![rule, event],
    )?;
    db.0.execute(
        "DELETE FROM hook_rule_admissions WHERE rule_id=?1 AND event_id=?2",
        params![rule, event],
    )?;
    Ok(())
}

pub(super) async fn deliver(s: &Arc<Service>, rule: &str, event: &str) -> Result<()> {
    let (rule, event) = (rule.to_owned(), event.to_owned());
    let admission = s.store.read({
        let (rule, event) = (rule.clone(), event.clone());
        move |db| {
            let row: Option<(u32, String, String)> = db.0.query_row(
                "SELECT version,event,fields FROM hook_rule_admissions WHERE rule_id=?1 AND event_id=?2 AND created_at>?3 \
                 AND NOT EXISTS(SELECT 1 FROM hook_rule_receipts r WHERE r.rule_id=?1 AND r.event_id=?2)",
                params![rule, event, now() - DELIVERY_PATIENCE], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            ).optional()?;
            let Some((number, payload, fields)) = row else { return Ok(None) };
            if !eligible(db, &rule, &event, number)? { return Ok(None) }
            let row = required(db.hook_rule(&rule)?, "Rule not found")?;
            let version = required(db.hook_rule_version(&rule, number)?, "Rule version not found")?;
            Ok(Some((serde_json::from_str::<HookEvent>(&payload)?, serde_json::from_str::<Vec<String>>(&fields)?, row, version)))
        }
    }).await?;
    let outcome = match admission {
        Some((event, fields, row, version)) => {
            // Check on-disk package approval as well as the source identity in SQLite.
            let current = s.store.plugin_instance(&event.instance_id).await?;
            let source_valid = if let Some(connection) = current {
                match crate::plugins::registry::active(s, &connection.plugin_id).await {
                    Ok(package) => {
                        package.digest == event.package_digest
                            && connection.revision == event.revision
                    }
                    Err(error) if matches!(error.status, 400 | 403 | 404) => false,
                    Err(error) => return Err(error),
                }
            } else {
                false
            };
            if source_valid {
                deliver_hook(s, &event, &fields, row, version).await?
            } else {
                Delivery::Handled
            }
        }
        None => Delivery::Handled,
    };
    if let Delivery::Handled = outcome {
        s.rules.clear_delivery(&(event.clone(), rule.clone()));
        s.store
            .transaction(move |db| discard(db, &rule, &event))
            .await?;
    }
    Ok(())
}
