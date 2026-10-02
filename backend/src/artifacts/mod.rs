//! Durable run deliverables. Publishing succeeds only after bytes and metadata are committed.
pub mod file;
pub mod sharing;

use crate::{
    config::{id, now},
    error::{Error, Result, required},
    service::Service,
    validation::{text, uuid},
};
use axum::{
    Json,
    body::Body,
    extract::Request,
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use futures_util::StreamExt;
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{path::Path, sync::Arc, time::Duration};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};

/// Per-conversation limits across all published versions.
const MAX_VERSIONS: usize = 500;
const MAX_TOTAL_BYTES: u64 = 2 * 1024 * 1024 * 1024;
/// Leading bytes kept to classify the file and excerpt text.
const SAMPLE_BYTES: usize = 8192;
const TRANSFER_INTERRUPTED: &str = "Artifact transfer was interrupted. Retry publication.";

pub struct Artifacts {
    transfers: tokio::sync::Semaphore,
    commit: tokio::sync::Mutex<()>,
}

impl Default for Artifacts {
    fn default() -> Self {
        Self {
            transfers: tokio::sync::Semaphore::new(2),
            commit: tokio::sync::Mutex::default(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PreviewStatus {
    Pending,
    Ready,
    None,
    Unavailable,
}

impl PreviewStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Ready => "ready",
            Self::None => "none",
            Self::Unavailable => "unavailable",
        }
    }
}

/// A newly published version (`artifact:{run}:{id}`), before its visibility is applied.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct NewArtifact<'a> {
    id: &'a str,
    run_id: &'a str,
    message_id: &'a Value,
    key: &'a str,
    version: u64,
    title: &'a str,
    name: &'a str,
    group: &'a str,
    kind: &'static str,
    media_type: &'static str,
    size: u64,
    digest: &'a str,
    created_at: i64,
    url: String,
    preview_status: PreviewStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    excerpt: Option<String>,
}

pub fn tool() -> Value {
    json!({
        "name": "publish_artifact",
        "description": "Publish a finished file for the user to view and download, including after this VM stops. Use for requested screenshots, videos, audio, documents and other deliverables. Files must be in the current run workspace or /tmp; max 512 MB. Use the same key for revisions of one deliverable. Wait for success before telling the user it is available. Publish each file separately; matching group values form a gallery. Returns a durable private URL and, when visibility is public, a publicUrl readable by anyone with the link. Files are private by default; only set visibility to public when the user requests public sharing. Each new version is private unless explicitly public. Never publish credentials or unrelated private files.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "visibility": { "type": "string", "enum": ["private", "public"] },
                "path": { "type": "string" },
                "title": { "type": "string", "maxLength": 160 },
                "key": { "type": "string", "maxLength": 160 },
                "group": { "type": "string", "maxLength": 160 },
            },
            "required": ["path", "title", "key"],
            "additionalProperties": false,
        },
    })
}

fn bounded<'a>(value: &'a Value, key: &str, limit: usize) -> Result<&'a str> {
    let s = text(value, key).trim();
    if s.is_empty() || s.len() > limit {
        return Err(Error::bad(format!("Invalid {key}.")));
    }
    Ok(s)
}

/// Validated `publish_artifact` arguments.
struct Publication<'a> {
    path: &'a str,
    title: &'a str,
    key: &'a str,
    group: &'a str,
    visibility: Option<&'a str>,
}

impl<'a> Publication<'a> {
    fn parse(args: &'a Value, visibility: Option<&'a str>) -> Result<Self> {
        let publication = Self {
            path: bounded(args, "path", 4096)?,
            title: bounded(args, "title", 160)?,
            key: bounded(args, "key", 160)?,
            group: text(args, "group"),
            visibility,
        };
        if publication.group.len() > 160 {
            return Err(Error::bad("Group is too long."));
        }
        Ok(publication)
    }

    /// A retry of an already committed publication from the same turn.
    fn repeats(&self, existing: &Value, digest: &str, message_id: &Value) -> bool {
        existing["key"] == self.key
            && existing["digest"] == digest
            && existing["title"] == self.title
            && existing["group"] == self.group
            && existing["messageId"] == *message_id
    }
}

