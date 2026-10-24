//! Loopback origin for an execution node added without a manager address.
//!
//! Beacon introduces the node to its manager. The gateway then reaches the
//! manager directly over WebRTC when the networks allow it ([`direct`]), and
//! otherwise through Beacon ([`relay`]). Both paths end on the manager's node
//! certificate, pinned from the install command, so Beacon can neither read
//! nor impersonate the manager. Each local connection is carried unchanged:
//! the node and manager keep authenticating every request with their own
//! codes and tokens, and the gateway holds no credential.
pub mod direct;
pub mod relay;

use serde::{Deserialize, Serialize};
use std::{
    io,
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};
use tokio::{
    io::{AsyncRead, AsyncWrite, AsyncWriteExt},
    net::{TcpListener, UnixListener},
    sync::Semaphore,
};
use tokio_util::sync::CancellationToken;

pub const PORT: u16 = 4319;
const MAX_CONNECTIONS: usize = 128;
// After an answer, before the next request of a pooled connection.
const IDLE: std::time::Duration = std::time::Duration::from_secs(15);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Config {
    pub beacon: String,
    pub installation_id: String,
    /// The manager's node certificate pin.
    pub pin: String,
    #[serde(default = "default_port")]
    pub port: u16,
    /// Off keeps every connection on Beacon, for networks that block UDP.
    #[serde(default = "enabled")]
    pub direct: bool,
}

fn default_port() -> u16 {
    PORT
}

fn enabled() -> bool {
    true
}

#[derive(Debug)]
pub struct Invalid(pub String);

impl std::fmt::Display for Invalid {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for Invalid {}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, Invalid(message.to_owned()))
}

impl Config {
    pub fn validate(&self) -> io::Result<()> {
        beacon_origin(&self.beacon)?;
        if !installation_id(&self.installation_id) {
            return Err(invalid(
                "Invalid installation ID in the gateway configuration.",
            ));
        }
        if !cairn_protocol::nodes::valid_pin(&self.pin) {
            return Err(invalid("Invalid manager certificate pin."));
        }
        Ok(())
    }

    pub fn load(root: &Path) -> io::Result<Self> {
        let config: Self = serde_json::from_slice(&std::fs::read(root.join("config.json"))?)
            .map_err(|_| invalid("Invalid gateway configuration."))?;
        config.validate()?;
        Ok(config)
    }
}

fn installation_id(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(index, byte)| match index {
            8 | 13 | 18 | 23 => byte == b'-',
            _ => byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte),
        })
}

/// The Beacon origin: HTTPS, or HTTP on loopback for tests.
pub fn beacon_origin(value: &str) -> io::Result<url::Url> {
    let url = url::Url::parse(value).map_err(|_| invalid("Use the HTTPS Beacon origin."))?;
    let loopback = matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
    if url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || !matches!(url.path(), "" | "/")
        || url.query().is_some()
        || url.fragment().is_some()
        || !(url.scheme() == "https" || url.scheme() == "http" && loopback)
    {
        return Err(invalid(
            "Use the HTTPS Beacon origin (HTTP only on loopback for tests).",
        ));
    }
    Ok(url)
}

/// Writes the configuration under `root`; `false` when it was already current.
pub fn configure(root: &Path, beacon: &str, installation: &str, pin: &str) -> io::Result<bool> {
    let mut config = Config {
        beacon: beacon_origin(beacon)?
            .as_str()
            .trim_end_matches('/')
            .to_owned(),
        installation_id: installation.to_ascii_lowercase(),
        pin: pin.to_ascii_lowercase(),
        port: PORT,
        direct: true,
    };
    if !installation_id(&config.installation_id) || !cairn_protocol::nodes::valid_pin(&config.pin) {
        return Err(invalid(
            "Copy the complete command from Nodes → Add a machine.",
        ));
    }
    let path = root.join("config.json");
    if let Ok(current) = std::fs::read(&path)
        && let Ok(current) = serde_json::from_slice::<Config>(&current)
    {
        if current.installation_id != config.installation_id {
            return Err(invalid(
                "This machine already runs nodes for another installation. Uninstall them before adding it to this one.",
            ));
        }
        config.port = current.port;
        config.direct = current.direct;
        if current == config {
            return Ok(false);
        }
    }
    std::fs::create_dir_all(root)?;
    std::fs::set_permissions(root, std::os::unix::fs::PermissionsExt::from_mode(0o700))?;
    let temporary = root.join(".config.json.tmp");
    std::fs::write(&temporary, serde_json::to_vec(&config)?)?;
    std::fs::set_permissions(
        &temporary,
        std::os::unix::fs::PermissionsExt::from_mode(0o600),
    )?;
    std::fs::rename(temporary, path)?;
    Ok(true)
}

