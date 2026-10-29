//! Direct node connections. Beacon forwards a node's offer and this answer;
//! after that the node and the manager talk over UDP without Beacon. DTLS ends
//! on the manager's node certificate, which the node checks against its pin.
//! Each DataChannel the node opens carries one HTTP connection to node routes.
use super::{NodeCertificate, serve_connection};
use crate::{
    direct::DirectConnections,
    error::{Error, Result},
};
use axum::Router;
use cairn_protocol::nodes::{CONTROL_CHANNEL, DATA_CHANNEL, MAX_PEERS, MAX_SDP};
use rtc::{
    ice::mdns::MulticastDnsMode,
    peer_connection::configuration::setting_engine::{SctpMaxMessageSize, SettingEngineBuilder},
};
use std::{
    net::{IpAddr, SocketAddr},
    path::PathBuf,
    sync::Arc,
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::{Notify, Semaphore, mpsc},
};
use tokio_util::sync::CancellationToken;
use webrtc::{
    data_channel::{DataChannel, DataChannelEvent},
    peer_connection::{
        PeerConnection, PeerConnectionBuilder, PeerConnectionEventHandler, RTCConfigurationBuilder,
        RTCIceGatheringState, RTCIceServer, RTCPeerConnectionState, RTCSessionDescription,
    },
};

// WebRTC delivers binary messages of at most this size.
const MESSAGE: usize = 16_384;
const BUFFER: usize = 256 * 1024;
// A disk transfer needs a wide window: 4 MiB covers 80 MB/s at 50 ms.
const RECEIVE_WINDOW: u32 = 4 << 20;
const SEND_BUFFER: usize = 1 << 20;
const GATHERING: Duration = Duration::from_secs(5);
const CHANNELS_PER_PEER: usize = cairn_protocol::MAX_NODE_CHANNELS;

#[derive(Clone)]
pub(crate) struct NodePeers {
    router: Router,
    directory: PathBuf,
    direct: DirectConnections,
    stun_urls: Vec<String>,
    enabled: bool,
    slots: Arc<Semaphore>,
    stop: CancellationToken,
}

impl NodePeers {
    pub(crate) fn new(
        router: Router,
        directory: PathBuf,
        direct: DirectConnections,
        stop: CancellationToken,
    ) -> Self {
        // The same switch and STUN servers as direct connections of the app.
        let config = crate::direct::peer::PeerConfig::load().ok();
        Self {
            router,
            directory,
            direct,
            enabled: config.as_ref().is_some_and(|config| config.enabled),
            stun_urls: config.map(|config| config.stun_urls).unwrap_or_default(),
            slots: Arc::new(Semaphore::new(MAX_PEERS)),
            stop,
        }
    }

    pub(crate) async fn certificate(&self) -> Result<Arc<NodeCertificate>> {
        NodeCertificate::load(&self.directory).await
    }

    /// The complete answer to `offer`, or `None` so the node uses Beacon.
    pub(crate) async fn answer(&self, offer: String, address: Option<IpAddr>) -> Option<String> {
        if !self.enabled || offer.len() > MAX_SDP {
            return None;
        }
        let permit = self.slots.clone().try_acquire_owned().ok()?;
        let peer = SocketAddr::new(address.unwrap_or(IpAddr::from([0, 0, 0, 0])), 0);
        match self.negotiate(offer).await {
            Ok((answer, connection, events)) => {
                let router = self.router.clone();
                let stop = self.stop.clone();
                tokio::spawn(async move {
                    let _permit = permit;
                    serve_peer(connection, events, router, peer, stop).await;
                });
                Some(answer)
            }
            Err(error) => {
                tracing::debug!(reason = %error.message, "Direct node offer refused");
                None
            }
        }
    }

