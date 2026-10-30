//! Per-actor wall timings without SQL, parameters, identifiers or per-job logs.
use std::time::{Duration, Instant};

pub(super) struct Completed {
    pub queue: Duration,
    pub execution: Duration,
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
}

impl Metrics {
    pub fn new(lane: &'static str, index: usize) -> Self {
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
        }
    }

    pub fn record(&mut self, job: &Completed) {
        let micros = |duration: Duration| duration.as_micros().min(u128::from(u64::MAX)) as u64;
        let queue_us = micros(job.queue);
        let execution_us = micros(job.execution);
        self.completed = self.completed.saturating_add(1);
        self.errors = self.errors.saturating_add(u64::from(job.failed));
        self.abandoned_replies = self
            .abandoned_replies
            .saturating_add(u64::from(job.abandoned_reply));
        self.queue_us = self.queue_us.saturating_add(queue_us);
        self.execution_us = self.execution_us.saturating_add(execution_us);
        self.max_queue_us = self.max_queue_us.max(queue_us);
        self.max_execution_us = self.max_execution_us.max(execution_us);

        let elapsed = self.since.elapsed();
        if elapsed < Duration::from_secs(30) {
            return;
        }
        tracing::info!(
            target: "cairn_performance",
            operation = "database_actor",
            lane = self.lane,
            actor = self.index,
            interval_ms = elapsed.as_millis().min(u128::from(u64::MAX)) as u64,
            completed = self.completed,
            errors = self.errors,
            abandoned_replies = self.abandoned_replies,
            queue_us = self.queue_us,
            execution_us = self.execution_us,
            max_queue_us = self.max_queue_us,
            max_execution_us = self.max_execution_us,
        );
        *self = Self::new(self.lane, self.index);
    }
}
