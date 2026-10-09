//! Authenticated network-bench client; only safe reads can fall back to HTTPS.
use bytes::BytesMut;
use cairn_installation::direct::peer::PeerConfig;
use cairn_protocol::{
    ApiRequest, Frame, REQUEST_TIMEOUT, Role,
    data_channel::{EncodedFrame, FrameDecoder},
    direct::{DirectAuthorization, DirectSignal},
};
use reqwest::Client;
use serde_json::{Value, json};
use std::{io::Read, sync::Arc, time::Instant};
use tokio::sync::mpsc;
use webrtc::{
    data_channel::DataChannelEvent,
    peer_connection::{
        PeerConnection, PeerConnectionBuilder, PeerConnectionEventHandler, RTCConfigurationBuilder,
        RTCIceCandidateInit, RTCIceServer, RTCPeerConnectionIceEvent, RTCSessionDescription,
    },
};

type TestResult<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

struct Events(mpsc::Sender<DirectSignal>);

#[async_trait::async_trait]
impl PeerConnectionEventHandler for Events {
    async fn on_ice_candidate(&self, event: RTCPeerConnectionIceEvent) {
        if let Ok(candidate) = event.candidate.to_json() {
            let _ = self.0.try_send(DirectSignal::Candidate {
                candidate: candidate.candidate,
                sdp_mid: Some("0".into()),
                sdp_m_line_index: Some(0),
            });
        }
    }
}

async fn send_signal(
    http: &Client,
    base: &str,
    cookie: &str,
    csrf: &str,
    signal: &DirectSignal,
) -> TestResult<()> {
    let response = http
        .post(format!("{base}/signal"))
        .header("cookie", cookie)
        .header(
            "origin",
            base.split("/api/").next().ok_or("Fixture origin missing")?,
        )
        .header("x-csrf-token", csrf)
        .json(signal)
        .send()
        .await?;
    if response.status().as_u16() != 204 {
        return Err("Signal not accepted".into());
    }
    Ok(())
}

async fn direct_read(
    http: &Client,
    origin: &str,
    cookie: &str,
    installation: &str,
    path: &str,
    config: PeerConfig,
    phase: &mut &'static str,
) -> TestResult<(Vec<u8>, String, String)> {
    *phase = "session";
    let session: Value = http
        .get(format!("{origin}/api/account/session"))
        .header("cookie", cookie)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let csrf = session["csrf"].as_str().ok_or("Fixture session missing")?;
    let (events, mut candidates) = mpsc::channel(32);
    *phase = "peer";
    let peer: Arc<dyn PeerConnection> = Arc::new(
        PeerConnectionBuilder::new()
            .with_configuration(
                RTCConfigurationBuilder::default()
                    .with_ice_servers(vec![RTCIceServer {
                        urls: config.stun_urls.clone(),
                        ..Default::default()
                    }])
                    .build(),
            )
            .with_handler(Arc::new(Events(events)))
            .with_udp_addrs(vec!["0.0.0.0:0"])
            .with_data_channel_send_buffer_limit(65_536)
            .build()
            .await?,
    );
    let result = tokio::time::timeout(REQUEST_TIMEOUT, async {
        let channel = peer.create_data_channel("cairn.v4", None).await?;
        let mut offer = peer.create_offer(None).await?;
        offer.sdp = cairn_protocol::direct::uppercase_sdp_fingerprints(&offer.sdp);
        let fingerprint = offer
            .sdp
            .lines()
            .find_map(|line| line.strip_prefix("a=fingerprint:"))
            .ok_or("DTLS fingerprint missing")?;

        *phase = "authorize";
        let response: Value = http
            .post(format!(
                "{origin}/api/installations/{installation}/direct/authorize"
            ))
            .header("origin", origin)
            .header("cookie", cookie)
            .header("x-csrf-token", csrf)
            .json(&json!({
                "fingerprint": fingerprint,
                "versions": [4],
            }))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        let grant: DirectAuthorization = serde_json::from_value(response["grant"].clone())?;
        if config.stun_urls.is_empty() {
            let stun = response["iceServers"][0]["urls"][0]
                .as_str()
                .ok_or("Official STUN missing")?;
            peer.set_configuration(
                RTCConfigurationBuilder::default()
                    .with_ice_servers(vec![RTCIceServer {
                        urls: vec![stun.to_owned()],
                        ..Default::default()
                    }])
                    .build(),
            )
            .await?;
        }

        let base = format!(
            "{origin}/api/installations/{installation}/direct/{}",
            grant.claims.connection_id
        );
        *phase = "offer";
        peer.set_local_description(offer.clone()).await?;
        send_signal(
            http,
            &base,
            cookie,
            csrf,
            &DirectSignal::Offer { sdp: offer.sdp },
        )
        .await?;
        *phase = "signals";
        let mut signals = http
            .get(format!("{base}/events"))
            .header("cookie", cookie)
            .send()
            .await?
            .error_for_status()?;

        let mut buffered = String::new();
        let mut early = Vec::new();
        let mut answered = false;
        *phase = "ice";
        loop {
            tokio::select! {
                Some(candidate) = candidates.recv() => send_signal(http, &base, cookie, csrf, &candidate).await?,
                chunk = signals.chunk() => {
                    buffered.push_str(std::str::from_utf8(&chunk?.ok_or("Signaling closed")?)?);
                    if buffered.len() > 65_536 {
                        return Err::<_, Box<dyn std::error::Error + Send + Sync>>(
                            "Signaling buffer full".into(),
                        );
                    }
                    while let Some(end) = buffered.find("\n\n") {
                        let event: String = buffered.drain(..end + 2).collect();
                        for line in event.lines() {
                            if let Some(data) = line.strip_prefix("data: ") {
                                match serde_json::from_str::<DirectSignal>(data)? {
                                    DirectSignal::Answer { sdp } => {
                                        peer.set_remote_description(RTCSessionDescription::answer(sdp)?)
                                            .await?;
                                        answered = true;
                                        for candidate in early.drain(..) {
                                            peer.add_ice_candidate(candidate).await?;
                                        }
                                    }
                                    DirectSignal::Candidate {
                                        candidate,
                                        sdp_mid,
                                        sdp_m_line_index,
                                    } => {
                                        let candidate = RTCIceCandidateInit {
                                            candidate,
                                            sdp_mid,
                                            sdp_mline_index: sdp_m_line_index,
                                            ..Default::default()
                                        };
                                        if answered {
                                            peer.add_ice_candidate(candidate).await?;
                                        } else if early.len() < 16 {
                                            early.push(candidate);
                                        } else {
                                            return Err("Too many early candidates".into());
                                        }
                                    }
                                    _ => return Err("Unexpected offer".into()),
                                }
                            }
                        }
                    }
                }
                event = channel.poll() => match event {
                    Some(DataChannelEvent::OnOpen) => break,
                    None | Some(DataChannelEvent::OnClose | DataChannelEvent::OnError) => {
                        return Err("Direct channel closed".into());
                    }
                    _ => {}
                },
            }
        }

        *phase = "request";
        let request = Frame::Request(ApiRequest {
            id: "bench-read".into(),
            account_id: String::new(),
            role: Role::Member,
            mcp_scopes: None,
            public_artifact: None,
            method: "GET".into(),
            path: path.into(),
            headers: Vec::new(),
            body: Vec::new(),
        });
        for packet in EncodedFrame::new(1, &request)
            .map_err(std::io::Error::other)?
            .packets()
        {
            channel.send(BytesMut::from(packet.as_slice())).await?;
        }

        let mut decoder = FrameDecoder::default();
        *phase = "response";
        loop {
            match channel.poll().await {
                Some(DataChannelEvent::OnMessage(message)) if !message.is_string => {
                    if let Some(Frame::Response(response)) =
                        decoder.push(&message.data).map_err(std::io::Error::other)?
                    {
                        if response.id != "bench-read" || response.status != 200 {
                            return Err("Direct API read failed".into());
                        }
                        let pair = peer
                            .sctp()
                            .await
                            .ok_or("SCTP missing")?
                            .transport()
                            .ice_transport()
                            .get_selected_candidate_pair()
                            .await?
                            .ok_or("No selected ICE route")?;
                        if !pair
                            .local()
                            .protocol
                            .to_string()
                            .eq_ignore_ascii_case("udp")
                        {
                            return Err("UDP route required".into());
                        }
                        return Ok((
                            response.body,
                            pair.local().typ.to_string(),
                            pair.remote().typ.to_string(),
                        ));
                    }
                }
                None | Some(DataChannelEvent::OnClose | DataChannelEvent::OnError) => {
                    return Err("Direct read interrupted".into());
                }
                _ => {}
            }
        }
    })
    .await;
    peer.close().await?;
    result.map_err(|_| "Direct negotiation timed out")?
}

