//! B3 resource commands. HTTP, public MCP and execution grants share these
//! handlers, including the existing bundle editor and bounded file transports.
use super::{ResumeContext, decode};
use crate::{
    error::{Error, Result},
    service::Service,
    skills::{self, lifecycle::Authority, sources},
};
use cairn_contracts::*;
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use std::sync::Arc;

pub(crate) struct ResourceOperation {
    pub name: &'static str,
    scope: &'static str,
    description: &'static str,
}

pub(crate) const RESOURCE_OPERATIONS: &[ResourceOperation] = &[
    ResourceOperation {
        name: "import_project",
        scope: "manage",
        description: "Import a project through an already authorized plugin instance. Does not grant project or plugin access to the caller.",
    },
    ResourceOperation {
        name: "list_source_projects",
        scope: "manage",
        description: "List a bounded page of projects from an already authorized plugin instance.",
    },
    ResourceOperation {
        name: "list_skills",
        scope: "read",
        description: "List skills, source metadata and usage. Optional scope is global or a project UUID; omit it to list all scopes.",
    },
    ResourceOperation {
        name: "read_skill",
        scope: "read",
        description: "Read a skill bundle, or one file when path is supplied. Scope defaults to global.",
    },
    ResourceOperation {
        name: "save_skill",
        scope: "manage",
        description: "Create or replace SKILL.md through the existing bundle writer. Scope defaults to global.",
    },
    ResourceOperation {
        name: "edit_skill_files",
        scope: "manage",
        description: "Atomically edit a skill bundle against base revision (null to create). Stage large files first.",
    },
    ResourceOperation {
        name: "stage_skill_upload",
        scope: "manage",
        description: "Stage base64 file/archive bytes (maximum 5 MiB) for import or edit_skill_files. Returns a private upload id expiring after one hour. Raw HTTP uploads are an equivalent transport.",
    },
    ResourceOperation {
        name: "import_skill",
        scope: "manage",
        description: "Import a skill from a staged archive or an already authorized plugin instance. Multiple folders return bounded candidates without writing.",
    },
    ResourceOperation {
        name: "delete_skill",
        scope: "manage",
        description: "Delete a skill and forget its source. A mission selecting the skill prevents deletion.",
    },
    ResourceOperation {
        name: "export_skill_archive",
        scope: "read",
        description: "Export one base64 ZIP chunk (512 KiB decoded). Repeat with the returned revision and offset plus decoded bytes until size is reached.",
    },
    ResourceOperation {
        name: "get_skill_source_status",
        scope: "manage",
        description: "Compare an imported skill's source with its latest version through an already authorized plugin instance.",
    },
    ResourceOperation {
        name: "update_skill_source",
        scope: "manage",
        description: "Replace an imported skill from the chosen source version against its current base revision.",
    },
    ResourceOperation {
        name: "forget_skill_source",
        scope: "manage",
        description: "Forget a skill's source while retaining its files.",
    },
];

pub(crate) fn resource_operation(name: &str) -> Option<&'static ResourceOperation> {
    RESOURCE_OPERATIONS.iter().find(|op| op.name == name)
}

fn output<T: DeserializeOwned + Serialize>(value: Value) -> Result<Value> {
    let typed: T = serde_json::from_value(value)?;
    Ok(serde_json::to_value(typed)?)
}

