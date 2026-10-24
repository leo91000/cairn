//! Each conversation keeps the execution backend its environment runs on. Agents
//! only grant nodes; Firecracker is the default whenever a VM node is allowed.
use crate::{
    error::Result,
    native::policy::node_native,
    service::{allowed, policy},
    store::Db,
    validation::text,
};
use serde_json::Value;

pub const NATIVE: &str = "native";
pub const FIRECRACKER: &str = "firecracker";

/// The backend `node` executes conversations with.
pub fn of_node(node: &Value) -> &'static str {
    if node_native(node) {
        NATIVE
    } else {
        FIRECRACKER
    }
}

fn known(value: &Value) -> Option<&'static str> {
    match value.as_str() {
        Some(NATIVE) => Some(NATIVE),
        Some(FIRECRACKER) => Some(FIRECRACKER),
        _ => None,
    }
}

/// The backend already chosen for a run: its prepared environment, else the
/// choice recorded before preparation. `None` until its first start decides.
pub fn recorded(run: &Value, checkpoint: &Value) -> Option<&'static str> {
    known(&checkpoint["prepared"]["backend"]).or_else(|| known(&run["executionBackend"]))
}

/// The backend a new environment uses without an explicit node: a VM whenever
/// the agent may use one, otherwise its native nodes.
pub fn default_for(access: &Value, nodes: &[Value]) -> &'static str {
    let usable =
        |node: &&Value| node["revoked"] != true && allowed(&access["nodes"], text(node, "id"));
    let vm = nodes.iter().filter(usable).any(|node| !node_native(node));
    let native = nodes.iter().filter(usable).any(node_native);
    if !vm && native { NATIVE } else { FIRECRACKER }
}

/// An explicitly targeted node selects the backend of an undecided environment.
fn targeted<'a>(run: &Value, nodes: &'a [Value]) -> Option<&'a Value> {
    ["targetNodeId", "pinnedNodeId", "preferredNodeId"]
        .iter()
        .filter_map(|field| run[*field].as_str())
        .find_map(|id| {
            nodes
                .iter()
                .find(|node| node["id"] == id && node["revoked"] != true)
        })
}

/// The backend of `run`, deciding it from explicit targets and the agent's
/// nodes when its environment has not been created yet. A move to the other
/// backend selects its destination with the backend it will run on there.
pub fn of_run(run: &Value, checkpoint: &Value, access: &Value, nodes: &[Value]) -> &'static str {
    known(&run["targetBackend"])
        .or_else(|| recorded(run, checkpoint))
        .or_else(|| targeted(run, nodes).map(of_node))
        .unwrap_or_else(|| default_for(access, nodes))
}

