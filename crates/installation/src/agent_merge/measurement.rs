//! Opt-in read-shape measurements for the merge precondition, not HTTP latency.
use super::*;
use crate::store::Store;
use std::time::Instant;

fn cpu_ms() -> f64 {
    let mut value = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    assert_eq!(
        unsafe { libc::clock_gettime(libc::CLOCK_PROCESS_CPUTIME_ID, &mut value) },
        0
    );
    value.tv_sec as f64 * 1000.0 + value.tv_nsec as f64 / 1_000_000.0
}

#[tokio::test]
#[ignore = "reproducible local read/write measurement; run alone"]
async fn measure_resource_merge_precondition() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(root.path()).unwrap();
    let present = crate::config::id();
    let absent = crate::config::id();
    let other = crate::config::id();
    let mut rows = Vec::new();
    for index in 0..2000 {
        let kind = if index % 2 == 0 {
            crate::plugins::routes::KIND
        } else {
            crate::plugins::grants::KIND
        };
        let row = json!({
            "id": crate::config::id(),
            "connectionId": crate::config::id(),
            "resourceId": crate::config::id(),
            "agentId": if index == 1999 {&present} else {&other},
            "revision": crate::config::id(),
            "ownerAccessId": "synthetic-owner",
            "enabled": false,
            "scopes":{"channel":(0..32).map(|i|format!("C{i:010}")).collect::<Vec<_>>()},
            "people":(0..32).map(|i|json!({"id":format!("U{i:010}"),"name":format!("Synthetic person {i}")})).collect::<Vec<_>>(),
        });
        rows.push((kind, row));
    }
    let bytes: usize = rows
        .iter()
        .map(|(_, row)| serde_json::to_vec(row).unwrap().len())
        .sum();
    let seed = rows.clone();
    store
        .transaction(move |db| {
            for (kind, row) in seed {
                db.put(kind, &row)?;
            }
            Ok(())
        })
        .await
        .unwrap();
    drop(store);
    let mut reads = Vec::new();
    for (case, source) in [("present", present), ("absent", absent)] {
        for implementation in ["complete-record-scan", "existence-projection"] {
            // A fresh Store has cold reader/statement caches. OS caches remain warm.
            let store = Store::open(root.path()).unwrap();
            for sample in 0..6 {
                let source = source.clone();
                let cpu = cpu_ms();
                let started = Instant::now();
                let (found, materialized) = store
                    .read(move |db| {
                        if implementation == "existence-projection" {
                            let found = resource_references(db, &source)?;
                            Ok((found, serde_json::to_vec(&found)?.len()))
                        } else {
                            let mut records = db.list(crate::plugins::routes::KIND)?;
                            records.extend(db.list(crate::plugins::grants::KIND)?);
                            let found = records.iter().any(|row| row["agentId"] == source);
                            Ok((found, serde_json::to_vec(&records)?.len()))
                        }
                    })
                    .await
                    .unwrap();
                assert_eq!(found, case == "present");
                reads.push(json!({
                    "case": case,
                    "implementation": implementation,
                    "sample": sample,
                    "wallMs": started.elapsed().as_secs_f64() * 1000.0,
                    "processCpuMs": cpu_ms() - cpu,
                    "materializedJsonBytes": materialized,
                }));
            }
        }
    }
    let store = Store::open(root.path()).unwrap();
    let mut writes = Vec::new();
    for sample in 0..6 {
        let (kind, row) = rows[0].clone();
        let cpu = cpu_ms();
        let started = Instant::now();
        store.put(kind, row).await.unwrap();
        writes.push(json!({
            "sample": sample,
            "wallMs": started.elapsed().as_secs_f64() * 1000.0,
            "processCpuMs": cpu_ms() - cpu,
        }));
    }
    eprintln!(
        "RESOURCE_MERGE_MEASUREMENT {}",
        json!({
            "rows": rows.len(),
            "storedJsonBytes": bytes,
            "readerCache":"fresh per implementation; sample0 cold reader/statement caches then5warm; OS cache retained",
            "scope":"actual merge existence predicate versus complete JSON-record scan; not a whole merge, HTTP or production network measurement",
            "recordShape":"synthetic inactive reference rows with32channels/32people; scan payload, not provider authority fixtures",
            "writes": "existing same-value record write path; unchanged by predicate",
            "reads": reads,
            "writesSamples": writes,
            "host": {
                "parallelism": std::thread::available_parallelism().ok().map(std::num::NonZero::get),
                "cpuMax": std::fs::read_to_string("/sys/fs/cgroup/cpu.max").ok(),
                "memoryMax": std::fs::read_to_string("/sys/fs/cgroup/memory.max").ok(),
            },
        })
    );
}
