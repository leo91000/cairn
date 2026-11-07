mod diagnostics;
pub mod jsonrpc;

use crate::{
    config::Config,
    error::{Error, Result},
    process::{codex_environment, command},
};
use diagnostics::{
    Availability, CleanupAction, Diagnostics, Initiator, Observation, Reader, ReaderOutcome,
    Stderr, Termination, Writer, WriterOutcome,
};
use jsonrpc::{ErrorObject, Frame, METHOD_NOT_FOUND, Message};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    convert::Infallible,
    io,
    path::Path,
    process::Stdio,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader},
    process::Child,
    sync::{Mutex, mpsc, oneshot},
};
use tokio_util::sync::CancellationToken;

type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value>>>>>;

/// MCP frames are bounded even if a server never emits a newline. Codex frames
/// are not: they can contain large tool results or conversation history.
const MCP_FRAME_LIMIT: usize = 2_000_000;

#[derive(Debug)]
pub struct Incoming {
    pub method: String,
    pub params: Value,
    pub id: Option<Value>,
}

#[derive(Clone)]
pub struct Rpc {
    jsonrpc: bool,
    outgoing: mpsc::Sender<Frame>,
    pending: Pending,
    sequence: Arc<AtomicU64>,
    closed: CancellationToken,
    failure: Arc<Mutex<Option<(u16, String)>>>,
    diagnostics: Option<Diagnostics>,
}

pub struct Session {
    pub auth: Option<crate::accounts::codex::Client>,
    pub rpc: Rpc,
    pub incoming: mpsc::Receiver<Incoming>,
    stop: CancellationToken,
    finished: Option<oneshot::Receiver<()>>,
    telemetry: Option<crate::performance::native::Collector>,
}

impl Drop for Session {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}

async fn write_frames(
    stdin: &mut (impl AsyncWrite + Unpin),
    output: &mut mpsc::Receiver<Frame>,
) -> Writer {
    while let Some(frame) = output.recv().await {
        let Ok(mut bytes) = serde_json::to_vec(&frame) else {
            return Writer {
                outcome: WriterOutcome::Encode,
                io_kind: None,
            };
        };
        bytes.push(b'\n');
        if let Err(error) = stdin.write_all(&bytes).await {
            return Writer {
                outcome: WriterOutcome::Io,
                io_kind: Some(error.kind().into()),
            };
        }
    }
    Writer {
        outcome: WriterOutcome::Closed,
        io_kind: None,
    }
}

#[derive(Debug)]
enum ReadFailure {
    Eof,
    Parse,
    Io(io::Error),
    FrameLimit,
    IncomingClosed,
}

impl ReadFailure {
    fn observation(&self) -> (Initiator, Reader) {
        let (initiator, outcome, io_kind) = match self {
            Self::Eof => (Initiator::ReaderEof, ReaderOutcome::Eof, None),
            Self::Parse => (Initiator::ReaderParse, ReaderOutcome::Parse, None),
            Self::Io(error) => (
                Initiator::ReaderIo,
                ReaderOutcome::Io,
                Some(error.kind().into()),
            ),
            Self::FrameLimit => (Initiator::ReaderFrameLimit, ReaderOutcome::FrameLimit, None),
            Self::IncomingClosed => (
                Initiator::IncomingClosed,
                ReaderOutcome::IncomingClosed,
                None,
            ),
        };
        (initiator, Reader { outcome, io_kind })
    }

    fn into_error(self) -> Error {
        match self {
            Self::Io(error) => error.into(),
            Self::FrameLimit => Error::bad_gateway("MCP response exceeded the 2000000-byte limit."),
            Self::Eof | Self::Parse | Self::IncomingClosed => unavailable(),
        }
    }
}

/// Reads one newline-terminated frame into `bytes`.
async fn read_frame(
    reader: &mut BufReader<impl AsyncRead + Unpin>,
    bytes: &mut Vec<u8>,
    jsonrpc: bool,
) -> std::result::Result<(), ReadFailure> {
    bytes.clear();
    loop {
        let buffer = reader.fill_buf().await.map_err(ReadFailure::Io)?;
        if buffer.is_empty() {
            return Err(ReadFailure::Eof);
        }
        let end = buffer.iter().position(|b| *b == b'\n').map(|i| i + 1);
        let length = end.unwrap_or(buffer.len());
        if jsonrpc && bytes.len() + length > MCP_FRAME_LIMIT {
            return Err(ReadFailure::FrameLimit);
        }
        bytes.extend_from_slice(&buffer[..length]);
        reader.consume(length);
        if end.is_some() {
            return Ok(());
        }
    }
}

