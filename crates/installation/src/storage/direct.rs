//! Scoped S3 transfers on the execution node. Credentials and capabilities never
//! enter the guest, command line, diagnostics or persistent plaintext metadata.
use crate::{
    error::{Error, Result},
    nodes::{shared_blocks, snapshots},
    storage::block_codec,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use futures_util::{StreamExt, stream};
use md5::{Digest, Md5};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::Path,
    sync::{Arc, OnceLock},
    time::Duration,
};
use tokio::sync::Semaphore;

pub(crate) const BATCH: usize = 4;
pub(crate) const AUTH_SECONDS: u64 = 180;
pub(crate) const TRANSFER_SECONDS: u64 = 120;
pub(crate) const RESPONSE_HEADER: &str = "x-cairn-storage-transfer";

// Deliberately no Debug: both URLs and object keys are secret capabilities.
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Request {
    pub url: String,
    pub headers: BTreeMap<String, String>,
    #[serde(default)]
    pub installation_local: bool,
}

impl Request {
    pub(crate) fn signed(
        value: &aws_sdk_s3::presigning::PresignedRequest,
        installation_local: bool,
    ) -> Result<Self> {
        let request = Self {
            installation_local,
            url: value.uri().to_owned(),
            headers: value
                .headers()
                .map(|(k, v)| (k.to_owned(), v.to_owned()))
                .collect(),
        };
        request.target()?;
        Ok(request)
    }

    fn target(&self) -> Result<url::Url> {
        let url = url::Url::parse(&self.url)
            .map_err(|_| Error::unavailable("Invalid direct storage destination."))?;
        let loopback = url.host().is_some_and(|host| match host {
            url::Host::Domain(host) => host == "localhost",
            url::Host::Ipv4(ip) => ip.is_loopback(),
            url::Host::Ipv6(ip) => ip.is_loopback(),
        });
        // The bundled Garage is on the installation's private Compose network.
        // Only its colocated runner shares the manager-written origin marker.
        let integrated = self.installation_local
            && url.host_str() == Some("garage")
            && url.port() == Some(3900)
            && crate::nodes::node_data_dir()
                .join("manager-origin")
                .is_file();
        if !(url.scheme() == "https" || url.scheme() == "http" && (loopback || integrated))
            || !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
        {
            return Err(Error::unavailable(
                "Direct storage requires HTTPS reachable by this node; private Garage HTTP is limited to its installation's local runner.",
            ));
        }
        Ok(url)
    }

