//! Explicit local metadata qualification; output belongs in the PR, never in git.
use super::*;
use std::{
    io::{Read, Seek},
    os::fd::AsRawFd,
    time::Instant,
};

fn distribution(mut samples: Vec<f64>) -> serde_json::Value {
    samples.sort_by(f64::total_cmp);
    serde_json::json!({ "samples": samples.len(), "medianMs": samples[samples.len() / 2], "p95Ms": samples[samples.len() * 95 / 100] })
}

fn reads(path: &Path) -> serde_json::Value {
    let mut file = std::fs::File::open(path).unwrap();
    let size = file.metadata().unwrap().len() as usize;
    let mut bytes = vec![0; size];
    let mut warm = Vec::new();
    let mut cold = Vec::new();
    for _ in 0..500 {
        file.rewind().unwrap();
        let started = Instant::now();
        file.read_exact(&mut bytes).unwrap();
        warm.push(started.elapsed().as_secs_f64() * 1000.0);
        unsafe {
            libc::posix_fadvise(file.as_raw_fd(), 0, 0, libc::POSIX_FADV_DONTNEED);
        }
        let started = Instant::now();
        std::fs::File::open(path)
            .unwrap()
            .read_exact(&mut bytes)
            .unwrap();
        cold.push(started.elapsed().as_secs_f64() * 1000.0);
    }
    serde_json::json!({ "encodedBytes": size, "warmRead": distribution(warm), "advisoryColdRead": distribution(cold) })
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

#[tokio::test]
#[ignore = "explicit local native suspension metadata benchmark"]
async fn native_suspension_metadata_measurements() {
    // Compare the existing native lease record's atomic fsynced write primitive
    // with reason arbitration while an already confirmed storage fence remains.
    let root = tempfile::tempdir().unwrap();
    let cpu = Arc::new(TestCpu::default());
    let pause = Suspension::testing(root.path(), "attempt", cpu.clone());
    pause.bind().await.unwrap();
    pause.set_reason(&Reason::Storage, true).await.unwrap();
    let reference = root.path().join("attempt.lease");
    let mut lease_writes = Vec::new();
    let started_cpu = cpu_ms();
    for _ in 0..100 {
        let started = Instant::now();
        super::super::watchdog::grant(&reference, crate::nodes::boot_ms() + 60_000)
            .await
            .unwrap();
        lease_writes.push(started.elapsed().as_secs_f64() * 1000.0);
    }
    let lease_cpu = cpu_ms() - started_cpu;
    let reason = Reason::Exclusive {
        reservation_id: crate::config::id(),
        epoch: 1,
    };
    let mut writes = Vec::new();
    let started_cpu = cpu_ms();
    for index in 0..100 {
        let started = Instant::now();
        pause.set_reason(&reason, index % 2 == 0).await.unwrap();
        writes.push(started.elapsed().as_secs_f64() * 1000.0);
    }
    let suspension_cpu = cpu_ms() - started_cpu;
    assert_eq!(*cpu.transitions.lock().await, vec![true]);
    println!(
        "{}",
        serde_json::json!({
            "benchmark": "native-suspension-metadata",
            "workload": "100 fsynced native lease writes vs 100 alternating exclusive reason changes while a storage fence remains; 500 warm and 500 advisory-cold reads per file",
            "coldReadPolicy": "POSIX_FADV_DONTNEED before opening a fresh descriptor; advisory only, not physically cold disk or production network latency",
            "cpuControl": "one synthetic systemd freeze before timing; zero CPU transitions during measured mutations",
            "host": { "kernel": std::fs::read_to_string("/proc/sys/kernel/osrelease").unwrap().trim(),
                "cpu": std::fs::read_to_string("/proc/cpuinfo").unwrap().lines().find(|line| line.starts_with("model name")).unwrap_or("unknown"),
                "memory": std::fs::read_to_string("/proc/meminfo").unwrap().lines().next(),
                "parallelism": std::thread::available_parallelism().unwrap().get(), "directory": root.path() },
            "referenceLease": { "read": reads(&reference), "write": distribution(lease_writes), "cpuMs": lease_cpu },
            "suspension": { "read": reads(&pause.path), "write": distribution(writes), "cpuMs": suspension_cpu }
        })
    );
}
