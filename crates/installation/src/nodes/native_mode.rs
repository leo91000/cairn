//! Owner-reviewed host privileges. Requested and observed state are deliberately separate.
use crate::{
    config::{id, now},
    error::{Error, Result},
    service::{Service, policy},
    store::Db,
    validation::text,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub fn owner() -> Option<String> {
    crate::native::policy::caller().or_else(|| {
        crate::backend_migration::OWNER
            .try_with(Clone::clone)
            .ok()
            .flatten()
    })
}

pub const WARNING: &str = "Native administrator gives every admitted conversation passwordless unrestricted sudo on the real host. Root can read local files and credentials, alter Cairn, services and processes, and escape cooperative budgets, leases and cancellation. Existing remote Cairn permissions do not expand. Returning to protected stops managed executions and restricts new ones; it does not undo changes, clean the host or establish a new trust boundary. Root can defeat its own supervisor: stop confirmation is cooperative, not tamper-proof revocation.";

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Mode {
    #[default]
    Protected,
    Administrator,
}

pub fn requested(node: &Value) -> Value {
    if node["nativeModeRequest"].is_object() {
        node["nativeModeRequest"].clone()
    } else {
        json!({ "mode": "protected", "generation": null })
    }
}

pub fn applied(node: &Value) -> bool {
    let request = requested(node);
    let Ok(mode) = serde_json::from_value::<Mode>(request["mode"].clone()) else {
        return false;
    };
    let valid_generation = request["generation"].as_str().map_or(
        mode == Mode::Protected && request["generation"].is_null(),
        |generation| crate::validation::uuid(generation).is_ok(),
    );
    if !valid_generation {
        return false;
    }
    if node["nativeModeRequest"].is_object() && node["capabilities"]["nativeModeProtocol"] != 1 {
        return false;
    }
    // An old controller remains usable only with the unchanged protected default.
    if node["nativeModeObserved"].is_null() && node["nativeModeRequest"].is_null() {
        return true;
    }
    node["nativeModeObserved"]["status"] == "applied"
        && node["nativeModeObserved"]["mode"] == request["mode"]
        && node["nativeModeObserved"]["generation"] == request["generation"]
}

pub fn administrator(node: &Value) -> bool {
    requested(node)["mode"] == "administrator"
}

/// Review belongs to a node's current administrative generation, never to an
/// agent-side privilege toggle or a wildcard that includes future machines.
pub fn reviewed(node: &Value, agent: &Value) -> bool {
    !administrator(node)
        || node["nativeReviewedAgents"]
            .as_array()
            .is_some_and(|agents| agents.contains(&agent["id"]))
}

/// Generation acknowledgement and restart-intent clearing share the admission
/// transaction. A later owner confirmation can only stamp a newer interruption.
pub(crate) fn acknowledge_plan(
    db: &Db<'_>,
    run_id: &str,
    attempt_id: &str,
    placement: &Value,
) -> Result<Value> {
    let node = db
        .get("nodes", text(placement, "nodeId"))?
        .unwrap_or_default();
    let run = db.run(run_id)?.unwrap_or_default();
    let attempt = db.get("node-attempts", attempt_id)?.unwrap_or_default();
    if !applied(&node)
        || requested(&node) != placement["nativeMode"]
        || attempt["nativeMode"] != placement["nativeMode"]
        || attempt["runId"] != run_id
        || attempt["nodeId"] != placement["nodeId"]
        || attempt["released"] == true
        || !crate::native::policy::allows(db, &run, &node)?
    {
        return Err(Error::unavailable(
            "No authorized node privilege generation is confirmed. The existing environment is preserved.",
        ));
    }
    db.patch_run(
        run_id,
        &json!({ "nativeMode": placement["nativeMode"], "nativeModeInterruption": null }),
    )?;
    Ok(node["storage"].clone())
}

pub(crate) fn review_agent(
    db: &Db<'_>,
    agent: &Value,
    previous: Option<&Value>,
    owner_review: bool,
    native_reviews: &[String],
) -> Result<()> {
    if !owner_review && !native_reviews.is_empty() {
        return Err(Error::forbidden(
            "Only the owner can confirm administrative host access.",
        ));
    }
    let access = policy(agent);
    for mut node in db.list("nodes")? {
        if !administrator(&node) {
            continue;
        }
        let id = text(&node, "id").to_owned();
        let mut reviewed = node["nativeReviewedAgents"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let allowed = crate::native::policy::native(agent) && super::agent_allows(agent, &id);
        reviewed.retain(|saved| saved != &agent["id"] || allowed);
        let explicit = access["nodes"]
            .as_array()
            .is_some_and(|nodes| nodes.iter().any(|node| node == &id));
        let was_explicit = previous.is_some_and(|agent| {
            policy(agent)["nodes"]
                .as_array()
                .is_some_and(|nodes| nodes.iter().any(|node| node == &id))
        });
        let confirmed = native_reviews.iter().any(|node| node == &id);
        if allowed && explicit && !reviewed.contains(&agent["id"]) {
            if confirmed && owner_review {
                reviewed.push(agent["id"].clone());
            } else if !was_explicit {
                return Err(Error::forbidden(
                    "Review and explicitly confirm passwordless host sudo for each new administrative node access.",
                ));
            }
        }
        if node["nativeReviewedAgents"] != json!(reviewed) {
            node["nativeReviewedAgents"] = reviewed.into();
            db.put("nodes", &node)?;
        }
    }
    Ok(())
}

fn snapshot(db: &Db<'_>, node: &Value, mode: Mode) -> Result<Value> {
    let node_id = text(node, "id");
    let agents = db
        .list("agents")?
        .into_iter()
        .filter(|agent| crate::native::policy::native(agent) && super::agent_allows(agent, node_id))
        .map(|agent| json!({ "id": agent["id"], "name": agent["name"], "access": policy(&agent) }))
        .collect::<Vec<_>>();
    let authority = db.kv("native-authority")?.unwrap_or_default();
    let trusted_people = node["nativeAccounts"].as_array().into_iter().flatten().map(|account| json!({
        "id": account, "email": authority["accounts"][account.as_str().unwrap_or("")]["email"],
    })).collect::<Vec<_>>();
    let mut conversations = Vec::new();
    for run in db.runs(None, None, i64::MAX, 0, true)? {
        let checkpoint = super::db_checkpoint(db, text(&run, "id"))?;
        if crate::native::policy::native(&run["snapshot"]["agent"])
            && (run["nodeId"] == node_id
                || checkpoint["nodeId"] == node_id
                || run["pinnedNodeId"] == node_id
                || run["preferredNodeId"] == node_id)
        {
            conversations.push(json!({
                "id": run["id"], "agentId": super::run_agent(&run),
                "name": run["snapshot"]["task"]["name"], "status": run["status"],
                "nativePrincipals": run["nativePrincipals"],
            }));
        }
    }
    let tasks = db
        .list("tasks")?
        .into_iter()
        .filter(|task| agents.iter().any(|agent| agent["id"] == task["agentId"]))
        .map(|task| json!({ "id": task["id"], "name": task["name"], "agentId": task["agentId"] }))
        .collect::<Vec<_>>();
    Ok(json!({
        "nodeId": node_id, "mode": mode, "request": requested(node),
        "trustedAccounts": node["nativeAccounts"], "trustedPeople": trusted_people, "enabled": node["nativeEnabled"],
        "agents": agents, "conversations": conversations, "tasks": tasks,
        "warning": WARNING,
        "interruptions": "All managed executions on this node and their process descendants stop before restart. Files and conversation history remain. Queued work, schedules, retries and pinned/preferred conversations use the node's confirmed mode under current admission. New placements wait throughout a transition or an unconfirmed failure.",
    }))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Change {
    mode: Mode,
    preview_id: Option<String>,
    #[serde(default)]
    confirm: bool,
}

pub async fn change(
    s: &Service,
    node: &str,
    body: Value,
    authorize: impl FnOnce(&Db<'_>) -> Result<()> + Send + 'static,
) -> Result<Value> {
    let change: Change = serde_json::from_value(body).map_err(|_| {
        Error::bad("Choose protected or administrator and preview before confirming.")
    })?;
    let node_id = node.to_owned();
    let owner = owner();
    s.store.transaction(move |db| {
        authorize(db)?;
        let mut node = db.get("nodes", &node_id)?.ok_or_else(|| Error::not_found("Node not found."))?;
        if node["revoked"] == true || !crate::native::policy::node_native(&node) {
            return Err(Error::conflict("Choose an enrolled native node."));
        }
        if node["capabilities"]["nativeModeProtocol"] != 1 {
            return Err(Error::conflict("Update the native supervisor before changing host privileges."));
        }
        let view = snapshot(db, &node, change.mode)?;
        if !change.confirm {
            let preview = id();
            db.set(&format!("native-mode-preview:{preview}"), &view, Some(now() + 300_000))?;
            return Ok(json!({ "previewId": preview, "preview": view }));
        }
        let preview = change.preview_id.ok_or_else(|| Error::bad("Preview and explicitly confirm the affected access and interruptions."))?;
        crate::validation::uuid(&preview)?;
        let key = format!("native-mode-preview:{preview}");
        if db.kv(&key)?.as_ref() != Some(&view) {
            return Err(Error::conflict("The node, agents or conversations changed. Preview again before confirming."));
        }
        if change.mode == Mode::Administrator && owner.is_none() {
            node["nativeModeProposal"] = json!({ "previewId": preview, "mode": change.mode, "status": "awaiting-owner" });
            db.put("nodes", &node)?;
            return Ok(json!({ "status": "awaiting-owner", "node": super::public(node), "message": "The installation owner must review and confirm native administrator in this node's details. An agent cannot elevate its host." }));
        }
        node["nativeModeProposal"] = Value::Null;
        let generation = id();
        node["nativeModeRequest"] = json!({ "mode": change.mode, "generation": generation });
        node["nativeModeStatus"] = "pending".into();
        node["executionReady"] = false.into();
        // The owner's confirmation reviews precisely the agents shown, including
        // existing wildcard grants. Future agents and future nodes gain nothing.
        node["nativeReviewedAgents"] = view["agents"].as_array().unwrap().iter()
            .map(|agent| agent["id"].clone()).collect::<Vec<_>>().into();
        for attempt in db.list("node-attempts")? {
            if attempt["nodeId"] == node_id && super::is_active_attempt(&attempt)
                && db.run(text(&attempt, "runId"))?.is_some()
            {
                db.patch_run(text(&attempt, "runId"), &json!({
                    "nativeModeInterruption": generation, "nodeState": "updating",
                }))?;
            }
        }
        db.set(&key, &Value::Null, Some(now()))?;
        db.put("nodes", &node)?;
        db.audit("node.native-mode.requested", &json!({ "nodeId": node_id, "mode": change.mode, "generation": generation }))?;
        Ok(super::public(node))
    }).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn plan_acknowledgement_preserves_a_newer_transition_interruption() {
        let root = tempfile::tempdir().unwrap();
        let store = crate::store::Store::open(root.path()).unwrap();
        let node_id = id();
        let agent_id = id();
        let run_id = id();
        let attempt_id = id();
        let person = id();
        let generation = id();
        let mode = json!({ "mode": "protected", "generation": generation });
        let node = json!({
            "id": node_id, "nativeEnabled": true, "nativeAccounts": [person],
            "capabilities": { "backend": "native", "nativeModeProtocol": 1 },
            "nativeModeRequest": mode, "nativeModeObserved": { "mode": "protected", "generation": generation, "status": "applied" },
        });
        let agent =
            json!({ "id": agent_id, "access": { "backend": "native", "nodes": [node_id] } });
        let run = json!({
            "id": run_id, "createdAt": now(), "status": "running", "snapshot": { "agent": agent },
            "nativePrincipals": [person], "nativeAuthorityNodes": [node_id], "nativeProvenanceUnknown": false,
        });
        store.put("nodes", node.clone()).await.unwrap();
        store.put("agents", agent).await.unwrap();
        store.put("node-attempts", json!({ "id": attempt_id, "runId": run_id, "nodeId": node_id, "nativeMode": mode, "released": false })).await.unwrap();
        store
            .set(
                "native-authority",
                json!({ "observedAt": now(), "accounts": { (person): {} } }),
                None,
            )
            .await
            .unwrap();
        let saved = run.clone();
        store
            .transaction(move |db| db.add_run(&saved, None).map(|_| ()))
            .await
            .unwrap();
        let placement = json!({ "nodeId": node_id, "nativeMode": mode });
        let run_key = run_id.clone();
        let attempt_key = attempt_id.clone();
        let confirmed = placement.clone();
        store
            .transaction(move |db| acknowledge_plan(db, &run_key, &attempt_key, &confirmed))
            .await
            .unwrap();
        assert_eq!(store.run(&run_id).await.unwrap()["nativeMode"], mode);

        let mut changed = node;
        let newer = id();
        changed["nativeModeRequest"]["generation"] = newer.clone().into();
        store.put("nodes", changed).await.unwrap();
        store
            .patch_run(&run_id, json!({ "nativeModeInterruption": newer }))
            .await
            .unwrap();
        let placement = json!({ "nodeId": node_id, "nativeMode": mode });
        let run_key = run_id.clone();
        let error = store
            .transaction(move |db| acknowledge_plan(db, &run_key, &attempt_id, &placement))
            .await
            .unwrap_err();
        assert_eq!(error.status, 503);
        assert_eq!(
            store.run(&run_id).await.unwrap()["nativeModeInterruption"],
            newer
        );
    }
}
