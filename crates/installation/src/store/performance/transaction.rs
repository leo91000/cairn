//! Finished slow transactions, without SQL, payloads or a connection on the worker.
use super::CpuTime;
use std::time::{Duration, Instant};

const SLOW_TRANSACTION: Duration = Duration::from_millis(250);

#[derive(Clone, Copy)]
enum Phase {
    Begin,
    Body,
    Commit,
    Done,
}

#[derive(Default)]
struct Cost {
    wall: Duration,
    cpu: Option<Duration>,
}

struct Observation {
    caller: &'static str,
    started: Instant,
    phase_started: Instant,
    cpu: CpuTime,
    phase: Phase,
    begin: Cost,
    body: Cost,
    commit: Cost,
    outcome: &'static str,
}

impl Observation {
    fn advance(&mut self, next: Phase) {
        if matches!(self.phase, Phase::Done) {
            return;
        }
        let now = Instant::now();
        let cost = Cost {
            wall: now.duration_since(self.phase_started),
            cpu: self.cpu.checkpoint(),
        };
        match self.phase {
            Phase::Begin => self.begin = cost,
            Phase::Body => self.body = cost,
            Phase::Commit => self.commit = cost,
            Phase::Done => return,
        }
        self.phase_started = now;
        self.phase = next;
    }
}

pub(crate) struct TransactionTimings(Option<Observation>);

impl TransactionTimings {
    pub(crate) fn new(caller: &'static str) -> Self {
        if !crate::performance::enabled() {
            return Self(None);
        }
        let now = Instant::now();
        Self(Some(Observation {
            caller,
            started: now,
            phase_started: now,
            cpu: CpuTime::start(),
            phase: Phase::Begin,
            begin: Cost::default(),
            body: Cost::default(),
            commit: Cost::default(),
            outcome: "aborted",
        }))
    }

    pub(crate) fn begun(&mut self) {
        if let Some(observation) = &mut self.0 {
            observation.advance(Phase::Body);
        }
    }

    pub(crate) fn executed(&mut self) {
        if let Some(observation) = &mut self.0 {
            observation.advance(Phase::Commit);
        }
    }

    pub(crate) fn committed(&mut self) {
        if let Some(observation) = &mut self.0 {
            observation.advance(Phase::Done);
            observation.outcome = "committed";
        }
    }

    pub(crate) fn failed(&mut self) {
        if let Some(observation) = &mut self.0 {
            observation.outcome = match observation.phase {
                Phase::Begin => "begin_error",
                Phase::Body => "body_error",
                Phase::Commit => "commit_error",
                Phase::Done => "aborted",
            };
        }
    }
}

impl Drop for TransactionTimings {
    fn drop(&mut self) {
        let Some(mut observation) = self.0.take() else {
            return;
        };
        observation.advance(Phase::Done);
        let total = observation.started.elapsed();
        if total < SLOW_TRANSACTION {
            return;
        }
        let micros = |value: Duration| value.as_micros().min(u128::from(u64::MAX)) as u64;
        let total_us = micros(total);
        let measured = observation.begin.wall + observation.body.wall + observation.commit.wall;
        let other_us = micros(total.saturating_sub(measured));
        crate::performance::try_record(move |delivery| {
            tracing::info!(
                target: "cairn_performance",
                parent: None,
                operation = "database_transaction",
                lane = "write",
                actor = 0_u64,
                caller = observation.caller,
                outcome = observation.outcome,
                total_us,
                begin_us = micros(observation.begin.wall),
                body_us = micros(observation.body.wall),
                commit_us = micros(observation.commit.wall),
                other_us,
                begin_cpu_available = observation.begin.cpu.is_some(),
                begin_cpu_us = observation.begin.cpu.map_or(0, micros),
                body_cpu_available = observation.body.cpu.is_some(),
                body_cpu_us = observation.body.cpu.map_or(0, micros),
                commit_cpu_available = observation.commit.cpu.is_some(),
                commit_cpu_us = observation.commit.cpu.map_or(0, micros),
                diagnostic_queue_us = delivery.queue_us,
                diagnostic_dropped_reports_total = delivery.dropped_reports_total,
            );
        });
    }
}
