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
use cairn_contracts::ChatAttachment;
use serde::Serialize;
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
pub(crate) const MAX_CHAT: u64 = 200 * 1024 * 1024;

pub(crate) mod migration;
pub(crate) mod sql;
mod uploads;

pub use sql::Reservation;
pub use uploads::Attachments;

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

/// An image's size as it displays, after the camera's orientation, read from its
/// header without decoding the picture. Readers reserve this box before it loads.
fn dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    use image::ImageDecoder;

    if bytes.starts_with(b"GIF8") {
        let width = u16::from_le_bytes(bytes.get(6..8)?.try_into().ok()?);
        let height = u16::from_le_bytes(bytes.get(8..10)?.try_into().ok()?);
        return Some((width.into(), height.into())).filter(|(w, h)| *w > 0 && *h > 0);
    }
    let mut decoder = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .ok()?
        .into_decoder()
        .ok()?;
    let (width, height) = decoder.dimensions();
    let turned = matches!(
        decoder.orientation().ok()?,
        image::metadata::Orientation::Rotate90
            | image::metadata::Orientation::Rotate270
            | image::metadata::Orientation::Rotate90FlipH
            | image::metadata::Orientation::Rotate270FlipH
    );
    let size = if turned {
        (height, width)
    } else {
        (width, height)
    };
    Some(size).filter(|(w, h)| *w > 0 && *h > 0)
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
        let attachment = required(
            db.uploaded_attachment(chat, id)?,
            "Attachment not found in this chat",
        )?
        .metadata;
        total += attachment.size;
        attachments.push(serde_json::to_value(attachment)?);
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

fn content_disposition(attachment: &ChatAttachment) -> String {
    let encoded: String =
        url::form_urlencoded::byte_serialize(attachment.name.as_bytes()).collect();
    let disposition = if attachment.kind == "image" {
        "inline"
    } else {
        "attachment"
    };
    format!(
        "{disposition}; filename*=UTF-8''{}",
        encoded.replace('+', "%20")
    )
}

impl Service {
    async fn uploaded_attachment(&self, chat: &str, id: &str) -> Result<sql::UploadedAttachment> {
        let chat = chat.to_owned();
        let id = id.to_owned();
        required(
            self.store
                .read(move |db| db.uploaded_attachment(&chat, &id))
                .await?,
            "Attachment not found",
        )
    }

    pub(crate) async fn expire_attachment_uploads(&self) -> Result<()> {
        uploads::expire(&self.store, &self.config.data_dir.join("chat-attachments")).await
    }

    pub async fn reconcile_attachments(&self) -> Result<()> {
        uploads::reconcile(&self.store, &self.config.data_dir.join("chat-attachments")).await
    }

    pub async fn attachment_http(
        &self,
        chat: &str,
        id: &str,
        request: Request,
    ) -> Result<Response> {
        uuid(chat)?;
        uuid(id)?;
        crate::conversation_lifecycle::require_active(&self.conversation_boundary(chat).await?)?;
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
    /// files plugins receive. The domain reserves quota before its private I/O.
    pub async fn store_attachment(
        &self,
        chat: &str,
        id: &str,
        name: &str,
        bytes: &[u8],
    ) -> Result<Value> {
        self.store_attachment_authorized(chat, id, name, bytes, |_| Ok(()))
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
        uuid(chat)?;
        uuid(id)?;
        if bytes.len() > MAX_FILE {
            return Err(Error::too_large("Files must be 10 MB or smaller."));
        }
        let media_type = media(bytes);
        let image = media_type.starts_with("image/");
        let dimensions = image.then(|| dimensions(bytes)).flatten();
        let attachment = ChatAttachment {
            id: id.to_owned(),
            chat_id: chat.to_owned(),
            name: filename(name),
            size: bytes.len() as u64,
            media_type: media_type.to_owned(),
            kind: if image { "image" } else { "file" }.into(),
            digest: hex::encode(Sha256::digest(bytes)),
            created_at: now(),
            width: dimensions.map(|(w, _)| w),
            height: dimensions.map(|(_, h)| h),
        };
        Ok(serde_json::to_value(
            self.attachments
                .upload(
                    self.store.clone(),
                    self.config.data_dir.join("chat-attachments"),
                    attachment,
                    bytes.to_vec(),
                    authorize,
                )
                .await?,
        )?)
    }

    async fn download_attachment(&self, chat: &str, id: &str) -> Result<Response> {
        let upload = self.uploaded_attachment(chat, id).await?;
        let source = upload.path(&self.config.data_dir);
        let attachment = upload.metadata;
        let file = tokio::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(source)
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
            HeaderValue::from_str(&content_disposition(&attachment)).map_err(Error::internal)?,
        );
        headers.insert(
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_static("default-src 'none'; sandbox"),
        );
        Ok(response)
    }

