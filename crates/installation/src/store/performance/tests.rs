use super::*;
use crate::store::{Pool, Store};
use std::{
    io::{self, Write},
    sync::{Arc, Mutex, atomic::AtomicU64},
};
use tokio::sync::{mpsc, watch};
use tracing_subscriber::fmt::MakeWriter;

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

async fn read_probe(store: &Store, private: String) -> usize {
    store.read(move |_| Ok(private.len())).await.unwrap()
}

async fn write_probe(store: &Store, private: String) -> usize {
    store.write(move |_| Ok(private.len())).await.unwrap()
}

async fn transaction_probe(store: &Store, private: String) -> usize {
    store.transaction(move |_| Ok(private.len())).await.unwrap()
}

#[tokio::test]
async fn reports_originating_read_write_and_transaction_without_captured_values() {
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

    let output = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
    assert!(!output.contains(private));
    let lines = output.lines().collect::<Vec<_>>();
    assert_eq!(lines.len(), 3);
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
    let connection = Connection::open_in_memory().unwrap();
    let mut metrics = Metrics::new("read", 0, &connection);
    let completed = |caller, queue, execution| Completed {
        caller,
        queue: Duration::from_micros(queue),
        execution: Duration::from_micros(execution),
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
                failed: false,
                abandoned_reply: false,
            },
            &connection,
        );
    });

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
    assert_eq!(output.lines().count(), 1);
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