fn response_error(jsonrpc: bool, error: &Value) -> Error {
    let not_found = error["code"] == METHOD_NOT_FOUND;
    let status = if jsonrpc && not_found { 501 } else { 502 };
    let message = if not_found {
        "Update Codex to support this account operation."
    } else {
        "Codex could not complete this operation. Reconnect it and try again."
    };
    Error::new(status, message)
}

/// Routes peer requests to `incoming` and responses to their pending callers.
async fn read_frames(
    stdout: impl AsyncRead + Unpin,
    jsonrpc: bool,
    incoming: mpsc::Sender<Incoming>,
    pending: Pending,
) -> ReadFailure {
    let mut reader = BufReader::new(stdout);
    let mut bytes = Vec::new();
    loop {
        if let Err(failure) = read_frame(&mut reader, &mut bytes, jsonrpc).await {
            return failure;
        }
        let Ok(message) = serde_json::from_slice::<Value>(&bytes) else {
            return ReadFailure::Parse;
        };
        if let Some(method) = message["method"].as_str() {
            let request = Incoming {
                method: method.into(),
                params: message["params"].clone(),
                id: message.get("id").cloned(),
            };
            if incoming.send(request).await.is_err() {
                return ReadFailure::IncomingClosed;
            }
            continue;
        }
        let Some(id) = message["id"].as_u64() else {
            continue;
        };
        let Some(reply) = pending.lock().await.remove(&id) else {
            continue;
        };
        let result = match message.get("error") {
            Some(error) => Err(response_error(jsonrpc, error)),
            None => Ok(message["result"].clone()),
        };
        // The caller may have timed out and dropped its receiver.
        let _ = reply.send(result);
    }
}

/// Count what was actually read; retain no stderr content and never let stderr
/// EOF select the transport's initiating failure. Cancellation leaves Partial.
async fn drain_stderr(
    stderr: &mut (impl AsyncRead + Unpin),
    observation: &mut Stderr,
) -> Infallible {
    let mut buffer = [0; 8192];
    loop {
        match stderr.read(&mut buffer).await {
            Ok(0) => {
                observation.availability = Availability::Eof;
                break;
            }
            Ok(bytes) => observation.bytes = observation.bytes.saturating_add(bytes as u64),
            Err(error) => {
                observation.availability = Availability::Io;
                observation.io_kind = Some(error.kind().into());
                break;
            }
        }
    }
    std::future::pending().await
}

/// Keep the existing SIGTERM/two-second/kill ownership sequence. Observe status
/// before cleanup when available, and never overwrite that first observation.
async fn terminate(child: &mut Child, observation: &mut Termination) {
    if observation.child.observation == Observation::Unavailable {
        match child.try_wait() {
            Ok(Some(status)) => observation
                .child
                .observe(&Ok(status), Observation::BeforeCleanup),
            Err(error) => observation
                .child
                .observe(&Err(error), Observation::BeforeCleanup),
            Ok(None) => {}
        }
    }
    if let Some(pid) = child.id() {
        observation.cleanup.action = CleanupAction::Term;
        unsafe {
            libc::kill(pid as i32, libc::SIGTERM);
        }
    }
    match tokio::time::timeout(Duration::from_secs(2), child.wait()).await {
        Ok(result) => observation
            .child
            .observe(&result, Observation::AfterCleanup),
        Err(_) => {
            observation.cleanup.action = CleanupAction::Kill;
            if let Err(error) = child.kill().await {
                let io_kind = diagnostics::IoKind::from(error.kind());
                tracing::warn!(?io_kind, "could not kill child process");
            }
            match child.try_wait() {
                Ok(Some(status)) => observation
                    .child
                    .observe(&Ok(status), Observation::AfterCleanup),
                Err(error) => observation
                    .child
                    .observe(&Err(error), Observation::AfterCleanup),
                Ok(None) => {}
            }
        }
    }
}

