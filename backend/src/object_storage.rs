//! S3 operations stay server-side. Recovery blocks use a shared SDK client;
//! bucket administration and purge operations use the AWS CLI.
use crate::{
    error::{Error, Result},
    service::Service,
};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    path::Path,
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
    get_metrics: crate::storage::metrics::Counter,
    put_metrics: crate::storage::metrics::Counter,
    purge_metrics: crate::storage::metrics::Counter,
}

impl HotS3 {
    pub fn new() -> Self {
        Self {
            client: Mutex::new(None),
            reads: Semaphore::new(8),
            writes: Semaphore::new(HOT_WRITE_CONCURRENCY),
            pending: Mutex::new(HashMap::new()),
            get_metrics: Default::default(),
            put_metrics: Default::default(),
            purge_metrics: Default::default(),
        }
    }

    pub(crate) fn performance(&self) -> Value {
        json!({
            "get": self.get_metrics.snapshot(),
            "put": self.put_metrics.snapshot(),
            "purgeKey": self.purge_metrics.snapshot()
        })
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
}

fn setting(config: &Value, suffix: &str, key: &str) -> String {
    std::env::var(format!("STORAGE_S3_{suffix}"))
        .ok()
        .filter(|value| !value.is_empty())
        .or_else(|| {
            std::env::var(format!("ARCHIVE_S3_{suffix}"))
                .ok()
                .filter(|v| !v.is_empty())
        })
        .unwrap_or_else(|| config[key].as_str().unwrap_or("").into())
}

impl Storage {
    pub fn configured(s: &Service) -> Result<Self> {
        let primary = s.config.data_dir.join("storage-s3.json");
        let file = if primary.exists() {
            primary
        } else {
            s.config.data_dir.join("archive-s3.json")
        };
        let config: Value = if file.exists() {
            serde_json::from_slice(&std::fs::read(file)?)?
        } else {
            json!({})
        };
        let bucket = setting(&config, "BUCKET", "bucket");
        if bucket.is_empty()
            || !bucket
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'.')
        {
            return Err(Error::conflict(
                "Configure a valid STORAGE_S3_BUCKET on the server.",
            ));
        }
        let endpoint = setting(&config, "ENDPOINT", "endpoint");
        if !endpoint.is_empty() && !endpoint.starts_with("https://") {
            return Err(Error::conflict(
                "STORAGE_S3_ENDPOINT must be an https:// URL.",
            ));
        }
        Ok(Self {
            bucket,
            binary: config["awsBinary"].as_str().unwrap_or("aws").into(),
            endpoint: Some(endpoint).filter(|e| !e.is_empty()),
            region: {
                let configured = setting(&config, "REGION", "region");
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
            .map_err(|_| Error::unavailable("Object storage operation timed out; it will retry."))?
            .map_err(|_| Error::unavailable("Unable to start the server's AWS CLI."))?;
        if !output.status.success()
            && missing.is_some_and(|code| String::from_utf8_lossy(&output.stderr).contains(code))
        {
            return Ok(Value::Null);
        }
        if !output.status.success() {
            return Err(Error::unavailable(
                "Object storage operation failed; local data is retained and the operation will retry.",
            ));
        }
        if output.stdout.is_empty() {
            return Ok(Value::Null);
        }
        serde_json::from_slice(&output.stdout)
            .map_err(|_| Error::unavailable("Invalid response from object storage."))
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
                "The storage bucket must block all public access.",
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
                "Use a dedicated storage bucket without lifecycle rules that could remove active disk blocks.",
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
                "Object Lock is incompatible with automatic trash deletion. Use a dedicated \
                    bucket without Object Lock.",
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
                "The storage bucket must not grant public access.",
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
                "Use a dedicated storage bucket without a bucket policy; grant access \
                    through the server's credentials.",
            )),
            // Providers without bucket policies (OVHcloud) cannot expose the bucket through one.
            Err(_) if self.endpoint.is_some() => Ok(()),
            Err(error) => Err(error),
        }
    }

    fn uri(&self, key: &str) -> String {
        format!("s3://{}/{key}", self.bucket)
    }

