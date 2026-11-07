//! The direct path. Beacon forwards one offer and one answer, each with every
//! candidate; the node and the manager then exchange UDP without Beacon. The
//! manager's DTLS certificate must match the pin, so an answer altered on the
//! way cannot put anyone else in the middle.
use crate::Config;
use cairn_protocol::nodes::{CONTROL_CHANNEL, DATA_CHANNEL, MAX_SDP, certificate_pin};
use rtc::{
    ice::{mdns::MulticastDnsMode, network_type::NetworkType},
    peer_connection::configuration::setting_engine::{SctpMaxMessageSize, SettingEngineBuilder},
};
use std::{net::IpAddr, sync::Arc, time::Duration};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    sync::{Notify, RwLock, watch},
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
// A disk transfer needs a wide window: 4 MiB covers 80 MB/s at 50 ms.
const RECEIVE_WINDOW: u32 = 4 << 20;
const SEND_BUFFER: usize = 1 << 20;
const GATHERING: Duration = Duration::from_secs(5);
const CONNECTING: Duration = Duration::from_secs(15);
const OPENING: Duration = Duration::from_secs(5);
// At start, connections wait this long for the first direct attempt.
const SETTLING: Duration = Duration::from_secs(20);
// Networks that block a direct connection rarely change; retry slowly.
const FIRST_RETRY: Duration = Duration::from_secs(30);
const LAST_RETRY: Duration = Duration::from_secs(600);

/// The current direct connection to the manager, kept up in the background.
#[derive(Clone)]
pub struct Direct {
    current: Arc<RwLock<Option<Arc<dyn PeerConnection>>>>,
    // Whether the first attempt has finished, and whether a connection is up.
    settled: Arc<watch::Sender<bool>>,
    available: Arc<watch::Sender<bool>>,
}

impl Direct {
    pub fn start(config: Arc<Config>, stop: CancellationToken) -> Self {
        let direct = Self {
            current: Arc::default(),
            settled: Arc::new(watch::channel(!config.direct).0),
            available: Arc::new(watch::channel(false).0),
        };
        if config.direct {
            tokio::spawn(direct.clone().maintain(config, stop));
        }
        direct
    }

    /// Changes to `true` while a direct connection is up.
    pub fn available(&self) -> watch::Receiver<bool> {
        self.available.subscribe()
    }

    async fn maintain(self, config: Arc<Config>, stop: CancellationToken) {
        let mut retry = FIRST_RETRY;
        loop {
            let attempt = tokio::select! {
                () = stop.cancelled() => return,
                attempt = connect(&config) => attempt,
            };
            let connected = attempt.is_ok();
            if connected {
                tracing::info!("Connected directly to the manager");
            }
            if let Ok((connection, _)) = &attempt {
                *self.current.write().await = Some(connection.clone());
            }
            self.available.send_replace(connected);
            self.settled.send_replace(true);
            match attempt {
                Ok((connection, closed)) => {
                    tokio::select! {
                        () = stop.cancelled() => {}
                        () = closed.cancelled() => {}
                    }
                    *self.current.write().await = None;
                    self.available.send_replace(false);
                    let _ = connection.close().await;
                    tracing::info!(
                        "Direct connection to the manager ended; using Beacon meanwhile"
                    );
                    retry = FIRST_RETRY;
                    // A lost connection is usually back at once.
                    continue;
                }
                Err(reason) => {
                    tracing::info!(%reason, "No direct connection to the manager; using Beacon");
                }
            }
            tokio::select! {
                () = stop.cancelled() => return,
                () = tokio::time::sleep(retry) => {}
            }
            retry = (retry * 2).min(LAST_RETRY);
        }
    }

    /// An open channel to the manager, or `None` to go through Beacon.
    pub async fn channel(&self) -> Option<Arc<dyn DataChannel>> {
        let mut settled = self.settled.subscribe();
        let _ = tokio::time::timeout(SETTLING, settled.wait_for(|settled| *settled)).await;
        let connection = self.current.read().await.clone()?;
        let channel =
            tokio::time::timeout(OPENING, connection.create_data_channel(DATA_CHANNEL, None))
                .await
                .ok()?
                .ok()?;
        let opened = tokio::time::timeout(OPENING, async {
            while let Some(event) = channel.poll().await {
                match event {
                    DataChannelEvent::OnOpen => return true,
                    DataChannelEvent::OnClose | DataChannelEvent::OnError => return false,
                    _ => {}
                }
            }
            false
        })
        .await;
        if matches!(opened, Ok(true)) {
            Some(channel)
        } else {
            let _ = channel.close().await;
            None
        }
    }
}

