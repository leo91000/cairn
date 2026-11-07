//! Controlled real Subscriber callbacks at registration, receipt and final Drop.
//! These tests must be executed through the credential-isolated hosted launcher.
use super::*;
use std::{
    fmt,
    io::Read,
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{
        Weak,
        atomic::{AtomicUsize, Ordering},
    },
};
use tracing::{
    Event, Metadata, Subscriber,
    field::{Field, Visit},
    span::{Attributes, Id, Record},
};
use tracing_core::span::Current;

const CURRENT: usize = 1;
const CLONE: usize = 2;
const CLOSE: usize = 4;
const ENTER: usize = 8;
const EXIT: usize = 16;
const EVENT: usize = 32;

#[derive(Default)]
struct Probe {
    target: Mutex<Option<Weak<Mutex<State>>>>,
    metadata: Mutex<Option<&'static Metadata<'static>>>,
    panic_mask: AtomicUsize,
    blocked: AtomicUsize,
    currents: AtomicUsize,
    clones: AtomicUsize,
    closes: AtomicUsize,
    receipts: Mutex<Vec<String>>,
}

impl Probe {
    fn bind(&self, diagnostics: &Diagnostics) {
        *self.target.lock().unwrap() = Some(Arc::downgrade(&diagnostics.0));
    }

    fn callback(&self, stage: usize) {
        let target = self.target.lock().unwrap().clone();
        if let Some(state) = target.and_then(|state| state.upgrade()) {
            // Attempt actual reentrant access without letting the pre-fix bug
            // deadlock an entire hosted test process.
            if state.try_lock().is_err() {
                self.blocked.fetch_add(1, Ordering::Relaxed);
            }
        }
        if self.panic_mask.fetch_and(!stage, Ordering::Relaxed) & stage != 0 {
            panic!("controlled diagnostic Subscriber callback");
        }
    }
}

struct Controlled(Arc<Probe>);

struct ReceiptVisitor<'a>(&'a Probe);

impl Visit for ReceiptVisitor<'_> {
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        if field.name() == "termination" {
            self.0.receipts.lock().unwrap().push(format!("{value:?}"));
        }
    }
}

impl Subscriber for Controlled {
    fn enabled(&self, _: &Metadata<'_>) -> bool {
        true
    }

    fn new_span(&self, attributes: &Attributes<'_>) -> Id {
        *self.0.metadata.lock().unwrap() = Some(attributes.metadata());
        Id::from_u64(1)
    }

