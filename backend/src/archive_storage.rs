//! S3 operations stay server-side. The AWS CLI supplies signing and multipart transfer.
use crate::{
    error::{Error, Result},
    service::Service,
};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::process::Command;

#[derive(Clone)]
pub struct Storage {
    pub bucket: String,
    binary: String,
    /// S3-compatible providers (OVHcloud, Scaleway…) need an explicit endpoint.
    pub(crate) endpoint: Option<String>,
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
