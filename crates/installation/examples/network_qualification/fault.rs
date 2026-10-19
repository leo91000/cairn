//! Fixture-only adapter. Crypto operations and bytes are delegated unchanged.
//! The socket drops a datagram only after exact ciphertext-to-SCTP correlation.
#![allow(dead_code)] // Shared by the installation and request-direction launchers.

use rtc::crypto::*;
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    fmt,
    future::Future,
    io,
    net::SocketAddr,
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll},
    time::{Duration, Instant},
};
use webrtc::runtime::TokioRuntime;
use webrtc::runtime::{
    AsyncInterval, AsyncTcpListener, AsyncTcpStream, AsyncUdpSocket, JoinHandle, RecvMeta, Runtime,
    Transmit,
};

#[derive(Clone, Copy, Default)]
pub enum Mode {
    #[default]
    Pass,
    Recoverable,
    Starve,
    HeadersOnly,
    Opening,
}

pub struct Plan {
    pub mode: Mode,
    pub peers_created: usize,
    eligible_peer: usize,
    generation: usize,
    pub fragmented: bool,
    pub armed: bool,
    target: Option<(usize, u32)>,
    pub attempts: usize,
    pub dropped_datagrams: usize,
    pub dropped_data_chunks: usize,
    pub passed_target: usize,
    pub times_ms: Vec<u128>,
    started: Instant,
    pub errors: usize,
    pub passed_headers: usize,
    pub dropped_handshake: usize,
    pub opening_peers: usize,
    pub opening_drops: [usize; 2],
    pub dispatch: Vec<serde_json::Value>,
}

impl Default for Plan {
    fn default() -> Self {
        Self {
            peers_created: 0,
            eligible_peer: 0,
            generation: 0,
            mode: Mode::Pass,
            fragmented: false,
            armed: false,
            target: None,
            attempts: 0,
            dropped_datagrams: 0,
            dropped_data_chunks: 0,
            passed_target: 0,
            times_ms: Vec::new(),
            started: Instant::now(),
            errors: 0,
            passed_headers: 0,
            dropped_handshake: 0,
            opening_peers: 1,
            opening_drops: [0; 2],
            dispatch: Vec::new(),
        }
    }
}

impl Plan {
    pub fn arm(&mut self, mode: Mode, fragmented: bool, next_peer: bool) {
        let peers_created = self.peers_created;
        let eligible_peer = if next_peer {
            peers_created
        } else {
            peers_created
                .checked_sub(1)
                .expect("no established fixture peer")
        };
        *self = Self {
            mode,
            fragmented,
            armed: true,
            peers_created,
            eligible_peer,
            generation: self.generation + 1,
            ..Self::default()
        };
    }

    fn forwarded(&mut self, disposition: Disposition) {
        if self.generation == disposition.generation {
            self.passed_target += usize::from(disposition.target);
            self.passed_headers += disposition.headers;
        }
    }

    pub fn dispatch_event(&mut self, event: &'static str, method: &str, bytes: Option<u64>) {
        if self.dispatch.len() < 64 {
            self.dispatch.push(serde_json::json!({
                "event": event,
                "method": method,
                "elapsedMs": self.started.elapsed().as_millis(),
                "bytes": bytes,
            }));
        }
    }

    fn decide(&mut self, peer: usize, chunks: &[Data]) -> Disposition {
        let mut disposition = Disposition {
            generation: self.generation,
            ..Disposition::default()
        };
        if !self.armed || peer != self.eligible_peer {
            return disposition;
        }
        if matches!(self.mode, Mode::Starve | Mode::HeadersOnly) {
            disposition.drop = chunks
                .iter()
                .any(|chunk| !matches!(self.mode, Mode::HeadersOnly) || !chunk.headers_only);
            if disposition.drop {
                if self.target.is_none() {
                    self.target = chunks.first().map(|data| (peer, data.tsn));
                }
                if chunks
                    .iter()
                    .any(|data| Some((peer, data.tsn)) == self.target)
                {
                    self.attempts += 1;
                    if self.times_ms.len() < 16 {
                        self.times_ms.push(self.started.elapsed().as_millis());
                    }
                }
                self.dropped_datagrams += 1;
                self.dropped_data_chunks += chunks.len();
            } else {
                disposition.headers = chunks.iter().filter(|chunk| chunk.headers_only).count();
            }
            return disposition;
        }
        if !matches!(self.mode, Mode::Recoverable) {
            return disposition;
        }
        if self.target.is_none()
            && let Some(data) = chunks
                .iter()
                .find(|data| data.flags & 2 != 0 && (!self.fragmented || data.flags & 1 == 0))
        {
            self.target = Some((peer, data.tsn));
        }
        if !chunks
            .iter()
            .any(|data| Some((peer, data.tsn)) == self.target)
        {
            return disposition;
        }
        self.attempts += 1;
        if self.times_ms.len() < 16 {
            self.times_ms.push(self.started.elapsed().as_millis());
        }
        disposition.drop = self.attempts <= 2;
        if disposition.drop {
            self.dropped_datagrams += 1;
            self.dropped_data_chunks += chunks.len();
        } else {
            disposition.target = true;
        }
        disposition
    }
}