    fn record(&self, _: &Id, _: &Record<'_>) {}

    fn record_follows_from(&self, _: &Id, _: &Id) {}

    fn enter(&self, _: &Id) {
        self.0.callback(ENTER);
    }

    fn exit(&self, _: &Id) {
        self.0.callback(EXIT);
    }

    fn current_span(&self) -> Current {
        self.0.currents.fetch_add(1, Ordering::Relaxed);
        self.0.callback(CURRENT);
        match *self.0.metadata.lock().unwrap() {
            Some(metadata) => Current::new(Id::from_u64(1), metadata),
            None => Current::none(),
        }
    }

    fn clone_span(&self, id: &Id) -> Id {
        self.0.clones.fetch_add(1, Ordering::Relaxed);
        self.0.callback(CLONE);
        id.clone()
    }

    fn try_close(&self, _: Id) -> bool {
        self.0.closes.fetch_add(1, Ordering::Relaxed);
        self.0.callback(CLOSE);
        false
    }

    fn event(&self, event: &Event<'_>) {
        self.0.callback(EVENT);
        event.record(&mut ReceiptVisitor(&self.0));
    }
}

fn controlled(test: impl FnOnce(Arc<Probe>)) {
    let probe = Arc::new(Probe::default());
    tracing::subscriber::with_default(Controlled(probe.clone()), || {
        let span = tracing::info_span!(
            "agent_attempt",
            run_id = "11111111-1111-4111-8111-111111111111",
            attempt_id = "22222222-2222-4222-8222-222222222222"
        );
        let _entered = span.enter();
        test(probe);
    });
}

#[test]
fn subscriber_current_or_clone_failure_cannot_escape_or_strand_registration() {
    for stage in [CURRENT, CLONE] {
        controlled(|probe| {
            let diagnostics = Diagnostics::new();
            probe.bind(&diagnostics);
            probe.panic_mask.store(stage, Ordering::Relaxed);
            let request = catch_unwind(AssertUnwindSafe(|| {
                diagnostics.request("initialize", 1, Instant::now(), Duration::from_secs(120))
            }));
            assert!(
                request.is_ok(),
                "diagnostic registration must remain best effort"
            );
            let request = request
                .unwrap()
                .expect("failed span capture must not lose the RPC guard");
            assert_eq!(diagnostics.snapshot()["active"], 1);
            diagnostics.closed(Termination::new(Initiator::ReaderEof));
            drop(request);
            assert_eq!(diagnostics.snapshot()["active"], 0);
            assert_eq!(diagnostics.snapshot()["emitted"], true);
            assert_eq!(probe.blocked.load(Ordering::Relaxed), 0);
            assert_eq!(probe.panic_mask.load(Ordering::Relaxed), 0);
        });
    }
}

#[test]
fn subscriber_reentrancy_never_observes_the_diagnostic_state_locked() {
    controlled(|probe| {
        let diagnostics = Diagnostics::new();
        probe.bind(&diagnostics);
        let first = diagnostics
            .request("initialize", 1, Instant::now(), Duration::from_secs(120))
            .unwrap();
        let latest = diagnostics
            .request("thread/resume", 2, Instant::now(), Duration::from_secs(120))
            .unwrap();
        diagnostics.closed(Termination::new(Initiator::ReaderEof));
        drop(first);
        drop(latest);
        let snapshot = diagnostics.snapshot();
        assert_eq!(snapshot["active"], 0);
        assert_eq!(snapshot["startup_rpc"]["request_id"], 2);
        assert_eq!(snapshot["startup_rpc"]["disposition"], "cancelled");
        assert_eq!(probe.blocked.load(Ordering::Relaxed), 0);
        assert!(probe.currents.load(Ordering::Relaxed) >= 3);
        assert!(probe.clones.load(Ordering::Relaxed) >= 3);
        assert!(probe.closes.load(Ordering::Relaxed) >= 2);
        assert_eq!(probe.receipts.lock().unwrap().len(), 1);
        diagnostics.closed(Termination::new(Initiator::Stop));
        assert_eq!(probe.receipts.lock().unwrap().len(), 1);
    });
}

#[test]
fn final_guard_receipt_does_not_clone_or_replace_the_captured_span() {
    controlled(|probe| {
        let diagnostics = Diagnostics::new();
        probe.bind(&diagnostics);
        let request = diagnostics
            .request("initialize", 1, Instant::now(), Duration::from_secs(120))
            .unwrap();
        diagnostics.closed(Termination::new(Initiator::ReaderEof));
        let before = probe.clones.load(Ordering::Relaxed);
        probe.panic_mask.store(CLONE, Ordering::Relaxed);
        let dropped = catch_unwind(AssertUnwindSafe(|| drop(request)));
        assert!(
            dropped.is_ok(),
            "final request Drop must not invoke a panicking span clone"
        );
        assert_eq!(probe.clones.load(Ordering::Relaxed), before);
        assert_eq!(diagnostics.snapshot()["active"], 0);
        assert_eq!(diagnostics.snapshot()["emitted"], true);
        assert_eq!(probe.receipts.lock().unwrap().len(), 1);
        probe.panic_mask.store(0, Ordering::Relaxed);
    });
}

#[test]
fn final_correlation_close_failure_does_not_escape() {
    controlled(|probe| {
        let diagnostics = Diagnostics::new();
        probe.bind(&diagnostics);
        probe.panic_mask.store(CLOSE, Ordering::Relaxed);
        let dropped = catch_unwind(AssertUnwindSafe(|| drop(diagnostics)));
        assert!(
            dropped.is_ok(),
            "dropping diagnostic correlation must contain try_close panic"
        );
        assert_eq!(probe.panic_mask.load(Ordering::Relaxed), 0);
    });
}

#[test]
fn replacement_and_rejected_capture_close_failures_keep_the_owned_guard() {
    controlled(|probe| {
        let diagnostics = Diagnostics::new();
        probe.bind(&diagnostics);
        probe.panic_mask.store(CLOSE, Ordering::Relaxed);
        let request = catch_unwind(AssertUnwindSafe(|| {
            diagnostics.request("initialize", 1, Instant::now(), Duration::from_secs(120))
        }));
        assert!(
            request.is_ok(),
            "retiring old correlation must not lose the returned guard"
        );
        let request = request.unwrap().unwrap();
        assert_eq!(probe.panic_mask.load(Ordering::Relaxed), 0);
        diagnostics.closing();
        probe.panic_mask.store(CLOSE, Ordering::Relaxed);
        let rejected = catch_unwind(AssertUnwindSafe(|| {
            diagnostics.request("thread/resume", 2, Instant::now(), Duration::from_secs(120))
        }));
        assert!(
            rejected.is_ok(),
            "unused capture must be destroyed safely outside the State lock"
        );
        assert!(rejected.unwrap().is_none());
        assert_eq!(probe.panic_mask.load(Ordering::Relaxed), 0);
        assert_eq!(diagnostics.snapshot()["active"], 1);
        diagnostics.closed(Termination::new(Initiator::ReaderEof));
        drop(request);
        assert_eq!(diagnostics.snapshot()["active"], 0);
        assert_eq!(diagnostics.snapshot()["startup_rpc"]["request_id"], 1);
        assert_eq!(probe.receipts.lock().unwrap().len(), 1);
        assert_eq!(probe.blocked.load(Ordering::Relaxed), 0);
    });
}

#[test]
fn callback_failures_during_unwind_are_contained() {
    const CHILD: &str = "CAIRN_TEST_DIAGNOSTIC_UNWIND_CHILD";
    const REACHED: &str = "CAIRN_DIAGNOSTIC_UNWIND_CHILD_REACHED";
    if std::env::var_os(CHILD).is_some() {
        controlled(|probe| {
            let diagnostics = Diagnostics::new();
            probe.bind(&diagnostics);
            let request = diagnostics
                .request("initialize", 1, Instant::now(), Duration::from_secs(120))
                .unwrap();
            diagnostics.closed(Termination::new(Initiator::ReaderEof));
            // Multiple callback failures must not turn an already-unwinding
            // Request/Diagnostics destructor into a process abort.
            probe
                .panic_mask
                .store(EVENT | EXIT | CLOSE, Ordering::Relaxed);
            let unwinding = catch_unwind(AssertUnwindSafe(|| {
                let _diagnostics = diagnostics;
                let _request = request;
                panic!("controlled outer unwind");
            }));
            assert!(unwinding.is_err());
            assert_eq!(probe.panic_mask.load(Ordering::Relaxed), 0);
            assert_eq!(probe.blocked.load(Ordering::Relaxed), 0);
        });
        println!("{REACHED}");
        return;
    }

    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "rpc::diagnostics::subscriber_tests::callback_failures_during_unwind_are_contained",
            "--nocapture",
        ])
        .env(CHILD, "1")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let mut output = String::new();
                child
                    .stdout
                    .take()
                    .unwrap()
                    .take(4097)
                    .read_to_string(&mut output)
                    .unwrap();
                assert!(
                    status.success(),
                    "Subscriber failure during unwind must not abort the test child"
                );
                assert!(output.len() <= 4096);
                assert!(
                    output.contains(REACHED),
                    "the exact child regression must execute, not select zero tests"
                );
                break;
            }
            Ok(None) => {}
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                panic!("could not observe Subscriber unwind test child");
            }
        }
        if started.elapsed() >= Duration::from_secs(5) {
            let _ = child.kill();
            let _ = child.wait();
            panic!("Subscriber unwind test child exceeded its bound");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}
