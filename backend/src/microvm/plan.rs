//! Execution plan written by the manager for one VM attempt.
use crate::{execution::Sandbox, provider::Provider, validation::text};
use serde_json::Value;
use std::path::Path;

/// Guest home directory; its import carries the provider credentials.
pub const HOME: &str = "/home/node";
/// Volatile chat inbox refreshed during a run.
pub const CHAT_INBOX: &str = "/run/leo-chat";
/// Volatile runner code supplied by the current controller, outside the saved disk.
pub const ENTRYPOINT: &str = "/run/leo-entrypoint/leo";

/// Typed view of the plan document. The document itself is forwarded unchanged
/// (except for storage credentials) to the guest entrypoint.
#[derive(Clone, Debug)]
pub struct Plan(Value);

/// A host directory copied into the guest.
#[derive(Clone, Copy, Debug)]
pub struct Import<'a> {
    pub source: &'a Path,
    pub target: &'a str,
    pub read_only: bool,
}

impl Plan {
    pub fn new(value: Value) -> Self {
        Self(value)
    }

    pub fn as_value(&self) -> &Value {
        &self.0
    }

    /// The attempt identifier.
    pub fn id(&self) -> &str {
        text(&self.0, "id")
    }

    pub fn run_id(&self) -> &str {
        text(&self.0, "runId")
    }

    pub fn cwd(&self) -> &Path {
        Path::new(text(&self.0, "cwd"))
    }

    /// `None` without a deadline field; `Some(Null)` for an unlimited attempt.
    pub fn expires(&self) -> Option<&Value> {
        self.0.get("expires")
    }

    pub fn deadline(&self) -> Option<i64> {
        self.0["expires"].as_i64()
    }

    pub fn node_lease_required(&self) -> bool {
        self.0["nodeLeaseRequired"] == true
    }

    pub fn sandbox(&self) -> Option<Sandbox> {
        Sandbox::of(&self.0["sandbox"])
    }

    pub fn has_imports(&self) -> bool {
        self.0["imports"].is_array()
    }

    pub fn imports(&self) -> impl Iterator<Item = Import<'_>> {
        self.0["imports"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|import| Import {
                source: Path::new(text(import, "source")),
                target: text(import, "target"),
                read_only: import["readOnly"] == true,
            })
    }

    pub fn import_to(&self, target: &str) -> Option<Import<'_>> {
        self.imports().find(|import| import.target == target)
    }

    /// Host file receiving the chat result, if this attempt is a chat.
    pub fn chat_output(&self) -> Option<&str> {
        self.0["chat"]["output"].as_str()
    }

    pub fn chat_provider(&self) -> Provider {
        Provider::of_agent(&self.0["chat"])
    }

    pub fn storage(&self) -> &Value {
        &self.0["storage"]
    }

    pub fn set_vm_limits(&mut self, cpu: u32, memory: u64) {
        self.0["resources"]["cpu"] = cpu.into();
        self.0["resources"]["memoryMiB"] = memory.into();
    }

    pub fn resources(&self) -> Option<&Value> {
        self.0.get("resources")
    }

    pub fn command(&self) -> Option<Vec<&str>> {
        self.0["command"]
            .as_array()
            .map(|args| args.iter().filter_map(Value::as_str).collect())
    }

    /// The plan without its storage grant, which only the host may use.
    pub fn for_guest(&self) -> Value {
        let mut plan = self.0.clone();
        if let Some(plan) = plan.as_object_mut() {
            plan.remove("storage");
            if self.command().is_none() {
                plan.insert(
                    "command".into(),
                    serde_json::json!([ENTRYPOINT, "runner-entry"]),
                );
            }
        }
        plan
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn retained_guests_use_current_runner_code_without_exposing_storage_or_overwriting_commands() {
        let plan = Plan::new(json!({ "id": "attempt", "storage": { "grant": "private" } }));
        let guest = plan.for_guest();
        assert_eq!(guest["command"], json!([ENTRYPOINT, "runner-entry"]));
        assert!(guest.get("storage").is_none());
        assert!(plan.as_value().get("command").is_none());
        let custom = Plan::new(json!({ "command": ["/usr/local/bin/node", "probe.mjs"] }));
        assert_eq!(custom.for_guest()["command"], custom.as_value()["command"]);
    }
}
