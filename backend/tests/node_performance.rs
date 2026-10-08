//! Repeatable benchmarks through the shipped backup and outbound transport interfaces.
//! Capture benchmarks require LEO_NODE_TEST_S3_ENDPOINT and synthetic AWS credentials.
//! Run alone with --ignored --nocapture --test-threads=1. All identities/data are fixtures.
mod common;

use common::relay_fixture::router;

use axum::{
    Json, Router,
    body::Bytes,
    extract::Request,
    middleware::{self, Next},
    response::IntoResponse,
};
use leo_agent_manager::{
    auth,
    config::{Config, id, now},
    nodes::{LOCAL_NODE_ID, publication, relay, snapshots},
    run_status::RunStatus,
    service::Service,
    storage::{Disk, bootstrap, policy::Policy, runtime},
    store::Store,
};
use serde_json::{Value, json};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use tempfile::TempDir;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

const MIB: usize = 1024 * 1024;

#[tokio::test]
#[ignore = "explicit ext4 bootstrap benchmark on persistent host storage"]
async fn conversation_bootstrap_by_logical_disk_size() {
    let directory = std::env::var("LEO_BOOTSTRAP_BENCH_ROOT").unwrap_or_else(|_| "/var/tmp".into());
    let context = json!({
        "master": "http://127.0.0.1:1/",
        "grant": "fixture-bootstrap",
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
                .prefix("leo-bootstrap-bench-")
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

async fn service(root: &TempDir, origin: String, runner: String) -> Arc<Service> {
    std::fs::create_dir_all(root.path().join("home")).unwrap();
    if let Ok(endpoint) = std::env::var("LEO_NODE_TEST_S3_ENDPOINT") {
        let parsed: url::Url = endpoint.parse().unwrap();
        assert_eq!(parsed.host_str(), Some("127.0.0.1"));
        std::fs::create_dir_all(root.path().join("data")).unwrap();
        std::fs::write(
            root.path().join("data/storage-s3.json"),
            json!({ "bucket": "leo-node-test", "region": "us-east-1" }).to_string(),
        )
        .unwrap();
    }
    Service::new(Config {
        public_url: origin,
        codex_bin: "false".into(),
        claude_bin: "false".into(),
        gh_bin: "false".into(),
        concurrency: 4,
        runner_url: runner,
        ..common::config(root.path())
    })
    .await
    .unwrap()
}

#[tokio::test]
#[ignore = "explicit scale benchmark for backup lookup"]
async fn backup_lookup_with_many_conversations() {
    let root = TempDir::new().unwrap();
    let store = Store::open(root.path()).unwrap();
    store
        .transaction(|db| {
            for index in 0..20_000 {
                db.put(
                    "node-backups",
                    &json!({ "id": format!("other-{index}"), "runId": format!("run-{index}") }),
                )?;
            }
            db.put(
                "node-backups",
                &json!({ "id": "wanted", "runId": "wanted-run" }),
            )?;
            Ok(())
        })
        .await
        .unwrap();
    for sample in 0..3 {
        let started = Instant::now();
        let found = store.node_backups_for_run("wanted-run").await.unwrap();
        assert_eq!(found.len(), 1);
        let wall = started.elapsed().as_secs_f64() * 1000.0;
        let sample = json!({ "sample": sample, "records": 20_001, "wallMs": wall });
        println!("NODE_LOOKUP_PERF {sample}");
    }
}

#[tokio::test]
async fn backup_lookup_is_scoped_to_one_conversation() {
    let root = TempDir::new().unwrap();
    let store = Store::open(root.path()).unwrap();
    for (kind, id, run) in [
        ("node-backups", "a", "one"),
        ("node-backups", "b", "two"),
        ("other-kind", "c", "one"),
    ] {
        store
            .put(kind, json!({ "id": id, "runId": run }))
            .await
            .unwrap();
    }
    let found = store.node_backups_for_run("one").await.unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0]["id"], "a");
}

/// CPU seconds and bytes read by this process so far.
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

/// A controller that answers every request with `data`.
fn constant_controller(data: Bytes) -> Router {
    Router::new().fallback(move || {
        let data = data.clone();
        async move { data.into_response() }
    })
}

/// Serves the master router, delaying each request by `latency_ms`.
///
/// One injected round-trip wait per outbound request, not a bandwidth limit
/// or a TCP/WAN emulator. A streaming request pays it only once.
async fn delayed_master(s: &Arc<Service>, latency_ms: u64) -> Router {
    router(s.clone()).await.unwrap().layer(middleware::from_fn(
        move |request: Request, next: Next| async move {
            tokio::time::sleep(Duration::from_millis(latency_ms)).await;
            next.run(request).await
        },
    ))
}

#[tokio::test]
#[ignore = "explicit performance run; includes latency simulation"]
async fn outbound_transfer_with_ack_latency() {
    for latency_ms in [0, 50] {
        let root = TempDir::new().unwrap();
        let (listener, address) = common::bind().await;
        let origin = format!("http://{address}");
        let s = service(&root, origin.clone(), String::new()).await;
        let (node, token) = (id(), auth::token());
        s.store
            .put("nodes", json!({ "id": node, "revoked": false }))
            .await
            .unwrap();
        s.store
            .set(
                &format!("node-token:{}", auth::digest(&token)),
                json!(node),
                None,
            )
            .await
            .unwrap();
        let master = common::serve(listener, delayed_master(&s, latency_ms).await);
        let bytes = Bytes::from(vec![37u8; 4 * MIB]);
        let (controller, controller_task) =
            common::serve_locally(constant_controller(bytes.clone())).await;
        let stop = CancellationToken::new();
        let task = tokio::spawn(relay::run(
            origin.parse().unwrap(),
            token,
            controller,
            "controller-fixture".into(),
            stop.clone(),
        ));
        for sample in 0..3 {
            let before = usage();
            let started = Instant::now();
            let response = s
                .node_transport
                .request(
                    &node,
                    "GET",
                    &format!("/snapshots/{}/{}", id(), "a".repeat(64)),
                    vec![],
                )
                .await
                .unwrap();
            let received = axum::body::to_bytes(response.into_body(), bytes.len() + 1)
                .await
                .unwrap();
            let elapsed = started.elapsed();
            assert_eq!(received, bytes);
            let details = json!({
                "sample": sample,
                "latencyMs": latency_ms,
                "bytes": bytes.len(),
                "mibPerSecond": 4.0 / elapsed.as_secs_f64(),
            });
            report("outbound", elapsed, before, &details);
        }
        stop.cancel();
        task.await.unwrap().unwrap();
        master.abort();
        controller_task.abort();
    }
}

/// Indexes `disk` as a controller snapshot of the fixture runtime.
async fn controller_manifest(disk: &std::path::Path) -> Value {
    let mut manifest = snapshots::index(disk).await.unwrap();
    manifest["runtime"] = json!({ "runtimeId": "fixture" });
    manifest["capturedAt"] = now().into();
    manifest
}

/// Streams the blocks a publication or legacy batch names, in the requested order.
async fn serve_block_batch(
    source: &std::path::Path,
    manifest: Value,
    request: Request,
) -> axum::response::Response {
    let limit = if request.uri().path().ends_with("/publication") {
        snapshots::MAX_PUBLICATION_BLOCKS
    } else {
        snapshots::READ_BATCH
    };
    let body = axum::body::to_bytes(request.into_body(), 8192)
        .await
        .unwrap();
    let body: Value = serde_json::from_slice(&body).unwrap();
    let hashes = body["hashes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|hash| hash.as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    assert!(!hashes.is_empty() && hashes.len() <= limit);

    let block_size = |hash: &String| {
        manifest["blocks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|block| block["hash"] == *hash)
            .unwrap()["size"]
            .as_u64()
            .unwrap()
    };
    let length: u64 = hashes.iter().map(block_size).sum();

    let stream = futures_util::stream::try_unfold(
        (source.to_owned(), manifest, hashes.into_iter()),
        |(source, manifest, mut hashes)| async move {
            let Some(hash) = hashes.next() else {
                return Ok::<_, leo_agent_manager::error::Error>(None);
            };
            let bytes = snapshots::block(&source, &manifest, &hash).await?;
            Ok(Some((bytes, (source, manifest, hashes))))
        },
    );
    axum::http::Response::builder()
        .header("content-length", length)
        .body(axum::body::Body::from_stream(stream))
        .unwrap()
}

#[tokio::test]
#[ignore = "explicit performance run; writes a 64 MiB recovery fixture"]
async fn master_reuses_unchanged_blocks() {
    let root = TempDir::new().unwrap();
    let disk = root.path().join("disk");
    // Distinct nonzero blocks, so neither sparse holes nor cross-block dedup hide work.
    let mut bytes = vec![0; 64 * MIB];
    for (index, block) in bytes.chunks_mut(4 * MIB).enumerate() {
        block.fill(index as u8 + 1);
    }
    std::fs::write(&disk, &bytes).unwrap();
    let state = Arc::new(RwLock::new(controller_manifest(&disk).await));
    let (source, data) = (disk.clone(), state.clone());
    let app = Router::new().fallback(move |request: Request| {
        let (source, data) = (source.clone(), data.clone());
        async move {
            if request.method() == "DELETE" {
                return Json(json!({ "ok": true })).into_response();
            }
            let manifest = data.read().await.clone();
            if request.uri().path().ends_with("/snapshot") {
                return Json(json!({ "id": id(), "manifest": manifest })).into_response();
            }
            if request.uri().path().ends_with("/blocks")
                || request.uri().path().ends_with("/publication")
            {
                return serve_block_batch(&source, manifest, request).await;
            }
            snapshots::block(
                &source,
                &manifest,
                request.uri().path().rsplit('/').next().unwrap(),
            )
            .await
            .unwrap()
            .into_response()
        }
    });
    let (runner, task) = common::serve_locally(app).await;
    let s = service(&root, format!("http://{}", common::HOST), runner).await;
    let run = id();
    let record = json!({
        "id": run,
        "taskId": run,
        "createdAt": now(),
        "status": RunStatus::Running,
        "sessionId": "fixture",
    });
    common::add_run(&s.store, &record).await;
    common::set_checkpoint(
        &s.store,
        &run,
        json!({ "nodeId": LOCAL_NODE_ID, "runnerId": id() }),
    )
    .await;
    for kind in ["base", "unchanged", "delta"] {
        for sample in 0..if kind == "base" { 1 } else { 3 } {
            if kind == "delta" {
                bytes[..4 * MIB].fill(100 + sample);
                std::fs::write(&disk, &bytes).unwrap();
                *state.write().await = controller_manifest(&disk).await;
            }
            let before = usage();
            let started = Instant::now();
            let result = publication::capture(&s, &s.store.run(&run).await.unwrap())
                .await
                .unwrap();
            let details = json!({ "sample": sample, "uploadedBytes": result["uploadedBytes"] });
            report(kind, started.elapsed(), before, &details);
            if kind == "base" {
                let blocks = s
                    .config
                    .data_dir
                    .join("node-backups")
                    .join(&run)
                    .join("blocks");
                let stored_bytes: u64 = std::fs::read_dir(blocks)
                    .unwrap()
                    .map(|entry| entry.unwrap().metadata().unwrap().len())
                    .sum();
                let storage =
                    json!({ "plaintextBytes": bytes.len(), "storedBlockBytes": stored_bytes });
                println!("NODE_STORAGE {storage}");
            }
            assert_eq!(
                result["uploadedBytes"],
                match kind {
                    "base" => 64 * MIB,
                    "delta" => 4 * MIB,
                    _ => 0,
                }
            );
        }
    }
    // Byte-for-byte restoration is part of the benchmark, outside the timed captures.
    let latest = s.store.run(&run).await.unwrap();
    let backup = s
        .get("node-backups", latest["backup"]["id"].as_str().unwrap())
        .await
        .unwrap();
    let manifest = publication::manifest(&s, &backup).await.unwrap();
    snapshots::restore(&root.path().join("restored"), &manifest, |hash| {
        let (s, backup) = (s.clone(), backup.clone());
        async move { publication::read_block(&s, &backup, &hash).await }
    })
    .await
    .unwrap();
    assert_eq!(std::fs::read(root.path().join("restored")).unwrap(), bytes);
    task.abort();
}