    async fn stored_attachment(
        &self,
        chat: &str,
        id: &str,
    ) -> Result<(sql::UploadedAttachment, Vec<u8>)> {
        let attachment = self.uploaded_attachment(chat, id).await?;
        let mut bytes = Vec::new();
        tokio::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(attachment.path(&self.config.data_dir))
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
        if attachment.metadata.kind != "image" && !pdf {
            return Err(Error::not_found("Preview is unavailable."));
        }

        let _permit = crate::artifacts::preview::JOBS
            .acquire()
            .await
            .map_err(Error::internal)?;
        let directory = tempfile::tempdir()?;
        let target = directory.path().join("preview.jpg");
        let source = attachment.path(&self.config.data_dir);
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
        let (attachment, bytes) = self.stored_attachment(chat, id).await?;
        let summary = if bytes.starts_with(b"%PDF-") {
            let _permit = crate::artifacts::preview::JOBS
                .acquire()
                .await
                .map_err(Error::internal)?;
            let source = attachment.path(&self.config.data_dir);
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
            let stored = self.uploaded_attachment(chat, id).await?;
            if serde_json::to_value(&stored.metadata)? != *attachment {
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
                .open(stored.path(&self.config.data_dir))
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
    use super::{dimensions, filename, text_summary};
    use serde_json::json;

    fn encoded(width: u32, height: u32, format: image::ImageFormat) -> Vec<u8> {
        let mut bytes = std::io::Cursor::new(Vec::new());
        image::RgbImage::new(width, height)
            .write_to(&mut bytes, format)
            .unwrap();
        bytes.into_inner()
    }

    /// A JPEG whose EXIF header asks viewers to turn it a quarter clockwise, as phone cameras do.
    fn rotated_jpeg(width: u32, height: u32) -> Vec<u8> {
        let jpeg = encoded(width, height, image::ImageFormat::Jpeg);
        let mut exif = b"Exif\0\0MM\0\x2a\0\0\0\x08\0\x01".to_vec();
        // Orientation (0x0112), SHORT, one value: 6, then no further directory.
        exif.extend_from_slice(b"\x01\x12\0\x03\0\0\0\x01\0\x06\0\0\0\0\0\0");
        let mut bytes = jpeg[..2].to_vec();
        bytes.extend_from_slice(&[0xff, 0xe1]);
        bytes.extend_from_slice(&u16::try_from(exif.len() + 2).unwrap().to_be_bytes());
        bytes.extend_from_slice(&exif);
        bytes.extend_from_slice(&jpeg[2..]);
        bytes
    }

    #[test]
    fn image_dimensions_come_from_headers_as_the_picture_displays() {
        assert_eq!(
            dimensions(&encoded(64, 32, image::ImageFormat::Png)),
            Some((64, 32))
        );
        assert_eq!(
            dimensions(&encoded(48, 30, image::ImageFormat::Jpeg)),
            Some((48, 30))
        );
        assert_eq!(dimensions(&rotated_jpeg(48, 30)), Some((30, 48)));
        assert_eq!(
            dimensions(&encoded(20, 40, image::ImageFormat::WebP)),
            Some((20, 40))
        );
        assert_eq!(
            dimensions(b"GIF89a\x40\x01\xf0\x00\0\0\0"),
            Some((320, 240))
        );
        assert_eq!(dimensions(b"GIF89a\0\0"), None);
        assert_eq!(dimensions(b"\x89PNG\r\n\x1a\ntruncated"), None);
        assert_eq!(dimensions(b"plain text"), None);
    }

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
