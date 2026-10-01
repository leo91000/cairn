//! One guest-private Codex process, leased to one chat attempt at a time.
//! The controller must retain this service only with its conversation's VM.
use super::run_session;
use crate::{
    config::Config,
    error::{Error, Result},
    rpc::Session,
    skills::private_dir,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{os::unix::fs::PermissionsExt, path::Path, time::Duration};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::{UnixListener, UnixStream},
    sync::mpsc,
};
use tokio_util::sync::CancellationToken;

const MAX_PLAN: usize = 8_000_000;
// Match the host's total output bound; native tool items can exceed control frames.
const MAX_EVENT: usize = 100_000_000;

#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
enum Reply {
    Event { value: Value },
    Finished { status: u16, message: String },
}

/// Bind before bootstrapping so concurrent service owners cannot start Codex.
/// A pre-existing socket is never unlinked: the controller owns lifecycle cleanup.
pub async fn serve(
    config: &Config,
    home: &Path,
    socket: &Path,
    stop: CancellationToken,
) -> Result<()> {
    let parent = socket
        .parent()
        .ok_or_else(|| Error::bad("Invalid Codex socket."))?;
    private_dir(parent).await?;
    let listener = UnixListener::bind(socket)?;
    tokio::fs::set_permissions(socket, std::fs::Permissions::from_mode(0o600)).await?;
    let result = async {
        let mut session =
            Session::codex_until(config, home, &[], Some(&config.home), &stop).await?;
        let result = serve_session(listener, &mut session, home, stop).await;
        session.close().await;
        result
    }
    .await;
    let _ = tokio::fs::remove_file(socket).await;
    result
}

async fn serve_session(
    listener: UnixListener,
    session: &mut Session,
    home: &Path,
    stop: CancellationToken,
) -> Result<()> {
    loop {
        let (mut client, _) = tokio::select! {
            () = stop.cancelled() => return Ok(()),
            client = listener.accept() => client?,
            incoming = session.incoming.recv() => {
                let incoming = incoming.ok_or_else(|| Error::unavailable("Resident Codex stopped."))?;
                // There is no account relay outside a running attempt.
                if let Some(id) = incoming.id {
                    session.rpc.reject(id).await?;
                }
                continue;
            }
        };
        require_owner(&client)?;
        let result = handle(&mut client, &listener, session, home, stop.clone()).await;
        // An unknown client/native state cannot be offered to another attempt.
        result?;
    }
}

fn require_owner(stream: &UnixStream) -> Result<()> {
    if stream.peer_cred()?.uid() != unsafe { libc::geteuid() } {
        return Err(Error::new(403, "Codex service belongs to another owner."));
    }
    Ok(())
}

async fn read_value(read: &mut (impl AsyncRead + Unpin), limit: usize) -> Result<Value> {
    // Length-prefixed admission keeps an untrusted client from allocating an
    // unbounded newline frame. The other direction carries normal chat events.
    let length = read.read_u32().await? as usize;
    if length > limit {
        return Err(Error::bad("Codex message exceeded the supported limit."));
    }
    let mut bytes = vec![0; length];
    read.read_exact(&mut bytes).await?;
    Ok(serde_json::from_slice(&bytes)?)
}

async fn write_value(
    write: &mut (impl AsyncWrite + Unpin),
    value: &impl Serialize,
    limit: usize,
) -> Result<()> {
    let bytes = serde_json::to_vec(value)?;
    if bytes.len() > limit {
        return Err(Error::bad("Codex message exceeded the supported limit."));
    }
    let send = async {
        write.write_u32(bytes.len() as u32).await?;
        write.write_all(&bytes).await?;
        write.flush().await?;
        Ok::<(), Error>(())
    };
    tokio::time::timeout(Duration::from_secs(10), send)
        .await
        .map_err(|_| Error::unavailable("Codex client stopped reading events."))?
}

/// Readiness means native initialization has finished, not just that a socket exists.
pub async fn ready(socket: &Path) -> Result<bool> {
    let mut client = UnixStream::connect(socket).await?;
    require_owner(&client)?;
    write_value(&mut client, &json!({ "op": "ready" }), MAX_PLAN).await?;
    Ok(read_value(&mut client, MAX_EVENT).await?["ready"] == true)
}

