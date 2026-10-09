//! Same-conversation reuse only; idle CPUs are paused, never executing a lease.
use super::{Budget, Plan, Reservation};
use crate::error::{Error, Result};
use serde_json::{Value, json};
use std::time::{Duration, Instant};

const MAX_RETAINED: usize = 2;
const MAX_MEMORY_BYTES: u64 = 2048 * 1_048_576;

pub(super) struct Retained {
    pub run: String,
    pub budget: Budget,
    pub key: Value,
    pub expires: Instant,
    pub reservation: Reservation,
    pub volume: Option<std::sync::Arc<crate::storage::runtime::Volume>>,
}

impl Retained {
    pub fn compatible(&self, plan: &Plan, budget: &Budget, now: Instant) -> bool {
        (now < self.expires || self.publication_pending())
            && self.budget == *budget
            && self.key == key(plan)
            && self
                .reservation
                .vm
                .as_ref()
                .is_some_and(|vm| vm.accepts_retained(plan))
    }

    pub fn bytes(&self) -> Option<u64> {
        self.reservation.vm.as_ref()?.resident_bytes()
    }

    /// This is cached durable-journal accounting, never a filesystem inspection
    /// in the admission path. Uncertain accounting fails closed.
    pub fn publication_pending(&self) -> bool {
        self.volume.as_ref().is_some_and(|volume| {
            volume
                .disk
                .accounting()
                .map_or(true, |status| status["dirtyBytes"] != 0)
        })
    }
}

pub(super) fn lifetime() -> Result<Duration> {
    let seconds = match std::env::var("CAIRN_VM_RETENTION_SECONDS") {
        Ok(value) => value
            .parse::<u64>()
            .ok()
            .filter(|value| *value <= 300)
            .ok_or_else(|| Error::bad("CAIRN_VM_RETENTION_SECONDS must be between 0 and 300."))?,
        Err(std::env::VarError::NotPresent) => 180,
        Err(_) => return Err(Error::bad("Invalid VM retention configuration.")),
    };
    Ok(Duration::from_secs(seconds))
}

pub(super) fn eligible(plan: &Plan) -> bool {
    plan.command().is_none()
        && plan.as_value()["chat"].is_object()
        && plan.chat_provider() == crate::provider::Provider::Codex
}

/// Filesystem mounts, privileges and disk geometry cannot change on a live VM.
pub(super) fn key(plan: &Plan) -> Value {
    let imports: Vec<_> = plan
        .imports()
        .map(|import| {
            json!({
                "source": import.source,
                "target": import.target,
                "readOnly": import.read_only,
            })
        })
        .collect();
    json!({
        "provider": plan.as_value()["chat"]["provider"],
        "cwd": plan.cwd(),
        "sandbox": plan.as_value()["sandbox"],
        "writableRoots": plan.as_value()["chat"]["writableRoots"],
        "diskMiB": plan.as_value()["resources"]["diskMiB"].as_u64().unwrap_or(super::DEFAULT_DISK_MIB),
        "imports": imports,
    })
}

pub(super) fn within_budget(count: usize, bytes: u64, budget: &Budget) -> bool {
    let allowance = (budget.limits.memory_mi_b * 1_048_576 / 4).min(MAX_MEMORY_BYTES);
    count <= MAX_RETAINED && bytes <= allowance
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_ram_is_bounded_independently_of_virtual_guest_size() {
        let mut budget = Budget {
            slots: 8,
            limits: crate::nodes::Resources {
                cpu: 8,
                memory_mi_b: 32768,
                disk_mi_b: 65536,
            },
        };
        assert!(within_budget(2, 2048 * 1_048_576, &budget));
        assert!(!within_budget(3, 1, &budget));
        assert!(!within_budget(1, 2048 * 1_048_576 + 1, &budget));
        budget.limits.memory_mi_b = 2048;
        assert!(within_budget(1, 512 * 1_048_576, &budget));
        assert!(!within_budget(1, 512 * 1_048_576 + 1, &budget));
    }

    #[test]
    fn live_privileges_and_mounts_must_match_but_access_tokens_are_refreshed() {
        let mut value = json!({
            "chat": { "provider": "codex", "codexConfig": { "mcp_servers": { "token": "old" } } },
            "cwd": "/data/runs/fixture", "sandbox": "workspace-write",
            "imports": [{ "source": "/data/runs/fixture/home", "target": "/home/node", "readOnly": false }],
        });
        let original = key(&Plan::new(value.clone()));
        value["chat"]["codexConfig"]["mcp_servers"]["token"] = "new".into();
        assert_eq!(key(&Plan::new(value.clone())), original);
        value["imports"][0]["readOnly"] = true.into();
        assert_ne!(key(&Plan::new(value.clone())), original);
        value["imports"][0]["readOnly"] = false.into();
        value["sandbox"] = "yolo".into();
        assert_ne!(key(&Plan::new(value)), original);
    }
}
