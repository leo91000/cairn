//! S3 operations stay server-side. Recovery blocks use a shared SDK client;
//! infrequent cold archive operations still use the AWS CLI.
use crate::{
    error::{Error, Result},
    service::Service,
};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{Arc, Weak},
    time::Duration,
};
use tokio::process::Command;
use tokio::sync::{Mutex, OnceCell, Semaphore};

type PendingRead = OnceCell<Result<bytes::Bytes>>;
type ReadKey = (String, String, u64, Option<String>, String);
pub(crate) const HOT_WRITE_CONCURRENCY: usize = 4;

#[derive(PartialEq, Eq)]
struct ClientKey {
    endpoint: Option<String>,
    region: String,
    environment_endpoint: Option<String>,
    profile: Option<String>,
}

/// One connection and identity cache per server configuration, shared by all
/// recovery operations. A changed endpoint or region replaces the cached client.
pub struct HotS3 {
    client: Mutex<Option<(ClientKey, aws_sdk_s3::Client)>>,
    reads: Semaphore,
    writes: Semaphore,
    pending: Mutex<HashMap<ReadKey, Weak<PendingRead>>>,
}

impl HotS3 {
    pub fn new() -> Self {
        Self {
            client: Mutex::new(None),
            reads: Semaphore::new(8),
            writes: Semaphore::new(HOT_WRITE_CONCURRENCY),
            pending: Mutex::new(HashMap::new()),
        }
    }

    async fn client(&self, endpoint: Option<&str>, region: &str) -> aws_sdk_s3::Client {
        let identity = ClientKey {
            endpoint: endpoint.map(str::to_owned),
            region: region.to_owned(),
            environment_endpoint: std::env::var("AWS_ENDPOINT_URL_S3").ok(),
            profile: std::env::var("AWS_PROFILE").ok(),
        };
        let mut cached = self.client.lock().await;
        if let Some((key, client)) = &*cached
            && key == &identity
        {
            return client.clone();
        }
        let mut loader = aws_config::defaults(aws_config::BehaviorVersion::latest());
        if !region.is_empty() {
            loader = loader.region(aws_config::Region::new(region.to_owned()));
        }
        let config = loader
            .timeout_config(
                aws_config::timeout::TimeoutConfig::builder()
                    .operation_attempt_timeout(Duration::from_secs(40))
                    .operation_timeout(Duration::from_secs(120))
                    .build(),
            )
            .load()
            .await;
        let mut builder = aws_sdk_s3::config::Builder::from(&config);
        // Read-back verification already checks the stored bytes. Avoid optional
        // checksum headers that some S3-compatible providers do not implement.
        builder = builder.request_checksum_calculation(
            aws_sdk_s3::config::RequestChecksumCalculation::WhenRequired,
        );
        if let Some(endpoint) = endpoint {
            builder = builder.endpoint_url(endpoint);
        }
        if endpoint.is_some() || std::env::var_os("AWS_ENDPOINT_URL_S3").is_some() {
            builder = builder.force_path_style(true);
        }
        let client = aws_sdk_s3::Client::from_conf(builder.build());
        *cached = Some((identity, client.clone()));
        client
    }
}