async fn handle(
    client: &mut UnixStream,
    listener: &UnixListener,
    session: &mut Session,
    home: &Path,
    stop: CancellationToken,
) -> Result<()> {
    let (mut read, mut write) = client.split();
    let plan = tokio::time::timeout(Duration::from_secs(10), read_value(&mut read, MAX_PLAN))
        .await
        .map_err(|_| Error::bad("Codex lease admission timed out."))??;
    if plan["op"] == "ready" {
        write_value(&mut write, &json!({ "ready": true }), MAX_EVENT).await?;
        return Ok(());
    }
    if !plan["codexConfig"]["mcp_servers"].is_object() {
        return Err(Error::bad(
            "Resident Codex requires a thread configuration.",
        ));
    }
    tokio::select! {
        result = execute_lease(session, home, plan, &mut read, &mut write, stop) => result,
        result = reject_busy(listener) => result,
    }
}

async fn reject_busy(listener: &UnixListener) -> Result<()> {
    loop {
        let (mut client, _) = listener.accept().await?;
        if require_owner(&client).is_err() {
            continue;
        }
        // An active attempt never queues behind another native turn. Admission
        // owns its retry policy; do not hold an account lease in this guest queue.
        let reply = Reply::Finished {
            status: 409,
            message: "Codex service is already leased.".into(),
        };
        let _ = tokio::time::timeout(
            Duration::from_millis(100),
            write_value(&mut client, &reply, MAX_EVENT),
        )
        .await;
    }
}

async fn execute_lease(
    session: &mut Session,
    home: &Path,
    plan: Value,
    read: &mut (impl AsyncRead + Unpin),
    write: &mut (impl AsyncWrite + Unpin),
    stop: CancellationToken,
) -> Result<()> {
    let (events, mut received) = mpsc::channel(32);
    let cancel = CancellationToken::new();
    let run = run_session(session, home, plan, events, cancel.clone(), true);
    tokio::pin!(run);
    let mut disconnected = false;
    let mut failure = None;
    let result = loop {
        let mut byte = [0_u8; 1];
        tokio::select! {
            result = &mut run => break result,
            event = received.recv() => {
                if let Some(value) = event
                    && !disconnected
                    && let Err(error) = write_value(write, &Reply::Event { value }, MAX_EVENT).await
                {
                    failure = Some(error);
                    disconnected = true;
                    cancel.cancel();
                }
            }
            () = stop.cancelled(), if !disconnected => {
                disconnected = true;
                cancel.cancel();
            }
            _ = read.read(&mut byte), if !disconnected => {
                // No further bytes are valid on this one-attempt lease.
                disconnected = true;
                cancel.cancel();
            }
        }
    };
    while let Ok(value) = received.try_recv() {
        if !disconnected {
            write_value(write, &Reply::Event { value }, MAX_EVENT).await?;
        }
    }
    if disconnected {
        return Err(failure.unwrap_or_else(|| Error::unavailable("Codex client disconnected.")));
    }
    let reply = match &result {
        Ok(()) => Reply::Finished {
            status: 200,
            message: String::new(),
        },
        Err(error) => Reply::Finished {
            status: error.status,
            message: error.message.clone(),
        },
    };
    write_value(write, &reply, MAX_EVENT).await?;
    result
}

/// A client never retries a broken lease: the owner must retire its VM first.
pub async fn run(
    socket: &Path,
    plan: Value,
    events: mpsc::Sender<Value>,
    cancel: CancellationToken,
) -> Result<()> {
    let mut stream = UnixStream::connect(socket).await?;
    require_owner(&stream)?;
    write_value(&mut stream, &plan, MAX_PLAN).await?;
    loop {
        let reply: Reply = tokio::select! {
            () = cancel.cancelled() => return Err(Error::unavailable("Conversation stopped.")),
            reply = read_value(&mut stream, MAX_EVENT) => {
                serde_json::from_value(reply?)?
            }
        };
        match reply {
            Reply::Event { value } => events
                .send(value)
                .await
                .map_err(|_| Error::unavailable("Codex event output stopped."))?,
            Reply::Finished { status: 200, .. } => return Ok(()),
            Reply::Finished { status, message } => return Err(Error::new(status, message)),
        }
    }
}
