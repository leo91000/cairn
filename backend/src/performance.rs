//! Local structured timings. Identities are run/VM IDs, never paths, URLs or credentials.
use std::time::Instant;

pub(crate) struct Operation {
    operation: &'static str,
    id: String,
    phase: &'static str,
    started: Instant,
    phase_started: Instant,
    finished: bool,
}

impl Operation {
    pub(crate) fn new(operation: &'static str, id: &str, phase: &'static str) -> Self {
        let now = Instant::now();
        tracing::info!(target: "leo_performance", operation, id, phase, event = "started");
        Self {
            operation,
            id: id.to_owned(),
            phase,
            started: now,
            phase_started: now,
            finished: false,
        }
    }

    pub(crate) fn next(&mut self, phase: &'static str) {
        self.record("phase_completed");
        self.phase = phase;
        self.phase_started = Instant::now();
        tracing::info!(target: "leo_performance", operation = self.operation, id = self.id, phase, event = "started");
    }

    pub(crate) fn finish(mut self) {
        self.record("completed");
        self.finished = true;
    }

    fn record(&self, event: &'static str) {
        tracing::info!(target: "leo_performance", operation = self.operation, id = self.id,
            phase = self.phase, event, phase_ms = self.phase_started.elapsed().as_millis() as u64,
            total_ms = self.started.elapsed().as_millis() as u64);
    }
}

impl Drop for Operation {
    fn drop(&mut self) {
        if !self.finished {
            self.record("incomplete");
        }
    }
}
