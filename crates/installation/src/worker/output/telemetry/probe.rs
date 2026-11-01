//! Optional test-owned stage counts. Never retains payloads or credentials.
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Default)]
pub(in crate::worker) struct Probe {
    pub enabled_attempts: AtomicU64,
    pub enabled_true: AtomicU64,
    pub enabled_false: AtomicU64,
    pub admission_attempts: AtomicU64,
    pub admitted: AtomicU64,
    pub not_admitted: AtomicU64,
    pub callback_entered: AtomicU64,
    pub callback_returned: AtomicU64,
    pub capture_target_events: AtomicU64,
    pub capture_record_fields: AtomicU64,
    pub capture_parse_ok: AtomicU64,
    pub capture_parse_failed: AtomicU64,
    pub capture_snapshot_objects: AtomicU64,
    pub capture_pushed: AtomicU64,
}

impl Probe {
    pub(in crate::worker) fn bump(counter: &AtomicU64) {
        let _ = counter.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
            Some(value.saturating_add(1))
        });
    }

    pub(in crate::worker) fn snapshot(&self) -> serde_json::Value {
        serde_json::json!({
            "enabled_attempts": self.enabled_attempts.load(Ordering::Relaxed),
            "enabled_true": self.enabled_true.load(Ordering::Relaxed),
            "enabled_false": self.enabled_false.load(Ordering::Relaxed),
            "admission_attempts": self.admission_attempts.load(Ordering::Relaxed),
            "admitted": self.admitted.load(Ordering::Relaxed),
            "not_admitted": self.not_admitted.load(Ordering::Relaxed),
            "callback_entered": self.callback_entered.load(Ordering::Relaxed),
            "callback_returned": self.callback_returned.load(Ordering::Relaxed),
            "capture_target_events": self.capture_target_events.load(Ordering::Relaxed),
            "capture_record_fields": self.capture_record_fields.load(Ordering::Relaxed),
            "capture_parse_ok": self.capture_parse_ok.load(Ordering::Relaxed),
            "capture_parse_failed": self.capture_parse_failed.load(Ordering::Relaxed),
            "capture_snapshot_objects": self.capture_snapshot_objects.load(Ordering::Relaxed),
            "capture_pushed": self.capture_pushed.load(Ordering::Relaxed),
        })
    }
}

#[derive(Clone, Default)]
pub(in crate::worker) struct Capture {
    reports: std::sync::Arc<std::sync::Mutex<Vec<serde_json::Value>>>,
    probe: Option<std::sync::Arc<Probe>>,
}

struct Record<'a>(&'a Capture);

impl tracing::field::Visit for Record<'_> {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() != "record" {
            return;
        }
        if let Some(probe) = &self.0.probe {
            Probe::bump(&probe.capture_record_fields);
        }
        // This is the existing test capture decision, not an extra diagnostic
        // copy. Fixed stage counters never retain or print this field/value.
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&format!("{value:?}")) else {
            if let Some(probe) = &self.0.probe {
                Probe::bump(&probe.capture_parse_failed);
            }
            return;
        };
        if let Some(probe) = &self.0.probe {
            Probe::bump(&probe.capture_parse_ok);
        }
        if value["snapshot"].is_object() {
            if let Some(probe) = &self.0.probe {
                Probe::bump(&probe.capture_snapshot_objects);
            }
            self.0.reports.lock().unwrap().push(value);
            if let Some(probe) = &self.0.probe {
                Probe::bump(&probe.capture_pushed);
            }
        }
    }
}

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for Capture {
    fn on_event(&self, event: &tracing::Event<'_>, _: tracing_subscriber::layer::Context<'_, S>) {
        if event.metadata().target() == "cairn_performance"
            && let Some(probe) = &self.probe
        {
            Probe::bump(&probe.capture_target_events);
        }
        event.record(&mut Record(self));
    }
}

impl Capture {
    pub(in crate::worker) fn with_dispatch(
        probe: Option<std::sync::Arc<Probe>>,
    ) -> (Self, tracing::Dispatch) {
        use tracing_subscriber::layer::SubscriberExt;
        let capture = Self {
            reports: std::sync::Arc::default(),
            probe,
        };
        let dispatch = tracing::Dispatch::new(tracing_subscriber::registry().with(capture.clone()));
        (capture, dispatch)
    }

    pub(in crate::worker) fn records(&self) -> Vec<serde_json::Value> {
        self.reports.lock().unwrap().clone()
    }
}
