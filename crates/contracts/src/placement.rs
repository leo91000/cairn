//! Conversation placement and exclusive-reservation wire contracts.
use crate::*;

/// Execution allowances after node reserves are deducted. Configured node
/// budgets retain NodeResources' separate 128 MiB disk minimum.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExecutionResources {
    #[schemars(range(min = 1, max = 4096))]
    pub cpu: u32,
    #[schemars(range(min = 128, max = 1073741824_u64))]
    #[ts(type = "number")]
    pub memory_mi_b: u64,
    #[schemars(range(min = 0, max = 1099511627776_u64))]
    #[ts(type = "number")]
    pub disk_mi_b: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "lowercase")]
pub enum ExclusiveState {
    Queued,
    Acquiring,
    Acquired,
    Releasing,
    Released,
    Failed,
    Expired,
}

impl ExclusiveState {
    pub fn terminal(self) -> bool {
        matches!(self, Self::Released | Self::Failed | Self::Expired)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Acquiring => "acquiring",
            Self::Acquired => "acquired",
            Self::Releasing => "releasing",
            Self::Released => "released",
            Self::Failed => "failed",
            Self::Expired => "expired",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExclusiveReservation {
    pub id: String,
    pub node_id: String,
    pub owner_run_id: String,
    pub reason: String,
    #[schemars(
        with = "String",
        pattern("^(queued|acquiring|acquired|releasing|released|failed|expired)$")
    )]
    pub state: ExclusiveState,
    #[ts(type = "number")]
    #[serde(default)]
    pub created_at: i64,
    #[ts(type = "number | null")]
    pub expires_at: Option<i64>,
    #[serde(default)]
    pub renewal_pending: bool,
    #[ts(type = "number")]
    #[serde(default)]
    pub wait_until: i64,
    #[serde(default)]
    pub paused_run_ids: Vec<String>,
    #[ts(type = "number")]
    #[serde(default)]
    pub minimum_available_memory_mi_b: u64,
    pub error: Option<String>,
}

