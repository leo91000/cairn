use super::*;
use crate::config::Config;
use std::io::Write;

fn started(method: &str) -> String {
    format!(
        "2026-11-01T00:19:49Z INFO agent_attempt{{run_id=\"untrusted-span\"}}: cairn_performance: operation=\"codex_rpc\" method=\"{method}\" request_id=1 event=\"started\" timeout_ms=20000\n"
    )
}

fn completed(elapsed: u64, succeeded: bool) -> String {
    format!(
        "2026-11-01T00:19:49Z INFO cairn_performance: operation=\"codex_rpc\" method=\"thread/items/list\" request_id=1 event=\"completed\" elapsed_ms={elapsed} queue_ms=0 timeout_ms=20000 succeeded={succeeded}\n"
    )
}

fn publisher_fixture() -> (Publisher, watch::Receiver<Snapshot>) {
    let (sender, receiver) = watch::channel(Snapshot::default());
    (
        Publisher {
            snapshot: Snapshot::default(),
            sender,
            started: Instant::now(),
        },
        receiver,
    )
}

#[test]
fn schema_is_closed_and_does_not_retain_child_text() {
    assert!(matches!(
        parse(&started("thread/items/list")),
        Some(Sample::Started(Method::Items))
    ));
    assert!(matches!(
        parse(&started("thread/turns/list")),
        Some(Sample::Started(Method::Turns))
    ));
    assert!(matches!(
        parse(&completed(12345, false)),
        Some(Sample::Completed {
            elapsed_ms: 12345,
            succeeded: false,
            ..
        })
    ));
    let valid = started("thread/items/list");
    for unknown in [
        started("arbitrary-sensitive-method"),
        valid.replace("INFO", "WARN"),
        valid.replace("started", "untrusted-phase"),
        valid.replace(
            "timeout_ms=20000",
            "timeout_ms=20000 secret=untrusted-value",
        ),
        valid.replace("request_id=1", "request_id=18446744073709551616"),
        valid.trim_end().to_owned(),
        format!("{valid}extra"),
        completed(MAX_DURATION_MS + 1, true),
        completed(1, true).replace("elapsed_ms=1", "elapsed_ms=-1"),
        completed(1, true).replace("succeeded=true", "succeeded=unknown"),
        format!(
            "{}{}\n",
            valid.trim_end(),
            " ".repeat(DIAGNOSTIC_FRAGMENT_LIMIT)
        ),
    ] {
        assert!(parse(&unknown).is_none());
    }
    let missing_queue = completed(1, false).replace("queue_ms=0 ", "");
    assert!(matches!(
        parse(&missing_queue),
        Some(Sample::Completed { queue_ms: None, .. })
    ));
    let (mut publisher, receiver) = publisher_fixture();
    publisher.observe(&valid);
    let serialized = serde_json::to_string(&*receiver.borrow()).unwrap();
    assert!(!serialized.contains("untrusted-span"));
    assert!(!serialized.contains("request_id"));
    assert!(!serialized.contains("2026-11"));
}

#[test]
fn counters_are_cumulative_saturating_and_coverage_is_explicit() {
    let (mut publisher, receiver) = publisher_fixture();
    for _ in 0..6090 {
        assert!(publisher.observe(&started("thread/items/list")));
        assert!(publisher.observe(&completed(1, true)));
    }
    assert!(publisher.observe(&completed(500, false)));
    assert!(!publisher.observe("unrecognized"));
    let snapshot = *receiver.borrow();
    assert_eq!(snapshot.recognized_records, 12181);
    assert_eq!(snapshot.items.started, 6090);
    assert_eq!(snapshot.items.completed, 6091);
    assert_eq!(snapshot.items.failed, 1);
    assert_eq!(snapshot.items.slow, 1);
    assert_eq!(snapshot.items.elapsed_ms_total, 6590);
    assert_eq!(snapshot.items.elapsed_ms_min, Some(1));
    assert_eq!(snapshot.items.elapsed_ms_max, 500);
    assert_eq!(snapshot.unsupported_records, 1);
    assert!(!snapshot.reader_closed);
    assert!(!snapshot.eof);
    publisher.snapshot.items.started = u64::MAX;
    publisher.observe(&started("thread/items/list"));
    assert_eq!(receiver.borrow().items.started, u64::MAX);
    assert!(receiver.borrow().overflow);
    publisher.finish(false);
    assert!(receiver.borrow().reader_closed);
    assert!(!receiver.borrow().eof);
    // A later well-formed reader's EOF is a different scoped observation.
    let (mut complete, receiver) = publisher_fixture();
    complete.finish(true);
    assert!(receiver.borrow().eof);
    assert!(serde_json::to_vec(&*receiver.borrow()).unwrap().len() < 4096);
}

