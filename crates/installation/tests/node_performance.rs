//! Repeatable benchmark through the shipped local ext4 bootstrap interface.
//! Run alone with --ignored --nocapture --test-threads=1. All identities/data are fixtures.

mod common;

use cairn_installation::storage::{Disk, bootstrap, policy::Policy, runtime};
use serde_json::{Value, json};
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

const MIB: usize = 1024 * 1024;

#[tokio::test]
#[ignore = "explicit ext4 bootstrap benchmark on persistent host storage"]
async fn conversation_bootstrap_by_logical_disk_size() {
    let directory =
        std::env::var("CAIRN_BOOTSTRAP_BENCH_ROOT").unwrap_or_else(|_| "/var/tmp".into());
    let context = json!({
        "owner": "fixture-bootstrap",
        "policy": Policy {
            reserve_mi_b: 64,
            reserve_percent: 1,
            ..Policy::default()
        }
    });
    for sample in 0..2 {
        let sizes = if sample == 0 {
            [32_768, 400_844]
        } else {
            [400_844, 32_768]
        };
        for size_mib in sizes {
            let root = tempfile::Builder::new()
                .prefix("cairn-bootstrap-bench-")
                .tempdir_in(&directory)
                .unwrap();
            let before = usage();
            let started = Instant::now();
            let size = size_mib * MIB as u64;
            bootstrap::prepare(root.path(), size, &context, &CancellationToken::new())
                .await
                .unwrap();
            let elapsed = started.elapsed();
            let volume = runtime::load(root.path()).await.unwrap();
            assert_eq!(volume.disk.size(), size);
            let mut header = [0; 64];
            volume.disk.read_at(1024, &mut header).unwrap();
            assert_eq!(&header[56..58], &[0x53, 0xef]);
            let blocks = u32::from_le_bytes(header[4..8].try_into().unwrap()) as u64;
            let block_size = 1024_u64 << u32::from_le_bytes(header[24..28].try_into().unwrap());
            assert_eq!(blocks * block_size, size);
            drop(volume);
            let reopened = runtime::load(root.path()).await.unwrap();
            let mut restored = [0; 64];
            reopened.disk.read_at(1024, &mut restored).unwrap();
            assert_eq!(restored, header);
            report(
                "conversation_bootstrap",
                elapsed,
                before,
                &json!({
                    "sample": sample,
                    "logicalDiskMiB": size_mib,
                    "directory": directory,
                    "integrityCheckedAfterReopen": true,
                    "counterScope": "process including integrity checks; wallMs measures prepare only"
                }),
            );
        }
    }
}

fn usage() -> (f64, u64) {
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::zeroed();
    assert_eq!(
        unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) },
        0
    );
    let usage = unsafe { usage.assume_init() };
    let cpu = (usage.ru_utime.tv_sec + usage.ru_stime.tv_sec) as f64
        + (usage.ru_utime.tv_usec + usage.ru_stime.tv_usec) as f64 / 1_000_000.0;
    let io = std::fs::read_to_string("/proc/self/io").unwrap();
    let reads = io
        .lines()
        .find_map(|line| line.strip_prefix("rchar: "))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    (cpu, reads)
}

fn report(kind: &str, elapsed: Duration, before: (f64, u64), details: &Value) {
    let after = usage();
    let report = json!({
        "scenario": kind,
        "wallMs": elapsed.as_secs_f64() * 1000.0,
        "cpuMs": (after.0 - before.0) * 1000.0,
        "processReadBytes": after.1 - before.1,
        "details": details,
    });
    println!("NODE_PERF {report}");
}
