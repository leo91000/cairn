//! Run with `cargo run --locked --release --example storage-benchmark`.
//! Uses real LazyDisk reads and durable writes, without network or VM overhead.
use leo_agent_manager::storage::{BlockSource, Disk, LazyDisk, LocalDisk, digest};
use serde_json::json;
use std::{
    hint::black_box,
    io::{self, Write},
    sync::Arc,
    time::Instant,
};

struct Offline;

impl BlockSource for Offline {
    fn fetch(&self, _: &str) -> io::Result<Vec<u8>> {
        Err(io::Error::other("Benchmark must not use the network"))
    }
}

fn legacy_hash(bytes: &[u8]) -> String {
    hex::encode(aws_lc_rs::digest::digest(&aws_lc_rs::digest::SHA256, bytes).as_ref())
}

fn reads(disk: &dyn Disk, blocks: u64, repeats: u64) -> serde_json::Value {
    let mut bytes = [0; 4096];
    let mut latencies = Vec::new();
    let all = Instant::now();
    for _ in 0..repeats {
        for index in 0..blocks {
            let started = Instant::now();
            disk.read_at(index * 4 * 1024 * 1024, &mut bytes).unwrap();
            latencies.push(started.elapsed().as_nanos() as u64);
            assert!(bytes.iter().all(|&byte| byte == index as u8 + 1));
        }
    }
    let elapsed_ms = all.elapsed().as_secs_f64() * 1000.0;
    latencies.sort_unstable();
    json!({
        "reads": latencies.len(), "requestedBytes": latencies.len() * bytes.len(),
        "elapsedMs": elapsed_ms,
        "p50Ns": latencies[latencies.len() / 2],
        "p95Ns": latencies[latencies.len() * 95 / 100],
        "p99Ns": latencies[latencies.len() * 99 / 100]
    })
}

fn working_set(name: &str, hash: fn(&[u8]) -> String, memory_mib: u64) {
    const BLOCK: u64 = 4 * 1024 * 1024;
    let root = tempfile::tempdir().unwrap();
    let directory = root.path().join("lazy");
    std::fs::create_dir_all(directory.join("cache")).unwrap();
    let mut blocks = Vec::new();
    let raw = root.path().join("direct.disk");
    let mut file = std::fs::File::create(&raw).unwrap();
    for index in 0..9 {
        let bytes = vec![index as u8 + 1; BLOCK as usize];
        let hash = hash(&bytes);
        blocks.push(json!({"offset": index * BLOCK, "size": BLOCK, "hash": hash}));
        std::fs::write(directory.join("cache").join(hash), &bytes).unwrap();
        file.write_all(&bytes).unwrap();
    }
    file.sync_all().unwrap();
    let manifest = json!({"version": 1, "size": 9 * BLOCK, "blockSize": BLOCK, "blocks": blocks});
    let disk = LazyDisk::create(&directory, &manifest, Arc::new(Offline)).unwrap();
    disk.set_context(&json!({"policy": {"memoryCacheMiB": memory_mib}}))
        .unwrap();
    let cold = reads(&disk, 9, 1);
    let warm = reads(&disk, 9, 64);
    let direct = LocalDisk::open(&raw, false).unwrap();
    let direct = reads(&direct, 9, 64);
    println!(
        "{}",
        json!({"case": name, "cold": cold, "warm": warm, "direct": direct, "metrics": disk.performance()})
    );
}

fn main() {
    working_set("sha256-32mib-cache", legacy_hash, 32);
    working_set("sha256-256mib-cache", legacy_hash, 256);
    working_set("blake3-256mib-cache", digest::block, 256);
    let bytes = vec![37; 4 * 1024 * 1024];
    for (name, hash) in [
        ("sha256", legacy_hash as fn(&[u8]) -> String),
        ("blake3", digest::block),
    ] {
        let started = Instant::now();
        for _ in 0..256 {
            black_box(hash(black_box(&bytes)));
        }
        println!(
            "{}",
            json!({"case": "hash", "algorithm": name, "bytes": 1024_u64.pow(3), "elapsedMs": started.elapsed().as_secs_f64() * 1000.0})
        );
    }
}