impl Session {
    pub async fn codex(
        config: &Config,
        home: &Path,
        args: &[String],
        cwd: Option<&Path>,
    ) -> Result<Self> {
        Self::codex_until(config, home, args, cwd, &CancellationToken::new()).await
    }

    pub(crate) async fn codex_until(
        config: &Config,
        home: &Path,
        args: &[String],
        cwd: Option<&Path>,
        stop: &CancellationToken,
    ) -> Result<Self> {
        let mut args = args.to_vec();
        let guest_endpoint = crate::performance::native::guest_endpoint();
        let telemetry = if guest_endpoint.is_none() {
            crate::performance::native::Collector::start().await.ok()
        } else {
            None
        };
        if let Some(endpoint) = guest_endpoint.as_deref().or_else(|| {
            telemetry
                .as_ref()
                .map(crate::performance::native::Collector::endpoint)
        }) {
            args.extend(crate::performance::native::arguments(endpoint));
        }
        args.extend(
            [
                "-c",
                "cli_auth_credentials_store=\"file\"",
                "-c",
                "forced_login_method=\"chatgpt\"",
                // Cairn supplies skills and MCP servers itself. Codex's unused native
                // catalog clones ~120 MiB per fresh home and stalls the final disk sync.
                "-c",
                "features.plugins=false",
                "app-server",
                "--listen",
                "stdio://",
            ]
            .map(str::to_owned),
        );
        let mut command = command(
            &config.codex_bin,
            &args,
            &codex_environment(config, home),
            cwd,
        );
        command.stdin(Stdio::piped());
        let mut session = Self::spawn(command).await?;
        session.telemetry = telemetry;
        let initialized = tokio::select! {
            result = session.initialize_codex() => result,
            () = stop.cancelled() => Err(Error::unavailable("Codex initialization stopped.")),
        };
        if let Err(error) = initialized {
            // The caller cannot close a session that failed to initialize. Reap its
            // process here before relinquishing ownership (and its account lease).
            session.close().await;
            return Err(error);
        }
        Ok(session)
    }

    async fn initialize_codex(&mut self) -> Result<()> {
        // Initialization does not require interactive requests. Reject them while negotiating.
        let rpc = self.rpc.clone();
        let params = json!({
            "clientInfo": {
                "name": "cairn_installation",
                "version": env!("CARGO_PKG_VERSION"),
            },
            "capabilities": { "experimentalApi": true },
        });
        let initialize = rpc.request("initialize", params);
        tokio::pin!(initialize);
        loop {
            tokio::select! {
                result = &mut initialize => {
                    result?;
                    break;
                }
                incoming = self.incoming.recv() => {
                    let Some(incoming) = incoming else {
                        return Err(unavailable());
                    };
                    if let Some(id) = incoming.id {
                        rpc.reject(id).await?;
                    }
                }
            }
        }
        rpc.notify("initialized", json!({})).await
    }

    pub async fn spawn(command: tokio::process::Command) -> Result<Self> {
        Self::spawn_with_protocol(command, false).await
    }