/// Local connections carried on each path, for diagnostics and tests.
#[derive(Default)]
pub struct Statistics {
    pub direct: AtomicU64,
    pub relay: AtomicU64,
}

pub struct Gateway {
    tcp: TcpListener,
    unix: UnixListener,
    socket: PathBuf,
    config: Arc<Config>,
    statistics: Arc<Statistics>,
}

trait Local: AsyncRead + AsyncWrite + Unpin + Send + 'static {}

impl<T: AsyncRead + AsyncWrite + Unpin + Send + 'static> Local for T {}

impl Gateway {
    /// Listens on loopback and on `root/socket/gateway.sock`. Firecracker
    /// nodes run in a container with their own loopback; they reach the
    /// gateway through that socket, mounted at the same path.
    pub async fn bind(config: Config, root: &Path) -> io::Result<Self> {
        config.validate()?;
        let tcp = TcpListener::bind(("127.0.0.1", config.port)).await?;
        let directory = root.join("socket");
        std::fs::create_dir_all(&directory)?;
        std::fs::set_permissions(
            &directory,
            std::os::unix::fs::PermissionsExt::from_mode(0o700),
        )?;
        let socket = directory.join("gateway.sock");
        let _ = std::fs::remove_file(&socket);
        let unix = UnixListener::bind(&socket)?;
        std::fs::set_permissions(&socket, std::os::unix::fs::PermissionsExt::from_mode(0o600))?;
        Ok(Self {
            tcp,
            unix,
            socket,
            config: Arc::new(config),
            statistics: Arc::default(),
        })
    }

    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.tcp.local_addr()
    }

    pub fn statistics(&self) -> Arc<Statistics> {
        self.statistics.clone()
    }

    pub async fn serve(self, stop: CancellationToken) {
        let slots = Arc::new(Semaphore::new(MAX_CONNECTIONS));
        let direct = direct::Direct::start(self.config.clone(), stop.clone());
        loop {
            let local: Box<dyn Local> = tokio::select! {
                () = stop.cancelled() => break,
                accepted = self.tcp.accept() => match accepted {
                    Ok((stream, _)) => Box::new(stream),
                    Err(_) => continue,
                },
                accepted = self.unix.accept() => match accepted {
                    Ok((stream, _)) => Box::new(stream),
                    Err(_) => continue,
                },
            };
            let Ok(permit) = slots.clone().try_acquire_owned() else {
                tokio::spawn(fail(local, 503, "Too many local node connections."));
                continue;
            };
            let config = self.config.clone();
            let direct = direct.clone();
            let statistics = self.statistics.clone();
            tokio::spawn(async move {
                let _permit = permit;
                carry(local, &config, &direct, &statistics).await;
            });
        }
        let _ = std::fs::remove_file(&self.socket);
    }
}

async fn carry(
    local: Box<dyn Local>,
    config: &Config,
    direct: &direct::Direct,
    statistics: &Statistics,
) {
    if let Some(channel) = direct.channel().await {
        statistics.direct.fetch_add(1, Ordering::Relaxed);
        tracing::debug!("Connection carried directly");
        direct::splice(local, channel).await;
        tracing::debug!("Direct connection ended");
        return;
    }
    match relay::open(config).await {
        Ok(remote) => {
            statistics.relay.fetch_add(1, Ordering::Relaxed);
            tracing::debug!("Connection carried through Beacon");
            relayed(local, remote, direct.available()).await;
            tracing::debug!("Connection through Beacon ended");
        }
        Err(refused) => {
            tracing::debug!(status = refused.status, reason = %refused.message, "Connection refused");
            fail(local, refused.status, &refused.message).await;
        }
    }
}

/// Copies a connection through Beacon. Once a direct connection is up, one
/// left idle after the manager's last bytes is closed: its client reconnects,
/// and goes direct, before its next request, so pooled connections do not keep
/// disks on Beacon. A request still waiting for its answer is never cut.
async fn relayed<L, R>(local: L, remote: R, mut available: tokio::sync::watch::Receiver<bool>)
where
    L: AsyncRead + AsyncWrite + Unpin,
    R: AsyncRead + AsyncWrite + Unpin,
{
    use tokio::io::AsyncReadExt;
    let (mut from_local, mut to_local) = tokio::io::split(local);
    let (mut from_remote, mut to_remote) = tokio::io::split(remote);
    let (mut local_buffer, mut remote_buffer) = (vec![0; 65_536], vec![0; 65_536]);
    let (mut local_open, mut watching, mut answered) = (true, true, false);
    let mut active = tokio::time::Instant::now();
    loop {
        let idle = tokio::time::sleep_until(active + IDLE);
        tokio::select! {
            read = from_local.read(&mut local_buffer), if local_open => match read {
                Ok(0) | Err(_) => {
                    // The manager answers what it has, then closes.
                    local_open = false;
                    let _ = to_remote.shutdown().await;
                }
                Ok(read) => {
                    if to_remote.write_all(&local_buffer[..read]).await.is_err() {
                        break;
                    }
                    active = tokio::time::Instant::now();
                    answered = false;
                }
            },
            read = from_remote.read(&mut remote_buffer) => match read {
                Ok(0) | Err(_) => break,
                Ok(read) => {
                    if to_local.write_all(&remote_buffer[..read]).await.is_err() {
                        break;
                    }
                    active = tokio::time::Instant::now();
                    answered = true;
                }
            },
            () = idle, if answered && *available.borrow() => break,
            changed = available.changed(), if watching => watching = changed.is_ok(),
        }
    }
    let _ = to_local.shutdown().await;
    let _ = to_remote.shutdown().await;
}

