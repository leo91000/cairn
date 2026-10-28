//! Live grants an agent holds on resources: exclusive reservations of nodes and
//! plugin instances such as 1Password service accounts.
//!
//! Until A3, nodes and plugin instances keep their existing permission lists
//! (`exclusiveAgents`, `agentIds`). These explicit adapters update typed SQL
//! profiles and resource lists in the same transaction. There is no startup
//! reconstruction or reader for profiles from older installations.
//!
//! These grants are live, outside a run's access snapshot. Changing them does not
//! broaden its frozen policy. Granting a plugin still suspends an externally
//! open delegation, which must never reach secrets.

use crate::{
    error::{Error, Result, required},
    plugins::connections::KIND as PLUGIN_INSTANCES,
    store::Db,
    validation::text,
};
use serde_json::{Value, json};

pub const EXCLUSIVE_NODES: &str = "exclusiveNodes";
pub const PLUGINS: &str = "plugins";

/// The most grants of each kind one agent may hold.
const MAX_GRANTS: usize = 100;

fn ids(value: &Value) -> Vec<String> {
    let mut ids = Vec::new();
    for id in value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
    {
        if !ids.iter().any(|known| known == id) {
            ids.push(id.to_owned());
        }
    }
    ids
}

/// Nodes on which the agent may suspend other conversations.
pub fn exclusive_nodes(agent: &Value) -> Vec<String> {
    ids(&agent[EXCLUSIVE_NODES])
}

/// Plugin instances whose commands, tools and instructions the agent receives.
pub fn plugins(agent: &Value) -> Vec<String> {
    ids(&agent[PLUGINS])
}

/// Exclusive reservations suspend Firecracker VMs, so only a Firecracker agent can
/// reserve a Firecracker node.
fn reservable(agent: &Value, node: &Value) -> bool {
    !crate::native::policy::native(agent) && !crate::native::policy::node_native(node)
}

/// Checks the grants an owner selected: existing, not revoked, and only on plugin
/// instances that give agents access. Instances the agent already holds stay accepted
/// while their package awaits review, so unrelated edits still save.
pub(crate) fn validate(db: &Db<'_>, agent: &Value, accepts_agents: &[String]) -> Result<()> {
    let held = db
        .agent_snapshot(text(agent, "id"))?
        .map(|saved| plugins(&saved))
        .unwrap_or_default();
    let nodes = exclusive_nodes(agent);
    let instances = plugins(agent);
    if nodes.len() > MAX_GRANTS || instances.len() > MAX_GRANTS {
        return Err(Error::bad(
            "Select at most 100 nodes and 100 plugin instances.",
        ));
    }
    for node in &nodes {
        let record = db.get("nodes", node)?.ok_or_else(|| {
            Error::bad("A node selected for exclusive reservations no longer exists.")
        })?;
        if record["revoked"] == true {
            return Err(Error::bad("A revoked node cannot be reserved."));
        }
        if !reservable(agent, &record) {
            return Err(Error::bad(
                "Exclusive reservations need a Firecracker VM node used by a Firecracker agent.",
            ));
        }
    }
    for instance in &instances {
        db.get(PLUGIN_INSTANCES, instance)?
            .ok_or_else(|| Error::bad("A selected plugin instance no longer exists."))?;
        let accepted =
            accepts_agents.iter().any(|id| id == instance) || held.iter().any(|id| id == instance);
        if !accepted {
            return Err(Error::bad(
                "A selected plugin instance does not give agents access.",
            ));
        }
    }
    Ok(())
}

/// Copies the agent's grants to every node and plugin instance, adding or removing
/// the agent so enforcement sees exactly the grants saved on the typed profile.
pub(crate) fn mirror(db: &Db<'_>, agent: &Value) -> Result<()> {
    let id = text(agent, "id");
    let nodes = exclusive_nodes(agent);
    let instances = plugins(agent);

    for mut node in db.list("nodes")? {
        let granted = nodes.iter().any(|node_id| node["id"] == *node_id);
        if set_member(&mut node["exclusiveAgents"], id, granted) {
            db.put("nodes", &node)?;
        }
    }

    for mut instance in db.list(PLUGIN_INSTANCES)? {
        let granted = instances
            .iter()
            .any(|instance_id| instance["id"] == *instance_id);
        if set_member(&mut instance["agentIds"], id, granted) {
            db.put(PLUGIN_INSTANCES, &instance)?;
        }
    }
    Ok(())
}

/// Adds or removes `id` from a JSON list. Returns whether the list changed.
fn set_member(list: &mut Value, id: &str, present: bool) -> bool {
    if !list.is_array() {
        if !present {
            return false;
        }
        *list = json!([]);
    }
    let Some(items) = list.as_array_mut() else {
        return false;
    };
    let listed = items.iter().any(|item| item == id);
    if listed == present {
        return false;
    }
    if present {
        items.push(id.into());
    } else {
        items.retain(|item| item != id);
    }
    true
}

/// Sets which agents hold a grant on one resource, updating those agents.
/// Used when a client still edits grants from the resource side.
fn grant_resource(db: &Db<'_>, key: &str, resource: &str, agents: &[String]) -> Result<()> {
    for mut agent in db.agent_snapshots()? {
        let granted = agents.iter().any(|id| agent["id"] == *id);
        if set_member(&mut agent[key], resource, granted) {
            crate::service::touch(&mut agent);
            db.save_agent_permissions(&agent)?;
        }
    }
    Ok(())
}

