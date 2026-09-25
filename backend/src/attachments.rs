use crate::{
    config::now,
    error::{Error, Result, required},
    service::Service,
    skills::{atomic_write, private_dir},
    store::Db,
    validation::{text, uuid},
};
use axum::{
    Json,
    body::{Body, to_bytes},
    extract::Request,
    http::{HeaderValue, header},
    response::{IntoResponse, Response},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::io::AsyncReadExt;

pub const MAX_FILE: usize = 10 * 1024 * 1024;
const MAX_MESSAGE: u64 = 40 * 1024 * 1024;

fn key(chat: &str, id: &str) -> String {
    format!("chat-attachment:{chat}:{id}")
}

fn filename(name: &str) -> String {
    let mut clean = String::new();
    for c in name.chars() {
        let c = if c.is_alphanumeric() || " ._-()".contains(c) {
            c
        } else {
            '_'
        };
        if clean.len() + c.len_utf8() > 180 {
            break;
        }
        clean.push(c);
    }
    let name = clean.trim();
    if name.is_empty() || name == "." || name == ".." {
        "attachment".into()
    } else {
        name.into()
    }
}

fn media(bytes: &[u8]) -> &'static str {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return "image/png";
    }
    if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        return "image/jpeg";
    }
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        return "image/gif";
    }
    if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
        return "image/webp";
    }
    "application/octet-stream"
}