    async fn negotiate(&self, offer: String) -> Result<(String, Arc<dyn PeerConnection>, Events)> {
        let certificate = NodeCertificate::load(&self.directory).await?.webrtc()?;
        let interfaces = rtc::shared::ifaces::ifaces().map_err(Error::internal)?;
        let (udp_addrs, network_types): (Vec<_>, Vec<_>) = crate::direct::peer::udp_bindings(
            interfaces
                .into_iter()
                .filter_map(|interface| interface.addr.map(|address| address.ip())),
        )
        .into_iter()
        .unzip();
        if udp_addrs.is_empty() {
            return Err(Error::unavailable("No usable network interface."));
        }
        let (channels, incoming) = mpsc::channel(CHANNELS_PER_PEER);
        let events = Events {
            channels: incoming,
            closed: CancellationToken::new(),
        };
        let gathered = Arc::new(Notify::new());
        let handler = Arc::new(Handler {
            channels,
            gathered: gathered.clone(),
            closed: events.closed.clone(),
        });
        let settings = SettingEngineBuilder::new()
            .with_network_types(network_types)
            .with_multicast_dns_mode(MulticastDnsMode::Disabled)
            .with_sctp_max_message_size(SctpMaxMessageSize::Bounded(MESSAGE as u32))
            .with_sctp_max_receive_buffer_size(RECEIVE_WINDOW);
        let stun_urls = if self.stun_urls.is_empty() {
            self.direct.stun_urls()
        } else {
            self.stun_urls.clone()
        };
        let connection: Arc<dyn PeerConnection> = Arc::new(
            PeerConnectionBuilder::new()
                .with_configuration(
                    RTCConfigurationBuilder::default()
                        .with_ice_servers(vec![RTCIceServer {
                            urls: stun_urls,
                            ..Default::default()
                        }])
                        .with_certificates(vec![certificate])
                        .build(),
                )
                .with_setting_engine(settings.build())
                .with_handler(handler)
                .with_udp_addrs(udp_addrs)
                .with_data_channel_send_buffer_limit(SEND_BUFFER)
                .build()
                .await
                .map_err(Error::internal)?,
        );
        let answer = async {
            let gathering = gathered.notified();
            tokio::pin!(gathering);
            gathering.as_mut().enable();
            connection
                .set_remote_description(
                    RTCSessionDescription::offer(offer)
                        .map_err(|_| Error::bad("Invalid node offer."))?,
                )
                .await
                .map_err(|_| Error::bad("Invalid node offer."))?;
            let answer = connection
                .create_answer(None)
                .await
                .map_err(Error::internal)?;
            connection
                .set_local_description(answer)
                .await
                .map_err(Error::internal)?;
            // Every candidate travels in this one answer; a slow STUN server
            // only costs its reflexive candidate.
            let _ = tokio::time::timeout(GATHERING, gathering).await;
            let answer = connection
                .local_description()
                .await
                .ok_or_else(|| Error::internal("No local description."))?;
            Ok::<_, Error>(answer.sdp)
        }
        .await;
        match answer {
            Ok(answer) if answer.len() <= MAX_SDP => Ok((answer, connection, events)),
            result => {
                let _ = connection.close().await;
                result.and(Err(Error::internal("Answer too large.")))
            }
        }
    }
}

struct Events {
    channels: mpsc::Receiver<Arc<dyn DataChannel>>,
    closed: CancellationToken,
}

struct Handler {
    channels: mpsc::Sender<Arc<dyn DataChannel>>,
    gathered: Arc<Notify>,
    closed: CancellationToken,
}

#[async_trait::async_trait]
impl PeerConnectionEventHandler for Handler {
    async fn on_ice_gathering_state_change(&self, state: RTCIceGatheringState) {
        if state == RTCIceGatheringState::Complete {
            self.gathered.notify_one();
        }
    }

    async fn on_data_channel(&self, channel: Arc<dyn DataChannel>) {
        if self.channels.try_send(channel.clone()).is_err() {
            let _ = channel.close().await;
        }
    }

    async fn on_connection_state_change(&self, state: RTCPeerConnectionState) {
        if matches!(
            state,
            RTCPeerConnectionState::Failed | RTCPeerConnectionState::Closed
        ) {
            self.closed.cancel();
        }
    }
}