impl Default for HotS3 {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone)]
pub struct Storage {
    pub bucket: String,
    binary: String,
    /// S3-compatible providers (OVHcloud, Scaleway…) need an explicit endpoint.
    pub(crate) endpoint: Option<String>,
    region: String,
    hot: Arc<HotS3>,
    /// AWS names its cold tier GLACIER; OVHcloud Cold Archive is DEEP_ARCHIVE.
    cold_class: String,
}
fn setting(config: &Value, env: &str, key: &str) -> String {
    std::env::var(env)
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| config[key].as_str().unwrap_or("").into())
}
impl Storage {
    pub fn configured(s: &Service) -> Result<Self> {
        let file = s.config.data_dir.join("archive-s3.json");
        let config: Value = if file.exists() {
            serde_json::from_slice(&std::fs::read(file)?)?
        } else {
            json!({})
        };
        let bucket = setting(&config, "ARCHIVE_S3_BUCKET", "bucket");
        if bucket.is_empty()
            || !bucket
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'.')
        {
            return Err(Error::new(
                409,
                "Configure a valid ARCHIVE_S3_BUCKET on the server.",
            ));
        }
        let endpoint = setting(&config, "ARCHIVE_S3_ENDPOINT", "endpoint");
        if !endpoint.is_empty() && !endpoint.starts_with("https://") {
            return Err(Error::new(
                409,
                "ARCHIVE_S3_ENDPOINT must be an https:// URL.",
            ));
        }
        let cold_class =
            match setting(&config, "ARCHIVE_S3_COLD_STORAGE_CLASS", "coldStorageClass").as_str() {
                "" | "GLACIER" => "GLACIER",
                "DEEP_ARCHIVE" => "DEEP_ARCHIVE",
                _ => {
                    return Err(Error::new(
                        409,
                        "ARCHIVE_S3_COLD_STORAGE_CLASS must be GLACIER or DEEP_ARCHIVE.",
                    ));
                }
            };
        Ok(Self {
            bucket,
            binary: config["awsBinary"].as_str().unwrap_or("aws").into(),
            endpoint: Some(endpoint).filter(|e| !e.is_empty()),
            region: {
                let configured = setting(&config, "ARCHIVE_S3_REGION", "region");
                if configured.is_empty() {
                    std::env::var("AWS_REGION")
                        .ok()
                        .filter(|value| !value.is_empty())
                        .or_else(|| {
                            std::env::var("AWS_DEFAULT_REGION")
                                .ok()
                                .filter(|value| !value.is_empty())
                        })
                        .unwrap_or_default()
                } else {
                    configured
                }
            },
            hot: s.hot_s3.clone(),
            cold_class: cold_class.into(),
        })
    }
    async fn call(&self, args: Vec<String>) -> Result<Value> {
        self.optional(args, None).await
    }
    async fn optional(&self, args: Vec<String>, missing: Option<&str>) -> Result<Value> {
        let mut command = Command::new(&self.binary);
        command.args(args).args(["--output", "json"]);
        if let Some(endpoint) = &self.endpoint {
            command.args(["--endpoint-url", endpoint]);
        }
        command
            .env("AWS_PAGER", "")
            .env("AWS_CLI_AUTO_PROMPT", "off")
            .stdin(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        if std::env::var("AWS_DEFAULT_REGION").is_ok_and(|region| region.is_empty()) {
            command.env_remove("AWS_DEFAULT_REGION");
        }
        let output = tokio::time::timeout(Duration::from_secs(7200), command.output())
            .await
            .map_err(|_| Error::new(503, "Archive storage operation timed out; it will retry."))?
            .map_err(|_| Error::new(503, "Unable to start the server's AWS CLI."))?;
        if !output.status.success()
            && missing.is_some_and(|code| String::from_utf8_lossy(&output.stderr).contains(code))
        {
            return Ok(Value::Null);
        }
        if !output.status.success() {
            return Err(Error::new(
                503,
                "Archive storage operation failed; local data is retained and the operation will retry.",
            ));
        }
        if output.stdout.is_empty() {
            return Ok(Value::Null);
        }
        serde_json::from_slice(&output.stdout)
            .map_err(|_| Error::new(503, "Invalid response from archive storage."))
    }
    pub async fn validate(&self) -> Result<()> {
        let block = self
            .optional(
                vec![
                    "s3api".into(),
                    "get-public-access-block".into(),
                    "--bucket".into(),
                    self.bucket.clone(),
                ],
                Some("NotImplemented"),
            )
            .await?;
        if block.is_null() {
            self.validate_private().await?;
        } else if [
            "BlockPublicAcls",
            "IgnorePublicAcls",
            "BlockPublicPolicy",
            "RestrictPublicBuckets",
        ]
        .iter()
        .any(|k| block["PublicAccessBlockConfiguration"][k] != true)
        {
            return Err(Error::bad(
                "The archive bucket must block all public access.",
            ));
        }
        let lifecycle = self
            .optional(
                vec![
                    "s3api".into(),
                    "get-bucket-lifecycle-configuration".into(),
                    "--bucket".into(),
                    self.bucket.clone(),
                ],
                Some("NoSuchLifecycleConfiguration"),
            )
            .await?;
        if lifecycle["Rules"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|r| r["Status"] == "Enabled")
        {
            return Err(Error::bad(
                "Use a dedicated archive bucket without enabled lifecycle rules; Léo manages retention.",
            ));
        }
        let lock = self
            .optional(
                vec![
                    "s3api".into(),
                    "get-object-lock-configuration".into(),
                    "--bucket".into(),
                    self.bucket.clone(),
                ],
                Some("ObjectLockConfigurationNotFoundError"),
            )
            .await?;
        if lock["ObjectLockConfiguration"]["ObjectLockEnabled"] == "Enabled" {
            return Err(Error::bad(
                "Object Lock is incompatible with automatic trash deletion. Use a dedicated bucket without Object Lock.",
            ));
        }
        self.call(vec![
            "s3api".into(),
            "head-bucket".into(),
            "--bucket".into(),
            self.bucket.clone(),
        ])
        .await?;
        Ok(())
    }
    /// Providers without public access blocks (OVHcloud) must show a private ACL and no bucket policy.
    async fn validate_private(&self) -> Result<()> {
        let acl = self
            .call(vec![
                "s3api".into(),
                "get-bucket-acl".into(),
                "--bucket".into(),
                self.bucket.clone(),
            ])
            .await?;
        if acl["Grants"].as_array().into_iter().flatten().any(|grant| {
            let uri = grant["Grantee"]["URI"].as_str().unwrap_or("");
            uri.ends_with("/AllUsers") || uri.ends_with("/AuthenticatedUsers")
        }) {
            return Err(Error::bad(
                "The archive bucket must not grant public access.",
            ));
        }
        let policy = self
            .optional(
                vec![
                    "s3api".into(),
                    "get-bucket-policy".into(),
                    "--bucket".into(),
                    self.bucket.clone(),
                ],
                Some("NoSuchBucketPolicy"),
            )
            .await;
        match policy {
            Ok(Value::Null) => Ok(()),
            Ok(_) => Err(Error::bad(
                "Use a dedicated archive bucket without a bucket policy; grant access through the server's credentials.",
            )),
            // Providers without bucket policies (OVHcloud) cannot expose the bucket through one.
            Err(_) if self.endpoint.is_some() => Ok(()),
            Err(error) => Err(error),
        }
    }
    fn uri(&self, key: &str) -> String {
        format!("s3://{}/{key}", self.bucket)
    }
    pub async fn upload(&self, path: &Path, key: &str) -> Result<()> {
        self.call(vec![
            "s3".into(),
            "cp".into(),
            path.to_string_lossy().into(),
            self.uri(key),
            "--only-show-errors".into(),
            "--sse".into(),
            "AES256".into(),
        ])
        .await?;
        Ok(())
    }
    pub async fn download(&self, key: &str, path: &Path) -> Result<()> {
        self.call(vec![
            "s3".into(),
            "cp".into(),
            self.uri(key),
            path.to_string_lossy().into(),
            "--only-show-errors".into(),
            "--force-glacier-transfer".into(),
        ])
        .await?;
        Ok(())
    }
    pub async fn upload_bytes(&self, bytes: Vec<u8>, key: &str) -> Result<()> {
        let _permit = self.hot.writes.acquire().await.map_err(Error::internal)?;
        let client = self
            .hot
            .client(self.endpoint.as_deref(), &self.region)
            .await;
        let bytes = bytes::Bytes::from(bytes);
        client
            .put_object()
            .bucket(&self.bucket)
            .key(key)
            .server_side_encryption(aws_sdk_s3::types::ServerSideEncryption::Aes256)
            .body(aws_sdk_s3::primitives::ByteStream::from(bytes.clone()))
            .send()
            .await
            .map_err(|_| {
                Error::new(503, "Recovery block upload failed; local data is retained.")
            })?;
        self.verify_bytes(key, &bytes).await
    }
    async fn verify_bytes(&self, key: &str, bytes: &[u8]) -> Result<()> {
        let remote = self.download_bytes(key, bytes.len() as u64).await?;
        if remote != bytes {
            return Err(Error::bad("Remote backup checksum mismatch."));
        }
        Ok(())
    }
    /// Verify a cached block or manifest before publishing its recovery point.
    pub async fn upload_file_verified(&self, path: &Path, key: &str) -> Result<()> {
        let bytes = tokio::fs::read(path).await?;
        self.upload_bytes(bytes, key).await
    }
    /// The response body is bounded even if a provider ignores Content-Length or Range.
    pub async fn download_bytes(&self, key: &str, limit: u64) -> Result<Vec<u8>> {
        let request = (
            self.bucket.clone(),
            key.to_owned(),
            limit,
            self.endpoint.clone(),
            self.region.clone(),
        );
        let pending = {
            let mut reads = self.hot.pending.lock().await;
            if reads.len() >= 1024 {
                reads.retain(|_, pending| pending.strong_count() > 0);
            }
            reads
                .get(&request)
                .and_then(Weak::upgrade)
                .unwrap_or_else(|| {
                    let pending = Arc::new(PendingRead::new());
                    reads.insert(request.clone(), Arc::downgrade(&pending));
                    pending
                })
        };
        let result = pending
            .get_or_init(|| async { self.fetch_bytes(key, limit).await.map(bytes::Bytes::from) })
            .await
            .clone();
        let mut reads = self.hot.pending.lock().await;
        if reads
            .get(&request)
            .is_some_and(|entry| entry.ptr_eq(&Arc::downgrade(&pending)))
        {
            reads.remove(&request);
        }
        result.map(|bytes| bytes.to_vec())
    }

    async fn fetch_bytes(&self, key: &str, limit: u64) -> Result<Vec<u8>> {
        let _permit = self.hot.reads.acquire().await.map_err(Error::internal)?;
        let client = self
            .hot
            .client(self.endpoint.as_deref(), &self.region)
            .await;
        tokio::time::timeout(Duration::from_secs(60), async {
            let output = client
                .get_object()
                .bucket(&self.bucket)
                .key(key)
                .send()
                .await
                .map_err(|error| {
                    if error
                        .as_service_error()
                        .is_some_and(|error| error.is_no_such_key())
                        || error
                            .raw_response()
                            .is_some_and(|response| response.status().as_u16() == 404)
                    {
                        Error::new(409, "Remote recovery block is missing.")
                    } else {
                        Error::new(503, "Recovery storage unavailable; the read will retry.")
                    }
                })?;
            if output
                .content_length()
                .is_some_and(|length| length < 0 || length as u64 > limit)
            {
                return Err(Error::bad("Remote block exceeds the transfer limit."));
            }
            let capacity = output.content_length().unwrap_or(0).max(0) as u64;
            let mut bytes = Vec::with_capacity(capacity.min(limit) as usize);
            let mut body = output.body;
            while let Some(chunk) = body.try_next().await.map_err(|_| {
                Error::new(
                    503,
                    "Recovery block transfer interrupted; the read will retry.",
                )
            })? {
                if chunk.len() as u64 > limit.saturating_sub(bytes.len() as u64) {
                    return Err(Error::bad("Remote block exceeds the transfer limit."));
                }
                bytes.extend_from_slice(&chunk);
            }
            Ok(bytes)
        })
        .await
        .map_err(|_| {
            Error::new(
                503,
                "Recovery block transfer timed out; the read will retry.",
            )
        })?
    }
    pub async fn cold(&self, key: &str) -> Result<()> {
        let head = self
            .call(vec![
                "s3api".into(),
                "head-object".into(),
                "--bucket".into(),
                self.bucket.clone(),
                "--key".into(),
                key.into(),
            ])
            .await?;
        if head["StorageClass"] != self.cold_class.as_str() {
            self.call(vec![
                "s3".into(),
                "cp".into(),
                self.uri(key),
                self.uri(key),
                "--storage-class".into(),
                self.cold_class.clone(),
                "--metadata-directive".into(),
                "REPLACE".into(),
                "--only-show-errors".into(),
                "--sse".into(),
                "AES256".into(),
            ])
            .await?;
        }
        let versions = self
            .call(vec![
                "s3api".into(),
                "list-object-versions".into(),
                "--bucket".into(),
                self.bucket.clone(),
                "--prefix".into(),
                key.into(),
            ])
            .await?;
        for version in versions["Versions"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|v| v["Key"] == key && v["IsLatest"] == false)
        {
            self.call(vec![
                "s3api".into(),
                "delete-object".into(),
                "--bucket".into(),
                self.bucket.clone(),
                "--key".into(),
                key.into(),
                "--version-id".into(),
                version["VersionId"].as_str().unwrap_or("null").into(),
            ])
            .await?;
        }
        Ok(())
    }
    pub async fn ready(&self, key: &str) -> Result<bool> {
        let head = self
            .call(vec![
                "s3api".into(),
                "head-object".into(),
                "--bucket".into(),
                self.bucket.clone(),
                "--key".into(),
                key.into(),
            ])
            .await?;
        if head["StorageClass"] != self.cold_class.as_str() {
            return Ok(true);
        }
        if let Some(restore) = head["Restore"].as_str() {
            return Ok(restore.contains("ongoing-request=\"false\""));
        }
        self.call(vec![
            "s3api".into(),
            "restore-object".into(),
            "--bucket".into(),
            self.bucket.clone(),
            "--key".into(),
            key.into(),
            "--restore-request".into(),
            json!({"Days":3,"GlacierJobParameters":{"Tier":"Standard"}}).to_string(),
        ])
        .await?;
        Ok(false)
    }
    pub async fn purge(&self, prefix: &str) -> Result<()> {
        let listed = self
            .call(vec![
                "s3api".into(),
                "list-object-versions".into(),
                "--bucket".into(),
                self.bucket.clone(),
                "--prefix".into(),
                prefix.into(),
            ])
            .await?;
        for item in listed["Versions"]
            .as_array()
            .into_iter()
            .flatten()
            .chain(listed["DeleteMarkers"].as_array().into_iter().flatten())
        {
            let key = item["Key"]
                .as_str()
                .filter(|k| k.starts_with(prefix))
                .ok_or_else(|| Error::internal("Unexpected archive key"))?;
            self.call(vec![
                "s3api".into(),
                "delete-object".into(),
                "--bucket".into(),
                self.bucket.clone(),
                "--key".into(),
                key.into(),
                "--version-id".into(),
                item["VersionId"].as_str().unwrap_or("null").into(),
            ])
            .await?;
        }
        let uploads = self
            .call(vec![
                "s3api".into(),
                "list-multipart-uploads".into(),
                "--bucket".into(),
                self.bucket.clone(),
                "--prefix".into(),
                prefix.into(),
            ])
            .await?;
        for item in uploads["Uploads"].as_array().into_iter().flatten() {
            let key = item["Key"]
                .as_str()
                .filter(|k| k.starts_with(prefix))
                .ok_or_else(|| Error::internal("Unexpected archive upload"))?;
            self.call(vec![
                "s3api".into(),
                "abort-multipart-upload".into(),
                "--bucket".into(),
                self.bucket.clone(),
                "--key".into(),
                key.into(),
                "--upload-id".into(),
                item["UploadId"].as_str().unwrap_or("").into(),
            ])
            .await?;
        }
        self.call(vec![
            "s3".into(),
            "rm".into(),
            self.uri(prefix),
            "--recursive".into(),
            "--only-show-errors".into(),
        ])
        .await?;
        Ok(())
    }
}

