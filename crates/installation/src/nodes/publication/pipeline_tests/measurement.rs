//! Opt-in same-process loopback reference. Nothing contacts a real provider.
use super::*;
use crate::storage::{BlockSource, Disk, LazyDisk, digest};
use std::io;

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

fn process_hwm_kib() -> i64 {
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::uninit();
    assert_eq!(
        unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) },
        0
    );
    unsafe { usage.assume_init() }.ru_maxrss
}

struct Source {
    read: direct::Read,
    runtime: tokio::runtime::Handle,
}

impl BlockSource for Source {
    fn fetch(&self, hash: &str) -> io::Result<Vec<u8>> {
        assert_eq!(hash, self.read.hash);
        self.runtime
            .block_on(direct::download(self.read.clone()))
            .map_err(|error| io::Error::other(error.message))
    }
}

impl Fixture {
    async fn reset_measurement(&self, conditions: RemoteConditions) {
        assert_eq!(self.object_server.in_flight.load(Ordering::SeqCst), 0);
        self.object_server.bodies.lock().await.clear();
        *self.object_server.conditions.lock().unwrap() = conditions;
        *self.object_server.blocked_object.lock().unwrap() = None;
        *self.object_server.next_byte.lock().await = None;
        *self.node_metrics.lock().unwrap() = None;
        self.object_server.started.store(0, Ordering::SeqCst);
        self.object_server.maximum.store(0, Ordering::SeqCst);
        self.object_server.encoded_bytes.store(0, Ordering::SeqCst);
    }

    async fn reads(&self) -> Value {
        let object = &self.objects[0];
        let read = self
            .storage
            .direct_read(&self.service.vault, &object.hash, &object.id, object.size)
            .await
            .unwrap();
        let directory = self.root.path().join(format!("read-{}", id()));
        let disk = LazyDisk::create(
            &directory,
            &self.snapshot["manifest"],
            Arc::new(Source {
                read,
                runtime: tokio::runtime::Handle::current(),
            }),
        )
        .unwrap();
        let expected = Contents::Mixed.block(0)[..4096].to_vec();
        tokio::task::spawn_blocking(move || {
            let mut output = [0; 4096];
            let started = Instant::now();
            let cpu = cpu_ms();
            disk.read_at(0, &mut output).unwrap();
            let cold_ms = started.elapsed().as_secs_f64() * 1000.0;
            let cold_cpu_ms = cpu_ms() - cpu;
            assert_eq!(output.as_slice(), expected);
            let started = Instant::now();
            let cpu = cpu_ms();
            for _ in 0..256 {
                disk.read_at(0, &mut output).unwrap();
            }
            let warm_ms = started.elapsed().as_secs_f64() * 1000.0 / 256.0;
            let warm_cpu_ms = (cpu_ms() - cpu) / 256.0;
            assert_eq!(output.as_slice(), expected);
            drop(disk);
            std::fs::remove_dir_all(directory).unwrap();
            json!({
                "cold4KiBWallMs": cold_ms,
                "cold4KiBCpuMs": cold_cpu_ms,
                "warm4KiBWallMs": warm_ms,
                "warm4KiBCpuMs": warm_cpu_ms,
                "warmSamples": 256,
                "coldCondition": "fresh real LazyDisk verified cache, existing manifest, same loopback object bytes; host page cache not dropped",
                "warmCondition": "same LazyDisk block in its verified cache"
            })
        })
        .await
        .unwrap()
    }
}

