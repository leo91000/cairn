//! Opt-in measurements against the exact v1 reference and the shipped v2 codec.
//! The S3 launcher supplies a loopback server and synthetic credentials only.
mod common;

use cairn_installation::{
    error::Result,
    nodes::snapshots::BLOCK,
    object_storage::Storage,
    service::Service,
    storage::{
        BlockSource, Disk, LazyDisk,
        block_codec::{BlockCodec, MAX_ENCODED_BYTES},
        digest,
    },
    store::Store,
    vault::Vault,
};
use rand::{RngCore, SeedableRng};
use serde_json::{Value, json};
use std::{
    io,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Instant,
};

const SAMPLES: usize = 16;
const WARMUPS: usize = 2;

fn corpus() -> Vec<(&'static str, Vec<u8>)> {
    let mut sparse = vec![0; BLOCK as usize];
    sparse[..4096].fill(37);
    // Freeze this fixture so changes to the implementation keep the same workload.
    let text = include_bytes!("fixtures/block-codec-source.txt");
    let source = text
        .iter()
        .copied()
        .cycle()
        .take(BLOCK as usize)
        .collect::<Vec<_>>();
    let mut random = vec![0; BLOCK as usize];
    rand::rngs::StdRng::seed_from_u64(42).fill_bytes(&mut random);
    let mut mixed = source.clone();
    mixed[..BLOCK as usize / 2].copy_from_slice(&random[..BLOCK as usize / 2]);
    vec![
        ("sparse", sparse),
        ("source", source),
        ("mixed", mixed),
        ("incompressible", random),
    ]
}

async fn encode(
    codec: &BlockCodec,
    vault: &Vault,
    scope: &str,
    bytes: Vec<u8>,
    compressed: bool,
) -> Vec<u8> {
    if compressed {
        codec
            .encode(vault.clone(), scope.to_owned(), bytes)
            .await
            .unwrap()
    } else {
        let (vault, scope) = (vault.clone(), scope.to_owned());
        tokio::task::spawn_blocking(move || {
            let mut encoded = b"CAIRNB\x01\0".to_vec();
            encoded.extend(vault.encrypt_bytes(&scope, &bytes).unwrap());
            encoded
        })
        .await
        .unwrap()
    }
}

async fn decode(
    codec: &BlockCodec,
    vault: &Vault,
    scope: &str,
    hash: &str,
    encoded: Vec<u8>,
    compressed: bool,
) -> Result<Vec<u8>> {
    if compressed {
        codec
            .decode(
                vault.clone(),
                scope.to_owned(),
                hash.to_owned(),
                Some(BLOCK),
                encoded,
            )
            .await
    } else {
        let (vault, scope, hash) = (vault.clone(), scope.to_owned(), hash.to_owned());
        tokio::task::spawn_blocking(move || {
            let bytes = vault.decrypt_bytes(&scope, &encoded[8..])?;
            assert_eq!(bytes.len() as u64, BLOCK);
            assert!(digest::matches(&hash, &bytes));
            Ok(bytes)
        })
        .await
        .unwrap()
    }
}

fn cpu_ms() -> f64 {
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::uninit();
    assert_eq!(
        unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) },
        0
    );
    let usage = unsafe { usage.assume_init() };
    (usage.ru_utime.tv_sec + usage.ru_stime.tv_sec) as f64 * 1000.0
        + (usage.ru_utime.tv_usec + usage.ru_stime.tv_usec) as f64 / 1000.0
}

fn summary(samples: &[f64]) -> Value {
    let mut ordered = samples.to_vec();
    ordered.sort_by(f64::total_cmp);
    json!({
        "samples": ordered.len(),
        "p50Ms": ordered[ordered.len() / 2],
        "p95Ms": ordered[(ordered.len() * 95 / 100).min(ordered.len() - 1)],
        "meanMs": ordered.iter().sum::<f64>() / ordered.len() as f64
    })
}