struct Handler {
    gathered: Arc<Notify>,
    connected: Arc<Notify>,
    closed: CancellationToken,
}

#[async_trait::async_trait]
impl PeerConnectionEventHandler for Handler {
    async fn on_ice_gathering_state_change(&self, state: RTCIceGatheringState) {
        if state == RTCIceGatheringState::Complete {
            self.gathered.notify_one();
        }
    }

    async fn on_connection_state_change(&self, state: RTCPeerConnectionState) {
        match state {
            RTCPeerConnectionState::Connected => self.connected.notify_one(),
            RTCPeerConnectionState::Failed | RTCPeerConnectionState::Closed => self.closed.cancel(),
            _ => {}
        }
    }
}

/// The UDP sockets to gather on: one per address family with a usable interface.
fn udp_bindings() -> Result<Vec<(&'static str, NetworkType)>, String> {
    let interfaces = rtc::shared::ifaces::ifaces().map_err(|error| error.to_string())?;
    let (mut ipv4, mut ipv6) = (false, false);
    for address in interfaces
        .into_iter()
        .filter_map(|interface| interface.addr.map(|address| address.ip()))
        .filter(|address| cairn_protocol::direct::usable_candidate_address(*address))
    {
        match address {
            IpAddr::V4(_) => ipv4 = true,
            IpAddr::V6(_) => ipv6 = true,
        }
    }
    let bindings: Vec<_> = [
        (ipv4, "0.0.0.0:0", NetworkType::Udp4),
        (ipv6, "[::]:0", NetworkType::Udp6),
    ]
    .into_iter()
    .filter_map(|(available, bind, family)| available.then_some((bind, family)))
    .collect();
    if bindings.is_empty() {
        return Err("no usable network interface".into());
    }
    Ok(bindings)
}

