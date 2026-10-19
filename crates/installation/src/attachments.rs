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
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::io::AsyncReadExt;

pub const MAX_FILE: usize = 10 * 1024 * 1024;
const MAX_MESSAGE: u64 = 40 * 1024 * 1024;
const MAX_CHAT: u64 = 200 * 1024 * 1024;

fn key(chat: &str, id: &str) -> String {
    format!("chat-attachment:{chat}:{id}")
}

pub(crate) fn filename(name: &str) -> String {
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

fn attachments_of(message: &Value) -> &[Value] {
    message["attachments"].as_array().map_or(&[], Vec::as_slice)
}

pub fn same(a: &Value, b: &Value) -> bool {
    attachments_of(a) == attachments_of(b)
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum AttachmentKind {
    Image,
    File,
}

/// An uploaded chat file (`chat-attachment:{chat}:{id}`). Messages embed copies.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Attachment {
    id: String,
    chat_id: String,
    name: String,
    size: u64,
    media_type: String,
    kind: AttachmentKind,
    digest: String,
    created_at: i64,
}

impl Attachment {
    fn content_disposition(&self) -> String {
        let encoded: String = url::form_urlencoded::byte_serialize(self.name.as_bytes()).collect();
        let disposition = match self.kind {
            AttachmentKind::Image => "inline",
            AttachmentKind::File => "attachment",
        };
        format!(
            "{disposition}; filename*=UTF-8''{}",
            encoded.replace('+', "%20")
        )
    }
}

impl Service {
    pub(crate) fn attachment_path(&self, chat: &str, id: &str) -> PathBuf {
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
        match request.method().as_str() {
            "PUT" => self.upload_attachment(chat, id, request).await,
            "GET" => {
                let flags: HashMap<String, String> =
                    serde_urlencoded::from_str(request.uri().query().unwrap_or(""))
                        .map_err(|_| Error::bad("Invalid query."))?;
                if flags.contains_key("preview") {
                    self.attachment_preview(chat, id).await
                } else if flags.contains_key("summary") {
                    self.attachment_summary(chat, id).await
                } else {
                    self.download_attachment(chat, id).await
                }
            }
            _ => Err(Error::method_not_allowed("Method not allowed.")),
        }
    }

    async fn upload_attachment(&self, chat: &str, id: &str, request: Request) -> Result<Response> {
        let query: HashMap<String, String> =
            serde_urlencoded::from_str(request.uri().query().unwrap_or(""))
                .map_err(|_| Error::bad("Invalid file name."))?;
        let name = query
            .get("name")
            .filter(|s| !s.trim().is_empty() && s.len() <= 1000)
            .ok_or_else(|| Error::bad("A file name is required."))?;
        let bytes = tokio::time::timeout(
            Duration::from_secs(30),
            to_bytes(request.into_body(), MAX_FILE),
        )
        .await
        .map_err(|_| Error::timeout("Upload timed out."))?
        .map_err(|_| Error::too_large("Files must be 10 MB or smaller."))?;
        let attachment = self.store_attachment(chat, id, name, &bytes).await?;
        Ok(Json(attachment).into_response())
    }

    /// Stores one file for a chat and returns its record, for HTTP uploads and for
    /// files plugins receive (Discord attachments). Serializes the quota check and
    /// write for every caller; the name is cleaned here.
    pub async fn store_attachment(
        &self,
        chat: &str,
        id: &str,
        name: &str,
        bytes: &[u8],
    ) -> Result<Value> {
        self.store_attachment_checked(chat, id, name, bytes, || async { Ok(()) }, |_| Ok(()))
            .await
    }

    pub(crate) async fn store_attachment_authorized<F>(
        &self,
        chat: &str,
        id: &str,
        name: &str,
        bytes: &[u8],
        authorize: F,
    ) -> Result<Value>
    where
        F: Fn(&Db<'_>) -> Result<()> + Send + Sync + Clone + 'static,
    {
        self.store_attachment_checked(chat, id, name, bytes, || async { Ok(()) }, authorize)
            .await
    }

    /// Stage bytes privately, then fence the caller in the same transaction which
    /// publishes their metadata. The shared quota lock covers both entry points.
    pub(crate) async fn store_attachment_checked<F, Fut, G>(
        &self,
        chat: &str,
        id: &str,
        name: &str,
        bytes: &[u8],
        check: F,
        check_in: G,
    ) -> Result<Value>
    where
        F: Fn() -> Fut,
        Fut: std::future::Future<Output = Result<()>>,
        G: Fn(&Db<'_>) -> Result<()> + Clone + Send + 'static,
    {
        uuid(chat)?;
        uuid(id)?;
        let _guard = self.attachment_upload.lock().await;
        check().await?;
        if bytes.len() > MAX_FILE {
            return Err(Error::too_large("Files must be 10 MB or smaller."));
        }
        let name = filename(name);
        let digest = hex::encode(Sha256::digest(bytes));
        let k = key(chat, id);
        let lookup = k.clone();
        let lookup_check = check_in.clone();
        let existing = self
            .store
            .read(move |db| {
                let transaction = rusqlite::Transaction::new_unchecked(
                    db.0,
                    rusqlite::TransactionBehavior::Deferred,
                )?;
                let db = &Db(&transaction);
                lookup_check(db)?;
                let existing = db.kv(&lookup)?;
                transaction.commit()?;
                Ok(existing)
            })
            .await?;
        if let Some(existing) = existing {
            if existing["digest"] != digest || existing["name"] != name {
                return Err(Error::conflict(
                    "This attachment identifier has already been used.",
                ));
            }
            return Ok(existing);
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
        if used + bytes.len() as u64 > MAX_CHAT {
            return Err(Error::too_large(
                "This chat has reached its 200 MB attachment limit. Start a new chat.",
            ));
        }
        let media_type = media(bytes);
        let kind = if media_type.starts_with("image/") {
            AttachmentKind::Image
        } else {
            AttachmentKind::File
        };
        let attachment = serde_json::to_value(Attachment {
            id: id.to_owned(),
            chat_id: chat.to_owned(),
            name,
            size: bytes.len() as u64,
            media_type: media_type.to_owned(),
            kind,
            digest,
            created_at: now(),
        })?;
        let path = self.attachment_path(chat, id);
        private_dir(path.parent().unwrap()).await?;
        atomic_write(&path, bytes).await?;
        let chat_id = chat.to_owned();
        let saved = attachment.clone();
        let commit = async {
            check().await?;
            self.store
                .transaction(move |db| {
                    check_in(db)?;
                    let chat = required(db.get("chats", &chat_id)?, "Chat not found")?;
                    crate::conversation_lifecycle::require_active(&chat)?;
                    db.set(&k, &saved, None)
                })
                .await
        }
        .await;
        if let Err(error) = commit {
            if let Err(cleanup) = tokio::fs::remove_file(&path).await {
                tracing::warn!(error = %cleanup, "could not remove an uncommitted attachment");
            }
            return Err(error);
        }
        Ok(attachment)
    }

    async fn download_attachment(&self, chat: &str, id: &str) -> Result<Response> {
        let attachment = required(self.store.kv(&key(chat, id)).await?, "Attachment not found")?;
        let attachment = Attachment::deserialize(&attachment)?;
        let file = tokio::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(self.attachment_path(chat, id))
            .await?;
        let body = Body::from_stream(tokio_util::io::ReaderStream::new(
            file.take(MAX_FILE as u64),
        ));
        let mut response = body.into_response();
        let headers = response.headers_mut();
        headers.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_str(&attachment.media_type).map_err(Error::internal)?,
        );
        headers.insert(
            header::CONTENT_DISPOSITION,
            HeaderValue::from_str(&attachment.content_disposition()).map_err(Error::internal)?,
        );
        headers.insert(
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_static("default-src 'none'; sandbox"),
        );
        Ok(response)
    }

    async fn stored_attachment(&self, chat: &str, id: &str) -> Result<(Attachment, Vec<u8>)> {
        let attachment = required(self.store.kv(&key(chat, id)).await?, "Attachment not found")?;
        let attachment = Attachment::deserialize(&attachment)?;
        let mut bytes = Vec::new();
        tokio::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(self.attachment_path(chat, id))
            .await?
            .take(MAX_FILE as u64)
            .read_to_end(&mut bytes)
            .await?;
        Ok((attachment, bytes))
    }

    /// A JPEG of an image or of the first PDF page, rendered on demand and never stored.
    async fn attachment_preview(&self, chat: &str, id: &str) -> Result<Response> {
        let (attachment, bytes) = self.stored_attachment(chat, id).await?;
        let pdf = bytes.starts_with(b"%PDF-");
        if attachment.kind != AttachmentKind::Image && !pdf {
            return Err(Error::not_found("Preview is unavailable."));
        }

        let _permit = crate::artifacts::preview::JOBS
            .acquire()
            .await
            .map_err(Error::internal)?;
        let directory = tempfile::tempdir()?;
        let target = directory.path().join("preview.jpg");
        let source = self.attachment_path(chat, id);
        let render = async {
            if pdf {
                crate::artifacts::preview::thumbnail(true, &source, &target).await
            } else {
                crate::artifacts::preview::image_thumbnail(&source, &target).await
            }
        };
        let rendered = matches!(
            tokio::time::timeout(Duration::from_secs(25), render).await,
            Ok(Ok(true))
        );
        if !rendered {
            return Err(Error::not_found("Preview is unavailable."));
        }

        let image = tokio::fs::read(&target).await?;
        Ok(([(header::CONTENT_TYPE, "image/jpeg")], image).into_response())
    }

    /// Page count of a PDF, or line count and opening excerpt of a UTF-8 text file.
    async fn attachment_summary(&self, chat: &str, id: &str) -> Result<Response> {
        let (_, bytes) = self.stored_attachment(chat, id).await?;
        let summary = if bytes.starts_with(b"%PDF-") {
            let _permit = crate::artifacts::preview::JOBS
                .acquire()
                .await
                .map_err(Error::internal)?;
            let source = self.attachment_path(chat, id);
            let pages = tokio::time::timeout(
                Duration::from_secs(10),
                crate::artifacts::preview::pdf_pages(&source),
            )
            .await
            .ok()
            .and_then(Result::ok)
            .flatten();
            match pages {
                Some(pages) => serde_json::json!({ "pages": pages }),
                None => serde_json::json!({}),
            }
        } else {
            text_summary(&bytes)
        };
        Ok(Json(summary).into_response())
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

const SUMMARY_SAMPLE: usize = 64 * 1024;
const EXCERPT_CHARS: usize = 600;

fn text_summary(bytes: &[u8]) -> Value {
    let sample = &bytes[..bytes.len().min(SUMMARY_SAMPLE)];
    if sample.contains(&0) {
        return serde_json::json!({});
    }

    // A sample may end in the middle of a character; anything else is not UTF-8 text.
    let readable = match std::str::from_utf8(sample) {
        Ok(text) => text,
        Err(error) if error.error_len().is_none() => {
            std::str::from_utf8(&sample[..error.valid_up_to()]).unwrap_or_default()
        }
        Err(_) => return serde_json::json!({}),
    };

    let newlines = bytes.iter().filter(|&&byte| byte == b'\n').count();
    let unterminated = !bytes.is_empty() && !bytes.ends_with(b"\n");
    let excerpt: String = readable
        .replace("\r\n", "\n")
        .chars()
        .take(EXCERPT_CHARS)
        .collect();
    serde_json::json!({
        "lines": newlines + usize::from(unterminated),
        "excerpt": excerpt,
    })
}

/// A Codex user input item.
#[derive(Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
enum InputItem {
    Text {
        text: String,
        text_elements: [(); 0],
    },
    LocalImage {
        path: PathBuf,
    },
}

impl InputItem {
    fn text(text: String) -> Self {
        Self::Text {
            text,
            text_elements: [],
        }
    }
}

pub fn input(message: &str, attachments: &Value, directory: &Path) -> Value {
    let mut content = vec![InputItem::text(message.to_owned())];
    for attachment in attachments.as_array().into_iter().flatten() {
        let name = text(attachment, "name");
        let path = directory
            .join("attachments")
            .join(text(attachment, "id"))
            .join(filename(name));
        let description = format!("Attached file: {name}\nLocal path: {}", path.display());
        content.push(InputItem::text(description));
        if attachment["kind"] == "image" {
            content.push(InputItem::LocalImage { path });
        }
    }
    serde_json::to_value(content).expect("attachment paths are UTF-8")
}

#[cfg(test)]
mod tests {
    use super::{filename, text_summary};
    use serde_json::json;

    #[test]
    fn text_summaries_count_lines_and_skip_binary_content() {
        assert_eq!(
            text_summary(b"id,name\r\n1,Acme\r\n2,Globex"),
            json!({ "lines": 3, "excerpt": "id,name\n1,Acme\n2,Globex" })
        );
        assert_eq!(text_summary(b"one\ntwo\n")["lines"], 2);
        assert_eq!(text_summary(b"")["lines"], 0);
        assert_eq!(text_summary(b"PK\x03\x04\0\0binary"), json!({}));
        assert_eq!(text_summary(b"\xff\xfe\xfdlatin"), json!({}));

        let long = "€".repeat(30_000);
        let summary = text_summary(long.as_bytes());
        assert_eq!(summary["lines"], 1);
        assert_eq!(summary["excerpt"].as_str().unwrap().chars().count(), 600);
    }

    #[test]
    fn file_names_preserve_dotfiles_and_bound_utf8_bytes_without_path_components() {
        assert_eq!(filename(".env"), ".env");
        assert_eq!(filename(".."), "attachment");
        assert!(!filename("../../private\r\nfile").contains('/'));
        assert!(filename(&"日本語".repeat(100)).len() <= 180);
    }
}