#[tokio::test]
async fn actual_reader_marks_eof_errors_and_unsupported_fragments_distinctly() {
    struct Reader<'a> {
        bytes: &'a [u8],
        error: bool,
    }
    impl tokio::io::AsyncRead for Reader<'_> {
        fn poll_read(
            mut self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
            buffer: &mut tokio::io::ReadBuf<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            if self.bytes.is_empty() {
                return std::task::Poll::Ready(if self.error {
                    Err(std::io::Error::other("synthetic read failure"))
                } else {
                    Ok(())
                });
            }
            let count = buffer.remaining().min(self.bytes.len());
            let bytes = self.bytes;
            buffer.put_slice(&bytes[..count]);
            self.bytes = &bytes[count..];
            std::task::Poll::Ready(Ok(()))
        }
    }
    for error in [false, true] {
        let input = started("thread/items/list");
        let (publisher, receiver) = publisher_fixture();
        let (sender, mut events) = tokio::sync::mpsc::channel(32);
        super::super::read_diagnostics(
            Reader {
                bytes: input.as_bytes(),
                error,
            },
            sender,
            Some(publisher),
        )
        .await;
        assert!(events.recv().await.is_none());
        assert_eq!(receiver.borrow().recognized_records, 1);
        assert!(receiver.borrow().reader_closed);
        assert_eq!(receiver.borrow().eof, !error);
    }
    let input = started("thread/items/list");
    let (publisher, receiver) = publisher_fixture();
    let (sender, mut events) = tokio::sync::mpsc::channel(32);
    super::super::read_diagnostics(input.trim_end().as_bytes(), sender, Some(publisher)).await;
    let output = events.recv().await.unwrap();
    assert_eq!(output.kind, super::super::OutputKind::Performance);
    assert_eq!(receiver.borrow().recognized_records, 0);
    assert_eq!(receiver.borrow().unsupported_records, 1);
    assert!(receiver.borrow().eof);
}

#[derive(Clone)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl Write for Capture {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn capture() -> (Capture, tracing::Dispatch) {
    let capture = Capture(Arc::default());
    let writer = capture.clone();
    let dispatch = tracing::Dispatch::new(
        tracing_subscriber::fmt()
            .with_ansi(false)
            .without_time()
            .with_writer(move || writer.clone())
            .finish(),
    );
    (capture, dispatch)
}

async fn service() -> (tempfile::TempDir, Arc<Service>) {
    let root = tempfile::tempdir().unwrap();
    let service = Service::new(Config {
        data_dir: root.path().join("data"),
        home: root.path().join("home"),
        workspace_roots: vec![root.path().into()],
        public_url: "http://localhost:4310".into(),
        host: "127.0.0.1".into(),
        port: 0,
        codex_bin: "unused".into(),
        claude_bin: "unused".into(),
        opencode_bin: "unused".into(),
        opencode_go_usage_url: crate::accounts::opencode::USAGE_URL.into(),
        concurrency: 1,
        logger: false,
        worker_enabled: false,
        runner_url: String::new(),
    })
    .await
    .unwrap();
    (root, service)
}

fn lease(root: &std::path::Path) -> Lease {
    Lease {
        account_id: crate::config::id(),
        provider: crate::provider::Provider::Codex,
        run_id: crate::config::id(),
        home: root.into(),
        model: "fixture".into(),
    }
}

fn reporter(service: Arc<Service>, receiver: watch::Receiver<Snapshot>) -> Reporter {
    Reporter {
        service,
        secrets: Vec::new(),
        redactions: Arc::default(),
        receiver,
        stop: CancellationToken::new(),
        run: crate::config::id(),
        attempt: crate::config::id(),
    }
}