pub async fn hash(path: PathBuf) -> Result<String> {
    tokio::task::spawn_blocking(move || {
        use sha2::{Digest, Sha256};
        use std::io::Read;
        let mut input = std::fs::File::open(path)?;
        let mut hash = Sha256::new();
        let mut buffer = vec![0; 1024 * 1024];
        loop {
            let n = input.read(&mut buffer)?;
            if n == 0 {
                break;
            }
            hash.update(&buffer[..n]);
        }
        Ok(hex::encode(hash.finalize()))
    })
    .await
    .map_err(Error::internal)?
}

/// Bounded authenticated frames. AAD binds every frame to its archive and position;
/// an authenticated terminal frame makes truncation detectable, including at EOF.
pub async fn crypt(
    s: &Service,
    input: PathBuf,
    output: PathBuf,
    archive: String,
    decrypt: bool,
) -> Result<()> {
    let vault = s.vault.clone();
    tokio::task::spawn_blocking(move || {
        use base64::{Engine, engine::general_purpose::STANDARD};
        use std::io::{BufRead, BufReader, Read, Write};
        let mut source = BufReader::new(std::fs::File::open(input)?);
        let mut target = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(output)?;
        let mut index = 0u64;
        loop {
            let aad = format!("conversation-archive-v1:{archive}:{index}");
            if decrypt {
                let mut line = Vec::new();
                source
                    .by_ref()
                    .take(4 * 1024 * 1024)
                    .read_until(b'\n', &mut line)?;
                if line.last() != Some(&b'\n') {
                    return Err(Error::bad("Truncated or invalid archive."));
                }
                let value = vault.decrypt(&aad, &serde_json::from_slice::<Value>(&line)?)?;
                if value["end"] == true {
                    if !source.fill_buf()?.is_empty() {
                        return Err(Error::bad("Unexpected archive trailer."));
                    }
                    break;
                }
                let bytes = STANDARD
                    .decode(
                        value["data"]
                            .as_str()
                            .ok_or_else(|| Error::bad("Invalid archive frame."))?,
                    )
                    .map_err(Error::internal)?;
                target.write_all(&bytes)?;
            } else {
                let mut bytes = vec![0; 1024 * 1024];
                let n = source.read(&mut bytes)?;
                let value = if n == 0 {
                    json!({"end":true})
                } else {
                    json!({"data":STANDARD.encode(&bytes[..n])})
                };
                serde_json::to_writer(&mut target, &vault.encrypt(&aad, &value)?)?;
                target.write_all(b"\n")?;
                if n == 0 {
                    break;
                }
            }
            index += 1;
        }
        target.sync_all()?;
        Ok(())
    })
    .await
    .map_err(Error::internal)?
}

pub async fn tar(args: Vec<String>) -> Result<()> {
    let status = Command::new("tar")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .status()
        .await?;
    if !status.success() {
        return Err(Error::new(
            503,
            "Archive filesystem transfer failed; local data is retained.",
        ));
    }
    Ok(())
}
