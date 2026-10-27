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
    Public(Vec<String>, Option<String>),
    Installation(String),
}

impl ResumeContext {
    pub(crate) fn http(input: &Input) -> Result<Self> {
        let identity = input
            .identity
            .as_ref()
            .ok_or_else(|| Error::unauthorized("A verified identity is required."))?;
        if let Some(scopes) = &identity.mcp_scopes {
            return Ok(Self::Public(
                scopes.clone(),
                identity.mcp_credential_digest.clone(),
            ));
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
            ResumeContext::Public(scopes, _) => {
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
        if origin.known() && policy::supports(&run["snapshot"]["agent"], "native") {
            policy::refresh_for(s, &run["snapshot"]["agent"]).await?;
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

pub(crate) struct AgentOperation {
    pub name: &'static str,
    scope: &'static str,
    description: &'static str,
}

pub(crate) const AGENT_OPERATIONS: &[AgentOperation] = &[
    AgentOperation {
        name: "set_agent_avatar",
        scope: "manage",
        description: "Upload a PNG, JPEG or WebP portrait as base64 (at most 5 MiB); normalization and status match the apps.",
    },
    AgentOperation {
        name: "list_agents",
        scope: "read",
        description: "List typed agent profiles.",
    },
    AgentOperation {
        name: "get_agent",
        scope: "read",
        description: "Read an agent profile and its portrait status.",
    },
    AgentOperation {
        name: "save_agent",
        scope: "manage",
        description: "Create an agent with a stable optional creationId. access.backend=auto chooses an available authorized VM or native node; nodePriorities lists preferred authorized nodes in order. Existing conversations keep their environment. Expanding permissions requires the owner.",
    },
    AgentOperation {
        name: "replace_agent",
        scope: "manage",
        description: "Replace all settings at the expected revision, including access.backend (auto, firecracker or native) and ordered nodePriorities. Priorities confer no access. Agents cannot expand permissions or approve sensitive rights.",
    },
    AgentOperation {
        name: "delete_agent",
        scope: "manage",
        description: "Delete an unused agent.",
    },
    AgentOperation {
        name: "preview_agent",
        scope: "manage",
        description: "Preview work and delegations affected by a replacement, without writing.",
    },
    AgentOperation {
        name: "merge_agents",
        scope: "manage",
        description: "Merge an idle agent into a destination already covering all its grants, preserving resumable work.",
    },
    AgentOperation {
        name: "get_agent_avatar_configuration",
        scope: "read",
        description: "Whether avatar generation is configured; no account details are returned.",
    },
    AgentOperation {
        name: "generate_agent_avatar",
        scope: "manage",
        description: "Schedule an agent portrait. Read get_agent for generation status.",
    },
];

pub(crate) fn agent_operation(name: &str) -> Option<&'static AgentOperation> {
    AGENT_OPERATIONS.iter().find(|op| op.name == name)
}

impl AgentOperation {
    pub(crate) fn descriptor(&self) -> Value {
        use cairn_contracts::*;
        let (input, output) = match self.name {
            "set_agent_avatar" => (
                schema_for::<SetAgentAvatarInput>(Contract::Deserialize),
                schema_for::<Agent>(Contract::Serialize),
            ),
            "list_agents" => (
                schema_for::<ListAgentsInput>(Contract::Deserialize),
                schema_for::<ListAgentsOutput>(Contract::Serialize),
            ),
            "save_agent" => (
                schema_for::<CreateAgentInput>(Contract::Deserialize),
                schema_for::<Agent>(Contract::Serialize),
            ),
            "replace_agent" => (
                schema_for::<ReplaceAgentInput>(Contract::Deserialize),
                schema_for::<Agent>(Contract::Serialize),
            ),
            "preview_agent" => (
                schema_for::<ReplaceAgentInput>(Contract::Deserialize),
                schema_for::<AgentPreview>(Contract::Serialize),
            ),
            "delete_agent" => (
                schema_for::<AgentInput>(Contract::Deserialize),
                schema_for::<DeleteAgentOutput>(Contract::Serialize),
            ),
            "merge_agents" => (
                schema_for::<MergeAgentsInput>(Contract::Deserialize),
                schema_for::<MergeAgentsOutput>(Contract::Serialize),
            ),
            "get_agent_avatar_configuration" => (
                schema_for::<ListAgentsInput>(Contract::Deserialize),
                schema_for::<AvatarConfiguration>(Contract::Serialize),
            ),
            _ => (
                schema_for::<AgentInput>(Contract::Deserialize),
                schema_for::<Agent>(Contract::Serialize),
            ),
        };
        serde_json::json!({
            "name": self.name,
            "description": self.description,
            "inputSchema": input,
            "outputSchema": output,
            "_meta": { "securitySchemes": [{ "type": "oauth2", "scopes": [self.scope] }] },
            "annotations": {
                "readOnlyHint": self.scope == "read" || self.name == "preview_agent",
                "destructiveHint": self.scope != "read" && self.name != "preview_agent",
                "openWorldHint": self.name == "generate_agent_avatar",
            },
        })
    }

    pub(crate) async fn invoke(
        &self,
        s: &Arc<Service>,
        input: Value,
        context: ResumeContext,
    ) -> Result<Value> {
        use cairn_contracts::*;
        let (editor, bearer) = match context {
            ResumeContext::Human(_) => (crate::service::AgentEditor::Owner, None),
            ResumeContext::Public(scopes, _) => {
                if !scopes.iter().any(|scope| scope == self.scope) {
                    return Err(Error::forbidden(format!(
                        "The {} scope is required.",
                        self.scope
                    )));
                }
                (crate::service::AgentEditor::Installation, None)
            }
            ResumeContext::Installation(bearer) => {
                crate::installation_mcp::authorize(s, &bearer).await?;
                (crate::service::AgentEditor::Installation, Some(bearer))
            }
        };
        let authorize = crate::installation_mcp::management_authorization(bearer.as_deref());
        match self.name {
            "set_agent_avatar" => {
                let input: SetAgentAvatarInput = decode(input)?;
                Ok(serde_json::to_value(
                    s.avatars
                        .upload(s, &input.agent_id.to_string(), &input.data, authorize)
                        .await?,
                )?)
            }
            "save_agent" => Ok(serde_json::to_value(
                s.save_profile(
                    crate::agents::SaveProfile::Create(decode(input)?),
                    editor,
                    authorize,
                )
                .await?,
            )?),
            "replace_agent" => Ok(serde_json::to_value(
                s.save_profile(
                    crate::agents::SaveProfile::Replace(decode(input)?),
                    editor,
                    authorize,
                )
                .await?,
            )?),
            "preview_agent" => Ok(serde_json::to_value(
                s.agent_preview(decode(input)?, authorize).await?,
            )?),
            "merge_agents" => Ok(serde_json::to_value(
                s.merge_agents_authorized(decode(input)?, authorize).await?,
            )?),
            "generate_agent_avatar" => {
                let input: AgentInput = decode(input)?;
                Ok(serde_json::to_value(
                    s.avatars
                        .generate_authorized(s, &input.agent_id.to_string(), authorize)
                        .await?,
                )?)
            }
            "delete_agent" => {
                let input: AgentInput = decode(input)?;
                let id = input.agent_id.to_string();
                s.store
                    .transaction(move |db| {
                        authorize(db)?;
                        crate::error::required(db.agent(&id)?, "Agent not found")?;
                        crate::service::ensure_removable(db, "agents", &id)?;
                        crate::service::forget_agent(db, &id)?;
                        db.remove_agent(&id)
                    })
                    .await?;
                Ok(serde_json::to_value(DeleteAgentOutput { deleted: true })?)
            }
            name => {
                let agent_id = if name == "get_agent" {
                    Some(decode::<AgentInput>(input)?.agent_id)
                } else {
                    let _: ListAgentsInput = decode(input)?;
                    None
                };
                let configured = if name == "get_agent_avatar_configuration" {
                    s.avatars.configured(s).await?
                } else {
                    false
                };
                let list = name == "list_agents";
                s.store
                    .read(move |db| {
                        let tx = rusqlite::Transaction::new_unchecked(
                            db.0,
                            rusqlite::TransactionBehavior::Deferred,
                        )?;
                        let db = Db(&tx);
                        authorize(&db)?;
                        let result = if let Some(id) = agent_id {
                            serde_json::to_value(crate::error::required(
                                db.agent(&id.to_string())?,
                                "Agent not found",
                            )?)?
                        } else if list {
                            serde_json::to_value(ListAgentsOutput {
                                agents: db.agents()?,
                            })?
                        } else {
                            serde_json::to_value(AvatarConfiguration { configured })?
                        };
                        tx.commit()?;
                        Ok(result)
                    })
                    .await
            }
        }
    }
}

pub(crate) use crate::missions::{MISSION_OPERATIONS, MissionOperation, mission_operation};

mod mcps;

pub(crate) use mcps::{MCP_OPERATIONS, McpOperation, mcp_operation};

/// Plugin management retains one named entry with a closed, generated command
/// union. Manifest payloads do not supply authority or Cairn configuration keys.
pub(crate) struct ManagePlugin;

pub(crate) const MANAGE_PLUGIN: ManagePlugin = ManagePlugin;

impl ManagePlugin {
    pub(crate) fn descriptor(&self) -> Value {
        serde_json::json!({
            "name": "manage_plugin",
            "description": "Configure a plugin instance with a typed command. Setup and sign-in keep credentials on the host. Agents cannot grant plugin access or approve native delegations; the owner does so in the apps.",
            "inputSchema": cairn_contracts::schema_for::<cairn_contracts::ManagePluginInput>(cairn_contracts::Contract::Deserialize),
            "outputSchema": cairn_contracts::schema_for::<cairn_contracts::ManagePluginOutput>(cairn_contracts::Contract::Serialize),
            "_meta": { "securitySchemes": [{ "type": "oauth2", "scopes": ["manage"] }] },
            "annotations": { "readOnlyHint": false, "destructiveHint": true, "openWorldHint": true },
        })
    }

    pub(crate) async fn invoke(
        &self,
        s: &Arc<Service>,
        input: Value,
        context: ResumeContext,
    ) -> Result<Value> {
        let bearer = match context {
            ResumeContext::Human(_) => None,
            ResumeContext::Public(scopes, _) => {
                if !scopes.iter().any(|scope| scope == "manage") {
                    return Err(Error::forbidden("The manage scope is required."));
                }
                None
            }
            ResumeContext::Installation(bearer) => Some(bearer),
        };
        let input = crate::plugins::management::decode(input)?;
        Ok(serde_json::to_value(
            Box::pin(crate::plugins::management::invoke(
                s,
                input,
                bearer.as_deref(),
                false,
                None,
            ))
            .await?,
        )?)
    }
}

pub(crate) use crate::runs::{RUN_OPERATIONS, RunOperation, run_operation};

pub(crate) mod plugin_packages;

pub(crate) use plugin_packages::{PLUGIN_PACKAGE_OPERATIONS, plugin_package_operation};