/// Records the agents allowed to reserve a node, on the agents and on the node. Only
/// agents allowed to run on it can reserve it.
pub(crate) fn set_node_agents(db: &Db<'_>, node: &mut Value, agents: &[String]) -> Result<()> {
    let node_id = text(node, "id").to_owned();
    let mut runnable = Vec::new();
    for agent in agents {
        let may_run = db.agent_snapshot(agent)?.is_some_and(|saved| {
            crate::service::allowed(&crate::service::policy(&saved)["nodes"], &node_id)
                && reservable(&saved, node)
        });
        if may_run {
            runnable.push(agent.clone());
        }
    }
    grant_resource(db, EXCLUSIVE_NODES, &node_id, &runnable)?;
    node["exclusiveAgents"] = json!(runnable);
    Ok(())
}

/// Records the agents allowed to use a plugin instance on those agents. The
/// instance itself stores the same list as `agentIds`.
pub(crate) fn set_instance_agents(db: &Db<'_>, instance: &str, agents: &[String]) -> Result<()> {
    grant_resource(db, PLUGINS, instance, agents)
}

/// Removes one agent's exclusive reservation of a node, on the agent and the node.
pub(crate) fn release_node(db: &Db<'_>, agent: &mut Value, node: &str) -> Result<()> {
    if !set_member(&mut agent[EXCLUSIVE_NODES], node, false) {
        return Ok(());
    }
    if let Some(mut record) = db.get("nodes", node)?
        && set_member(&mut record["exclusiveAgents"], text(agent, "id"), false)
    {
        db.put("nodes", &record)?;
    }
    Ok(())
}

/// Removes a deleted agent from every resource copy.
pub(crate) fn forget_agent(db: &Db<'_>, id: &str) -> Result<()> {
    mirror(db, &json!({ "id": id }))
}

/// Removes a revoked node or a deleted plugin instance from every agent.
pub(crate) fn forget_resource(db: &Db<'_>, key: &str, resource: &str) -> Result<()> {
    grant_resource(db, key, resource, &[])
}

pub(crate) fn save_profile(
    db: &Db<'_>,
    profile: cairn_contracts::Agent,
    access: &Value,
    owner_review: bool,
    native_reviews: &[String],
) -> Result<cairn_contracts::Agent> {
    let mut agent = serde_json::to_value(profile)?;
    // Nodes held before stay accepted only while the execution backend is unchanged.
    let previous_nodes = db
        .agent_snapshot(text(&agent, "id"))?
        .filter(|saved| crate::service::policy(saved)["backend"] == access["backend"])
        .and_then(|saved| crate::service::policy(&saved)["nodes"].as_array().cloned());
    // Serialize grants with revocation; a concurrent editor must never
    // restore a grant that the revocation transaction just removed.
    // Nodes the agent already held stay accepted, so unrelated edits still save.
    let held = |node: &Value| {
        previous_nodes
            .as_ref()
            .is_some_and(|nodes| nodes.iter().any(|held| held == node))
    };
    for node in access["nodes"].as_array().into_iter().flatten() {
        if node == crate::nodes::LOCAL_NODE_ID {
            // The current runner only runs Firecracker VMs.
            if access["backend"] == "native" && !held(node) {
                return Err(Error::bad(
                    "Native agents need explicit native nodes: the current runner only runs Firecracker VMs.",
                ));
            }
            continue;
        }
        let record = required(
            db.get("nodes", node.as_str().unwrap_or(""))?,
            "Node not found",
        )?;
        if record["revoked"] == true {
            return Err(Error::bad("A revoked node cannot be authorized."));
        }
        if !held(node)
            && crate::native::policy::node_native(&record) != (access["backend"] == "native")
        {
            return Err(Error::bad(
                "Choose nodes that use this agent's execution backend.",
            ));
        }
    }
    // Read the latest portrait inside the save transaction: editing an agent
    // must not overwrite an upload or background generation that just finished.
    if let Some(existing) = db.agent_snapshot(text(&agent, "id"))?
        && let Some(avatar) = existing.get("avatar")
    {
        agent["avatar"] = avatar.clone();
    }
    crate::nodes::native_mode::review_agent(
        db,
        &agent,
        db.agent_snapshot(text(&agent, "id"))?.as_ref(),
        owner_review,
        native_reviews,
    )?;
    crate::service::touch(&mut agent);
    db.save_agent_permissions(&agent)?;
    crate::agent_grants::mirror(db, &agent)?;
    db.audit("agent.saved", &json!({ "id": agent["id"] }))?;
    Ok(serde_json::from_value(agent)?)
}

#[cfg(test)]
mod tests {
    use super::set_member;
    use serde_json::json;

    #[test]
    fn membership_changes_only_when_needed() {
        let mut list = json!(null);
        assert!(!set_member(&mut list, "a", false));
        assert!(set_member(&mut list, "a", true));
        assert!(!set_member(&mut list, "a", true));
        assert_eq!(list, json!(["a"]));
        assert!(set_member(&mut list, "a", false));
        assert_eq!(list, json!([]));
    }
}
