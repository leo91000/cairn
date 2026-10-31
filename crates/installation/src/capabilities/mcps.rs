use super::{ResumeContext, decode};
use crate::{
    error::{Error, Result},
    service::Service,
};
use cairn_contracts::*;
use serde_json::{Value, json};

pub(crate) struct McpOperation {
    pub name: &'static str,
    scope: &'static str,
    description: &'static str,
}

pub(crate) const MCP_OPERATIONS: &[McpOperation] = &[
    McpOperation {
        name: "list_mcps",
        scope: "read",
        description: "List external MCP connections without credentials.",
    },
    McpOperation {
        name: "create_mcp",
        scope: "manage",
        description: "Create an external MCP connection. Credentials are encrypted separately.",
    },
    McpOperation {
        name: "update_mcp",
        scope: "manage",
        description: "Replace connection settings using the current revision.",
    },
    McpOperation {
        name: "test_mcp",
        scope: "manage",
        description: "Discover tools using the connection's current credentials.",
    },
    McpOperation {
        name: "disconnect_mcp",
        scope: "manage",
        description: "Remove connection credentials and invalidate its active grants.",
    },
    McpOperation {
        name: "delete_mcp",
        scope: "manage",
        description: "Delete a connection and revoke agent selections.",
    },
    McpOperation {
        name: "begin_mcp_connection",
        scope: "manage",
        description: "Begin OAuth sign-in. Agents receive the human management URL; only an authenticated human session can authorize.",
    },
    McpOperation {
        name: "mcp_connection_status",
        scope: "manage",
        description: "Read OAuth sign-in status. Only the initiating human session can complete its native callback.",
    },
    McpOperation {
        name: "cancel_mcp_connection",
        scope: "manage",
        description: "Cancel pending OAuth sign-in without removing existing credentials.",
    },
];

pub(crate) fn mcp_operation(name: &str) -> Option<&'static McpOperation> {
    MCP_OPERATIONS.iter().find(|op| op.name == name)
}

impl McpOperation {
    pub(crate) fn descriptor(&self) -> Value {
        let (input, output) = match self.name {
            "list_mcps" => (
                schema_for::<ListMcpsInput>(Contract::Deserialize),
                schema_for::<ListMcpsOutput>(Contract::Serialize),
            ),
            "create_mcp" => (
                schema_for::<McpInput>(Contract::Deserialize),
                schema_for::<McpView>(Contract::Serialize),
            ),
            "update_mcp" => (
                schema_for::<UpdateMcpInput>(Contract::Deserialize),
                schema_for::<McpView>(Contract::Serialize),
            ),
            "test_mcp" => (
                schema_for::<McpIdInput>(Contract::Deserialize),
                schema_for::<McpView>(Contract::Serialize),
            ),
            "begin_mcp_connection" => (
                schema_for::<BeginMcpInput>(Contract::Deserialize),
                schema_for::<BeginMcpOutput>(Contract::Serialize),
            ),
            "mcp_connection_status" => (
                schema_for::<McpIdInput>(Contract::Deserialize),
                schema_for::<McpConnectionStatus>(Contract::Serialize),
            ),
            _ => (
                schema_for::<McpIdInput>(Contract::Deserialize),
                schema_for::<McpMutationOutput>(Contract::Serialize),
            ),
        };
        json!({
            "name":self.name,
            "description":self.description,
            "inputSchema":input,
            "outputSchema":output,
            "_meta":{"securitySchemes":[{"type":"oauth2","scopes":[self.scope]}]},
            "annotations":{"readOnlyHint":self.scope=="read","destructiveHint":self.scope!="read","openWorldHint":self.scope!="read"}
        })
    }