async fn connect(config: &Config) -> Result<(Arc<dyn PeerConnection>, CancellationToken), String> {
    let beacon = crate::beacon_origin(&config.beacon).map_err(|error| error.to_string())?;
    let endpoint = beacon
        .join(&format!(
            "api/relay/{}/nodes/direct",
            config.installation_id
        ))
        .map_err(|error| error.to_string())?;
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(|error| error.to_string())?;
    let ice: serde_json::Value = client
        .get(endpoint.clone())
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|_| "Beacon is unreachable".to_owned())?
        .json()
        .await
        .map_err(|_| "invalid ICE servers".to_owned())?;
    let urls: Vec<String> = ice["iceServers"][0]["urls"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|url| url.as_str())
        .filter(|url| url.starts_with("stun:") && url.len() <= 256)
        .map(str::to_owned)
        .take(4)
        .collect();

    let (udp_addrs, network_types): (Vec<_>, Vec<_>) = udp_bindings()?.into_iter().unzip();
    let gathered = Arc::new(Notify::new());
    let connected = Arc::new(Notify::new());
    let closed = CancellationToken::new();
    let settings = SettingEngineBuilder::new()
        .with_network_types(network_types)
        .with_multicast_dns_mode(MulticastDnsMode::Disabled)
        .with_sctp_max_message_size(SctpMaxMessageSize::Bounded(MESSAGE as u32))
        .with_sctp_max_receive_buffer_size(RECEIVE_WINDOW);
    let connection: Arc<dyn PeerConnection> = Arc::new(
        PeerConnectionBuilder::new()
            .with_configuration(
                RTCConfigurationBuilder::default()
                    .with_ice_servers(vec![RTCIceServer {
                        urls,
                        ..Default::default()
                    }])
                    .build(),
            )
            .with_setting_engine(settings.build())
            .with_handler(Arc::new(Handler {
                gathered: gathered.clone(),
                connected: connected.clone(),
                closed: closed.clone(),
            }))
            .with_udp_addrs(udp_addrs)
            .with_data_channel_send_buffer_limit(SEND_BUFFER)
            .build()
            .await
            .map_err(|error| error.to_string())?,
    );
    let result = async {
        // Opened with the offer and kept open: the manager drops a
        // connection whose control channel never arrives.
        let control = connection
            .create_data_channel(CONTROL_CHANNEL, None)
            .await
            .map_err(|error| error.to_string())?;
        let gathering = gathered.notified();
        tokio::pin!(gathering);
        gathering.as_mut().enable();
        let offer = connection
            .create_offer(None)
            .await
            .map_err(|error| error.to_string())?;
        connection
            .set_local_description(offer)
            .await
            .map_err(|error| error.to_string())?;
        let _ = tokio::time::timeout(GATHERING, gathering).await;
        let offer = connection
            .local_description()
            .await
            .ok_or("no local description")?
            .sdp;
        if offer.len() > MAX_SDP {
            return Err("offer too large".to_owned());
        }

        let response = client
            .post(endpoint)
            .json(&serde_json::json!({ "sdp": offer }))
            .send()
            .await
            .map_err(|_| "Beacon is unreachable".to_owned())?;
        if !response.status().is_success() {
            let body: serde_json::Value = response.json().await.unwrap_or_default();
            return Err(body["error"]
                .as_str()
                .unwrap_or("the offer was refused")
                .to_owned());
        }
        let answer: serde_json::Value = response
            .json()
            .await
            .map_err(|_| "invalid answer".to_owned())?;
        let answer = answer["sdp"].as_str().ok_or("invalid answer")?.to_owned();
        let connecting = connected.notified();
        tokio::pin!(connecting);
        connecting.as_mut().enable();
        connection
            .set_remote_description(
                RTCSessionDescription::answer(answer).map_err(|_| "invalid answer".to_owned())?,
            )
            .await
            .map_err(|_| "invalid answer".to_owned())?;
        tokio::select! {
            () = &mut connecting => {}
            () = closed.cancelled() => return Err("the networks blocked a direct connection".into()),
            () = tokio::time::sleep(CONNECTING) => {
                return Err("the networks blocked a direct connection".into());
            }
        }

        let certificates = connection
            .sctp()
            .await
            .ok_or("no DTLS transport")?
            .transport()
            .get_remote_certificates()
            .await
            .map_err(|error| error.to_string())?;
        let pinned = certificates
            .first()
            .is_some_and(|certificate| certificate_pin(certificate) == config.pin);
        if !pinned {
            tracing::warn!("The manager's certificate does not match this node's pin");
            return Err("the manager's certificate does not match this node".into());
        }

        let opened = tokio::time::timeout(OPENING, async {
            while let Some(event) = control.poll().await {
                match event {
                    DataChannelEvent::OnOpen => return true,
                    DataChannelEvent::OnClose | DataChannelEvent::OnError => return false,
                    _ => {}
                }
            }
            false
        })
        .await;
        if !matches!(opened, Ok(true)) {
            return Err("the manager did not open the connection".into());
        }
        // Losing SCTP also ends this peer even if ICE and DTLS still look up.
        let lifetime = closed.clone();
        tokio::spawn(async move {
            while let Some(event) = control.poll().await {
                if matches!(
                    event,
                    DataChannelEvent::OnClosing
                        | DataChannelEvent::OnClose
                        | DataChannelEvent::OnError
                ) {
                    break;
                }
            }

            lifetime.cancel();
        });
        Ok(())
    }
    .await;
    match result {
        Ok(()) => Ok((connection, closed)),
        Err(reason) => {
            let _ = connection.close().await;
            Err(reason)
        }
    }
}

/// Carries one local connection over `channel`, in both directions.
pub async fn splice<L>(local: L, channel: Arc<dyn DataChannel>)
where
    L: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let (mut from_local, mut to_local) = tokio::io::split(local);
    let inbound = {
        let channel = channel.clone();
        async move {
            while let Some(event) = channel.poll().await {
                match event {
                    DataChannelEvent::OnMessage(message) if !message.is_string => {
                        if to_local.write_all(&message.data).await.is_err() {
                            break;
                        }
                    }
                    DataChannelEvent::OnClose
                    | DataChannelEvent::OnClosing
                    | DataChannelEvent::OnError
                    | DataChannelEvent::OnMessage(_) => break,
                    _ => {}
                }
            }
            let _ = to_local.shutdown().await;
        }
    };
    let outbound = {
        let channel = channel.clone();
        async move {
            let mut buffer = vec![0; MESSAGE];
            loop {
                match from_local.read(&mut buffer).await {
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
    tokio::join!(inbound, outbound);
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
