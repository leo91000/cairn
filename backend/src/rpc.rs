use crate::{
    config::Config,
    error::{Error, Result},
    process::{codex_environment, command},
};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    path::Path,
    process::Stdio,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    sync::{Mutex, mpsc, oneshot},
};
use tokio_util::sync::CancellationToken;

type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value>>>>>;

#[derive(Debug)]
pub struct Incoming {
    pub method: String,
    pub params: Value,
    pub id: Option<Value>,
}

#[derive(Clone)]
pub struct Rpc {
    jsonrpc: bool,
    outgoing: mpsc::Sender<Value>,
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
}

impl Drop for Session {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}

impl Session {
    pub async fn codex(
        config: &Config,
        home: &Path,
        args: &[String],
        cwd: Option<&Path>,
    ) -> Result<Self> {
        let mut args = args.to_vec();
        args.extend(
            [
                "-c",
                "cli_auth_credentials_store=\"file\"",
                "-c",
                "forced_login_method=\"chatgpt\"",
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
        let initialized = async {
            // Initialization does not require interactive requests. Reject them while negotiating.
            let rpc = session.rpc.clone();
            let initialize = rpc.request(
                "initialize",
                json!({
                    "clientInfo": {
                        "name": "leo_agent_manager",
                        "version": env!("CARGO_PKG_VERSION")
                    }
                ,
                    "capabilities": {
                        "experimentalApi": true
                    }
                }
                ),
            );
            tokio::pin!(initialize);
            loop {
                tokio::select! {
                    result = &mut initialize => {
                result?;
                break;
                }
                ,
                    incoming = session.incoming.recv() => {
                if let Some(incoming)=incoming {
                if let Some(id)=incoming.id {
                rpc.reject(id).await?;
                }
                }
                else{
                return Err(unavailable());
                }
                }
                }
            }
            rpc.notify("initialized", json!({})).await?;
            Ok::<_, Error>(())
        }
        .await;
        if let Err(error) = initialized {
            // The caller cannot close a session that failed to initialize. Reap its
            // process here before relinquishing ownership (and its account lease).
            session.close().await;
            return Err(error);
        }
        Ok(session)
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
        let (outgoing, mut output) = mpsc::channel::<Value>(64);
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
            let writer = async {
                while let Some(value) = output.recv().await {
                    let mut bytes = serde_json::to_vec(&value)?;
                    bytes.push(b'\n');
                    stdin.write_all(&bytes).await?;
                }
                Ok::<_, Error>(())
            };
            let reader = async {
                let mut reader = BufReader::new(stdout);
                let mut bytes = Vec::new();
                loop {
                    bytes.clear();
                    // Bound MCP frames even if a server never emits a newline.
                    // Codex frames can contain large tool results or conversation history.
                    loop {
                        let buffer = reader.fill_buf().await?;
                        if buffer.is_empty() {
                            return Err::<(), Error>(unavailable());
                        }
                        let end = buffer.iter().position(|b| *b == b'\n').map(|i| i + 1);
                        let length = end.unwrap_or(buffer.len());
                        if jsonrpc && bytes.len() + length > 2_000_000 {
                            return Err(Error::bad_gateway(
                                "MCP response exceeded the 2000000-byte limit.",
                            ));
                        }
                        bytes.extend_from_slice(&buffer[..length]);
                        reader.consume(length);
                        if end.is_some() {
                            break;
                        }
                    }
                    let message: Value =
                        serde_json::from_slice(&bytes).map_err(|_| unavailable())?;
                    if let Some(method) = message["method"].as_str() {
                        incoming
                            .send(Incoming {
                                method: method.into(),
                                params: message["params"].clone(),
                                id: message.get("id").cloned(),
                            })
                            .await
                            .map_err(|_| unavailable())?;
                        continue;
                    }
                    if let Some(id) = message["id"].as_u64()
                        && let Some(reply) = pending.lock().await.remove(&id)
                    {
                        let result = if message.get("error").is_some() {
                            Err(Error::new(
                                if jsonrpc && message["error"]["code"] == -32601 {
                                    501
                                } else {
                                    502
                                },
                                if message["error"]["code"] == -32601 {
                                    "Update Codex to support this account operation."
                                } else {
                                    "Codex could not complete this operation. Reconnect it and try again."
                                },
                            ))
                        } else {
                            Ok(message["result"].clone())
                        };
                        let _ = reply.send(result);
                    }
                }
            };
            let drain = async { tokio::io::copy(&mut stderr, &mut tokio::io::sink()).await };
            tokio::pin!(writer, reader, drain);
            tokio::select! {
                _ = stopping.cancelled() => {
            }
            ,
                _ = closed.cancelled() => {
            }
            ,
                _ = child.wait() => {
            }
            ,
                _ = &mut writer => {
            }
            ,
                result = &mut reader => {
                if let Err(error) = result {
                    *failure.lock().await = Some((error.status, error.message));
                }
            }
            ,
                _ = async{
            let _=(&mut drain).await;
            std::future::pending::<()>().await} => {
            }
            }
            closed.cancel();
            for (_, reply) in pending.lock().await.drain() {
                let _ = reply.send(Err(unavailable()));
            }
            if let Some(pid) = child.id() {
                unsafe {
                    libc::kill(pid as i32, libc::SIGTERM);
                }
            }
            if tokio::time::timeout(Duration::from_secs(2), child.wait())
                .await
                .is_err()
            {
                let _ = child.kill().await;
            }
            let _ = done.send(());
        });
        Ok(Self {
            auth: None,
            rpc,
            incoming: receiver,
            stop,
            finished: Some(finished),
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
            let Some(incoming)=incoming else{
            return Err(unavailable());
            }
            ;
            if !self.handle_auth(&incoming).await? && let Some(id)=incoming.id{
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

    async fn send(&self, mut value: Value) -> Result<()> {
        if self.jsonrpc {
            value["jsonrpc"] = "2.0".into();
        }
        tokio::select! {
            _ = self.closed.cancelled() => Err(self.failure().await),
            result = self.outgoing.send(value) => result.map_err(|_|unavailable())
        }
    }

    pub async fn request(&self, method: &str, params: Value) -> Result<Value> {
        let id = self.sequence.fetch_add(1, Ordering::Relaxed) + 1;
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, tx);
        let result = async {
            self.send(json!({
                "id": id,
                "method": method,
                "params": params
            }
            ))
            .await?;
            tokio::select! {
                _ = self.closed.cancelled() => Err(self.failure().await),
                result = rx => match result {
                Ok(Ok(value)) => Ok(value),
                _ if self.closed.is_cancelled() => Err(self.failure().await),
                Ok(Err(error)) => Err(error),
                Err(_) => Err(unavailable()),
            }
            }
        };
        let result = tokio::time::timeout(Duration::from_secs(20), result)
            .await
            .unwrap_or_else(|_| Err(Error::gateway_timeout("Codex account request timed out.")));
        self.pending.lock().await.remove(&id);
        result
    }

    pub async fn notify(&self, method: &str, params: Value) -> Result<()> {
        self.send(json!({
            "method": method,
            "params": params
        }
        ))
        .await
    }

    pub async fn reply(&self, id: Value, result: Value) -> Result<()> {
        self.send(json!({
            "id": id,
            "result": result
        }
        ))
        .await
    }

    pub async fn reject(&self, id: Value) -> Result<()> {
        self.send(json!({
            "id": id,
            "error": {
                "code": -32601,
                "message": "Interactive tool requests are unavailable. Ask the user in a plain assistant message instead."
            }
        }
        ))
        .await
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