/// [`of_run`] from one consistent database view.
pub fn of_run_in(db: &Db<'_>, run: &Value) -> Result<&'static str> {
    let checkpoint = super::db_checkpoint(db, text(run, "id"))?;
    if let Some(backend) = recorded(run, &checkpoint) {
        return Ok(backend);
    }
    let agent = db
        .get("agents", super::run_agent(run))?
        .unwrap_or_else(|| run["snapshot"]["agent"].clone());
    Ok(of_run(
        run,
        &checkpoint,
        &policy(&agent),
        &db.list("nodes")?,
    ))
}

/// What a new conversation of `agent` starts on, before any explicit target.
pub fn default_for_agent(db: &Db<'_>, agent: &Value) -> Result<&'static str> {
    Ok(default_for(&policy(agent), &db.list("nodes")?))
}

/// Whether `agent` may reach any native node, so its requests need fresh
/// native authority even while its current conversation runs in a VM.
pub fn may_use_native(access: &Value, nodes: &[Value]) -> bool {
    nodes.iter().any(|node| {
        node_native(node) && node["revoked"] != true && allowed(&access["nodes"], text(node, "id"))
    })
}

/// The disk format of a saved environment and, for VM disks, the guest runtime
/// image it was created with. Native nodes can mount VM disks directly; a VM can
/// only boot a native disk after converting it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DiskFormat {
    Vm { runtime: Option<String> },
    Native,
}

impl DiskFormat {
    pub fn of(checkpoint: &Value) -> Self {
        match checkpoint["diskFormat"].as_str() {
            Some("vm") => Self::Vm {
                runtime: checkpoint["diskRuntimeId"].as_str().map(str::to_owned),
            },
            Some(NATIVE) => Self::Native,
            _ if checkpoint["prepared"]["backend"] == NATIVE => Self::Native,
            _ => Self::Vm {
                runtime: checkpoint["runtimeId"].as_str().map(str::to_owned),
            },
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Vm { .. } => "vm",
            Self::Native => NATIVE,
        }
    }

    /// Whether `backend` can execute this disk without converting its files.
    pub fn runs_on(&self, backend: &str) -> bool {
        backend == NATIVE || matches!(self, Self::Vm { .. })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn nodes() -> Vec<Value> {
        vec![
            json!({ "id": "vm", "capabilities": { "backend": "firecracker" } }),
            json!({ "id": "gpu", "capabilities": { "backend": "native" } }),
            json!({ "id": "gone", "revoked": true, "capabilities": { "backend": "firecracker" } }),
        ]
    }

    #[test]
    fn new_environments_prefer_a_vm_unless_only_native_nodes_are_allowed() {
        let nodes = nodes();
        assert_eq!(default_for(&json!({ "nodes": null }), &nodes), FIRECRACKER);
        assert_eq!(
            default_for(&json!({ "nodes": ["vm", "gpu"] }), &nodes),
            FIRECRACKER
        );
        assert_eq!(default_for(&json!({ "nodes": ["gpu"] }), &nodes), NATIVE);
        assert_eq!(
            default_for(&json!({ "nodes": ["gpu", "gone"] }), &nodes),
            NATIVE
        );
        assert_eq!(default_for(&json!({ "nodes": [] }), &nodes), FIRECRACKER);
    }

    #[test]
    fn a_prepared_environment_keeps_its_backend_and_targets_decide_new_ones() {
        let nodes = nodes();
        let access = json!({ "nodes": null });
        let fresh = json!({ "id": "run" });
        assert_eq!(of_run(&fresh, &Value::Null, &access, &nodes), FIRECRACKER);
        let pinned = json!({ "id": "run", "pinnedNodeId": "gpu" });
        assert_eq!(of_run(&pinned, &Value::Null, &access, &nodes), NATIVE);
        let chosen = json!({ "id": "run", "executionBackend": "native" });
        assert_eq!(of_run(&chosen, &Value::Null, &access, &nodes), NATIVE);
        let prepared = json!({ "prepared": { "backend": "firecracker" } });
        assert_eq!(of_run(&pinned, &prepared, &access, &nodes), FIRECRACKER);
    }

    #[test]
    fn disk_formats_follow_where_the_environment_was_created() {
        let vm = json!({ "prepared": { "backend": "firecracker" }, "runtimeId": "image-1" });
        assert_eq!(
            DiskFormat::of(&vm),
            DiskFormat::Vm {
                runtime: Some("image-1".into())
            }
        );
        let native = json!({ "prepared": { "backend": "native" }, "runtimeId": "tools-1" });
        assert_eq!(DiskFormat::of(&native), DiskFormat::Native);
        let mounted = json!({
            "prepared": { "backend": "native" },
            "runtimeId": "tools-1",
            "diskFormat": "vm",
            "diskRuntimeId": "image-1",
        });
        assert_eq!(
            DiskFormat::of(&mounted),
            DiskFormat::Vm {
                runtime: Some("image-1".into())
            }
        );
        assert!(DiskFormat::of(&mounted).runs_on(FIRECRACKER));
        assert!(DiskFormat::of(&mounted).runs_on(NATIVE));
        assert!(!DiskFormat::Native.runs_on(FIRECRACKER));
    }
}