#[derive(Clone, Copy, Default)]
struct Disposition {
    drop: bool,
    target: bool,
    headers: usize,
    generation: usize,
}

#[derive(Clone)]
struct Data {
    tsn: u32,
    flags: u8,
    headers_only: bool,
}

struct Metadata {
    data: Vec<Data>,
    opening: bool,
}

fn sctp_metadata(packet: &[u8]) -> io::Result<Metadata> {
    if packet.len() < 12 {
        return Err(io::Error::other("short SCTP common header"));
    }
    let mut at = 12;
    let mut chunks = Vec::new();
    let mut opening = false;
    while at < packet.len() {
        if at + 4 > packet.len() {
            return Err(io::Error::other("short SCTP chunk"));
        }
        let size = u16::from_be_bytes([packet[at + 2], packet[at + 3]]) as usize;
        if size < 4 || at + size > packet.len() {
            return Err(io::Error::other("invalid SCTP chunk"));
        }
        if matches!(packet[at], 1 | 2) {
            if size < 20 {
                return Err(io::Error::other("short SCTP INIT exchange"));
            }
            opening = true;
        }
        if packet[at] == 0 {
            if size < 16 {
                return Err(io::Error::other("short DATA chunk"));
            }
            let ppid = u32::from_be_bytes(packet[at + 12..at + 16].try_into().unwrap());
            // DCEP (50) and SCTP controls are never application-loss proof.
            if ppid == 53 {
                if packet[at + 1] & 4 != 0 {
                    return Err(io::Error::other("unordered qualification DATA"));
                }
                let payload = &packet[at + 16..at + size];
                let headers_only = packet[at + 1] & 3 == 3
                    && payload.len() >= 13
                    && payload[0] == 1
                    && payload[9..13] == [0, 0, 0, 0]
                    && matches!(serde_json::from_slice::<cairn_protocol::Frame>(&payload[13..]), Ok(cairn_protocol::Frame::StreamStart(response)) if response.status == 200);
                chunks.push(Data {
                    headers_only,
                    tsn: u32::from_be_bytes(packet[at + 4..at + 8].try_into().unwrap()),
                    flags: packet[at + 1],
                });
            }
        }
        at += (size + 3) & !3;
    }
    if at != packet.len() {
        return Err(io::Error::other("invalid SCTP padding"));
    }
    Ok(Metadata {
        data: chunks,
        opening,
    })
}

struct PeerState {
    index: usize,
    plan: Arc<Mutex<Plan>>,
    sealed: Mutex<HashMap<[u8; 32], Metadata>>,
}

impl PeerState {
    fn register(&self, plaintext: &[u8], ciphertext: &[u8], tag: &[u8]) {
        let Ok(chunks) = sctp_metadata(plaintext) else {
            self.plan.lock().unwrap().errors += 1;
            return;
        };
        let mut sealed = self.sealed.lock().unwrap();
        if sealed.len() >= 1024 {
            self.plan.lock().unwrap().errors += 1;
            return;
        }
        let digest = record_digest(ciphertext, tag);
        sealed.insert(digest, chunks);
    }

