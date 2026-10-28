use super::*;
use crate::{
    object_storage::{
        HotS3,
        tests::{fixture_storage, fixture_storage_at},
    },
    storage::digest,
};
use axum::{
    Router,
    body::{Body, Bytes, to_bytes},
    extract::Request,
    http::StatusCode,
    response::IntoResponse,
};
use std::sync::atomic::{AtomicBool, Ordering};

async fn service(root: &Path) -> Arc<Service> {
    Service::new(crate::config::Config {
        data_dir: root.join("data"),
        home: root.join("home"),
        workspace_roots: vec![root.into()],
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
    .unwrap()
}

async fn peer(app: Router) -> (String, tokio::task::AbortHandle) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (url, server.abort_handle())
}

#[tokio::test]
async fn transfer_diagnostics_isolate_concurrent_attempts_and_conserve_volumes() {
    let root = tempfile::tempdir().unwrap();
    let s = service(root.path()).await;
    let fail = Arc::new(AtomicBool::new(false));
    let node = Router::new().fallback({
        let fail = fail.clone();
        move |request: Request| {
            let fail = fail.clone();
            async move {
                let input: Value =
                    serde_json::from_slice(&to_bytes(request.into_body(), 16384).await.unwrap())
                        .unwrap();
                assert!(input["diagnostic"]["attempt"].is_string());
                tokio::time::sleep(Duration::from_millis(25)).await;
                if fail.load(Ordering::Relaxed) {
                    return StatusCode::SERVICE_UNAVAILABLE.into_response();
                }
                let count = input["hashes"].as_array().unwrap().len();
                let stream = futures_util::stream::unfold(count, |left| async move {
                    if left == 0 {
                        return None;
                    }
                    tokio::time::sleep(Duration::from_millis(25)).await;
                    Some((
                        Ok::<_, std::io::Error>(Bytes::from(vec![42; 4096])),
                        left - 1,
                    ))
                });
                Body::from_stream(stream).into_response()
            }
        }
    });
    let (node_url, node_server) = peer(node).await;
    let (s3_url, s3_server) = peer(Router::new().fallback(|request: Request| async move {
        let bytes = to_bytes(request.into_body(), 8192).await.unwrap();
        assert!(!bytes.is_empty());
        tokio::time::sleep(Duration::from_millis(40)).await;
        StatusCode::OK
    }))
    .await;
    let storage = fixture_storage(&s3_url, "diagnostics", Arc::new(HotS3::new())).await;
    let run = json!({});
    let node = json!("remote");
    let snapshot = json!({ "publicationDiagnostics": true });
    let run_id = id();
    let snapshot_id = id();
    let attempt = id();
    let other_attempt = id();
    let objects = vec![
        shared_blocks::Object {
            id: id(),
            hash: digest::block(&vec![42; 4096]),
            size: 4096,
            ready: false,
        },
        shared_blocks::Object {
            id: id(),
            hash: digest::block(b"reused"),
            size: 6,
            ready: true,
        },
    ];
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
    let first = diagnostic::Metrics::new(&attempt, &run_id, &snapshot_id, "manager");
    let second = diagnostic::Metrics::new(&other_attempt, &run_id, &snapshot_id, "manager");
    let manifest = json!({});
    let (one, two) = tokio::join!(
        publication.transfer_observed(&first, &manifest, &objects),
        publication.transfer_observed(&second, &manifest, &objects),
    );
    assert_eq!(one.unwrap(), 4096);
    assert_eq!(two.unwrap(), 4096);
    for metrics in [&first, &second] {
        let value = metrics.snapshot();
        let stages = &value["stages"];
        for stage in [
            "nodeHeaders",
            "nodeBody",
            "compress",
            "encrypt",
            "hash",
            "putRequest",
            "putChecksum",
        ] {
            assert_eq!(stages[stage]["completed"], 1, "{stage}: {value}");
            assert_eq!(stages[stage]["inFlight"], 0);
        }
        assert_eq!(stages["nodeBody"]["bytes"], 4096);
        assert_eq!(stages["compress"]["bytes"], 4096);
        let encoded = stages["putRequest"]["bytes"].as_u64().unwrap();
        assert!(encoded > 0 && encoded < 4096);
        assert_eq!(stages["putChecksum"]["bytes"], encoded);
        assert!(stages["nodeHeaders"]["wallUsSum"].as_u64().unwrap() >= 20_000);
        assert!(stages["nodeBody"]["wallUsSum"].as_u64().unwrap() >= 20_000);
        assert!(stages["uploadDrain"]["wallUsSum"].as_u64().unwrap() >= 30_000);
        assert_eq!(
            value["observedUnionUs"].as_u64().unwrap() + value["unattributedUs"].as_u64().unwrap(),
            value["wallUs"].as_u64().unwrap()
        );
    }
    assert_ne!(first.attempt, second.attempt);
    fail.store(true, Ordering::Relaxed);
    let failed = diagnostic::Metrics::new(&id(), &run_id, &snapshot_id, "manager");
    assert!(
        publication
            .transfer_observed(&failed, &manifest, &objects)
            .await
            .is_err()
    );
    assert_eq!(
        failed.snapshot()["stages"]["nodeHeaders"]["completed"],
        1,
        "HTTP header receipt is distinct from status acceptance"
    );
    assert_eq!(failed.snapshot()["stages"]["putRequest"]["started"], 0);
    assert_eq!(first.snapshot()["stages"]["putRequest"]["completed"], 1);
    fail.store(false, Ordering::Relaxed);
    let cancelled = diagnostic::Metrics::new(&id(), &run_id, &snapshot_id, "manager");
    {
        let transfer = publication.transfer_observed(&cancelled, &manifest, &objects);
        tokio::pin!(transfer);
        tokio::time::timeout(Duration::from_secs(5), async {
            tokio::select! {
                result = &mut transfer => panic!("transfer completed before cancellation: {result:?}"),
                () = async {
                    while cancelled.snapshot()["stages"]["nodeBody"]["inFlight"] != 1 { tokio::task::yield_now().await; }
                } => {}
            }
        }).await.unwrap();
    }
    let cancelled = cancelled.snapshot();
    assert_eq!(cancelled["stages"]["nodeBody"]["failedOrCancelled"], 1);
    assert_eq!(cancelled["stages"]["nodeBody"]["inFlight"], 0);
    assert_eq!(cancelled["stages"]["putRequest"]["started"], 0);
    node_server.abort();
    s3_server.abort();
}

#[tokio::test]
async fn transfer_diagnostics_measure_base_get_decode_and_failed_put_without_losing_volume() {
    let root = tempfile::tempdir().unwrap();
    let s = service(root.path()).await;
    let object = shared_blocks::Object {
        id: id(),
        hash: digest::block(&vec![42; 4096]),
        size: 4096,
        ready: false,
    };
    let scope = shared_blocks::key(&object.hash, &object.id);
    let encoded = s
        .hot_s3
        .block_codec
        .encode(s.vault.clone(), scope, vec![42; 4096])
        .await
        .unwrap();
    let encoded_len = encoded.len();
    let fail = Arc::new(AtomicBool::new(false));
    let (url, server) = peer(Router::new().fallback({
        let fail = fail.clone();
        move |request: Request| {
            let encoded = encoded.clone();
            let fail = fail.clone();
            async move {
                tokio::time::sleep(Duration::from_millis(20)).await;
                if request.method() == "GET" {
                    return Body::from_stream(futures_util::stream::once(async move {
                        tokio::time::sleep(Duration::from_millis(20)).await;
                        Ok::<_, std::io::Error>(Bytes::from(encoded))
                    }))
                    .into_response();
                }
                if fail.load(Ordering::Relaxed) {
                    StatusCode::SERVICE_UNAVAILABLE.into_response()
                } else {
                    StatusCode::OK.into_response()
                }
            }
        }
    }))
    .await;
    let logical_endpoint = "https://diagnostic-fixture.invalid";
    let storage = fixture_storage_at(&url, logical_endpoint, "diagnostics", s.hot_s3.clone()).await;
    tokio::fs::write(
        s.config.data_dir.join("storage-s3.json"),
        json!({ "bucket": "diagnostics", "region": "us-east-1", "endpoint": logical_endpoint })
            .to_string(),
    )
    .await
    .unwrap();
    let run = json!({});
    let node = json!("remote");
    let snapshot = json!({});
    let run_id = id();
    let snapshot_id = id();
    let attempt = id();
    let publication = Publication {
        s: &s,
        run: &run,
        run_id: &run_id,
        attempt: "",
        publication_attempt: &attempt,
        node: &node,
        storage: &storage,
        base: "http://127.0.0.1:1",
        credential: "fixture",
        snapshot: &snapshot,
        snapshot_id: &snapshot_id,
        budget: 0,
    };
    let backup = Arc::new(
        json!({ "destination": "s3", "endpoint": logical_endpoint, "bucket": "diagnostics", "runId": run_id }),
    );
    let block = json!({ "hash": object.hash, "object": object.id, "size": object.size });
    let sources = HashMap::from([(object.hash.clone(), (backup, block))]);
    let metrics = diagnostic::Metrics::new(&attempt, &run_id, &snapshot_id, "manager");
    for fails in [false, true] {
        fail.store(fails, Ordering::Relaxed);
        let metrics = if fails {
            diagnostic::Metrics::new(&id(), &run_id, &snapshot_id, "manager")
        } else {
            metrics.clone()
        };
        diagnostic::scope(Some(metrics.clone()), async {
            let mut reads = snapshots::Fetch::new(
                s.disk_http.clone(),
                "http://127.0.0.1:1".into(),
                "fixture".into(),
                vec![],
            );
            let mut uploads = JoinSet::new();
            assert_eq!(
                publication
                    .start_uploads(
                        std::slice::from_ref(&object),
                        &sources,
                        &mut reads,
                        &mut uploads
                    )
                    .await
                    .unwrap(),
                4096
            );
            let result = uploads.join_next().await.unwrap().unwrap();
            assert_eq!(result.is_err(), fails);
        })
        .await;
        let value = metrics.snapshot();
        let stages = &value["stages"];
        for stage in [
            "baseRead",
            "getBody",
            "decrypt",
            "decompress",
            "compress",
            "encrypt",
        ] {
            assert_eq!(stages[stage]["completed"], 1, "{stage}: {value}");
        }
        assert_eq!(stages["baseRead"]["bytes"], 4096);
        assert_eq!(stages["getBody"]["bytes"], encoded_len);
        assert_eq!(stages["decompress"]["bytes"], 4096);
        assert_eq!(stages["nodeHeaders"]["started"], 0);
        assert_eq!(stages["putRequest"]["failedOrCancelled"], u64::from(fails));
        assert!(stages["getHeaders"]["wallUsSum"].as_u64().unwrap() >= 15_000);
        assert!(stages["getBody"]["wallUsSum"].as_u64().unwrap() >= 15_000);
    }
    server.abort();
}

#[derive(Clone)]
struct LogWriter(Arc<std::sync::Mutex<Vec<u8>>>);

impl std::io::Write for LogWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn default_logging_filter_correlates_pretransfer_failure_and_cancelled_summary() {
    use tracing::instrument::WithSubscriber;
    let root = tempfile::tempdir().unwrap();
    let s = service(root.path()).await;
    let bytes = Arc::new(std::sync::Mutex::new(Vec::new()));
    let output = bytes.clone();
    let subscriber = tracing_subscriber::fmt()
        .json()
        .with_env_filter("warn,cairn_performance=info")
        .with_writer(move || LogWriter(output.clone()))
        .finish();
    async {
        assert!(capture_owned(&s, &json!({ "id": id() })).await.is_err());
        let metrics =
            diagnostic::Metrics::new(&id(), "credential-not-a-uuid", "path-not-a-uuid", "manager");
        let _summary = diagnostic::Summary {
            metrics,
            outcome: "cancelled",
        };
    }
    .with_subscriber(subscriber)
    .await;
    let logs = String::from_utf8(bytes.lock().unwrap().clone()).unwrap();
    let rows: Vec<Value> = logs
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let phase = rows
        .iter()
        .find(|row| {
            row["fields"]["operation"] == "disk_publication"
                && row["fields"]["event"] == "incomplete"
        })
        .unwrap();
    let attempt = phase["span"]["publication_attempt"].as_str().unwrap();
    crate::validation::uuid(attempt).unwrap();
    assert_eq!(
        rows.iter()
            .find(|row| row["fields"]["operation"] == "s3_totals")
            .unwrap()["fields"]["publication_attempt"],
        attempt
    );
    let summary = rows
        .iter()
        .find(|row| row["fields"]["operation"] == "disk_publication_transfer")
        .unwrap();
    assert_eq!(summary["fields"]["outcome"], "cancelled");
    assert_eq!(summary["fields"]["run_id"], "unknown");
    assert!(!logs.contains("credential-not-a-uuid"));
    assert!(!logs.contains("path-not-a-uuid"));
}

#[tokio::test]
async fn converted_publications_have_distinct_attempt_summaries_on_success_and_failure() {
    use tracing::instrument::WithSubscriber;
    let root = tempfile::tempdir().unwrap();
    let s = service(root.path()).await;
    let fail_put = Arc::new(AtomicBool::new(false));
    let requests = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let (endpoint, server) = peer(Router::new().fallback({
        let fail_put = fail_put.clone();
        let requests = requests.clone();
        move |request: Request| {
            let fail_put = fail_put.clone();
            let requests = requests.clone();
            async move {
                let _ = to_bytes(request.into_body(), 32768).await.unwrap();
                requests.fetch_add(1, Ordering::Relaxed);
                if fail_put.load(Ordering::Relaxed) {
                    (
                        StatusCode::BAD_REQUEST,
                        "<Error><Code>BadDigest</Code><Message>fixture</Message></Error>",
                    )
                        .into_response()
                } else {
                    StatusCode::OK.into_response()
                }
            }
        }
    }))
    .await;
    let logical_endpoint = "https://converted-fixture.invalid";
    let _storage =
        fixture_storage_at(&endpoint, logical_endpoint, "diagnostics", s.hot_s3.clone()).await;
    tokio::fs::write(
        s.config.data_dir.join("storage-s3.json"),
        json!({
            "bucket": "diagnostics", "region": "us-east-1", "endpoint": logical_endpoint
        })
        .to_string(),
    )
    .await
    .unwrap();
    let bytes = Arc::new(std::sync::Mutex::new(Vec::new()));
    let output = bytes.clone();
    let subscriber = tracing_subscriber::fmt()
        .json()
        .with_env_filter("warn,cairn_performance=info")
        .with_writer(move || LogWriter(output.clone()))
        .finish();
    let dispatch = tracing::Dispatch::new(subscriber);
    for (case, value) in [42, 43, 44].into_iter().enumerate() {
        let run = json!({
            "id": id(), "taskId": id(), "status": "interrupted", "createdAt": 0,
            "backendMigration": { "id": id() }
        });
        let saved = run.clone();
        s.store
            .transaction(move |db| db.add_run_fixture(&saved, None))
            .await
            .unwrap();
        let disk = root.path().join(format!("converted-{case}"));
        tokio::fs::write(&disk, vec![value; 4096]).await.unwrap();
        let manifest = snapshots::index(&disk).await.unwrap();
        if case == 2 {
            tokio::fs::write(&disk, vec![45; 4096]).await.unwrap();
        }
        fail_put.store(case == 1, Ordering::Relaxed);
        let before = requests.load(Ordering::Relaxed);
        let node = json!({ "id": super::super::LOCAL_NODE_ID });
        let result = converted_environment(&s, &run, &node, &disk, manifest)
            .with_subscriber(dispatch.clone())
            .await;
        match case {
            0 => {
                assert_eq!(result.unwrap()["uploadedBytes"], 4096);
                assert_eq!(requests.load(Ordering::Relaxed) - before, 2);
            }
            1 => {
                assert!(result.is_err());
                assert_eq!(requests.load(Ordering::Relaxed) - before, 1);
            }
            _ => {
                assert_eq!(
                    result.unwrap_err().message,
                    "Converted disk changed during publication."
                );
                assert_eq!(requests.load(Ordering::Relaxed), before);
            }
        }
    }
    let logs = String::from_utf8(bytes.lock().unwrap().clone()).unwrap();
    let rows: Vec<Value> = logs
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let summaries: Vec<_> = rows
        .iter()
        .filter(|row| row["fields"]["operation"] == "disk_publication_transfer")
        .collect();
    assert_eq!(summaries.len(), 3, "{logs}");
    let mut attempts = HashSet::new();
    for (case, row) in summaries.iter().enumerate() {
        let fields = &row["fields"];
        let attempt = fields["publication_attempt"].as_str().unwrap();
        crate::validation::uuid(attempt).unwrap();
        assert!(attempts.insert(attempt));
        assert_eq!(row["span"]["publication_attempt"], attempt);
        assert_eq!(row["span"]["source"], "converted_environment");
        assert_eq!(
            fields["outcome"],
            if case == 0 { "success" } else { "error" }
        );
        let metrics: Value = serde_json::from_str(fields["metrics"].as_str().unwrap()).unwrap();
        let stages = &metrics["stages"];
        assert_eq!(metrics["plannedPlaintextBytes"], 4096);
        assert_eq!(stages["convertedRead"]["completed"], 1);
        assert_eq!(stages["convertedRead"]["bytes"], 4096);
        assert_eq!(stages["hash"]["completed"], 1);
        assert_eq!(stages["nodeHeaders"]["started"], 0);
        assert_eq!(
            stages["compress"]["completed"],
            if case == 2 { 0 } else { 1 }
        );
        assert_eq!(stages["putRequest"]["completed"], u64::from(case == 0));
        assert_eq!(
            stages["putRequest"]["failedOrCancelled"],
            u64::from(case == 1)
        );
    }
    assert!(!logs.contains(logical_endpoint));
    server.abort();
}