#[tokio::test]
#[ignore = "controlled publication measurement; run alone with --nocapture --test-threads=1"]
async fn direct_publication_window_comparison() {
    let head = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .output()
        .unwrap();
    let kernel = std::process::Command::new("uname")
        .arg("-r")
        .output()
        .unwrap();
    println!(
        "DIRECT_PIPELINE_HOST {}",
        json!({
            "head": String::from_utf8_lossy(&head.stdout).trim(),
            "kernel": String::from_utf8_lossy(&kernel.stdout).trim(),
            "arch": std::env::consts::ARCH,
            "availableParallelism": std::thread::available_parallelism().unwrap().get(),
            "debugAssertions": cfg!(debug_assertions),
            "transport": "same-process real manager/node routes, presigned loopback object HTTP; synthetic fixture credentials only",
            "cache": "immutable source file written once and host page cache warm; verified-read cache separately reset",
            "productionLatencyOrSpeedupMeasured": false
        })
    );
    let fixture = Fixture::with_contents(128, Contents::Mixed).await;
    fixture.storage.require_direct().await.unwrap();
    let dataset = fixture
        .objects
        .iter()
        .map(|object| object.hash.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let dataset_hash = digest::block(dataset.as_bytes());
    let mut expected_encoded = None;
    for (case, conditions) in [
        ("control_floor", RemoteConditions::default()),
        (
            "control_and_stragglers",
            RemoteConditions {
                control_ms: 25,
                put_ms: 20,
                tail_ms: 100,
                bytes_per_second: 0,
            },
        ),
        (
            "aggregate_bandwidth",
            RemoteConditions {
                control_ms: 25,
                put_ms: 20,
                tail_ms: 100,
                bytes_per_second: 16 * 1024 * 1024,
            },
        ),
    ] {
        for trial in 0..4 {
            for window in if trial % 2 == 0 { [1, 2] } else { [2, 1] } {
                fixture.reset_measurement(conditions).await;
                let started = Instant::now();
                let cpu = cpu_ms();
                let (plain, manager) = fixture.transfer_measured(Some(window)).await.unwrap();
                let write_ms = started.elapsed().as_secs_f64() * 1000.0;
                let write_cpu_ms = cpu_ms() - cpu;
                assert_eq!(plain, 128 * snapshots::BLOCK);
                assert_eq!(fixture.object_server.started.load(Ordering::SeqCst), 128);
                assert_eq!(fixture.object_server.in_flight.load(Ordering::SeqCst), 0);
                assert!(fixture.object_server.maximum.load(Ordering::SeqCst) <= direct::BATCH);
                let node = fixture
                    .node_metrics
                    .lock()
                    .unwrap()
                    .as_ref()
                    .unwrap()
                    .snapshot();
                assert_eq!(node["stages"]["putRequest"]["completed"], 128);
                assert_eq!(node["stages"]["putRequest"]["failedOrCancelled"], 0);
                let encoded = fixture.object_server.encoded_bytes.load(Ordering::SeqCst);
                if let Some(expected) = expected_encoded {
                    assert_eq!(
                        encoded, expected,
                        "same immutable objects/keys/codec must preserve encoded byte counts"
                    );
                } else {
                    expected_encoded = Some(encoded);
                }
                assert_eq!(node["stages"]["putRequest"]["bytes"], encoded);
                let reads = fixture.reads().await;
                if trial == 0 {
                    continue;
                }
                println!(
                    "DIRECT_PIPELINE_PERF {}",
                    json!({
                        "case": case,
                        "mode": if window == 1 { "sequential-reference" } else { "two-batch-candidate" },
                        "maximumControlBatches": window,
                        "plaintextBufferUpperBoundBytes": window as u64 * direct::BATCH as u64 * snapshots::BLOCK,
                        "encodedBufferUpperBoundBytes": direct::BATCH as u64 * block_codec::MAX_ENCODED_BYTES,
                        "trial": trial,
                        "datasetHash": dataset_hash,
                        "blocks": 128,
                        "plaintextBytes": plain,
                        "encodedBytes": encoded,
                        "sourceReader": "raw immutable fixture file through real node Reader; no mounted block device",
                        "writeWallMs": write_ms,
                        "writeCpuMs": write_cpu_ms,
                        "processHwmKiB": process_hwm_kib(),
                        "hwmScope": "cumulative process high-water includes loopback object server and previous trials",
                        "serverMaxPutInFlight": fixture.object_server.maximum.load(Ordering::SeqCst),
                        "globalPutLimit": direct::BATCH,
                        "controlDelayPerBatchMs": conditions.control_ms,
                        "putDelayMs": conditions.put_ms,
                        "oneInFourAdditionalTailMs": conditions.tail_ms,
                        "aggregateObjectBodyBytesPerSecond": conditions.bytes_per_second,
                        "reads": reads,
                        "manager": manager,
                        "node": node
                    })
                );
            }
        }
    }
}
