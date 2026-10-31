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

pub(super) struct Drain {
    started: Instant,
    phase_started: Instant,
    phase: Phase,
    durations: [Duration; 6],
    finished: bool,
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

impl Drain {
    pub fn new() -> Self {
        let now = Instant::now();
        Self {
            started: now,
            phase_started: now,
            phase: Phase::Inventory,
            durations: [Duration::ZERO; 6],
            finished: false,
            attempts: 0,
            stale_attempts: 0,
            pause_requests: 0,
            pause_failures: 0,
            capture_calls: 0,
            capture_waits: 0,
            capture_failures: 0,
            placement_releases: 0,
            placement_failures: 0,
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
        let milliseconds = |phase: Phase| self.durations[phase.index()].as_millis() as u64;
        // Formatting layers may skip a root span and restore older context.
        // Emit this single summary with an empty thread-local span stack.
        crate::performance::without_span_context(|| {
            let root = tracing::info_span!(target: "cairn_performance", parent: None, "maintenance_summary");
            if root.is_disabled() {
                return;
            }
            let _entered = root.enter();
            tracing::info!(
                target: "cairn_performance",
                parent: &root,
                operation = "node_maintenance_drain",
                outcome = outcome.label(),
                phase = self.phase.label(),
                elapsed_ms = now.saturating_duration_since(self.started).as_millis() as u64,
                inventory_ms = milliseconds(Phase::Inventory),
                preparation_ms = milliseconds(Phase::Preparation),
                pause_request_ms = milliseconds(Phase::PauseRequest),
                capture_ms = milliseconds(Phase::Capture),
                capture_wait_ms = milliseconds(Phase::CaptureWait),
                placement_release_ms = milliseconds(Phase::PlacementRelease),
                attempts = self.attempts,
                stale_attempts = self.stale_attempts,
                pause_requests = self.pause_requests,
                pause_failures = self.pause_failures,
                capture_calls = self.capture_calls,
                capture_waits = self.capture_waits,
                capture_failures = self.capture_failures,
                placement_releases = self.placement_releases,
                placement_failures = self.placement_failures,
            );
        });
    }
}

impl Drop for Drain {
    fn drop(&mut self) {
        if !self.finished {
            self.record(Outcome::Cancelled);
        }
    }
}
