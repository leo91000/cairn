//! One private-context-free summary of the manager's maintenance preparation.
use std::time::Duration;
use tokio::time::Instant;

#[derive(Clone, Copy)]
pub(super) enum Phase {
    Inventory,
    Preparation,
    PauseRequest,
    Capture,
    CaptureWait,
    PlacementRelease,
}

impl Phase {
    fn index(self) -> usize {
        self as usize
    }

    fn label(self) -> &'static str {
        match self {
            Self::Inventory => "inventory",
            Self::Preparation => "preparation",
            Self::PauseRequest => "pause_request",
            Self::Capture => "capture",
            Self::CaptureWait => "capture_wait",
            Self::PlacementRelease => "placement_release",
        }
    }
}

pub(super) enum Outcome {
    Completed,
    Error,
    Deadline,
    Cancelled,
}

impl Outcome {
    fn label(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Error => "error",
            Self::Deadline => "deadline",
            Self::Cancelled => "cancelled",
        }
    }
}

#[derive(Clone, Copy, Default)]
pub(super) struct Counters {
    pub attempts: u64,
    pub stale_attempts: u64,
    pub pause_requests: u64,
    pub pause_failures: u64,
    pub capture_calls: u64,
    pub capture_waits: u64,
    pub capture_failures: u64,
    pub placement_releases: u64,
    pub placement_failures: u64,
}

pub(super) struct Drain {
    started: Instant,
    phase_started: Instant,
    phase: Phase,
    durations: [Duration; 6],
    finished: bool,
    pub counters: Counters,
}

impl Drain {
    pub fn new() -> Self {
        let now = Instant::now();
        Self {
            started: now,
            phase_started: now,
            phase: Phase::Inventory,
            durations: [Duration::ZERO; 6],
            finished: false,
            counters: Counters::default(),
        }
    }

    fn settle(&mut self, now: Instant) {
        self.durations[self.phase.index()] += now.saturating_duration_since(self.phase_started);
        self.phase_started = now;
    }

    pub fn next(&mut self, phase: Phase) {
        self.settle(Instant::now());
        self.phase = phase;
    }

    pub fn finish(mut self, outcome: Outcome) {
        self.finished = true;
        self.record(outcome);
    }

    fn record(&mut self, outcome: Outcome) {
        let now = Instant::now();
        self.settle(now);
        let summary = Summary {
            phase: self.phase.label(),
            outcome: outcome.label(),
            elapsed_ms: now.saturating_duration_since(self.started).as_millis() as u64,
            durations_ms: self.durations.map(|duration| duration.as_millis() as u64),
            counters: self.counters,
        };
        let _ = crate::performance::try_record(move |delivery| summary.emit(delivery));
    }
}

struct Summary {
    phase: &'static str,
    outcome: &'static str,
    elapsed_ms: u64,
    durations_ms: [u64; 6],
    counters: Counters,
}

impl Summary {
    fn emit(self, delivery: crate::performance::Delivery) {
        tracing::info!(
            target: "cairn_performance",
            parent: None,
            operation = "node_maintenance_drain",
            outcome = self.outcome,
            phase = self.phase,
            elapsed_ms = self.elapsed_ms,
            diagnostic_queue_us = delivery.queue_us,
            dropped_reports_total = delivery.dropped_reports_total,
            inventory_ms = self.durations_ms[Phase::Inventory.index()],
            preparation_ms = self.durations_ms[Phase::Preparation.index()],
            pause_request_ms = self.durations_ms[Phase::PauseRequest.index()],
            capture_ms = self.durations_ms[Phase::Capture.index()],
            capture_wait_ms = self.durations_ms[Phase::CaptureWait.index()],
            placement_release_ms = self.durations_ms[Phase::PlacementRelease.index()],
            attempts = self.counters.attempts,
            stale_attempts = self.counters.stale_attempts,
            pause_requests = self.counters.pause_requests,
            pause_failures = self.counters.pause_failures,
            capture_calls = self.counters.capture_calls,
            capture_waits = self.counters.capture_waits,
            capture_failures = self.counters.capture_failures,
            placement_releases = self.counters.placement_releases,
            placement_failures = self.counters.placement_failures,
        );
    }
}

impl Drop for Drain {
    fn drop(&mut self) {
        if !self.finished {
            self.record(Outcome::Cancelled);
        }
    }
}