impl ResourceOperation {
    pub(crate) fn descriptor(&self) -> Value {
        let input = match self.name {
            "import_project" => schema_for::<ImportProjectInput>(Contract::Deserialize),
            "list_source_projects" => schema_for::<ListSourceProjectsInput>(Contract::Deserialize),
            "list_skills" => schema_for::<ListSkillsInput>(Contract::Deserialize),
            "read_skill" => schema_for::<ReadSkillInput>(Contract::Deserialize),
            "save_skill" => schema_for::<SaveSkillInput>(Contract::Deserialize),
            "edit_skill_files" => schema_for::<EditSkillFilesInput>(Contract::Deserialize),
            "stage_skill_upload" => schema_for::<StageSkillUploadInput>(Contract::Deserialize),
            "import_skill" => schema_for::<ImportSkillInput>(Contract::Deserialize),
            "export_skill_archive" => schema_for::<ExportSkillArchiveInput>(Contract::Deserialize),
            "update_skill_source" => schema_for::<UpdateSkillSourceInput>(Contract::Deserialize),
            _ => schema_for::<SkillInput>(Contract::Deserialize),
        };
        let result = match self.name {
            "import_project" => schema_for::<Project>(Contract::Serialize),
            "list_source_projects" => schema_for::<PluginProjectPage>(Contract::Serialize),
            "list_skills" => schema_for::<Vec<Skill>>(Contract::Serialize),
            "read_skill" => schema_for::<ReadSkillOutput>(Contract::Serialize),
            "save_skill" => schema_for::<SkillMetadata>(Contract::Serialize),
            "stage_skill_upload" => schema_for::<SkillUpload>(Contract::Serialize),
            "import_skill" => schema_for::<SkillImport>(Contract::Serialize),
            "export_skill_archive" => schema_for::<SkillArchiveChunk>(Contract::Serialize),
            "get_skill_source_status" => schema_for::<SkillSourceStatus>(Contract::Serialize),
            "delete_skill" => schema_for::<DeleteSkillOutput>(Contract::Serialize),
            "forget_skill_source" => schema_for::<ForgetSkillSourceOutput>(Contract::Serialize),
            _ => schema_for::<SkillBundle>(Contract::Serialize),
        };
        json!({
            "name": self.name,
            "description": self.description,
            "inputSchema": input,
            "outputSchema": result,
            "_meta": { "securitySchemes": [{ "type": "oauth2", "scopes": [self.scope] }] },
            "annotations": {
                "readOnlyHint": self.scope == "read" || matches!(self.name, "get_skill_source_status" | "list_source_projects"),
                "destructiveHint": matches!(self.name, "delete_skill" | "update_skill_source" | "edit_skill_files" | "save_skill" | "forget_skill_source"),
                "openWorldHint": true,
            },
        })
    }

