use super::Completed;
use std::{collections::HashMap, time::Duration};

const CAPACITY: usize = 64;
const TOP: usize = 8;

fn micros(value: Duration) -> u64 {
    value.as_micros().min(u128::from(u64::MAX)) as u64
}

#[derive(Clone, Copy, Default)]
pub(super) struct Totals {
    pub completed: u64,
    pub errors: u64,
    pub abandoned_replies: u64,
    pub queue_us: u64,
    pub execution_us: u64,
    pub cpu_us: u64,
    pub cpu_sampled_jobs: u64,
}

impl Totals {
    fn record(&mut self, job: &Completed) {
        self.completed = self.completed.saturating_add(1);
        self.errors = self.errors.saturating_add(u64::from(job.failed));
        self.abandoned_replies = self
            .abandoned_replies
            .saturating_add(u64::from(job.abandoned_reply));
        self.queue_us = self.queue_us.saturating_add(micros(job.queue));
        self.execution_us = self.execution_us.saturating_add(micros(job.execution));
        if let Some(cpu) = job.cpu {
            self.cpu_us = self.cpu_us.saturating_add(micros(cpu));
            self.cpu_sampled_jobs = self.cpu_sampled_jobs.saturating_add(1);
        }
    }

    fn subtract(&mut self, other: Self) {
        self.completed = self.completed.saturating_sub(other.completed);
        self.errors = self.errors.saturating_sub(other.errors);
        self.abandoned_replies = self
            .abandoned_replies
            .saturating_sub(other.abandoned_replies);
        self.queue_us = self.queue_us.saturating_sub(other.queue_us);
        self.execution_us = self.execution_us.saturating_sub(other.execution_us);
        self.cpu_us = self.cpu_us.saturating_sub(other.cpu_us);
        self.cpu_sampled_jobs = self.cpu_sampled_jobs.saturating_sub(other.cpu_sampled_jobs);
    }

    fn emit(&self, caller: &'static str, index: usize, window: u64, interval_ms: u64, other: bool) {
        tracing::info!(
            target: "cairn_performance",
            parent: None,
            operation = "database_caller",
            lane = "write",
            actor = index,
            window,
            interval_ms,
            caller,
            other,
            completed = self.completed,
            errors = self.errors,
            abandoned_replies = self.abandoned_replies,
            queue_us = self.queue_us,
            execution_us = self.execution_us,
            cpu_us = self.cpu_us,
            cpu_sampled_jobs = self.cpu_sampled_jobs,
            cpu_available = self.cpu_sampled_jobs == self.completed,
        );
    }
}

#[derive(Default)]
pub(super) struct Callers {
    tracked: HashMap<&'static str, Totals>,
    pub recorded_jobs: u64,
    pub untracked_jobs: u64,
}

impl Callers {
    pub fn record(&mut self, job: &Completed) {
        self.recorded_jobs = self.recorded_jobs.saturating_add(1);
        if let Some(totals) = self.tracked.get_mut(job.caller) {
            totals.record(job);
            return;
        }
        if self.tracked.len() == CAPACITY {
            self.untracked_jobs = self.untracked_jobs.saturating_add(1);
            return;
        }
        let mut totals = Totals::default();
        totals.record(job);
        self.tracked.insert(job.caller, totals);
    }

    pub fn len(&self) -> usize {
        self.tracked.len()
    }

    pub fn emit(&self, index: usize, window: u64, interval_ms: u64, mut remaining: Totals) {
        let mut by_wall = self.tracked.iter().collect::<Vec<_>>();
        by_wall.sort_by_key(|(caller, totals)| (std::cmp::Reverse(totals.execution_us), **caller));
        let mut selected = by_wall.iter().take(TOP).copied().collect::<Vec<_>>();
        let mut by_cpu = by_wall;
        by_cpu.sort_by_key(|(caller, totals)| (std::cmp::Reverse(totals.cpu_us), **caller));
        for candidate in by_cpu.into_iter().take(TOP) {
            if !selected.iter().any(|(caller, _)| *caller == candidate.0) {
                selected.push(candidate);
            }
        }
        for (caller, totals) in selected {
            totals.emit(caller, index, window, interval_ms, false);
            remaining.subtract(*totals);
        }
        if remaining.completed > 0 {
            remaining.emit("other", index, window, interval_ms, true);
        }
    }
}
