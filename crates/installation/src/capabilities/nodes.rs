//! Inventory and enrollment commands shared by the apps and both agent tools.
use super::{ResumeContext, decode};
use crate::{
    error::{Error, Result},
    nodes,
    service::Service,
};
use cairn_contracts::*;
use serde_json::{Value, json};

pub(crate) struct NodeOperation {
    pub name: &'static str,
    scope: &'static str,
    description: &'static str,
}

pub(crate) const NODE_OPERATIONS: &[NodeOperation] = &[
    NodeOperation {
        name: "get_conversation_placement",
        scope: "read",
        description: "Read the conversation's placement and its agent's currently authorized nodes.",
    },
    NodeOperation {
        name: "configure_conversation_placement",
        scope: "manage",
        description: "Replace pinned/preferred node selection within the conversation agent's current grants. This does not grant node access or exclusivity.",
    },
    NodeOperation {
        name: "move_to_node",
        scope: "run",
        description: "Pause, save and resume a conversation on an authorized node, preserving its backend, capacity and fencing rules. Failure preserves its environment.",
    },
    NodeOperation {
        name: "release_node_reservation",
        scope: "manage",
        description: "Release a node's exclusive reservation. Other work resumes only after the controller confirms release.",
    },
    NodeOperation {
        name: "list_nodes",
        scope: "read",
        description: "List node inventory, detected capacity, availability and current agent grants. Tags and hardware do not grant authority.",
    },
    NodeOperation {
        name: "list_native_callers",
        scope: "manage",
        description: "List current installation people eligible for an explicit native trust selection.",
    },
    NodeOperation {
        name: "create_node_enrollment",
        scope: "manage",
        description: "Invite a new machine, retaining the current certificate, identity, backend and single-use enrollment proof.",
    },
    NodeOperation {
        name: "update_node",
        scope: "manage",
        description: "Replace node configuration at its current revision. New trusted administrative people require explicit human owner confirmation.",
    },
    NodeOperation {
        name: "change_native_node_mode",
        scope: "manage",
        description: "Preview or confirm the node's current native mode and exact affected access. Agents may propose administrator mode; only the authenticated owner can approve it.",
    },
    NodeOperation {
        name: "update_node_storage",
        scope: "manage",
        description: "Replace a node's storage policy at its current revision after probing the current storage configuration.",
    },
    NodeOperation {
        name: "revoke_node",
        scope: "manage",
        description: "Revoke the node identity and its live grants, optionally releasing pinned/preferred placements.",
    },
];

pub(crate) fn node_operation(name: &str) -> Option<&'static NodeOperation> {
    NODE_OPERATIONS.iter().find(|op| op.name == name)
}

impl NodeOperation {
    pub(crate) fn descriptor(&self) -> Value {
        let (input, output) = match self.name {
            "get_conversation_placement" => (
                schema_for::<GetPlacementInput>(Contract::Deserialize),
                schema_for::<ConversationPlacement>(Contract::Serialize),
            ),
            "configure_conversation_placement" => (
                schema_for::<ConfigurePlacementInput>(Contract::Deserialize),
                schema_for::<ConversationPlacement>(Contract::Serialize),
            ),
            "move_to_node" => (
                schema_for::<MoveToNodeInput>(Contract::Deserialize),
                schema_for::<MoveToNodeOutput>(Contract::Serialize),
            ),
            "release_node_reservation" => (
                schema_for::<ReleaseExclusiveInput>(Contract::Deserialize),
                schema_for::<ExclusiveReservation>(Contract::Serialize),
            ),
            "list_nodes" => (
                schema_for::<ListNodesInput>(Contract::Deserialize),
                schema_for::<Vec<ExecutionNode>>(Contract::Serialize),
            ),
            "list_native_callers" => (
                schema_for::<ListNodesInput>(Contract::Deserialize),
                schema_for::<Vec<NativeCaller>>(Contract::Serialize),
            ),
            "create_node_enrollment" => (
                schema_for::<CreateNodeEnrollmentInput>(Contract::Deserialize),
                schema_for::<NodeEnrollment>(Contract::Serialize),
            ),
            "update_node" => (
                schema_for::<UpdateNodeInput>(Contract::Deserialize),
                schema_for::<ExecutionNode>(Contract::Serialize),
            ),
            "change_native_node_mode" => (
                schema_for::<ChangeNativeNodeModeInput>(Contract::Deserialize),
                schema_for::<NativeModeChangeOutput>(Contract::Serialize),
            ),
            "update_node_storage" => (
                schema_for::<UpdateNodeStorageInput>(Contract::Deserialize),
                schema_for::<ExecutionNode>(Contract::Serialize),
            ),
            _ => (
                schema_for::<RevokeNodeInput>(Contract::Deserialize),
                schema_for::<NodeRevocation>(Contract::Serialize),
            ),
        };
        json!({
            "name": self.name,
            "description": self.description,
            "inputSchema": input,
            "outputSchema": output,
            "_meta": {
                "securitySchemes": [{
                    "type": "oauth2",
                    "scopes": [self.scope]
                }]
            },
            "annotations": {
                "readOnlyHint": self.scope == "read",
                "destructiveHint": self.scope != "read",
                "openWorldHint": self.scope != "read"
            }
        })
    }

