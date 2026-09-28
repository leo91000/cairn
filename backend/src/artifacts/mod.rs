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
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{path::Path, sync::Arc, time::Duration};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};

pub struct Artifacts {
    transfers: tokio::sync::Semaphore,
    commit: tokio::sync::Mutex<()>,
}
impl Default for Artifacts {
    fn default() -> Self {
        Self {
            transfers: tokio::sync::Semaphore::new(2),
            commit: Default::default(),
        }
    }
}
pub fn tool() -> Value {
    json!({"name":"publish_artifact","description":"Publish a finished file for the user to view and download, including after this VM stops. Use for requested screenshots, videos, audio, documents and other deliverables. Files must be in the current run workspace or /tmp; max 512 MB. Use the same key for revisions of one deliverable. Wait for success before telling the user it is available. Publish each file separately; matching group values form a gallery. Returns a durable private URL and, when visibility is public, a publicUrl readable by anyone with the link. Files are private by default; only set visibility to public when the user requests public sharing. Each new version is private unless explicitly public. Never publish credentials or unrelated private files.","inputSchema":{"type":"object","properties":{"visibility":{"type":"string","enum":["private","public"]},"path":{"type":"string"},"title":{"type":"string","maxLength":160},"key":{"type":"string","maxLength":160},"group":{"type":"string","maxLength":160}},"required":["path","title","key"],"additionalProperties":false}})
}
fn bounded<'a>(value: &'a Value, key: &str, limit: usize) -> Result<&'a str> {
    let s = text(value, key).trim();
    if s.is_empty() || s.len() > limit {
        return Err(Error::bad(format!("Invalid {key}.")));
    }
    Ok(s)
}
impl Artifacts {
    pub async fn publish(&self, s: &Service, bearer: &str, args: &Value) -> Result<Value> {
        if args.get("visibility").is_some() {
            sharing::visibility(args)?;
        }
        let _permit = self.transfers.acquire().await.map_err(Error::internal)?;
        let run = crate::project_workspaces::authorize(s, bearer).await?;
        let path = bounded(args, "path", 4096)?;
        let title = bounded(args, "title", 160)?;
        let key = bounded(args, "key", 160)?;
        if text(args, "group").len() > 160 {
            return Err(Error::bad("Group is too long."));
        }
        let run_id = text(&run, "id");
        let checkpoint = required(
            s.store.kv(&format!("run-checkpoint:{run_id}")).await?,
            "Workspace is not ready",
        )?;
        let attempt = text(&checkpoint, "runnerId");
        uuid(attempt)?;
        let credential = crate::execution::secret(&s.config.data_dir, "runner-secret").await?;
        let response = s
            .http
            .post(format!(
                "{}/runs/{attempt}/artifact",
                crate::nodes::transport::url(s, text(&run, "id")).await?
            ))
            .bearer_auth(credential)
            .json(&json!({"runId":run_id,"path":path}))
            .timeout(Duration::from_secs(300))
            .send()
            .await
            .map_err(|_| {
                Error::new(503, "Artifact transfer was interrupted. Retry publication.")
            })?;
        if !response.status().is_success() {
            return Err(Error::bad(
                "Cannot read artifact. Use a finished file in the run workspace or /tmp, without symlinks.",
            ));
        }
        let expected = response
            .content_length()
            .filter(|n| *n <= file::MAX_FILE)
            .ok_or_else(|| Error::bad("Invalid artifact size."))?;
        let directory = s.config.data_dir.join("artifacts");
        crate::skills::private_dir(&directory).await?;
        let temporary = tempfile::NamedTempFile::new_in(&directory)?;
        let mut output = tokio::fs::File::from_std(temporary.reopen()?);
        let mut stream = response.bytes_stream();
        let mut digest = Sha256::new();
        let mut size = 0u64;
        let mut sample = Vec::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|_| {
                Error::new(503, "Artifact transfer was interrupted. Retry publication.")
            })?;
            size += chunk.len() as u64;
            if size > expected {
                return Err(Error::bad(
                    "Artifact changed during transfer. Finish writing it before publishing.",
                ));
            }
            let remaining = 8192usize.saturating_sub(sample.len());
            sample.extend_from_slice(&chunk[..remaining.min(chunk.len())]);
            digest.update(&chunk);
            output.write_all(&chunk).await?;
        }
        if size != expected {
            return Err(Error::new(
                503,
                "Artifact transfer was incomplete. Retry publication.",
            ));
        }
        output.sync_all().await?;
        drop(output);
        // Recheck cancellation and permissions after the potentially long transfer.
        let current = crate::project_workspaces::authorize(s, bearer).await?;
        let current_checkpoint = s
            .store
            .kv(&format!("run-checkpoint:{run_id}"))
            .await?
            .unwrap_or_default();
        if current_checkpoint["runnerId"] != checkpoint["runnerId"]
            || current["chatExecution"]["messageId"] != run["chatExecution"]["messageId"]
        {
            return Err(Error::new(
                409,
                "The active turn changed. Publish again from the current turn.",
            ));
        }
        let _commit = self.commit.lock().await;
        let existing = list(s, run_id).await?;
        let digest = hex::encode(digest.finalize());
        if let Some(item) = existing.iter().find(|v| {
            v["key"] == key
                && v["digest"] == digest
                && v["title"] == title
                && v["group"] == text(args, "group")
                && v["messageId"] == run["chatExecution"]["messageId"]
        }) {
            return if let Some(value) = args.get("visibility") {
                sharing::set(
                    s,
                    run_id,
                    text(item, "id"),
                    value.as_str().unwrap(),
                    Some(bearer),
                )
                .await
            } else {
                Ok(item.clone())
            };
        }
        if existing.len() >= 500
            || existing
                .iter()
                .map(|v| v["size"].as_u64().unwrap_or(0))
                .sum::<u64>()
                + size
                > 2 * 1024 * 1024 * 1024
        {
            return Err(Error::new(
                413,
                "This conversation has reached its artifact limit (2 GB or 500 versions).",
            ));
        }
        let revision = existing
            .iter()
            .filter(|v| v["key"] == key)
            .map(|v| v["version"].as_u64().unwrap_or(0))
            .max()
            .unwrap_or(0)
            + 1;
        let name = Path::new(path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("download");
        let (kind, media) = file::classify(name, &sample);
        let artifact_id = id();
        let mut artifact = json!({"id":artifact_id,"runId":run_id,"messageId":run["chatExecution"]["messageId"],"key":key,"version":revision,"title":title,"name":name,"group":text(args,"group"),"kind":kind,"mediaType":media,"size":size,"digest":digest,"createdAt":now(),"url":format!("/api/runs/{run_id}/artifacts/{artifact_id}"),"previewStatus":if ["image","video","audio","pdf"].contains(&kind) {"pending"} else {"none"}});
        if ["markdown", "code"].contains(&kind) {
            artifact["excerpt"] = String::from_utf8_lossy(&sample)
                .chars()
                .take(400)
                .collect::<String>()
                .into();
        }
        temporary
            .persist_noclobber(directory.join(&artifact_id))
            .map_err(|e| Error::internal(e.error))?;
        std::fs::File::open(&directory)?.sync_all()?;
        let mut record = artifact.clone();
        let visibility = args["visibility"].as_str().unwrap_or("private").to_owned();
        let origin = s.config.public_url.clone();
        let owned_run = run_id.to_owned();
        let token = bearer.to_owned();
        let expected_attempt = attempt.to_owned();
        artifact = s
            .store
            .transaction(move |db| {
                crate::conversation_lifecycle::require_active_run(db, &owned_run)?;
                let current = crate::project_workspaces::authorize_in(db, &token)?;
                let checkpoint = db
                    .kv(&format!("run-checkpoint:{owned_run}"))?
                    .unwrap_or_default();
                if current["id"] != owned_run
                    || checkpoint["runnerId"] != expected_attempt
                    || current["chatExecution"]["messageId"] != record["messageId"]
                {
                    return Err(Error::new(
                        409,
                        "The active turn changed during publication.",
                    ));
                }
                sharing::apply(db, &mut record, &visibility, &origin)?;
                db.set(
                    &format!("artifact:{owned_run}:{}", text(&record, "id")),
                    &record,
                    None,
                )?;
                db.event(
                    &owned_run,
                    "artifact",
                    text(&record, "title"),
                    Some(&record),
                )?;
                Ok(record)
            })
            .await?;
        let service = s.clone();
        let published = artifact.clone();
        tokio::spawn(async move {
            let _ = super::artifacts::preview::prepare(service, published).await;
        });
        Ok(artifact)
    }
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
        return Err(Error::new(405, "Method not allowed."));
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
    if preview && record["previewStatus"] != "ready" {
        return Err(Error::new(404, "Preview is unavailable."));
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
    let range = match file::range(
        request
            .headers()
            .get(header::RANGE)
            .and_then(|v| v.to_str().ok()),
        size,
    ) {
        Ok(range) => range,
        Err(_) => {
            return Ok((
                StatusCode::RANGE_NOT_SATISFIABLE,
                [(header::CONTENT_RANGE, format!("bytes */{size}"))],
            )
                .into_response());
        }
    };
    let (start, count) = range.map(|(a, b)| (a, b - a + 1)).unwrap_or((0, size));
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
