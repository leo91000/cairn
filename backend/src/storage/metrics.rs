//! Bounded counters: no per-I/O logging, allocation, or stored samples.
use serde_json::{Value, json};
use std::{
    sync::atomic::{AtomicU64, Ordering::Relaxed},
    time::Instant,
};

#[derive(Default)]
pub(crate) struct Counter {
    count: AtomicU64,
    bytes: AtomicU64,
    micros: AtomicU64,
    max_micros: AtomicU64,
    errors: AtomicU64,
    over_10ms: AtomicU64,
    over_100ms: AtomicU64,
    over_1s: AtomicU64,
}

pub(crate) struct Sample<'a> {
    counter: &'a Counter,
    started: Instant,
    bytes: u64,
    success: bool,
}

impl Counter {
    pub(crate) fn start(&self) -> Sample<'_> {
        Sample {
            counter: self,
            started: Instant::now(),
            bytes: 0,
            success: false,
        }
    }

    pub(crate) fn snapshot(&self) -> Value {
        json!({
            "count": self.count.load(Relaxed),
            "bytes": self.bytes.load(Relaxed),
            "totalMicros": self.micros.load(Relaxed),
            "maxMicros": self.max_micros.load(Relaxed),
            "errors": self.errors.load(Relaxed),
            "over10Ms": self.over_10ms.load(Relaxed),
            "over100Ms": self.over_100ms.load(Relaxed),
            "over1s": self.over_1s.load(Relaxed)
        })
    }
}

impl Sample<'_> {
    pub(crate) fn finish(mut self, bytes: usize) {
        self.bytes = bytes as u64;
        self.success = true;
    }
}

impl Drop for Sample<'_> {
    fn drop(&mut self) {
        let micros = u64::try_from(self.started.elapsed().as_micros()).unwrap_or(u64::MAX);
        let counter = self.counter;
        counter.count.fetch_add(1, Relaxed);
        counter.bytes.fetch_add(self.bytes, Relaxed);
        counter.micros.fetch_add(micros, Relaxed);
        counter.max_micros.fetch_max(micros, Relaxed);
        counter.errors.fetch_add(u64::from(!self.success), Relaxed);
        counter
            .over_10ms
            .fetch_add(u64::from(micros >= 10_000), Relaxed);
        counter
            .over_100ms
            .fetch_add(u64::from(micros >= 100_000), Relaxed);
        counter
            .over_1s
            .fetch_add(u64::from(micros >= 1_000_000), Relaxed);
    }
}

#[derive(Default)]
pub(super) struct Metrics {
    pub reads: Counter,
    pub writes: Counter,
    pub syncs: Counter,
    pub remote: Counter,
    pub verification: Counter,
    pub memory_hits: AtomicU64,
    pub coalesced_reads: AtomicU64,
    pub disk_hits: AtomicU64,
    pub journal_rows: AtomicU64,
}

impl Metrics {
    pub fn snapshot(&self) -> Value {
        json!({
            "read": self.reads.snapshot(),
            "write": self.writes.snapshot(),
            "sync": self.syncs.snapshot(),
            "remoteFetch": self.remote.snapshot(),
            "blockVerification": self.verification.snapshot(),
            "memoryHits": self.memory_hits.load(Relaxed),
            "coalescedReads": self.coalesced_reads.load(Relaxed),
            "diskHits": self.disk_hits.load(Relaxed),
            "journalRows": self.journal_rows.load(Relaxed)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counters_include_failed_operations_without_counting_their_bytes() {
        let counter = Counter::default();
        counter.start().finish(4096);
        drop(counter.start());
        let value = counter.snapshot();
        assert_eq!(value["count"], 2);
        assert_eq!(value["bytes"], 4096);
        assert_eq!(value["errors"], 1);
    }
}