    pub(crate) fn invoke<'a>(
        &'a self,
        s: &'a Arc<Service>,
        input: Value,
        context: ResumeContext,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Value>> + Send + 'a>> {
        // Keep the large source-download future off the callers' stacks. API
        // dispatch and MCP both compose several domains in the same future.
        Box::pin(self.invoke_inner(s, input, context))
    }

    async fn invoke_inner(
        &self,
        s: &Arc<Service>,
        input: Value,
        context: ResumeContext,
    ) -> Result<Value> {
        let mut authority = Authority::new(s, context, self.scope).await?;
        let result = match self.name {
            "import_project" => {
                let input: ImportProjectInput = decode(input)?;
                return crate::plugins::projects::import(s, &input, authority).await;
            }
            "list_source_projects" => {
                let input: ListSourceProjectsInput = decode(input)?;
                return output::<PluginProjectPage>(
                    crate::plugins::projects::list(s, &input, authority).await?,
                );
            }
            "list_skills" => {
                let input: ListSkillsInput = decode(input)?;
                let mut items = Vec::new();
                if input.scope.as_deref().is_none_or(|scope| scope == "global") {
                    items.extend(s.skills.list("global", None).await?);
                }
                let projects = match input.scope.as_deref() {
                    None => s.store.projects().await?,
                    Some("global") => Vec::new(),
                    Some(scope) => vec![s.get_project(scope).await?],
                };
                for project in projects {
                    items.extend(
                        s.skills
                            .list(
                                &project.id.to_string(),
                                Some(std::path::Path::new(&project.settings.path)),
                            )
                            .await?,
                    );
                }
                sources::attach(s, &mut items).await?;
                s.attach_skill_usage(&mut items).await?;
                authority.recheck(s).await?;
                return output::<Vec<Skill>>(items.into());
            }
            "stage_skill_upload" => {
                use base64::Engine;
                let input: StageSkillUploadInput = decode(input)?;
                if input.content.len() > skills::bundle::UPLOAD_BYTES.div_ceil(3) * 4 {
                    return Err(Error::too_large("Uploads are limited to 5 MiB."));
                }
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(&input.content)
                    .map_err(|_| Error::bad("Invalid base64 content."))?;
                return stage_resource_upload(s, &bytes, authority).await;
            }
            "import_skill" => {
                let command: ImportSkillInput = decode(input)?;
                let scope = match &command {
                    ImportSkillInput::Archive { scope, .. }
                    | ImportSkillInput::Plugin { scope, .. } => scope.clone(),
                };
                let project = authority.project(s, &scope).await?;
                let result = match &command {
                    ImportSkillInput::Archive { upload, path, .. } => {
                        authority.recheck(s).await?;
                        s.skills
                            .import_archive(
                                &scope,
                                upload,
                                path.as_deref(),
                                project.as_deref(),
                                authority.publication(s),
                            )
                            .await?
                    }
                    ImportSkillInput::Plugin { .. } => {
                        sources::import(s, &scope, &command, project.as_deref(), authority).await?
                    }
                };
                if let Some(name) = result["imported"]["name"].as_str() {
                    s.store
                        .audit("skill.saved", json!({"scope": scope, "name": name}))
                        .await?;
                }
                return output::<SkillImport>(result);
            }
            "read_skill" => {
                let input: ReadSkillInput = decode(input)?;
                skills::name(&input.name)?;
                let project = authority.project(s, &input.scope).await?;
                let result = if let Some(path) = input.path {
                    output::<SkillFileContent>(
                        s.skills
                            .read(&input.name, &path, project.as_deref())
                            .await?,
                    )?
                } else {
                    let mut bundle = s
                        .skills
                        .bundle(&input.scope, &input.name, project.as_deref())
                        .await?;
                    sources::source(s, &input.scope, &input.name, &mut bundle).await?;
                    output::<SkillBundle>(bundle)?
                };
                authority.recheck(s).await?;
                return Ok(result);
            }
            "save_skill" => {
                let input: SaveSkillInput = decode(input)?;
                let project = authority.project(s, &input.scope).await?;
                authority.recheck(s).await?;
                let saved = s
                    .skills
                    .save_authorized(
                        &input.name,
                        &input.content,
                        project.as_deref(),
                        Some(authority.publication(s)),
                    )
                    .await?;
                s.store
                    .audit(
                        "skill.saved",
                        json!({"scope": input.scope, "name": input.name}),
                    )
                    .await?;
                return output::<SkillMetadata>(saved);
            }
            "edit_skill_files" => {
                let input: EditSkillFilesInput = decode(input)?;
                let project = authority.project(s, &input.scope).await?;
                let (changes, uploads) = s
                    .skills
                    .changes(&serde_json::to_value(input.changes)?)
                    .await?;
                authority.recheck(s).await?;
                let saved = s
                    .skills
                    .apply_authorized(
                        &input.name,
                        input.base.as_deref(),
                        &changes,
                        project.as_deref(),
                        Some(authority.publication(s)),
                    )
                    .await?;
                s.skills.discard(&uploads).await;
                s.store
                    .audit(
                        "skill.saved",
                        json!({"scope": input.scope, "name": input.name}),
                    )
                    .await?;
                let mut bundle = skills::bundle_response(&input.scope, &input.name, &saved);
                sources::source(s, &input.scope, &input.name, &mut bundle).await?;
                return output::<SkillBundle>(bundle);
            }
            "export_skill_archive" => {
                let input: ExportSkillArchiveInput = decode(input)?;
                if input.offset > u64::from(u32::MAX) {
                    return Err(Error::bad("offset: too large"));
                }
                let project = authority.project(s, &input.scope).await?;
                let result = s
                    .skills
                    .archive(
                        &input.name,
                        input.revision.as_deref().filter(|r| !r.is_empty()),
                        input.offset,
                        project.as_deref(),
                    )
                    .await?;
                authority.recheck(s).await?;
                return output::<SkillArchiveChunk>(result);
            }
            "update_skill_source" => {
                let input: UpdateSkillSourceInput = decode(input)?;
                let project = authority.project(s, &input.scope).await?;
                return output::<SkillBundle>(
                    sources::update(
                        s,
                        &input.scope,
                        &input.name,
                        &input,
                        project.as_deref(),
                        authority,
                    )
                    .await?,
                );
            }
            _ => {
                let input: SkillInput = decode(input)?;
                skills::name(&input.name)?;
                let project = authority.project(s, &input.scope).await?;
                match self.name {
                    "get_skill_source_status" => {
                        let result = sources::status(
                            s,
                            &input.scope,
                            &input.name,
                            project.as_deref(),
                            authority.clone(),
                        )
                        .await?;
                        authority.recheck(s).await?;
                        return Ok(serde_json::to_value(result)?);
                    }
                    "forget_skill_source" => {
                        sources::forget(s, &input.scope, &input.name, authority).await?;
                        serde_json::to_value(ForgetSkillSourceOutput { detached: true })?
                    }
                    "delete_skill" => {
                        skills::lifecycle::remove(
                            s,
                            &input.scope,
                            &input.name,
                            project.as_deref(),
                            authority,
                        )
                        .await?;
                        serde_json::to_value(DeleteSkillOutput { deleted: true })?
                    }
                    _ => unreachable!(),
                }
            }
        };
        Ok(result)
    }
}

/// Raw HTTP and base64 tool uploads share staging and final authorization.
pub(crate) async fn stage_resource_upload(
    s: &Service,
    bytes: &[u8],
    authority: Authority,
) -> Result<Value> {
    let result = s.skills.stage(bytes).await?;
    if let Err(error) = authority.recheck(s).await {
        s.skills
            .discard(&[crate::validation::text(&result, "id").to_owned()])
            .await;
        return Err(error);
    }
    output::<SkillUpload>(result)
}