    pub(crate) async fn invoke(
        &self,
        s: &Service,
        input: Value,
        context: ResumeContext,
    ) -> Result<Value> {
        let (bearer, public_credential) = match context {
            ResumeContext::Human(_) => (None, None),
            ResumeContext::Public(scopes, credential) => {
                if !scopes.iter().any(|scope| scope == self.scope) {
                    return Err(Error::forbidden(format!(
                        "The {} scope is required.",
                        self.scope
                    )));
                }
                let credential = credential.ok_or_else(|| {
                    Error::unauthorized("A verified public credential is required.")
                })?;
                (None, Some(credential))
            }
            ResumeContext::Installation(bearer) => {
                crate::installation_mcp::authorize(s, &bearer).await?;
                (Some(bearer), None)
            }
        };
        let authorize = crate::installation_mcp::management_authorization(bearer.as_deref());
        let before = authorize.clone();
        s.store.read(move |db| before(db)).await?;
        match self.name {
            "get_conversation_placement" => {
                let input: GetPlacementInput = decode(input)?;
                nodes::placement::configure_authorized(
                    s,
                    &input.run_id.to_string(),
                    None,
                    authorize,
                )
                .await
            }
            "configure_conversation_placement" => {
                let input: ConfigurePlacementInput = decode(input)?;
                let run = input.run_id.to_string();
                let target = run.clone();
                let native = crate::native::policy::run_authorization(&run);
                nodes::placement::configure_authorized(
                    s,
                    &target,
                    Some(serde_json::to_value(input.selection)?),
                    move |db| {
                        authorize(db)?;
                        native(db)
                    },
                )
                .await
            }
            "move_to_node" => {
                let input: MoveToNodeInput = decode(input)?;
                input.validate().map_err(Error::bad)?;
                let run = s.store.run(&input.run_id.to_string()).await?;
                nodes::moves::request_authorized(
                    s,
                    &run,
                    &serde_json::to_value(input)?,
                    nodes::moves::Authorization::new(authorize, public_credential),
                )
                .await
            }
            "release_node_reservation" => {
                let input: ReleaseExclusiveInput = decode(input)?;
                nodes::exclusive::release_authorized(s, &input.reservation_id, None, authorize)
                    .await
            }
            "list_nodes" => {
                let _: ListNodesInput = decode(input)?;
                let _ = nodes::refresh_local(s).await;
                let result = nodes::typed_inventory(s).await?;
                s.store
                    .read(move |db| {
                        authorize(db)?;
                        Ok(serde_json::to_value(result)?)
                    })
                    .await
            }
            "list_native_callers" => {
                let _: ListNodesInput = decode(input)?;
                nodes::native_callers(s, authorize).await
            }
            "create_node_enrollment" => {
                let input: CreateNodeEnrollmentInput = decode(input)?;
                nodes::invite(s, serde_json::to_value(input)?, authorize).await
            }
            "update_node" => {
                let input: UpdateNodeInput = decode(input)?;
                nodes::configure_node(
                    s,
                    &input.node_id.to_string(),
                    serde_json::to_value(input.node)?,
                    authorize,
                )
                .await
            }
            "update_node_storage" => {
                let input: UpdateNodeStorageInput = decode(input)?;
                nodes::configure_storage(
                    s,
                    &input.node_id.to_string(),
                    &serde_json::to_value(&input)?,
                    authorize,
                )
                .await
            }
            "change_native_node_mode" => {
                let input: ChangeNativeNodeModeInput = decode(input)?;
                let value = nodes::native_mode::change(
                    s,
                    &input.node_id.to_string(),
                    serde_json::to_value(input.change)?,
                    authorize,
                )
                .await?;
                let output = if value["previewId"].is_string() {
                    {
                        let preview: NativeModePreview = decode(value)?;
                        NativeModeChangeOutput::Preview {
                            preview_id: preview.preview_id,
                            preview: preview.preview,
                        }
                    }
                } else if value["status"] == "awaiting-owner" {
                    NativeModeChangeOutput::AwaitingOwner {
                        node: decode(value["node"].clone())?,
                        message: value["message"].as_str().unwrap_or_default().into(),
                    }
                } else {
                    NativeModeChangeOutput::Requested {
                        node: decode(value)?,
                    }
                };
                Ok(serde_json::to_value(output)?)
            }
            _ => {
                let input: RevokeNodeInput = decode(input)?;
                nodes::revoke(
                    s,
                    &input.node_id.to_string(),
                    json!({"releasePlacements": input.release_placements}),
                    authorize,
                )
                .await
            }
        }
    }
}
