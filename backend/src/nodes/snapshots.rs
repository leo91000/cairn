//! Content-addressed disk manifests. Capture callers must provide an immutable disk.
use crate::{
    error::{Error, Result},
    validation::text,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{future::Future, path::Path};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};

pub const MAX_MANIFEST_BYTES: usize = 64 * 1024 * 1024;
pub const BLOCK: u64 = 4 * 1024 * 1024;
pub const READ_BATCH: usize = 8;

/// Next offset at or after `offset` that may hold data. Holes read as zeros, so blocks
/// entirely inside one need neither reading nor hashing.
fn next_data(file: &std::fs::File, offset: u64) -> std::io::Result<u64> {
    use std::os::fd::AsRawFd;
    let position = unsafe { libc::lseek(file.as_raw_fd(), offset as libc::off_t, libc::SEEK_DATA) };
    if position >= 0 {
        return Ok(position as u64);
    }
    let error = std::io::Error::last_os_error();
    match error.raw_os_error() {
        // Only a hole remains after this offset.
        Some(libc::ENXIO) => Ok(u64::MAX),
        // A filesystem without hole reporting: treat everything as data.
        Some(libc::EINVAL) => Ok(offset),
        _ => Err(error),
    }
}

pub async fn index(path: &Path) -> Result<Value> {
    let path = path.to_owned();
    tokio::task::spawn_blocking(move || -> Result<Value> {
        use std::os::unix::fs::{FileExt, OpenOptionsExt};
        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&path)?;
        let size = file.metadata()?.len();
        let mut blocks = Vec::new();
        let mut buffer = vec![0; BLOCK as usize];
        let (mut offset, mut read, mut data) = (0, 0u64, next_data(&file, 0)?);
        while offset < size {
            let length = (size - offset).min(BLOCK) as usize;
            if data < offset {
                data = next_data(&file, offset)?;
            }
            let hash = if data >= offset + length as u64 {
                Value::Null
            } else {
                file.read_exact_at(&mut buffer[..length], offset)?;
                read += length as u64;
                if buffer[..length].iter().all(|b| *b == 0) {
                    Value::Null
                } else {
                    hex::encode(Sha256::digest(&buffer[..length])).into()
                }
            };
            blocks.push(json!({"offset": offset,"size": length,"hash": hash}));
            offset += length as u64;
        }
        Ok(json!({
            "version": 1,
            "size": size,
            "blockSize": BLOCK,
            "blocks": blocks,
            "localBytesRead": read
        }))
    })
    .await
    .map_err(Error::internal)?
}

pub fn validate(manifest: &Value) -> Result<()> {
    let blocks = manifest["blocks"]
        .as_array()
        .ok_or_else(|| Error::bad("Missing backup blocks."))?;
    let size = manifest["size"]
        .as_u64()
        .filter(|s| *s > 0 && *s <= 1024 * 1024 * 1024 * 1024)
        .ok_or_else(|| Error::bad("Invalid backup size."))?;
    if manifest["version"] != 1
        || manifest["blockSize"] != BLOCK
        || blocks.len() as u64 != size.div_ceil(BLOCK)
    {
        return Err(Error::bad("Invalid backup manifest."));
    }
    for (i, block) in blocks.iter().enumerate() {
        let offset = i as u64 * BLOCK;
        if block["offset"] != offset
            || block["size"] != (size - offset).min(BLOCK)
            || !(block["hash"].is_null() || valid_hash(text(block, "hash")))
        {
            return Err(Error::bad("Invalid backup extent."));
        }
    }
    Ok(())
}

