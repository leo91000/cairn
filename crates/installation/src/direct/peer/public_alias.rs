//! Announce explicit public aliases only after the kernel accepts a real ICE
//! check from the candidate's socket to an eligible remote destination.
use super::{DirectConnections, DirectSignal};
use rtc::{
    ice::candidate::{CandidateType, unmarshal_candidate},
    stun::{
        attributes::{
            ATTR_ICE_CONTROLLED, ATTR_ICE_CONTROLLING, ATTR_MESSAGE_INTEGRITY, ATTR_USERNAME,
        },
        message::Message,
    },
};
use std::{
    collections::{HashMap, HashSet},
    fmt,
    future::Future,
    io::{self, IoSliceMut},
    net::{IpAddr, SocketAddr},
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll},
    time::{Duration, Instant},
};
use tokio_util::sync::CancellationToken;
use webrtc::runtime::{
    AsyncInterval, AsyncTcpListener, AsyncTcpStream, AsyncUdpSocket, JoinHandle, RecvMeta, Runtime,
    TokioRuntime, Transmit,
};

const ALIAS_WAIT: Duration = Duration::from_secs(5);

#[derive(Default)]
struct AliasState {
    remote: HashSet<SocketAddr>,
    checked: HashSet<SocketAddr>,
    pending: HashMap<SocketAddr, DirectSignal>,
    announced: HashSet<SocketAddr>,
}

pub(super) struct PublicAliases {
    public: Option<IpAddr>,
    deadline: Instant,
    state: Mutex<AliasState>,
    id: String,
    direct: DirectConnections,
    failed: CancellationToken,
}

impl fmt::Debug for PublicAliases {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PublicAliases").finish_non_exhaustive()
    }
}

impl PublicAliases {
    pub(super) fn new(
        public: Option<IpAddr>,
        id: String,
        direct: DirectConnections,
        failed: CancellationToken,
    ) -> Self {
        Self {
            public,
            deadline: Instant::now() + ALIAS_WAIT,
            state: Mutex::new(AliasState::default()),
            id,
            direct,
            failed,
        }
    }

    pub(super) fn remote_candidate(&self, raw: &str) {
        if self.public.is_none() || Instant::now() >= self.deadline {
            return;
        }
        let raw = raw.strip_prefix("candidate:").unwrap_or(raw);
        let Ok(candidate) = unmarshal_candidate(raw) else {
            return;
        };
        let Ok(address) = candidate.address().parse::<IpAddr>() else {
            return;
        };
        // A check to an unroutable private host must not release the alias
        // before the client's actual public mapping arrives in trickle ICE.
        let private = match address {
            IpAddr::V4(ip) => ip.is_private(),
            IpAddr::V6(ip) => ip.is_unique_local(),
        };
        if candidate.component() != 1
            || !candidate.network_type().is_udp()
            || !matches!(
                candidate.candidate_type(),
                CandidateType::Host | CandidateType::ServerReflexive
            )
            || !cairn_protocol::direct::usable_candidate_address(address)
            || private
            || candidate.port() == 0
        {
            return;
        }
        self.state
            .lock()
            .unwrap()
            .remote
            .insert(SocketAddr::new(address, candidate.port()));
    }

    pub(super) fn host_candidate(&self, local: SocketAddr) {
        let Some(public) = self.public else { return };
        if Instant::now() >= self.deadline
            || local.is_ipv4() != public.is_ipv4()
            || local.ip().is_unspecified()
            || local.ip().is_loopback()
        {
            return;
        }
        let mut state = self.state.lock().unwrap();
        if state.announced.contains(&local) {
            return;
        }
        let port = local.port();
        state.pending.insert(local, DirectSignal::Candidate {
            candidate: format!("candidate:cairn-public 1 udp 1694498815 {public} {port} typ srflx raddr {} rport {port}", local.ip()),
            sdp_mid: Some("0".into()),
            sdp_m_line_index: Some(0),
        });
        self.announce(&mut state, local);
    }

    fn sent_check(&self, local: SocketAddr, transmit: &Transmit<'_>) {
        // Binding requests to STUN discovery servers, responses and application
        // datagrams cannot satisfy this gate. ICE destinations come only from
        // this authorized negotiation's numeric remote candidates.
        let packet = transmit.contents;
        if Instant::now() >= self.deadline
            || packet.len() < 20
            || packet[..2] != [0, 1]
            || packet[4..8] != [0x21, 0x12, 0xa4, 0x42]
            || local.is_ipv4() != transmit.destination.is_ipv4()
        {
            return;
        }
        let mut state = self.state.lock().unwrap();
        if state.remote.contains(&transmit.destination) {
            let mut message = Message::default();
            if message.unmarshal_binary(packet).is_err()
                || !message.contains(ATTR_USERNAME)
                || !message.contains(ATTR_MESSAGE_INTEGRITY)
                || !(message.contains(ATTR_ICE_CONTROLLED)
                    || message.contains(ATTR_ICE_CONTROLLING))
            {
                return;
            }
            state.checked.insert(local);
            self.announce(&mut state, local);
        }
    }