pub fn message(db: &Db<'_>, chat: &str, value: &mut Value) -> Result<()> {
    let ids = value["attachmentIds"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    if ids.len() > 8 {
        return Err(Error::bad("Attach up to 8 files per message."));
    }
    let mut seen = HashSet::new();
    let mut total = 0;
    let mut attachments = Vec::new();
    for id in ids {
        let id = id
            .as_str()
            .ok_or_else(|| Error::bad("Invalid attachment identifier."))?;
        uuid(id)?;
        if !seen.insert(id.to_owned()) {
            return Err(Error::bad("Duplicate attachment."));
        }
        let attachment = required(db.kv(&key(chat, id))?, "Attachment not found in this chat")?;
        total += attachment["size"].as_u64().unwrap_or(MAX_MESSAGE + 1);
        attachments.push(attachment);
    }
    if total > MAX_MESSAGE {
        return Err(Error::bad(
            "Attachments must total at most 40 MB per message.",
        ));
    }
    if text(value, "text").trim().is_empty() && attachments.is_empty() {
        return Err(Error::bad("Write a message or attach a file."));
    }
    value["attachments"] = attachments.into();
    value.as_object_mut().unwrap().remove("attachmentIds");
    Ok(())
}

pub fn same(a: &Value, b: &Value) -> bool {
    a["attachments"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or(&[])
        == b["attachments"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or(&[])
}

impl Service {
    fn attachment_path(&self, chat: &str, id: &str) -> PathBuf {
        self.config
            .data_dir
            .join("chat-attachments")
            .join(chat)
            .join(id)
    }

    pub async fn attachment_http(
        &self,
        chat: &str,
        id: &str,
        request: Request,
    ) -> Result<Response> {
        uuid(chat)?;
        uuid(id)?;
        crate::conversation_lifecycle::require_active(&self.get("chats", chat).await?)?;
        let k = key(chat, id);
        match request.method().as_str() {
            "PUT" => {
                let _guard = self.attachment_upload.lock().await;
                let query: HashMap<String, String> =
                    serde_urlencoded::from_str(request.uri().query().unwrap_or(""))
                        .map_err(|_| Error::bad("Invalid file name."))?;
                let name = query
                    .get("name")
                    .filter(|s| !s.trim().is_empty() && s.len() <= 1000)
                    .ok_or_else(|| Error::bad("A file name is required."))?;
                let name = filename(name);
                let bytes = tokio::time::timeout(
                    Duration::from_secs(30),
                    to_bytes(request.into_body(), MAX_FILE),
                )
                .await
                .map_err(|_| Error::new(408, "Upload timed out."))?
                .map_err(|_| Error::new(413, "Files must be 10 MB or smaller."))?;
                let digest = hex::encode(Sha256::digest(&bytes));
                if let Some(existing) = self.store.kv(&k).await? {
                    if existing["digest"] != digest || existing["name"] != name {
                        return Err(Error::new(
                            409,
                            "This attachment identifier has already been used.",
                        ));
                    }
                    return Ok(Json(existing).into_response());
                }
                let prefix = format!("chat-attachment:{chat}:");
                let used = self
                    .store
                    .read(move |db| {
                        Ok(db
                            .keys(&prefix)?
                            .iter()
                            .map(|(_, v)| v["size"].as_u64().unwrap_or(0))
                            .sum::<u64>())
                    })
                    .await?;
                if used + bytes.len() as u64 > 200 * 1024 * 1024 {
                    return Err(Error::new(
                        413,
                        "This chat has reached its 200 MB attachment limit. Start a new chat.",
                    ));
                }
                let media_type = media(&bytes);
                let attachment = json!({
                    "id": id,
                    "chatId": chat,
                    "name": name,
                    "size": bytes.len(),
                    "mediaType": media_type,
                    "kind": if media_type.starts_with("image/") {
                        "image"
                    } else {
                        "file"
                    },
                    "digest": digest,
                    "createdAt": now(),
                });
                let path = self.attachment_path(chat, id);
                private_dir(path.parent().unwrap()).await?;
                atomic_write(&path, &bytes).await?;
                let chat_id = chat.to_owned();
                let saved = attachment.clone();
                let commit = self
                    .store
                    .transaction(move |db| {
                        let chat = required(db.get("chats", &chat_id)?, "Chat not found")?;
                        crate::conversation_lifecycle::require_active(&chat)?;
                        db.set(&k, &saved, None)
                    })
                    .await;
                if let Err(error) = commit {
                    let _ = tokio::fs::remove_file(&path).await;
                    return Err(error);
                }
                Ok(Json(attachment).into_response())
            }
            "GET" => {
                let attachment = required(self.store.kv(&k).await?, "Attachment not found")?;
                let file = tokio::fs::OpenOptions::new()
                    .read(true)
                    .custom_flags(libc::O_NOFOLLOW)
                    .open(self.attachment_path(chat, id))
                    .await?;
                let mut response = Body::from_stream(tokio_util::io::ReaderStream::new(
                    file.take(MAX_FILE as u64),
                ))
                .into_response();
                response.headers_mut().insert(
                    header::CONTENT_TYPE,
                    HeaderValue::from_str(text(&attachment, "mediaType"))
                        .map_err(Error::internal)?,
                );
                let encoded: String =
                    url::form_urlencoded::byte_serialize(text(&attachment, "name").as_bytes())
                        .collect();
                let disposition = if attachment["kind"] == "image" {
                    "inline"
                } else {
                    "attachment"
                };
                response.headers_mut().insert(
                    header::CONTENT_DISPOSITION,
                    HeaderValue::from_str(&format!(
                        "{disposition}; filename*=UTF-8''{}",
                        encoded.replace('+', "%20")
                    ))
                    .map_err(Error::internal)?,
                );
                response.headers_mut().insert(
                    header::CONTENT_SECURITY_POLICY,
                    HeaderValue::from_static("default-src 'none'; sandbox"),
                );
                Ok(response)
            }
            _ => Err(Error::new(405, "Method not allowed.")),
        }
    }

    pub async fn prepare_chat_files(&self, run_id: &str, attachments: &Value) -> Result<()> {
        uuid(run_id)?;
        for attachment in attachments.as_array().into_iter().flatten() {
            let chat = text(attachment, "chatId");
            let id = text(attachment, "id");
            uuid(chat)?;
            uuid(id)?;
            let stored = required(
                self.store.kv(&key(chat, id)).await?,
                "Attachment no longer available",
            )?;
            if stored != *attachment {
                return Err(Error::bad("Attachment metadata changed."));
            }
            let target = self
                .config
                .data_dir
                .join("runs")
                .join(run_id)
                .join("chat-input/attachments")
                .join(id)
                .join(filename(text(attachment, "name")));
            // Always restore a private copy from the immutable upload on restart.
            let source = tokio::fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW)
                .open(self.attachment_path(chat, id))
                .await?;
            let mut bytes = Vec::new();
            source
                .take(MAX_FILE as u64 + 1)
                .read_to_end(&mut bytes)
                .await?;
            if bytes.len() > MAX_FILE
                || hex::encode(Sha256::digest(&bytes)) != text(attachment, "digest")
            {
                return Err(Error::bad("Attachment integrity check failed."));
            }
            private_dir(target.parent().unwrap()).await?;
            atomic_write(&target, &bytes).await?;
        }
        Ok(())
    }
}

pub fn input(message: &str, attachments: &Value, directory: &Path) -> Value {
    let mut content = vec![json!({
        "type": "text",
        "text": message,
        "text_elements": [],
    })];
    for attachment in attachments.as_array().into_iter().flatten() {
        let path = directory
            .join("attachments")
            .join(text(attachment, "id"))
            .join(filename(text(attachment, "name")));
        content.push(json!({
            "type": "text",
            "text": format!(
                "Attached file: {}\nLocal path: {}",
                text(attachment, "name"),
                path.display()
            ),
            "text_elements": [],
        }));
        if attachment["kind"] == "image" {
            content.push(json!({
                "type": "localImage",
                "path": path,
            }));
        }
    }
    content.into()
}

#[cfg(test)]
mod tests {
    use super::filename;

    #[test]
    fn file_names_preserve_dotfiles_and_bound_utf8_bytes_without_path_components() {
        assert_eq!(filename(".env"), ".env");
        assert_eq!(filename(".."), "attachment");
        assert!(!filename("../../private\r\nfile").contains('/'));
        assert!(filename(&"日本語".repeat(100)).len() <= 180);
    }
}
