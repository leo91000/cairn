//! Bounded, best-effort diagnostics with no caller span stack or sink backpressure.
use std::{
    sync::{
        Mutex, TryLockError,
        atomic::{AtomicU64, Ordering},
        mpsc::{self, Receiver, SyncSender, TrySendError},
    },
    time::Instant,
};

const CAPACITY: usize = 16;
static WORKER: Mutex<Option<SyncSender<Report>>> = Mutex::new(None);
static DROPPED: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, Debug)]
pub(crate) struct Delivery {
    pub queue_us: u64,
    pub dropped_reports_total: u64,
}

struct Report {
    queued_at: Instant,
    dispatch: tracing::Dispatch,
    record: Box<dyn FnOnce(Delivery) + Send>,
}

enum Rejection {
    Busy,
    Full,
    Unavailable,
}

fn dropped() {
    let _ = DROPPED.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
        Some(value.saturating_add(1))
    });
}

fn enqueue(report: Report) -> std::result::Result<(), (Rejection, Report)> {
    let mut worker = match WORKER.try_lock() {
        Ok(worker) => worker,
        Err(TryLockError::WouldBlock) => return Err((Rejection::Busy, report)),
        Err(TryLockError::Poisoned(_)) => return Err((Rejection::Unavailable, report)),
    };
    if worker.is_none() {
        let (sender, receiver) = mpsc::sync_channel(CAPACITY);
        match std::thread::Builder::new()
            .name("cairn-diagnostics".into())
            .spawn(move || run(&receiver))
        {
            // One detached worker per process; a stalled sink cannot stall callers.
            Ok(thread) => drop(thread),
            Err(_) => return Err((Rejection::Unavailable, report)),
        }
        *worker = Some(sender);
    }
    let Some(sender) = worker.as_ref() else {
        return Err((Rejection::Unavailable, report));
    };
    match sender.try_send(report) {
        Ok(()) => Ok(()),
        Err(TrySendError::Full(report)) => Err((Rejection::Full, report)),
        Err(TrySendError::Disconnected(report)) => {
            *worker = None;
            Err((Rejection::Unavailable, report))
        }
    }
}

fn run(receiver: &Receiver<Report>) {
    while let Ok(report) = receiver.recv() {
        let delivery = Delivery {
            queue_us: report.queued_at.elapsed().as_micros() as u64,
            dropped_reports_total: DROPPED.load(Ordering::Relaxed),
        };
        // Callbacks own bounded observation data, never mutable caller state.
        // Unwinding a subscriber must not terminate the worker or its next report.
        let completed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            let Report {
                dispatch, record, ..
            } = report;
            tracing::dispatcher::with_default(&dispatch, || record(delivery));
        }));
        if completed.is_err() {
            dropped();
        }
    }
}

/// Admit an owned, bounded observation without waiting for the logging sink.
/// False means disabled or rejected. No diagnostic fallback runs on the caller.
/// At most one report executes and sixteen wait; startup/queue failures and
/// callback panics count as drops. Subscriber-owned IO errors are not observable.
pub(crate) fn try_record(record: impl FnOnce(Delivery) + Send + 'static) -> bool {
    if !super::enabled() {
        return false;
    }
    let report = Report {
        queued_at: Instant::now(),
        dispatch: tracing::dispatcher::get_default(Clone::clone),
        record: Box::new(record),
    };
    match enqueue(report) {
        Ok(()) => true,
        Err((_, report)) => {
            dropped();
            drop(report);
            false
        }
    }
}

/// Test-only barrier: release controlled sinks before flushing, never wait forever.
#[cfg(test)]
pub(crate) fn flush(timeout: std::time::Duration) -> bool {
    let started = Instant::now();
    match WORKER.try_lock() {
        Ok(worker) if worker.is_none() => return true,
        Ok(_) | Err(TryLockError::WouldBlock) => {}
        Err(TryLockError::Poisoned(_)) => return false,
    }
    let (sender, receiver) = mpsc::channel();
    let mut report = Report {
        queued_at: Instant::now(),
        dispatch: tracing::dispatcher::get_default(Clone::clone),
        record: Box::new(move |_| {
            let _ = sender.send(());
        }),
    };
    loop {
        match enqueue(report) {
            Ok(()) => break,
            Err((Rejection::Unavailable, _)) => return false,
            Err((Rejection::Busy | Rejection::Full, pending)) => report = pending,
        }
        let remaining = timeout.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            return false;
        }
        std::thread::sleep(remaining.min(std::time::Duration::from_millis(1)));
    }
    receiver
        .recv_timeout(timeout.saturating_sub(started.elapsed()))
        .is_ok()
}