    pub async fn upload_bytes(&self, bytes: Vec<u8>, key: &str) -> Result<()> {
        let sample = self.hot.put_metrics.start();
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
                Error::unavailable("Recovery block upload failed; local data is retained.")
            })?;
        sample.finish(bytes.len());
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
        let sample = self.hot.get_metrics.start();
        let _permit = self.hot.reads.acquire().await.map_err(Error::internal)?;
        let client = self
            .hot
            .client(self.endpoint.as_deref(), &self.region)
            .await;
        let bytes = tokio::time::timeout(Duration::from_secs(60), async {
            let output = client
                .get_object()
                .bucket(&self.bucket)
                .key(key)
                .send()
                .await
                .map_err(|error| {
                    use aws_sdk_s3::error::ProvideErrorMetadata;
                    let status = error.raw_response().map(|response| response.status().as_u16());
                    let code = error.as_service_error().and_then(|error| error.code());
                    if error
                        .as_service_error()
                        .is_some_and(|error| error.is_no_such_key())
                        || error
                            .raw_response()
                            .is_some_and(|response| response.status().as_u16() == 404)
                    {
                        Error::conflict("Remote recovery block is missing.")
                    } else if status.is_some_and(|status| (400..500).contains(&status) && !matches!(status, 408 | 429))
                        && !matches!(code, Some("RequestTimeout" | "RequestTimeoutException" | "SlowDown" | "Throttling"))
                    {
                        Error::new(424, "Recovery storage rejected the read; check its access and configuration.")
                    } else {
                        Error::unavailable("Recovery storage unavailable; the read will retry.")
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
                Error::unavailable("Recovery block transfer interrupted; the read will retry.",
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
            Error::unavailable("Recovery block transfer timed out; the read will retry.",
            )
        })??;
        sample.finish(bytes.len());
        Ok(bytes)
    }

    /// Collect one immutable publication object, including versions and delete markers.
    /// Exact-key filtering prevents a prefix match from deleting a sibling object.
    /// Publication uses PutObject only, so this path has no multipart uploads to abort.
    pub async fn purge_key(&self, key: &str) -> Result<()> {
        let sample = self.hot.purge_metrics.start();
        use aws_sdk_s3::types::{Delete, ObjectIdentifier};
        let client = self
            .hot
            .client(self.endpoint.as_deref(), &self.region)
            .await;
        loop {
            let page = client
                .list_object_versions()
                .bucket(&self.bucket)
                .prefix(key)
                .max_keys(1000)
                .send()
                .await
                .map_err(|_| {
                    Error::unavailable(
                        "Cannot list obsolete disk object versions; cleanup will retry.",
                    )
                })?;
            let mut objects = Vec::new();
            for (object_key, version) in page
                .versions()
                .iter()
                .map(|v| (v.key(), v.version_id()))
                .chain(
                    page.delete_markers()
                        .iter()
                        .map(|v| (v.key(), v.version_id())),
                )
            {
                if object_key == Some(key) {
                    objects.push(
                        ObjectIdentifier::builder()
                            .key(key)
                            .set_version_id(version.map(str::to_owned))
                            .build()
                            .map_err(Error::internal)?,
                    );
                }
            }
            if objects.is_empty() {
                sample.finish(0);
                return Ok(());
            }
            {
                let result = client
                    .delete_objects()
                    .bucket(&self.bucket)
                    .delete(
                        Delete::builder()
                            .set_objects(Some(objects))
                            .quiet(true)
                            .build()
                            .map_err(Error::internal)?,
                    )
                    .send()
                    .await
                    .map_err(|_| {
                        Error::unavailable(
                            "Cannot delete obsolete disk object; cleanup will retry.",
                        )
                    })?;
                if !result.errors().is_empty() {
                    return Err(Error::unavailable(
                        "Some obsolete disk versions could not be deleted; cleanup will retry.",
                    ));
                }
            }
            // Start at this exact key again after deletion. This avoids keeping a
            // pagination cursor that refers to a version we have just removed.
        }
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
                .ok_or_else(|| Error::internal("Unexpected storage key"))?;
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
                .ok_or_else(|| Error::internal("Unexpected storage upload"))?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn recovery_reads_distinguish_permanent_refusals_from_temporary_outages() {
        use aws_sdk_s3::config::{Credentials, Region, retry::RetryConfig};
        use axum::{Router, http::StatusCode, routing::get};
        for (status, code, expected) in [
            (403, "AccessDenied", 424),
            (400, "InvalidObjectState", 424),
            (400, "RequestTimeout", 503),
            (404, "NoSuchKey", 409),
            (429, "SlowDown", 503),
            (503, "SlowDown", 503),
        ] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let endpoint = format!("http://{}", listener.local_addr().unwrap());
            let app = Router::new().fallback(get(move || async move {
                (
                    StatusCode::from_u16(status).unwrap(),
                    [("content-type", "application/xml")],
                    format!("<Error><Code>{code}</Code><Message>fixture</Message></Error>"),
                )
            }));
            let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
            let sdk = aws_sdk_s3::Client::from_conf(
                aws_sdk_s3::config::Builder::new()
                    .behavior_version_latest()
                    .region(Region::new("us-east-1"))
                    .credentials_provider(Credentials::new(
                        "fixture", "fixture", None, None, "fixture",
                    ))
                    .endpoint_url(&endpoint)
                    .force_path_style(true)
                    .retry_config(RetryConfig::standard().with_max_attempts(1))
                    .build(),
            );
            let hot = Arc::new(HotS3::new());
            *hot.client.lock().await = Some((
                ClientKey {
                    endpoint: Some(endpoint.clone()),
                    region: "us-east-1".into(),
                    environment_endpoint: std::env::var("AWS_ENDPOINT_URL_S3").ok(),
                    profile: std::env::var("AWS_PROFILE").ok(),
                },
                sdk,
            ));
            let storage = Storage {
                bucket: "fixture".into(),
                binary: "unused".into(),
                endpoint: Some(endpoint),
                region: "us-east-1".into(),
                hot,
            };
            let error = storage.download_bytes("block", 4096).await.unwrap_err();
            server.abort();
            assert_eq!(
                error.status, expected,
                "unexpected classification for {code}"
            );
        }
    }
}