    fn drop_datagram(&self, packet: &[u8]) -> io::Result<Disposition> {
        // ICE/STUN and DTLS handshake records remain intact; opening plans
        // suppress only a correlated SCTP INIT/INIT-ACK, after authentication.
        if !packet.first().is_some_and(|kind| (20..=25).contains(kind)) {
            return Ok(Disposition::default());
        }
        let mut at = 0;
        let mut chunks = Vec::new();
        let mut opening = false;
        while at < packet.len() {
            if at + 13 > packet.len() {
                return Err(io::Error::other("short DTLS record"));
            }
            let size = u16::from_be_bytes([packet[at + 11], packet[at + 12]]) as usize;
            if at + 13 + size > packet.len() {
                return Err(io::Error::other("short DTLS payload"));
            }
            if packet[at] == 23 {
                // Both default AEADs append a 16-byte tag. The explicit nonce,
                // when present, precedes the ciphertext and is not hashed.
                let payload = &packet[at + 13..at + 13 + size];
                let mut sealed = self.sealed.lock().unwrap();
                let mut found = None;
                for nonce_size in [0, 8] {
                    if payload.len() >= nonce_size + 16 {
                        let (ciphertext, tag) =
                            payload[nonce_size..].split_at(payload.len() - nonce_size - 16);
                        if let Some(metadata) = sealed.remove(&record_digest(ciphertext, tag)) {
                            found = Some(metadata);
                            break;
                        }
                    }
                }
                let metadata = found
                    .ok_or_else(|| io::Error::other("unobserved encrypted application record"))?;
                opening |= metadata.opening;
                chunks.extend(metadata.data);
            }
            at += 13 + size;
        }
        let mut plan = self.plan.lock().unwrap();
        if plan.armed
            && opening
            && self.index >= plan.eligible_peer
            && self.index < plan.eligible_peer + plan.opening_peers
            && matches!(plan.mode, Mode::Opening)
        {
            plan.dropped_handshake += 1;
            let ordinal = self.index - plan.eligible_peer;
            plan.opening_drops[ordinal] += 1;
            return Ok(Disposition {
                drop: true,
                ..Disposition::default()
            });
        }
        Ok(plan.decide(self.index, &chunks))
    }
}

fn record_digest(ciphertext: &[u8], tag: &[u8]) -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update(ciphertext);
    hash.update(tag);
    hash.finalize().into()
}

struct Provider {
    inner: Arc<dyn RTCCryptoProvider>,
    state: Arc<PeerState>,
}

impl RTCCryptoProvider for Provider {
    fn name(&self) -> &'static str {
        "qualification-observer"
    }

    fn crypto(&self) -> &dyn RTCCrypto {
        self
    }

    fn random(&self) -> &dyn RTCRandom {
        self.inner.random()
    }
}

impl RTCCrypto for Provider {
    fn supports(&self, a: CryptoAlgorithm) -> bool {
        self.inner.crypto().supports(a)
    }

    fn hash(&self, a: HashAlgorithm, data: &[u8]) -> Result<Vec<u8>, CryptoError> {
        self.inner.crypto().hash(a, data)
    }

    fn block_encrypt(
        &self,
        a: BlockCipherAlgorithm,
        key: &[u8],
        block: &mut [u8],
    ) -> Result<(), CryptoError> {
        self.inner.crypto().block_encrypt(a, key, block)
    }

    fn new_hmac(&self, a: HmacAlgorithm, key: &[u8]) -> Result<Box<dyn Mac>, CryptoError> {
        self.inner.crypto().new_hmac(a, key)
    }

    fn new_stream_cipher(
        &self,
        a: StreamCipherAlgorithm,
        key: &[u8],
    ) -> Result<Box<dyn StreamCipher>, CryptoError> {
        self.inner.crypto().new_stream_cipher(a, key)
    }

    fn new_cbc(&self, a: CbcAlgorithm, key: &[u8]) -> Result<Box<dyn CbcCipher>, CryptoError> {
        self.inner.crypto().new_cbc(a, key)
    }

    fn start_key_exchange(
        &self,
        a: KeyExchangeAlgorithm,
    ) -> Result<Box<dyn ActiveKeyExchange>, CryptoError> {
        self.inner.crypto().start_key_exchange(a)
    }

    fn generate_signing_key(&self, a: SignatureScheme) -> Result<Arc<dyn SigningKey>, CryptoError> {
        self.inner.crypto().generate_signing_key(a)
    }

    fn import_signing_key(
        &self,
        a: SignatureScheme,
        key: &[u8],
    ) -> Result<Arc<dyn SigningKey>, CryptoError> {
        self.inner.crypto().import_signing_key(a, key)
    }

    fn verify_signature(
        &self,
        a: SignatureScheme,
        key: PublicKey<'_>,
        message: &[u8],
        signature: &[u8],
    ) -> Result<(), CryptoError> {
        self.inner
            .crypto()
            .verify_signature(a, key, message, signature)
    }