#[derive(Default)]
struct Measurements {
    write: Vec<f64>,
    read: Vec<f64>,
    cpu_write: Vec<f64>,
    cpu_read: Vec<f64>,
    warm: Vec<f64>,
    encoded_bytes: usize,
}

impl Measurements {
    fn record(&self, transport: &str, case: &str, hash: &str, compressed: bool) {
        println!(
            "BLOCK_CODEC_PERF {}",
            json!({
                "transport": transport,
                "case": case,
                "inputHash": hash,
                "format": if compressed { "v2-zstd-or-raw" } else { "v1-raw-reference" },
                "plaintextBytes": BLOCK,
                "encodedBytes": self.encoded_bytes,
                "write": summary(&self.write),
                "read": summary(&self.read),
                "processWriteCpu": summary(&self.cpu_write),
                "processReadCpu": summary(&self.cpu_read),
                "warm4KiB": if self.warm.is_empty() { Value::Null } else { summary(&self.warm) }
            })
        );
    }
}

#[tokio::test]
#[ignore = "explicit release benchmark; run alone with --nocapture --test-threads=1"]
async fn native_codec_read_write_reference() {
    let root = tempfile::tempdir().unwrap();
    let vault = Vault::new(Store::open(root.path()).unwrap(), root.path()).unwrap();
    let codec = BlockCodec::default();
    for (case, bytes) in corpus() {
        let hash = digest::block(&bytes);
        let mut results = [Measurements::default(), Measurements::default()];
        for trial in 0..SAMPLES + WARMUPS {
            for compressed in if trial % 2 == 0 {
                [false, true]
            } else {
                [true, false]
            } {
                let input = bytes.clone();
                let cpu = cpu_ms();
                let start = Instant::now();
                let encoded = encode(&codec, &vault, "benchmark", input, compressed).await;
                let write_ms = start.elapsed().as_secs_f64() * 1000.0;
                let write_cpu = cpu_ms() - cpu;
                let length = encoded.len();
                let cpu = cpu_ms();
                let start = Instant::now();
                let decoded = decode(&codec, &vault, "benchmark", &hash, encoded, compressed)
                    .await
                    .unwrap();
                let read_ms = start.elapsed().as_secs_f64() * 1000.0;
                let read_cpu = cpu_ms() - cpu;
                assert_eq!(decoded, bytes);
                if trial >= WARMUPS {
                    let result = &mut results[usize::from(compressed)];
                    result.write.push(write_ms);
                    result.read.push(read_ms);
                    result.cpu_write.push(write_cpu);
                    result.cpu_read.push(read_cpu);
                    result.encoded_bytes = length;
                }
            }
        }
        for (index, result) in results.iter().enumerate() {
            result.record("native-codec", case, &hash, index == 1);
        }
    }
    println!("BLOCK_CODEC_COUNTERS {}", codec.performance());
}

struct Source {
    storage: Storage,
    vault: Vault,
    codec: Arc<BlockCodec>,
    scope: String,
    runtime: tokio::runtime::Handle,
    compressed: bool,
    fetches: Arc<AtomicUsize>,
}

impl BlockSource for Source {
    fn fetch(&self, hash: &str) -> io::Result<Vec<u8>> {
        self.fetches.fetch_add(1, Ordering::Relaxed);
        self.runtime
            .block_on(async {
                let encoded = self
                    .storage
                    .download_bytes(&self.scope, MAX_ENCODED_BYTES)
                    .await?;
                decode(
                    &self.codec,
                    &self.vault,
                    &self.scope,
                    hash,
                    encoded,
                    self.compressed,
                )
                .await
            })
            .map_err(|error| io::Error::other(error.message))
    }
}

