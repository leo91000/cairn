//! Delay a real client's first DTLS flight without delaying ICE or changing peers.
use std::{
    future::Future,
    io::{self, IoSliceMut},
    net::SocketAddr,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    task::{Context, Poll},
    time::{Duration, Instant},
};
use webrtc::runtime::{
    AsyncInterval, AsyncTcpListener, AsyncTcpStream, AsyncUdpSocket, JoinHandle, RecvMeta, Runtime,
    TokioRuntime, Transmit,
};

#[derive(Debug)]
pub(super) struct DelayedDtlsRuntime {
    delay: Duration,
    flight: Arc<Mutex<Option<Pin<Box<tokio::time::Sleep>>>>>,
    sent: Arc<AtomicBool>,
}

impl DelayedDtlsRuntime {
    pub(super) fn new(delay: Duration) -> Self {
        Self {
            delay,
            flight: Arc::new(Mutex::new(None)),
            sent: Arc::new(AtomicBool::new(false)),
        }
    }

    pub(super) fn delayed_flight_sent(&self) -> bool {
        self.sent.load(Ordering::SeqCst)
    }
}

impl Runtime for DelayedDtlsRuntime {
    fn spawn(&self, future: Pin<Box<dyn Future<Output = ()> + Send>>) -> Box<dyn JoinHandle> {
        TokioRuntime.spawn(future)
    }

    fn spawn_reactor(
        &self,
        size: usize,
        future: Pin<Box<dyn Future<Output = ()> + Send>>,
    ) -> Box<dyn JoinHandle> {
        TokioRuntime.spawn_reactor(size, future)
    }

    fn now(&self) -> Instant {
        TokioRuntime.now()
    }

    fn yield_now(&self) -> Pin<Box<dyn Future<Output = ()> + Send + 'static>> {
        TokioRuntime.yield_now()
    }

    fn name(&self) -> &'static str {
        TokioRuntime.name()
    }

    fn wrap_udp_socket(&self, socket: std::net::UdpSocket) -> io::Result<Arc<dyn AsyncUdpSocket>> {
        Ok(Arc::new(DelayedDtlsSocket {
            inner: TokioRuntime.wrap_udp_socket(socket)?,
            delay: self.delay,
            flight: self.flight.clone(),
            sent: self.sent.clone(),
        }))
    }

    fn wrap_tcp_listener(
        &self,
        listener: std::net::TcpListener,
    ) -> io::Result<Arc<dyn AsyncTcpListener>> {
        TokioRuntime.wrap_tcp_listener(listener)
    }

    fn connect_tcp<'a>(
        &'a self,
        address: SocketAddr,
    ) -> Pin<Box<dyn Future<Output = io::Result<Arc<dyn AsyncTcpStream>>> + Send + 'a>> {
        TokioRuntime.connect_tcp(address)
    }

    fn resolve_host<'a>(
        &'a self,
        host: &'a str,
    ) -> Pin<Box<dyn Future<Output = io::Result<Vec<SocketAddr>>> + Send + 'a>> {
        TokioRuntime.resolve_host(host)
    }

    fn sleep(&self, duration: Duration) -> Pin<Box<dyn Future<Output = ()> + Send + 'static>> {
        TokioRuntime.sleep(duration)
    }

    fn interval(&self, period: Duration) -> Box<dyn AsyncInterval> {
        TokioRuntime.interval(period)
    }

    fn block_on(&self, future: Pin<Box<dyn Future<Output = ()> + '_>>) {
        TokioRuntime.block_on(future);
    }
}

#[derive(Debug)]
struct DelayedDtlsSocket {
    inner: Arc<dyn AsyncUdpSocket>,
    delay: Duration,
    flight: Arc<Mutex<Option<Pin<Box<tokio::time::Sleep>>>>>,
    sent: Arc<AtomicBool>,
}

impl AsyncUdpSocket for DelayedDtlsSocket {
    fn local_addr(&self) -> io::Result<SocketAddr> {
        self.inner.local_addr()
    }

    fn poll_send(&self, cx: &mut Context<'_>, transmit: &Transmit<'_>) -> Poll<io::Result<usize>> {
        // DTLS handshake records have content type 22. STUN checks pass through,
        // so this gate delays a real handshake flight after ICE has progressed.
        let handshake = transmit.contents.first() == Some(&22);
        if handshake {
            let mut flight = self.flight.lock().unwrap();
            let delay = flight.get_or_insert_with(|| Box::pin(tokio::time::sleep(self.delay)));
            if delay.as_mut().poll(cx).is_pending() {
                return Poll::Pending;
            }
        }

        let result = self.inner.poll_send(cx, transmit);
        if handshake && matches!(result, Poll::Ready(Ok(bytes)) if bytes == transmit.contents.len())
        {
            self.sent.store(true, Ordering::SeqCst);
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
