//! The VM-local MCP origin. A guest loopback port is relayed over vsock to the
//! run's MCP channel on the host, so agents never need the manager's network.
use super::{listener, wire};
use std::path::PathBuf;
use tokio::{net::UnixListener, task::JoinHandle};
use tokio_util::sync::CancellationToken;

/// The guest loopback port, and the host vsock port it is relayed to.
pub const PORT: u32 = wire::PORT + 2;
/// The origin of run-scoped MCP for an agent inside a VM.
pub const ORIGIN: &str = "http://127.0.0.1:5202";
/// The run channel's socket in the run home, served during an attempt by the
/// manager or by the node connector that executes it.
pub const SOCKET: &str = "cairn-mcp.sock";
/// Concurrent MCP connections of one VM. Tool calls can last as long as their
/// attempt, so connections are bounded in number rather than in duration.
const CONNECTIONS: usize = 32;

/// Guest side: relays loopback connections to the host for the VM's lifetime.
pub(super) async fn serve_guest(stop: CancellationToken) {
    use tokio_vsock::{VsockAddr, VsockStream};

    let listener = match tokio::net::TcpListener::bind(("127.0.0.1", PORT as u16)).await {
        Ok(listener) => listener,
        Err(error) => {
            tracing::warn!(%error, "Guest MCP listener is unavailable");
            return;
        }
    };
    let result = listener::serve(
        || listener.accept(),
        |(mut agent, _)| async move {
            if let Ok(mut host) = VsockStream::connect(VsockAddr::new(2, PORT)).await {
                let _ = tokio::io::copy_bidirectional(&mut agent, &mut host).await;
            }
        },
        CONNECTIONS,
        stop,
    )
    .await;
    if let Err(error) = result {
        tracing::warn!(%error, "Guest MCP listener stopped");
    }
}

/// Host side: relays an attempt's guest connections to its run channel. A run
/// without a channel gets its connections closed.
pub(super) fn relay(
    listener: UnixListener,
    channel: Option<PathBuf>,
    stop: CancellationToken,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let result = listener::serve(
            || listener.accept(),
            |(mut guest, _)| {
                let channel = channel.clone();
                async move {
                    let Some(channel) = channel else {
                        return;
                    };
                    if let Ok(mut run) = tokio::net::UnixStream::connect(channel).await {
                        let _ = tokio::io::copy_bidirectional(&mut guest, &mut run).await;
                    }
                }
            },
            CONNECTIONS,
            stop,
        )
        .await;
        if let Err(error) = result {
            tracing::warn!(%error, "VM MCP relay stopped");
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[test]
    fn the_agent_origin_is_the_relayed_guest_port() {
        assert_eq!(ORIGIN, format!("http://127.0.0.1:{PORT}"));
    }

    #[tokio::test]
    async fn guest_connections_reach_only_the_run_channel_until_the_attempt_ends() {
        let directory = tempfile::tempdir().unwrap();
        let guest = directory.path().join("v.sock_5202");
        let channel = directory.path().join(SOCKET);
        let run = UnixListener::bind(&channel).unwrap();
        let stop = CancellationToken::new();
        let relay = relay(
            UnixListener::bind(&guest).unwrap(),
            Some(channel),
            stop.clone(),
        );
        let answered = tokio::spawn(async move {
            let (mut stream, _) = run.accept().await.unwrap();
            let mut request = [0; 4];
            stream.read_exact(&mut request).await.unwrap();
            stream.write_all(b"pong").await.unwrap();
            request
        });

        let mut agent = tokio::net::UnixStream::connect(&guest).await.unwrap();
        agent.write_all(b"ping").await.unwrap();
        let mut reply = [0; 4];
        agent.read_exact(&mut reply).await.unwrap();

        assert_eq!(&answered.await.unwrap(), b"ping");
        assert_eq!(&reply, b"pong");
        stop.cancel();
        relay.await.unwrap();
        assert!(tokio::net::UnixStream::connect(&guest).await.is_err());
    }

    #[tokio::test]
    async fn a_run_without_a_channel_closes_guest_connections() {
        let directory = tempfile::tempdir().unwrap();
        let guest = directory.path().join("v.sock_5202");
        let stop = CancellationToken::new();
        let relay = relay(UnixListener::bind(&guest).unwrap(), None, stop.clone());

        let mut agent = tokio::net::UnixStream::connect(&guest).await.unwrap();
        let mut reply = Vec::new();
        agent.read_to_end(&mut reply).await.unwrap();

        assert!(reply.is_empty());
        stop.cancel();
        relay.await.unwrap();
    }
}
