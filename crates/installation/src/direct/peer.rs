//! The authorized UDP peer. All application traffic uses the relay dispatcher.
use super::{DirectConnections, DirectEvent};
use crate::{
    error::{Error, Result},
    relay::application::DirectTraffic,
};
use axum::Router;
use cairn_protocol::direct::{DirectAuthorization, DirectSignal};
use rtc::{
    ice::{mdns::MulticastDnsMode, network_type::NetworkType},
    peer_connection::configuration::setting_engine::{SctpMaxMessageSize, SettingEngineBuilder},
};
use std::{collections::HashMap, net::IpAddr, sync::Arc};
use tokio::{sync::mpsc, task::JoinSet};
use tokio_util::sync::CancellationToken;
use tracing::Instrument;
use webrtc::{
    data_channel::{DataChannel, DataChannelEvent},
    peer_connection::{
        PeerConnection, PeerConnectionBuilder, PeerConnectionEventHandler, RTCConfigurationBuilder,
        RTCIceCandidateInit, RTCIceServer, RTCPeerConnectionIceEvent, RTCPeerConnectionState,
        RTCSessionDescription,
    },
};

/// Stable lifecycle signal, created inside each spawned peer task.
pub const PEER_TASK_SPAN: &str = "direct_peer_task";

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
        let enabled = match std::env::var("CAIRN_DIRECT_ENABLED").as_deref() {
            Ok("false") => false,
            Ok("true") | Err(_) => true,
            _ => return Err(Error::bad("CAIRN_DIRECT_ENABLED must be true or false.")),
        };
        let stun_urls = std::env::var("CAIRN_DIRECT_STUN_URLS").map_or_else(
            |_| Self::default().stun_urls,
            |urls| {
                if urls.trim().is_empty() {
                    tracing::info!("Empty CAIRN_DIRECT_STUN_URLS; using authenticated beacon STUN");
                    Vec::new()
                } else {
                    urls.split(',').map(str::to_owned).collect()
                }
            },
        );
        if stun_urls.len() > 4
            || stun_urls
                .iter()
                .any(|url| !url.starts_with("stun:") || url.len() > 256)
        {
            return Err(Error::bad(
                "CAIRN_DIRECT_STUN_URLS requires at most four STUN URLs; TURN is deferred.",
            ));
        }
        let public_ip = std::env::var("CAIRN_DIRECT_PUBLIC_IP")
            .ok()
            .map(|value| {
                value
                    .parse::<IpAddr>()
                    .map_err(|_| Error::bad("Invalid direct public IP."))
            })
            .transpose()?;
        if public_ip.is_some_and(|ip| !cairn_protocol::direct::usable_candidate_address(ip)) {
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
                let signal = DirectSignal::Candidate {
                    candidate: candidate.candidate,
                    sdp_mid: Some("0".into()),
                    sdp_m_line_index: Some(0),
                };

                self.direct.send_signal(&self.id, signal)
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
            let port = event.candidate.port;
            let candidate = format!(
                "candidate:cairn-public 1 udp 1694498815 {public} {port} typ srflx raddr {local} rport {port}"
            );
            self.direct.send_signal(
                &self.id,
                DirectSignal::Candidate {
                    candidate,
                    sdp_mid: Some("0".into()),
                    sdp_m_line_index: Some(0),
                },
            )
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
    let mut peer_tasks = HashMap::<tokio::task::Id, String>::new();
    let traffic = DirectTraffic::default();
    let reassembly = cairn_protocol::data_channel::ReassemblyBudget::default();
    loop {
        tokio::select! {
            biased;
            () = stop.cancelled() => break,
            Some(completed) = jobs.join_next_with_id(), if !jobs.is_empty() => {
                let task = match completed {
                    Ok((task, ())) => task,
                    Err(error) => error.id(),
                };

                if let Some(id) = peer_tasks.remove(&task) {
                    peers.remove(&id);
                    direct.release_peer(&id);
                }
            }
            event = events.recv() => match event {
                Ok(DirectEvent::Signal { id, signal }) => {
                    if let Some(peer) = peers.get(&id) {
                        if peer.try_send(signal).is_err() {
                            direct.release_peer(&id);
                        }
                    } else if let DirectSignal::Offer { sdp } = signal
                        && let Some((authorization, closed)) = direct.pending_peer(&id)
                    {
                        let (sender, receiver) = mpsc::channel(16);
                        peers.insert(id.clone(), sender);
                        let direct = direct.clone();
                        let config = config.clone();
                        let router = router.clone();
                        let traffic = traffic.clone();
                        let reassembly = reassembly.clone();
                        let task = jobs.spawn(async move {
                            let span = tracing::debug_span!(PEER_TASK_SPAN);
                            let failed = CancellationToken::new();
                            let negotiation = Negotiation {
                                authorization,
                                sdp,
                                signals: receiver,
                            };
                            let operation = serve_peer(
                                router,
                                direct.clone(),
                                negotiation,
                                config,
                                failed.clone(),
                                traffic,
                                reassembly,
                            )
                            .instrument(span);

                            tokio::select! {
                                biased;
                                () = closed.cancelled() => {}
                                () = failed.cancelled() => {}
                                _ = operation => {}
                            }
                        });
                        peer_tasks.insert(task.id(), id);
                    }
                }
                Ok(DirectEvent::Revoked(_)) => {}
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                    tracing::warn!("Direct signaling lagged; retaining installation peer");
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            },
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
    reassembly: cairn_protocol::data_channel::ReassemblyBudget,
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
                    .with_multicast_dns_mode(MulticastDnsMode::Disabled)
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
            .await
            .map_err(Error::internal)?;

        let mut answer = peer.create_answer(None).await.map_err(Error::internal)?;
        answer.sdp = cairn_protocol::direct::uppercase_sdp_fingerprints(&answer.sdp);
        peer.set_local_description(answer.clone())
            .await
            .map_err(Error::internal)?;
        direct.send_signal(
            &authorization.claims.connection_id,
            DirectSignal::Answer { sdp: answer.sdp },
        )?;

        let deadline = tokio::time::sleep(cairn_protocol::REQUEST_TIMEOUT);
        tokio::pin!(deadline);
        let channel = loop {
            tokio::select! {
                () = &mut deadline => return Err(Error::gateway_timeout("Direct handshake timed out.")),
                Some(channel) = incoming.recv() => break channel,
                signal = signals.recv() => apply_signal(peer.as_ref(), signal).await?,
            }
        };
        *channel_to_close.lock().unwrap() = Some(channel.clone());
        if channel.label().await.map_err(Error::internal)? != "cairn.v4"
            || !channel.ordered().await.map_err(Error::internal)?
            || channel
                .max_retransmits()
                .await
                .map_err(Error::internal)?
                .is_some()
            || channel
                .max_packet_life_time()
                .await
                .map_err(Error::internal)?
                .is_some()
        {
            return Err(Error::bad("A reliable ordered cairn.v4 channel is required."));
        }

        let certificates = peer
            .sctp()
            .await
            .ok_or_else(|| Error::unauthorized("No DTLS transport."))?
            .transport()
            .get_remote_certificates()
            .await
            .map_err(Error::internal)?;
        let certificate = certificates
            .first()
            .ok_or_else(|| Error::unauthorized("No observed DTLS certificate."))?;
        let lease = direct.accept_certificate(&authorization.claims.connection_id, certificate)?;

        let (requests, input) = mpsc::channel(cairn_protocol::MAX_IN_FLIGHT);
        let (output, mut frames) = mpsc::channel(2);
        let rejected = output.clone();
        let dispatcher = traffic.serve(router, lease.clone(), input, output);

        let writer = async {
            let mut transfer = 1_u32;
            while let Some(frame) = frames.recv().await {
                let encoded = cairn_protocol::data_channel::EncodedFrame::new(transfer, &frame)
                    .map_err(Error::bad)?;
                transfer = transfer.wrapping_add(1).max(1);
                for packet in encoded.packets() {
                    channel
                        .send(bytes::BytesMut::from(packet.as_slice()))
                        .await
                        .map_err(Error::internal)?;
                }
            }
            Ok::<_, Error>(())
        };

        let reader = async {
            use cairn_protocol::data_channel::{
                DecodeError, FrameDecoder, REASSEMBLY_REJECTION_CODE,
                REASSEMBLY_REJECTION_HEADER,
            };
            let budget = reassembly.for_account(&lease.claims.account_id, lease.claims.role);
            let mut decoder = FrameDecoder::with_budget(budget);
            let mut timeout = tokio::time::interval(std::time::Duration::from_secs(1));
            loop {
                tokio::select! {
                    biased;
                    () = lease.closed.cancelled() => return Ok(()),
                    Some(_) = incoming.recv() => return Err(Error::bad("Only one DataChannel is allowed.")),
                    signal = signals.recv() => apply_signal(peer.as_ref(), signal).await?,
                    _ = timeout.tick() => {
                        decoder.expire();
                    }
                    event = channel.poll() => match event {
                        Some(DataChannelEvent::OnMessage(message)) if !message.is_string => {
                            // Owner waiters keep at most one packet while capacity is busy.
                            // Self-blocking transfers are rejected so this ordered
                            // channel can finish its already reserved assemblies.
                            let frame = loop {
                                match decoder.push(&message.data) {
                                    Ok(frame) => break frame,
                                    Err(DecodeError::Busy) => {
                                        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                                    }
                                    Err(DecodeError::Rejected(id)) => {
                                        if let Some(id) = id {
                                            let response = cairn_protocol::Frame::Response(
                                                cairn_protocol::ApiResponse {
                                                    id,
                                                    status: 503,
                                                    // No request entered the dispatcher: even a
                                                    // mutation can safely use the relay once.
                                                    headers: vec![(
                                                        REASSEMBLY_REJECTION_HEADER.into(),
                                                        REASSEMBLY_REJECTION_CODE.into(),
                                                    )],
                                                    body: b"Direct reassembly busy.".to_vec(),
                                                },
                                            );

                                            let send = rejected.send(response);
                                            tokio::pin!(send);
                                            loop {
                                                tokio::select! {
                                                    result = &mut send => {
                                                        result.map_err(|_| Error::unavailable("Direct response queue closed."))?;
                                                        break;
                                                    }
                                                    _ = timeout.tick() => {
                                                        decoder.expire();
                                                    }
                                                }
                                            }
                                        }
                                        break None;
                                    }
                                    Err(DecodeError::Invalid(message)) => return Err(Error::bad(message)),
                                }
                            };

                            if let Some(frame) = frame {
                                requests
                                    .try_send(frame)
                                    .map_err(|_| Error::unavailable("Direct ingress busy."))?;
                            }
                        }
                        None | Some(DataChannelEvent::OnClose) => return Ok(()),
                        Some(DataChannelEvent::OnMessage(_) | DataChannelEvent::OnError) => {
                            return Err(Error::bad("Invalid DataChannel message."));
                        }
                        _ => {}
                    },
                }
            }
        };

        tokio::select! {
            biased;
            () = lease.closed.cancelled() => {}
            result = dispatcher => result?,
            result = reader => result?,
            result = writer => result?,
        }
        Ok(())
    }
    .await;
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
