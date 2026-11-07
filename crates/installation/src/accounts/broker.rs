//! Run-scoped credential broker. Only the manager holds refresh credentials: a run asks this
//! private socket for access-only credentials, one JSON line per connection, and never writes
//! credentials back.
use super::{
    Lease,
    output_redactions::{Delivery, Handle},
};
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
    task: Option<tokio::task::JoinHandle<()>>,
}

impl Drop for Broker {
    fn drop(&mut self) {
        self.stop.cancel();
        // JoinHandle::drop detaches the independently owned admitted operation.
        // It must retain its account guard/session through durable completion.
        let _ = std::fs::remove_file(&self.path);
    }
}

impl Broker {
    #[cfg(test)]
    pub(crate) fn stopped_fixture(&self) -> CancellationToken {
        self.stop.clone()
    }

    /// Completion barrier: no accepted response can write after this returns.
    /// Admitted rotations retain their existing account lock through provider
    /// work, actual session termination and durable capture. Dropping this
    /// waiter detaches that owner, never aborts its critical section.
    pub(crate) async fn close(mut self) {
        self.stop.cancel();
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
    }
}

pub(crate) async fn serve_observed(s: &Service, lease: &Lease, scope: Handle) -> Result<Broker> {
    serve_inner(
        s,
        lease,
        Some(scope),
        #[cfg(test)]
        None,
    )
    .await
}

#[cfg(test)]
#[derive(Default)]
pub(crate) struct DeliveryProbe {
    pub(crate) selected: CancellationToken,
    pub(crate) resume: CancellationToken,
    pub(crate) partial: CancellationToken,
    pub(crate) small_socket: bool,
    pub(crate) written_bytes: std::sync::atomic::AtomicU64,
    pub(crate) total_bytes: std::sync::atomic::AtomicU64,
}

#[cfg(test)]
pub(crate) async fn serve_observed_fixture(
    s: &Service,
    lease: &Lease,
    scope: Handle,
    probe: std::sync::Arc<DeliveryProbe>,
) -> Result<Broker> {
    serve_inner(s, lease, Some(scope), Some(probe)).await
}

/// Serves the lease's account in its run home until the broker is dropped.
pub async fn serve(s: &Service, lease: &Lease) -> Result<Broker> {
    serve_inner(
        s,
        lease,
        None,
        #[cfg(test)]
        None,
    )
    .await
}

async fn serve_inner(
    s: &Service,
    lease: &Lease,
    scope: Option<Handle>,
    #[cfg(test)] probe: Option<std::sync::Arc<DeliveryProbe>>,
) -> Result<Broker> {
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
    let task = tokio::spawn(async move {
        loop {
            tokio::select! {
                biased;
                () = stopping.cancelled() => break,
                () = service.shutdown.cancelled() => break,
                accepted = listener.accept() => {
                    let Ok((stream, _)) = accepted else { break };
                    // The independent broker owner completes admitted provider
                    // rotation/session/capture while retaining its account lock.
                    // Stop cancels only socket I/O, never this critical section.
                    let _ = respond(&service, &lease, stream, scope.as_ref(), &stopping, #[cfg(test)] probe.as_deref()).await;
                }
            }
        }
    });
    Ok(Broker {
        stop,
        path,
        task: Some(task),
    })
}

async fn respond(
    s: &Service,
    lease: &Lease,
    stream: UnixStream,
    scope: Option<&Handle>,
    stop: &CancellationToken,
    #[cfg(test)] probe: Option<&DeliveryProbe>,
) -> Result<()> {
    let mut stream = BufReader::new(stream);
    let request = tokio::select! {
        biased;
        () = stop.cancelled() => return Err(Error::conflict("Account broker stopped.")),
        () = s.shutdown.cancelled() => return Err(Error::conflict("Account broker stopped.")),
        request = tokio::time::timeout(Duration::from_secs(2), line(&mut stream)) => {
            request.map_err(|_| Error::bad("Account request timed out."))??
        }
    };
    if stop.is_cancelled() || s.shutdown.is_cancelled() {
        return Err(Error::conflict("Account broker stopped."));
    }
    if let Some(scope) = scope {
        scope.check(lease)?;
    }
    // Neither provider errors nor credentials enter events or logs.
    let response = s.accounts.access(s, lease, &request).await.unwrap_or_else(|_| {
        json!({
            "error": "Account authentication is unavailable. Reconnect the account after its runs finish."
        })
    });
    #[cfg(test)]
    if let Some(probe) = probe {
        probe.selected.cancel();
        tokio::select! {
            biased;
            () = stop.cancelled() => return Err(Error::conflict("Account broker stopped.")),
            () = probe.resume.cancelled() => {}
        }
    }
    let _delivery: Option<Delivery> = scope
        .map(|scope| scope.admit(lease, super::codex::delivered_redactions(&response)))
        .transpose()?;
    let mut bytes = serde_json::to_vec(&response)?;
    bytes.push(b'\n');
    #[cfg(test)]
    if let Some(probe) = probe {
        probe
            .total_bytes
            .store(bytes.len() as u64, std::sync::atomic::Ordering::Relaxed);
        if probe.small_socket {
            use std::os::fd::AsRawFd;
            let capacity: libc::c_int = 4096;
            let result = unsafe {
                libc::setsockopt(
                    stream.get_ref().as_raw_fd(),
                    libc::SOL_SOCKET,
                    libc::SO_SNDBUF,
                    (&raw const capacity).cast(),
                    std::mem::size_of_val(&capacity) as libc::socklen_t,
                )
            };
            assert_eq!(result, 0);
        }
    }
    let mut written = 0;
    while written < bytes.len() {
        let count = tokio::select! {
            biased;
            () = stop.cancelled() => return Err(Error::conflict("Account broker stopped.")),
            () = s.shutdown.cancelled() => return Err(Error::conflict("Account broker stopped.")),
            count = stream.get_mut().write(&bytes[written..]) => count?,
        };
        if count == 0 {
            return Err(Error::bad("Account connection closed."));
        }
        written += count;
        #[cfg(test)]
        if let Some(probe) = probe {
            probe
                .written_bytes
                .store(written as u64, std::sync::atomic::Ordering::Relaxed);
            if written < bytes.len() {
                probe.partial.cancel();
            }
        }
    }
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

#[cfg(test)]
#[path = "broker_tests.rs"]
pub(crate) mod tests;
