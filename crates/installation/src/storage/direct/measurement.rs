//! Opt-in component comparison. No provider, HTTP, manager relay or production
//! latency is measured; each cold trial starts with an empty real LazyDisk cache.
use crate::{
    nodes::snapshots::BLOCK,
    storage::{
        BlockSource, Disk, LazyDisk,
        block_codec::{self, BlockCodec},
        digest,
    },
    store::Store,
    vault::Vault,
};
use rand::{RngCore, SeedableRng};
use serde_json::{Value, json};
use std::{io, sync::Arc, time::Instant};

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

fn p50(values: &mut [f64]) -> f64 {
    values.sort_by(f64::total_cmp);
    values[values.len() / 2]
}

struct Source {
    vault: Vault,
    codec: Arc<BlockCodec>,
    key: [u8; 32],
    scope: String,
    encoded: Vec<u8>,
    direct: bool,
    runtime: tokio::runtime::Handle,
}

impl BlockSource for Source {
    fn fetch(&self, hash: &str) -> io::Result<Vec<u8>> {
        if self.direct {
            return block_codec::decode_direct(&self.key, &self.scope, hash, BLOCK, &self.encoded)
                .map_err(|e| io::Error::other(e.message));
        }
        self.runtime
            .block_on(self.codec.decode(
                self.vault.clone(),
                self.scope.clone(),
                hash.into(),
                Some(BLOCK),
                self.encoded.clone(),
            ))
            .map_err(|e| io::Error::other(e.message))
    }
}

#[tokio::test]
#[ignore = "component measurement; run alone with --nocapture --test-threads=1"]
async fn measure_v2_v3_codec_and_cached_reads() {
    let root = tempfile::tempdir().unwrap();
    let vault = Vault::new(Store::open(root.path()).unwrap(), root.path()).unwrap();
    let codec = Arc::new(BlockCodec::default());
    let mut sparse = vec![0; BLOCK as usize];
    sparse[..4096].fill(37);
    let text = include_bytes!("../../../tests/fixtures/block-codec-source.txt");
    let source: Vec<_> = text.iter().copied().cycle().take(BLOCK as usize).collect();
    let mut random = vec![0; BLOCK as usize];
    rand::rngs::StdRng::seed_from_u64(42).fill_bytes(&mut random);
    let mut mixed = source.clone();
    mixed[..BLOCK as usize / 2].copy_from_slice(&random[..BLOCK as usize / 2]);
    for (case, bytes) in [
        ("sparse", sparse),
        ("source", source),
        ("mixed", mixed),
        ("incompressible", random),
    ] {
        let hash = digest::block(&bytes);
        let manifest = json!({
            "version": 1,
            "size": BLOCK,
            "blockSize": BLOCK,
            "blocks": [{ "offset": 0, "size": BLOCK, "hash": hash }]
        });
        let mut rows: [Vec<Value>; 2] = Default::default();
        for trial in 0..18 {
            for direct in if trial % 2 == 0 {
                [false, true]
            } else {
                [true, false]
            } {
                let scope = format!(
                    "shared-blocks/v{}/{hash}/{}",
                    if direct { 3 } else { 2 },
                    crate::config::id()
                );
                let key = vault.storage_object_key(&scope);
                let input = bytes.clone();
                let started = Instant::now();
                let cpu = cpu_ms();
                let encoded = if direct {
                    let (scope, hash) = (scope.clone(), hash.clone());
                    tokio::task::spawn_blocking(move || {
                        block_codec::encode_direct(&key, &scope, &hash, &input)
                    })
                    .await
                    .unwrap()
                    .unwrap()
                } else {
                    codec
                        .encode(vault.clone(), scope.clone(), input)
                        .await
                        .unwrap()
                };
                let write = started.elapsed().as_secs_f64() * 1000.0;
                let write_cpu = cpu_ms() - cpu;
                let encoded_bytes = encoded.len();
                let directory = root.path().join(crate::config::id());
                let disk = LazyDisk::create(
                    &directory,
                    &manifest,
                    Arc::new(Source {
                        vault: vault.clone(),
                        codec: codec.clone(),
                        key,
                        scope,
                        encoded,
                        direct,
                        runtime: tokio::runtime::Handle::current(),
                    }),
                )
                .unwrap();
                let expected = bytes[..4096].to_vec();
                let (cold, cold_cpu, warm) = tokio::task::spawn_blocking(move || {
                    let mut output = [0; 4096];
                    let cpu = cpu_ms();
                    let start = Instant::now();
                    disk.read_at(0, &mut output).unwrap();
                    let cold = start.elapsed().as_secs_f64() * 1000.0;
                    let cold_cpu = cpu_ms() - cpu;
                    assert_eq!(output.as_slice(), expected);
                    let start = Instant::now();
                    for _ in 0..256 {
                        disk.read_at(0, &mut output).unwrap();
                    }
                    let warm = start.elapsed().as_secs_f64() * 1000.0 / 256.0;
                    assert_eq!(output.as_slice(), expected);
                    (cold, cold_cpu, warm)
                })
                .await
                .unwrap();
                std::fs::remove_dir_all(directory).unwrap();
                if trial >= 2 {
                    rows[usize::from(direct)].push(json!({
                        "write": write,
                        "writeCpu": write_cpu,
                        "cold": cold,
                        "coldCpu": cold_cpu,
                        "warm": warm,
                        "encodedBytes": encoded_bytes
                    }));
                }
            }
        }
        for (index, rows) in rows.iter().enumerate() {
            let mut result = json!({
                "case": case,
                "format": if index == 0 { "v2-reference" } else { "v3-direct" },
                "hash": hash,
                "plaintextBytes": BLOCK,
                "encodedBytes": rows[0]["encodedBytes"],
                "samples": rows.len(),
                "transport": "in-process-codec-and-real-lazy-disk"
            });
            for metric in ["write", "writeCpu", "cold", "coldCpu", "warm"] {
                let mut samples: Vec<_> = rows
                    .iter()
                    .map(|row| row[metric].as_f64().unwrap())
                    .collect();
                result[format!("{metric}P50Ms")] = p50(&mut samples).into();
            }
            println!("DIRECT_STORAGE_PERF {result}");
        }
    }
}
