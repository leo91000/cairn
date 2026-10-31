use super::*;
use crate::store::{Pool, Store};
use std::{
    io::{self, Write},
    sync::{Arc, Mutex, atomic::AtomicU64},
};
use tokio::sync::{mpsc, watch};
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::prelude::*;

#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl Write for Capture {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for Capture {
    type Writer = Self;

    fn make_writer(&'a self) -> Self {
        self.clone()
    }
}

// Tracing interest and the bounded diagnostic worker are process-global. Give
// each observation fixture its own process, including under parallel libtest.
fn isolated_observation(name: &'static str) -> bool {
    const KEY: &str = "CAIRN_SQLITE_OBSERVATION_TEST";
    let test = format!("store::performance::tests::{name}");
    if std::env::var(KEY).ok().as_deref() == Some(test.as_str()) {
        return false;
    }
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", &test, "--nocapture", "--test-threads=1"])
        .env(KEY, &test)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success() && stdout.contains("1 passed; 0 failed; 0 ignored"),
        "isolated observation {test} did not pass exactly once:\n{stdout}\n{stderr}"
    );
    true
}

fn flush_diagnostics() {
    assert!(
        crate::performance::flush(Duration::from_secs(5)),
        "Queued diagnostics must finish within the test deadline"
    );
}

async fn read_probe(store: &Store, private: String) -> usize {
    store.read(move |_| Ok(private.len())).await.unwrap()
}

async fn write_probe(store: &Store, private: String) -> usize {
    store.write(move |_| Ok(private.len())).await.unwrap()
}

async fn transaction_probe(store: &Store, private: String) -> usize {
    store.transaction(move |_| Ok(private.len())).await.unwrap()
}

async fn sql_cost_probe(store: &Store, private: String) -> i64 {
    store
        .write(move |db| {
            let mut statement = db.0.prepare_cached("SELECT length(?1)")?;
            let mut total = 0;
            for _ in 0..1024 {
                total += statement.query_row([&private], |row| row.get::<_, i64>(0))?;
            }
            Ok(total)
        })
        .await
        .unwrap()
}

async fn failed_cost_probe(store: &Store, private: String) -> crate::error::Result<usize> {
    store
        .transaction(move |_| Err(crate::error::Error::bad(private)))
        .await
}

#[test]
fn failing_close_writer_cannot_abort_sqlite_jobs() {
    crate::test_process::run_ignored("store::performance::tests::failing_close_writer_child");
}

struct BlockedOutput {
    open: Mutex<bool>,
    wake: std::sync::Condvar,
    entered: std::sync::mpsc::SyncSender<()>,
}

struct BlockedWriter(Arc<BlockedOutput>);

impl std::io::Write for BlockedWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let mut open = self.0.open.lock().unwrap();
        if !*open {
            let _ = self.0.entered.try_send(());
        }
        while !*open {
            open = self.0.wake.wait(open).unwrap();
        }
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn blocked_diagnostic_output_cannot_delay_later_sqlite_jobs() {
    if isolated_observation("blocked_diagnostic_output_cannot_delay_later_sqlite_jobs") {
        return;
    }
    let (entered, observed) = std::sync::mpsc::sync_channel(1);
    let output = Arc::new(BlockedOutput {
        open: Mutex::new(false),
        wake: std::sync::Condvar::new(),
        entered,
    });
    let writer = output.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(move || BlockedWriter(writer.clone()))
        .finish();
    let dispatcher = tracing::Dispatch::new(subscriber);
    let (sender, mut receiver) = mpsc::channel::<crate::store::Job>(4);
    let actor = std::thread::spawn(move || {
        tracing::dispatcher::with_default(&dispatcher, || {
            let mut connection = Connection::open_in_memory().unwrap();
            let mut metrics = Metrics::new("write", 0, &connection);
            for _ in 0..2 {
                let completed = receiver.blocking_recv().unwrap()(&mut connection);
                metrics.since = Instant::now() - Duration::from_secs(31);
                metrics.record(&completed, &connection);
            }
            assert_eq!(metrics.window, 2);
        });
    });
    let store = Store(Arc::new(Pool {
        writer: sender.clone(),
        readers: sender,
        changes: watch::channel(0).0,
        discarded_changes: Arc::new(AtomicU64::new(0)),
    }));
    store.write(|db| {
        db.0.execute_batch("CREATE TABLE output_progress(value INTEGER); INSERT INTO output_progress VALUES (17)")?;
        Ok(())
    }).await.unwrap();
    observed.recv_timeout(Duration::from_secs(5)).unwrap();
    let read = tokio::time::timeout(
        Duration::from_secs(1),
        store.read(|db| {
            Ok(db
                .0
                .query_row("SELECT value + 6 FROM output_progress", [], |row| {
                    row.get::<_, i64>(0)
                })?)
        }),
    )
    .await;
    *output.open.lock().unwrap() = true;
    output.wake.notify_all();
    actor.join().unwrap();
    flush_diagnostics();
    assert!(
        read.is_ok(),
        "Diagnostic output backpressure must not delay a later SQLite job"
    );
    assert_eq!(read.unwrap().unwrap(), 23);
}

