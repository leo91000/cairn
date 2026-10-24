//! Optional observation of the unchanged file loader and its blocking-pool boundary.
use super::*;
use std::{io::Write, time::Instant};

fn cpu_ms(clock_id: libc::clockid_t) -> f64 {
    let mut clock: libc::timespec = unsafe { std::mem::zeroed() };
    assert_eq!(unsafe { libc::clock_gettime(clock_id, &mut clock) }, 0);
    clock.tv_sec as f64 * 1000.0 + clock.tv_nsec as f64 / 1_000_000.0
}

fn read_sample(directory: &Path) -> (Package, f64) {
    let cpu = cpu_ms(libc::CLOCK_THREAD_CPUTIME_ID);
    let package = read_version_at(directory, "reader", "1").unwrap();
    (package, cpu_ms(libc::CLOCK_THREAD_CPUTIME_ID) - cpu)
}

#[tokio::test]
#[ignore = "local package-file read/write observation; run alone with --nocapture"]
async fn compare_inline_and_offloaded_package_reads() {
    let root = tempfile::tempdir().unwrap();
    let manifest = json!({
        "id": "reader", "name": "Reader", "version": "1", "apiVersion": 2,
        "capabilities": ["resources"],
        "resources": [{
            "name": "workspace", "title": "Workspaces", "singular": "Workspace",
            "settings": [{
                "key": "externalId", "title": "Workspace ID", "type": "string",
                "pattern": "^T[0-9]+$"
            }]
        }]
    })
    .to_string();
    let source = "return {}";
    let mut cases = Vec::new();
    for offloaded in [false, true] {
        let directory = root
            .path()
            .join(if offloaded { "offloaded" } else { "inline" });
        let version = directory.join("reader/1");
        std::fs::create_dir_all(&version).unwrap();
        let mut writes = Vec::new();
        for _ in 0..6 {
            let start = Instant::now();
            let cpu = cpu_ms(libc::CLOCK_PROCESS_CPUTIME_ID);
            for (name, bytes) in [
                ("manifest.json", manifest.as_bytes()),
                ("main.luau", source.as_bytes()),
            ] {
                let mut file = std::fs::File::create(version.join(name)).unwrap();
                file.write_all(bytes).unwrap();
                file.sync_all().unwrap();
            }
            writes.push(json!({
                "wallMs": start.elapsed().as_secs_f64() * 1000.0,
                "processCpuMs": cpu_ms(libc::CLOCK_PROCESS_CPUTIME_ID) - cpu,
            }));
        }
        let mut reads = Vec::new();
        let mut digest = None;
        let batch_cpu = cpu_ms(libc::CLOCK_PROCESS_CPUTIME_ID);
        for _ in 0..6 {
            let start = Instant::now();
            let cpu = cpu_ms(libc::CLOCK_PROCESS_CPUTIME_ID);
            let (package, thread_cpu) = if offloaded {
                let directory = directory.clone();
                tokio::task::spawn_blocking(move || read_sample(&directory))
                    .await
                    .unwrap()
            } else {
                read_sample(&directory)
            };
            reads.push(json!({
                "wallMs": start.elapsed().as_secs_f64() * 1000.0,
                "processCpuMs": cpu_ms(libc::CLOCK_PROCESS_CPUTIME_ID) - cpu,
                "loaderThreadCpuMs": thread_cpu,
            }));
            assert_eq!(package.manifest.id, "reader");
            assert_eq!(package.source.main, source);
            if let Some(expected) = &digest {
                assert_eq!(&package.digest, expected);
            } else {
                digest = Some(package.digest);
            }
        }
        cases.push(json!({
            "offloaded": offloaded,
            "manifestBytes": manifest.len(),
            "sourceBytes": source.len(),
            "digest": digest,
            "readBatchProcessCpuMs": cpu_ms(libc::CLOCK_PROCESS_CPUTIME_ID) - batch_cpu,
            "reads": reads,
            "writes": writes,
        }));
    }
    assert_eq!(cases[0]["digest"], cases[1]["digest"]);
    println!(
        "PLUGIN_APPROVAL_READ_MEASUREMENT {}",
        json!({
            "availableParallelism": std::thread::available_parallelism().unwrap().get(),
            "cases": cases,
        })
    );
}
