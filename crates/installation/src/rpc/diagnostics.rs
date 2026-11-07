//! Content-free process/transport receipts. No protocol values or error messages
//! enter these types. A request Drop and process cleanup may arrive in either order.
use serde::Serialize;
use std::{
    io,
    mem::ManuallyDrop,
    os::unix::process::ExitStatusExt,
    process::ExitStatus,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tracing::Span;
use uuid::Uuid;

#[cfg(test)]
mod subscriber_tests;

const MAX_RECEIPT_BYTES: usize = 4096;

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(super) enum IoKind {
    NotFound,
    PermissionDenied,
    Interrupted,
    BrokenPipe,
    ConnectionReset,
    UnexpectedEof,
    InvalidData,
    TimedOut,
    Other,
}

impl From<io::ErrorKind> for IoKind {
    fn from(kind: io::ErrorKind) -> Self {
        match kind {
            io::ErrorKind::NotFound => Self::NotFound,
            io::ErrorKind::PermissionDenied => Self::PermissionDenied,
            io::ErrorKind::Interrupted => Self::Interrupted,
            io::ErrorKind::BrokenPipe => Self::BrokenPipe,
            io::ErrorKind::ConnectionReset => Self::ConnectionReset,
            io::ErrorKind::UnexpectedEof => Self::UnexpectedEof,
            io::ErrorKind::InvalidData => Self::InvalidData,
            io::ErrorKind::TimedOut => Self::TimedOut,
            _ => Self::Other,
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(super) enum Initiator {
    Stop,
    RpcClosed,
    ChildExit,
    ChildWaitError,
    ReaderEof,
    ReaderParse,
    ReaderIo,
    ReaderFrameLimit,
    IncomingClosed,
    WriterIo,
    WriterClosed,
    WriterEncode,
}

#[derive(Clone, Copy, Debug, Default, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(super) enum Observation {
    #[default]
    Unavailable,
    BeforeCleanup,
    AfterCleanup,
}

#[derive(Clone, Debug, Default, Serialize)]
pub(super) struct Child {
    pub observation: Observation,
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
    pub wait_error_kind: Option<IoKind>,
}

impl Child {
    /// Keep the first real wait observation, including a wait error. A later
    /// cleanup signal or wait result must not replace it.
    pub fn observe(&mut self, result: &io::Result<ExitStatus>, phase: Observation) {
        if self.observation != Observation::Unavailable {
            return;
        }
        self.observation = phase;
        match result {
            Ok(status) => {
                self.exit_code = status.code();
                self.signal = status.signal();
            }
            Err(error) => self.wait_error_kind = Some(error.kind().into()),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(super) enum ReaderOutcome {
    #[default]
    NotObserved,
    Eof,
    Parse,
    Io,
    FrameLimit,
    IncomingClosed,
}

#[derive(Clone, Debug, Default, Serialize)]
pub(super) struct Reader {
    pub outcome: ReaderOutcome,
    pub io_kind: Option<IoKind>,
}

#[derive(Clone, Copy, Debug, Default, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(super) enum WriterOutcome {
    #[default]
    NotObserved,
    Io,
    Closed,
    Encode,
}

#[derive(Clone, Debug, Default, Serialize)]
pub(super) struct Writer {
    pub outcome: WriterOutcome,
    pub io_kind: Option<IoKind>,
}

impl Writer {
    pub fn initiator(&self) -> Initiator {
        match self.outcome {
            WriterOutcome::Io => Initiator::WriterIo,
            WriterOutcome::Encode => Initiator::WriterEncode,
            WriterOutcome::Closed | WriterOutcome::NotObserved => Initiator::WriterClosed,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(super) enum CleanupAction {
    #[default]
    None,
    Term,
    Kill,
}

#[derive(Clone, Debug, Default, Serialize)]
pub(super) struct Cleanup {
    pub action: CleanupAction,
}

#[derive(Clone, Copy, Debug, Default, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Availability {
    #[default]
    Partial,
    Eof,
    Io,
}

#[derive(Clone, Debug, Default, Serialize)]
pub(super) struct Stderr {
    pub availability: Availability,
    pub bytes: u64,
    pub io_kind: Option<IoKind>,
}

#[derive(Clone, Debug, Serialize)]
pub(super) struct Termination {
    pub initiator: Initiator,
    pub child: Child,
    pub reader: Reader,
    pub writer: Writer,
    pub cleanup: Cleanup,
    pub stderr: Stderr,
}

impl Termination {
    pub fn new(initiator: Initiator) -> Self {
        Self {
            initiator,
            child: Child::default(),
            reader: Reader::default(),
            writer: Writer::default(),
            cleanup: Cleanup::default(),
            stderr: Stderr::default(),
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize)]
enum StartupMethod {
    #[serde(rename = "initialize")]
    Initialize,
    #[serde(rename = "account/login/start")]
    Login,
    #[serde(rename = "thread/resume")]
    Resume,
    #[serde(rename = "thread/read")]
    Read,
    #[serde(rename = "thread/turns/list")]
    Turns,
    #[serde(rename = "thread/items/list")]
    Items,
}

impl StartupMethod {
    fn parse(method: &str) -> Option<Self> {
        match method {
            "initialize" => Some(Self::Initialize),
            "account/login/start" => Some(Self::Login),
            "thread/resume" => Some(Self::Resume),
            "thread/read" => Some(Self::Read),
            "thread/turns/list" => Some(Self::Turns),
            "thread/items/list" => Some(Self::Items),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
enum Disposition {
    Succeeded,
    Failed,
    Cancelled,
}

#[derive(Clone, Debug, Serialize)]
struct StartupRpc {
    method: StartupMethod,
    request_id: u64,
    elapsed_ms: u64,
    timeout_ms: u64,
    disposition: Disposition,
}

#[derive(Serialize)]
struct Receipt {
    schema_version: u8,
    session_id: Uuid,
    #[serde(flatten)]
    termination: Termination,
    startup_rpc: Option<StartupRpc>,
}

/// Subscriber callbacks, including destruction of a captured Span, are fallible.
/// A second panic from a custom panic payload's destructor must not escape Drop.
fn best_effort<T>(operation: impl FnOnce() -> T) -> Option<T> {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(operation)) {
        Ok(value) => Some(value),
        Err(payload) => {
            if let Err(nested) =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drop(payload)))
            {
                std::mem::forget(nested);
            }
            None
        }
    }
}

/// Arc cloning is inert bookkeeping; Span cloning is a Subscriber callback.
/// Own exactly one Span and contain its destruction wherever the last Arc ends.
struct Correlation {
    span: Option<Span>,
}

impl Correlation {
    fn current() -> Arc<Self> {
        let span = best_effort(Span::current).unwrap_or_else(Span::none);
        Arc::new(Self { span: Some(span) })
    }

    fn emit(&self, termination: &str) {
        let Some(span) = &self.span else {
            return;
        };
        if span.is_none() {
            let _ = best_effort(|| {
                tracing::info!(
                    target: "cairn_performance",
                    operation = "codex_session",
                    event = "terminated",
                    run_id = "unknown",
                    attempt_id = "unknown",
                    termination = %termination,
                );
            });
            return;
        }

        // Do not use in_scope: an event panic followed by an exit panic in its
        // ordinary Entered destructor could abort before an outer catch runs.
        let Some(mut entered) = best_effort(|| ManuallyDrop::new(span.enter())) else {
            return;
        };
        let _ = best_effort(|| {
            tracing::info!(
                target: "cairn_performance",
                operation = "codex_session",
                event = "terminated",
                termination = %termination,
            );
        });
        let _ = best_effort(|| {
            // SAFETY: entered was initialized above and is explicitly dropped
            // exactly once. Neither success nor panic accesses it afterward;
            // ManuallyDrop prevents a second Subscriber::exit on scope exit.
            unsafe {
                ManuallyDrop::drop(&mut entered);
            }
        });
    }
}

impl Drop for Correlation {
    fn drop(&mut self) {
        if let Some(span) = self.span.take() {
            let _ = best_effort(|| drop(span));
        }
    }
}

struct State {
    session_id: Uuid,
    correlation: Arc<Correlation>,
    startup_rpc: Option<StartupRpc>,
    active: usize,
    termination: Option<Termination>,
    closing: bool,
    emitted: bool,
}

impl State {
    fn receipt(&mut self) -> Option<(Arc<Correlation>, Receipt)> {
        if self.emitted || self.active != 0 || self.termination.is_none() {
            return None;
        }
        // This is only an Arc increment, not Span::clone/Subscriber::clone_span.
        let correlation = Arc::clone(&self.correlation);
        let termination = self.termination.take()?;
        self.emitted = true;
        Some((
            correlation,
            Receipt {
                schema_version: 1,
                session_id: self.session_id,
                termination,
                startup_rpc: self.startup_rpc.clone(),
            },
        ))
    }
}

#[derive(Clone)]
pub(super) struct Diagnostics(Arc<Mutex<State>>);

impl Diagnostics {
    pub fn new() -> Self {
        Self(Arc::new(Mutex::new(State {
            session_id: Uuid::new_v4(),
            correlation: Correlation::current(),
            startup_rpc: None,
            active: 0,
            termination: None,
            closing: false,
            emitted: false,
        })))
    }

    /// Track only fixed startup methods, never arbitrary protocol method strings.
    /// The most recently started startup RPC supplies the bounded single receipt.
    pub fn request(
        &self,
        method: &str,
        id: u64,
        started: Instant,
        timeout: Duration,
    ) -> Option<Request> {
        let method = StartupMethod::parse(method)?;
        // Capture before touching active or taking the State mutex. Capture
        // failure keeps an unknown correlation while still returning an owned
        // request guard whose counter will settle normally.
        let mut incoming = Some(Correlation::current());
        let (request, retired) = self.update(|state| {
            if state.closing || state.emitted {
                return (None, None);
            }
            let Some(active) = state.active.checked_add(1) else {
                return (None, None);
            };
            let Some(correlation) = incoming.take() else {
                return (None, None);
            };
            let retired = std::mem::replace(&mut state.correlation, correlation);
            state.active = active;
            state.startup_rpc = Some(StartupRpc {
                method,
                request_id: id,
                elapsed_ms: 0,
                timeout_ms: millis(timeout),
                disposition: Disposition::Cancelled,
            });
            let request = Request {
                diagnostics: self.clone(),
                id,
                started,
                finished: false,
            };
            (Some(request), Some(retired))
        });
        // This may drop the last Arc and call Subscriber::try_close. Unused
        // incoming correlation on a closing State also drops outside the lock.
        drop(retired);
        request
    }

    pub fn closed(&self, termination: Termination) {
        self.update(|state| {
            state.closing = true;
            if !state.emitted && state.termination.is_none() {
                state.termination = Some(termination);
            }
        });
    }

    /// Freeze request selection before waking pending callers or beginning
    /// cleanup. A later request on the closed Rpc cannot steal its context.
    pub fn closing(&self) {
        self.update(|state| state.closing = true);
    }

    #[cfg(test)]
    pub fn snapshot(&self) -> serde_json::Value {
        let state = self.0.lock().unwrap();
        serde_json::json!({
            "active": state.active,
            "emitted": state.emitted,
            "startup_rpc": state.startup_rpc,
        })
    }

    fn update<T>(&self, update: impl FnOnce(&mut State) -> T) -> T {
        // No subscriber calls/destruction, awaits, serialization or payload
        // processing under this lock. Replaced correlations leave in the result.
        // Request Drop must also work while another future unwinds.
        let (result, receipt) = {
            let mut state = self
                .0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let result = update(&mut state);
            (result, state.receipt())
        };
        if let Some((correlation, receipt)) = receipt {
            Self::emit(&correlation, &receipt);
        }
        result
    }

    fn emit(correlation: &Correlation, receipt: &Receipt) {
        let _ = best_effort(|| {
            let Ok(termination) = serde_json::to_string(receipt) else {
                return;
            };
            // All strings are closed enums or a generated UUID; retain the
            // defensive bound if a future schema edit breaks that invariant.
            if termination.len() <= MAX_RECEIPT_BYTES {
                correlation.emit(&termination);
            }
        });
    }
}

fn millis(duration: Duration) -> u64 {
    duration.as_millis().min(u64::MAX as u128) as u64
}

/// Dropping an initialize/resume request is a lifecycle outcome, even when
/// incoming-channel closure wins before Rpc::request can log completion.
pub(super) struct Request {
    diagnostics: Diagnostics,
    id: u64,
    started: Instant,
    finished: bool,
}

impl Request {
    pub fn finish(mut self, succeeded: bool) {
        let _ = best_effort(|| {
            self.end(if succeeded {
                Disposition::Succeeded
            } else {
                Disposition::Failed
            });
        });
    }

    fn end(&mut self, disposition: Disposition) {
        self.finished = true;
        self.diagnostics.update(|state| {
            state.active -= 1;
            if let Some(rpc) = &mut state.startup_rpc
                && rpc.request_id == self.id
            {
                rpc.elapsed_ms = millis(self.started.elapsed());
                rpc.disposition = disposition;
            }
        });
    }
}

impl Drop for Request {
    fn drop(&mut self) {
        if !self.finished {
            let _ = best_effort(|| self.end(Disposition::Cancelled));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn child_wait_observation_never_becomes_an_induced_cleanup_status() {
        let mut child = Child::default();
        child.observe(
            &Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "PRIVATE_WAIT_PATH",
            )),
            Observation::BeforeCleanup,
        );
        child.observe(
            &Ok(ExitStatus::from_raw(43 << 8)),
            Observation::AfterCleanup,
        );
        let encoded = serde_json::to_string(&child).unwrap();
        assert_eq!(child.observation, Observation::BeforeCleanup);
        assert_eq!(child.wait_error_kind, Some(IoKind::PermissionDenied));
        assert!(child.exit_code.is_none());
        assert!(child.signal.is_none());
        assert!(!encoded.contains("PRIVATE_WAIT_PATH"));

        let mut signalled = Child::default();
        signalled.observe(
            &Ok(ExitStatus::from_raw(libc::SIGKILL)),
            Observation::BeforeCleanup,
        );
        signalled.observe(
            &Ok(ExitStatus::from_raw(43 << 8)),
            Observation::AfterCleanup,
        );
        assert_eq!(signalled.signal, Some(libc::SIGKILL));
        assert!(signalled.exit_code.is_none());
    }

    #[test]
    fn closure_waits_for_drop_or_completion_once_and_keeps_the_newest_request() {
        let diagnostics = Diagnostics::new();
        let older = diagnostics
            .request("initialize", 1, Instant::now(), Duration::from_secs(120))
            .unwrap();
        let newest = diagnostics
            .request("thread/resume", 2, Instant::now(), Duration::from_secs(120))
            .unwrap();
        diagnostics.closing();
        assert!(
            diagnostics
                .request("initialize", 3, Instant::now(), Duration::from_secs(120))
                .is_none()
        );
        diagnostics.closed(Termination::new(Initiator::ReaderEof));
        assert_eq!(diagnostics.snapshot()["emitted"], false);
        drop(newest);
        assert_eq!(diagnostics.snapshot()["emitted"], false);
        older.finish(false);
        let snapshot = diagnostics.snapshot();
        assert_eq!(snapshot["emitted"], true);
        assert_eq!(snapshot["active"], 0);
        assert_eq!(snapshot["startup_rpc"]["request_id"], 2);
        assert_eq!(snapshot["startup_rpc"]["disposition"], "cancelled");
        diagnostics.closed(Termination::new(Initiator::Stop));
        assert!(diagnostics.0.lock().unwrap().termination.is_none());
        assert!(
            diagnostics
                .request("initialize", 3, Instant::now(), Duration::from_secs(120))
                .is_none()
        );
    }

    #[test]
    fn successful_and_nonstartup_operations_keep_honest_lifecycle_values() {
        let diagnostics = Diagnostics::new();
        assert!(
            diagnostics
                .request("PRIVATE_METHOD", 1, Instant::now(), Duration::from_secs(20))
                .is_none()
        );
        let initialize = diagnostics
            .request("initialize", 2, Instant::now(), Duration::from_secs(120))
            .unwrap();
        initialize.finish(true);
        assert_eq!(
            diagnostics.snapshot()["startup_rpc"]["disposition"],
            "succeeded"
        );
        assert_eq!(diagnostics.snapshot()["startup_rpc"]["request_id"], 2);
        diagnostics.closed(Termination::new(Initiator::Stop));
        assert_eq!(diagnostics.snapshot()["emitted"], true);
    }

    #[test]
    fn closed_schema_has_a_large_margin_under_the_encoded_byte_bound() {
        let mut termination = Termination::new(Initiator::ReaderFrameLimit);
        termination.child.observation = Observation::AfterCleanup;
        termination.child.exit_code = Some(i32::MIN);
        termination.child.signal = Some(i32::MAX);
        termination.child.wait_error_kind = Some(IoKind::PermissionDenied);
        termination.reader.outcome = ReaderOutcome::IncomingClosed;
        termination.reader.io_kind = Some(IoKind::PermissionDenied);
        termination.writer.outcome = WriterOutcome::NotObserved;
        termination.writer.io_kind = Some(IoKind::PermissionDenied);
        termination.stderr.bytes = u64::MAX;
        termination.stderr.io_kind = Some(IoKind::PermissionDenied);
        let receipt = Receipt {
            schema_version: 1,
            session_id: Uuid::new_v4(),
            termination,
            startup_rpc: Some(StartupRpc {
                method: StartupMethod::Login,
                request_id: u64::MAX,
                elapsed_ms: u64::MAX,
                timeout_ms: u64::MAX,
                disposition: Disposition::Cancelled,
            }),
        };
        let encoded = serde_json::to_string(&receipt).unwrap();
        assert!(encoded.len() < MAX_RECEIPT_BYTES / 2);
        let value: serde_json::Value = serde_json::from_str(&encoded).unwrap();
        assert_eq!(value.as_object().unwrap().len(), 9);
        assert_eq!(value["child"].as_object().unwrap().len(), 4);
        assert_eq!(value["startup_rpc"].as_object().unwrap().len(), 5);
    }
}
