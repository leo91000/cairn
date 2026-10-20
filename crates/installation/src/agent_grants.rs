//! Live grants an agent holds on resources: exclusive reservations of nodes and
//! plugin instances such as 1Password service accounts.
//!
//! The owner edits them on the agent (`exclusiveNodes`, `plugins`). Nodes and plugin
//! instances keep a copy (`exclusiveAgents`, `agentIds`) written in the same
//! transaction: enforcement reads it, and so do releases that predate agent-side
//! grants. Those releases only edit the copy, so at startup any difference means the
//! copy is newer and the agent side is rebuilt from it.
//!
//! These grants are live, not part of a run's snapshot. They sit beside `access`
//! rather than in it: [`crate::service::policy`] (and the previous release's, which
//! merges every `access` key) never sees them, so changing them neither blocks queued
//! work nor changes the access a plugin delegation approved, before or after a
//! rollback. Granting a plugin instance still suspends a delegation open to everyone,
//! which must never reach secrets.

use crate::{
    error::{Error, Result},
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

/// Stores both grant lists without duplicates. Exclusive reservations need permission
/// to run on the node and a Firecracker agent, so losing either also removes them.
pub(crate) fn normalize(agent: &mut Value) {
    let access = crate::service::policy(agent);
    let native = crate::native::policy::native(agent);
    let exclusive = exclusive_nodes(agent)
        .into_iter()
        .filter(|node| !native && crate::service::allowed(&access["nodes"], node))
        .collect::<Vec<_>>();
    agent[EXCLUSIVE_NODES] = json!(exclusive);
    agent[PLUGINS] = json!(plugins(agent));
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
        .get("agents", text(agent, "id"))?
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
/// the agent so the resources list exactly the grants saved on the agent.
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
    for mut agent in db.list("agents")? {
        let granted = agents.iter().any(|id| agent["id"] == *id);
        if set_member(&mut agent[key], resource, granted) {
            crate::service::touch(&mut agent);
            db.put("agents", &agent)?;
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
        let may_run = db.get("agents", agent)?.is_some_and(|saved| {
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

/// Rebuilds the agent side from the resource copies, drops copies that name agents
/// which no longer exist, and gives every agent a revision. Idempotent; runs at
/// every startup after the other agent migrations.
pub(crate) fn reconcile(db: &Db<'_>) -> Result<()> {
    let agents = db.list("agents")?;
    let known = |id: &Value| agents.iter().any(|agent| agent["id"] == *id);

    let mut nodes = db.list("nodes")?;
    for node in &mut nodes {
        let Some(list) = node["exclusiveAgents"].as_array_mut() else {
            continue;
        };
        let before = list.len();
        list.retain(known);
        if list.len() != before {
            db.put("nodes", node)?;
        }
    }

    let mut instances = db.list(PLUGIN_INSTANCES)?;
    for instance in &mut instances {
        let Some(list) = instance["agentIds"].as_array_mut() else {
            continue;
        };
        let before = list.len();
        list.retain(known);
        if list.len() != before {
            db.put(PLUGIN_INSTANCES, instance)?;
        }
    }

    for mut agent in agents.clone() {
        let id = agent["id"].clone();
        let lists = |resources: &[Value], key: &str, skip_revoked: bool| {
            resources
                .iter()
                .filter(|resource| !(skip_revoked && resource["revoked"] == true))
                .filter(|resource| {
                    resource[key]
                        .as_array()
                        .is_some_and(|list| list.contains(&id))
                })
                .map(|resource| resource["id"].clone())
                .collect::<Vec<_>>()
        };
        let access = crate::service::policy(&agent);
        let (exclusive, unrunnable): (Vec<_>, Vec<_>) = lists(&nodes, "exclusiveAgents", true)
            .into_iter()
            .partition(|node| {
                let record = nodes.iter().find(|record| record["id"] == *node);
                crate::service::allowed(&access["nodes"], node.as_str().unwrap_or(""))
                    && record.is_some_and(|record| reservable(&agent, record))
            });
        // A previous release can remove node access without the reservation.
        for node in &unrunnable {
            if let Some(record) = nodes.iter_mut().find(|record| record["id"] == *node)
                && set_member(&mut record["exclusiveAgents"], text(&agent, "id"), false)
            {
                db.put("nodes", record)?;
            }
        }
        let instance_grants = lists(&instances, "agentIds", false);
        let same = |key: &str, expected: &[Value]| {
            let saved = ids(&agent[key]);
            agent[key].is_array()
                && saved.len() == expected.len()
                && expected
                    .iter()
                    .all(|id| saved.iter().any(|known| id == known))
        };
        let changed = !(same(EXCLUSIVE_NODES, &exclusive) && same(PLUGINS, &instance_grants));
        // Drafts of this change stored the grants inside `access`, where the previous
        // release would merge them into the delegation policy.
        let nested = agent["access"].as_object_mut().is_some_and(|access| {
            let exclusive = access.remove(EXCLUSIVE_NODES).is_some();
            let plugins = access.remove(PLUGINS).is_some();
            exclusive || plugins
        });
        // Agents saved before revisions existed get one, so editors detect later changes.
        let unversioned = !agent["revision"]
            .as_str()
            .is_some_and(|revision| !revision.is_empty());
        if changed || nested || unversioned {
            agent[EXCLUSIVE_NODES] = exclusive.into();
            agent[PLUGINS] = instance_grants.into();
            crate::service::touch(&mut agent);
            db.put("agents", &agent)?;
        }
    }
    Ok(())
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
