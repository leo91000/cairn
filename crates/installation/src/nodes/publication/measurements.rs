//! Opt-in local measurements; run alone with the same fixture on the PR base.
use super::*;
use crate::{object_storage::tests::fixture_storage, storage::Disk};
use axum::{Router, body::to_bytes, extract::Request, http::StatusCode, response::IntoResponse};
use rand::{RngCore, SeedableRng};
use std::sync::atomic::{AtomicU64, Ordering};

fn cpu_ms() -> f64 {
    let mut time = std::mem::MaybeUninit::<libc::timespec>::uninit();
    assert_eq!(
        unsafe { libc::clock_gettime(libc::CLOCK_PROCESS_CPUTIME_ID, time.as_mut_ptr()) },
        0
    );
    let time = unsafe { time.assume_init() };
    time.tv_sec as f64 * 1000.0 + time.tv_nsec as f64 / 1_000_000.0
}

async fn serve(app: Router) -> (String, tokio::task::AbortHandle) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (url, server.abort_handle())
}

#[tokio::test]
#[ignore = "explicit publication overhead measurement; run alone with --nocapture --test-threads=1"]
async fn publication_transfer_overhead_measurement() {
    tracing::subscriber::set_global_default(
        tracing_subscriber::fmt()
            .with_env_filter("warn,cairn_performance=info")
            .with_ansi(false)
            .with_writer(std::io::sink)
            .finish(),
    )
    .unwrap();

    let root = tempfile::tempdir().unwrap();
    let s = crate::service::Service::new(crate::config::Config {
        data_dir: root.path().join("data"),
        home: root.path().join("home"),
        workspace_roots: vec![root.path().into()],
        public_url: "http://localhost:4310".into(),
        host: "127.0.0.1".into(),
        port: 0,
        codex_bin: "false".into(),
        claude_bin: "false".into(),
        opencode_bin: "false".into(),
        opencode_go_usage_url: crate::accounts::opencode::USAGE_URL.into(),
        concurrency: 1,
        logger: false,
        worker_enabled: false,
        runner_url: String::new(),
    })
    .await
    .unwrap();

    let count = 16;
    let run_id = id();
    let directory = root.path().join("disks").join(&run_id);
    let base = json!({
        "version": 1,
        "size": count * snapshots::BLOCK,
        "blockSize": snapshots::BLOCK,
        "blocks": (0..count).map(|index| json!({
            "offset": index * snapshots::BLOCK,
            "size": snapshots::BLOCK,
            "hash": null
        })).collect::<Vec<_>>()
    });
    let source = json!({
        "master": "http://127.0.0.1:9/",
        "grant": "fixture"
    });
    let disk = crate::storage::runtime::create(&directory.join("lazy"), &base, &source)
        .await
        .unwrap();
    let mut random = vec![0; snapshots::BLOCK as usize];
    rand::rngs::StdRng::seed_from_u64(42).fill_bytes(&mut random);

    for index in 0..count {
        let mut bytes = if index % 2 == 0 {
            vec![37; snapshots::BLOCK as usize]
        } else {
            random.clone()
        };
        bytes[..8].copy_from_slice(&index.to_le_bytes());
        disk.write_at(index * snapshots::BLOCK, &bytes).unwrap();
    }

    let generation = disk.seal().unwrap();
    let mut manifest = disk.capture(generation).unwrap();
    manifest["onDemand"] = true.into();
    manifest["generation"] = generation.into();
    drop(disk);

    let snapshot_id = id();
    let captured = root.path().join("snapshots").join(&snapshot_id);
    tokio::fs::create_dir_all(&captured).await.unwrap();
    tokio::fs::write(captured.join("run"), &run_id)
        .await
        .unwrap();
    tokio::fs::write(captured.join("manifest.json"), manifest.to_string())
        .await
        .unwrap();
    let objects: Vec<_> = manifest["blocks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|block| shared_blocks::Object {
            id: id(),
            hash: text(block, "hash").to_owned(),
            size: snapshots::BLOCK,
            ready: false,
        })
        .collect();
    let hashes: Vec<_> = objects.iter().map(|object| object.hash.clone()).collect();

    let node_snapshot = snapshot_id.clone();
    let captured_node = captured.clone();
    let (node_url, node_server) = serve(Router::new().fallback(move |request: Request| {
        let captured = captured_node.clone();
        let snapshot = node_snapshot.clone();
        async move {
            let input: Value =
                serde_json::from_slice(&to_bytes(request.into_body(), 65536).await.unwrap())
                    .unwrap();
            let hashes = serde_json::from_value(input["hashes"].clone()).unwrap();
            assert!(input["diagnostic"].is_object());
            let diagnostic = input
                .get("diagnostic")
                .cloned()
                .map(serde_json::from_value)
                .transpose()
                .unwrap();
            let (_, body) =
                snapshots::served_publication_observed(&captured, hashes, diagnostic, &snapshot)
                    .await
                    .unwrap();
            body.into_response()
        }
    }))
    .await;

    let encoded_bytes = Arc::new(AtomicU64::new(0));
    let received = encoded_bytes.clone();
    let (s3_url, s3_server) = serve(Router::new().fallback(move |request: Request| {
        let received = received.clone();
        async move {
            let bytes = to_bytes(request.into_body(), 5 * 1024 * 1024)
                .await
                .unwrap();
            received.fetch_add(bytes.len() as u64, Ordering::Relaxed);
            StatusCode::OK
        }
    }))
    .await;

    let storage = fixture_storage(&s3_url, "measurement", s.hot_s3.clone()).await;
    let run = json!({});
    let node = json!("remote");
    let snapshot = json!({ "publicationDiagnostics": true });

    for trial in 0..8 {
        let attempt = id();
        let publication = Publication {
            s: &s,
            run: &run,
            run_id: &run_id,
            attempt: "",
            publication_attempt: &attempt,
            node: &node,
            storage: &storage,
            base: &node_url,
            credential: "fixture",
            snapshot: &snapshot,
            snapshot_id: &snapshot_id,
            budget: 0,
        };

        let mut reads = Vec::new();
        for warm in [false, true] {
            let held = if warm {
                Some(crate::storage::runtime::load(&directory).await.unwrap())
            } else {
                None
            };
            let cpu = cpu_ms();
            let start = std::time::Instant::now();
            let (_, body) = snapshots::served_publication(&captured, hashes.clone())
                .await
                .unwrap();
            let bytes = to_bytes(body, count as usize * snapshots::BLOCK as usize)
                .await
                .unwrap();
            let wall = start.elapsed().as_secs_f64() * 1000.0;
            let cpu = cpu_ms() - cpu;
            assert_eq!(bytes.len() as u64, count * snapshots::BLOCK);
            reads.push(json!({
                "warmReader": warm,
                "wallMs": wall,
                "processCpuMs": cpu
            }));
            drop(held);
        }

        encoded_bytes.store(0, Ordering::Relaxed);
        let cpu = cpu_ms();
        let start = std::time::Instant::now();
        let uploaded = publication
            .transfer_blocks(&manifest, &objects)
            .await
            .unwrap();
        let wall = start.elapsed().as_secs_f64() * 1000.0;
        let cpu = cpu_ms() - cpu;
        assert_eq!(uploaded, count * snapshots::BLOCK);
        println!(
            "PUBLICATION_TRANSFER_PERF {}",
            json!({
                "trial": trial,
                "warmup": trial < 2,
                "plaintextBytes": uploaded,
                "encodedBytes": encoded_bytes.load(Ordering::Relaxed),
                "blocks": count,
                "transferMs": wall,
                "processCpuMs": cpu,
                "reads": reads
            })
        );
    }

    node_server.abort();
    s3_server.abort();
}
