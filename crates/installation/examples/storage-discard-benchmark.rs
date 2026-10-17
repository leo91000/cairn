//! Local native comparison, no S3 or VM latency. Run alone through the backend
//! launcher's isolated environment; JSON output contains only aggregate metrics.
use cairn_installation::{
    storage::{BlockSource, Disk, LazyDisk, block_codec::BlockCodec},
    store::Store,
    vault::Vault,
};
use rand::{RngCore, SeedableRng};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    fs, io,
    os::unix::fs::MetadataExt,
    sync::{Arc, Mutex},
    time::Instant,
};

const BLOCK: usize = 4 * 1024 * 1024;
const BLOCKS: usize = 16;

struct Origin {
    encoded: Mutex<HashMap<String, Vec<u8>>>,
    vault: Vault,
    codec: Arc<BlockCodec>,
    runtime: tokio::runtime::Handle,
}

impl BlockSource for Origin {
    fn fetch(&self, hash: &str) -> io::Result<Vec<u8>> {
        let encoded = self
            .encoded
            .lock()
            .unwrap()
            .get(hash)
            .cloned()
            .ok_or_else(|| io::Error::other("Missing benchmark object"))?;
        tokio::task::block_in_place(|| {
            self.runtime.block_on(self.codec.decode(
                self.vault.clone(),
                format!("discard-benchmark:{hash}"),
                hash.to_owned(),
                Some(BLOCK as u64),
                encoded,
            ))
        })
        .map_err(|error| io::Error::other(error.message))
    }
}

fn cpu_ms() -> f64 {
    let mut clock: libc::timespec = unsafe { std::mem::zeroed() };
    assert_eq!(
        unsafe { libc::clock_gettime(libc::CLOCK_PROCESS_CPUTIME_ID, &mut clock) },
        0
    );
    clock.tv_sec as f64 * 1000.0 + clock.tv_nsec as f64 / 1_000_000.0
}

fn reads(disk: &LazyDisk) -> Value {
    let mut timings = Vec::new();
    let cpu = cpu_ms();
    for index in 0..BLOCKS {
        let mut bytes = [0; 4096];
        let started = Instant::now();
        disk.read_at((index * BLOCK + BLOCK / 4) as u64, &mut bytes)
            .unwrap();
        std::hint::black_box(bytes);
        timings.push(started.elapsed().as_secs_f64() * 1000.0);
    }
    let cpu = cpu_ms() - cpu;
    timings.sort_by(f64::total_cmp);
    json!({"count": timings.len(), "p50Ms": timings[timings.len()/2], "p95Ms": timings[timings.len()*95/100], "cpuMs": cpu})
}

