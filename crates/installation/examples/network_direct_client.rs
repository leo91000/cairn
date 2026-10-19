//! Authenticated network-bench client; only safe reads can fall back to HTTPS.
#[path = "network_qualification/fault.rs"]
mod fault;

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

struct NegotiationDiagnostics {
    phase: &'static str,
    states: Value,
    started: tokio::time::Instant,
    deadline_started: Option<tokio::time::Instant>,
    phases: Vec<Value>,
    response_messages: usize,
    response_bytes: usize,
    request_loss: Option<Arc<std::sync::Mutex<fault::Plan>>>,
}

impl NegotiationDiagnostics {
    fn new() -> Self {
        Self {
            phase: "initial",
            states: Value::Null,
            started: tokio::time::Instant::now(),
            deadline_started: None,
            phases: Vec::new(),
            response_messages: 0,
            response_bytes: 0,
            request_loss: None,
        }
    }

    fn record(&mut self, event: &'static str) {
        let now = tokio::time::Instant::now();
        let remaining = self.deadline_started.map(|started| {
            REQUEST_TIMEOUT
                .saturating_sub(now.duration_since(started))
                .as_millis()
        });
        // Fixed event names and bounded counts only; no request/signaling identities.
        assert!(self.phases.len() < 24);
        self.phases.push(json!({
            "event": event,
            "elapsedMs": now.duration_since(self.started).as_millis(),
            "remainingMs": remaining,
        }));
    }