    pub async fn spawn_with_protocol(
        mut command: tokio::process::Command,
        jsonrpc: bool,
    ) -> Result<Self> {
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = command.spawn().map_err(|_| {
            Error::unavailable("Unable to start the agent process. Check its installation and executable permissions.")
        })?;
        let mut stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let mut stderr = child.stderr.take().unwrap();
        let (outgoing, mut output) = mpsc::channel::<Frame>(64);
        let (incoming, receiver) = mpsc::channel(256);
        let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
        let stop = CancellationToken::new();
        let closed = CancellationToken::new();
        let failure = Arc::new(Mutex::new(None));
        let (done, finished) = oneshot::channel();
        let diagnostics = (!jsonrpc).then(Diagnostics::new);
        let rpc = Rpc {
            jsonrpc,
            outgoing,
            pending: pending.clone(),
            sequence: Arc::new(AtomicU64::new(0)),
            closed: closed.clone(),
            failure: failure.clone(),
            diagnostics: diagnostics.clone(),
        };
        let stopping = stop.clone();
        tokio::spawn(async move {
            let mut stderr_observation = Stderr::default();
            // Finish the select before probing/waiting/killing the child. Its
            // futures must relinquish their pipe/child borrows first.
            let (mut observation, read_error) = {
                let writer = write_frames(&mut stdin, &mut output);
                let reader = read_frames(stdout, jsonrpc, incoming, pending.clone());
                let drain = drain_stderr(&mut stderr, &mut stderr_observation);
                tokio::select! {
                    () = stopping.cancelled() => (Termination::new(Initiator::Stop), None),
                    () = closed.cancelled() => (Termination::new(Initiator::RpcClosed), None),
                    result = child.wait() => {
                        let initiator = if result.is_ok() {
                            Initiator::ChildExit
                        } else {
                            Initiator::ChildWaitError
                        };
                        let mut observation = Termination::new(initiator);
                        observation.child.observe(&result, Observation::BeforeCleanup);
                        (observation, None)
                    }
                    writer = writer => {
                        let mut observation = Termination::new(writer.initiator());
                        observation.writer = writer;
                        (observation, None)
                    }
                    result = reader => {
                        let (initiator, reader) = result.observation();
                        let error = result.into_error();
                        let mut observation = Termination::new(initiator);
                        observation.reader = reader;
                        (observation, Some(error))
                    }
                    never = drain => match never {},
                }
            };
            observation.stderr = stderr_observation;
            if let Some(diagnostics) = &diagnostics {
                diagnostics.closing();
            }
            if let Some(error) = read_error {
                *failure.lock().await = Some((error.status, error.message));
            }
            closed.cancel();
            for (_, reply) in pending.lock().await.drain() {
                let _ = reply.send(Err(unavailable()));
            }
            terminate(&mut child, &mut observation).await;
            if let Some(diagnostics) = diagnostics {
                diagnostics.closed(observation);
            }
            let _ = done.send(());
        });
        Ok(Self {
            auth: None,
            rpc,
            incoming: receiver,
            stop,
            finished: Some(finished),
            telemetry: None,
        })
    }

    pub async fn handle_auth(&mut self, incoming: &Incoming) -> Result<bool> {
        if incoming.method == "account/chatgptAuthTokens/refresh"
            && incoming.id.is_some()
            && let Some(auth) = &mut self.auth
        {
            auth.refresh(&self.rpc, incoming).await?;
            return Ok(true);
        }
        Ok(false)
    }

    pub async fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        let rpc = self.rpc.clone();
        let request = rpc.request(method, params);
        tokio::pin!(request);
        loop {
            tokio::select! {
                result = &mut request => return result,
                incoming = self.incoming.recv() => {
                    let Some(incoming) = incoming else {
                        return Err(unavailable());
                    };
                    if !self.handle_auth(&incoming).await?
                        && let Some(id) = incoming.id
                    {
                        rpc.reject(id).await?;
                    }
                }
            }
        }
    }

    pub async fn close(mut self) {
        self.shutdown().await;
    }

    pub(crate) async fn shutdown(&mut self) {
        self.stop.cancel();
        if let Some(finished) = self.finished.take() {
            let _ = finished.await;
        }
    }
}

impl Rpc {
    pub async fn failure(&self) -> Error {
        match &*self.failure.lock().await {
            Some((status, message)) => Error::new(*status, message),
            None => unavailable(),
        }
    }

    async fn send(&self, message: Message) -> Result<()> {
        let frame = Frame::new(message, self.jsonrpc);
        tokio::select! {
            () = self.closed.cancelled() => Err(self.failure().await),
            result = self.outgoing.send(frame) => result.map_err(|_| unavailable()),
        }
    }

