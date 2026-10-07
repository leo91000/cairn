//! The authorized UDP peer. All application traffic uses the relay dispatcher.
use super::{DirectConnections, DirectEvent};
use crate::{
    error::{Error, Result},
    relay::application::DirectTraffic,
};
use axum::Router;
use leo_relay_protocol::direct::{DirectAuthorization, DirectSignal};
use rtc::{
    ice::network_type::NetworkType,
    peer_connection::configuration::setting_engine::{SctpMaxMessageSize, SettingEngineBuilder},
};
use sha2::{Digest, Sha256};
use std::{collections::HashMap, net::IpAddr, sync::Arc};
use tokio::{sync::mpsc, task::JoinSet};
use tokio_util::sync::CancellationToken;
use webrtc::{
    data_channel::{DataChannel, DataChannelEvent},
    peer_connection::{
        PeerConnection, PeerConnectionBuilder, PeerConnectionEventHandler, RTCConfigurationBuilder,
        RTCIceCandidateInit, RTCIceServer, RTCPeerConnectionIceEvent, RTCPeerConnectionState,
        RTCSessionDescription,
    },
};

#[derive(Clone)]
pub struct PeerConfig {
    pub enabled: bool,
    pub stun_urls: Vec<String>,
    pub public_ip: Option<IpAddr>,
}

impl Default for PeerConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            stun_urls: Vec::new(),
            public_ip: None,
        }
    }
}

impl PeerConfig {
    pub fn load() -> Result<Self> {
        let enabled = match std::env::var("LEO_DIRECT_ENABLED").as_deref() {
            Ok("false") => false,
            Ok("true") | Err(_) => true,
            _ => return Err(Error::bad("LEO_DIRECT_ENABLED must be true or false.")),
        };
        let stun_urls = std::env::var("LEO_DIRECT_STUN_URLS").map_or_else(
            |_| Self::default().stun_urls,
            |urls| urls.split(',').map(str::to_owned).collect(),
        );
        if stun_urls.len() > 4
            || stun_urls
                .iter()
                .any(|url| !url.starts_with("stun:") || url.len() > 256)
        {
            return Err(Error::bad(
                "LEO_DIRECT_STUN_URLS requires at most four STUN URLs; TURN is deferred.",
            ));
        }
        let public_ip = std::env::var("LEO_DIRECT_PUBLIC_IP")
            .ok()
            .map(|value| {
                value
                    .parse::<IpAddr>()
                    .map_err(|_| Error::bad("Invalid direct public IP."))
            })
            .transpose()?;
        if public_ip.is_some_and(|ip| ip.is_unspecified() || ip.is_loopback() || ip.is_multicast())
        {
            return Err(Error::bad("A unicast direct public IP is required."));
        }
        Ok(Self {
            enabled,
            stun_urls,
            public_ip,
        })
    }
}

struct PeerEvents {
    id: String,
    direct: DirectConnections,
    channels: mpsc::Sender<Arc<dyn DataChannel>>,
    failed: CancellationToken,
    public_ip: Option<IpAddr>,
}

#[async_trait::async_trait]
impl PeerConnectionEventHandler for PeerEvents {
    async fn on_ice_candidate(&self, event: RTCPeerConnectionIceEvent) {
        let result = event
            .candidate
            .to_json()
            .map_err(Error::internal)
            .and_then(|candidate| {
                self.direct.send_signal(
                    &self.id,
                    DirectSignal::Candidate {
                        candidate: candidate.candidate,
                        sdp_mid: Some("0".into()),
                        sdp_m_line_index: Some(0),
                    },
                )
            });
        let public = if let Some(public) = self.public_ip
            && event.candidate.typ == webrtc::peer_connection::RTCIceCandidateType::Host
            && let Ok(local) = event.candidate.address.parse::<IpAddr>()
            && local.is_ipv4() == public.is_ipv4()
            && !local.is_unspecified()
            && !local.is_loopback()
        {
            // Only an explicit, port-preserving NAT mapping. Keep the bound
            // host candidate inside ICE; announce its public alias to the peer.
            self.direct.send_signal(&self.id, DirectSignal::Candidate {
                candidate: format!("candidate:leo-public 1 udp 1694498815 {public} {} typ srflx raddr {local} rport {}", event.candidate.port, event.candidate.port),
                sdp_mid: Some("0".into()),
                sdp_m_line_index: Some(0),
            })
        } else {
            Ok(())
        };
        if result.is_err() || public.is_err() {
            self.failed.cancel();
        }
    }

    async fn on_data_channel(&self, channel: Arc<dyn DataChannel>) {
        if self.channels.try_send(channel).is_err() {
            self.failed.cancel();
        }
    }

    async fn on_connection_state_change(&self, state: RTCPeerConnectionState) {
        if matches!(
            state,
            RTCPeerConnectionState::Failed | RTCPeerConnectionState::Closed
        ) {
            self.failed.cancel();
        }
    }
}

