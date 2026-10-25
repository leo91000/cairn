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
        let authorize = crate::installation_mcp::management_authorization(bearer.as_deref());
        s.worker
            .resume_authorized(s, &id, move |db: &Db<'_>| {
                authorize(db)?;
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

/// Project operations extend the B1 registry. Legacy domains keep their catalogue.
pub(crate) struct ProjectOperation {
    pub name: &'static str,
    scope: &'static str,
    description: &'static str,
}

pub(crate) const PROJECT_OPERATIONS: &[ProjectOperation] = &[
    ProjectOperation {
        name: "list_projects",
        scope: "read",
        description: "List registered projects.",
    },
    ProjectOperation {
        name: "get_project",
        scope: "read",
        description: "Read a registered project.",
    },
    ProjectOperation {
        name: "save_project",
        scope: "manage",
        description: "Create a project; creationId makes retries return the same project.",
    },
    ProjectOperation {
        name: "replace_project",
        scope: "manage",
        description: "Replace all project settings using the current revision.",
    },
    ProjectOperation {
        name: "delete_project",
        scope: "manage",
        description: "Delete an unused project; assigned projects, tasks and active work prevent deletion.",
    },
];

pub(crate) fn project_operation(name: &str) -> Option<&'static ProjectOperation> {
    PROJECT_OPERATIONS.iter().find(|op| op.name == name)
}

fn decode<T: serde::de::DeserializeOwned>(input: Value) -> Result<T> {
    serde_json::from_value(input).map_err(|error| Error::bad(error.to_string()))
}

impl ProjectOperation {
    pub(crate) fn descriptor(&self) -> Value {
        use cairn_contracts::*;
        let (input, output) = match self.name {
            "list_projects" => (
                schema_for::<ListProjectsInput>(Contract::Deserialize),
                schema_for::<ListProjectsOutput>(Contract::Serialize),
            ),
            "save_project" => (
                schema_for::<CreateProjectInput>(Contract::Deserialize),
                schema_for::<Project>(Contract::Serialize),
            ),
            "replace_project" => (
                schema_for::<ReplaceProjectInput>(Contract::Deserialize),
                schema_for::<Project>(Contract::Serialize),
            ),
            "delete_project" => (
                schema_for::<ProjectInput>(Contract::Deserialize),
                schema_for::<DeleteProjectOutput>(Contract::Serialize),
            ),
            _ => (
                schema_for::<ProjectInput>(Contract::Deserialize),
                schema_for::<Project>(Contract::Serialize),
            ),
        };
        serde_json::json!({
            "name": self.name,
            "description": self.description,
            "inputSchema": input,
            "outputSchema": output,
            "_meta": { "securitySchemes": [{ "type": "oauth2", "scopes": [self.scope] }] },
            "annotations": {
                "readOnlyHint": self.scope == "read",
                "destructiveHint": self.scope != "read",
                "openWorldHint": self.scope != "read",
            },
        })
    }

    pub(crate) async fn invoke(
        &self,
        s: &Service,
        input: Value,
        context: ResumeContext,
    ) -> Result<Value> {
        use cairn_contracts::*;
        let bearer = match context {
            ResumeContext::Human(_) => None, // HTTP enforces owner writes before dispatch.
            ResumeContext::Public(scopes) => {
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
        match self.name {
            "save_project" => Ok(serde_json::to_value(
                s.create_project(decode(input)?, None, authorize).await?,
            )?),
            "replace_project" => Ok(serde_json::to_value(
                s.replace_project(decode(input)?, authorize).await?,
            )?),
            "delete_project" => {
                let input: ProjectInput = decode(input)?;
                s.delete_project(input.project_id, authorize).await?;
                Ok(serde_json::to_value(DeleteProjectOutput { deleted: true })?)
            }
            "list_projects" => {
                let _: ListProjectsInput = decode(input)?;
                s.store
                    .read(move |db| {
                        let tx = rusqlite::Transaction::new_unchecked(
                            db.0,
                            rusqlite::TransactionBehavior::Deferred,
                        )?;
                        let db = Db(&tx);
                        authorize(&db)?;
                        let output = ListProjectsOutput {
                            projects: db.projects()?,
                        };
                        tx.commit()?;
                        Ok(serde_json::to_value(output)?)
                    })
                    .await
            }
            _ => {
                let input: ProjectInput = decode(input)?;
                s.store
                    .read(move |db| {
                        let tx = rusqlite::Transaction::new_unchecked(
                            db.0,
                            rusqlite::TransactionBehavior::Deferred,
                        )?;
                        let db = Db(&tx);
                        authorize(&db)?;
                        let project = crate::error::required(
                            db.project(&input.project_id.to_string())?,
                            "Project not found",
                        )?;
                        tx.commit()?;
                        Ok(serde_json::to_value(project)?)
                    })
                    .await
            }
        }
    }
}
