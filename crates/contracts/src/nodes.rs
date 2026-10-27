//! Inventory/configuration contracts. Machine observations grant no execution authority.
use crate::*;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(transparent)]
#[schemars(inline)]
pub struct NodeId(pub Uuid);

impl std::fmt::Display for NodeId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "kebab-case")]
pub enum NodeBackend {
    #[default]
    Firecracker,
    Native,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "kebab-case")]
pub enum NativeMode {
    #[default]
    Protected,
    Administrator,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NodeResources {
    #[schemars(range(min = 1, max = 4096))]
    pub cpu: u32,
    #[schemars(range(min = 128, max = 1073741824_u64))]
    #[ts(type = "number")]
    pub memory_mi_b: u64,
    #[schemars(range(min = 128, max = 1099511627776_u64))]
    #[ts(type = "number")]
    pub disk_mi_b: u64,
}

impl Default for NodeResources {
    fn default() -> Self {
        Self {
            cpu: 1,
            memory_mi_b: 128,
            disk_mi_b: 128,
        }
    }
}

impl NodeResources {
    pub fn validate(&self) -> Result<(), &'static str> {
        if !(1..=4096).contains(&self.cpu)
            || !(128..=1_073_741_824).contains(&self.memory_mi_b)
            || !(128..=1_099_511_627_776).contains(&self.disk_mi_b)
        {
            return Err("Invalid node resource limits.");
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NodeCapabilities {
    pub os: String,
    pub arch: String,
    #[serde(default)]
    #[schemars(with = "String", pattern("^(firecracker|native)$"))]
    pub backend: NodeBackend,
    pub kvm: bool,
    #[serde(default)]
    pub ublk: bool,
    #[serde(default)]
    pub native_mode_protocol: u32,
    pub cpu: u32,
    #[ts(type = "number")]
    pub memory_mi_b: u64,
    #[ts(type = "number")]
    pub disk_mi_b: u64,
    #[serde(default)]
    #[ts(type = "number")]
    pub disk_total_mi_b: u64,
}

impl NodeCapabilities {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.os != "linux" || self.arch != "x86_64" {
            return Err("Nodes require Linux x86-64.");
        }
        if self.disk_total_mi_b > 1_099_511_627_776 {
            return Err("Invalid detected disk capacity.");
        }
        self.resources().validate()
    }

    pub const fn resources(&self) -> NodeResources {
        NodeResources {
            cpu: self.cpu,
            memory_mi_b: self.memory_mi_b,
            disk_mi_b: self.disk_mi_b,
        }
    }

    pub fn limits(&self) -> NodeResources {
        if self.backend == NodeBackend::Native {
            return NodeResources {
                cpu: (self.cpu / 2).clamp(1, 8),
                memory_mi_b: (self.memory_mi_b / 4).clamp(128, 8192),
                disk_mi_b: (self.disk_mi_b / 5).clamp(128, 32768),
            };
        }
        NodeResources {
            cpu: self.cpu.saturating_sub(1).max(1),
            memory_mi_b: self.memory_mi_b.saturating_sub(512).max(128),
            disk_mi_b: (self.disk_mi_b * 4 / 5).max(128),
        }
    }

    pub fn tags(&self) -> Vec<String> {
        let mut tags = vec!["linux".into(), "x86_64".into()];
        if self.kvm {
            tags.push("kvm".into());
        }
        tags
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", default, deny_unknown_fields)]
pub struct NodeStoragePolicy {
    #[ts(type = "number")]
    pub cache_mi_b: u64,
    #[ts(type = "number")]
    pub memory_cache_mi_b: u64,
    #[ts(type = "number")]
    pub reserve_mi_b: u64,
    #[ts(type = "number")]
    pub reserve_percent: u64,
    #[ts(type = "number")]
    pub backup_seconds: u64,
    #[ts(type = "number")]
    pub max_dirty_seconds: u64,
}

impl Default for NodeStoragePolicy {
    fn default() -> Self {
        Self {
            cache_mi_b: 102400,
            memory_cache_mi_b: 256,
            reserve_mi_b: 10240,
            reserve_percent: 5,
            backup_seconds: 60,
            max_dirty_seconds: 300,
        }
    }
}

impl NodeStoragePolicy {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.cache_mi_b > 16 * 1024 * 1024
            || !(4..=4096).contains(&self.memory_cache_mi_b)
            || !(64..=16 * 1024 * 1024).contains(&self.reserve_mi_b)
            || !(1..=50).contains(&self.reserve_percent)
            || !(5..=3600).contains(&self.backup_seconds)
            || self.max_dirty_seconds < self.backup_seconds
            || self.max_dirty_seconds > 86400
        {
            return Err("Invalid storage limits.");
        }
        Ok(())
    }

    pub fn reserve(&self, total: u64) -> u64 {
        (self.reserve_mi_b * 1024 * 1024).max(total / 100 * self.reserve_percent)
    }

    pub fn pause_reason(
        &self,
        total: u64,
        free: u64,
        dirty_since: Option<i64>,
        now: i64,
    ) -> Option<&'static str> {
        if free <= self.reserve(total) {
            return Some("disk-space");
        }
        if dirty_since
            .is_some_and(|at| now.saturating_sub(at) >= (self.max_dirty_seconds * 1000) as i64)
        {
            return Some("backup-lag");
        }
        None
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NativeModeRequest {
    #[schemars(with = "String", pattern("^(protected|administrator)$"))]
    pub mode: NativeMode,
    pub generation: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "kebab-case")]
pub enum NativeObservationStatus {
    Pending,
    Applied,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NativeModeObserved {
    #[schemars(with = "String", pattern("^(protected|administrator)$"))]
    pub mode: NativeMode,
    pub generation: Option<String>,
    #[schemars(with = "String", pattern("^(pending|applied|failed)$"))]
    pub status: NativeObservationStatus,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub target: Option<NativeModeRequest>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NativeModeProposal {
    pub preview_id: String,
    #[schemars(with = "String", pattern("^(protected|administrator)$"))]
    pub mode: NativeMode,
    pub status: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "kebab-case")]
pub enum NodeStatus {
    Revoked,
    Local,
    Online,
    #[default]
    Offline,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", default, deny_unknown_fields)]
pub struct NodeUsage {
    #[ts(type = "number")]
    pub memory_mi_b: u64,
    #[ts(type = "number")]
    pub disk_mi_b: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StaleDisks {
    pub count: u32,
    #[ts(type = "number")]
    pub disk_mi_b: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NodeAgent {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub all_nodes: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExecutionNode {
    pub id: NodeId,
    pub name: String,
    pub revision: String,
    #[serde(default)]
    pub local: bool,
    #[serde(default)]
    pub accepting: bool,
    #[serde(default)]
    pub revoked: bool,
    #[serde(default)]
    #[schemars(with = "String", pattern("^(online|offline|local|revoked)$"))]
    pub status: NodeStatus,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub system_tags: Vec<String>,
    pub capabilities: NodeCapabilities,
    pub limits: NodeResources,
    #[serde(default)]
    #[ts(optional = nullable)]
    pub slots: Option<u32>,
    #[serde(default)]
    #[ts(optional = nullable)]
    pub runtime_slots: Option<u32>,
    #[serde(default)]
    pub native_enabled: bool,
    #[serde(default)]
    pub native_accounts: Vec<String>,
    #[serde(default)]
    pub native_reviewed_agents: Vec<String>,
    #[serde(default)]
    #[ts(optional = nullable)]
    pub native_mode_request: Option<NativeModeRequest>,
    #[serde(default)]
    #[ts(optional = nullable)]
    pub native_mode_observed: Option<NativeModeObserved>,
    #[serde(default)]
    #[ts(optional = nullable)]
    pub native_mode_proposal: Option<NativeModeProposal>,
    #[serde(default)]
    #[ts(optional = nullable)]
    pub native_mode_status: Option<String>,
    #[serde(default)]
    pub exclusive_reservations: bool,
    // A10 owns the reservation state and its writer. This view has no authority.
    #[serde(default)]
    #[schemars(with = "Option<std::collections::BTreeMap<String, serde_json::Value>>", extend("x-kotlin-type" = "ExclusiveReservation"))]
    #[ts(type = "import('../../nodes').ExclusiveReservation | null")]
    pub exclusive_reservation: Option<serde_json::Value>,
    #[serde(default)]
    pub exclusive_agents: Vec<String>,
    #[serde(default)]
    #[ts(optional = nullable)]
    pub usage: Option<NodeUsage>,
    #[serde(default)]
    #[ts(optional = nullable)]
    pub budget_error: Option<String>,
    #[serde(default)]
    #[ts(optional = nullable)]
    pub pressure: Option<String>,
    #[serde(default)]
    pub execution_ready: bool,
    #[serde(default)]
    #[ts(optional = nullable)]
    pub maintenance: Option<String>,
    #[serde(default)]
    #[ts(optional = nullable)]
    pub maintenance_error: Option<String>,
    #[serde(default)]
    #[ts(optional = nullable)]
    pub maintenance_generation: Option<String>,
    #[serde(default)]
    #[ts(optional = nullable)]
    #[ts(type = "number | null")]
    pub maintenance_started_at: Option<i64>,
    #[serde(default)]
    #[ts(optional = nullable)]
    pub image_digest: Option<String>,
    #[serde(default)]
    #[ts(optional = nullable)]
    pub update_error: Option<String>,
    #[serde(default)]
    pub runtime_id: String,
    #[serde(default)]
    #[ts(type = "number | null")]
    pub last_seen: Option<i64>,
    #[ts(type = "number")]
    pub created_at: i64,
    pub storage: NodeStoragePolicy,
    #[serde(default)]
    #[ts(optional = nullable)]
    pub retained_runs: Option<Vec<String>>,
    // A11 owns disk-growth accounting. The observation is typed at its boundary.
    #[serde(default)]
    #[ts(optional = nullable)]
    pub disk_growth_protocol: Option<u32>,
    #[serde(default)]
    #[ts(optional = nullable)]
    pub disk_capacity: Option<NodeDiskObservation>,
    #[serde(default)]
    pub occupied_slots: u32,
    #[serde(default)]
    pub available_slots: u32,
    #[serde(default)]
    pub agents: Vec<NodeAgent>,
    pub stale_disks: StaleDisks,
    #[serde(default)]
    #[ts(optional = nullable)]
    pub disk_accounting: Option<NodeDiskAccounting>,
}

impl ExecutionNode {
    pub fn slots(&self) -> u32 {
        self.slots.or(self.runtime_slots).unwrap_or(4).max(1)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NodeDiskObservation {
    pub protocol: u8,
    #[ts(type = "number")]
    pub captured_at: i64,
    #[ts(type = "number")]
    pub used_bytes: u64,
    #[ts(type = "number")]
    pub free_bytes: u64,
    #[ts(type = "{ [key: string]: number }")]
    pub owned_bytes: std::collections::BTreeMap<String, u64>,
    #[ts(type = "{ [key: string]: number }")]
    pub retained_bytes: std::collections::BTreeMap<String, u64>,
    pub requests: Vec<NodeGrowthRequest>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NodeGrowthRequest {
    pub attempt_id: String,
    #[ts(type = "number")]
    pub current_mi_b: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NodeDiskAccounting {
    #[ts(type = "number")]
    pub captured_at: i64,
    #[ts(type = "number")]
    pub local_bytes: u64,
    #[ts(type = "number")]
    pub unused_granted_bytes: u64,
    #[ts(type = "number")]
    pub host_free_bytes: u64,
    #[ts(type = "number")]
    pub host_reserve_bytes: u64,
    #[ts(type = "number")]
    pub pool_reserve_bytes: u64,
    #[ts(type = "number")]
    pub reclaimable_bytes: u64,
    #[ts(type = "number")]
    pub unsynchronized_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NodeConfiguration {
    pub revision: String,
    #[schemars(length(min = 1, max = 100))]
    pub name: String,
    #[schemars(length(max = 32))]
    pub tags: Vec<String>,
    pub limits: NodeResources,
    #[serde(default)]
    #[schemars(range(min = 1, max = 4096))]
    pub slots: Option<u32>,
    pub accepting: bool,
    #[serde(default)]
    pub native_enabled: Option<bool>,
    #[serde(default)]
    #[schemars(length(max = 100))]
    pub native_accounts: Option<Vec<String>>,
    #[serde(default)]
    pub confirm_native_administration: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdateNodeInput {
    pub node_id: NodeId,
    pub node: NodeConfiguration,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateNodeEnrollmentInput {
    #[schemars(length(min = 1, max = 100))]
    pub name: String,
    #[serde(default)]
    #[schemars(with = "String", pattern("^(firecracker|native)$"))]
    pub backend: NodeBackend,
    #[serde(default)]
    pub manager_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NodeEnrollment {
    pub node_id: NodeId,
    #[schemars(with = "String", pattern("^(firecracker|native)$"))]
    pub backend: NodeBackend,
    pub route: String,
    pub manager_url: Option<String>,
    pub install_command: Option<String>,
    pub code: String,
    #[ts(type = "number")]
    pub expires_at: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ListNodesInput {}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NodeInput {
    pub node_id: NodeId,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RevokeNodeInput {
    pub node_id: NodeId,
    #[serde(default)]
    pub release_placements: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NodeRevocation {
    pub node: ExecutionNode,
    pub released_placements: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdateNodeStorageInput {
    pub node_id: NodeId,
    pub revision: String,
    pub storage: NodeStoragePolicy,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NativeCaller {
    pub id: String,
    pub email: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NativeModeChange {
    #[schemars(with = "String", pattern("^(protected|administrator)$"))]
    pub mode: NativeMode,
    #[serde(default)]
    pub preview_id: Option<String>,
    #[serde(default)]
    pub confirm: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChangeNativeNodeModeInput {
    pub node_id: NodeId,
    pub change: NativeModeChange,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct NativeModeImpact {
    #[schemars(with = "String", pattern("^(protected|administrator)$"))]
    pub mode: NativeMode,
    pub warning: String,
    pub interruptions: String,
    pub trusted_accounts: Vec<String>,
    pub trusted_people: Vec<NativeTrustedPerson>,
    pub agents: Vec<NativeAffectedAgent>,
    pub conversations: Vec<NativeAffectedConversation>,
    pub tasks: Vec<NativeAffectedAgent>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct NativeTrustedPerson {
    pub id: String,
    pub email: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct NativeAffectedAgent {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct NativeAffectedConversation {
    pub id: String,
    pub name: Option<String>,
    pub status: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NativeModePreview {
    pub preview_id: String,
    pub preview: NativeModeImpact,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum NativeModeChangeOutput {
    Preview {
        preview_id: String,
        preview: NativeModeImpact,
    },
    AwaitingOwner {
        node: ExecutionNode,
        message: String,
    },
    Requested {
        node: ExecutionNode,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_contracts_keep_capacity_observation_separate_from_configuration_and_authority() {
        let corpus: serde_json::Value =
            serde_json::from_str(include_str!("../../../tests/fixtures/node-contracts.json"))
                .unwrap();
        for view in corpus["views"].as_array().unwrap() {
            let node: ExecutionNode = serde_json::from_value(view.clone()).unwrap();
            assert_eq!(serde_json::to_value(&node).unwrap(), *view);
            assert_eq!(node.slots(), 4);
            assert_eq!(
                node.native_mode_request.unwrap().mode,
                NativeMode::Protected
            );
        }
        for command in corpus["commands"].as_array().unwrap() {
            let request: UpdateNodeInput = serde_json::from_value(command.clone()).unwrap();
            request.node.limits.validate().unwrap();
        }
        for command in corpus["invalidCommands"].as_array().unwrap() {
            match serde_json::from_value::<UpdateNodeInput>(command.clone()) {
                Err(_) => {}
                Ok(request) => assert!(
                    request.node.limits.validate().is_err()
                        || request
                            .node
                            .slots
                            .is_some_and(|slots| !(1..=4096).contains(&slots))
                ),
            }
        }
    }
}
