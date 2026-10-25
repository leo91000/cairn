//! The first converted capability. Other management operations keep their existing dispatcher.
use crate::{
    error::{Error, Result},
    http::Input,
    native::policy::{self, Origin},
    service::Service,
    store::Db,
};
use cairn_contracts::{ResumeRunInput, ResumeRunOutput};
use serde_json::Value;
use std::sync::Arc;

/// Constructed by authenticated transport adapters, never from command JSON.
pub(crate) enum ResumeContext {
    Human(String),
    Public(Vec<String>),
    Installation(String),
}

impl ResumeContext {
    pub(crate) fn http(input: &Input) -> Result<Self> {
        let identity = input
            .identity
            .as_ref()
            .ok_or_else(|| Error::unauthorized("A verified identity is required."))?;
        if let Some(scopes) = &identity.mcp_scopes {
            return Ok(Self::Public(scopes.clone()));
        }
        Ok(Self::Human(identity.account_id.clone()))
    }
}

pub(crate) struct ResumeRun;

pub(crate) const RESUME_RUN: ResumeRun = ResumeRun;

impl ResumeRun {
    pub(crate) fn decode(&self, input: Value) -> Result<ResumeRunInput> {
        serde_json::from_value(input).map_err(|error| Error::bad(error.to_string()))
    }

    /// All adapters invoke this definition; the worker remains the only writer.
    pub(crate) async fn invoke(
        &self,
        s: &Arc<Service>,
        input: ResumeRunInput,
        context: ResumeContext,
    ) -> Result<ResumeRunOutput> {
        let id = input.run_id.to_string();
        let (origin, bearer) = match context {
            ResumeContext::Human(account) => (Origin::Human(account), None),
            ResumeContext::Public(scopes) => {
                if !scopes
                    .iter()
                    .any(|scope| scope == cairn_contracts::RESUME_RUN_SCOPE)
                {
                    return Err(Error::forbidden("The run scope is required."));
                }
                // An MCP scope is not a human native-execution proof.
                (Origin::Unknown, None)
            }
            ResumeContext::Installation(bearer) => {
                let run = crate::installation_mcp::authorize(s, &bearer).await?;
                let caller = crate::validation::text(&run, "id").to_owned();
                (Origin::Run(caller), Some(bearer))
            }
        };
        let run = s.store.run(&id).await?;
        if origin.known() && policy::native(&run["snapshot"]["agent"]) {
            policy::refresh_agent(s, crate::nodes::run_agent(&run)).await?;
        }
        let commit_id = id.clone();
        s.worker
            .resume_authorized(s, &id, move |db: &Db<'_>| {
                if let Some(bearer) = &bearer {
                    crate::installation_mcp::authorize_in(db, bearer)?;
                }
                policy::authorize_run_in(db, &commit_id, &origin)
            })
            .await?;
        Ok(ResumeRunOutput {
            run_id: input.run_id,
        })
    }

    pub(crate) fn descriptor(&self) -> Value {
        let input_schema =
            cairn_contracts::schema_for::<ResumeRunInput>(cairn_contracts::Contract::Deserialize);
        let output_schema =
            cairn_contracts::schema_for::<ResumeRunOutput>(cairn_contracts::Contract::Serialize);
        serde_json::json!({
            "name": cairn_contracts::RESUME_RUN_ID,
            "description": cairn_contracts::RESUME_RUN_DESCRIPTION,
            "inputSchema": input_schema,
            "outputSchema": output_schema,
            "_meta": {
                "securitySchemes": [{
                    "type": "oauth2",
                    "scopes": [cairn_contracts::RESUME_RUN_SCOPE],
                }],
            },
            "annotations": {
                "readOnlyHint": false,
                "destructiveHint": true,
                "idempotentHint": false,
                "openWorldHint": true,
            },
        })
    }
}