    fn new_aead(&self, a: AeadAlgorithm, key: &[u8]) -> Result<Box<dyn AeadCipher>, CryptoError> {
        Ok(Box::new(Cipher {
            inner: self.inner.crypto().new_aead(a, key)?,
            state: self.state.clone(),
        }))
    }
}

struct Cipher {
    inner: Box<dyn AeadCipher>,
    state: Arc<PeerState>,
}

impl AeadCipher for Cipher {
    fn tag_len(&self) -> usize {
        self.inner.tag_len()
    }

    fn seal_in_place(
        &mut self,
        nonce: &[u8],
        aad: &[u8],
        data: &mut [u8],
        tag: &mut [u8],
    ) -> Result<(), CryptoError> {
        let plaintext = (aad.len() == 13 && aad[8] == 23).then(|| data.to_vec());
        self.inner.seal_in_place(nonce, aad, data, tag)?;
        if let Some(plaintext) = plaintext {
            self.state.register(&plaintext, data, tag);
        }
        Ok(())
    }

    fn open_in_place(
        &mut self,
        nonce: &[u8],
        aad: &[u8],
        data: &mut [u8],
        tag: &[u8],
    ) -> Result<(), CryptoError> {
        self.inner.open_in_place(nonce, aad, data, tag)
    }
}

struct FaultRuntime {
    inner: TokioRuntime,
    state: Arc<PeerState>,
}

impl fmt::Debug for FaultRuntime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("QualificationRuntime")
    }
}

