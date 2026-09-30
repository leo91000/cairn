//! Local structured timings. Identities are run/VM/transfer IDs, never paths or credentials.
use std::time::{Duration, Instant};

mod activity;

pub use activity::{Activity, heartbeat};

/// Aggregate stream waits without emitting a log for every archive chunk.
#[derive(Default)]
pub(crate) struct StreamMetrics {
    pub bytes: u64,
    pub chunks: u64,
    pub read: Duration,
    pub write: Duration,
}

impl StreamMetrics {
    pub(crate) fn record(&self, id: &str, side: &'static str) {
        tracing::info!(
            target: "leo_performance",
            operation = "vm_archive_stream",
            id,
            side,
            bytes = self.bytes,
            chunks = self.chunks,
            read_ms = self.read.as_millis() as u64,
            write_ms = self.write.as_millis() as u64
        );
    }
}

/// Optional correlation fields from older plans may be absent. Never log arbitrary values.
pub fn identity(value: &str) -> &str {
    if uuid::Uuid::parse_str(value).is_ok() {
        value
    } else {
        "unknown"
    }
}

/// Observes slow awaits without timeouts, cancellation, or another database write.
/// The enclosing tracing span supplies run/attempt correlation.
pub async fn wait<T>(phase: &'static str, future: impl std::future::Future<Output = T>) -> T {
    let started = tokio::time::Instant::now();
    let mut heartbeat = heartbeat();
    tokio::pin!(future);
    let result = loop {
        tokio::select! {
            result = &mut future => break result,
            _ = heartbeat.tick() => {
                tracing::info!(target: "leo_performance", operation = "agent_wait", phase, event = "waiting", elapsed_ms = started.elapsed().as_millis() as u64);
            }
        }
    };
    let elapsed_ms = started.elapsed().as_millis() as u64;
    if elapsed_ms >= 100 {
        tracing::info!(target: "leo_performance", operation = "agent_wait", phase, event = "completed", elapsed_ms);
    }
    result
}

/// Times one operation and its phases; dropping it unfinished records `incomplete`.
pub(crate) struct Operation {
    operation: &'static str,
    id: String,
    phase: &'static str,
    started: Instant,
    phase_started: Instant,
    finished: bool,
}

impl Operation {
    pub(crate) fn id(&self) -> &str {
        &self.id
    }

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

    /// Closes the current phase and starts `phase`.
    pub(crate) fn next(&mut self, phase: &'static str) {
        self.record("phase_completed");
        self.phase = phase;
        self.phase_started = Instant::now();
        tracing::info!(
            target: "leo_performance",
            operation = self.operation,
            id = self.id,
            phase,
            event = "started"
        );
    }

    pub(crate) fn finish(mut self) {
        self.record("completed");
        self.finished = true;
    }

    fn record(&self, event: &'static str) {
        let phase_ms = self.phase_started.elapsed().as_millis() as u64;
        let total_ms = self.started.elapsed().as_millis() as u64;
        tracing::info!(
            target: "leo_performance",
            operation = self.operation,
            id = self.id,
            phase = self.phase,
            event,
            phase_ms,
            total_ms
        );
    }
}

impl Drop for Operation {
    fn drop(&mut self) {
        if !self.finished {
            self.record("incomplete");
        }
    }
}