pub fn valid_hash(hash: &str) -> bool {
    hash.len() == 64
        && hash
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

pub async fn block(path: &Path, manifest: &Value, hash: &str) -> Result<Vec<u8>> {
    block_where(path, manifest, hash, |_| true).await
}

/// Serves a block from a capture directory. An incremental capture holds only the
/// blocks written since its baseline, listed in `present.json`; the others are
/// already stored by the master.
pub async fn served(directory: &Path, hash: &str) -> Result<Vec<u8>> {
    Reader::open(directory).await?.block(hash).await
}

// A response owns the reader: cancellation releases stopped reads, and no cache
// or timer can retain an idle journal after the response has ended.
struct JournalReader {
    _cancel: Option<tokio_util::sync::DropGuard>,
    _stopped: Option<crate::file_lock::Guard>,
    volume: std::sync::Arc<crate::storage::runtime::Volume>,
    generation: i64,
}

struct Reader {
    directory: std::path::PathBuf,
    manifest: Value,
    journal: Option<JournalReader>,
    present: Option<std::collections::HashSet<u64>>,
}

impl Reader {
    async fn open(directory: &Path) -> Result<Self> {
        let manifest: Value =
            serde_json::from_slice(&tokio::fs::read(directory.join("manifest.json")).await?)?;
        if manifest["onDemand"] == true {
            let run = tokio::fs::read_to_string(directory.join("run")).await?;
            crate::validation::uuid(&run)?;
            let state = directory
                .parent()
                .and_then(Path::parent)
                .ok_or_else(|| Error::bad("Invalid snapshot directory."))?;
            let disk = state.join("disks").join(run);
            let stopped = match crate::file_lock::exclusive(&disk.join("lock"), "Disk active.") {
                Ok(lock) => Some(lock),
                Err(error) if error.is_conflict() => None,
                Err(error) => return Err(error),
            };
            let volume = crate::storage::runtime::load(&disk).await?;
            let cancel = stopped.as_ref().map(|_| volume.stop.clone().drop_guard());
            let generation = manifest["generation"]
                .as_i64()
                .ok_or_else(|| Error::bad("Invalid generation."))?;
            return Ok(Self {
                directory: directory.into(),
                manifest,
                present: None,
                journal: Some(JournalReader {
                    _cancel: cancel,
                    _stopped: stopped,
                    volume,
                    generation,
                }),
            });
        }
        let present = match tokio::fs::read(directory.join("present.json")).await {
            Ok(bytes) => Some(serde_json::from_slice(&bytes)?),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.into()),
        };
        Ok(Self {
            directory: directory.into(),
            manifest,
            journal: None,
            present,
        })
    }

    async fn block(&self, hash: &str) -> Result<Vec<u8>> {
        if let Some(journal) = &self.journal {
            let (volume, generation, hash) =
                (journal.volume.clone(), journal.generation, hash.to_owned());
            return tokio::task::spawn_blocking(move || {
                volume.disk.captured_block(generation, &hash)
            })
            .await
            .map_err(Error::internal)?
            .map_err(Into::into);
        }
        block_where(
            &self.directory.join("disk"),
            &self.manifest,
            hash,
            |offset| {
                self.present
                    .as_ref()
                    .is_none_or(|present| present.contains(&offset))
            },
        )
        .await
    }
}