/// A file received from the VM into a private temporary file.
struct Download {
    file: tempfile::NamedTempFile,
    size: u64,
    digest: String,
    sample: Vec<u8>,
}

async fn receive(response: reqwest::Response, directory: &Path) -> Result<Download> {
    let expected = response
        .content_length()
        .filter(|n| *n <= file::MAX_FILE)
        .ok_or_else(|| Error::bad("Invalid artifact size."))?;
    crate::skills::private_dir(directory).await?;
    let temporary = tempfile::NamedTempFile::new_in(directory)?;
    let mut output = tokio::fs::File::from_std(temporary.reopen()?);
    let mut stream = response.bytes_stream();
    let mut digest = Sha256::new();
    let mut size = 0u64;
    let mut sample = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| Error::unavailable(TRANSFER_INTERRUPTED))?;
        size += chunk.len() as u64;
        if size > expected {
            return Err(Error::bad(
                "Artifact changed during transfer. Finish writing it before publishing.",
            ));
        }
        let remaining = SAMPLE_BYTES.saturating_sub(sample.len());
        sample.extend_from_slice(&chunk[..remaining.min(chunk.len())]);
        digest.update(&chunk);
        output.write_all(&chunk).await?;
    }
    if size != expected {
        return Err(Error::unavailable(
            "Artifact transfer was incomplete. Retry publication.",
        ));
    }
    output.sync_all().await?;
    Ok(Download {
        file: temporary,
        size,
        digest: hex::encode(digest.finalize()),
        sample,
    })
}

fn over_limit(existing: &[Value], size: u64) -> bool {
    let stored: u64 = existing
        .iter()
        .map(|v| v["size"].as_u64().unwrap_or(0))
        .sum();
    existing.len() >= MAX_VERSIONS || stored + size > MAX_TOTAL_BYTES
}

fn next_version(existing: &[Value], key: &str) -> u64 {
    existing
        .iter()
        .filter(|v| v["key"] == key)
        .map(|v| v["version"].as_u64().unwrap_or(0))
        .max()
        .unwrap_or(0)
        + 1
}

fn preview_status(kind: &str) -> PreviewStatus {
    if ["image", "video", "audio", "pdf"].contains(&kind) {
        PreviewStatus::Pending
    } else {
        PreviewStatus::None
    }
}

fn excerpt(kind: &str, sample: &[u8]) -> Option<String> {
    ["markdown", "code"]
        .contains(&kind)
        .then(|| String::from_utf8_lossy(sample).chars().take(400).collect())
}

