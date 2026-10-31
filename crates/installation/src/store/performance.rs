//! Per-actor timings and bounded caller costs, without SQL or captured values.
use rusqlite::Connection;
use std::time::{Duration, Instant};

mod cache;
mod callers;
mod cpu;
mod wal;

use cache::Cache;
use callers::{Callers, Totals};
pub(super) use cpu::CpuTime;
use wal::Wal;

#[cfg(test)]
mod tests;

#[cfg(all(test, target_os = "linux"))]
mod measurement;

pub(super) struct Completed {
    pub caller: &'static str,
    pub queue: Duration,
    pub execution: Duration,
    pub cpu: Option<Duration>,
    pub failed: bool,
    pub abandoned_reply: bool,
}

pub(super) struct Metrics {
    lane: &'static str,
    index: usize,
    since: Instant,
    completed: u64,
    errors: u64,
    abandoned_replies: u64,
    queue_us: u64,
    execution_us: u64,
    max_queue_us: u64,
    max_execution_us: u64,
    max_queue_caller: &'static str,
    max_execution_caller: &'static str,
    cache: Option<Cache>,
    window: u64,
    cpu_us: u64,
    cpu_sampled_jobs: u64,
    callers: Callers,
}

impl Metrics {
    pub fn new(lane: &'static str, index: usize, connection: &Connection) -> Self {
        Self::window(lane, index, Cache::read(connection), 0)
    }

    fn window(lane: &'static str, index: usize, cache: Option<Cache>, window: u64) -> Self {
        Self {
            lane,
            index,
            since: Instant::now(),
            completed: 0,
            errors: 0,
            abandoned_replies: 0,
            queue_us: 0,
            execution_us: 0,
            max_queue_us: 0,
            max_execution_us: 0,
            max_queue_caller: "none",
            max_execution_caller: "none",
            cache,
            window,
            cpu_us: 0,
            cpu_sampled_jobs: 0,
            callers: Callers::default(),
        }
    }

    pub fn record(&mut self, job: &Completed, connection: &Connection) {
        let micros = |duration: Duration| duration.as_micros().min(u128::from(u64::MAX)) as u64;
        let queue_us = micros(job.queue);
        let execution_us = micros(job.execution);
        if self.completed == 0 || queue_us > self.max_queue_us {
            self.max_queue_caller = job.caller;
        }
        if self.completed == 0 || execution_us > self.max_execution_us {
            self.max_execution_caller = job.caller;
        }
        self.completed = self.completed.saturating_add(1);
        self.errors = self.errors.saturating_add(u64::from(job.failed));
        self.abandoned_replies = self
            .abandoned_replies
            .saturating_add(u64::from(job.abandoned_reply));
        self.queue_us = self.queue_us.saturating_add(queue_us);
        self.execution_us = self.execution_us.saturating_add(execution_us);
        self.max_queue_us = self.max_queue_us.max(queue_us);
        self.max_execution_us = self.max_execution_us.max(execution_us);
        if let Some(cpu) = job.cpu {
            self.cpu_us = self.cpu_us.saturating_add(micros(cpu));
            self.cpu_sampled_jobs = self.cpu_sampled_jobs.saturating_add(1);
        }
        if crate::performance::enabled() {
            self.callers.record(job);
        }

        let elapsed = self.since.elapsed();
        if elapsed < Duration::from_secs(30) {
            return;
        }
        let cache = Cache::read(connection);
        let delta = cache
            .zip(self.cache)
            .and_then(|(now, before)| now.delta(before));
        let cache_available = delta.is_some();
        let cache_values = delta.unwrap_or_default();
        // These are point-in-time values at window end, not interval deltas.
        // Only the writer samples WAL state; NOOP never performs a checkpoint.
        let wal = if self.lane == "write" {
            Wal::read(connection)
        } else {
            None
        };
        let wal_available = wal.is_some();
        let wal_values = wal.unwrap_or_default();
        let interval_ms = elapsed.as_millis().min(u128::from(u64::MAX)) as u64;
        crate::performance::without_span_context(|| {
            tracing::info!(
                target: "cairn_performance",
                parent: None,
                operation = "database_actor",
                lane = self.lane,
                actor = self.index,
                interval_ms,
                window = self.window,
                completed = self.completed,
                errors = self.errors,
                abandoned_replies = self.abandoned_replies,
                queue_us = self.queue_us,
                execution_us = self.execution_us,
                max_queue_us = self.max_queue_us,
                max_execution_us = self.max_execution_us,
                max_queue_caller = self.max_queue_caller,
                max_execution_caller = self.max_execution_caller,
                cache_available,
                cache_hits = cache_values.hits,
                cache_misses = cache_values.misses,
                cache_writes = cache_values.writes,
                cache_bytes = cache_values.bytes,
                wal_available,
                wal_frames = wal_values.frames,
                wal_checkpointed_frames = wal_values.checkpointed_frames,
                wal_pending_frames = wal_values.pending_frames,
                wal_autocheckpoint_frames = wal_values.autocheckpoint_frames,
                sqlite_synchronous = wal_values.synchronous,
                cpu_us = self.cpu_us,
                cpu_sampled_jobs = self.cpu_sampled_jobs,
                cpu_available = self.cpu_sampled_jobs == self.completed,
                caller_recorded_jobs = self.callers.recorded_jobs,
                caller_untracked_jobs = self.callers.untracked_jobs,
                tracked_callers = self.callers.len(),
                caller_coverage_complete = self.callers.recorded_jobs == self.completed
                    && self.callers.untracked_jobs == 0,
            );
            self.callers.emit(
                self.lane,
                self.index,
                self.window,
                interval_ms,
                Totals {
                    completed: self.completed,
                    errors: self.errors,
                    abandoned_replies: self.abandoned_replies,
                    queue_us: self.queue_us,
                    execution_us: self.execution_us,
                    cpu_us: self.cpu_us,
                    cpu_sampled_jobs: self.cpu_sampled_jobs,
                },
            );
        });
        *self = Self::window(self.lane, self.index, cache, self.window.saturating_add(1));
    }
}

/// A closure's Rust type names its source function, never its captured values.
/// Keep the outer caller before write/transaction wrap it in their own closure.
/// This is a diagnostic hint within one build, not a stable operation identifier.
pub(super) fn caller<T>(value: &T) -> &'static str {
    let name = std::any::type_name_of_val(value);
    let mut end = name.len().min(256);
    while !name.is_char_boundary(end) {
        end -= 1;
    }
    &name[..end]
}
