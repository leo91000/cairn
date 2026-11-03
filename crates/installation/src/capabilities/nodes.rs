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
        description: "Start a move to an authorized node, preserving backend, capacity waits and fencing. Returns an operation id; poll get_node_move_operation in separate tool calls. Failure preserves the current environment.",
    },
    NodeOperation {
        name: "get_node_move_operation",
        scope: "run",
        description: "Read this caller's pending move or its final result. Poll in separate tool calls; waiting for capacity does not extend the code worker deadline.",
    },
    NodeOperation {
        name: "cancel_node_move_operation",
        scope: "run",
        description: "Cancel this caller's capacity wait before admission, then poll its result. An admitted move keeps its durable fencing and recovery protocol.",
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
                schema_for::<NodeMoveStarted>(Contract::Serialize),
            ),
            "get_node_move_operation" | "cancel_node_move_operation" => (
                schema_for::<NodeMoveOperationInput>(Contract::Deserialize),
                schema_for::<NodeMoveOperation>(Contract::Serialize),
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
        let read_only = self.scope == "read" || self.name == "get_node_move_operation";
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
                "readOnlyHint": read_only,
                "destructiveHint": !read_only,
                "openWorldHint": !read_only
            }
        })
    }

    pub(crate) async fn invoke(
        &self,
        s: &Service,
        input: Value,
        context: ResumeContext,
    ) -> Result<Value> {
        let audience = match &context {
            ResumeContext::Human(_) => None,
            ResumeContext::Public(_, credential) => credential
                .as_ref()
                .map(|credential| nodes::move_operations::Audience::Public(credential.clone())),
            ResumeContext::Installation(bearer) => Some(
                nodes::move_operations::Audience::Installation(crate::auth::hex_digest(bearer)),
            ),
        };
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
        let authorize =
            crate::installation_mcp::management_authorization(s, bearer.as_deref()).await?;
        let before = authorize.clone();
        s.store.read(move |db| before.authorize(db)).await?;
        match self.name {
            "get_conversation_placement" => {
                let input: GetPlacementInput = decode(input)?;
                let placement_authorization = authorize.clone();
                nodes::placement::configure_authorized(
                    s,
                    &input.run_id.to_string(),
                    None,
                    move |db| placement_authorization.authorize(db),
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
                        authorize.authorize(db)?;
                        native(db)
                    },
                )
                .await
            }
            "move_to_node" => {
                let input: MoveToNodeInput = decode(input)?;
                input.validate().map_err(Error::bad)?;
                let run = s.store.run(&input.run_id.to_string()).await?;
                let authorization = nodes::moves::Authorization::new(|_| Ok(()), public_credential)
                    .with_management_authorization(authorize);
                if let Some(audience) = audience {
                    return s.node_move_operations.start(
                        s.clone(),
                        run,
                        serde_json::to_value(input)?,
                        authorization,
                        audience,
                    );
                }
                nodes::moves::request_authorized(
                    s,
                    &run,
                    &serde_json::to_value(input)?,
                    authorization,
                )
                .await
            }
            "get_node_move_operation" | "cancel_node_move_operation" => {
                let input: NodeMoveOperationInput = decode(input)?;
                let audience = audience.ok_or_else(|| {
                    Error::forbidden(
                        "Move operation results belong to their initiating tool caller.",
                    )
                })?;
                let id = input.operation_id.to_string();
                if self.name == "cancel_node_move_operation" {
                    s.node_move_operations.cancel(s, &id, &audience).await
                } else {
                    s.node_move_operations.get(s, &id, &audience).await
                }
            }
            "release_node_reservation" => {
                let input: ReleaseExclusiveInput = decode(input)?;
                nodes::exclusive::release_authorized(s, &input.reservation_id, None, move |db| {
                    authorize.authorize(db)
                })
                .await
            }
            "list_nodes" => {
                let _: ListNodesInput = decode(input)?;
                let _ = nodes::refresh_local(s).await;
                let result = nodes::typed_inventory(s).await?;
                authorize.recheck().await?;
                s.store
                    .read(move |db| {
                        authorize.authorize(db)?;
                        Ok(serde_json::to_value(result)?)
                    })
                    .await
            }
            "list_native_callers" => {
                let _: ListNodesInput = decode(input)?;
                nodes::native_callers_live(s, authorize).await
            }
            "create_node_enrollment" => {
                let input: CreateNodeEnrollmentInput = decode(input)?;
                nodes::invite_live(s, serde_json::to_value(input)?, authorize).await
            }
            "update_node" => {
                let input: UpdateNodeInput = decode(input)?;
                nodes::configure_node_live(
                    s,
                    &input.node_id.to_string(),
                    serde_json::to_value(input.node)?,
                    authorize,
                )
                .await
            }
            "update_node_storage" => {
                let input: UpdateNodeStorageInput = decode(input)?;
                nodes::configure_storage_live(
                    s,
                    &input.node_id.to_string(),
                    &serde_json::to_value(&input)?,
                    authorize,
                )
                .await
            }
            "change_native_node_mode" => {
                let input: ChangeNativeNodeModeInput = decode(input)?;
                let value = nodes::native_mode::change_live(
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
                nodes::revoke_live(
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