pub async fn run(
    router: Router,
    direct: DirectConnections,
    config: PeerConfig,
    stop: CancellationToken,
) {
    let mut events = direct.subscribe();
    let mut peers = HashMap::<String, mpsc::Sender<DirectSignal>>::new();
    let mut jobs = JoinSet::new();
    let traffic = DirectTraffic::default();
    loop {
        tokio::select! {
            biased;
            () = stop.cancelled() => break,
            Some(completed) = jobs.join_next(), if !jobs.is_empty() => {
                if let Ok(id) = completed { peers.remove(&id); }
            }
            event = events.recv() => {
                match event {
                    Ok(DirectEvent::Signal { id, signal }) => {
                        if let Some(peer) = peers.get(&id) {
                            if peer.try_send(signal).is_err() { direct.release_peer(&id); }
                        } else if let DirectSignal::Offer { sdp } = signal
                            && let Some((authorization, closed)) = direct.pending_peer(&id) {
                            let (sender, receiver) = mpsc::channel(16);
                            peers.insert(id.clone(), sender);
                            let direct = direct.clone();
                            let config = config.clone();
                            let router = router.clone();
                            let traffic = traffic.clone();
                            jobs.spawn(async move {
                                let failed = CancellationToken::new();
                                let negotiation = Negotiation { authorization, sdp, signals: receiver };
                                let operation = serve_peer(router, direct.clone(), negotiation, config, failed.clone(), traffic);
                                tokio::select! {
                                    biased;
                                    () = closed.cancelled() => {},
                                    () = failed.cancelled() => {},
                                    _ = operation => {},
                                }
                                direct.release_peer(&id);
                                id
                            });
                        }
                    }
                    Ok(DirectEvent::Revoked(_)) => {},
                    Err(_) => break,
                }
            }
        }
    }
    // Dropping each peer task closes its transport; never touches agent execution.
    jobs.abort_all();
    for id in peers.keys() {
        direct.release_peer(id);
    }
    while jobs.join_next().await.is_some() {}
}

struct Negotiation {
    authorization: DirectAuthorization,
    sdp: String,
    signals: mpsc::Receiver<DirectSignal>,
}