    fn enter(&mut self, phase: &'static str) {
        self.phase = phase;
        self.record(phase);
    }
}

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
    diagnostics: &mut NegotiationDiagnostics,
) -> TestResult<(Vec<u8>, String, String)> {
    diagnostics.enter("session");
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
    diagnostics.enter("peer");
    let builder = PeerConnectionBuilder::new();
    let builder = if let Some(plan) = &diagnostics.request_loss {
        let dependencies = fault::dependencies(0, plan.clone());
        builder
            .with_runtime(dependencies.runtime)
            .with_setting_engine(
                rtc::peer_connection::configuration::setting_engine::SettingEngineBuilder::new()
                    .with_crypto_provider(dependencies.crypto)
                    .build(),
            )
    } else {
        builder
    };
    let peer: Arc<dyn PeerConnection> = Arc::new(
        builder
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
    diagnostics.deadline_started = Some(tokio::time::Instant::now());
    diagnostics.record("deadline-started");
    let result = tokio::time::timeout(REQUEST_TIMEOUT, async {
        let channel = peer.create_data_channel("cairn.v4", None).await?;
        let mut offer = peer.create_offer(None).await?;
        offer.sdp = cairn_protocol::direct::uppercase_sdp_fingerprints(&offer.sdp);
        let fingerprint = offer
            .sdp
            .lines()
            .find_map(|line| line.strip_prefix("a=fingerprint:"))
            .ok_or("DTLS fingerprint missing")?;

        diagnostics.enter("authorize");
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
                .ok_or("Beacon STUN missing")?;
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
        diagnostics.enter("offer");
        peer.set_local_description(offer.clone()).await?;
        send_signal(
            http,
            &base,
            cookie,
            csrf,
            &DirectSignal::Offer { sdp: offer.sdp },
        )
        .await?;
        diagnostics.enter("signals");
        let mut signals = http
            .get(format!("{base}/events"))
            .header("cookie", cookie)
            .send()
            .await?
            .error_for_status()?;

        let mut buffered = String::new();
        let mut early = Vec::new();
        let mut answered = false;
        diagnostics.enter("ice");
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

        diagnostics.record("channel-open");
        diagnostics.enter("request");
        if let Some(plan) = &diagnostics.request_loss {
            plan.lock().unwrap().arm(fault::Mode::Recoverable, true, false);
        }
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
        diagnostics.record("request-queued");

        let mut decoder = FrameDecoder::default();
        diagnostics.enter("response");
        loop {
            match channel.poll().await {
                Some(DataChannelEvent::OnMessage(message)) if !message.is_string => {
                    if diagnostics.response_messages == 0 {
                        diagnostics.record("first-response-message");
                    }
                    diagnostics.response_messages += 1;
                    diagnostics.response_bytes += message.data.len();
                    if let Some(Frame::Response(response)) =
                        decoder.push(&message.data).map_err(std::io::Error::other)?
                    {
                        if response.id != "bench-read" || response.status != 200 {
                            return Err("Direct API read failed".into());
                        }
                        diagnostics.record("complete-response");
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
    diagnostics.record(if result.is_err() {
        "deadline-expired"
    } else {
        "operation-ended"
    });
    // State-only diagnostics before closing: no candidates, certificates or SDP.
    if let Some(sctp) = peer.sctp().await {
        let dtls = sctp.transport();
        let ice = dtls.ice_transport();
        diagnostics.states = json!({
            "ice": ice.state().await.ok().map(|state| format!("{state:?}")),
            "dtls": dtls.state().await.ok().map(|state| format!("{state:?}")),
            "sctp": sctp.state().await.ok().map(|state| format!("{state:?}")),
        });
    }

    diagnostics.record("local-close-requested");
    peer.close().await?;
    result.map_err(|_| "Direct negotiation timed out")?
}

#[tokio::main]
async fn main() -> TestResult<()> {
    let args: Vec<_> = std::env::args().collect();
    if args.len() != 5 && !(args.len() == 6 && args[5] == "--bounded-request-loss") {
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
    if relay_route != "relay" || !String::from_utf8_lossy(&relay_body).contains(&args[4]) {
        return Err("Authenticated relay preflight marker missing".into());
    }
    let mut diagnostics = NegotiationDiagnostics::new();
    if args.len() == 6 {
        diagnostics.request_loss = Some(Arc::new(std::sync::Mutex::new(fault::Plan::default())));
    }
    let direct = direct_read(
        &http,
        &args[1],
        &cookie,
        &args[2],
        &args[3],
        PeerConfig::load()?,
        &mut diagnostics,
    )
    .await;
    // Fixed phase/deadline metadata only. SDK/HTTP error text can contain URLs
    // or signaling details, so never put it in qualification artifacts.
    let failure_http_status = direct
        .as_ref()
        .err()
        .and_then(|error| error.downcast_ref::<reqwest::Error>())
        .and_then(reqwest::Error::status)
        .map(|status| status.as_u16());
    let direct_failure = direct.as_ref().err().map(|error| {
        json!({
            "phase": diagnostics.phase,
            "timedOut": error.to_string() == "Direct negotiation timed out",
        })
    });
    let (body, route, candidates) = match direct {
        Ok((body, local, remote)) => (
            body,
            "direct".to_owned(),
            Some(json!({ "local": local, "remote": remote })),
        ),
        Err(_) => {
            // Prove authenticated recovery after the failure, rather than label
            // the preflight body as a newly completed relay operation.
            let response = http
                .get(format!(
                    "{}/api/installations/{}{}",
                    args[1], args[2], args[3]
                ))
                .header("cookie", &cookie)
                .send()
                .await?
                .error_for_status()?;
            if response
                .headers()
                .get("x-cairn-transport")
                .and_then(|value| value.to_str().ok())
                != Some("relay")
            {
                return Err("Authenticated fallback route missing".into());
            }
            (response.bytes().await?.to_vec(), relay_route, None)
        }
    };
    if !String::from_utf8_lossy(&body).contains(&args[4]) {
        return Err("Conversation marker missing".into());
    }
    let request_loss = diagnostics.request_loss.as_ref().map(|plan| {
        let plan = plan.lock().unwrap();
        json!({
            "armed": plan.armed,
            "attempts": plan.attempts,
            "droppedDatagrams": plan.dropped_datagrams,
            "passedTarget": plan.passed_target,
            "errors": plan.errors,
            "attemptTimesMs": plan.times_ms,
        })
    });
    println!(
        "{}",
        json!({
            "route": route,
            "status": 200,
            "elapsedMs": started.elapsed().as_millis(),
            "candidatePair": candidates,
            "directFailure": direct_failure,
            "failureHttpStatus": failure_http_status,
            "directDiagnostics": diagnostics.states,
            "directPhases": diagnostics.phases,
            "responseMessages": diagnostics.response_messages,
            "responseBytes": diagnostics.response_bytes,
            "requestLoss": request_loss,
        })
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn response_phase_can_have_only_one_second_of_shared_budget() {
        let mut diagnostics = NegotiationDiagnostics::new();
        diagnostics.deadline_started = Some(tokio::time::Instant::now());
        let result = tokio::time::timeout(REQUEST_TIMEOUT, async {
            diagnostics.enter("ice");
            tokio::time::sleep(REQUEST_TIMEOUT - std::time::Duration::from_secs(1)).await;
            diagnostics.record("channel-open");
            diagnostics.record("request-queued");
            diagnostics.enter("response");
            std::future::pending::<()>().await;
        })
        .await;
        assert!(result.is_err());
        assert_eq!(diagnostics.phase, "response");
        let opened = diagnostics
            .phases
            .iter()
            .find(|phase| phase["event"] == "channel-open")
            .unwrap();
        assert_eq!(opened["remainingMs"], 1000);
        assert_eq!(opened["elapsedMs"], 29_000);
        diagnostics.record("deadline-expired");
        assert_eq!(diagnostics.phases.last().unwrap()["remainingMs"], 0);
    }
}