#[tokio::main]
async fn main() -> TestResult<()> {
    let args: Vec<_> = std::env::args().collect();
    if args.len() != 5 {
        return Err("Use ORIGIN INSTALLATION API_PATH MARKER".into());
    }
    let origin = url::Url::parse(&args[1])?;
    if origin.scheme() != "http" || origin.host_str() != Some("localhost") || origin.path() != "/" {
        return Err("Only the loopback bench origin is allowed".into());
    }
    let mut cookie = String::new();
    std::io::stdin().read_to_string(&mut cookie)?;
    if cookie.contains(['\r', '\n']) {
        return Err("Invalid fixture session".into());
    }
    let http = Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let started = Instant::now();
    let relay = http
        .get(format!(
            "{}/api/installations/{}{}",
            args[1], args[2], args[3]
        ))
        .header("cookie", &cookie)
        .send()
        .await?
        .error_for_status()?;
    let relay_route = relay
        .headers()
        .get("x-cairn-transport")
        .ok_or("Observed relay route missing")?
        .to_str()?
        .to_owned();
    let relay_body = relay.bytes().await?;
    let mut phase = "initial";
    let direct = direct_read(
        &http,
        &args[1],
        &cookie,
        &args[2],
        &args[3],
        PeerConfig::load()?,
        &mut phase,
    )
    .await;
    // Fixed phase/deadline metadata only. SDK/HTTP error text can contain URLs
    // or signaling details, so never put it in qualification artifacts.
    let direct_failure = direct.as_ref().err().map(|error| {
        json!({
            "phase": phase,
            "timedOut": error.to_string() == "Direct negotiation timed out",
        })
    });
    let (body, route, candidates) = match direct {
        Ok((body, local, remote)) => (
            body,
            "direct".to_owned(),
            Some(json!({ "local": local, "remote": remote })),
        ),
        Err(_) => (relay_body.to_vec(), relay_route, None),
    };
    if !String::from_utf8_lossy(&body).contains(&args[4]) {
        return Err("Conversation marker missing".into());
    }
    println!(
        "{}",
        json!({
            "route": route,
            "status": 200,
            "elapsedMs": started.elapsed().as_millis(),
            "candidatePair": candidates,
            "directFailure": direct_failure,
        })
    );
    Ok(())
}