    pub async fn request(&self, method: &str, params: Value) -> Result<Value> {
        // Initialization first loads the executable and libraries from disk;
        // resuming a retained native session then loads its on-disk context.
        // Cold reads can outlast an ordinary account or tool request after a move.
        // Keep this startup bounded; closing the session still wakes it at once.
        let timeout = if !self.jsonrpc && matches!(method, "initialize" | "thread/resume") {
            Duration::from_secs(120)
        } else {
            Duration::from_secs(20)
        };
        let started = Instant::now();
        let id = self.sequence.fetch_add(1, Ordering::Relaxed) + 1;
        let diagnostic_request = self
            .diagnostics
            .as_ref()
            .and_then(|diagnostics| diagnostics.request(method, id, started, timeout));
        let operation = if self.jsonrpc { "mcp_rpc" } else { "codex_rpc" };
        tracing::info!(
            target: "cairn_performance",
            operation,
            method,
            request_id = id,
            event = "started",
            timeout_ms = timeout.as_millis() as u64,
        );
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, tx);
        let mut queue_ms = None;
        let result = async {
            let method = method.to_owned();
            self.send(Message::Request { id, method, params }).await?;
            queue_ms = Some(started.elapsed().as_millis() as u64);
            tokio::select! {
                () = self.closed.cancelled() => Err(self.failure().await),
                result = rx => match result {
                    Ok(Ok(value)) => Ok(value),
                    _ if self.closed.is_cancelled() => Err(self.failure().await),
                    Ok(Err(error)) => Err(error),
                    Err(_) => Err(unavailable()),
                },
            }
        };
        let result = tokio::time::timeout(timeout, result)
            .await
            .unwrap_or_else(|_| {
                Err(Error::gateway_timeout(format!(
                    "Codex {method} request timed out."
                )))
            });
        self.pending.lock().await.remove(&id);
        if let Some(request) = diagnostic_request {
            request.finish(result.is_ok());
        }
        tracing::info!(
            target: "cairn_performance",
            operation,
            method,
            request_id = id,
            event = "completed",
            elapsed_ms = started.elapsed().as_millis() as u64,
            queue_ms,
            timeout_ms = timeout.as_millis() as u64,
            succeeded = result.is_ok(),
        );
        result
    }

    pub async fn notify(&self, method: &str, params: Value) -> Result<()> {
        let method = method.to_owned();
        self.send(Message::Notification { method, params }).await
    }

    pub async fn reply(&self, id: Value, result: Value) -> Result<()> {
        self.send(Message::Result { id, result }).await
    }

    pub async fn reject(&self, id: Value) -> Result<()> {
        self.reject_with_message(id, "Interactive tool requests are unavailable.")
            .await
    }

    pub async fn reject_with_message(&self, id: Value, message: &str) -> Result<()> {
        let error = ErrorObject {
            code: METHOD_NOT_FOUND,
            message: message.into(),
            data: None,
        };
        self.send(Message::Error { id, error }).await
    }
}

