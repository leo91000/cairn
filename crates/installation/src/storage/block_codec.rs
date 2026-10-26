//! Authenticated S3 block representations; content identity always names decoded bytes.
//! CPU work runs off Tokio's executor, with permits retained even if a caller cancels.
use super::{digest, metrics::Counter};
use crate::{
    error::{Error, Result},
    nodes::snapshots::BLOCK,
    vault::Vault,
};
use serde_json::{Value, json};
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering::Relaxed},
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

const V2: &[u8; 8] = b"CAIRNB\x02\0";
const RAW: u8 = 0;
const ZSTD: u8 = 1;
const PAYLOAD_HEADER: usize = 5;

/// Eight version bytes, twelve nonce bytes, sixteen tag bytes, codec and u32 length.
pub const MAX_ENCODED_BYTES: u64 = BLOCK + 8 + 28 + PAYLOAD_HEADER as u64;
const CPU_CONCURRENCY: usize = 4;
const READ_CONCURRENCY: usize = 16;

#[derive(Default)]
struct Metrics {
    admission: Counter,
    read_admission: Counter,
    encode: Counter,
    decode: Counter,
    encoded_bytes: AtomicU64,
    decoded_input_bytes: AtomicU64,
    raw_writes: AtomicU64,
    zstd_writes: AtomicU64,
    raw_reads: AtomicU64,
    zstd_reads: AtomicU64,
}

/// One bounded CPU budget and aggregate read/write measurements per manager.
pub struct BlockCodec {
    workers: Arc<Semaphore>,
    reads: Arc<Semaphore>,
    metrics: Arc<Metrics>,
}

impl Default for BlockCodec {
    fn default() -> Self {
        Self {
            workers: Arc::new(Semaphore::new(CPU_CONCURRENCY)),
            reads: Arc::new(Semaphore::new(READ_CONCURRENCY)),
            metrics: Arc::new(Metrics::default()),
        }
    }
}

impl BlockCodec {
    /// Counters are cumulative and contain no content, hashes, paths or credentials.
    pub fn performance(&self) -> Value {
        json!({
            "admission": self.metrics.admission.snapshot(),
            "readAdmission": self.metrics.read_admission.snapshot(),
            "encode": self.metrics.encode.snapshot(),
            "decode": self.metrics.decode.snapshot(),
            "encodedBytes": self.metrics.encoded_bytes.load(Relaxed),
            "decodedInputBytes": self.metrics.decoded_input_bytes.load(Relaxed),
            "rawWrites": self.metrics.raw_writes.load(Relaxed),
            "zstdWrites": self.metrics.zstd_writes.load(Relaxed),
            "rawReads": self.metrics.raw_reads.load(Relaxed),
            "zstdReads": self.metrics.zstd_reads.load(Relaxed)
        })
    }

    pub async fn encode(&self, vault: Vault, scope: String, bytes: Vec<u8>) -> Result<Vec<u8>> {
        let admission = self.metrics.admission.start();
        let permit = self
            .workers
            .clone()
            .acquire_owned()
            .await
            .map_err(Error::internal)?;
        admission.finish(0);
        let metrics = self.metrics.clone();

        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let sample = metrics.encode.start();
            let (encoded, codec) = encode(&vault, &scope, &bytes)?;
            metrics
                .encoded_bytes
                .fetch_add(encoded.len() as u64, Relaxed);
            let formats = if codec == ZSTD {
                &metrics.zstd_writes
            } else {
                &metrics.raw_writes
            };
            formats.fetch_add(1, Relaxed);
            sample.finish(bytes.len());
            Ok(encoded)
        })
        .await
        .map_err(Error::internal)?
    }

    /// The size from an authenticated manifest, when available, is checked before inflation.
    pub async fn decode(
        &self,
        vault: Vault,
        scope: String,
        hash: String,
        expected_size: Option<u64>,
        encoded: Vec<u8>,
    ) -> Result<Vec<u8>> {
        let permit = self.read_permit().await?;
        self.decode_reserved(vault, scope, hash, expected_size, encoded, permit)
            .await
    }

    /// Shared recovery acquires this before downloading a payload. The budget
    /// includes the CPU queue, so completed GETs cannot accumulate unbounded bytes.
    /// It is independent of CPU admission: a blocked GET never occupies a CPU slot.
    pub(crate) async fn read_permit(&self) -> Result<OwnedSemaphorePermit> {
        let sample = self.metrics.read_admission.start();
        let permit = self
            .reads
            .clone()
            .acquire_owned()
            .await
            .map_err(Error::internal)?;
        sample.finish(0);
        Ok(permit)
    }

    pub(crate) async fn decode_reserved(
        &self,
        vault: Vault,
        scope: String,
        hash: String,
        expected_size: Option<u64>,
        encoded: Vec<u8>,
        read_permit: OwnedSemaphorePermit,
    ) -> Result<Vec<u8>> {
        let admission = self.metrics.admission.start();
        let permit = self
            .workers
            .clone()
            .acquire_owned()
            .await
            .map_err(Error::internal)?;
        admission.finish(0);
        let metrics = self.metrics.clone();

        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let _read_permit = read_permit;
            let sample = metrics.decode.start();
            let (bytes, codec) = decode(&vault, &scope, &hash, expected_size, &encoded)
                .map_err(|_| Error::conflict("Recovery block integrity check failed."))?;
            metrics
                .decoded_input_bytes
                .fetch_add(encoded.len() as u64, Relaxed);
            let formats = match codec {
                ZSTD => &metrics.zstd_reads,
                _ => &metrics.raw_reads,
            };
            formats.fetch_add(1, Relaxed);
            sample.finish(bytes.len());
            Ok(bytes)
        })
        .await
        .map_err(Error::internal)?
    }
}