/// At most eight blocks per response, streamed one at a time. Hashes and lengths
/// remain authenticated by the captured manifest and checked again by the master.
pub async fn served_batch(
    directory: &Path,
    hashes: Vec<String>,
) -> Result<(u64, axum::body::Body)> {
    if hashes.is_empty() || hashes.len() > READ_BATCH || hashes.iter().any(|hash| !valid_hash(hash))
    {
        return Err(Error::bad("Invalid snapshot block batch."));
    }
    let reader = Reader::open(directory).await?;
    validate(&reader.manifest)?;
    let mut length = 0;
    for hash in &hashes {
        let block = reader.manifest["blocks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|block| block["hash"] == *hash)
            .ok_or_else(|| Error::not_found("Unknown backup block."))?;
        length += block["size"].as_u64().unwrap();
    }
    let stream = futures_util::stream::try_unfold(
        (reader, hashes.into_iter()),
        |(reader, mut hashes)| async move {
            let Some(hash) = hashes.next() else {
                return Ok::<_, Error>(None);
            };
            let bytes = reader.block(&hash).await?;
            Ok(Some((bytes, (reader, hashes))))
        },
    );
    Ok((length, axum::body::Body::from_stream(stream)))
}

async fn block_where(
    path: &Path,
    manifest: &Value,
    hash: &str,
    available: impl Fn(u64) -> bool,
) -> Result<Vec<u8>> {
    if !valid_hash(hash) {
        return Err(Error::bad("Invalid block digest."));
    }
    let block = manifest["blocks"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|b| b["hash"] == hash && b["offset"].as_u64().is_some_and(&available))
        .ok_or_else(|| Error::not_found("Unknown backup block."))?;
    let size = block["size"]
        .as_u64()
        .filter(|s| *s <= BLOCK)
        .ok_or_else(|| Error::bad("Invalid block size."))?;
    let mut file = tokio::fs::File::open(path).await?;
    file.seek(std::io::SeekFrom::Start(
        block["offset"]
            .as_u64()
            .ok_or_else(|| Error::bad("Invalid block offset."))?,
    ))
    .await?;
    let mut bytes = vec![0; size as usize];
    file.read_exact(&mut bytes).await?;
    if hex::encode(Sha256::digest(&bytes)) != hash {
        return Err(Error::conflict("Backup data changed."));
    }
    Ok(bytes)
}

pub async fn restore<F, Fut>(target: &Path, manifest: &Value, mut fetch: F) -> Result<()>
where
    F: FnMut(String) -> Fut,
    Fut: Future<Output = Result<Vec<u8>>>,
{
    validate(manifest)?;
    if target.exists() {
        return Err(Error::conflict("Restore cannot replace an existing disk."));
    }
    let directory = target
        .parent()
        .ok_or_else(|| Error::bad("Invalid restore directory."))?;
    crate::skills::private_dir(directory).await?;
    let temporary = tempfile::NamedTempFile::new_in(directory)?;
    let mut file = tokio::fs::File::from_std(temporary.reopen()?);
    file.set_len(manifest["size"].as_u64().unwrap()).await?;
    for block in manifest["blocks"].as_array().unwrap() {
        if block["hash"].is_null() {
            continue;
        }
        let hash = text(block, "hash");
        let bytes = fetch(hash.to_owned()).await?;
        if bytes.len() as u64 != block["size"].as_u64().unwrap()
            || hex::encode(Sha256::digest(&bytes)) != hash
        {
            return Err(Error::bad("Backup block integrity check failed."));
        }
        file.seek(std::io::SeekFrom::Start(block["offset"].as_u64().unwrap()))
            .await?;
        file.write_all(&bytes).await?;
    }
    file.sync_all().await?;
    drop(file);
    temporary
        .persist_noclobber(target)
        .map_err(Error::internal)?;
    tokio::fs::File::open(directory).await?.sync_all().await?;
    Ok(())
}

/// A peer cannot cause an unbounded allocation by lying about a block's size.
pub async fn response_block(response: reqwest::Response) -> Result<Vec<u8>> {
    use futures_util::StreamExt;
    let mut stream = response.bytes_stream();
    let mut bytes = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(Error::internal)?;
        if bytes.len().saturating_add(chunk.len()) > BLOCK as usize {
            return Err(Error::bad("Backup block exceeds its maximum size."));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

/// Fetch missing blocks in manifest order with bounded response and allocation
/// sizes. Older controllers keep working through their single-block endpoint.
pub struct Fetch {
    http: reqwest::Client,
    url: String,
    credential: String,
    pending: std::collections::VecDeque<(String, u64)>,
    stream: Option<std::pin::Pin<Box<dyn tokio::io::AsyncRead + Send>>>,
    remaining: usize,
    legacy: bool,
}

impl Fetch {
    pub fn new(
        http: reqwest::Client,
        url: String,
        credential: String,
        blocks: Vec<(String, u64)>,
    ) -> Self {
        Self {
            http,
            url,
            credential,
            pending: blocks.into(),
            stream: None,
            remaining: 0,
            legacy: false,
        }
    }

    pub async fn block(&mut self, hash: &str) -> Result<Vec<u8>> {
        use futures_util::TryStreamExt;
        let size = self
            .pending
            .front()
            .filter(|(expected, size)| expected == hash && *size <= BLOCK)
            .map(|(_, size)| *size)
            .ok_or_else(|| Error::bad("Unexpected snapshot block."))?;
        if self.stream.is_none() && !self.legacy {
            let batch: Vec<_> = self.pending.iter().take(READ_BATCH).collect();
            let length: u64 = batch.iter().map(|(_, size)| *size).sum();
            let hashes: Vec<_> = batch.iter().map(|(hash, _)| hash).collect();
            let response = self
                .http
                .post(format!("{}/blocks", self.url))
                .bearer_auth(&self.credential)
                .json(&json!({"hashes": hashes}))
                .timeout(std::time::Duration::from_secs(120))
                .send()
                .await
                .map_err(|_| Error::unavailable("Backup block transfer interrupted."))?;
            // Old relays translate unsupported operations into 503. Retry the
            // existing endpoint once; actual unavailability still fails there.
            if matches!(response.status().as_u16(), 404 | 405 | 503) {
                self.legacy = true;
            } else {
                if !response.status().is_success() {
                    return Err(Error::unavailable("Backup block batch unavailable."));
                }
                if response
                    .content_length()
                    .is_some_and(|actual| actual != length)
                {
                    return Err(Error::bad("Backup block batch has the wrong size."));
                }
                self.remaining = batch.len();
                self.stream = Some(Box::pin(tokio_util::io::StreamReader::new(
                    response.bytes_stream().map_err(std::io::Error::other),
                )));
            }
        }
        let bytes = if let Some(stream) = &mut self.stream {
            let mut bytes = vec![0; size as usize];
            stream
                .read_exact(&mut bytes)
                .await
                .map_err(|_| Error::unavailable("Backup block batch interrupted."))?;
            self.remaining -= 1;
            if self.remaining == 0 {
                if stream
                    .read(&mut [0])
                    .await
                    .map_err(|_| Error::unavailable("Backup block batch interrupted."))?
                    != 0
                {
                    return Err(Error::bad("Backup block batch exceeds its expected size."));
                }
                self.stream = None;
            }
            bytes
        } else {
            let response = self
                .http
                .get(format!("{}/{hash}", self.url))
                .bearer_auth(&self.credential)
                .timeout(std::time::Duration::from_secs(120))
                .send()
                .await
                .map_err(|_| Error::unavailable("Backup block transfer interrupted."))?;
            if !response.status().is_success() {
                return Err(Error::unavailable("Backup block unavailable."));
            }
            response_block(response).await?
        };
        self.pending.pop_front();
        Ok(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Router, body::Body, extract::Request, response::IntoResponse, routing::any};
    use std::sync::{Arc, Mutex};

    async fn peer(app: Router) -> (String, tokio::task::AbortHandle) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (format!("http://{address}"), server.abort_handle())
    }

    #[tokio::test]
    async fn fetch_batches_in_order_and_falls_back_once_for_old_runners() {
        for legacy_status in [None, Some(404), Some(405), Some(503)] {
            let legacy = legacy_status.is_some();
            let requests = Arc::new(Mutex::new(Vec::new()));
            let captured = requests.clone();
            let app = Router::new().fallback(any(move |request: Request| {
                let captured = captured.clone();
                async move {
                    assert_eq!(request.headers()["authorization"], "Bearer fixture");
                    let method = request.method().clone();
                    let path = request.uri().path().to_owned();
                    let bytes = axum::body::to_bytes(request.into_body(), 4096)
                        .await
                        .unwrap();
                    let mut hashes = Vec::new();
                    if method == "POST" && !legacy {
                        let body: Value = serde_json::from_slice(&bytes).unwrap();
                        hashes = body["hashes"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .map(|hash| hash.as_str().unwrap().to_owned())
                            .collect();
                        assert!(hashes.len() <= READ_BATCH);
                    }
                    captured
                        .lock()
                        .unwrap()
                        .push((method.clone(), hashes.clone()));
                    if method == "POST" && legacy {
                        return axum::http::StatusCode::from_u16(legacy_status.unwrap())
                            .unwrap()
                            .into_response();
                    }
                    if method == "GET" {
                        hashes.push(path.trim_start_matches('/').to_owned());
                    }
                    let bytes: Vec<_> = hashes
                        .iter()
                        .flat_map(|hash| {
                            let index = u8::from_str_radix(&hash[62..], 16).unwrap();
                            vec![index; index as usize + 1]
                        })
                        .collect();
                    bytes.into_response()
                }
            }));
            let (url, server) = peer(app).await;
            let blocks: Vec<_> = (0..READ_BATCH + 1)
                .map(|i| (format!("{i:064x}"), i as u64 + 1))
                .collect();
            let mut fetch = Fetch::new(
                reqwest::Client::new(),
                url,
                "fixture".into(),
                blocks.clone(),
            );
            for (index, (hash, size)) in blocks.iter().enumerate() {
                assert_eq!(
                    fetch.block(hash).await.unwrap(),
                    vec![index as u8; *size as usize]
                );
            }
            let requests = requests.lock().unwrap();
            let posts: Vec<_> = requests
                .iter()
                .filter(|(method, _)| *method == "POST")
                .collect();
            if legacy {
                assert_eq!(posts.len(), 1);
                assert_eq!(requests.len(), READ_BATCH + 2);
            } else {
                assert_eq!(requests.len(), 2);
                assert_eq!(posts[0].1.len(), READ_BATCH);
                assert_eq!(posts[1].1.len(), 1);
            }
            server.abort();
        }
    }

    #[tokio::test]
    async fn fetch_rejects_truncated_excess_and_failed_batches() {
        for (status, actual, expected) in [(200, 2, 3), (200, 4, 3), (500, 3, 3)] {
            let app = Router::new().fallback(any(move |request: Request| async move {
                assert_eq!(
                    request.method(),
                    "POST",
                    "transfer failures must not fall back"
                );
                let stream = futures_util::stream::iter([Ok::<_, std::io::Error>(vec![7; actual])]);
                (
                    axum::http::StatusCode::from_u16(status).unwrap(),
                    Body::from_stream(stream),
                )
            }));
            let (url, server) = peer(app).await;
            let hash = "a".repeat(64);
            let mut fetch = Fetch::new(
                reqwest::Client::new(),
                url,
                "fixture".into(),
                vec![(hash.clone(), expected)],
            );
            assert!(fetch.block(&hash).await.is_err());
            server.abort();
        }
    }

    #[tokio::test]
    async fn batch_rejects_invalid_or_unbounded_requests_before_opening_a_disk() {
        for hashes in [
            vec![],
            vec!["a".repeat(64); READ_BATCH + 1],
            vec!["../invalid".into()],
        ] {
            let error = served_batch(Path::new("/nonexistent-snapshot-fixture"), hashes)
                .await
                .err()
                .unwrap();
            assert_eq!(error.status, 400);
        }
    }
}
