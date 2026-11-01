//! Optional test-owned stage counts. Never retains payloads or credentials.
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU64, Ordering},
};

#[derive(Default)]
pub(in crate::worker) struct Probe {
    pub saved_capture_matches: AtomicBool,
    pub current_capture_matches: AtomicBool,
    pub static_info_allowed: AtomicBool,
    pub runtime_info_allowed: AtomicBool,
    pub catch_ok_true: AtomicU64,
    pub catch_ok_false: AtomicU64,
    pub catch_panicked: AtomicU64,
    pub capture_gate_registrations: AtomicU64,
    pub capture_gate_enabled_callbacks: AtomicU64,
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
    pub(super) fn matches_dispatch(self: &Arc<Self>, dispatch: &tracing::Dispatch) -> bool {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            dispatch
                .downcast_ref::<Capture>()
                .and_then(|capture| capture.probe.as_ref())
                .is_some_and(|captured| Arc::ptr_eq(self, captured))
        }))
        .unwrap_or(false)
    }

    pub(super) fn enabled_observation(
        &self,
        static_info: bool,
        runtime_info: bool,
        outcome: Option<crate::performance::EnabledOutcome>,
    ) {
        self.static_info_allowed
            .store(static_info, Ordering::Relaxed);
        self.runtime_info_allowed
            .store(runtime_info, Ordering::Relaxed);
        let counter = match outcome {
            Some(crate::performance::EnabledOutcome::True) => &self.catch_ok_true,
            Some(crate::performance::EnabledOutcome::False) => &self.catch_ok_false,
            Some(crate::performance::EnabledOutcome::Panicked) => &self.catch_panicked,
            None => return,
        };
        Self::bump(counter);
    }

    pub(in crate::worker) fn bump(counter: &AtomicU64) {
        let _ = counter.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
            Some(value.saturating_add(1))
        });
    }

    pub(in crate::worker) fn snapshot(&self) -> serde_json::Value {
        serde_json::json!({
            "saved_capture_matches": self.saved_capture_matches.load(Ordering::Relaxed),
            "current_capture_matches": self.current_capture_matches.load(Ordering::Relaxed),
            "static_info_allowed": self.static_info_allowed.load(Ordering::Relaxed),
            "runtime_info_allowed": self.runtime_info_allowed.load(Ordering::Relaxed),
            "catch_ok_true": self.catch_ok_true.load(Ordering::Relaxed),
            "catch_ok_false": self.catch_ok_false.load(Ordering::Relaxed),
            "catch_panicked": self.catch_panicked.load(Ordering::Relaxed),
            "capture_gate_registrations": self.capture_gate_registrations.load(Ordering::Relaxed),
            "capture_gate_enabled_callbacks": self.capture_gate_enabled_callbacks.load(Ordering::Relaxed),
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

#[derive(Clone)]
pub(in crate::worker) struct Capture {
    // Retain a second registrar before constructing the real scoped Dispatch.
    // Otherwise an actor's first registration under NONE can cache never in
    // tracing's single-dispatch fast path, independently of this capture.
    _callsite_registration: Arc<tracing::Dispatch>,
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
    fn register_callsite(
        &self,
        metadata: &'static tracing::Metadata<'static>,
    ) -> tracing::subscriber::Interest {
        if crate::performance::is_enabled_callsite(metadata)
            && let Some(probe) = &self.probe
        {
            Probe::bump(&probe.capture_gate_registrations);
        }
        // Preserve Layer's default enabled registration behavior.
        tracing::subscriber::Interest::always()
    }

    fn enabled(
        &self,
        metadata: &tracing::Metadata<'_>,
        _: tracing_subscriber::layer::Context<'_, S>,
    ) -> bool {
        if crate::performance::is_enabled_callsite(metadata)
            && let Some(probe) = &self.probe
        {
            Probe::bump(&probe.capture_gate_enabled_callbacks);
        }
        true
    }

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
        let callsite_registration = Arc::new(tracing::Dispatch::new(
            tracing::subscriber::NoSubscriber::default(),
        ));
        let capture = Self {
            _callsite_registration: callsite_registration,
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
