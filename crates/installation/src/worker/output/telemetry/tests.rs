use super::*;
use crate::{config::Config, service::Service};
use serde_json::{Value, json};
use std::sync::Arc;
use tracing::instrument::WithSubscriber;

const RUN: &str = "00000000-0000-4000-8000-000000000001";
const ATTEMPT: &str = "00000000-0000-4000-8000-000000000002";

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

#[test]
fn schema_is_closed_and_discards_child_numeric_values_and_text() {
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
            declared_success: false,
            queue_field_present: true,
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
        completed(1, true).replace("queue_ms=0", "queue_ms=unknown"),
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
        Some(Sample::Completed {
            queue_field_present: false,
            ..
        })
    ));
    let mut publisher = Publisher::new(RUN, ATTEMPT);
    publisher.observe(&valid);
    publisher.observe(&completed(17381, true));
    let serialized = serde_json::to_string(&publisher.snapshot).unwrap();
    for discarded in [
        "untrusted-span",
        "request_id",
        "2026-11",
        "elapsed_ms_min",
        "queue_ms",
    ] {
        assert!(!serialized.contains(discarded));
    }
    // Local elapsed time may independently coincide with a numeric token; the
    // isolated rotation test controls that clock and checks the complete record.
    assert_eq!(publisher.snapshot.items.completed_records, 1);
}

#[test]
fn counters_are_cumulative_saturating_and_coverage_is_explicit() {
    let mut publisher = Publisher::new(RUN, ATTEMPT);
    for _ in 0..6090 {
        assert!(publisher.observe(&started("thread/items/list")));
        assert!(publisher.observe(&completed(1, true)));
    }
    assert!(publisher.observe(&completed(500, false)));
    assert!(!publisher.observe("unrecognized"));
    let snapshot = publisher.snapshot;
    assert_eq!(snapshot.recognized_records, 12181);
    assert_eq!(snapshot.items.started_records, 6090);
    assert_eq!(snapshot.items.completed_records, 6091);
    assert_eq!(snapshot.items.declared_failure_records, 1);
    assert_eq!(snapshot.items.missing_queue_field_records, 0);
    assert_eq!(snapshot.unsupported_records, 1);
    assert!(!snapshot.reader_closed);
    assert!(!snapshot.eof);
    publisher.snapshot.items.started_records = u64::MAX;
    publisher.observe(&started("thread/items/list"));
    assert_eq!(publisher.snapshot.items.started_records, u64::MAX);
    assert!(publisher.snapshot.overflow);
    publisher.finish(false);
    assert!(publisher.snapshot.reader_closed);
    assert!(!publisher.snapshot.eof);
    publisher.finish(true);
    assert!(
        !publisher.snapshot.eof,
        "a later finish cannot upgrade error coverage"
    );
    assert!(!publisher.observe(&started("thread/items/list")));
    let mut complete = Publisher::new(RUN, ATTEMPT);
    complete.finish(true);
    assert!(complete.snapshot.eof);
    assert!(serde_json::to_vec(&publisher.snapshot).unwrap().len() < 4096);
}

fn capture() -> (Capture, tracing::Dispatch) {
    Capture::with_dispatch(None)
}

fn flushed(capture: &Capture) -> Vec<Value> {
    assert!(crate::performance::flush(Duration::from_secs(5)));
    capture.records()
}