    fn announce(&self, state: &mut AliasState, local: SocketAddr) {
        if state.checked.contains(&local)
            && let Some(signal) = state.pending.remove(&local)
        {
            state.announced.insert(local);
            if self.direct.send_signal(&self.id, signal).is_err() {
                self.failed.cancel();
            }
        }
    }
}

/// Use the library's runtime/socket interface to observe successful sends,
/// without replacing ICE or changing the socket's bind, packet or destination.
#[derive(Debug)]
pub(super) struct AliasRuntime {
    inner: TokioRuntime,
    aliases: Arc<PublicAliases>,
}

impl AliasRuntime {
    pub(super) fn new(aliases: Arc<PublicAliases>) -> Self {
        Self {
            inner: TokioRuntime,
            aliases,
        }
    }
}

impl Runtime for AliasRuntime {
    fn spawn(&self, future: Pin<Box<dyn Future<Output = ()> + Send>>) -> Box<dyn JoinHandle> {
        self.inner.spawn(future)
    }

    fn spawn_reactor(
        &self,
        size: usize,
        future: Pin<Box<dyn Future<Output = ()> + Send>>,
    ) -> Box<dyn JoinHandle> {
        self.inner.spawn_reactor(size, future)
    }

    fn now(&self) -> Instant {
        self.inner.now()
    }

    fn yield_now(&self) -> Pin<Box<dyn Future<Output = ()> + Send + 'static>> {
        self.inner.yield_now()
    }

    fn name(&self) -> &'static str {
        self.inner.name()
    }

    fn wrap_udp_socket(&self, socket: std::net::UdpSocket) -> io::Result<Arc<dyn AsyncUdpSocket>> {
        let local = socket.local_addr()?;
        Ok(Arc::new(AliasSocket {
            inner: self.inner.wrap_udp_socket(socket)?,
            local,
            aliases: self.aliases.clone(),
        }))
    }

    fn wrap_tcp_listener(
        &self,
        listener: std::net::TcpListener,
    ) -> io::Result<Arc<dyn AsyncTcpListener>> {
        self.inner.wrap_tcp_listener(listener)
    }

    fn connect_tcp<'a>(
        &'a self,
        address: SocketAddr,
    ) -> Pin<Box<dyn Future<Output = io::Result<Arc<dyn AsyncTcpStream>>> + Send + 'a>> {
        self.inner.connect_tcp(address)
    }

    fn resolve_host<'a>(
        &'a self,
        host: &'a str,
    ) -> Pin<Box<dyn Future<Output = io::Result<Vec<SocketAddr>>> + Send + 'a>> {
        self.inner.resolve_host(host)
    }

    fn sleep(&self, duration: Duration) -> Pin<Box<dyn Future<Output = ()> + Send + 'static>> {
        self.inner.sleep(duration)
    }

    fn interval(&self, period: Duration) -> Box<dyn AsyncInterval> {
        self.inner.interval(period)
    }

    fn block_on(&self, future: Pin<Box<dyn Future<Output = ()> + '_>>) {
        self.inner.block_on(future);
    }
}

#[derive(Debug)]
struct AliasSocket {
    inner: Arc<dyn AsyncUdpSocket>,
    local: SocketAddr,
    aliases: Arc<PublicAliases>,
}

impl AsyncUdpSocket for AliasSocket {
    fn local_addr(&self) -> io::Result<SocketAddr> {
        self.inner.local_addr()
    }

    fn poll_send(&self, cx: &mut Context<'_>, transmit: &Transmit<'_>) -> Poll<io::Result<usize>> {
        let result = self.inner.poll_send(cx, transmit);
        if matches!(result, Poll::Ready(Ok(bytes)) if bytes == transmit.contents.len()) {
            self.aliases.sent_check(self.local, transmit);
        }
        result
    }

    fn poll_recv(
        &self,
        cx: &mut Context<'_>,
        bufs: &mut [IoSliceMut<'_>],
        meta: &mut [RecvMeta],
    ) -> Poll<io::Result<usize>> {
        self.inner.poll_recv(cx, bufs, meta)
    }

    fn max_gso_segments(&self) -> usize {
        self.inner.max_gso_segments()
    }

    fn max_gro_segments(&self) -> usize {
        self.inner.max_gro_segments()
    }
}