/// An object result is accepted by both legacy and current MCP tool codecs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExclusiveStatusOutput {
    #[schemars(length(max = 20))]
    pub reservations: Vec<ExclusiveReservation>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PlacementSelection {
    pub pinned_node_id: Option<NodeId>,
    pub preferred_node_id: Option<NodeId>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConversationPlacement {
    pub nodes: Vec<ExecutionNode>,
    pub pinned_node_id: Option<NodeId>,
    pub preferred_node_id: Option<NodeId>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GetPlacementInput {
    pub run_id: RunId,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConfigurePlacementInput {
    pub run_id: RunId,
    pub selection: PlacementSelection,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MoveToNodeInput {
    pub run_id: RunId,
    pub node_id: NodeId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    // Reject disallowed characters even when the string ends with a newline.
    #[schemars(
        length(max = 32),
        extend("items" = schemars::json_schema!({
            "type": "string", "minLength": 1, "maxLength": 40,
            "pattern": "^(?!.*[^A-Za-z0-9_:./-]).+$"
        }))
    )]
    pub required_tags: Option<Vec<String>>,
    #[serde(default)]
    #[schemars(range(min = 0, max = 3600))]
    #[ts(type = "number")]
    pub wait_seconds: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReleaseExclusiveInput {
    #[schemars(with = "Uuid")]
    pub reservation_id: String,
}

fn exclusive_wait() -> u64 {
    300
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AcquireExclusiveInput {
    #[schemars(range(min = 1, max = 900))]
    #[ts(type = "number")]
    pub duration_seconds: u64,
    #[serde(default = "exclusive_wait")]
    #[schemars(range(min = 0, max = 3600))]
    #[ts(type = "number")]
    pub wait_seconds: u64,
    #[serde(default = "exclusive_wait")]
    #[schemars(range(min = 1, max = 3600))]
    #[ts(type = "number")]
    pub acquisition_timeout_seconds: u64,
    #[schemars(length(min = 1, max = 500))]
    pub reason: String,
    #[serde(default)]
    #[schemars(length(max = 16), extend("uniqueItems" = true))]
    pub member_run_ids: Vec<RunId>,
    #[serde(default)]
    #[schemars(range(min = 0, max = 1073741824_u64))]
    #[ts(type = "number")]
    pub minimum_available_memory_mi_b: u64,
}

impl AcquireExclusiveInput {
    pub fn validate(&self) -> Result<(), &'static str> {
        self.validate_stored()?;
        if self.reason.chars().count() > 500 {
            return Err("Invalid exclusive reservation request.");
        }
        Ok(())
    }

    /// Redaction can expand a valid raw reason, including in previous databases.
    /// Keep the raw input limit at admission without truncating persisted masks.
    pub fn validate_stored(&self) -> Result<(), &'static str> {
        let unique = self
            .member_run_ids
            .iter()
            .map(ToString::to_string)
            .collect::<std::collections::HashSet<_>>()
            .len();
        if !(1..=900).contains(&self.duration_seconds)
            || self.wait_seconds > 3600
            || !(1..=3600).contains(&self.acquisition_timeout_seconds)
            || self.reason.trim().is_empty()
            || self.member_run_ids.len() > 16
            || unique != self.member_run_ids.len()
            || self.minimum_available_memory_mi_b > 1_073_741_824
        {
            return Err("Invalid exclusive reservation request.");
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MoveToNodeOutput {
    pub status: String,
    pub node_id: NodeId,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NodeMoveStarted {
    pub operation: Uuid,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NodeMoveOperationInput {
    pub operation_id: Uuid,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
pub enum NodeMoveOperationState {
    Running,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NodeMoveOperation {
    pub id: Uuid,
    #[schemars(with = "String", pattern("^(running|completed|failed|cancelled)$"))]
    pub state: NodeMoveOperationState,
    pub result: Option<MoveToNodeOutput>,
    pub error: Option<String>,
    pub error_status: Option<u16>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RenewExclusiveInput {
    #[schemars(with = "Uuid")]
    pub reservation_id: String,
    #[schemars(range(min = 1, max = 900))]
    #[ts(type = "number")]
    pub duration_seconds: u64,
}

impl MoveToNodeInput {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.wait_seconds > 3600
            || self.required_tags.as_ref().is_some_and(|tags| {
                tags.len() > 32
                    || tags.iter().any(|tag| {
                        tag.is_empty()
                            || tag.len() > 40
                            || !tag
                                .bytes()
                                .all(|c| c.is_ascii_alphanumeric() || b"_:./-".contains(&c))
                    })
            })
        {
            return Err("Invalid movement request.");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placement_contracts_preserve_states_defaults_and_command_bounds() {
        let corpus: serde_json::Value = serde_json::from_str(include_str!(
            "../../../tests/fixtures/placement-contracts.json"
        ))
        .unwrap();
        for value in corpus["allocations"].as_array().unwrap() {
            let resources: ExecutionResources = serde_json::from_value(value.clone()).unwrap();
            let run: Run = serde_json::from_value(serde_json::json!({
                "id": "allocation-run", "taskId": "mission", "status": "running",
                "resources": value,
            }))
            .unwrap();
            assert_eq!(run.resources, Some(resources));
            let budget: NodeResources = serde_json::from_value(value.clone()).unwrap();
            assert!(budget.validate().is_err());
            budget.validate_allocation().unwrap();
        }
        for value in corpus["views"].as_array().unwrap() {
            let reservation: ExclusiveReservation = serde_json::from_value(value.clone()).unwrap();
            assert_eq!(serde_json::to_value(reservation).unwrap(), *value);
        }
        let status_value = serde_json::json!({ "reservations": corpus["views"] });
        let status: ExclusiveStatusOutput = serde_json::from_value(status_value.clone()).unwrap();
        assert_eq!(serde_json::to_value(status).unwrap(), status_value);
        assert!(serde_json::from_value::<ExclusiveStatusOutput>(corpus["views"].clone()).is_err());
        for value in corpus["moves"].as_array().unwrap() {
            let input: MoveToNodeInput = serde_json::from_value(value.clone()).unwrap();
            input.validate().unwrap();
        }
        for value in corpus["moveOperations"].as_array().unwrap() {
            let operation: NodeMoveOperation = serde_json::from_value(value.clone()).unwrap();
            assert_eq!(serde_json::to_value(operation).unwrap(), *value);
        }
        for value in corpus["invalidMoveOperations"].as_array().unwrap() {
            assert!(serde_json::from_value::<NodeMoveOperation>(value.clone()).is_err());
        }
        for value in corpus["invalidMoves"].as_array().unwrap() {
            if let Ok(input) = serde_json::from_value::<MoveToNodeInput>(value.clone()) {
                assert!(input.validate().is_err());
            }
        }
        for value in corpus["acquisitions"].as_array().unwrap() {
            let input: AcquireExclusiveInput = serde_json::from_value(value.clone()).unwrap();
            input.validate().unwrap();
        }
        for value in corpus["invalidAcquisitions"].as_array().unwrap() {
            if let Ok(input) = serde_json::from_value::<AcquireExclusiveInput>(value.clone()) {
                assert!(input.validate().is_err());
            }
        }
    }
}
