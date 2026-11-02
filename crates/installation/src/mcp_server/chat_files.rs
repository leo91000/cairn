//! Bounded file reads: installation access never opens caller-supplied host paths.
use super::chat_tools::page;
use crate::{
    conversation_lifecycle::require_active,
    error::{Error, Result, required},
    installation_mcp::management_authorization,
    service::Service,
    store::Db,
    validation::text,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncSeekExt};

const CHUNK_BYTES: u64 = 192 * 1024;

fn active_chat(db: &Db<'_>, id: &str) -> Result<Value> {
    let chat = required(db.conversation_boundary(id)?, "Chat not found")?;
    require_active(&chat)?;
    Ok(chat)
}

pub(super) async fn invoke(
    s: &Arc<Service>,
    name: &str,
    args: Value,
    bearer: Option<&str>,
) -> Result<Value> {
    let authorize = management_authorization(s, bearer).await?;
    let chat_id = text(&args, "chatId").to_owned();
    let authorized_chat = chat_id.clone();
    let authorize_chat = authorize.with_guard(move |db: &Db<'_>| {
        active_chat(db, &authorized_chat)?;
        Ok(())
    });
    let artifact = name.contains("artifact");
    let listing = name.starts_with("list_");
    let input = args.clone();
    let check = authorize_chat.clone();
    let data_dir = s.config.data_dir.clone();
    authorize_chat.recheck().await?;
    let (record, attachment_path) = s
        .store
        .read(move |db| {
            let transaction = rusqlite::Transaction::new_unchecked(
                db.0,
                rusqlite::TransactionBehavior::Deferred,
            )?;
            let db = &Db(&transaction);
            check.authorize(db)?;
            let chat = active_chat(db, &chat_id)?;
            let prefix = if artifact {
                if let Some(run) = chat["runId"].as_str() {
                    db.require_run(run)?;
                }
                format!("artifact:{}:", text(&chat, "runId"))
            } else {
                String::new()
            };
            let mut attachment_path = None;
            let result = if listing {
                let mut values: Vec<_> = if artifact {
                    db.keys(&prefix)?
                        .into_iter()
                        .map(|(_, value)| value)
                        .collect()
                } else {
                    db.attachments(&chat_id)?
                        .into_iter()
                        .map(serde_json::to_value)
                        .collect::<std::result::Result<_, _>>()?
                };
                values.sort_by(|a, b| {
                    a["createdAt"]
                        .as_i64()
                        .cmp(&b["createdAt"].as_i64())
                        .then_with(|| text(a, "id").cmp(text(b, "id")))
                });
                page(
                    &values,
                    &input,
                    if artifact { "artifacts" } else { "attachments" },
                )?
            } else {
                let id = text(
                    &input,
                    if artifact {
                        "artifactId"
                    } else {
                        "attachmentId"
                    },
                );
                if artifact {
                    required(
                        db.kv(&format!("{prefix}{id}"))?,
                        "File not found in this chat",
                    )?
                } else {
                    let upload = required(
                        db.uploaded_attachment(&chat_id, id)?,
                        "File not found in this chat",
                    )?;
                    attachment_path = Some(upload.path(&data_dir));
                    serde_json::to_value(upload.metadata)?
                }
            };
            transaction.commit()?;
            Ok((result, attachment_path))
        })
        .await?;
    if listing {
        authorize_chat.recheck().await?;
        return Ok(record);
    }
    if name == "set_chat_artifact_visibility" {
        return crate::artifacts::sharing::set_live(
            s,
            text(&record, "runId"),
            text(&args, "artifactId"),
            text(&args, "visibility"),
            authorize_chat,
        )
        .await;
    }

    let id = text(
        &args,
        if artifact {
            "artifactId"
        } else {
            "attachmentId"
        },
    );
    let path = if artifact {
        s.config.data_dir.join("artifacts").join(id)
    } else {
        attachment_path.ok_or_else(|| Error::internal("Missing attachment file path"))?
    };
    let mut file = tokio::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .await?;
    let metadata = file.metadata().await?;
    if !metadata.is_file() || metadata.len() != record["size"].as_u64().unwrap_or(u64::MAX) {
        return Err(Error::conflict("Stored file size changed."));
    }
    let offset = args["offset"].as_u64().unwrap();
    let size = metadata.len();
    if offset > size {
        return Err(Error::bad(
            "Offset is beyond the file. Use nextOffset from the previous chunk.",
        ));
    }
    file.seek(std::io::SeekFrom::Start(offset)).await?;
    let mut bytes = Vec::new();
    authorize_chat.recheck().await?;
    file.take(CHUNK_BYTES).read_to_end(&mut bytes).await?;
    let next = offset + bytes.len() as u64;
    // A transfer cannot return data after the execution grant or chat was revoked.
    authorize_chat.recheck().await?;
    s.store.read(move |db| authorize_chat.authorize(db)).await?;
    Ok(json!({
        "file": record,
        "encoding": "base64",
        "offset": offset,
        "totalBytes": size,
        "data": STANDARD.encode(bytes),
        "nextOffset": (next < size).then_some(next),
    }))
}
