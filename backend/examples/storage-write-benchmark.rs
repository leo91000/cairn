//! Durable write and verified read costs, without FUSE, networking or VM scheduling.
//! Run with the repository's pinned toolchain and `cargo run --release --example storage-write-benchmark`.
use leo_agent_manager::storage::{BlockSource, Disk, LazyDisk, LocalDisk, policy::Policy, runtime};
use serde_json::json;
use std::{io, sync::Arc, time::Instant};

struct Offline;

impl BlockSource for Offline {
    fn fetch(&self, _: &str) -> io::Result<Vec<u8>> {
        Err(io::Error::other("Benchmark must not use the network"))
    }
}

struct Fixture {
    disk: Arc<dyn Disk>,
    _root: tempfile::TempDir,
}

fn fixture(mode: &str) -> Fixture {
    const SIZE: u64 = 64 * 1024 * 1024;
    const BLOCK: u64 = 4 * 1024 * 1024;
    let benchmark_root = std::env::var_os("LEO_BENCH_ROOT").map_or_else(
        || std::env::current_dir().unwrap(),
        std::path::PathBuf::from,
    );
    let root = tempfile::Builder::new()
        .prefix("leo-storage-benchmark-")
        .tempdir_in(benchmark_root)
        .unwrap();
    if mode == "direct" {
        let raw = root.path().join("direct.disk");
        std::fs::File::create(&raw).unwrap().set_len(SIZE).unwrap();
        return Fixture {
            disk: Arc::new(LocalDisk::open(&raw, true).unwrap()),
            _root: root,
        };
    }
    let directory = root.path().join("disks/conversation");
    std::fs::create_dir_all(&directory).unwrap();
    let blocks = (0..SIZE / BLOCK)
        .map(|index| json!({"offset": index * BLOCK, "size": BLOCK, "hash": null}))
        .collect::<Vec<_>>();
    let manifest = json!({"version": 1, "size": SIZE, "blockSize": BLOCK, "blocks": blocks});
    let policy = Policy {
        reserve_mi_b: 64,
        reserve_percent: 1,
        ..Default::default()
    };
    let context = json!({"master": "http://127.0.0.1:1/", "grant": "fixture", "policy": policy});
    let disk = LazyDisk::create(&directory.join("lazy"), &manifest, Arc::new(Offline)).unwrap();
    disk.set_context(&context).unwrap();
    if mode == "journal" {
        return Fixture {
            disk: Arc::new(disk),
            _root: root,
        };
    }
    drop(disk);
    std::fs::write(
        root.path().join("storage-policy.json"),
        serde_json::to_vec(&policy).unwrap(),
    )
    .unwrap();
    Fixture {
        disk: runtime::open(&directory).unwrap(),
        _root: root,
    }
}

fn summary(mut samples: Vec<u64>) -> serde_json::Value {
    samples.sort_unstable();
    json!({
        "totalMs": samples.iter().sum::<u64>() as f64 / 1000.0,
        "p50Micros": samples[samples.len() / 2],
        "p95Micros": samples[samples.len() * 95 / 100],
        "p99Micros": samples[samples.len() * 99 / 100],
        "maxMicros": samples.last().unwrap()
    })
}

#[tokio::main]
async fn main() {
    let modes = ["direct", "journal", "admitted-journal"];
    for sample in 0..3 {
        for (size, count) in [(4096, 4000), (65536, 1000), (1048576, 128)] {
            for index in 0..modes.len() {
                let mode = modes[(index + sample) % modes.len()];
                let fixture = fixture(mode);
                let mut input = vec![0; size];
                let mut output = vec![0; size];
                let mut writes = Vec::with_capacity(count);
                let mut reads = Vec::with_capacity(count);
                for sequence in 0..count {
                    input.fill((sequence % 251) as u8);
                    let offset = ((sequence % 64) * size) as u64;
                    let started = Instant::now();
                    fixture.disk.write_at(offset, &input).unwrap();
                    writes.push(started.elapsed().as_micros() as u64);
                    let started = Instant::now();
                    fixture.disk.read_at(offset, &mut output).unwrap();
                    reads.push(started.elapsed().as_micros() as u64);
                    assert_eq!(output, input);
                }
                fixture.disk.sync().unwrap();
                println!(
                    "{}",
                    json!({
                        "sample": sample, "mode": mode, "writeBytes": size, "operations": count,
                        "writes": summary(writes), "checkedReads": summary(reads)
                    })
                );
            }
        }
    }
}
