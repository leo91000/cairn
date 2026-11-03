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

async fn recheck_read(
    s: &Service,
    authority: Authority,
    credential: Option<&str>,
    run: String,
) -> Result<()> {
    // External delegation coverage can change without a local revision change.
    authority.recheck(s).await?;
    if let Some(credential) = credential {
        crate::relay::authorize_read_mcp(s, credential).await?;
    }
    // A conversation may be trashed or purged during the remote check.
    // Fence the prepared result against current local state after that await.
    s.store
        .read(move |db| {
            authority.authorize(db)?;
            crate::conversation_lifecycle::require_active_run(db, &run)?;
            db.require_run(&run)?;
            Ok(())
        })
        .await?;
    Ok(())
}

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
        let credential = match &context {
            ResumeContext::Public(_, credential) if self.scope == "read" => {
                Some(credential.clone().ok_or_else(|| {
                    Error::unauthorized("A current public MCP authorization is required.")
                })?)
            }
            _ => None,
        };
        let authority = Authority::new(s, context, self.scope, false).await?;
        self.invoke_admitted(s, input, authority, credential).await
    }

    async fn invoke_admitted(
        &self,
        s: &Service,
        input: Value,
        mut authority: Authority,
        credential: Option<String>,
    ) -> Result<Value> {
        if let Some(credential) = &credential {
            crate::relay::authorize_read_mcp(s, credential).await?;
        }
        match self.name {
            "list_run_artifacts" => {
                let input: ListRunArtifactsInput = decode(input)?;
                if input.offset > 9007199254740991 || !(1..=100).contains(&input.limit) {
                    return Err(Error::bad("Invalid artifact page."));
                }
                let run = input.run_id.to_string();
                let auth = authority.clone();
                let selected_run = run.clone();
                let items = s
                    .store
                    .read(move |db| {
                        auth.authorize(db)?;
                        crate::conversation_lifecycle::require_active_run(db, &selected_run)?;
                        db.require_run(&selected_run)?;
                        db.artifacts(Some(&selected_run))
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
                recheck_read(s, authority, credential.as_deref(), run).await?;
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
                // Recheck both local execution access and the originating public
                // authorization after file I/O, before releasing any bytes.
                recheck_read(s, authority, credential.as_deref(), run).await?;
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
                crate::artifacts::sharing::set_live(
                    s,
                    &input.run_id.to_string(),
                    &input.artifact_id.to_string(),
                    input.visibility.as_str(),
                    crate::authorization::Authorization::live(
                        s,
                        authority.retain_result_authority(),
                    ),
                )
                .await
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin_grant_fixture::Fixture;
    use std::sync::atomic::Ordering;

    #[tokio::test]
    async fn admitted_artifact_reads_recheck_external_delegation_before_returning_private_data() {
        let (f, bearer) = Fixture::delegated_manager().await;
        let id = uuid::Uuid::new_v4().to_string();
        let bytes = b"private delegated artifact";
        let artifact = crate::common::deliverable(
            &f.run,
            &id,
            &json!({
                "size": bytes.len(),
            }),
        );
        f.s.store
            .transaction(move |db| db.insert_artifact(&artifact))
            .await
            .unwrap();
        let directory = f.s.config.data_dir.join("artifacts");
        tokio::fs::create_dir_all(&directory).await.unwrap();
        tokio::fs::write(directory.join(&id), bytes).await.unwrap();

        let mut exposed = Vec::new();
        for name in ["list_run_artifacts", "read_run_artifact"] {
            f.moved_parent.store(false, Ordering::SeqCst);
            let operation = artifact_operation(name).unwrap();
            let input = if name == "list_run_artifacts" {
                json!({
                    "runId": f.run,
                })
            } else {
                json!({
                    "runId": f.run,
                    "artifactId": id,
                })
            };
            let context = || ResumeContext::Installation(bearer.clone());
            let valid = operation
                .invoke(&f.s, input.clone(), context())
                .await
                .unwrap();
            if name == "list_run_artifacts" {
                assert_eq!(valid["artifacts"][0]["id"], id);
            } else {
                assert_eq!(
                    STANDARD.decode(valid["data"].as_str().unwrap()).unwrap(),
                    bytes
                );
            }

            let admitted = Authority::new(&f.s, context(), "read", false)
                .await
                .unwrap();
            // External ancestry changes after admission. All SQLite grants and
            // revisions remain valid, so the local guard alone still succeeds.
            f.moved_parent.store(true, Ordering::SeqCst);
            let local = admitted.clone();
            f.s.store.read(move |db| local.authorize(db)).await.unwrap();
            let result = operation.invoke_admitted(&f.s, input, admitted, None).await;
            match result {
                Err(error) => assert_eq!(error.status, 403, "{name}"),
                Ok(_) => exposed.push(name),
            }
        }
        f.s.shutdown.cancel();
        assert!(
            exposed.is_empty(),
            "private metadata or bytes escaped stale delegated authority: {exposed:?}"
        );
    }
}