/// Answers the local client with the reason, as JSON like the manager's errors.
async fn fail(mut local: Box<dyn Local>, status: u16, message: &str) {
    let body = serde_json::json!({ "error": message }).to_string();
    let reason = match status {
        429 => "Too Many Requests",
        503 => "Service Unavailable",
        _ => "Bad Gateway",
    };
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = local.write_all(response.as_bytes()).await;
    let _ = local.shutdown().await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncReadExt;

    /// Runs `relayed` between a fake client and a fake manager.
    fn relay_pair(
        available: &tokio::sync::watch::Sender<bool>,
    ) -> (
        tokio::io::DuplexStream,
        tokio::io::DuplexStream,
        tokio::task::JoinHandle<()>,
    ) {
        let (client, local) = tokio::io::duplex(1024);
        let (manager, remote) = tokio::io::duplex(1024);
        let task = tokio::spawn(relayed(local, remote, available.subscribe()));
        (client, manager, task)
    }

    #[tokio::test(start_paused = true)]
    async fn relayed_connections_move_direct_only_between_requests() {
        let (available, _) = tokio::sync::watch::channel(false);
        let (mut client, mut manager, task) = relay_pair(&available);
        let mut buffer = [0; 16];
        client.write_all(b"request").await.unwrap();
        manager.read_exact(&mut buffer[..7]).await.unwrap();
        manager.write_all(b"answer").await.unwrap();
        client.read_exact(&mut buffer[..6]).await.unwrap();
        // Without a direct connection, an idle connection stays.
        tokio::time::sleep(IDLE * 4).await;
        assert!(!task.is_finished());
        // With one, it closes once idle after the manager's answer.
        available.send_replace(true);
        tokio::time::sleep(IDLE + std::time::Duration::from_secs(1)).await;
        assert!(task.is_finished());
        assert_eq!(client.read(&mut buffer).await.unwrap(), 0);

        // A request still waiting for its answer is never cut.
        let (mut client, mut manager, task) = relay_pair(&available);
        client.write_all(b"slow request").await.unwrap();
        manager.read_exact(&mut buffer[..12]).await.unwrap();
        tokio::time::sleep(IDLE * 4).await;
        assert!(!task.is_finished());
        manager.write_all(b"late answer").await.unwrap();
        client.read_exact(&mut buffer[..11]).await.unwrap();
        assert_eq!(&buffer[..11], b"late answer");
    }

    const INSTALLATION: &str = "0b7d6f2e-8a51-4c43-9c1e-2f4a6b8d0e13";

    #[test]
    fn configure_keeps_one_installation_and_reports_changes() {
        let root = tempfile::tempdir().unwrap();
        let pin = "a".repeat(64);
        assert!(configure(root.path(), "https://cairn.example", INSTALLATION, &pin).unwrap());
        assert!(!configure(root.path(), "https://cairn.example/", INSTALLATION, &pin).unwrap());
        // A new manager certificate replaces the pin.
        assert!(
            configure(
                root.path(),
                "https://cairn.example",
                INSTALLATION,
                &"b".repeat(64)
            )
            .unwrap()
        );
        assert_eq!(Config::load(root.path()).unwrap().pin, "b".repeat(64));
        let other = "1b7d6f2e-8a51-4c43-9c1e-2f4a6b8d0e13";
        let refused = configure(root.path(), "https://cairn.example", other, &pin).unwrap_err();
        assert!(refused.to_string().contains("another installation"));
        for (beacon, installation, pin) in [
            ("http://cairn.example", INSTALLATION, "a".repeat(64)),
            ("https://cairn.example/path", INSTALLATION, "a".repeat(64)),
            ("https://cairn.example", "not-an-id", "a".repeat(64)),
            ("https://cairn.example", INSTALLATION, "a".repeat(63)),
        ] {
            assert!(
                configure(root.path(), beacon, installation, &pin).is_err(),
                "{beacon} {installation}"
            );
        }
    }
}
