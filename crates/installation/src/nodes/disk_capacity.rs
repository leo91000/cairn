//! One manager transaction owns admission promises and idempotent native growth.
use crate::{config::now, error::Result, store::Db, validation::text};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashMap;

pub(crate) const MIB: u64 = 1_048_576;
pub(crate) const STEP_MIB: u64 = 8192;
const MAX_BYTES: u64 = 1 << 40;

#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Observation {
    pub protocol: u8,
    pub captured_at: i64,
    pub used_bytes: u64,
    pub free_bytes: u64,
    pub owned_bytes: HashMap<String, u64>,
    #[serde(default)]
    pub retained_bytes: HashMap<String, u64>,
    pub requests: Vec<GrowthRequest>,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct GrowthRequest {
    pub attempt_id: String,
    pub current_mi_b: u64,
}

pub(crate) fn pool_reserve(limit: u64) -> u64 {
    (limit / 20).clamp(MIB, 10 * 1024 * MIB)
}

fn observation(node: &Value) -> Option<Observation> {
    let value: Observation = serde_json::from_value(node["diskCapacity"].clone()).ok()?;
    let age = now().checked_sub(value.captured_at)?;
    (value.protocol == 1 && (0..=30_000).contains(&age)).then_some(value)
}

/// Attribution includes only exclusively owned payloads, on the same accounting
/// basis as total usage, capped by live consumption and allocated backing.
/// Shared prepared-base inodes receive no double credit.
fn unused(attempts: &[Value], node: &Value, sample: &Observation) -> (u64, u64) {
    let mut credit_left = sample.used_bytes;
    let mut promised = 0u64;
    for attempt in attempts
        .iter()
        .filter(|a| a["nodeId"] == node["id"] && super::is_active_attempt(a))
    {
        let allowance = attempt["resources"]["diskMiB"]
            .as_u64()
            .unwrap_or(32768)
            .saturating_mul(MIB);
        let credit = sample
            .owned_bytes
            .get(text(attempt, "id"))
            .copied()
            .unwrap_or(0)
            .min(allowance)
            .min(credit_left);
        credit_left -= credit;
        promised = promised.saturating_add(allowance - credit);
    }
    (promised, credit_left)
}

pub(crate) fn fits(node: &Value, attempts: &[Value], additional: u64) -> bool {
    let Some(sample) = observation(node) else {
        return false;
    };
    let limit = node["limits"]["diskMiB"]
        .as_u64()
        .unwrap_or(0)
        .saturating_mul(MIB);
    let promised = unused(attempts, node, &sample).0.saturating_add(additional);
    let host_reserve = crate::storage::policy::Policy::for_node(&node["storage"])
        .unwrap_or_default()
        .reserve(
            node["capabilities"]["diskTotalMiB"]
                .as_u64()
                .unwrap_or(0)
                .saturating_mul(MIB),
        );
    sample
        .used_bytes
        .saturating_add(promised)
        .saturating_add(pool_reserve(limit))
        <= limit
        && promised.saturating_add(host_reserve) <= sample.free_bytes
}

/// A stopped conversation can reuse its exclusively owned, currently measured
/// backing. Credit shares the same total-usage cap as active promises.
pub(crate) fn admission_fits(
    node: &Value,
    attempts: &[Value],
    run: &str,
    allowance: u64,
    reuse: bool,
) -> bool {
    let credit = if reuse {
        observation(node).map_or(0, |sample| {
            sample
                .retained_bytes
                .get(run)
                .copied()
                .unwrap_or(0)
                .min(unused(attempts, node, &sample).1)
                .min(allowance)
        })
    } else {
        0
    };
    fits(node, attempts, allowance.saturating_sub(credit))
}

/// Called only inside the authenticated heartbeat transaction. A durable grant
/// survives a lost reply and is released through the ordinary fenced attempt.
pub(crate) fn report(db: &Db<'_>, node: &mut Value, body: &Value) -> Result<Value> {
    let Ok(mut sample) = serde_json::from_value::<Observation>(body.clone()) else {
        // Negotiation is sticky: an incomplete heartbeat must never downgrade
        // durable reservations to the legacy physical-only admission policy.
        node["diskCapacity"] = Value::Null;
        return Ok(Value::Null);
    };
    let age = now().saturating_sub(sample.captured_at);
    if sample.protocol != 1
        || !(0..=30_000).contains(&age)
        || sample.owned_bytes.len() > 4096
        || sample.retained_bytes.len() > 4096
        || sample.requests.len() > 4096
        || sample.used_bytes > (1 << 60)
        || sample.free_bytes > (1 << 60)
        || !crate::native::policy::node_native(node)
    {
        return Ok(Value::Null);
    }
    // Serial heartbeat requests from a node cannot replace a newer observation.
    if node["diskCapacity"]["capturedAt"]
        .as_i64()
        .is_some_and(|at| at > sample.captured_at)
    {
        return Ok(Value::Null);
    }
    let requests = std::mem::take(&mut sample.requests);
    node["diskGrowthProtocol"] = 1.into();
    node["diskCapacity"] = serde_json::to_value(&sample)?;
    let mut attempts = db.list("node-attempts")?;
    let mut grants = Vec::new();
    for request in requests {
        let Some(index) = attempts.iter().position(|a| a["id"] == request.attempt_id) else {
            continue;
        };
        let attempt = &attempts[index];
        let run_id = text(attempt, "runId");
        let Some(run) = db.run(run_id)? else { continue };
        let checkpoint = super::db_checkpoint(db, run_id)?;
        let agent = db
            .get("agents", super::run_agent(&run))?
            .unwrap_or_default();
        if attempt["nodeId"] != node["id"]
            || !super::is_active_attempt(attempt)
            || attempt["leaseExpiresAt"]
                .as_i64()
                .is_none_or(|until| until <= now())
            || checkpoint["runnerId"] != attempt["id"]
            || checkpoint["nodeId"] != node["id"]
            || node["executionReady"] != true
            || run["status"] != "running"
            || !run["cancelRequestedAt"].is_null()
            || !super::agent_allows(&agent, text(node, "id"))
            || !crate::native::policy::allows(db, &run, node)?
        {
            continue;
        }
        let current = attempt["resources"]["diskMiB"].as_u64().unwrap_or(32768);
        let next = current
            .checked_add(STEP_MIB)
            .filter(|next| next.saturating_mul(MIB) <= MAX_BYTES);
        let allowance = if request.current_mi_b < current {
            current
        } else if request.current_mi_b == current
            && next.is_some()
            && fits(node, &attempts, STEP_MIB * MIB)
        {
            let next = next.unwrap();
            persist_grant(db, &mut attempts[index], next)?;
            next
        } else {
            current
        };
        if allowance == current && request.current_mi_b == current {
            super::eviction::request(db, node)?;
        }
        grants.push(json!({
            "attemptId": request.attempt_id, "allowanceMiB": allowance,
            "reason": if allowance > request.current_mi_b { "granted" } else { "shared-capacity" },
        }));
    }
    Ok(json!({ "protocol": 1, "grants": grants }))
}