// The domain binds the outer version as well as the immutable object locator.
// Replacing a v2 header with v1 cannot reinterpret authenticated compressed bytes.
fn v2_scope(scope: &str) -> String {
    format!("disk-block:v2:{scope}")
}

fn valid_size(size: u64) -> Result<usize> {
    if size == 0 || size > BLOCK {
        return Err(Error::bad("Invalid disk block size."));
    }
    Ok(size as usize)
}

fn encode(vault: &Vault, scope: &str, bytes: &[u8]) -> Result<(Vec<u8>, u8)> {
    valid_size(bytes.len() as u64)?;
    let compressed = zstd::bulk::compress(bytes, 3)?;
    let (codec, data) = if compressed.len() < bytes.len() {
        (ZSTD, compressed.as_slice())
    } else {
        (RAW, bytes)
    };
    let mut payload = Vec::with_capacity(PAYLOAD_HEADER + data.len());
    payload.push(codec);
    payload.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    payload.extend_from_slice(data);

    let encrypted = vault.encrypt_bytes(&v2_scope(scope), &payload)?;
    let mut encoded = Vec::with_capacity(V2.len() + encrypted.len());
    encoded.extend_from_slice(V2);
    encoded.extend_from_slice(&encrypted);
    Ok((encoded, codec))
}

fn decode(
    vault: &Vault,
    scope: &str,
    hash: &str,
    expected_size: Option<u64>,
    encoded: &[u8],
) -> Result<(Vec<u8>, u8)> {
    if let Some(size) = expected_size {
        valid_size(size)?;
    }
    if !encoded.starts_with(V2) || encoded.len() as u64 > MAX_ENCODED_BYTES || !digest::valid(hash)
    {
        return Err(Error::bad("Unsupported disk block envelope."));
    }
    let ciphertext = &encoded[V2.len()..];
    let (bytes, codec) = {
        let payload = vault.decrypt_bytes(&v2_scope(scope), ciphertext)?;
        let header = payload
            .get(..PAYLOAD_HEADER)
            .ok_or_else(|| Error::bad("Invalid disk block header."))?;
        let size = valid_size(u32::from_le_bytes(header[1..5].try_into().unwrap()) as u64)?;
        if expected_size.is_some_and(|expected| expected != size as u64) {
            return Err(Error::bad("Disk block size mismatch."));
        }
        let data = &payload[PAYLOAD_HEADER..];
        let bytes = match header[0] {
            RAW if data.len() == size => data.to_vec(),
            ZSTD => decompress(data, size)?,
            _ => return Err(Error::bad("Invalid disk block codec or length.")),
        };
        (bytes, header[0])
    };

    valid_size(bytes.len() as u64)?;
    if expected_size.is_some_and(|expected| expected != bytes.len() as u64)
        || !digest::matches(hash, &bytes)
    {
        return Err(Error::bad("Backup integrity check failed."));
    }
    Ok((bytes, codec))
}