impl Artifacts {
    pub async fn publish(&self, s: &Service, bearer: &str, args: &Value) -> Result<Value> {
        let visibility = match args.get("visibility") {
            Some(_) => Some(sharing::visibility(args)?),
            None => None,
        };
        let _permit = self.transfers.acquire().await.map_err(Error::internal)?;
        let run = crate::project_workspaces::authorize(s, bearer).await?;
        let publication = Publication::parse(args, visibility)?;
        let run_id = text(&run, "id");
        let checkpoint = required(
            s.store.kv(&format!("run-checkpoint:{run_id}")).await?,
            "Workspace is not ready",
        )?;
        let attempt = text(&checkpoint, "runnerId");
        uuid(attempt)?;
        let response = export(s, run_id, attempt, publication.path).await?;
        let directory = s.config.data_dir.join("artifacts");
        let download = receive(response, &directory).await?;
        // Recheck cancellation and permissions after the potentially long transfer.
        let current = crate::project_workspaces::authorize(s, bearer).await?;
        let current_checkpoint = s
            .store
            .kv(&format!("run-checkpoint:{run_id}"))
            .await?
            .unwrap_or_default();
        let message_id = &run["chatExecution"]["messageId"];
        if current_checkpoint["runnerId"] != checkpoint["runnerId"]
            || current["chatExecution"]["messageId"] != *message_id
        {
            return Err(Error::conflict(
                "The active turn changed. Publish again from the current turn.",
            ));
        }
        let _commit = self.commit.lock().await;
        let existing = list(s, run_id).await?;
        let repeated = existing
            .iter()
            .find(|v| publication.repeats(v, &download.digest, message_id));
        if let Some(item) = repeated {
            return match publication.visibility {
                Some(value) => sharing::set(s, run_id, text(item, "id"), value, Some(bearer)).await,
                None => Ok(item.clone()),
            };
        }
        if over_limit(&existing, download.size) {
            return Err(Error::too_large(
                "This conversation has reached its artifact limit (2 GB or 500 versions).",
            ));
        }
        let name = Path::new(publication.path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("download");
        let (kind, media_type) = file::classify(name, &download.sample);
        let artifact_id = id();
        let artifact = NewArtifact {
            id: &artifact_id,
            run_id,
            message_id,
            key: publication.key,
            version: next_version(&existing, publication.key),
            title: publication.title,
            name,
            group: publication.group,
            kind,
            media_type,
            size: download.size,
            digest: &download.digest,
            created_at: now(),
            url: format!("/api/runs/{run_id}/artifacts/{artifact_id}"),
            preview_status: preview_status(kind),
            excerpt: excerpt(kind, &download.sample),
        };
        let record = serde_json::to_value(artifact)?;
        download
            .file
            .persist_noclobber(directory.join(&artifact_id))
            .map_err(|e| Error::internal(e.error))?;
        std::fs::File::open(&directory)?.sync_all()?;
        let visibility = publication.visibility.unwrap_or("private");
        let artifact = match commit(s, bearer, attempt, record, visibility).await {
            Ok(artifact) => artifact,
            Err(error) => {
                // A revocation can win after the file's fsync but before the
                // metadata transaction. Only remove bytes once the database
                // confirms they were not committed. Cancellation/uncertainty
                // still leave reconciliation to recover(), not a Drop guard.
                let key = format!("artifact:{run_id}:{artifact_id}");
                if matches!(s.store.kv(&key).await, Ok(None)) {
                    if let Err(cleanup) = tokio::fs::remove_file(directory.join(&artifact_id)).await
                    {
                        tracing::warn!(%cleanup, "Could not remove rejected artifact");
                    } else {
                        std::fs::File::open(&directory)?.sync_all()?;
                    }
                }
                return Err(error);
            }
        };
        let service = s.clone();
        let published = artifact.clone();
        tokio::spawn(async move {
            if let Err(error) = preview::prepare(service, published).await {
                tracing::warn!(error = %error, "artifact preview failed");
            }
        });
        Ok(artifact)
    }
}

/// Streams a finished file out of the run's VM.
async fn export(s: &Service, run_id: &str, attempt: &str, path: &str) -> Result<reqwest::Response> {
    let credential = crate::execution::secret(&s.config.data_dir, "runner-secret").await?;
    let node = crate::nodes::transport::url(s, run_id).await?;
    let response = s
        .http
        .post(format!("{node}/runs/{attempt}/artifact"))
        .bearer_auth(credential)
        .json(&json!({
            "runId": run_id,
            "path": path
        }))
        .timeout(Duration::from_secs(300))
        .send()
        .await
        .map_err(|_| Error::unavailable(TRANSFER_INTERRUPTED))?;
    if !response.status().is_success() {
        return Err(Error::bad(
            "Cannot read artifact. Use a finished file in the run workspace or /tmp, without symlinks.",
        ));
    }
    Ok(response)
}

/// Commits the record if the publishing turn is still the active one.
async fn commit(
    s: &Service,
    bearer: &str,
    attempt: &str,
    mut record: Value,
    visibility: &str,
) -> Result<Value> {
    let visibility = visibility.to_owned();
    let origin = s.config.public_url.clone();
    let token = bearer.to_owned();
    let expected_attempt = attempt.to_owned();
    s.store
        .transaction(move |db| {
            let run = text(&record, "runId").to_owned();
            crate::conversation_lifecycle::require_active_run(db, &run)?;
            let current = crate::project_workspaces::authorize_in(db, &token)?;
            let checkpoint = db.kv(&format!("run-checkpoint:{run}"))?.unwrap_or_default();
            if current["id"] != run
                || checkpoint["runnerId"] != expected_attempt
                || current["chatExecution"]["messageId"] != record["messageId"]
            {
                return Err(Error::conflict(
                    "The active turn changed during publication.",
                ));
            }
            sharing::apply(db, &mut record, &visibility, &origin)?;
            db.set(
                &format!("artifact:{run}:{}", text(&record, "id")),
                &record,
                None,
            )?;
            db.event(&run, "artifact", text(&record, "title"), Some(&record))?;
            Ok(record)
        })
        .await
}

pub(crate) mod preview;

pub async fn list(s: &Service, run: &str) -> Result<Vec<Value>> {
    uuid(run)?;
    s.store.run(run).await?;
    let mut items: Vec<_> = s
        .store
        .keys(&format!("artifact:{run}:"))
        .await?
        .into_iter()
        .map(|(_, v)| v)
        .collect();
    items.sort_by_key(|v| v["createdAt"].as_i64().unwrap_or(0));
    Ok(items)
}

pub async fn http(
    s: &Service,
    run: &str,
    artifact: Option<&str>,
    request: Request,
) -> Result<Response> {
    uuid(run)?;
    if !["GET", "HEAD"].contains(&request.method().as_str()) {
        return Err(Error::method_not_allowed("Method not allowed."));
    }
    let Some(artifact) = artifact else {
        return Ok(Json(list(s, run).await?).into_response());
    };
    uuid(artifact)?;
    s.store.run(run).await?;
    let record = required(
        s.store.kv(&format!("artifact:{run}:{artifact}")).await?,
        "Artifact not found",
    )?;
    if request
        .uri()
        .query()
        .is_some_and(|q| q.split('&').any(|p| p == "metadata=1"))
    {
        return Ok(Json(record).into_response());
    }
    serve(s, &record, request).await
}

async fn serve(s: &Service, record: &Value, request: Request) -> Result<Response> {
    let artifact = text(record, "id");
    let query: std::collections::HashMap<String, String> =
        serde_urlencoded::from_str(request.uri().query().unwrap_or("")).map_err(Error::internal)?;
    let preview = query.contains_key("preview");
    if preview && record["previewStatus"] != PreviewStatus::Ready.as_str() {
        return Err(Error::not_found("Preview is unavailable."));
    }
    let path = s.config.data_dir.join("artifacts").join(if preview {
        format!("{artifact}.jpg")
    } else {
        artifact.to_owned()
    });
    let mut file = tokio::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .await?;
    let size = file.metadata().await?.len();
    let requested = request
        .headers()
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok());
    let Ok(range) = file::range(requested, size) else {
        return Ok((
            StatusCode::RANGE_NOT_SATISFIABLE,
            [(header::CONTENT_RANGE, format!("bytes */{size}"))],
        )
            .into_response());
    };
    let (start, count) = range.map_or((0, size), |(a, b)| (a, b - a + 1));
    file.seek(std::io::SeekFrom::Start(start)).await?;
    let body = if request.method() == "HEAD" {
        Body::empty()
    } else {
        Body::from_stream(tokio_util::io::ReaderStream::new(file.take(count)))
    };
    let mut response = body.into_response();
    if let Some((a, b)) = range {
        *response.status_mut() = StatusCode::PARTIAL_CONTENT;
        response.headers_mut().insert(
            header::CONTENT_RANGE,
            HeaderValue::from_str(&format!("bytes {a}-{b}/{size}")).map_err(Error::internal)?,
        );
    }
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_LENGTH, HeaderValue::from(count));
    headers.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(if preview {
            "image/jpeg"
        } else {
            text(record, "mediaType")
        })
        .map_err(Error::internal)?,
    );
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-cache"),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("default-src 'none'; sandbox"),
    );
    let name: String =
        url::form_urlencoded::byte_serialize(text(record, "name").as_bytes()).collect();
    let disposition = if query.contains_key("download") || record["kind"] == "file" {
        "attachment"
    } else {
        "inline"
    };
    headers.insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_str(&format!(
            "{disposition}; filename*=UTF-8''{}",
            name.replace('+', "%20")
        ))
        .map_err(Error::internal)?,
    );
    Ok(response)
}