async fn serve_peer(
    router: Router,
    direct: DirectConnections,
    negotiation: Negotiation,
    config: PeerConfig,
    failed: CancellationToken,
    traffic: DirectTraffic,
) -> Result<()> {
    let Negotiation {
        authorization,
        sdp,
        mut signals,
    } = negotiation;
    let (channels, mut incoming) = mpsc::channel(1);
    let handler = Arc::new(PeerEvents {
        id: authorization.claims.connection_id.clone(),
        direct: direct.clone(),
        channels,
        failed: failed.clone(),
        public_ip: config.public_ip,
    });
    let peer: Arc<dyn PeerConnection> = Arc::new(
        PeerConnectionBuilder::new()
            .with_configuration(
                RTCConfigurationBuilder::default()
                    .with_ice_servers(vec![RTCIceServer {
                        urls: if config.stun_urls.is_empty() {
                            direct.stun_urls()
                        } else {
                            config.stun_urls
                        },
                        ..Default::default()
                    }])
                    .build(),
            )
            .with_setting_engine(
                SettingEngineBuilder::new()
                    .with_network_types(vec![NetworkType::Udp4, NetworkType::Udp6])
                    .with_sctp_max_message_size(SctpMaxMessageSize::Bounded(16_384))
                    .with_sctp_max_receive_buffer_size(65_536)
                    .build(),
            )
            .with_handler(handler)
            .with_udp_addrs(vec!["0.0.0.0:0", "[::]:0"])
            .with_data_channel_send_buffer_limit(65_536)
            .build()
            .await
            .map_err(Error::internal)?,
    );
    let channel_to_close = Arc::new(std::sync::Mutex::new(None));
    let _close = ClosePeer {
        peer: peer.clone(),
        channel: channel_to_close.clone(),
    };
    let result: Result<()> = async {
        peer.set_remote_description(RTCSessionDescription::offer(sdp).map_err(Error::internal)?)
            .await.map_err(Error::internal)?;
        let mut answer = peer.create_answer(None).await.map_err(Error::internal)?;
        answer.sdp = answer.sdp.lines().map(|line| {
            if let Some(fingerprint) = line.strip_prefix("a=fingerprint:sha-256 ") {
                format!("a=fingerprint:sha-256 {}", fingerprint.to_ascii_uppercase())
            } else { line.to_owned() }
        }).collect::<Vec<_>>().join("\r\n") + "\r\n";
        peer.set_local_description(answer.clone()).await.map_err(Error::internal)?;
        direct.send_signal(&authorization.claims.connection_id, DirectSignal::Answer { sdp: answer.sdp })?;
        let deadline = tokio::time::sleep(leo_relay_protocol::REQUEST_TIMEOUT);
        tokio::pin!(deadline);
        let channel = loop {
            tokio::select! {
                () = &mut deadline => return Err(Error::gateway_timeout("Direct handshake timed out.")),
                Some(channel) = incoming.recv() => break channel,
                signal = signals.recv() => apply_signal(peer.as_ref(), signal).await?,
            }
        };
        *channel_to_close.lock().unwrap() = Some(channel.clone());
        if channel.label().await.map_err(Error::internal)? != "leo.v4"
            || !channel.ordered().await.map_err(Error::internal)?
            || channel.max_retransmits().await.map_err(Error::internal)?.is_some()
            || channel.max_packet_life_time().await.map_err(Error::internal)?.is_some() {
            return Err(Error::bad("A reliable ordered leo.v4 channel is required."));
        }
        let certificates = peer.sctp().await.ok_or_else(|| Error::unauthorized("No DTLS transport."))?
            .transport().get_remote_certificates().await.map_err(Error::internal)?;
        let certificate = certificates.first().ok_or_else(|| Error::unauthorized("No observed DTLS certificate."))?;
        let fingerprint = format!("sha-256 {}", Sha256::digest(certificate).iter()
            .map(|byte| format!("{byte:02X}")).collect::<Vec<_>>().join(":"));
        let (current, _) = direct.pending_peer(&authorization.claims.connection_id)
            .ok_or_else(|| Error::unauthorized("Direct authorization closed."))?;
        let lease = direct.accept_peer(&current, &current.claims.session_id, &fingerprint)?;
        let (requests, input) = mpsc::channel(leo_relay_protocol::MAX_IN_FLIGHT);
        let (output, mut frames) = mpsc::channel(2);
        let dispatcher = traffic.serve(router, lease.clone(), input, output);
        let writer = async {
            let mut transfer = 1_u32;
            while let Some(frame) = frames.recv().await {
                let encoded = leo_relay_protocol::data_channel::EncodedFrame::new(transfer, &frame).map_err(Error::bad)?;
                transfer = transfer.wrapping_add(1).max(1);
                for packet in encoded.packets() {
                    channel.send(bytes::BytesMut::from(packet.as_slice())).await.map_err(Error::internal)?;
                }
            }
            Ok::<_, Error>(())
        };
        let reader = async {
            let mut decoder = leo_relay_protocol::data_channel::FrameDecoder::default();
            let mut timeout = tokio::time::interval(std::time::Duration::from_secs(1));
            loop {
                tokio::select! {
                    biased;
                    () = lease.closed.cancelled() => return Ok(()),
                    Some(_) = incoming.recv() => return Err(Error::bad("Only one DataChannel is allowed.")),
                    signal = signals.recv() => apply_signal(peer.as_ref(), signal).await?,
                    _ = timeout.tick() => {
                        if decoder.expired() { return Err(Error::bad("Incomplete frame expired.")); }
                    }
                    event = channel.poll() => {
                        match event {
                            Some(DataChannelEvent::OnMessage(message)) if !message.is_string => {
                                if let Some(frame) = decoder.push(&message.data).map_err(Error::bad)? {
                                    requests.try_send(frame).map_err(|_| Error::unavailable("Direct ingress busy."))?;
                                }
                            }
                            None | Some(DataChannelEvent::OnClose) => return Ok(()),
                            Some(DataChannelEvent::OnMessage(_) | DataChannelEvent::OnError) => return Err(Error::bad("Invalid DataChannel message.")),
                            _ => {},
                        }
                    }
                }
            }
        };
        tokio::select! {
            biased;
            () = lease.closed.cancelled() => {},
            result = dispatcher => result?,
            result = reader => result?,
            result = writer => result?,
        }
        Ok(())
    }.await;
    result
}

async fn apply_signal(peer: &dyn PeerConnection, signal: Option<DirectSignal>) -> Result<()> {
    let Some(DirectSignal::Candidate {
        candidate,
        sdp_mid,
        sdp_m_line_index,
    }) = signal
    else {
        return Err(Error::bad("Expected ICE candidate."));
    };
    peer.add_ice_candidate(RTCIceCandidateInit {
        candidate,
        sdp_mid,
        sdp_mline_index: sdp_m_line_index,
        ..Default::default()
    })
    .await
    .map_err(Error::internal)
}

struct ClosePeer {
    peer: Arc<dyn PeerConnection>,
    channel: Arc<std::sync::Mutex<Option<Arc<dyn DataChannel>>>>,
}

impl Drop for ClosePeer {
    fn drop(&mut self) {
        let peer = self.peer.clone();
        let channel = self.channel.lock().unwrap().take();
        tokio::spawn(async move {
            if let Some(channel) = channel {
                let _ = channel.close().await;
                // The library marks the channel Closed before its driver sends
                // the SCTP stream reset. Application dispatch has already stopped.
                // Leave a bounded grace period for that reset, then close UDP.
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            }
            let _ = peer.close().await;
        });
    }
}