/// Serves the node's channels until the connection fails, closes or the
/// manager stops. ICE consent checks end a connection whose node is gone.
async fn serve_peer(
    connection: Arc<dyn PeerConnection>,
    mut events: Events,
    router: Router,
    peer: SocketAddr,
    stop: CancellationToken,
) {
    let slots = Arc::new(Semaphore::new(CHANNELS_PER_PEER));
    let mut control = false;
    // A node that never opens its control channel holds no slot for long.
    let opened = tokio::time::sleep(cairn_protocol::REQUEST_TIMEOUT);
    tokio::pin!(opened);
    loop {
        tokio::select! {
            () = stop.cancelled() => break,
            () = events.closed.cancelled() => break,
            () = &mut opened, if !control => break,
            channel = events.channels.recv() => {
                let Some(channel) = channel else { break };
                match channel.label().await.as_deref() {
                    Ok(DATA_CHANNEL) => {
                        let Ok(permit) = slots.clone().try_acquire_owned() else {
                            let _ = channel.close().await;
                            continue;
                        };
                        let router = router.clone();
                        tokio::spawn(async move {
                            let _permit = permit;
                            serve_channel(channel, router, peer).await;
                        });
                    }
                    Ok(CONTROL_CHANNEL) if !control => {
                        control = true;
                        // SCTP can end while ICE and DTLS still look connected.
                        let lifetime = events.closed.clone();
                        tokio::spawn(async move {
                            while let Some(event) = channel.poll().await {
                                if matches!(event, DataChannelEvent::OnClosing | DataChannelEvent::OnClose | DataChannelEvent::OnError) {
                                    break;
                                }
                            }
                            lifetime.cancel();
                        });
                    }
                    _ => {
                        let _ = channel.close().await;
                    }
                }
            }
        }
    }
    let _ = connection.close().await;
}

/// Carries one HTTP/1.1 connection over `channel`, in both directions.
async fn serve_channel(channel: Arc<dyn DataChannel>, router: Router, peer: SocketAddr) {
    if !matches!(channel.ordered().await, Ok(true))
        || !matches!(channel.max_retransmits().await, Ok(None))
        || !matches!(channel.max_packet_life_time().await, Ok(None))
    {
        let _ = channel.close().await;
        return;
    }
    let (http_side, channel_side) = tokio::io::duplex(BUFFER);
    let (mut from_http, mut to_http) = tokio::io::split(channel_side);
    let inbound = {
        let channel = channel.clone();
        async move {
            while let Some(event) = channel.poll().await {
                match event {
                    DataChannelEvent::OnMessage(message) if !message.is_string => {
                        if to_http.write_all(&message.data).await.is_err() {
                            break;
                        }
                    }
                    DataChannelEvent::OnClose | DataChannelEvent::OnClosing => break,
                    DataChannelEvent::OnMessage(_) | DataChannelEvent::OnError => break,
                    _ => {}
                }
            }
            let _ = to_http.shutdown().await;
        }
    };
    let outbound = {
        let channel = channel.clone();
        async move {
            let mut buffer = vec![0; MESSAGE];
            loop {
                match from_http.read(&mut buffer).await {
                    Ok(0) | Err(_) => break,
                    Ok(read) => {
                        if channel
                            .send(bytes::BytesMut::from(&buffer[..read]))
                            .await
                            .is_err()
                        {
                            return;
                        }
                    }
                }
            }
            close_when_sent(channel.as_ref()).await;
        }
    };
    tokio::join!(serve_connection(http_side, router, peer), inbound, outbound);
}

/// Closing resets the stream at once; let the peer acknowledge what was
/// queued first, so the end of a response is never lost.
async fn close_when_sent(channel: &dyn DataChannel) {
    let _ = tokio::time::timeout(Duration::from_secs(30), async {
        while channel
            .outstanding_bytes()
            .await
            .is_ok_and(|bytes| bytes > 0)
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    let _ = channel.close().await;
}
