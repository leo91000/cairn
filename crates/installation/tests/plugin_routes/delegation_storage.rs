//! Narrow Store observations; this does not time delegation or provider execution.
use super::*;
use cairn_installation::store::Store;
use std::time::Instant;

fn cpu_ms() -> f64 {
    let mut clock = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    assert_eq!(
        unsafe { libc::clock_gettime(libc::CLOCK_PROCESS_CPUTIME_ID, &mut clock) },
        0
    );
    clock.tv_sec as f64 * 1000.0 + clock.tv_nsec as f64 / 1_000_000.0
}

#[tokio::test]
#[ignore = "manual delegated-source storage observation; run alone with --ignored --exact"]
async fn measure_delegated_source_storage() {
    let mut cases = Vec::new();
    for shape in [
        "previous-no-source",
        "delegated-source",
        "delegated-suspended-source",
    ] {
        let directory = TempDir::new().unwrap();
        let mut pairs = Vec::new();
        let mut lookup = (String::new(), String::new());
        for index in 0..50 {
            let chat = format!("00000000-0000-4000-8000-{index:012}");
            let message = "00000000-0000-4000-8000-000000000100";
            let actor = format!("plugin-message-requester:{chat}:{message}");
            let control = format!("plugin-requester-suspended:{chat}:delegated:{message}");
            lookup = (actor.clone(), control.clone());
            if shape != "previous-no-source" {
                pairs.push((actor, json!({
                    "connectionId": "00000000-0000-4000-8000-000000000101",
                    "connectionRevision": "00000000-0000-4000-8000-000000000102",
                    "binding": {
                        "id": "00000000-0000-4000-8000-000000000103",
                        "revision": "00000000-0000-4000-8000-000000000104",
                        "resource": "00000000-0000-4000-8000-000000000105",
                        "resources": [["00000000-0000-4000-8000-000000000105", "resource-revision"]],
                        "digest": "synthetic-package-digest",
                        "context": { "channel": "C1", "thread": "source-thread" },
                        "person": { "id": "alice", "revision": "person-revision" },
                    },
                    "subject": "alice",
                })));
                pairs.push((control, json!(shape == "delegated-suspended-source")));
            }
        }
        let encoded: usize = pairs
            .iter()
            .map(|(_, value)| serde_json::to_vec(value).unwrap().len())
            .sum();
        let key_bytes: usize = pairs.iter().map(|(key, _)| key.len()).sum();
        let store = Store::open(directory.path()).unwrap();
        let seed = pairs.clone();
        store
            .transaction(move |db| {
                for (key, value) in seed {
                    db.set(&key, &value, None)?;
                }
                Ok(())
            })
            .await
            .unwrap();
        drop(store);
        let store = Store::open(directory.path()).unwrap();
        let reads = store
            .read(move |db| {
                let mut samples = Vec::new();
                for sample in 0..6 {
                    let cpu = cpu_ms();
                    let start = Instant::now();
                    let values = [db.kv(&lookup.0)?, db.kv(&lookup.1)?];
                    samples.push(json!({
                        "sample": sample, "wallMs": start.elapsed().as_secs_f64() * 1000.0,
                        "processCpuMs": cpu_ms() - cpu,
                        "returnedJsonBytes": serde_json::to_vec(&values)?.len(),
                    }));
                }
                Ok(samples)
            })
            .await
            .unwrap();
        let mut writes = Vec::new();
        for sample in 0..6 {
            let values: Vec<_> = pairs.iter().take(2).cloned().collect();
            let cpu = cpu_ms();
            let start = Instant::now();
            store
                .transaction(move |db| {
                    for (key, value) in values {
                        db.set(&key, &value, None)?;
                    }
                    Ok(())
                })
                .await
                .unwrap();
            writes.push(json!({
                "sample": sample, "wallMs": start.elapsed().as_secs_f64() * 1000.0,
                "processCpuMs": cpu_ms() - cpu,
            }));
        }
        cases.push(json!({
            "shape": shape, "records": pairs.len(), "encodedJsonBytes": encoded,
            "keyBytes": key_bytes, "reads": reads, "writes": writes,
        }));
    }
    eprintln!(
        "DELEGATED_SOURCE_STORAGE {}",
        json!({
            "cases": cases,
            "workload": "50 synthetic delegated inputs; missing host metadata versus existing principal JSON plus per-input boolean; six paired KV reads and six transactions writing zero or two KV values",
            "cache": "Store reopened after seeding; first read connection-cold, next five on same connection; OS cache retained",
            "limits": "encoded JSON/key bytes exclude SQLite pages, compression, indexes and WAL; reads exclude actor scheduling, writes include it; reference is a no-op transaction, not full prior chat creation; excludes comparison scans, package files, vault, authority/network/provider I/O and whole-service recovery; no production or speedup claim",
            "host": {
                "parallelism": std::thread::available_parallelism().ok().map(std::num::NonZero::get),
                "loadavg": std::fs::read_to_string("/proc/loadavg").ok(),
                "ioPressure": std::fs::read_to_string("/proc/pressure/io").ok(),
                "cpuMax": std::fs::read_to_string("/sys/fs/cgroup/cpu.max").ok(),
                "memoryMax": std::fs::read_to_string("/sys/fs/cgroup/memory.max").ok(),
            },
        })
    );
}