impl Runtime for FaultRuntime {
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
        "qualification-runtime"
    }

    fn wrap_udp_socket(&self, socket: std::net::UdpSocket) -> io::Result<Arc<dyn AsyncUdpSocket>> {
        Ok(Arc::new(FaultSocket {
            inner: self.inner.wrap_udp_socket(socket)?,
            state: self.state.clone(),
            pending: Mutex::default(),
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

struct FaultSocket {
    inner: Arc<dyn AsyncUdpSocket>,
    state: Arc<PeerState>,
    pending: Mutex<Option<([u8; 32], Disposition)>>,
}

impl fmt::Debug for FaultSocket {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("QualificationSocket")
    }
}

impl AsyncUdpSocket for FaultSocket {
    fn local_addr(&self) -> io::Result<SocketAddr> {
        self.inner.local_addr()
    }

    fn poll_send(&self, cx: &mut Context<'_>, transmit: &Transmit<'_>) -> Poll<io::Result<usize>> {
        if transmit
            .segment_size
            .is_some_and(|size| size < transmit.contents.len())
        {
            self.state.plan.lock().unwrap().errors += 1;
            return Poll::Ready(Err(io::Error::other(
                "unexpected segmented qualification send",
            )));
        }
        let digest = record_digest(transmit.contents, &[]);
        let mut pending = self.pending.lock().unwrap();
        let disposition = if let Some((previous, disposition)) = *pending {
            if previous != digest {
                self.state.plan.lock().unwrap().errors += 1;
                return Poll::Ready(Err(io::Error::other(
                    "qualification send changed while pending",
                )));
            }
            disposition
        } else {
            match self.state.drop_datagram(transmit.contents) {
                Ok(disposition) if disposition.drop => {
                    return Poll::Ready(Ok(transmit.contents.len()));
                }
                Ok(disposition) => disposition,
                Err(error) => {
                    self.state.plan.lock().unwrap().errors += 1;
                    return Poll::Ready(Err(error));
                }
            }
        };
        match self.inner.poll_send(cx, transmit) {
            Poll::Pending => {
                *pending = Some((digest, disposition));
                Poll::Pending
            }
            Poll::Ready(result) => {
                *pending = None;
                if matches!(result, Ok(bytes) if bytes == transmit.contents.len()) {
                    self.state.plan.lock().unwrap().forwarded(disposition);
                } else {
                    self.state.plan.lock().unwrap().errors += 1;
                }
                Poll::Ready(result)
            }
        }
    }

    fn poll_recv(
        &self,
        cx: &mut Context<'_>,
        bufs: &mut [io::IoSliceMut<'_>],
        meta: &mut [RecvMeta],
    ) -> Poll<io::Result<usize>> {
        self.inner.poll_recv(cx, bufs, meta)
    }

    fn max_gso_segments(&self) -> usize {
        1
    }

    fn max_gro_segments(&self) -> usize {
        self.inner.max_gro_segments()
    }
}

pub fn dependencies(
    index: usize,
    plan: Arc<Mutex<Plan>>,
) -> cairn_installation::direct::peer::PeerDependencies {
    plan.lock().unwrap().peers_created = index + 1;
    let state = Arc::new(PeerState {
        index,
        plan,
        sealed: Mutex::default(),
    });
    cairn_installation::direct::peer::PeerDependencies {
        runtime: Arc::new(FaultRuntime {
            inner: TokioRuntime,
            state: state.clone(),
        }),
        crypto: Arc::new(Provider {
            inner: default_provider().expect("pinned crypto provider"),
            state,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn packet(ppid: u32) -> Vec<u8> {
        let mut packet = vec![0; 12];
        packet.extend([0, 2]); // DATA, ordered beginning fragment.
        packet.extend(20_u16.to_be_bytes());
        packet.extend(42_u32.to_be_bytes());
        packet.extend([0; 4]);
        packet.extend(ppid.to_be_bytes());
        packet.extend([7; 4]);
        packet
    }

    fn seal(state: &Arc<PeerState>, plaintext: &[u8], sequence: u8) -> Vec<u8> {
        let inner = default_provider().unwrap();
        let provider = Provider {
            inner: inner.clone(),
            state: state.clone(),
        };
        let mut wrapped = provider
            .crypto()
            .new_aead(AeadAlgorithm::Aes128Gcm, &[9; 16])
            .unwrap();
        let mut reference = inner
            .crypto()
            .new_aead(AeadAlgorithm::Aes128Gcm, &[9; 16])
            .unwrap();
        let nonce = [sequence; 12];
        let mut aad = [0; 13];
        aad[8] = 23;
        let mut observed = plaintext.to_vec();
        let mut expected = plaintext.to_vec();
        let mut tag = [0; 16];
        let mut expected_tag = [0; 16];
        wrapped
            .seal_in_place(&nonce, &aad, &mut observed, &mut tag)
            .unwrap();
        reference
            .seal_in_place(&nonce, &aad, &mut expected, &mut expected_tag)
            .unwrap();
        assert_eq!(observed, expected, "fixture must preserve ciphertext");
        assert_eq!(
            tag, expected_tag,
            "fixture must preserve authentication tag"
        );
        let mut record = vec![23, 0xfe, 0xfd, 0, 1, 0, 0, 0, 0, 0, sequence];
        record.extend(((8 + observed.len() + 16) as u16).to_be_bytes());
        record.extend(&nonce[4..]);
        record.extend(observed);
        record.extend(tag);
        record
    }

    #[test]
    fn correlated_real_aead_records_drop_two_data_attempts_then_forward() {
        let plan = Arc::new(Mutex::new(Plan::default()));
        plan.lock().unwrap().arm(Mode::Recoverable, true, true);
        let state = Arc::new(PeerState {
            index: 0,
            plan: plan.clone(),
            sealed: Mutex::default(),
        });
        for sequence in 0..3 {
            let record = seal(&state, &packet(53), sequence);
            let disposition = state.drop_datagram(&record).unwrap();
            assert_eq!(disposition.drop, sequence < 2);
            if !disposition.drop {
                plan.lock().unwrap().forwarded(disposition);
            }
        }
        let plan = plan.lock().unwrap();
        assert_eq!(plan.dropped_datagrams, 2);
        assert_eq!(plan.dropped_data_chunks, 2);
        assert_eq!(plan.passed_target, 1);
        assert_eq!(plan.errors, 0);
    }

    #[test]
    fn dcep_and_sctp_control_are_not_data_loss_evidence() {
        let plan = Arc::new(Mutex::new(Plan::default()));
        plan.lock().unwrap().arm(Mode::Starve, false, true);
        let state = Arc::new(PeerState {
            index: 0,
            plan: plan.clone(),
            sealed: Mutex::default(),
        });
        let dcep = seal(&state, &packet(50), 0);
        assert!(!state.drop_datagram(&dcep).unwrap().drop);
        assert_eq!(plan.lock().unwrap().dropped_data_chunks, 0);
    }

    #[test]
    fn unobserved_application_record_is_refused() {
        let state = Arc::new(PeerState {
            index: 0,
            plan: Arc::default(),
            sealed: Mutex::default(),
        });
        let record = seal(&state, &packet(53), 0);
        state.sealed.lock().unwrap().clear();
        assert!(state.drop_datagram(&record).is_err());
    }

    #[test]
    fn opening_plan_requires_correlated_sctp_init_for_both_selected_peers() {
        let plan = Arc::new(Mutex::new(Plan::default()));
        plan.lock().unwrap().arm(Mode::Opening, false, true);
        plan.lock().unwrap().opening_peers = 2;
        let mut init = vec![0; 12];
        init.extend([1, 0, 0, 20]);
        init.extend([0; 16]);
        for index in 0..3 {
            let state = Arc::new(PeerState {
                index,
                plan: plan.clone(),
                sealed: Mutex::default(),
            });
            let handshake = [22, 0xfe, 0xfd, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
            assert!(!state.drop_datagram(&handshake).unwrap().drop);
            let record = seal(&state, &init, index as u8);
            assert_eq!(state.drop_datagram(&record).unwrap().drop, index < 2);
        }
        let plan = plan.lock().unwrap();
        assert_eq!(plan.opening_drops, [1, 1]);
        assert_eq!(plan.dropped_handshake, 2);
        assert_eq!(plan.dropped_data_chunks, 0);
        assert_eq!(plan.errors, 0);
    }

    #[derive(Debug)]
    struct PendingSocket(std::sync::atomic::AtomicUsize);

    impl AsyncUdpSocket for PendingSocket {
        fn local_addr(&self) -> io::Result<SocketAddr> {
            Ok(SocketAddr::from(([127, 0, 0, 1], 5000)))
        }

        fn poll_send(
            &self,
            cx: &mut Context<'_>,
            transmit: &Transmit<'_>,
        ) -> Poll<io::Result<usize>> {
            if self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed) == 0 {
                cx.waker().wake_by_ref();
                Poll::Pending
            } else {
                Poll::Ready(Ok(transmit.contents.len()))
            }
        }

        fn poll_recv(
            &self,
            _: &mut Context<'_>,
            _: &mut [io::IoSliceMut<'_>],
            _: &mut [RecvMeta],
        ) -> Poll<io::Result<usize>> {
            Poll::Pending
        }
    }

    #[test]
    fn forwarded_proof_waits_for_complete_runtime_send_after_pending() {
        let plan = Arc::new(Mutex::new(Plan::default()));
        plan.lock().unwrap().arm(Mode::Recoverable, true, true);
        let state = Arc::new(PeerState {
            index: 0,
            plan: plan.clone(),
            sealed: Mutex::default(),
        });
        let socket = FaultSocket {
            inner: Arc::new(PendingSocket(std::sync::atomic::AtomicUsize::new(0))),
            state: state.clone(),
            pending: Mutex::default(),
        };
        let mut context = Context::from_waker(std::task::Waker::noop());
        for sequence in 0..2 {
            let bytes = seal(&state, &packet(53), sequence);
            let transmit = Transmit {
                destination: SocketAddr::from(([127, 0, 0, 1], 5001)),
                ecn: None,
                contents: &bytes,
                segment_size: None,
                src_ip: None,
            };
            assert!(matches!(
                socket.poll_send(&mut context, &transmit),
                Poll::Ready(Ok(_))
            ));
        }
        let bytes = seal(&state, &packet(53), 2);
        let transmit = Transmit {
            destination: SocketAddr::from(([127, 0, 0, 1], 5001)),
            ecn: None,
            contents: &bytes,
            segment_size: None,
            src_ip: None,
        };
        assert!(socket.poll_send(&mut context, &transmit).is_pending());
        assert_eq!(plan.lock().unwrap().passed_target, 0);
        assert!(matches!(
            socket.poll_send(&mut context, &transmit),
            Poll::Ready(Ok(_))
        ));
        let plan = plan.lock().unwrap();
        assert_eq!(plan.passed_target, 1);
        assert_eq!(
            plan.attempts, 3,
            "a pending poll must not consume metadata twice"
        );
        assert_eq!(plan.dropped_datagrams, 2);
        assert_eq!(plan.errors, 0);
    }

    #[test]
    fn a_different_peer_cannot_satisfy_the_armed_plan() {
        let mut plan = Plan {
            peers_created: 1,
            ..Plan::default()
        };
        plan.arm(Mode::Recoverable, true, true);
        let fragment = Data {
            tsn: 1,
            flags: 2,
            headers_only: false,
        };
        assert!(!plan.decide(0, std::slice::from_ref(&fragment)).drop);
        assert_eq!(plan.attempts, 0);
        assert!(plan.decide(1, &[fragment]).drop);
        assert_eq!(plan.attempts, 1);
    }
}