fn persist_grant(db: &Db<'_>, attempt: &mut Value, allowance: u64) -> Result<()> {
    attempt["resources"]["diskMiB"] = allowance.into();
    db.put("node-attempts", attempt)?;
    let run_id = text(attempt, "runId");
    let mut run = db.run(run_id)?.unwrap();
    run["resources"]["diskMiB"] = allowance.into();
    db.patch_run(run_id, &json!({ "resources": run["resources"] }))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node() -> Value {
        json!({
            "id": "node", "limits": { "diskMiB": 102400 },
            "capabilities": { "diskTotalMiB": 102400 },
            "diskCapacity": { "protocol": 1, "capturedAt": now(), "usedBytes": 20 * 1024 * MIB,
                "freeBytes": 80 * 1024 * MIB, "ownedBytes": { "one": 10 * 1024 * MIB }, "requests": [] },
        })
    }

    #[test]
    fn admission_and_growth_protect_unused_promises_and_host_reserve() {
        let mut node = node();
        let attempts =
            vec![json!({ "id": "one", "nodeId": "node", "resources": { "diskMiB": 32768 } })];
        // 20 used + (32 - 10) promised + 32 newcomer + 5 pool reserve = 79 GiB.
        assert!(fits(&node, &attempts, 32 * 1024 * MIB));
        assert!(!fits(&node, &attempts, 64 * 1024 * MIB));
        node["diskCapacity"]["freeBytes"] = (30 * 1024 * MIB).into();
        assert!(!fits(&node, &attempts, 8 * 1024 * MIB));
        node["diskCapacity"]["capturedAt"] = (now() - 31_000).into();
        assert!(!fits(&node, &attempts, 0));
    }

    #[test]
    fn same_node_resume_reuses_retained_backing_without_double_crediting_other_attempts() {
        let mut node = node();
        node["limits"]["diskMiB"] = 32768.into();
        node["diskCapacity"]["usedBytes"] = (256 * MIB).into();
        node["diskCapacity"]["retainedBytes"] = json!({ "resuming": 256 * MIB });
        assert!(!fits(&node, &[], 31_000 * MIB));
        assert!(admission_fits(&node, &[], "resuming", 31_000 * MIB, true));
        assert!(!admission_fits(
            &node,
            &[],
            "different-run",
            31_000 * MIB,
            true
        ));
        assert!(!admission_fits(&node, &[], "resuming", 31_000 * MIB, false));
        let attempts = [json!({ "id": "one", "nodeId": "node", "resources": { "diskMiB": 256 } })];
        node["diskCapacity"]["ownedBytes"] = json!({ "one": 256 * MIB });
        assert!(!admission_fits(
            &node,
            &attempts,
            "resuming",
            31_000 * MIB,
            true
        ));
    }

    #[tokio::test]
    async fn missing_report_cannot_downgrade_previously_negotiated_admission() {
        let root = tempfile::tempdir().unwrap();
        let store = crate::store::Store::open(root.path()).unwrap();
        store
            .transaction(|db| {
                let mut node = node();
                node["diskGrowthProtocol"] = 1.into();
                assert!(report(db, &mut node, &Value::Null)?.is_null());
                assert_eq!(node["diskGrowthProtocol"], 1);
                assert!(!fits(&node, &[], MIB));
                Ok(())
            })
            .await
            .unwrap();
    }

    #[test]
    fn shared_inodes_and_unknown_footprints_cannot_create_capacity() {
        let node = node();
        let mut attempts =
            vec![json!({ "id": "one", "nodeId": "node", "resources": { "diskMiB": 32768 } })];
        attempts.push(json!({ "id": "two", "nodeId": "node", "resources": { "diskMiB": 32768 } }));
        assert!(!fits(&node, &attempts, 32 * 1024 * MIB));
        attempts[0]["released"] = true.into();
        assert!(fits(&node, &attempts, 32 * 1024 * MIB));
    }
}
