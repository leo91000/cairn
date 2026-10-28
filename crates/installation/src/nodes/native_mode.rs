//! Owner-reviewed host privileges. Requested and observed state are deliberately separate.
use crate::{
    config::{id, now},
    error::{Error, Result},
    service::{Service, policy},
    store::Db,
    validation::text,
};
use cairn_contracts::{ExecutionNode, NativeModeProposal, NativeModeRequest};
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

pub use cairn_contracts::NativeMode as Mode;

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
    if node["capabilities"]["nativeModeProtocol"] != 1 {
        return false;
    }
    node["nativeModeObserved"]["status"] == "applied"
        && node["nativeModeObserved"]["mode"] == request["mode"]
        && node["nativeModeObserved"]["generation"] == request["generation"]
}

pub fn administrator(node: &Value) -> bool {
    requested(node)["mode"] == "administrator"
}

pub(crate) fn unconfirmed(node: &Value) -> bool {
    node["revoked"] != true && node["capabilities"]["backend"] == "native" && !applied(node)
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
        .node_boundary(text(placement, "nodeId"))?
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
    db.update_run_boundary(
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
    for mut node in db.nodes()? {
        if !node
            .native_mode_request
            .as_ref()
            .is_some_and(|r| r.mode == Mode::Administrator)
        {
            continue;
        }
        let id = node.id.to_string();
        let mut reviewed = node.native_reviewed_agents.clone();
        let allowed =
            crate::native::policy::supports(agent, "native") && super::agent_allows(agent, &id);
        reviewed.retain(|saved| saved != text(agent, "id") || allowed);
        let explicit = access["nodes"]
            .as_array()
            .is_some_and(|nodes| nodes.iter().any(|node| node == &id));
        let was_explicit = previous.is_some_and(|agent| {
            policy(agent)["nodes"]
                .as_array()
                .is_some_and(|nodes| nodes.iter().any(|node| node == &id))
        });
        let confirmed = native_reviews.iter().any(|node| node == &id);
        if allowed && !reviewed.iter().any(|id| id == text(agent, "id")) {
            if confirmed && owner_review {
                reviewed.push(text(agent, "id").to_owned());
            } else if explicit && !was_explicit {
                return Err(Error::forbidden(
                    "Review and explicitly confirm passwordless host sudo for each new administrative node access.",
                ));
            }
        }
        if node.native_reviewed_agents != reviewed {
            node.native_reviewed_agents = reviewed;
            db.save_node(&node)?;
        }
    }
    Ok(())
}

/// Deleted and merged identities must never transfer a host review to a later
/// creation retry that happens to reuse the same UUID.
pub(crate) fn forget_agent(db: &Db<'_>, agent_id: &str) -> Result<()> {
    for mut node in db.nodes()? {
        let previous = node.native_reviewed_agents.len();
        node.native_reviewed_agents
            .retain(|saved| saved != agent_id);
        if node.native_reviewed_agents.len() != previous {
            db.save_node(&node)?;
        }
    }
    Ok(())
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct ReviewSnapshot {
    node_id: String,
    node_revision: String,
    mode: Mode,
    request: NativeModeRequest,
    trusted_accounts: Vec<String>,
    trusted_people: Vec<cairn_contracts::NativeTrustedPerson>,
    enabled: bool,
    agents: Vec<ReviewAgent>,
    conversations: Vec<ReviewConversation>,
    tasks: Vec<ReviewTask>,
    warning: &'static str,
    interruptions: &'static str,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct ReviewAgent {
    id: String,
    name: String,
    revision: String,
    access: cairn_contracts::AccessPolicy,
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReviewConversation {
    id: String,
    agent_id: String,
    name: Option<String>,
    status: String,
    native_principals: Option<Vec<String>>,
    node_id: Option<String>,
    checkpoint_node_id: Option<String>,
    pinned_node_id: Option<String>,
    preferred_node_id: Option<String>,
    target_node_id: Option<String>,
    // Explicit A10 movement boundary; the node approval has no movement writer.
    move_request: Option<Value>,
    reservations: Vec<String>,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct ReviewTask {
    id: String,
    name: String,
    agent_id: String,
}

fn snapshot(db: &Db<'_>, node: &ExecutionNode, mode: Mode) -> Result<ReviewSnapshot> {
    let node_id = node.id.to_string();
    let agents = db
        .agent_snapshots()?
        .into_iter()
        .filter(|agent| crate::native::policy::supports(agent, "native") && super::agent_allows(agent, &node_id))
        .map(|agent| Ok(serde_json::from_value::<ReviewAgent>(json!({
            "id": agent["id"], "name": agent["name"], "revision": agent["revision"], "access": policy(&agent)
        }))?))
        .collect::<Result<Vec<_>>>()?;
    let authority = db.kv("native-authority")?.unwrap_or_default();
    let trusted_people = node
        .native_accounts
        .iter()
        .map(|account| cairn_contracts::NativeTrustedPerson {
            id: account.clone(),
            email: authority["accounts"][account]["email"]
                .as_str()
                .map(str::to_owned),
        })
        .collect::<Vec<_>>();
    let mut conversations = Vec::new();
    let mut reservations = db
        .list("node-attempts")?
        .into_iter()
        .filter(|attempt| attempt["nodeId"] == node_id && attempt["released"] != true)
        .map(|attempt| json!({ "id": attempt["id"], "runId": attempt["runId"] }))
        .collect::<Vec<_>>();
    reservations.sort_by(|left, right| text(left, "id").cmp(text(right, "id")));
    for run in db.runs(None, None, i64::MAX, 0, true)? {
        let checkpoint = super::db_checkpoint(db, text(&run, "id"))?;
        let reserved = reservations
            .iter()
            .filter(|attempt| attempt["runId"] == run["id"])
            .map(|attempt| attempt["id"].clone())
            .collect::<Vec<_>>();
        let already_associated = run["nodeId"] == node_id
            || checkpoint["nodeId"] == node_id
            || run["pinnedNodeId"] == node_id
            || run["preferredNodeId"] == node_id
            || run["targetNodeId"] == node_id
            || run["moveRequest"]["nodeId"] == node_id
            || !reserved.is_empty();
        // Automatic queued work has no node yet. Include every potential first
        // placement covered by the displayed agent grants, even at capacity or
        // while offline; a concurrent arrival must invalidate this preview.
        let unplaced_candidate = run["nodeId"].as_str().is_none()
            && checkpoint["nodeId"].as_str().is_none()
            && ["pinnedNodeId", "targetNodeId"].iter().all(|field| {
                run[*field]
                    .as_str()
                    .is_none_or(|selected| selected == node_id)
            })
            && agents
                .iter()
                .any(|agent| agent.id == super::run_agent(&run));
        if crate::native::policy::supports(&run["snapshot"]["agent"], "native")
            && (already_associated || unplaced_candidate)
        {
            conversations.push(serde_json::from_value::<ReviewConversation>(json!({
                "id": run["id"], "agentId": super::run_agent(&run),
                "name": run["snapshot"]["task"]["name"], "status": run["status"],
                "nativePrincipals": run["nativePrincipals"],
                "nodeId": run["nodeId"], "checkpointNodeId": checkpoint["nodeId"],
                "pinnedNodeId": run["pinnedNodeId"], "preferredNodeId": run["preferredNodeId"],
                "targetNodeId": run["targetNodeId"], "moveRequest": run["moveRequest"],
                "reservations": reserved,
            }))?);
        }
    }
    let tasks = db
        .missions()?
        .into_iter()
        .filter(|task| {
            agents
                .iter()
                .any(|agent| agent.id == task.settings.agent_id.to_string())
        })
        .map(|task| ReviewTask {
            id: task.id.to_string(),
            name: task.settings.name,
            agent_id: task.settings.agent_id.to_string(),
        })
        .collect::<Vec<_>>();
    Ok(ReviewSnapshot {
        node_id,
        node_revision: node.revision.clone(),
        mode,
        request: node.native_mode_request.clone().unwrap_or_default(),
        trusted_accounts: node.native_accounts.clone(),
        trusted_people,
        enabled: node.native_enabled,
        agents,
        conversations,
        tasks,
        warning: WARNING,
        interruptions: "All managed executions on this node and their process descendants stop before restart. Files and conversation history remain. Queued work, schedules, retries and pinned/preferred conversations use the node's confirmed mode under current admission. New placements wait throughout a transition or an unconfirmed failure.",
    })
}

pub async fn change(
    s: &Service,
    node: &str,
    body: Value,
    authorize: impl FnOnce(&Db<'_>) -> Result<()> + Send + 'static,
) -> Result<Value> {
    let change: cairn_contracts::NativeModeChange = serde_json::from_value(body).map_err(|_| {
        Error::bad("Choose protected or administrator and preview before confirming.")
    })?;
    let node_id = node.to_owned();
    let owner = owner();
    s.store.transaction(move |db| {
        authorize(db)?;
        let mut node = db.node(&node_id)?.ok_or_else(|| Error::not_found("Node not found."))?;
        if node.revoked || node.capabilities.backend != cairn_contracts::NodeBackend::Native {
            return Err(Error::conflict("Choose an enrolled native node."));
        }
        if node.capabilities.native_mode_protocol != 1 {
            return Err(Error::conflict("Update the native supervisor before changing host privileges."));
        }
        let view = snapshot(db, &node, change.mode)?;
        let wire_view = serde_json::to_value(&view)?;
        if !change.confirm {
            let preview = id();
            db.set(&format!("native-mode-preview:{preview}"), &wire_view, Some(now() + 300_000))?;
            return Ok(json!({ "previewId": preview, "preview": view }));
        }
        let preview = change.preview_id.ok_or_else(|| Error::bad("Preview and explicitly confirm the affected access and interruptions."))?;
        crate::validation::uuid(&preview)?;
        let key = format!("native-mode-preview:{preview}");
        if db.kv(&key)?.as_ref() != Some(&wire_view) {
            return Err(Error::conflict("The node, agents or conversations changed. Preview again before confirming."));
        }
        if change.mode == Mode::Administrator && owner.is_none() {
            node.native_mode_proposal = Some(NativeModeProposal { preview_id: preview, mode: change.mode, status: "awaiting-owner".into() });
            node.revision = id();
            db.save_node(&node)?;
            return Ok(json!({
                "status": "awaiting-owner",
                "node": super::public(db, node)?,
                "message": "The installation owner must review and confirm native administrator in this node's details. An agent cannot elevate its host.",
            }));
        }
        node.native_mode_proposal = None;
        let generation = id();
        node.native_mode_request = Some(NativeModeRequest { mode: change.mode, generation: Some(generation.clone()) });
        node.native_mode_status = Some("pending".into());
        node.execution_ready = false;
        // The owner's confirmation reviews precisely the agents shown, including
        // existing wildcard grants. Future agents and future nodes gain nothing.
        node.native_reviewed_agents = view.agents.iter().map(|agent| agent.id.clone()).collect();
        for attempt in db.list("node-attempts")? {
            if attempt["nodeId"] == node_id && super::is_active_attempt(&attempt)
                && db.run(text(&attempt, "runId"))?.is_some()
            {
                db.update_run_boundary(text(&attempt, "runId"), &json!({
                    "nativeModeInterruption": generation, "nodeState": "updating",
                }))?;
            }
        }
        db.set(&key, &Value::Null, Some(now()))?;
        node.revision = id();
        db.save_node(&node)?;
        db.audit("node.native-mode.requested", &json!({ "nodeId": node_id, "mode": change.mode, "generation": generation }))?;
        super::public(db, node)
    }).await
}

#[cfg(test)]
use crate::node_fixture::*;

#[cfg(test)]
use crate::run_fixture::DbRunFixture;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_fixture::StoreAgentFixture;

    #[test]
    fn protected_mode_requires_the_current_controller_acknowledgement() {
        let mut node = json!({ "capabilities": { "nativeModeProtocol": 1 } });
        assert!(!applied(&node));
        node["nativeModeObserved"] = json!({
            "mode": "protected", "generation": null, "status": "applied",
        });
        assert!(applied(&node));
        node["capabilities"]["nativeModeProtocol"] = Value::Null;
        assert!(!applied(&node));
    }

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
            "id": run_id, "taskId": id(), "createdAt": now(), "status": "running", "snapshot": { "agent": agent },
            "nativePrincipals": [person], "nativeAuthorityNodes": [node_id], "nativeProvenanceUnknown": false,
        });
        store.put_node_fixture(node.clone()).await.unwrap();
        store.put_agent_fixture(agent).await.unwrap();
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
            .transaction(move |db| db.add_run_fixture(&saved, None).map(|_| ()))
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
        store.put_node_fixture(changed).await.unwrap();
        store
            .update_run_boundary(&run_id, json!({ "nativeModeInterruption": newer }))
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

    #[tokio::test]
    async fn administrative_migration_reads_keep_revocation_without_workload_review() {
        let root = tempfile::tempdir().unwrap();
        let store = crate::store::Store::open(root.path()).unwrap();
        let node_id = id();
        let agent_id = id();
        let person = id();
        let generation = id();
        let node = json!({
            "id": node_id, "nativeEnabled": true, "nativeAccounts": [person],
            "capabilities": { "backend": "native", "nativeModeProtocol": 1 },
            "nativeModeRequest": { "mode": "administrator", "generation": generation },
            "nativeModeObserved": { "mode": "administrator", "generation": generation, "status": "applied" },
            "nativeReviewedAgents": [],
        });
        let grant = json!({ "nativePrincipals": [person], "nativeAuthorityNodes": [node_id], "nativeProvenanceUnknown": false });
        let mut run = grant.clone();
        run["snapshot"] = json!({ "agent": { "id": agent_id, "access": { "backend": "native", "nodes": [node_id] } } });
        run["backendMigration"] = json!({ "sourceReadReady": true, "grant": grant });
        store
            .put_agent_fixture(
                json!({ "id": agent_id, "access": { "backend": "firecracker", "nodes": [] } }),
            )
            .await
            .unwrap();
        store
            .set(
                "native-authority",
                json!({ "observedAt": now(), "accounts": { (person): {} } }),
                None,
            )
            .await
            .unwrap();
        store
            .read(move |db| {
                assert!(!crate::native::policy::allows(db, &run, &node)?);
                assert!(crate::native::policy::allows_migration_source(
                    db, &run, &node
                )?);
                let mut unfenced = run.clone();
                unfenced["backendMigration"]["sourceReadReady"] = false.into();
                assert!(!crate::native::policy::allows_migration_source(
                    db, &unfenced, &node
                )?);
                let mut revoked = node.clone();
                revoked["revoked"] = true.into();
                assert!(!crate::native::policy::allows_migration_source(
                    db, &run, &revoked
                )?);
                revoked = node;
                revoked["nativeAccounts"] = json!([]);
                assert!(!crate::native::policy::allows_migration_source(
                    db, &run, &revoked
                )?);
                Ok(())
            })
            .await
            .unwrap();
    }
}