#[tokio::test]
async fn numeric_rotations_and_canonical_observed_intermediate_secrets_are_redacted() {
    let _registration = tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default());
    let (_root, service) = service().await;
    let account = lease(_root.path());
    let key = format!("codex-account:{}", account.account_id);
    service
        .vault
        .set(
            &key,
            &serde_json::json!({ "tokens": { "access_token": "19873" } }),
        )
        .await
        .unwrap();
    let (mut publisher, receiver) = publisher_fixture();
    publisher.observe(&completed(17381, true));
    publisher.observe(&completed(19873, true));
    publisher.finish(true);
    let reporter = reporter(service.clone(), receiver);
    // Canonical output saw B; the Vault is already C when the reporter refreshes.
    let mut canonical_known = vec!["17381".into()];
    reporter.redactions.merge(&mut canonical_known);
    let (capture, dispatch) = capture();
    let refreshing_service = service.clone();
    reporter
        .run(move || {
            let service = refreshing_service.clone();
            let account = account.clone();
            async move { service.accounts.redactions(&service, &account).await }
        })
        .with_subscriber(dispatch)
        .await;
    assert!(crate::performance::flush(Duration::from_secs(5)));
    let output = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
    assert!(output.contains("agent_rpc_summary"));
    assert!(output.contains("[redacted]"));
    assert!(!output.contains("17381"));
    assert!(!output.contains("19873"));
    assert!(output.contains("reader_closed\\\":true") || output.contains("reader_closed\":true"));
    service.shutdown.cancel();
}

#[tokio::test]
async fn pending_refresh_holds_no_redaction_lock_and_cancellation_releases_reporter() {
    let _registration = tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default());
    let (_root, service) = service().await;
    let (mut publisher, receiver) = publisher_fixture();
    let reporter = reporter(service.clone(), receiver);
    let secrets = reporter.redactions.clone();
    let stop = reporter.stop.clone();
    let entered = CancellationToken::new();
    let release = CancellationToken::new();
    let entered_task = entered.clone();
    let release_task = release.clone();
    let (capture, dispatch) = capture();
    let task = tokio::spawn(
        reporter
            .run(move || {
                let entered = entered_task.clone();
                let release = release_task.clone();
                async move {
                    entered.cancel();
                    release.cancelled().await;
                    Ok(vec!["not-yet-observed".into()])
                }
            })
            .with_subscriber(dispatch),
    );
    publisher.observe(&started("thread/items/list"));
    entered.cancelled().await;
    // Canonical refresh can merge independently while reporter I/O is pending.
    let mut canonical = vec!["canonical-rotation".into()];
    secrets.merge(&mut canonical);
    assert_eq!(canonical, ["canonical-rotation"]);
    stop.cancel();
    task.await.unwrap();
    assert!(!release.is_cancelled());
    assert!(capture.0.lock().unwrap().is_empty());
    service.shutdown.cancel();
}

#[tokio::test]
async fn redaction_failure_has_only_a_static_fail_closed_diagnostic() {
    let _registration = tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default());
    let (_root, service) = service().await;
    let (mut publisher, receiver) = publisher_fixture();
    publisher.observe(&completed(17381, true));
    publisher.finish(true);
    let reporter = reporter(service.clone(), receiver);
    let (capture, dispatch) = capture();
    reporter
        .run(|| async {
            Err(crate::error::Error::internal(
                "untrusted-redaction-error-17381",
            ))
        })
        .with_subscriber(dispatch)
        .await;
    assert!(crate::performance::flush(Duration::from_secs(5)));
    let output = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
    assert!(output.contains("redaction_unavailable"));
    assert!(!output.contains("17381"));
    assert!(!output.contains("untrusted-redaction-error"));
    assert!(!output.contains("recognized_records"));
    service.shutdown.cancel();
}

#[tokio::test]
async fn unsupported_only_stream_never_performs_a_duplicate_redaction_lookup() {
    let _registration = tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default());
    let (_root, service) = service().await;
    let (mut publisher, receiver) = publisher_fixture();
    assert!(!publisher.observe(&started("other-method")));
    publisher.finish(true);
    let reporter = reporter(service.clone(), receiver);
    let (capture, dispatch) = capture();
    let refreshed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let observed = refreshed.clone();
    reporter
        .run(move || {
            observed.store(true, std::sync::atomic::Ordering::Relaxed);
            async { Ok(Vec::new()) }
        })
        .with_subscriber(dispatch)
        .await;
    assert!(!refreshed.load(std::sync::atomic::Ordering::Relaxed));
    assert!(capture.0.lock().unwrap().is_empty());
    service.shutdown.cancel();
}