// The sink and tracing interest are process-global. These children run alone
// under both ordinary parallel libtest and nextest, using the existing helper.
#[test]
fn isolated_reporting_observations() {
    for child in [
        "worker::output::telemetry::tests::numeric_rotation_without_canonical_child",
        "worker::output::telemetry::tests::reader_coverage_child",
        "worker::output::telemetry::tests::cadence_and_cancellation_child",
        "worker::output::telemetry::tests::unsupported_only_child",
    ] {
        crate::test_process::run_ignored(child);
    }
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

#[tokio::test]
#[ignore = "process-global diagnostic capture, invoked by isolated_reporting_observations"]
async fn numeric_rotation_without_canonical_child() {
    let (_root, service) = service().await;
    let key = format!("codex-account:{}", crate::config::id());
    service
        .vault
        .set(&key, &json!({ "tokens": { "access_token": "8471" } }))
        .await
        .unwrap();
    let probe = Arc::new(Probe::default());
    let (capture, dispatch) = Capture::with_dispatch(Some(probe.clone()));
    async {
        let mut publisher = Publisher::new(RUN, ATTEMPT);
        publisher.probe_fixture(probe.clone());
        service
            .vault
            .set(&key, &json!({ "tokens": { "access_token": "17381" } }))
            .await
            .unwrap();
        assert_eq!(
            service.vault.get(&key).await.unwrap().unwrap()["tokens"]["access_token"],
            "17381"
        );
        let input = completed(17381, false)
            .replace("request_id=1", "request_id=17381")
            .replace("queue_ms=0", "queue_ms=17381");
        // Model a previously sent window without depending on SQLite or CPU speed.
        tokio::time::pause();
        publisher.next_report = Instant::now() + PERIOD;
        assert!(publisher.observe(&input));
        tokio::time::resume();
        // No canonical refresh or reporter refresh ever observes B.
        service
            .vault
            .set(&key, &json!({ "tokens": { "access_token": "19873" } }))
            .await
            .unwrap();
        assert_eq!(
            service.vault.get(&key).await.unwrap().unwrap()["tokens"]["access_token"],
            "19873"
        );
        // Deterministic local monotonic measurement, independent of SQLite waits.
        tokio::time::pause();
        publisher.started = Instant::now() - Duration::from_millis(10);
        publisher.finish(true);
        tokio::time::resume();
    }
    .with_subscriber(dispatch)
    .await;
    let reports = flushed(&capture);
    assert_eq!(
        reports.len(),
        1,
        "fixed_reporting_stages={}",
        probe.snapshot()
    );
    let snapshot = &reports[0]["snapshot"];
    assert_eq!(
        snapshot["items"],
        json!({
            "started_records": 0,
            "completed_records": 1,
            "declared_failure_records": 1,
            "missing_queue_field_records": 0,
        })
    );
    assert_eq!(snapshot["local_reader_elapsed_ms"], 10);
    assert_eq!(snapshot["reader_closed"], true);
    assert_eq!(snapshot["eof"], true);
    let encoded = reports[0].to_string();
    assert!(!encoded.contains("17381"));
    assert!(!encoded.contains("19873"));
    for omitted in [
        "request_id",
        "elapsed_ms_min",
        "elapsed_ms_max",
        "elapsed_ms_total",
        "queue_ms_total",
        "queue_ms_max",
    ] {
        assert!(!encoded.contains(omitted));
    }
    service.shutdown.cancel();
}

#[tokio::test]
#[ignore = "process-global diagnostic capture, invoked by isolated_reporting_observations"]
async fn reader_coverage_child() {
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
    let (capture, dispatch) = capture();
    for error in [false, true] {
        async {
            let input = started("thread/items/list");
            let publisher = Publisher::new(RUN, ATTEMPT);
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
        }
        .with_subscriber(dispatch.clone())
        .await;
        let reports = flushed(&capture);
        let final_record = reports.last().unwrap();
        assert_eq!(final_record["snapshot"]["recognized_records"], 1);
        assert_eq!(final_record["snapshot"]["reader_closed"], true);
        assert_eq!(final_record["snapshot"]["eof"], !error);
    }
    async {
        let input = started("thread/items/list");
        let publisher = Publisher::new(RUN, ATTEMPT);
        let (sender, mut events) = tokio::sync::mpsc::channel(32);
        super::super::read_diagnostics(input.trim_end().as_bytes(), sender, Some(publisher)).await;
        let output = events.recv().await.unwrap();
        assert_eq!(output.kind, super::super::OutputKind::Performance);
    }
    .with_subscriber(dispatch)
    .await;
    assert_eq!(flushed(&capture).len(), 4);
}

#[tokio::test]
#[ignore = "process-global diagnostic capture, invoked by isolated_reporting_observations"]
async fn cadence_and_cancellation_child() {
    let (capture, dispatch) = capture();
    async {
        tokio::time::pause();
        let mut publisher = Publisher::new(RUN, ATTEMPT);
        assert!(publisher.observe(&started("thread/items/list")));
        for _ in 0..100 {
            publisher.observe(&completed(1, true));
        }
        assert_eq!(flushed(&capture).len(), 1);
        tokio::time::advance(PERIOD).await;
        publisher.observe(&started("thread/items/list"));
        assert_eq!(flushed(&capture).len(), 2);
        publisher.finish(true);
        publisher.finish(true);
        assert!(!publisher.observe(&started("thread/items/list")));
        let reports = flushed(&capture);
        assert_eq!(reports.len(), 3);
        assert_eq!(reports[2]["snapshot"]["recognized_records"], 102);
        assert_eq!(reports[2]["snapshot"]["items"]["completed_records"], 100);
        assert_eq!(reports[2]["snapshot"]["eof"], true);
        {
            let mut cancelled = Publisher::new(RUN, ATTEMPT);
            cancelled.observe(&started("thread/items/list"));
        }
        let reports = flushed(&capture);
        assert_eq!(reports.len(), 4);
        assert_eq!(reports[3]["snapshot"]["reader_closed"], false);
        assert_eq!(reports[3]["snapshot"]["eof"], false);
        tokio::time::resume();
    }
    .with_subscriber(dispatch)
    .await;
}

#[tokio::test]
#[ignore = "process-global diagnostic capture, invoked by isolated_reporting_observations"]
async fn unsupported_only_child() {
    let (capture, dispatch) = capture();
    async {
        let mut publisher = Publisher::new(RUN, ATTEMPT);
        assert!(!publisher.observe(&started("other-method")));
        publisher.finish(true);
        assert_eq!(publisher.snapshot.unsupported_records, 1);
    }
    .with_subscriber(dispatch)
    .await;
    assert!(flushed(&capture).is_empty());
}

#[tokio::test]
async fn cumulative_watch_retains_final_coverage_without_an_output_queue_or_reporter_task() {
    let (mut publisher, mut reports) = Publisher::channel(RUN, ATTEMPT);
    for _ in 0..6090 {
        assert!(publisher.observe(&started("thread/items/list")));
        assert!(publisher.observe(&completed(1, true)));
    }
    publisher.finish(true);
    drop(publisher);
    // Sender is already gone: the final unseen value must still be available.
    assert!(reports.changed().await);
    let latest = reports.receiver.borrow_and_update().unwrap();
    assert_eq!(latest.recognized_records, 12180);
    assert_eq!(latest.items.started_records, 6090);
    assert_eq!(latest.items.completed_records, 6090);
    assert!(latest.reader_closed && latest.eof);
    assert!(!reports.changed().await);
    assert!(std::mem::size_of::<Snapshot>() < 1024);
}

#[tokio::test]
async fn dropped_reader_observer_does_not_upgrade_partial_coverage_to_eof() {
    let (mut publisher, mut reports) = Publisher::channel(RUN, ATTEMPT);
    publisher.observe(&started("thread/items/list"));
    drop(publisher);
    assert!(reports.changed().await);
    let latest = reports.receiver.borrow_and_update().unwrap();
    assert_eq!(latest.recognized_records, 1);
    assert!(!latest.reader_closed && !latest.eof);
    assert!(!reports.changed().await);
}

#[tokio::test]
#[ignore = "process-global V2 diagnostic capture, invoked by isolated_v2_reporting"]
async fn numeric_rotation_v2_channel_child() {
    let (_root, service) = service().await;
    let key = format!("codex-account:{}", crate::config::id());
    service
        .vault
        .set(&key, &json!({ "tokens": { "access_token": "8471" } }))
        .await
        .unwrap();
    let probe = Arc::new(Probe::default());
    let (capture, dispatch) = Capture::with_dispatch(Some(probe.clone()));
    async {
        let (mut publisher, mut consumer) = Publisher::channel_fixture(RUN, ATTEMPT, probe.clone());
        service
            .vault
            .set(&key, &json!({ "tokens": { "access_token": "17381" } }))
            .await
            .unwrap();
        assert_eq!(
            service.vault.get(&key).await.unwrap().unwrap()["tokens"]["access_token"],
            "17381"
        );
        let input = completed(17381, false)
            .replace("request_id=1", "request_id=17381")
            .replace("queue_ms=0", "queue_ms=17381");
        // Model a previously sent window without depending on SQLite or CPU speed.
        tokio::time::pause();
        publisher.next_report = Instant::now() + PERIOD;
        assert!(publisher.observe(&input));
        tokio::time::resume();
        // No canonical refresh or reporter refresh ever observes B.
        service
            .vault
            .set(&key, &json!({ "tokens": { "access_token": "19873" } }))
            .await
            .unwrap();
        assert_eq!(
            service.vault.get(&key).await.unwrap().unwrap()["tokens"]["access_token"],
            "19873"
        );
        // Deterministic local monotonic measurement, independent of SQLite waits.
        tokio::time::pause();
        publisher.started = Instant::now() - Duration::from_millis(10);
        publisher.finish(true);
        tokio::time::resume();
        drop(publisher);
        assert!(
            consumer.changed().await,
            "fixed_reporting_stages={}",
            probe.snapshot()
        );
        consumer.publish_latest();
        assert!(
            !consumer.changed().await,
            "fixed_reporting_stages={}",
            probe.snapshot()
        );
    }
    .with_subscriber(dispatch)
    .await;
    let reports = flushed(&capture);
    assert_eq!(
        reports.len(),
        1,
        "fixed_reporting_stages={}",
        probe.snapshot()
    );
    let snapshot = &reports[0]["snapshot"];
    assert_eq!(
        snapshot["items"],
        json!({
            "started_records": 0,
            "completed_records": 1,
            "declared_failure_records": 1,
            "missing_queue_field_records": 0,
        })
    );
    assert_eq!(snapshot["local_reader_elapsed_ms"], 10);
    assert_eq!(snapshot["reader_closed"], true);
    assert_eq!(snapshot["eof"], true);
    let encoded = reports[0].to_string();
    assert!(!encoded.contains("17381"));
    assert!(!encoded.contains("19873"));
    for omitted in [
        "request_id",
        "elapsed_ms_min",
        "elapsed_ms_max",
        "elapsed_ms_total",
        "queue_ms_total",
        "queue_ms_max",
    ] {
        assert!(!encoded.contains(omitted));
    }
    service.shutdown.cancel();
}

#[test]
fn isolated_v2_reporting() {
    crate::test_process::run_ignored(
        "worker::output::telemetry::tests::numeric_rotation_v2_channel_child",
    );
}