fn decompress(data: &[u8], size: usize) -> Result<Vec<u8>> {
    // Accept exactly one ordinary frame with a known bounded content size.
    // Reject skippable frames, concatenation, trailing bytes and oversized output.
    if !data.starts_with(&[0x28, 0xb5, 0x2f, 0xfd])
        || zstd::zstd_safe::find_frame_compressed_size(data).ok() != Some(data.len())
        || zstd::zstd_safe::get_frame_content_size(data).ok() != Some(Some(size as u64))
    {
        return Err(Error::bad("Invalid compressed disk block."));
    }
    // Bulk decoding uses the bounded destination as history. Unlike streaming,
    // it does not allocate a window from the frame's advertised window size.
    let mut decoder = zstd::bulk::Decompressor::new()?;
    let bytes = decoder.decompress(data, size)?;
    if bytes.len() != size {
        return Err(Error::bad("Disk block size mismatch."));
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Store;
    use rand::{RngCore, SeedableRng};

    fn fixture() -> (tempfile::TempDir, Vault) {
        let root = tempfile::tempdir().unwrap();
        let vault = Vault::new(Store::open(root.path()).unwrap(), root.path()).unwrap();
        (root, vault)
    }

    fn authenticated(vault: &Vault, codec: u8, size: u32, data: &[u8]) -> Vec<u8> {
        let mut payload = vec![codec];
        payload.extend_from_slice(&size.to_le_bytes());
        payload.extend_from_slice(data);
        let mut encoded = V2.to_vec();
        encoded.extend(vault.encrypt_bytes(&v2_scope("object"), &payload).unwrap());
        encoded
    }

    #[test]
    fn round_trips_full_partial_and_tiny_blocks() {
        let (_root, vault) = fixture();
        for size in [1, 4096, BLOCK as usize - 13, BLOCK as usize] {
            let bytes = vec![37; size];
            {
                let hash = digest::block(&bytes);
                let (encoded, _) = encode(&vault, "object", &bytes).unwrap();
                assert!(encoded.len() as u64 <= MAX_ENCODED_BYTES);
                assert_eq!(
                    decode(&vault, "object", &hash, Some(size as u64), &encoded)
                        .unwrap()
                        .0,
                    bytes
                );
            }
        }
        assert!(encode(&vault, "object", &[]).is_err());
        assert!(encode(&vault, "object", &vec![0; BLOCK as usize + 1]).is_err());
    }

    #[test]
    fn compression_saves_space_and_incompressible_blocks_fall_back_to_raw() {
        let (_root, vault) = fixture();
        let bytes = vec![7; BLOCK as usize];
        let (encoded, codec) = encode(&vault, "object", &bytes).unwrap();
        assert_eq!(codec, ZSTD);
        assert!(encoded.len() < 4096);

        let mut random = vec![0; BLOCK as usize];
        rand::rngs::StdRng::seed_from_u64(42).fill_bytes(&mut random);
        let (encoded, codec) = encode(&vault, "object", &random).unwrap();
        assert_eq!(codec, RAW);
        assert_eq!(encoded.len() as u64, MAX_ENCODED_BYTES);
        assert_eq!(
            decode(&vault, "object", &digest::block(&random), None, &encoded)
                .unwrap()
                .0,
            random
        );
    }

    #[test]
    fn reads_independent_v2_raw_and_zstd_golden_envelopes() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join("mcp-encryption-key"),
            (0u8..32).collect::<Vec<_>>(),
        )
        .unwrap();
        let vault = Vault::new(Store::open(root.path()).unwrap(), root.path()).unwrap();
        // Generated with Python AESGCM and zstandard, using the documented v2 format.
        let fixtures = [
            (
                "434149524e42020018191a1b1c1d1e1f20212223e6ae9629423fd290fad1e110b68b0f8d130b4010db210c3c756ccfa030baac6e7490892dbb259a7608",
                b"historical disk work".to_vec(),
            ),
            (
                "434149524e4202002425262728292a2b2c2d2e2f199786a5e0abaac2ea83e3139b2e2a84f28fb3de2366e806c487651a58944605c4917aaaadfad32e",
                vec![7; 4096],
            ),
        ];
        for (encoded, bytes) in fixtures {
            assert_eq!(
                decode(
                    &vault,
                    "object",
                    &digest::block(&bytes),
                    Some(bytes.len() as u64),
                    &hex::decode(encoded).unwrap()
                )
                .unwrap()
                .0,
                bytes
            );
        }
    }

    #[test]
    fn authenticates_version_locator_codec_and_length_then_verifies_plaintext() {
        let (_root, vault) = fixture();
        let bytes = vec![3; 4096];
        let hash = digest::block(&bytes);
        let (encoded, _) = encode(&vault, "object", &bytes).unwrap();
        assert!(decode(&vault, "other-object", &hash, None, &encoded).is_err());
        assert!(decode(&vault, "object", &digest::block(b"other"), None, &encoded).is_err());
        assert!(decode(&vault, "object", &hash, Some(4095), &encoded).is_err());
        for offset in [6, 8, 20, encoded.len() - 1] {
            let mut corrupt = encoded.clone();
            corrupt[offset] ^= 1;
            assert!(decode(&vault, "object", &hash, None, &corrupt).is_err());
        }
        let mut downgrade = encoded.clone();
        downgrade[..8].copy_from_slice(b"CAIRNB\x01\0");
        assert!(decode(&vault, "object", &hash, None, &downgrade).is_err());
        let mut upgrade = V2.to_vec();
        upgrade.extend(vault.encrypt_bytes("object", &bytes).unwrap());
        assert!(decode(&vault, "object", &hash, None, &upgrade).is_err());
        for length in [0, 7, 8, 35, encoded.len() - 1] {
            assert!(decode(&vault, "object", &hash, None, &encoded[..length]).is_err());
        }
    }

    #[test]
    fn rejects_authenticated_bombs_bad_lengths_unknown_codecs_and_noncanonical_frames() {
        let (_root, vault) = fixture();
        let bytes = vec![7; 4096];
        let hash = digest::block(&bytes);
        let compressed = zstd::bulk::compress(&bytes, 3).unwrap();
        let mut trailing = compressed.clone();
        trailing.push(0);
        let concatenated = [compressed.as_slice(), compressed.as_slice()].concat();
        let unknown_size = zstd::stream::encode_all(bytes.as_slice(), 3).unwrap();
        let oversized = zstd::bulk::compress(&vec![0; BLOCK as usize + 1], 3).unwrap();
        let skippable = [0x50, 0x2a, 0x4d, 0x18, 0, 0, 0, 0];
        for (codec, size, data) in [
            (99, 4096, compressed.as_slice()),
            (RAW, 4095, bytes.as_slice()),
            (RAW, 4097, bytes.as_slice()),
            (RAW, 0, &[]),
            (RAW, u32::MAX, &[]),
            (ZSTD, 4096, &[]),
            (ZSTD, 4095, compressed.as_slice()),
            (ZSTD, BLOCK as u32, oversized.as_slice()),
            (ZSTD, 4096, unknown_size.as_slice()),
            (ZSTD, 4096, trailing.as_slice()),
            (ZSTD, 4096, concatenated.as_slice()),
            (ZSTD, 4096, skippable.as_slice()),
        ] {
            let encoded = authenticated(&vault, codec, size, data);
            assert!(
                decode(&vault, "object", &hash, None, &encoded).is_err(),
                "codec={codec} size={size}"
            );
        }
        let mut no_header = V2.to_vec();
        no_header.extend(vault.encrypt_bytes(&v2_scope("object"), &[RAW]).unwrap());
        assert!(decode(&vault, "object", &hash, None, &no_header).is_err());
        assert!(
            decode(
                &vault,
                "object",
                &hash,
                None,
                &vec![0; MAX_ENCODED_BYTES as usize + 1]
            )
            .is_err()
        );
    }

    #[tokio::test]
    async fn parallel_work_is_bounded_and_errors_are_counted_without_success_bytes() {
        let (_root, vault) = fixture();
        let codec = Arc::new(BlockCodec::default());
        let mut tasks = tokio::task::JoinSet::new();
        for _ in 0..16 {
            let (codec, vault) = (codec.clone(), vault.clone());
            tasks.spawn(async move {
                let bytes = vec![42; BLOCK as usize];
                let encoded = codec
                    .encode(vault.clone(), "object".into(), bytes.clone())
                    .await
                    .unwrap();
                assert_eq!(
                    codec
                        .decode(
                            vault,
                            "object".into(),
                            digest::block(&bytes),
                            Some(BLOCK),
                            encoded
                        )
                        .await
                        .unwrap(),
                    bytes
                );
            });
        }
        while let Some(task) = tasks.join_next().await {
            task.unwrap();
        }
        assert_eq!(
            codec
                .decode(vault, "object".into(), digest::block(b"a"), None, vec![])
                .await
                .unwrap_err()
                .status,
            409
        );
        let metrics = codec.performance();
        assert_eq!(metrics["encode"]["count"], 16);
        assert_eq!(metrics["decode"]["count"], 17);
        assert_eq!(metrics["decode"]["errors"], 1);
        assert_eq!(metrics["decode"]["bytes"], 16 * BLOCK);
        assert_eq!(metrics["zstdWrites"], 16);
        assert_eq!(metrics["zstdReads"], 16);
        assert!(metrics["encode"]["maxInFlight"].as_u64().unwrap() <= CPU_CONCURRENCY as u64);
        assert!(metrics["decode"]["maxInFlight"].as_u64().unwrap() <= CPU_CONCURRENCY as u64);
        assert_eq!(codec.workers.available_permits(), CPU_CONCURRENCY);
        assert_eq!(codec.reads.available_permits(), READ_CONCURRENCY);
    }

    #[tokio::test]
    async fn saturated_recovery_admission_does_not_block_encoding_and_cancelled_waiters_drain() {
        let (_root, vault) = fixture();
        let codec = Arc::new(BlockCodec::default());
        let held = codec
            .reads
            .clone()
            .acquire_many_owned(READ_CONCURRENCY as u32)
            .await
            .unwrap();
        let bytes = vec![7; 4096];
        let hash = digest::block(&bytes);
        let encoded = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            codec.encode(vault.clone(), "object".into(), bytes.clone()),
        )
        .await
        .unwrap()
        .unwrap();
        let (worker, secret, identity, input) =
            (codec.clone(), vault.clone(), hash.clone(), encoded.clone());
        let waiting = tokio::spawn(async move {
            worker
                .decode(secret, "object".into(), identity, None, input)
                .await
        });
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while codec.performance()["readAdmission"]["inFlight"] != 1 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        waiting.abort();
        assert!(waiting.await.unwrap_err().is_cancelled());
        assert_eq!(codec.performance()["readAdmission"]["errors"], 1);
        assert_eq!(codec.workers.available_permits(), CPU_CONCURRENCY);
        drop(held);
        assert_eq!(
            codec
                .decode(vault, "object".into(), hash, None, encoded)
                .await
                .unwrap(),
            bytes
        );
        assert_eq!(codec.reads.available_permits(), READ_CONCURRENCY);
    }

    #[test]
    fn cancelling_a_caller_keeps_its_queued_blocking_job_within_the_cpu_budget() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .max_blocking_threads(1)
            .build()
            .unwrap();
        let (_root, vault) = fixture();
        let bytes = vec![7; 4096];
        let hash = digest::block(&bytes);
        let encoded = encode(&vault, "object", &bytes).unwrap().0;
        runtime.block_on(async {
            let (entered, ready) = tokio::sync::oneshot::channel();
            let (release, wait) = std::sync::mpsc::channel();
            let occupied = tokio::task::spawn_blocking(move || {
                entered.send(()).unwrap();
                wait.recv().unwrap();
            });
            ready.await.unwrap();
            let codec = Arc::new(BlockCodec::default());
            let worker = codec.clone();
            let caller = tokio::spawn(async move {
                worker
                    .decode(vault, "object".into(), hash, None, encoded)
                    .await
            });
            tokio::time::timeout(std::time::Duration::from_secs(5), async {
                while codec.workers.available_permits() == CPU_CONCURRENCY {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            caller.abort();
            assert!(caller.await.unwrap_err().is_cancelled());
            assert_eq!(codec.workers.available_permits(), CPU_CONCURRENCY - 1);
            assert_eq!(codec.reads.available_permits(), READ_CONCURRENCY - 1);
            assert_eq!(codec.performance()["decode"]["count"], 0);
            release.send(()).unwrap();
            occupied.await.unwrap();
            tokio::time::timeout(std::time::Duration::from_secs(5), async {
                while codec.workers.available_permits() != CPU_CONCURRENCY {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            assert_eq!(codec.performance()["decode"]["count"], 1);
            assert_eq!(codec.performance()["decode"]["errors"], 0);
            assert_eq!(codec.reads.available_permits(), READ_CONCURRENCY);
        });
    }
}