#[tokio::test]
#[ignore = "requires isolated loopback S3; run scripts/tests/node_s3_test.py --compression-benchmark"]
async fn loopback_s3_write_and_cold_warm_disk_read_reference() {
    let endpoint = std::env::var("CAIRN_NODE_TEST_S3_ENDPOINT").unwrap();
    let url = url::Url::parse(&endpoint).unwrap();
    assert_eq!(url.host_str(), Some("127.0.0.1"));
    let root = tempfile::tempdir().unwrap();
    let s = Service::new(common::config(root.path())).await.unwrap();
    std::fs::write(
        s.config.data_dir.join("storage-s3.json"),
        json!({ "bucket": "cairn-node-test" }).to_string(),
    )
    .unwrap();
    let storage = Storage::configured(&s).unwrap();
    let codec = Arc::new(BlockCodec::default());
    for (case, bytes) in corpus() {
        let hash = digest::block(&bytes);
        let manifest = json!({
            "version": 1,
            "size": BLOCK,
            "blockSize": BLOCK,
            "blocks": [{ "offset": 0, "size": BLOCK, "hash": hash }]
        });
        let mut results = [Measurements::default(), Measurements::default()];
        for trial in 0..SAMPLES + WARMUPS {
            for compressed in if trial % 2 == 0 {
                [false, true]
            } else {
                [true, false]
            } {
                let object = cairn_installation::config::id();
                let scope = format!("shared-blocks/v1/{hash}/{object}");
                let input = bytes.clone();
                let cpu = cpu_ms();
                let start = Instant::now();
                let encoded = encode(&codec, &s.vault, &scope, input, compressed).await;
                let length = encoded.len();
                storage.upload_bytes(encoded, &scope).await.unwrap();
                let write_ms = start.elapsed().as_secs_f64() * 1000.0;
                let write_cpu = cpu_ms() - cpu;

                let fetches = Arc::new(AtomicUsize::new(0));
                let source = Arc::new(Source {
                    storage: storage.clone(),
                    vault: s.vault.clone(),
                    codec: codec.clone(),
                    scope: scope.clone(),
                    runtime: tokio::runtime::Handle::current(),
                    compressed,
                    fetches: fetches.clone(),
                });
                // Every trial starts with an empty local disk and decoded block cache.
                let directory = root.path().join(&object);
                let disk = LazyDisk::create(&directory, &manifest, source).unwrap();
                let expected = bytes[..4096].to_vec();
                let (read_ms, read_cpu, warm_ms) = tokio::task::spawn_blocking(move || {
                    let mut output = vec![0; 4096];
                    let cpu = cpu_ms();
                    let start = Instant::now();
                    disk.read_at(0, &mut output).unwrap();
                    let read_ms = start.elapsed().as_secs_f64() * 1000.0;
                    let read_cpu = cpu_ms() - cpu;
                    assert_eq!(output, expected);
                    let start = Instant::now();
                    for _ in 0..256 {
                        disk.read_at(0, &mut output).unwrap();
                        assert_eq!(output, expected);
                    }
                    (
                        read_ms,
                        read_cpu,
                        start.elapsed().as_secs_f64() * 1000.0 / 256.0,
                    )
                })
                .await
                .unwrap();
                assert_eq!(
                    fetches.load(Ordering::Relaxed),
                    1,
                    "Exactly one cold fetch and no warm fetches"
                );
                storage.purge_key(&scope).await.unwrap();
                std::fs::remove_dir_all(directory).unwrap();
                if trial >= WARMUPS {
                    let result = &mut results[usize::from(compressed)];
                    result.write.push(write_ms);
                    result.read.push(read_ms);
                    result.cpu_write.push(write_cpu);
                    result.cpu_read.push(read_cpu);
                    result.warm.push(warm_ms);
                    result.encoded_bytes = length;
                }
            }
        }
        for (index, result) in results.iter().enumerate() {
            result.record("loopback-s3-and-lazy-disk", case, &hash, index == 1);
        }
    }
    println!("BLOCK_CODEC_COUNTERS {}", codec.performance());
    s.shutdown.cancel();
}
