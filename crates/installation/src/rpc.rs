pub mod jsonrpc;

use crate::{
    config::Config,
    error::{Error, Result},
    process::{codex_environment, command},
};
use jsonrpc::{ErrorObject, Frame, METHOD_NOT_FOUND, Message};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    path::Path,
    process::Stdio,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin},
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

async fn write_frames(stdin: &mut ChildStdin, output: &mut mpsc::Receiver<Frame>) -> Result<()> {
    while let Some(frame) = output.recv().await {
        let mut bytes = serde_json::to_vec(&frame)?;
        bytes.push(b'\n');
        stdin.write_all(&bytes).await?;
    }
    Ok(())
}

/// Reads one newline-terminated frame into `bytes`.
async fn read_frame(
    reader: &mut BufReader<impl AsyncRead + Unpin>,
    bytes: &mut Vec<u8>,
    jsonrpc: bool,
) -> Result<()> {
    bytes.clear();
    loop {
        let buffer = reader.fill_buf().await?;
        if buffer.is_empty() {
            return Err(unavailable());
        }
        let end = buffer.iter().position(|b| *b == b'\n').map(|i| i + 1);
        let length = end.unwrap_or(buffer.len());
        if jsonrpc && bytes.len() + length > MCP_FRAME_LIMIT {
            return Err(Error::bad_gateway(
                "MCP response exceeded the 2000000-byte limit.",
            ));
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
) -> Result<()> {
    let mut reader = BufReader::new(stdout);
    let mut bytes = Vec::new();
    loop {
        read_frame(&mut reader, &mut bytes, jsonrpc).await?;
        let message: Value = serde_json::from_slice(&bytes).map_err(|_| unavailable())?;
        if let Some(method) = message["method"].as_str() {
            let request = Incoming {
                method: method.into(),
                params: message["params"].clone(),
                id: message.get("id").cloned(),
            };
            incoming.send(request).await.map_err(|_| unavailable())?;
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

/// Asks the process to exit, then kills it if it has not after two seconds.
pub(crate) async fn terminate(child: &mut Child) {
    if let Some(pid) = child.id() {
        unsafe {
            libc::kill(pid as i32, libc::SIGTERM);
        }
    }
    if tokio::time::timeout(Duration::from_secs(2), child.wait())
        .await
        .is_err()
        && let Err(error) = child.kill().await
    {
        tracing::warn!(%error, "could not kill child process");
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
        let mut child = command.spawn().map_err(|_| unavailable())?;
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
        let rpc = Rpc {
            jsonrpc,
            outgoing,
            pending: pending.clone(),
            sequence: Arc::new(AtomicU64::new(0)),
            closed: closed.clone(),
            failure: failure.clone(),
        };
        let stopping = stop.clone();
        tokio::spawn(async move {
            let writer = write_frames(&mut stdin, &mut output);
            let reader = read_frames(stdout, jsonrpc, incoming, pending.clone());
            let drain = async {
                // Keep stderr flowing; once it closes, leave the other branches to finish.
                let _ = tokio::io::copy(&mut stderr, &mut tokio::io::sink()).await;
                std::future::pending::<()>().await;
            };
            tokio::select! {
                () = stopping.cancelled() => {}
                () = closed.cancelled() => {}
                _ = child.wait() => {}
                _ = writer => {}
                result = reader => {
                    if let Err(error) = result {
                        *failure.lock().await = Some((error.status, error.message));
                    }
                }
                () = drain => {}
            }
            closed.cancel();
            for (_, reply) in pending.lock().await.drain() {
                let _ = reply.send(Err(unavailable()));
            }
            terminate(&mut child).await;
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
        let error = ErrorObject {
            code: METHOD_NOT_FOUND,
            message: "Interactive tool requests are unavailable. Ask the user in a plain assistant message instead.".into(),
            data: None,
        };
        self.send(Message::Error { id, error }).await
    }

    pub async fn closed(&self) {
        self.closed.cancelled().await;
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
            },
            output,
        )
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
