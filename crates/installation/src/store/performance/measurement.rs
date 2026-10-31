use super::Metrics;
use crate::store::{Pool, Store};
use rusqlite::Connection;
use std::{
    sync::{Arc, atomic::AtomicU64},
    time::Instant,
};
use tokio::sync::{mpsc, oneshot, watch};

#[cfg(target_os = "linux")]
fn process_cpu_micros() -> u128 {
    let mut value = std::mem::MaybeUninit::<libc::timespec>::uninit();
    // SAFETY: a successful clock_gettime initializes the supplied timespec.
    assert_eq!(
        unsafe { libc::clock_gettime(libc::CLOCK_PROCESS_CPUTIME_ID, value.as_mut_ptr()) },
        0
    );
    // SAFETY: the successful call above initialized the value.
    let value = unsafe { value.assume_init() };
    u128::try_from(value.tv_sec).unwrap() * 1_000_000
        + u128::try_from(value.tv_nsec).unwrap() / 1000
}

#[cfg(target_os = "linux")]
async fn sample(enabled: bool, jobs: usize) -> (u128, u128) {
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(if enabled {
            tracing::level_filters::LevelFilter::INFO
        } else {
            tracing::level_filters::LevelFilter::OFF
        })
        .with_writer(std::io::sink)
        .finish();
    let dispatcher = tracing::Dispatch::new(subscriber);
    let (sender, mut receiver) = mpsc::channel::<crate::store::Job>(256);
    let (ready, initialized) = oneshot::channel();
    let actor = std::thread::spawn(move || {
        tracing::dispatcher::with_default(&dispatcher, || {
            let mut connection = Connection::open_in_memory().unwrap();
            let mut metrics = Metrics::new("write", 0, &connection);
            ready.send(()).unwrap();
            for _ in 0..jobs {
                let completed = receiver.blocking_recv().unwrap()(&mut connection);
                metrics.record(&completed, &connection);
            }
        });
    });
    let store = Store(Arc::new(Pool {
        writer: sender.clone(),
        readers: sender,
        changes: watch::channel(0).0,
        discarded_changes: Arc::new(AtomicU64::new(0)),
    }));
    initialized.await.unwrap();
    let cpu = process_cpu_micros();
    let started = Instant::now();
    for _ in 0..jobs {
        assert_eq!(store.write(|_| Ok(1)).await.unwrap(), 1);
    }
    actor.join().unwrap();
    (started.elapsed().as_micros(), process_cpu_micros() - cpu)
}

#[cfg(target_os = "linux")]
#[tokio::test]
#[ignore = "explicit caller telemetry overhead measurement; run alone"]
async fn measure_writer_cost_sampling_overhead() {
    const JOBS: usize = 20_000;
    for enabled in [false, true] {
        sample(enabled, 1000).await;
    }
    for pair in 0..4 {
        let order = if pair % 2 == 0 {
            [false, true]
        } else {
            [true, false]
        };
        for enabled in order {
            let (elapsed, cpu) = sample(enabled, JOBS).await;
            println!(
                "{}",
                serde_json::json!({
                    "benchmark": "writer_caller_telemetry_overhead",
                    "pair": pair,
                    "enabled": enabled,
                    "jobs": JOBS,
                    "elapsedMicros": elapsed,
                    "processCpuMicros": cpu,
                    "sqlite": rusqlite::version(),
                    "profile": "test profile selected by Cargo; record flags separately",
                    "workload": "actual writer queue and no-op closure, in-memory SQLite",
                    "timing": "initialization excluded; loop and actor teardown included",
                    "limits": "local observation, not application latency or production CPU",
                })
            );
        }
    }
}

fn sample_report(enabled: bool, reports: usize) -> (u128, u128) {
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(if enabled {
            tracing::level_filters::LevelFilter::INFO
        } else {
            tracing::level_filters::LevelFilter::OFF
        })
        .with_writer(std::io::sink)
        .finish();
    tracing::subscriber::with_default(subscriber, || {
        let connection = Connection::open_in_memory().unwrap();
        let mut metrics = Metrics::new("write", 0, &connection);
        let completed = super::Completed {
            caller: "fixed_report_measurement_caller",
            queue: std::time::Duration::ZERO,
            execution: std::time::Duration::ZERO,
            cpu: None,
            failed: false,
            abandoned_reply: false,
        };
        let cpu = process_cpu_micros();
        let started = Instant::now();
        for _ in 0..reports {
            metrics.since = Instant::now() - std::time::Duration::from_secs(31);
            metrics.record(&completed, &connection);
        }
        (started.elapsed().as_micros(), process_cpu_micros() - cpu)
    })
}

#[test]
#[ignore = "explicit isolated report boundary overhead measurement; run alone"]
fn measure_isolated_report_overhead() {
    const REPORTS: usize = 100;
    for enabled in [false, true] {
        sample_report(enabled, 10);
    }
    for pair in 0..4 {
        let order = if pair % 2 == 0 {
            [false, true]
        } else {
            [true, false]
        };
        for enabled in order {
            let (elapsed, cpu) = sample_report(enabled, REPORTS);
            println!(
                "{}",
                serde_json::json!({
                    "benchmark": "isolated_database_report_overhead",
                    "pair": pair,
                    "enabled": enabled,
                    "reports": REPORTS,
                    "elapsedMicros": elapsed,
                    "processCpuMicros": cpu,
                    "sqlite": rusqlite::version(),
                    "workload": "forced actual Metrics boundary, one caller, in-memory SQLite",
                    "timing": "initialization excluded; sampling, formatting and isolation included",
                    "limits": "local observation with sink writer; not production logging or query latency",
                })
            );
        }
    }
}