fn unavailable() -> Error {
    Error::unavailable(
        "Codex disconnected before finishing the operation. Try again or resume the conversation.",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        pin::Pin,
        task::{Context, Poll},
    };
    use tokio::io::ReadBuf;

    fn rpc(jsonrpc: bool) -> (Rpc, mpsc::Receiver<Frame>) {
        let (outgoing, output) = mpsc::channel(32);
        (
            Rpc {
                jsonrpc,
                outgoing,
                pending: Arc::default(),
                sequence: Arc::new(AtomicU64::new(0)),
                closed: CancellationToken::new(),
                failure: Arc::default(),
                diagnostics: (!jsonrpc).then(Diagnostics::new),
            },
            output,
        )
    }

    /// A real AsyncRead/AsyncWrite fault boundary, rather than constructing the
    /// generic adapter Error that has already lost the IO-kind discriminator.
    struct FaultIo;

    impl AsyncRead for FaultIo {
        fn poll_read(
            self: Pin<&mut Self>,
            _: &mut Context<'_>,
            _: &mut ReadBuf<'_>,
        ) -> Poll<io::Result<()>> {
            Poll::Ready(Err(io::Error::new(
                io::ErrorKind::ConnectionReset,
                "PRIVATE_IO_PATH",
            )))
        }
    }

    impl AsyncWrite for FaultIo {
        fn poll_write(
            self: Pin<&mut Self>,
            _: &mut Context<'_>,
            _: &[u8],
        ) -> Poll<io::Result<usize>> {
            Poll::Ready(Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "PRIVATE_IO_PATH",
            )))
        }

        fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }

        fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }

    #[tokio::test]
    async fn dropped_real_initialize_future_retains_a_receipt_in_both_closure_orders() {
        for close_first in [false, true] {
            let (rpc, mut output) = rpc(false);
            let diagnostics = rpc.diagnostics.as_ref().unwrap().clone();
            {
                let request = rpc.request("initialize", json!({ "private": "PRIVATE_PROMPT" }));
                tokio::pin!(request);
                tokio::select! {
                    frame = output.recv() => assert!(matches!(frame.unwrap().message, Message::Request { id: 1, .. })),
                    _ = &mut request => panic!("initialize must be pending before cancellation"),
                }
                if close_first {
                    diagnostics.closed(Termination::new(Initiator::ReaderEof));
                    assert_eq!(diagnostics.snapshot()["emitted"], false);
                }
                // Leaving this scope actually drops Rpc::request's owned future.
            }
            if !close_first {
                assert_eq!(diagnostics.snapshot()["emitted"], false);
                diagnostics.closed(Termination::new(Initiator::ReaderEof));
            }
            let snapshot = diagnostics.snapshot();
            assert_eq!(snapshot["emitted"], true);
            assert_eq!(snapshot["active"], 0);
            assert_eq!(snapshot["startup_rpc"]["method"], "initialize");
            assert_eq!(snapshot["startup_rpc"]["request_id"], 1);
            assert_eq!(snapshot["startup_rpc"]["timeout_ms"], 120_000);
            assert_eq!(snapshot["startup_rpc"]["disposition"], "cancelled");
            assert!(!snapshot.to_string().contains("PRIVATE_PROMPT"));
        }
    }

    #[tokio::test]
    async fn actual_reader_boundaries_preserve_eof_parse_io_and_incoming_closure() {
        let (incoming, receiver) = mpsc::channel(1);
        let pending = Pending::default();
        let failure = read_frames(&b""[..], false, incoming.clone(), pending.clone()).await;
        assert_eq!(failure.observation().1.outcome, ReaderOutcome::Eof);
        let failure =
            read_frames(&b"{invalid\n"[..], false, incoming.clone(), pending.clone()).await;
        assert_eq!(failure.observation().1.outcome, ReaderOutcome::Parse);
        let failure = read_frames(FaultIo, false, incoming.clone(), pending.clone()).await;
        assert_eq!(
            failure.observation().1.io_kind,
            Some(diagnostics::IoKind::ConnectionReset)
        );
        assert!(
            !serde_json::to_string(&failure.observation().1)
                .unwrap()
                .contains("PRIVATE_IO_PATH")
        );
        let oversized = vec![b'x'; MCP_FRAME_LIMIT + 1];
        let failure = read_frames(
            oversized.as_slice(),
            true,
            incoming.clone(),
            pending.clone(),
        )
        .await;
        assert_eq!(failure.observation().1.outcome, ReaderOutcome::FrameLimit);
        let error = failure.into_error();
        assert_eq!(error.status, 502);
        assert_eq!(
            error.message,
            "MCP response exceeded the 2000000-byte limit."
        );
        drop(receiver);
        let failure =
            read_frames(&b"{\"method\":\"fixture\"}\n"[..], false, incoming, pending).await;
        assert_eq!(
            failure.observation().1.outcome,
            ReaderOutcome::IncomingClosed
        );
    }

    #[tokio::test]
    async fn actual_writer_failure_records_only_its_io_kind() {
        let (outgoing, mut output) = mpsc::channel(1);
        outgoing
            .send(Frame::new(
                Message::Notification {
                    method: "fixture".into(),
                    params: json!({ "private": "PRIVATE_PROMPT" }),
                },
                false,
            ))
            .await
            .unwrap();
        let observation = write_frames(&mut FaultIo, &mut output).await;
        assert_eq!(observation.outcome, WriterOutcome::Io);
        assert_eq!(observation.io_kind, Some(diagnostics::IoKind::BrokenPipe));
        let encoded = serde_json::to_string(&observation).unwrap();
        assert!(!encoded.contains("PRIVATE_IO_PATH"));
        assert!(!encoded.contains("PRIVATE_PROMPT"));
    }

    #[tokio::test]
    async fn stderr_cancellation_keeps_honest_partial_counts_without_retaining_content() {
        let (mut writing, mut reading) = tokio::io::duplex(64);
        let private = b"PRIVATE_STDERR_CONTENT";
        writing.write_all(private).await.unwrap();
        let mut observation = Stderr::default();
        {
            let draining = drain_stderr(&mut reading, &mut observation);
            tokio::pin!(draining);
            tokio::select! {
                biased;
                never = &mut draining => match never {},
                () = std::future::ready(()) => {}
            }
        }
        assert_eq!(observation.bytes, private.len() as u64);
        assert!(matches!(observation.availability, Availability::Partial));
        assert!(observation.io_kind.is_none());
        assert!(
            !serde_json::to_string(&observation)
                .unwrap()
                .contains("PRIVATE_STDERR_CONTENT")
        );
    }

    #[tokio::test]
    async fn missing_agent_executable_is_not_an_established_process_disconnect() {
        let root = tempfile::TempDir::new().unwrap();
        let result = Session::spawn(tokio::process::Command::new(
            root.path().join("missing-codex"),
        ))
        .await;
        let Err(error) = result else {
            panic!("An absent binary must not start")
        };
        assert!(error.message.contains("executable permissions"));
        assert!(crate::run_retry::startup_failure(&error).is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn cold_native_initialization_keeps_its_reply() {
        let (rpc, mut output) = rpc(false);
        let requesting = rpc.clone();
        let request =
            tokio::spawn(async move { requesting.request("initialize", json!({})).await });
        output.recv().await.unwrap();

        tokio::time::advance(Duration::from_secs(21)).await;
        tokio::task::yield_now().await;
        assert!(!request.is_finished());
        rpc.pending
            .lock()
            .await
            .remove(&1)
            .unwrap()
            .send(Ok(json!({})))
            .unwrap();
        assert_eq!(request.await.unwrap().unwrap(), json!({}));
        assert!(rpc.pending.lock().await.is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn a_slow_native_resume_keeps_its_reply_without_relaxing_account_requests() {
        let (rpc, mut output) = rpc(false);
        let resume_rpc = rpc.clone();
        let resume =
            tokio::spawn(async move { resume_rpc.request("thread/resume", json!({})).await });
        output.recv().await.unwrap();
        let account_rpc = rpc.clone();
        let account =
            tokio::spawn(async move { account_rpc.request("account/read", json!({})).await });
        output.recv().await.unwrap();

        tokio::time::advance(Duration::from_secs(21)).await;
        assert_eq!(account.await.unwrap().unwrap_err().status, 504);
        assert!(!resume.is_finished());
        let reply = rpc.pending.lock().await.remove(&1).unwrap();
        reply
            .send(Ok(json!({ "thread": { "id": "saved-session" } })))
            .unwrap();
        assert_eq!(
            resume.await.unwrap().unwrap()["thread"]["id"],
            "saved-session"
        );
        assert!(rpc.pending.lock().await.is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn a_hung_native_resume_expires_and_releases_its_pending_reply() {
        let (rpc, mut output) = rpc(false);
        let requesting = rpc.clone();
        let request =
            tokio::spawn(async move { requesting.request("thread/resume", json!({})).await });
        output.recv().await.unwrap();

        tokio::time::advance(Duration::from_secs(121)).await;
        let error = request.await.unwrap().unwrap_err();
        assert_eq!(error.status, 504);
        assert_eq!(error.message, "Codex thread/resume request timed out.");
        assert!(rpc.pending.lock().await.is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn closing_a_session_interrupts_native_resume_immediately() {
        let (rpc, mut output) = rpc(false);
        let requesting = rpc.clone();
        let request =
            tokio::spawn(async move { requesting.request("thread/resume", json!({})).await });
        output.recv().await.unwrap();

        rpc.closed.cancel();
        assert_eq!(request.await.unwrap().unwrap_err().status, 503);
        assert!(rpc.pending.lock().await.is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn mcp_requests_keep_the_short_deadline_even_for_a_resume_named_method() {
        let (rpc, mut output) = rpc(true);
        let requesting = rpc.clone();
        let request =
            tokio::spawn(async move { requesting.request("thread/resume", json!({})).await });
        output.recv().await.unwrap();

        tokio::time::advance(Duration::from_secs(21)).await;
        assert_eq!(request.await.unwrap().unwrap_err().status, 504);
        assert!(rpc.pending.lock().await.is_empty());
    }
}
