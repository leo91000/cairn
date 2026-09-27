//! Repeatable benchmarks through the shipped backup and outbound transport interfaces.
//! Run alone with --ignored --nocapture --test-threads=1. All identities/data are fixtures.
use axum::{extract::Request, response::IntoResponse};
use leo_agent_manager::{
    auth,
    config::{Config, id, now},
    http::router,
    nodes::{LOCAL_NODE_ID, backups, relay, snapshots},
    service::Service,
};
use serde_json::{Value, json};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use tempfile::TempDir;
use tokio::{net::TcpListener, sync::RwLock};

async fn service(root: &TempDir, origin: String, runner: String) -> Arc<Service> {
    std::fs::create_dir_all(root.path().join("home")).unwrap();
    Service::new(Config {
        data_dir: root.path().join("data"),
        home: root.path().join("home"),
        workspace_roots: vec![root.path().into()],
        public_url: origin,
        host: "127.0.0.1".into(),
        port: 0,
        setup_token: "fixture".into(),
        codex_bin: "false".into(),
        claude_bin: "false".into(),
        gh_bin: "false".into(),
        concurrency: 4,
        logger: false,
        worker_enabled: false,
        runner_url: runner,
    })
    .await
    .unwrap()
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

fn report(kind: &str, elapsed: Duration, before: (f64, u64), extra: Value) {
    let after = usage();
    println!(
        "NODE_PERF {}",
        json!({"scenario":kind,"wallMs":elapsed.as_secs_f64()*1000.0,
        "cpuMs":(after.0-before.0)*1000.0,"processReadBytes":after.1-before.1,"details":extra})
    );
}

#[tokio::test]
#[ignore = "explicit performance run; includes latency simulation"]
async fn outbound_transfer_with_ack_latency() {
    for latency_ms in [0, 50] {
        let root = TempDir::new().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let s = service(&root, origin.clone(), String::new()).await;
        let (node, token) = (id(), auth::token());
        s.store
            .put("nodes", json!({"id":node,"revoked":false}))
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
        // One injected round-trip wait per outbound request, not a bandwidth limit
        // or a TCP/WAN emulator. A streaming request pays it only once.
        let app = router(s.clone())
            .await
            .unwrap()
            .layer(axum::middleware::from_fn(
                move |request: Request, next: axum::middleware::Next| async move {
                    tokio::time::sleep(Duration::from_millis(latency_ms)).await;
                    next.run(request).await
                },
            ));
        let master = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let bytes = bytes::Bytes::from(vec![37u8; 4 * 1024 * 1024]);
        let served = bytes.clone();
        let fixture = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let controller = format!("http://{}", fixture.local_addr().unwrap());
        let app = axum::Router::new().fallback(move || {
            let data = served.clone();
            async move { data.into_response() }
        });
        let controller = (
            controller,
            tokio::spawn(async move { axum::serve(fixture, app).await.unwrap() }),
        );
        let stop = tokio_util::sync::CancellationToken::new();
        let task = tokio::spawn(relay::run(
            origin.parse().unwrap(),
            token,
            controller.0,
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
            report(
                "outbound",
                elapsed,
                before,
                json!({"sample":sample,"latencyMs":latency_ms,
                "bytes":bytes.len(),"mibPerSecond":4.0/elapsed.as_secs_f64()}),
            );
        }
        stop.cancel();
        task.await.unwrap().unwrap();
        master.abort();
        controller.1.abort();
    }
}

#[tokio::test]
#[ignore = "explicit performance run; writes a 64 MiB recovery fixture"]
async fn master_reuses_unchanged_blocks() {
    let root = TempDir::new().unwrap();
    let disk = root.path().join("disk");
    // Distinct nonzero blocks, so neither sparse holes nor cross-block dedup hide work.
    let mut bytes = vec![0; 64 * 1024 * 1024];
    for (index, block) in bytes.chunks_mut(4 * 1024 * 1024).enumerate() {
        block.fill(index as u8 + 1);
    }
    std::fs::write(&disk, &bytes).unwrap();
    let mut manifest = snapshots::index(&disk).await.unwrap();
    manifest["runtime"] = json!({"runtimeId":"fixture"});
    manifest["capturedAt"] = now().into();
    let state = Arc::new(RwLock::new(manifest));
    let (source, data) = (disk.clone(), state.clone());
    let app = axum::Router::new().fallback(move |request: Request| {
        let (source, data) = (source.clone(), data.clone());
        async move {
            if request.method() == "DELETE" {
                return axum::Json(json!({"ok":true})).into_response();
            }
            let manifest = data.read().await.clone();
            if request.uri().path().ends_with("/snapshot") {
                return axum::Json(json!({"id":id(),"manifest":manifest})).into_response();
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
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let runner = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let s = service(&root, "http://localhost:4310".into(), runner).await;
    let run = id();
    let record =
        json!({"id":run,"taskId":run,"createdAt":now(),"status":"running","sessionId":"fixture"});
    let saved = record.clone();
    s.store
        .write(move |db| db.add_run(&saved, None))
        .await
        .unwrap();
    s.store
        .set(
            &format!("run-checkpoint:{run}"),
            json!({"nodeId":LOCAL_NODE_ID,"runnerId":id()}),
            None,
        )
        .await
        .unwrap();
    for kind in ["base", "unchanged", "delta"] {
        for sample in 0..if kind == "base" { 1 } else { 3 } {
            if kind == "delta" {
                bytes[..4 * 1024 * 1024].fill(100 + sample);
                std::fs::write(&disk, &bytes).unwrap();
                let mut manifest = snapshots::index(&disk).await.unwrap();
                manifest["runtime"] = json!({"runtimeId":"fixture"});
                manifest["capturedAt"] = now().into();
                *state.write().await = manifest;
            }
            let before = usage();
            let started = Instant::now();
            let result = backups::capture(&s, &s.store.run(&run).await.unwrap())
                .await
                .unwrap();
            report(
                kind,
                started.elapsed(),
                before,
                json!({"sample":sample,"uploadedBytes":result["uploadedBytes"]}),
            );
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
                println!(
                    "NODE_STORAGE {}",
                    json!({"plaintextBytes":bytes.len(),"storedBlockBytes":stored_bytes})
                );
            }
            assert_eq!(
                result["uploadedBytes"],
                match kind {
                    "base" => 64 * 1024 * 1024,
                    "delta" => 4 * 1024 * 1024,
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
    let manifest = backups::manifest(&s, &backup).await.unwrap();
    snapshots::restore(&root.path().join("restored"), &manifest, |hash| {
        let (s, backup) = (s.clone(), backup.clone());
        async move { backups::read_block(&s, &backup, &hash).await }
    })
    .await
    .unwrap();
    assert_eq!(std::fs::read(root.path().join("restored")).unwrap(), bytes);
    task.abort();
}