    /// `binding` is supplied only by the authenticated HTTP adapter, never command JSON.
    pub(crate) async fn invoke(
        &self,
        s: &Service,
        input: Value,
        context: ResumeContext,
        binding: Option<String>,
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
        let authorize =
            crate::installation_mcp::management_authorization(s, bearer.as_deref()).await?;
        match self.name {
            "list_mcps" => {
                let _: ListMcpsInput = decode(input)?;
                s.store
                    .read({
                        let authorize = authorize.clone();
                        move |db| authorize.authorize(db)
                    })
                    .await?;
                let mcps = s.mcps.list(s).await?;
                s.store.read(move |db| authorize.authorize(db)).await?;
                Ok(json!({"mcps":mcps}))
            }
            "create_mcp" => {
                let input: McpInput = decode(input)?;
                Ok(serde_json::to_value(
                    s.mcps.save_live(s, input, None, None, authorize).await?,
                )?)
            }
            "update_mcp" => {
                let input: UpdateMcpInput = decode(input)?;
                s.mcps.assert_management(s, &input.id).await?;
                Ok(serde_json::to_value(
                    s.mcps
                        .save_live(
                            s,
                            input.connection.into(),
                            Some(&input.id),
                            Some(input.revision),
                            authorize,
                        )
                        .await?,
                )?)
            }
            "begin_mcp_connection" => {
                let input: BeginMcpInput = decode(input)?;
                s.mcps.assert_management(s, &input.id).await?;
                if let Some(binding) = binding {
                    return if input.native {
                        s.mcps.connect_native(s, &input.id, &binding).await
                    } else {
                        s.mcps.connect(s, &input.id, &binding).await
                    };
                }
                let server = s.mcps.server(s, &input.id).await?;
                if !server.uses_oauth() || !server.is_http() {
                    return Err(Error::bad("This connection does not use OAuth."));
                }
                s.store.read(move |db| authorize.authorize(db)).await?;
                let url = crate::mcps::callback_url(s)
                    .await?
                    .trim_end_matches("/callback")
                    .to_owned();
                Ok(serde_json::to_value(BeginMcpOutput { url })?)
            }
            _ => {
                let input: McpIdInput = decode(input)?;
                s.mcps.assert_management(s, &input.id).await?;
                match self.name {
                    "test_mcp" => Ok(serde_json::to_value(
                        s.mcps.test_live(s, &input.id, authorize).await?,
                    )?),
                    "mcp_connection_status" => {
                        if let Some(binding) = binding {
                            return s.mcps.finish_native_callback(s, &input.id, &binding).await;
                        }
                        let status=s.store.read(move |db|{authorize.authorize(db)?;let server=crate::error::required(db.mcp_server(&input.id)?,"MCP connection not found.")?;let pending:bool=db.0.query_row("SELECT EXISTS(SELECT 1 FROM mcp_pending_oauth WHERE server_id=? AND expires_at>?)",rusqlite::params![input.id,crate::config::now()],|row|row.get(0))?;Ok(McpConnectionStatus{pending,result:Some(serde_json::to_value(server.state)?.as_str().unwrap().to_owned())})}).await?;
                        Ok(serde_json::to_value(status)?)
                    }
                    "cancel_mcp_connection" => {
                        let _guard = s.mcps.lock(&input.id).await;
                        let mut secrets = s.mcps.server_secrets(s, &input.id).await?;
                        secrets.verifier = None;
                        let ciphertext = s
                            .vault
                            .encrypt_bytes(&input.id, &serde_json::to_vec(&secrets)?)?;
                        s.store
                            .transaction(move |db| {
                                authorize.authorize(db)?;
                                crate::error::required(
                                    db.mcp_server(&input.id)?,
                                    "MCP connection not found.",
                                )?;
                                db.cancel_mcp_oauth(&input.id)?;
                                db.save_mcp_credentials(&input.id, &ciphertext)
                            })
                            .await?;
                        Ok(serde_json::to_value(McpMutationOutput { completed: true })?)
                    }
                    _ => {
                        s.mcps
                            .disconnect_live(s, &input.id, self.name == "delete_mcp", authorize)
                            .await?;
                        Ok(serde_json::to_value(McpMutationOutput { completed: true })?)
                    }
                }
            }
        }
    }
}