    fn apply(
        &self,
        client: &reqwest::Client,
        method: reqwest::Method,
    ) -> Result<reqwest::RequestBuilder> {
        let mut request = client.request(method, self.target()?);
        for (name, value) in &self.headers {
            request = request.header(name, value);
        }
        Ok(request)
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Read {
    pub object: String,
    pub hash: String,
    pub size: u64,
    pub key: String,
    pub expires_at: i64,
    pub get: Request,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Upload {
    pub read: Read,
    pub put: Request,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Batch {
    pub publication_id: String,
    pub objects: Vec<Upload>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Receipt {
    pub publication_id: String,
    pub objects: Vec<String>,
    pub plaintext_bytes: u64,
    pub encoded_bytes: u64,
}

fn client() -> Result<reqwest::Client> {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    if let Some(client) = CLIENT.get() {
        return Ok(client.clone());
    }
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(TRANSFER_SECONDS))
        .build()
        .map_err(Error::internal)?;
    Ok(CLIENT.get_or_init(|| client).clone())
}

fn scope(read: &Read) -> Result<(String, [u8; 32])> {
    crate::validation::uuid(&read.object)?;
    if !snapshots::valid_hash(&read.hash) || read.size == 0 || read.size > snapshots::BLOCK {
        return Err(Error::bad("Invalid direct storage authorization."));
    }
    if read.expires_at <= crate::config::now() {
        return Err(Error::unavailable(
            "Direct storage authorization expired; request a fresh ticket.",
        ));
    }
    let key = STANDARD
        .decode(&read.key)
        .ok()
        .and_then(|key| key.try_into().ok())
        .ok_or_else(|| Error::bad("Invalid scoped storage key."))?;
    Ok((shared_blocks::object_key(3, &read.hash, &read.object), key))
}

async fn encoded(client: &reqwest::Client, read: &Read) -> Result<Vec<u8>> {
    let response = read
        .get
        .apply(client, reqwest::Method::GET)?
        .send()
        .await
        .map_err(|_| {
            Error::unavailable("Direct storage download interrupted; no manager fallback.")
        })?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Err(Error::conflict("Direct storage object is missing."));
    }
    if !response.status().is_success() {
        return Err(Error::unavailable(
            "Direct storage block unavailable; no manager fallback.",
        ));
    }
    let mut response = response;
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| Error::unavailable("Direct storage body interrupted."))?
    {
        if chunk.len() as u64 > block_codec::MAX_ENCODED_BYTES.saturating_sub(bytes.len() as u64) {
            return Err(Error::bad("Direct storage object exceeds its size limit."));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

/// Called after the manager has authorized this exact manifest object. A direct
/// failure never switches to the legacy manager-to-node plaintext route.
pub(crate) async fn download(read: Read) -> Result<Vec<u8>> {
    // Reads have their own budget: a cold-read outage must not prevent uploads,
    // and a replay verification must not deadlock behind its own upload permit.
    static READS: OnceLock<Arc<Semaphore>> = OnceLock::new();
    let read_permit = READS
        .get_or_init(|| Arc::new(Semaphore::new(BATCH)))
        .clone()
        .acquire_owned()
        .await
        .map_err(Error::internal)?;
    // Invalid tickets or unreachable destinations are control-plane failures,
    // not evidence that the stored object is corrupt.
    let (scope, key) = scope(&read).map_err(|_| {
        Error::unavailable(
            "Invalid or expired direct storage authorization; request a fresh ticket.",
        )
    })?;
    let bytes = encoded(&client()?, &read).await?;
    tokio::task::spawn_blocking(move || {
        let _read = read_permit;
        block_codec::decode_direct(&key, &scope, &read.hash, read.size, &bytes)
    })
    .await
    .map_err(Error::internal)?
}

/// A detached route task owns this batch until every started PUT has completed.
/// The manager persists grants first; an ambiguous failure retains GC tombstones.
pub(crate) async fn upload_snapshot(directory: &Path, batch: Batch) -> Result<Receipt> {
    static JOBS: OnceLock<Semaphore> = OnceLock::new();
    crate::validation::uuid(&batch.publication_id)?;
    if batch.objects.is_empty() || batch.objects.len() > BATCH {
        return Err(Error::bad("Invalid direct publication batch."));
    }
    let client = client()?;
    let reader = Arc::new(snapshots::Reader::open(directory).await?);
    let mut jobs = stream::iter(batch.objects.into_iter().map(|upload| {
        let (client, reader) = (client.clone(), reader.clone());
        async move {
            let _permit = JOBS
                .get_or_init(|| Semaphore::new(BATCH))
                .acquire()
                .await
                .map_err(Error::internal)?;
            let (scope, key) = scope(&upload.read)?;
            if upload.put.headers.get("if-none-match").map(String::as_str) != Some("*") {
                return Err(Error::bad("Direct storage upload must be create-only."));
            }
            let bytes = reader.block(&upload.read.hash).await?;
            if bytes.len() as u64 != upload.read.size {
                return Err(Error::bad("Direct publication block size mismatch."));
            }
            let hash = upload.read.hash.clone();
            let encoded = tokio::task::spawn_blocking(move || {
                block_codec::encode_direct(&key, &scope, &hash, &bytes)
            })
            .await
            .map_err(Error::internal)??;
            let length = encoded.len() as u64;
            let checksum = STANDARD.encode(Md5::digest(&encoded));
            let response = upload
                .put
                .apply(&client, reqwest::Method::PUT)?
                .header("content-md5", checksum)
                .body(encoded)
                .send()
                .await
                .map_err(|_| {
                    Error::unavailable("Direct storage upload interrupted; local journal retained.")
                })?;
            if response.status() == reqwest::StatusCode::PRECONDITION_FAILED {
                // A lost earlier response may have committed. Never accept 412
                // merely because the key exists: authenticate and verify its bytes.
                download(upload.read.clone()).await?;
            } else if !response.status().is_success() {
                return Err(Error::unavailable(
                    "Direct storage upload failed; local journal retained, no manager fallback.",
                ));
            }
            Ok((upload.read.object, upload.read.size, length))
        }
    }))
    .buffer_unordered(BATCH);
    let mut receipt = Receipt {
        publication_id: batch.publication_id,
        objects: Vec::new(),
        plaintext_bytes: 0,
        encoded_bytes: 0,
    };
    let mut failure = None;
    while let Some(result) = jobs.next().await {
        match result {
            Ok((object, plain, encoded)) => {
                receipt.objects.push(object);
                receipt.plaintext_bytes += plain;
                receipt.encoded_bytes += encoded;
            }
            Err(error) => failure = Some(error),
        }
    }
    if let Some(error) = failure {
        return Err(error);
    }
    Ok(receipt)
}

#[cfg(test)]
mod measurement;

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn invalid_download_capabilities_do_not_report_object_corruption() {
        let mut read = Read {
            object: crate::config::id(),
            hash: crate::storage::digest::block(b"x"),
            size: 1,
            key: STANDARD.encode([7; 32]),
            expires_at: crate::config::now() + 180_000,
            get: Request {
                url: "http://storage.example/object".into(),
                headers: BTreeMap::new(),
                installation_local: false,
            },
        };
        assert_eq!(download(read.clone()).await.unwrap_err().status, 503);
        read.key = "invalid".into();
        assert_eq!(download(read).await.unwrap_err().status, 503);
    }

    #[test]
    fn direct_codec_authenticates_object_key_identity_size_and_version() {
        let bytes = vec![23; 8192];
        let hash = crate::storage::digest::block(&bytes);
        let key = [7; 32];
        let scope = "shared-blocks/v3/fixture/object";
        let encoded = block_codec::encode_direct(&key, scope, &hash, &bytes).unwrap();
        assert_eq!(
            block_codec::decode_direct(&key, scope, &hash, bytes.len() as u64, &encoded).unwrap(),
            bytes
        );
        assert!(encoded.len() < bytes.len());
        assert!(block_codec::decode_direct(&[8; 32], scope, &hash, 8192, &encoded).is_err());
        assert!(block_codec::decode_direct(&key, "another object", &hash, 8192, &encoded).is_err());
        assert!(block_codec::decode_direct(&key, scope, &hash, 4096, &encoded).is_err());
        assert!(
            block_codec::decode_direct(
                &key,
                scope,
                &crate::storage::digest::block(b"other"),
                8192,
                &encoded
            )
            .is_err()
        );
        assert!(block_codec::encode_direct(&key, scope, &hash, b"wrong").is_err());
        let mut corrupt = encoded.clone();
        *corrupt.last_mut().unwrap() ^= 1;
        assert!(block_codec::decode_direct(&key, scope, &hash, 8192, &corrupt).is_err());
        let mut downgraded = encoded;
        downgraded[6] = 2;
        assert!(block_codec::decode_direct(&key, scope, &hash, 8192, &downgraded).is_err());
    }

    #[test]
    fn storage_keys_are_scoped_and_do_not_disclose_the_vault_key() {
        let root = tempfile::tempdir().unwrap();
        let store = crate::store::Store::open(root.path()).unwrap();
        let vault = crate::vault::Vault::new(store, root.path()).unwrap();
        let a = vault.storage_object_key("a");
        let b = vault.storage_object_key("b");
        assert_ne!(a, b);
        assert_eq!(a, vault.storage_object_key("a"));
        assert_ne!(
            a.as_slice(),
            std::fs::read(root.path().join("mcp-encryption-key")).unwrap()
        );
    }

    #[test]
    fn expired_capabilities_retry_without_reporting_object_corruption() {
        let read = Read {
            object: crate::config::id(),
            hash: crate::storage::digest::block(b"x"),
            size: 1,
            key: STANDARD.encode([7; 32]),
            expires_at: 0,
            get: Request {
                url: "https://storage.example/object".into(),
                headers: BTreeMap::new(),
                installation_local: false,
            },
        };
        assert_eq!(scope(&read).unwrap_err().status, 503);
    }

    #[test]
    fn direct_destinations_reject_plaintext_remote_urls_and_credentials() {
        for url in [
            "http://storage.example/object",
            "https://user:secret@storage.example/object",
            "https://storage.example/object#fragment",
        ] {
            assert!(
                Request {
                    installation_local: false,
                    url: url.into(),
                    headers: BTreeMap::new()
                }
                .target()
                .is_err()
            );
        }
        assert!(
            Request {
                installation_local: false,
                url: "https://storage.example/object?signature=fixture".into(),
                headers: BTreeMap::new()
            }
            .target()
            .is_ok()
        );
        assert!(
            Request {
                installation_local: false,
                url: "http://[::1]:3900/object".into(),
                headers: BTreeMap::new()
            }
            .target()
            .is_ok()
        );
    }
}
