//! Run-scoped credential broker. Only the manager holds refresh credentials: a run asks this
//! private socket for access-only credentials, one JSON line per connection, and never writes
//! credentials back.
use super::Lease;
use crate::{
    error::{Error, Result},
    service::Service,
    skills::private_dir,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::{UnixListener, UnixStream},
};
use tokio_util::sync::CancellationToken;

pub const SOCKET: &str = "cairn-auth.sock";
const LIMIT: usize = 128_000;

/// What a run asks its broker for. Claude Code runs always ask for current credentials.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Request {
    /// Codex: the run's access token was rejected. Rotate it unless another run already did.
    #[serde(default)]
    pub refresh: bool,
    /// Codex: a digest of the access token the run holds.
    #[serde(default)]
    pub previous: String,
}

pub struct Broker {
    stop: CancellationToken,
    path: PathBuf,
}

impl Drop for Broker {
    fn drop(&mut self) {
        self.stop.cancel();
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Serves the lease's account in its run home until the broker is dropped.
pub async fn serve(s: &Service, lease: &Lease) -> Result<Broker> {
    use std::os::unix::fs::PermissionsExt;
    private_dir(&lease.home).await?;
    let path = lease.home.join(SOCKET);
    if path.exists() {
        tokio::fs::remove_file(&path).await?;
    }
    let listener = UnixListener::bind(&path)?;
    tokio::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).await?;
    let stop = CancellationToken::new();
    let stopping = stop.clone();
    let service = s.clone();
    let lease = lease.clone();
    tokio::spawn(async move {
        loop {
            tokio::select! {
                () = stopping.cancelled() => break,
                () = service.shutdown.cancelled() => break,
                accepted = listener.accept() => {
                    let Ok((stream, _)) = accepted else { break };
                    // One request at a time per run. Never cancel a token rotation between
                    // the provider's response and durable storage.
                    let _ = respond(&service, &lease, stream).await;
                }
            }
        }
    });
    Ok(Broker { stop, path })
}

async fn respond(s: &Service, lease: &Lease, stream: UnixStream) -> Result<()> {
    let mut stream = BufReader::new(stream);
    let request = tokio::time::timeout(Duration::from_secs(2), line(&mut stream))
        .await
        .map_err(|_| Error::bad("Account request timed out."))??;
    // Neither provider errors nor credentials enter events or logs.
    let response = s.accounts.access(s, lease, &request).await.unwrap_or_else(|_| {
        json!({
            "error": "Account authentication is unavailable. Reconnect the account after its runs finish."
        })
    });
    let mut bytes = serde_json::to_vec(&response)?;
    bytes.push(b'\n');
    stream.get_mut().write_all(&bytes).await?;
    Ok(())
}

async fn line(stream: &mut BufReader<UnixStream>) -> Result<Value> {
    let mut bytes = Vec::new();
    loop {
        let buffer = stream.fill_buf().await?;
        if buffer.is_empty() {
            return Err(Error::bad("Account connection closed."));
        }
        let end = buffer.iter().position(|b| *b == b'\n');
        let length = end.map_or(buffer.len(), |n| n + 1);
        if bytes.len() + length > LIMIT {
            return Err(Error::bad("Invalid account message."));
        }
        bytes.extend_from_slice(&buffer[..length]);
        stream.consume(length);
        if end.is_some() {
            return serde_json::from_slice(&bytes)
                .map_err(|_| Error::bad("Invalid account message."));
        }
    }
}

/// The run side: one request, one response.
pub async fn request(socket: &Path, request: &impl Serialize, timeout: Duration) -> Result<Value> {
    tokio::time::timeout(timeout, async {
        let mut stream = UnixStream::connect(socket).await?;
        let mut bytes = serde_json::to_vec(request)?;
        bytes.push(b'\n');
        stream.write_all(&bytes).await?;
        line(&mut BufReader::new(stream)).await
    })
    .await
    .map_err(|_| Error::unavailable("Account authentication timed out."))?
}

/// The socket a run reaches its broker through: inside a microVM it is mapped elsewhere.
pub fn socket(home: &Path) -> PathBuf {
    std::env::var_os("CAIRN_AUTH_SOCKET").map_or_else(|| home.join(SOCKET), PathBuf::from)
}