struct FailingWriter(Arc<AtomicU64>);

impl std::io::Write for FailingWriter {
    fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
        self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        panic!("synthetic SQLite diagnostic writer failure");
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[tokio::test]
#[ignore = "isolated abort-capable diagnostic writer fixture, run by parent test"]
async fn failing_close_writer_child() {
    #[cfg(unix)]
    {
        let limit = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        // SAFETY: this child owns its core-dump limits and supplies a valid limit.
        assert_eq!(unsafe { libc::setrlimit(libc::RLIMIT_CORE, &limit) }, 0);
    }
    let attempts = Arc::new(AtomicU64::new(0));
    let observed_attempts = attempts.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_span_events(tracing_subscriber::fmt::format::FmtSpan::CLOSE)
        .with_writer(move || FailingWriter(observed_attempts.clone()))
        .finish();
    let dispatcher = tracing::Dispatch::new(subscriber);
    let (sender, mut receiver) = mpsc::channel::<crate::store::Job>(4);
    let actor = std::thread::spawn(move || {
        tracing::dispatcher::with_default(&dispatcher, || {
            let mut connection = Connection::open_in_memory().unwrap();
            let mut metrics = Metrics::new("write", 0, &connection);
            for _ in 0..2 {
                let completed = receiver.blocking_recv().unwrap()(&mut connection);
                metrics.since = Instant::now() - Duration::from_secs(31);
                metrics.record(&completed, &connection);
            }
            assert_eq!(metrics.window, 2);
        });
    });
    let store = Store(Arc::new(Pool {
        writer: sender.clone(),
        readers: sender,
        changes: watch::channel(0).0,
        discarded_changes: Arc::new(AtomicU64::new(0)),
    }));
    let written = store.write(|db| {
        db.0.execute_batch("CREATE TABLE protected_writer(value INTEGER); INSERT INTO protected_writer VALUES (17)")?;
        Ok(17)
    }).await.unwrap();
    assert_eq!(written, 17);
    let read = store
        .read(|db| {
            Ok(db
                .0
                .query_row("SELECT value + 6 FROM protected_writer", [], |row| {
                    row.get::<_, i64>(0)
                })?)
        })
        .await
        .unwrap();
    assert_eq!(read, 23);
    actor.join().unwrap();
    flush_diagnostics();
    assert_eq!(attempts.load(std::sync::atomic::Ordering::Relaxed), 2);
}

#[tokio::test]
async fn failing_diagnostic_filter_cannot_interrupt_real_sqlite_jobs() {
    if isolated_observation("failing_diagnostic_filter_cannot_interrupt_real_sqlite_jobs") {
        return;
    }
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let observed_calls = calls.clone();
    let filter = tracing_subscriber::filter::dynamic_filter_fn(move |metadata, _| {
        if metadata.is_event() && metadata.target() == "cairn_performance" {
            observed_calls.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        assert!(
            !metadata.is_event() || metadata.target() != "cairn_performance",
            "synthetic performance filter failure"
        );
        true
    });
    let subscriber = tracing_subscriber::registry()
        .with(tracing_subscriber::layer::Identity::new().with_filter(filter));
    let dispatcher = tracing::Dispatch::new(subscriber);
    let (sender, mut receiver) = mpsc::channel::<crate::store::Job>(4);
    let actor = std::thread::spawn(move || {
        tracing::dispatcher::with_default(&dispatcher, || {
            let mut connection = Connection::open_in_memory().unwrap();
            let mut metrics = Metrics::new("write", 0, &connection);
            for _ in 0..2 {
                let completed = receiver.blocking_recv().unwrap()(&mut connection);
                assert!(completed.cpu.is_none());
                metrics.since = Instant::now() - Duration::from_secs(31);
                metrics.record(&completed, &connection);
            }
            assert_eq!(metrics.window, 2);
        });
    });
    let store = Store(Arc::new(Pool {
        writer: sender.clone(),
        readers: sender,
        changes: watch::channel(0).0,
        discarded_changes: Arc::new(AtomicU64::new(0)),
    }));
    let written = store.write(|db| {
        db.0.execute_batch("CREATE TABLE protected_jobs(value INTEGER); INSERT INTO protected_jobs VALUES (17)")?;
        Ok(17)
    }).await.unwrap();
    assert_eq!(written, 17);
    let read = store
        .read(|db| {
            Ok(db
                .0
                .query_row("SELECT value + 6 FROM protected_jobs", [], |row| {
                    row.get::<_, i64>(0)
                })?)
        })
        .await
        .unwrap();
    assert_eq!(read, 23);
    actor.join().unwrap();
    assert!(calls.load(std::sync::atomic::Ordering::Relaxed) > 0);
}

#[tokio::test]
async fn caller_costs_include_failed_work_after_its_receiver_is_abandoned() {
    if isolated_observation("caller_costs_include_failed_work_after_its_receiver_is_abandoned") {
        return;
    }
    let capture = Capture::default();
    let subscriber = tracing_subscriber::fmt()
        .without_time()
        .with_ansi(false)
        .with_writer(capture.clone())
        .finish();
    let dispatcher = tracing::Dispatch::new(subscriber);
    let (sender, mut receiver) = mpsc::channel::<crate::store::Job>(4);
    let store = Store(Arc::new(Pool {
        writer: sender.clone(),
        readers: sender,
        changes: watch::channel(0).0,
        discarded_changes: Arc::new(AtomicU64::new(0)),
    }));
    let private = "synthetic-private-failed-caller-never-log";
    let task = tokio::spawn(async move { failed_cost_probe(&store, private.into()).await });
    let job = receiver.recv().await.unwrap();
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    std::thread::spawn(move || {
        tracing::dispatcher::with_default(&dispatcher, || {
            let mut connection = Connection::open_in_memory().unwrap();
            let mut metrics = Metrics::new("write", 0, &connection);
            let completed = job(&mut connection);
            assert!(completed.failed);
            assert!(completed.abandoned_reply);
            metrics.since = Instant::now() - Duration::from_secs(31);
            metrics.record(&completed, &connection);
        });
    })
    .join()
    .unwrap();
    flush_diagnostics();
    let output = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
    assert!(!output.contains(private));
    let caller = output
        .lines()
        .find(|line| line.contains("operation=\"database_caller\""))
        .unwrap();
    assert!(caller.contains("failed_cost_probe"));
    assert!(caller.contains("completed=1"));
    assert!(caller.contains("errors=1"));
    assert!(caller.contains("abandoned_replies=1"));
}

#[tokio::test]
async fn real_reader_jobs_report_cpu_sampling_and_caller_cost_rows() {
    if isolated_observation("real_reader_jobs_report_cpu_sampling_and_caller_cost_rows") {
        return;
    }
    let capture = Capture::default();
    let subscriber = tracing_subscriber::fmt()
        .without_time()
        .with_ansi(false)
        .with_writer(capture.clone())
        .finish();
    let dispatcher = tracing::Dispatch::new(subscriber);
    let (writer, _writer_receiver) = mpsc::channel::<crate::store::Job>(4);
    let (readers, mut receiver) = mpsc::channel::<crate::store::Job>(4);
    let store = Store(Arc::new(Pool {
        writer,
        readers,
        changes: watch::channel(0).0,
        discarded_changes: Arc::new(AtomicU64::new(0)),
    }));
    let task = tokio::spawn(async move { read_probe(&store, "private-reader-value".into()).await });
    let job = receiver.recv().await.unwrap();
    std::thread::spawn(move || {
        tracing::dispatcher::with_default(&dispatcher, || {
            let mut connection = Connection::open_in_memory().unwrap();
            let mut metrics = Metrics::new("read", 0, &connection);
            let completed = job(&mut connection);
            assert_eq!(completed.cpu.is_some(), cfg!(target_os = "linux"));
            metrics.since = Instant::now() - Duration::from_secs(31);
            metrics.record(&completed, &connection);
            assert_eq!(metrics.callers.len(), 0);
        });
    })
    .join()
    .unwrap();
    assert_eq!(task.await.unwrap(), "private-reader-value".len());
    flush_diagnostics();
    let output = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
    assert_eq!(output.lines().count(), 2);
    assert!(output.contains("operation=\"database_actor\""));
    let available = cfg!(target_os = "linux");
    assert!(output.contains(&format!("cpu_available={available}")));
    assert!(output.contains(&format!("cpu_sampled_jobs={}", u8::from(available))));
    let caller = output
        .lines()
        .find(|line| line.contains("database_caller"))
        .unwrap();
    assert!(caller.contains("read_probe"));
    assert!(caller.contains("lane=\"read\""));
    assert!(!output.contains("lane=\"write\""));
    assert!(!output.contains("private-reader-value"));
}

#[tokio::test]
async fn json_cost_rows_isolate_both_current_span_and_span_list() {
    if isolated_observation("json_cost_rows_isolate_both_current_span_and_span_list") {
        return;
    }
    let capture = Capture::default();
    let subscriber = tracing_subscriber::fmt()
        .json()
        .with_current_span(true)
        .with_span_list(true)
        .without_time()
        .with_writer(capture.clone())
        .finish();
    let dispatcher = tracing::Dispatch::new(subscriber);
    let (sender, mut receiver) = mpsc::channel::<crate::store::Job>(4);
    let actor = std::thread::spawn(move || {
        tracing::dispatcher::with_default(&dispatcher, || {
            let mut connection = Connection::open_in_memory().unwrap();
            let mut metrics = Metrics::new("write", 0, &connection);
            let private_span = tracing::info_span!(
                "synthetic_private_json_parent",
                private = "synthetic-private-json-parent-never-log"
            );
            let _entered = private_span.enter();
            let completed = receiver.blocking_recv().unwrap()(&mut connection);
            metrics.since = Instant::now() - Duration::from_secs(31);
            metrics.record(&completed, &connection);
        });
    });
    let store = Store(Arc::new(Pool {
        writer: sender.clone(),
        readers: sender,
        changes: watch::channel(0).0,
        discarded_changes: Arc::new(AtomicU64::new(0)),
    }));
    let private = "synthetic-private-json-capture-never-log";
    assert_eq!(write_probe(&store, private.into()).await, private.len());
    actor.join().unwrap();
    flush_diagnostics();
    let output = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
    assert!(!output.contains(private));
    assert!(!output.contains("synthetic_private_json_parent"));
    assert!(!output.contains("synthetic-private-json-parent-never-log"));
    let rows = output
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(rows.len(), 2);
    assert!(
        rows.iter()
            .all(|row| row.get("span").is_none() && row.get("spans").is_none())
    );
}

#[test]
fn per_layer_filters_cannot_restore_private_json_context() {
    if isolated_observation("per_layer_filters_cannot_restore_private_json_context") {
        return;
    }
    let capture = Capture::default();
    let output = tracing_subscriber::fmt::layer()
        .json()
        .with_current_span(true)
        .with_span_list(true)
        .without_time()
        .with_writer(capture.clone())
        .with_filter(tracing_subscriber::filter::filter_fn(|metadata| {
            metadata.name() != "database_metrics"
        }));
    let other = tracing_subscriber::layer::Identity::new().with_filter(
        tracing_subscriber::filter::filter_fn(|metadata| metadata.name() == "database_metrics"),
    );
    let subscriber = tracing_subscriber::registry().with(output).with(other);
    tracing::subscriber::with_default(subscriber, || {
        let private_span = tracing::info_span!(
            "synthetic_private_layered_span",
            private = "synthetic-private-layered-never-log"
        );
        let _entered = private_span.enter();
        let root =
            tracing::info_span!(target: "cairn_performance", parent: None, "database_metrics");
        assert!(!root.is_disabled());
        let connection = Connection::open_in_memory().unwrap();
        let mut metrics = Metrics::new("write", 0, &connection);
        metrics.since = Instant::now() - Duration::from_secs(31);
        metrics.record(
            &Completed {
                caller: "fixed_layered_filter_caller",
                queue: Duration::ZERO,
                execution: Duration::ZERO,
                cpu: None,
                failed: false,
                abandoned_reply: false,
            },
            &connection,
        );
    });
    flush_diagnostics();
    let output = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
    assert!(output.contains("database_actor"));
    assert!(output.contains("database_caller"));
    assert!(
        !output.contains("synthetic-private-layered-never-log"),
        "Per-layer filters must not restore a private current span"
    );
    assert!(!output.contains("synthetic_private_layered_span"));
}

#[test]
fn event_only_filter_reports_without_inheriting_private_context() {
    if isolated_observation("event_only_filter_reports_without_inheriting_private_context") {
        return;
    }
    let capture = Capture::default();
    let layer = tracing_subscriber::fmt::layer()
        .json()
        .with_current_span(true)
        .with_span_list(true)
        .without_time()
        .with_writer(capture.clone())
        .with_filter(tracing_subscriber::filter::filter_fn(|metadata| {
            metadata.is_event() || metadata.name() == "synthetic_private_filter_span"
        }));
    let subscriber = tracing_subscriber::registry().with(layer);
    tracing::subscriber::with_default(subscriber, || {
        let private_span = tracing::info_span!(
            "synthetic_private_filter_span",
            private = "synthetic-private-filter-never-log"
        );
        let _entered = private_span.enter();
        let disabled = tracing::info_span!(
            target: "cairn_performance",
            parent: None,
            "database_metrics"
        );
        assert!(disabled.is_disabled());
        assert!(tracing::event_enabled!(target: "cairn_performance", tracing::Level::INFO));
        let connection = Connection::open_in_memory().unwrap();
        let mut metrics = Metrics::new("write", 0, &connection);
        metrics.since = Instant::now() - Duration::from_secs(31);
        metrics.record(
            &Completed {
                caller: "fixed_filter_test_caller",
                queue: Duration::ZERO,
                execution: Duration::ZERO,
                cpu: None,
                failed: false,
                abandoned_reply: false,
            },
            &connection,
        );
        assert_eq!(metrics.completed, 0);
        assert_eq!(metrics.window, 1);
    });
    flush_diagnostics();
    let output = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
    assert!(!output.contains("synthetic-private-filter-never-log"));
    assert!(!output.contains("synthetic_private_filter_span"));
    let rows = output
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(rows.len(), 2);
    assert!(
        rows.iter()
            .all(|row| row.get("span").is_none() && row.get("spans").is_none())
    );
}

#[tokio::test]
async fn writer_reports_cumulative_costs_for_real_originating_jobs_without_values() {
    if isolated_observation(
        "writer_reports_cumulative_costs_for_real_originating_jobs_without_values",
    ) {
        return;
    }
    let capture = Capture::default();
    let subscriber = tracing_subscriber::fmt()
        .without_time()
        .with_ansi(false)
        .with_writer(capture.clone())
        .finish();
    let dispatcher = tracing::Dispatch::new(subscriber);
    let (sender, mut receiver) = mpsc::channel::<crate::store::Job>(4);
    let actor = std::thread::spawn(move || {
        tracing::dispatcher::with_default(&dispatcher, || {
            let mut connection = Connection::open_in_memory().unwrap();
            let mut metrics = Metrics::new("write", 0, &connection);
            let private_span = tracing::info_span!(
                "synthetic_private_parent",
                private = "synthetic-private-parent-never-log"
            );
            let _entered = private_span.enter();
            for index in 0..4 {
                let job = receiver.blocking_recv().unwrap();
                let completed = job(&mut connection);
                if index == 3 {
                    metrics.since = Instant::now() - Duration::from_secs(31);
                }
                metrics.record(&completed, &connection);
            }
            assert_eq!(metrics.callers.len(), 0);
            assert_eq!(metrics.cpu_us, 0);
            assert_eq!(metrics.cpu_sampled_jobs, 0);
            assert_eq!(metrics.window, 1);
        });
    });
    let store = Store(Arc::new(Pool {
        writer: sender.clone(),
        readers: sender,
        changes: watch::channel(0).0,
        discarded_changes: Arc::new(AtomicU64::new(0)),
    }));
    let private = "synthetic-private-caller-cost-never-log";
    for _ in 0..3 {
        assert_eq!(
            sql_cost_probe(&store, private.into()).await,
            i64::try_from(private.len()).unwrap() * 1024
        );
    }
    assert_eq!(write_probe(&store, private.into()).await, private.len());
    actor.join().unwrap();

    flush_diagnostics();
    let output = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
    assert!(!output.contains(private));
    assert!(!output.contains("synthetic-private-parent-never-log"));
    assert!(!output.contains("SELECT"));
    let callers = output
        .lines()
        .filter(|line| line.contains("operation=\"database_caller\""))
        .collect::<Vec<_>>();
    assert_eq!(
        callers.len(),
        2,
        "Writer diagnostics must report cumulative costs per originating function"
    );
    let repeated = callers
        .iter()
        .find(|line| line.contains("sql_cost_probe"))
        .unwrap();
    assert!(repeated.contains("completed=3"));
    let repeated_cpu_jobs = if cfg!(target_os = "linux") { 3 } else { 0 };
    assert!(repeated.contains(&format!("cpu_sampled_jobs={repeated_cpu_jobs}")));
    assert!(repeated.contains("execution_us="));
    assert!(repeated.contains("cpu_us="));
    let once = callers
        .iter()
        .find(|line| line.contains("tests::write_probe"))
        .unwrap();
    assert!(once.contains("completed=1"));
    let once_cpu_jobs = if cfg!(target_os = "linux") { 1 } else { 0 };
    assert!(once.contains(&format!("cpu_sampled_jobs={once_cpu_jobs}")));
}

#[test]
fn caller_cost_rows_bound_cardinality_and_account_for_omitted_jobs() {
    if isolated_observation("caller_cost_rows_bound_cardinality_and_account_for_omitted_jobs") {
        return;
    }
    let capture = Capture::default();
    let subscriber = tracing_subscriber::fmt()
        .without_time()
        .with_ansi(false)
        .with_writer(capture.clone())
        .finish();
    let mut callers = Callers::default();
    for index in 0..66 {
        let name: &'static str =
            Box::leak(format!("bounded_test_caller_{index:02}").into_boxed_str());
        callers.record(&Completed {
            caller: name,
            queue: Duration::from_micros(2),
            execution: Duration::from_micros(20),
            cpu: Some(Duration::from_micros(10)),
            failed: false,
            abandoned_reply: false,
        });
    }
    assert_eq!(callers.len(), 64);
    assert_eq!(callers.recorded_jobs, 66);
    assert_eq!(callers.untracked_jobs, 2);
    tracing::subscriber::with_default(subscriber, || {
        callers.emit(
            "write",
            0,
            7,
            30_000,
            Totals {
                completed: 66,
                queue_us: 132,
                execution_us: 1320,
                cpu_us: 660,
                cpu_sampled_jobs: 66,
                ..Totals::default()
            },
        );
    });
    flush_diagnostics();
    let output = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
    let rows = output.lines().collect::<Vec<_>>();
    assert!(rows.len() <= 17);
    assert!(!output.contains("bounded_test_caller_64"));
    assert!(!output.contains("bounded_test_caller_65"));
    let total = |field: &str| {
        rows.iter()
            .map(|row| {
                row.split_whitespace()
                    .find_map(|token| token.strip_prefix(&format!("{field}=")))
                    .unwrap()
                    .parse::<u64>()
                    .unwrap()
            })
            .sum::<u64>()
    };
    assert_eq!(total("completed"), 66);
    assert_eq!(total("queue_us"), 132);
    assert_eq!(total("execution_us"), 1320);
    assert_eq!(total("cpu_us"), 660);
    assert_eq!(total("cpu_sampled_jobs"), 66);
    assert!(rows.iter().any(|row| row.contains("other=true")));
    assert!(rows.iter().all(|row| row.contains("window=7")));
}

#[test]
fn caller_cost_rows_include_cpu_and_wall_leaders_without_duplicates() {
    if isolated_observation("caller_cost_rows_include_cpu_and_wall_leaders_without_duplicates") {
        return;
    }
    let capture = Capture::default();
    let subscriber = tracing_subscriber::fmt()
        .without_time()
        .with_ansi(false)
        .with_writer(capture.clone())
        .finish();
    let mut callers = Callers::default();
    for index in 0..17 {
        let name: &'static str =
            Box::leak(format!("ranked_test_caller_{index:02}").into_boxed_str());
        callers.record(&Completed {
            caller: name,
            queue: Duration::ZERO,
            execution: Duration::from_micros(if index < 8 { 100 } else { 1 }),
            cpu: Some(Duration::from_micros(if (8..16).contains(&index) {
                100
            } else {
                1
            })),
            failed: false,
            abandoned_reply: false,
        });
    }
    tracing::subscriber::with_default(subscriber, || {
        callers.emit(
            "write",
            0,
            0,
            30_000,
            Totals {
                completed: 17,
                execution_us: 809,
                cpu_us: 809,
                cpu_sampled_jobs: 17,
                ..Totals::default()
            },
        );
    });
    flush_diagnostics();
    let output = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
    assert_eq!(output.lines().count(), 17);
    for index in 0..16 {
        assert_eq!(
            output
                .matches(&format!("ranked_test_caller_{index:02}\""))
                .count(),
            1
        );
    }
    assert!(!output.contains("ranked_test_caller_16"));
    let other = output
        .lines()
        .find(|row| row.contains("other=true"))
        .unwrap();
    assert!(other.contains("completed=1"));
    assert!(other.contains("execution_us=1"));
    assert!(other.contains("cpu_us=1"));
}

#[tokio::test]
async fn reports_originating_read_write_and_transaction_without_captured_values() {
    if isolated_observation(
        "reports_originating_read_write_and_transaction_without_captured_values",
    ) {
        return;
    }
    let capture = Capture::default();
    let subscriber = tracing_subscriber::fmt()
        .without_time()
        .with_ansi(false)
        .with_writer(capture.clone())
        .finish();
    let dispatcher = tracing::Dispatch::new(subscriber);
    let (sender, mut receiver) = mpsc::channel::<crate::store::Job>(4);
    let actor = std::thread::spawn(move || {
        tracing::dispatcher::with_default(&dispatcher, || {
            let mut db = Connection::open_in_memory().unwrap();
            let mut metrics = Metrics::new("write", 0, &db);
            for _ in 0..3 {
                let job = receiver.blocking_recv().unwrap();
                let completed = job(&mut db);
                metrics.since = Instant::now() - Duration::from_secs(31);
                metrics.record(&completed, &db);
            }
        });
    });
    let store = Store(Arc::new(Pool {
        writer: sender.clone(),
        readers: sender,
        changes: watch::channel(0).0,
        discarded_changes: Arc::new(AtomicU64::new(0)),
    }));
    let private = "synthetic-private-value-never-log";
    assert_eq!(read_probe(&store, private.into()).await, private.len());
    assert_eq!(write_probe(&store, private.into()).await, private.len());
    assert_eq!(
        transaction_probe(&store, private.into()).await,
        private.len()
    );
    actor.join().unwrap();

    flush_diagnostics();
    let output = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
    assert!(!output.contains(private));
    let lines = output
        .lines()
        .filter(|line| line.contains("operation=\"database_actor\""))
        .collect::<Vec<_>>();
    assert_eq!(lines.len(), 3);
    assert_eq!(
        output
            .lines()
            .filter(|line| line.contains("operation=\"database_caller\""))
            .count(),
        3
    );
    for (line, origin) in lines
        .iter()
        .zip(["read_probe", "write_probe", "transaction_probe"])
    {
        assert!(line.contains("max_execution_caller="));
        assert!(line.contains("max_queue_caller="));
        for field in ["max_execution_caller", "max_queue_caller"] {
            let value = line
                .split_once(&format!("{field}=\""))
                .unwrap()
                .1
                .split('"')
                .next()
                .unwrap();
            assert!(value.contains(origin));
        }
    }
}

#[test]
fn keeps_queue_and_execution_origins_separate_and_resets_between_windows() {
    if isolated_observation("keeps_queue_and_execution_origins_separate_and_resets_between_windows")
    {
        return;
    }
    let connection = Connection::open_in_memory().unwrap();
    let mut metrics = Metrics::new("read", 0, &connection);
    let completed = |caller, queue, execution| Completed {
        caller,
        queue: Duration::from_micros(queue),
        execution: Duration::from_micros(execution),
        cpu: None,
        failed: false,
        abandoned_reply: false,
    };
    metrics.record(&completed("waiting_caller", 90, 10), &connection);
    metrics.record(&completed("blocking_caller", 5, 80), &connection);
    assert_eq!(metrics.max_queue_caller, "waiting_caller");
    assert_eq!(metrics.max_execution_caller, "blocking_caller");
    assert_eq!(metrics.max_queue_us, 90);
    assert_eq!(metrics.max_execution_us, 80);

    metrics.since = Instant::now() - Duration::from_secs(31);
    metrics.record(&completed("fast_caller", 1, 1), &connection);
    assert_eq!(metrics.completed, 0);
    assert_eq!(metrics.max_queue_caller, "none");
    assert_eq!(metrics.max_execution_caller, "none");
    metrics.record(&completed("next_window_caller", 0, 0), &connection);
    assert_eq!(metrics.max_queue_caller, "next_window_caller");
    assert_eq!(metrics.max_execution_caller, "next_window_caller");
}

#[test]
fn bounds_compiler_generated_names() {
    type Nested = Vec<Vec<Vec<Vec<Vec<Vec<Vec<Vec<Vec<Vec<Vec<Vec<String>>>>>>>>>>>>;
    let value: (Nested, Nested, Nested) = Default::default();
    let complete = std::any::type_name_of_val(&value);
    assert!(complete.len() > 256);
    let bounded = caller(&value);
    assert!(bounded.len() <= 256);
    assert!(complete.starts_with(bounded));
}

#[test]
fn reports_wal_frames_without_checkpointing_or_changing_durability() {
    if isolated_observation("reports_wal_frames_without_checkpointing_or_changing_durability") {
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let database = root.path().join("private.db");
    let connection = Connection::open(&database).unwrap();
    connection
        .execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;
             PRAGMA wal_autocheckpoint=0;
             CREATE TABLE private_values(value TEXT);
             INSERT INTO private_values VALUES('synthetic-private-do-not-log');
             PRAGMA wal_checkpoint(TRUNCATE);
             UPDATE private_values SET value='synthetic-private-updated-do-not-log';",
        )
        .unwrap();
    let frames = || {
        connection
            .query_row("PRAGMA wal_checkpoint(NOOP)", [], |row| {
                Ok((row.get::<_, i64>(1)?, row.get::<_, i64>(2)?))
            })
            .unwrap()
    };
    let before = frames();
    assert!(
        before.0 > before.1,
        "The fixture must have pending WAL pages"
    );
    let database_before = std::fs::read(&database).unwrap();
    let wal_before = std::fs::read(database.with_extension("db-wal")).unwrap();
    let changes_before = connection.total_changes();
    let capture = Capture::default();
    let subscriber = tracing_subscriber::fmt()
        .without_time()
        .with_ansi(false)
        .with_writer(capture.clone())
        .finish();
    let mut metrics = Metrics::new("write", 0, &connection);
    metrics.since = Instant::now() - Duration::from_secs(31);
    tracing::subscriber::with_default(subscriber, || {
        metrics.record(
            &Completed {
                caller: "fixed_test_caller",
                queue: Duration::ZERO,
                execution: Duration::ZERO,
                cpu: None,
                failed: false,
                abandoned_reply: false,
            },
            &connection,
        );
    });

    flush_diagnostics();
    let output = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
    assert!(output.contains("wal_available=true"));
    assert!(output.contains(&format!("wal_frames={}", before.0)));
    assert!(output.contains(&format!("wal_checkpointed_frames={}", before.1)));
    assert!(output.contains(&format!("wal_pending_frames={}", before.0 - before.1)));
    assert!(output.contains("wal_autocheckpoint_frames=0"));
    assert!(output.contains("sqlite_synchronous=2"));
    for private in ["synthetic-private", "private_values", "UPDATE", "INSERT"] {
        assert!(!output.contains(private));
    }
    assert_eq!(
        output
            .lines()
            .filter(|line| line.contains("operation=\"database_actor\""))
            .count(),
        1
    );
    assert_eq!(
        output
            .lines()
            .filter(|line| line.contains("operation=\"database_caller\""))
            .count(),
        1
    );
    assert_eq!(frames(), before);
    assert_eq!(connection.total_changes(), changes_before);
    assert_eq!(std::fs::read(&database).unwrap(), database_before);
    assert_eq!(
        std::fs::read(database.with_extension("db-wal")).unwrap(),
        wal_before
    );
    for (pragma, expected) in [("wal_autocheckpoint", 0), ("synchronous", 2)] {
        assert_eq!(
            connection
                .pragma_query_value(None, pragma, |row| row.get::<_, i64>(0))
                .unwrap(),
            expected
        );
    }
}
