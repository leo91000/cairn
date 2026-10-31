//! Run deliverables, including mission runs without conversations, on the shared registry.
use super::{ResumeContext, decode};
use crate::{
    error::{Error, Result, required},
    service::Service,
    skills::lifecycle::Authority,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use cairn_contracts::*;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncSeekExt};

pub(crate) struct ArtifactOperation {
    pub name: &'static str,
    scope: &'static str,
    description: &'static str,
}

pub(crate) const ARTIFACT_OPERATIONS: &[ArtifactOperation] = &[
    ArtifactOperation {
        name: "list_run_artifacts",
        scope: "read",
        description: "List published file versions of a run, including a mission run without a conversation. Paginated; files remain private unless explicitly shared.",
    },
    ArtifactOperation {
        name: "read_run_artifact",
        scope: "read",
        description: "Read a run artifact as bounded base64 chunks, including private files and mission runs. Start at offset 0; append decoded data until nextOffset is null. file.digest is the complete SHA-256.",
    },
    ArtifactOperation {
        name: "set_run_artifact_visibility",
        scope: "manage",
        description: "Enable or revoke a public link for one run artifact version, including a mission run. Only make files public when requested. Human sharing requires the installation owner; execution access requires a current installation grant. Revocation cannot remove downloaded copies.",
    },
];

pub(crate) fn artifact_operation(name: &str) -> Option<&'static ArtifactOperation> {
    ARTIFACT_OPERATIONS.iter().find(|op| op.name == name)
}

impl ArtifactOperation {
    pub(crate) fn descriptor(&self) -> Value {
        let (input, output) = match self.name {
            "list_run_artifacts" => (
                schema_for::<ListRunArtifactsInput>(Contract::Deserialize),
                schema_for::<ListRunArtifactsOutput>(Contract::Serialize),
            ),
            "read_run_artifact" => (
                schema_for::<ReadRunArtifactInput>(Contract::Deserialize),
                schema_for::<ReadRunArtifactOutput>(Contract::Serialize),
            ),
            _ => (
                schema_for::<SetRunArtifactVisibilityInput>(Contract::Deserialize),
                schema_for::<Deliverable>(Contract::Serialize),
            ),
        };
        json!({
            "name": self.name,
            "description": self.description,
            "inputSchema": input,
            "outputSchema": output,
            "_meta": {
                "securitySchemes": [{ "type": "oauth2", "scopes": [self.scope] }],
            },
            "annotations": {
                "readOnlyHint": self.scope == "read",
                "destructiveHint": self.scope == "manage",
                "idempotentHint": true,
                "openWorldHint": false,
            },
        })
    }

    pub(crate) async fn invoke(
        &self,
        s: &Service,
        input: Value,
        context: ResumeContext,
    ) -> Result<Value> {
        let authority = Authority::new(s, context, self.scope).await?;
        match self.name {
            "list_run_artifacts" => {
                let input: ListRunArtifactsInput = decode(input)?;
                if input.offset > 9007199254740991 || !(1..=100).contains(&input.limit) {
                    return Err(Error::bad("Invalid artifact page."));
                }
                let run = input.run_id.to_string();
                let items = s
                    .store
                    .read(move |db| {
                        authority.authorize(db)?;
                        crate::conversation_lifecycle::require_active_run(db, &run)?;
                        db.require_run(&run)?;
                        db.artifacts(Some(&run))
                    })
                    .await?;
                let offset =
                    usize::try_from(input.offset).map_err(|_| Error::bad("Invalid offset."))?;
                let mut artifacts = Vec::new();
                let mut bytes = 0;
                for item in items.iter().skip(offset).take(input.limit as usize) {
                    let size = serde_json::to_vec(item)?.len();
                    if !artifacts.is_empty() && bytes + size > 256 * 1024 {
                        break;
                    }
                    if size > 256 * 1024 {
                        return Err(Error::too_large("Artifact metadata is too large."));
                    }
                    bytes += size;
                    artifacts.push(item.clone());
                }
                let next = offset + artifacts.len();
                Ok(serde_json::to_value(ListRunArtifactsOutput {
                    artifacts,
                    next_offset: (next < items.len()).then_some(next as u64),
                    has_more: next < items.len(),
                })?)
            }
            "read_run_artifact" => {
                let input: ReadRunArtifactInput = decode(input)?;
                let run = input.run_id.to_string();
                let id = input.artifact_id.to_string();
                let auth = authority.clone();
                let check_run = run.clone();
                let item = s
                    .store
                    .read(move |db| {
                        auth.authorize(db)?;
                        crate::conversation_lifecycle::require_active_run(db, &check_run)?;
                        db.require_run(&check_run)?;
                        required(
                            db.artifact(&check_run, &id)?,
                            "Artifact not found in this run",
                        )
                    })
                    .await?;
                if input.offset > item.size {
                    return Err(Error::bad("Offset is beyond the file."));
                }
                let path = s.config.data_dir.join("artifacts").join(&item.id);
                let mut file = tokio::fs::OpenOptions::new()
                    .read(true)
                    .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
                    .open(path)
                    .await?;
                let metadata = file.metadata().await?;
                if !metadata.is_file() || metadata.len() != item.size {
                    return Err(Error::conflict("Stored file size changed."));
                }
                file.seek(std::io::SeekFrom::Start(input.offset)).await?;
                let mut bytes = Vec::new();
                file.take(192 * 1024).read_to_end(&mut bytes).await?;
                // The same revocable execution grant and lifecycle are checked after IO.
                s.store
                    .read(move |db| {
                        authority.authorize(db)?;
                        crate::conversation_lifecycle::require_active_run(db, &run)?;
                        db.require_run(&run)?;
                        Ok(())
                    })
                    .await?;
                let next = input.offset + bytes.len() as u64;
                Ok(serde_json::to_value(ReadRunArtifactOutput {
                    offset: input.offset,
                    total_bytes: item.size,
                    next_offset: (next < item.size).then_some(next),
                    file: item,
                    encoding: "base64".to_owned(),
                    data: STANDARD.encode(bytes),
                })?)
            }
            _ => {
                let input: SetRunArtifactVisibilityInput = decode(input)?;
                crate::artifacts::sharing::set_authorized(
                    s,
                    &input.run_id.to_string(),
                    &input.artifact_id.to_string(),
                    input.visibility.as_str(),
                    move |db| authority.authorize(db),
                )
                .await
            }
        }
    }
}