async fn trial(trim: bool, trial: usize) -> Value {
    let root = tempfile::tempdir().unwrap();
    let vault = Vault::new(Store::open(root.path()).unwrap(), root.path()).unwrap();
    let codec = Arc::new(BlockCodec::default());
    let origin = Arc::new(Origin {
        encoded: Mutex::new(HashMap::new()),
        vault: vault.clone(),
        codec: codec.clone(),
        runtime: tokio::runtime::Handle::current(),
    });
    let manifest = json!({
        "version": 1,
        "size": BLOCK * BLOCKS,
        "blockSize": BLOCK,
        "blocks": (0..BLOCKS).map(|index| json!({"offset": index * BLOCK, "size": BLOCK, "hash": null})).collect::<Vec<_>>()
    });
    let directory = root.path().join("disk");
    let disk = Arc::new(LazyDisk::create(&directory, &manifest, origin.clone()).unwrap());
    let mut random = rand::rngs::StdRng::seed_from_u64(214);
    let mut expected = Vec::new();
    let mut writes = Vec::new();
    let mut write_cpu = 0.0;
    for index in 0..BLOCKS {
        let mut bytes = vec![index as u8; BLOCK];
        random.fill_bytes(&mut bytes[..BLOCK / 2]);
        let cpu_started = cpu_ms();
        let started = Instant::now();
        disk.write_at((index * BLOCK) as u64, &bytes).unwrap();
        writes.push(started.elapsed().as_secs_f64() * 1000.0);
        write_cpu += cpu_ms() - cpu_started;
        expected.push(bytes);
    }
    let start = Arc::new(std::sync::Barrier::new(2));
    let worker = {
        let start = start.clone();
        let disk = disk.clone();
        std::thread::spawn(move || {
            start.wait();
            let cpu = cpu_ms();
            let started = Instant::now();
            if trim {
                disk.discard(0, 8 * BLOCK).unwrap();
                for index in 8..12 {
                    disk.discard((index * BLOCK) as u64, BLOCK / 2).unwrap();
                }
            }
            (started.elapsed().as_secs_f64() * 1000.0, cpu_ms() - cpu)
        })
    };
    let foreground_cpu = cpu_ms();
    start.wait();
    let mut foreground_writes = Vec::new();
    let mut foreground_reads = Vec::new();
    // Non-overlapping foreground data remains byte-identical. Start with the
    // maintenance worker; the reference worker does no discard. Wall latencies
    // capture contention; process CPU includes both concurrent activities.
    let foreground_offset = ((BLOCKS - 1) * BLOCK + BLOCK / 4) as u64;
    let foreground_bytes = &expected[BLOCKS - 1][BLOCK / 4..BLOCK / 4 + 4096];
    for _ in 0..32 {
        let started = Instant::now();
        disk.write_at(foreground_offset, foreground_bytes).unwrap();
        foreground_writes.push(started.elapsed().as_secs_f64() * 1000.0);
        let mut actual = [0; 4096];
        let started = Instant::now();
        disk.read_at(foreground_offset, &mut actual).unwrap();
        foreground_reads.push(started.elapsed().as_secs_f64() * 1000.0);
        assert_eq!(&actual, foreground_bytes);
    }
    let (trim_ms, trim_cpu) = worker.join().unwrap();
    let foreground_cpu = cpu_ms() - foreground_cpu;
    foreground_writes.sort_by(f64::total_cmp);
    foreground_reads.sort_by(f64::total_cmp);
    if trim {
        for bytes in &mut expected[..8] {
            bytes.fill(0);
        }
        for bytes in expected.iter_mut().take(12).skip(8) {
            bytes[..BLOCK / 2].fill(0);
        }
    }
    let before_repeat = disk.accounting().unwrap();
    if trim {
        disk.discard(0, 8 * BLOCK).unwrap();
        assert_eq!(disk.accounting().unwrap(), before_repeat);
    }
    let mut journal_allocated = 0;
    let mut journal_logical = 0;
    for entry in fs::read_dir(&directory).unwrap().flatten() {
        if entry.file_name().to_string_lossy().ends_with(".segment") {
            let metadata = entry.metadata().unwrap();
            journal_allocated += metadata.blocks() * 512;
            journal_logical += metadata.len();
        }
    }
    let publication_cpu = cpu_ms();
    let started = Instant::now();
    let generation = disk.seal_completed().unwrap();
    let manifest = disk.capture(generation).unwrap();
    let mut encoded_bytes = 0;
    let mut objects = HashMap::new();
    for block in manifest["blocks"].as_array().unwrap() {
        let index = block["offset"].as_u64().unwrap() as usize / BLOCK;
        if let Some(hash) = block["hash"].as_str() {
            let bytes = disk.captured_block(generation, hash).unwrap();
            assert_eq!(bytes, expected[index]);
            if !objects.contains_key(hash) {
                let encoded = codec
                    .encode(vault.clone(), format!("discard-benchmark:{hash}"), bytes)
                    .await
                    .unwrap();
                encoded_bytes += encoded.len();
                objects.insert(hash.to_owned(), encoded.len());
                origin
                    .encoded
                    .lock()
                    .unwrap()
                    .insert(hash.to_owned(), encoded);
            }
        } else {
            assert!(expected[index].iter().all(|byte| *byte == 0));
        }
    }
    disk.commit_published(generation, "benchmark").unwrap();
    let publication_ms = started.elapsed().as_secs_f64() * 1000.0;
    let publication_cpu = cpu_ms() - publication_cpu;
    drop(disk);
    // Force the cold path through the real authenticated codec. Encoded
    // objects are in memory: this explicitly excludes network latency.
    fs::remove_dir_all(directory.join("cache")).unwrap();
    let disk = LazyDisk::open(&directory, origin.clone()).unwrap();
    let cold = reads(&disk);
    let warm = reads(&disk);
    // Verify every byte from the published state, including partially trimmed blocks.
    for (index, expected) in expected.iter().enumerate() {
        let mut actual = vec![0; BLOCK];
        disk.read_at((index * BLOCK) as u64, &mut actual).unwrap();
        assert_eq!(&actual, expected);
    }
    writes.sort_by(f64::total_cmp);
    json!({
        "trial": trial,
        "discard": trim,
        "logicalBytes": BLOCK * BLOCKS,
        "discardedBytes": if trim { 40 * 1024 * 1024 } else { 0 },
        "encodedLiveBytes": encoded_bytes,
        "liveObjects": objects.len(),
        "write": {"count": writes.len(), "p50Ms": writes[writes.len()/2], "p95Ms": writes[writes.len()*95/100], "cpuMs": write_cpu},
        "trim": {"wallMs": trim_ms, "processCpuMsIncludingForeground": trim_cpu},
        "foregroundDuringTrim": {
            "count": 32,
            "readP50Ms": foreground_reads[16],
            "readP95Ms": foreground_reads[30],
            "writeP50Ms": foreground_writes[16],
            "writeP95Ms": foreground_writes[30],
            "processCpuMsIncludingTrim": foreground_cpu
        },
        "publication": {"wallMs": publication_ms, "cpuMs": publication_cpu},
        "journalAllocatedBytes": journal_allocated,
        "journalLogicalBytes": journal_logical,
        "cold4KiB": cold,
        "warm4KiB": warm,
        "verified": true
    })
}

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() {
    let mut trials = Vec::new();
    for index in 0..3 {
        for trim in if index % 2 == 0 {
            [false, true]
        } else {
            [true, false]
        } {
            trials.push(trial(trim, index).await);
        }
    }
    println!(
        "{}",
        json!({
            "schema": 1,
            "workload": "16 unique 4 MiB regions; each half seeded random, half repeated; discard 8 whole regions and random halves of 4 more",
            "reference": "same native LazyDisk/BlockCodec with discard disabled",
            "host": {
                "kernel": String::from_utf8(std::process::Command::new("uname").arg("-srvm").output().unwrap().stdout).unwrap().trim(),
                "cpus": std::thread::available_parallelism().unwrap().get(),
                "profile": if cfg!(debug_assertions) { "development" } else { "release" }
            },
            "limits": "No VM, S3, network or filesystem allocation model. Cold means empty decoded memory/disk caches and real codec decode; encoded objects are in memory. Host page cache is not flushed. CPU excludes fixture generation and includes all process threads. S3 ownership/GC unchanged; bytes count only current unique encoded objects.",
            "trials": trials
        })
    );
}

#[test]
#[ignore = "local reference: run alone with --ignored --nocapture --test-threads=1"]
fn storage_discard_reference() {
    main();
}
