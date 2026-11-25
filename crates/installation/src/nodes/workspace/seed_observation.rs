//! Time an initial native seed without logging from inside its storage guard.
use std::time::Instant;

pub(in crate::nodes) struct Seed(Option<Report>);

struct Report {
    operation: &'static str,
    attempt: String,
    started_at_ms: i64,
    started: Instant,
    phase_started: Instant,
    phase: &'static str,
    completed_phases: Vec<(&'static str, u64)>,
    received_bytes: u64,
    cache_hit: bool,
    reserved: bool,
    finished: bool,
}

impl Seed {
    pub(in crate::nodes) fn new(
        enabled: bool,
        operation: &'static str,
        attempt: &str,
        phase: &'static str,
        started: Instant,
    ) -> Self {
        Self((enabled && crate::performance::enabled()).then(|| Report {
            operation,
            attempt: crate::performance::identity(attempt).to_owned(),
            started_at_ms: crate::config::now() - started.elapsed().as_millis() as i64,
            started,
            phase_started: started,
            phase,
            completed_phases: Vec::new(),
            received_bytes: 0,
            cache_hit: false,
            reserved: false,
            finished: false,
        }))
    }

    pub(in crate::nodes) fn next(&mut self, phase: &'static str) {
        if let Some(report) = &mut self.0 {
            report.completed_phases.push((
                report.phase,
                report.phase_started.elapsed().as_millis() as u64,
            ));
            report.phase = phase;
            report.phase_started = Instant::now();
        }
    }

    pub(in crate::nodes) fn cache_mode(&mut self, hit: bool, reserved: bool) {
        if let Some(report) = &mut self.0 {
            report.cache_hit = hit;
            report.reserved = reserved;
        }
    }

    pub(in crate::nodes) fn received(&mut self, bytes: u64) {
        if let Some(report) = &mut self.0 {
            report.received_bytes = bytes;
        }
    }

    pub(in crate::nodes) fn finish(&mut self) {
        if let Some(report) = &mut self.0 {
            report.finished = true;
        }
    }
}

impl Drop for Seed {
    fn drop(&mut self) {
        let Some(report) = self.0.take() else {
            return;
        };
        let phase_ms = report.phase_started.elapsed().as_millis() as u64;
        let elapsed_ms = report.started.elapsed().as_millis() as u64;
        crate::performance::try_record(move |delivery| {
            tracing::info!(
                target: "cairn_performance",
                operation = report.operation,
                attempt_id = report.attempt,
                started_at_ms = report.started_at_ms,
                elapsed_ms,
                phase = report.phase,
                phase_ms,
                completed_phases = ?report.completed_phases,
                received_bytes = report.received_bytes,
                skill_cache_hit = report.cache_hit,
                reserved = report.reserved,
                completed = report.finished,
                dropped_reports_total = delivery.dropped_reports_total,
                "Initial native seed observation"
            );
        });
    }
}
