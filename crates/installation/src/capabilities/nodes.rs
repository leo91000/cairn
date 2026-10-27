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
        let bearer = match context {
            ResumeContext::Human(_) => None,
            ResumeContext::Public(scopes, _) => {
                if !scopes.iter().any(|scope| scope == self.scope) {
                    return Err(Error::forbidden(format!(
                        "The {} scope is required.",
                        self.scope
                    )));
                }
                None
            }
            ResumeContext::Installation(bearer) => {
                crate::installation_mcp::authorize(s, &bearer).await?;
                Some(bearer)
            }
        };
        let authorize = crate::installation_mcp::management_authorization(bearer.as_deref());
        let before = authorize.clone();
        s.store.read(move |db| before(db)).await?;
        match self.name {
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
