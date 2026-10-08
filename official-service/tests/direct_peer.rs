mod common;

use common::RelayedInstallation;
use leo_relay_protocol::{
    Frame,
    direct::{DirectAuthorization, DirectSignal},
};
use reqwest::{Method, StatusCode};
use serde_json::json;
use std::{sync::Arc, time::Duration};
use tokio::sync::mpsc;
use webrtc::{
    data_channel::{DataChannel, DataChannelEvent},
    peer_connection::{
        PeerConnection, PeerConnectionBuilder, PeerConnectionEventHandler,
        RTCPeerConnectionIceEvent, RTCSessionDescription,
    },
};

struct ClientEvents(mpsc::Sender<DirectSignal>);

#[async_trait::async_trait]
impl PeerConnectionEventHandler for ClientEvents {
    async fn on_ice_candidate(&self, event: RTCPeerConnectionIceEvent) {
        let candidate = event.candidate.to_json().unwrap();
        self.0
            .try_send(DirectSignal::Candidate {
                candidate: candidate.candidate,
                sdp_mid: candidate.sdp_mid,
                sdp_m_line_index: candidate.sdp_mline_index,
            })
            .unwrap();
    }
}

async fn production_connector(relay: &mut RelayedInstallation) {
    configured_connector(
        relay,
        leo_agent_manager::direct::peer::PeerConfig::default(),
    )
    .await;
}

async fn configured_connector(
    relay: &mut RelayedInstallation,
    config: leo_agent_manager::direct::peer::PeerConfig,
) {
    relay.stop.cancel();
    (&mut relay.connector).await.unwrap().unwrap();
    relay.stop = tokio_util::sync::CancellationToken::new();
    relay.connector = tokio::spawn(leo_agent_manager::relay::connect_with_peer(
        relay
            .installation
            .config
            .data_dir
            .join("installation-relay"),
        relay.router.clone(),
        relay.installation.clone(),
        relay.stop.clone(),
        config,
    ));
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if relay.get("/chats").send().await.unwrap().status() == StatusCode::OK {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
}

async fn signal_as(
    relay: &RelayedInstallation,
    cookie: &str,
    session: &serde_json::Value,
    grant: &DirectAuthorization,
    signal: &DirectSignal,
) {
    let path = format!(
        "/api/installations/{}/direct/{}/signal",
        grant.claims.installation_id, grant.claims.connection_id
    );
    let response = relay
        .app
        .authenticated(cookie, session, Method::POST, &path)
        .json(signal)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
}

async fn client(
    relay: &RelayedInstallation,
) -> (
    Arc<dyn PeerConnection>,
    Arc<dyn DataChannel>,
    DirectAuthorization,
) {
    client_with_fingerprint(relay, &relay.cookie, &relay.session, false).await
}

async fn client_with_fingerprint(
    relay: &RelayedInstallation,
    cookie: &str,
    session: &serde_json::Value,
    mismatched: bool,
) -> (
    Arc<dyn PeerConnection>,
    Arc<dyn DataChannel>,
    DirectAuthorization,
) {
    client_with_offer_candidates(relay, cookie, session, mismatched, "").await
}

async fn client_with_offer_candidates(
    relay: &RelayedInstallation,
    cookie: &str,
    session: &serde_json::Value,
    mismatched: bool,
    extra_candidates: &str,
) -> (
    Arc<dyn PeerConnection>,
    Arc<dyn DataChannel>,
    DirectAuthorization,
) {
    let (events, mut candidates) = mpsc::channel(32);
    let peer: Arc<dyn PeerConnection> = Arc::new(
        PeerConnectionBuilder::new()
            .with_handler(Arc::new(ClientEvents(events)))
            .with_udp_addrs(vec!["0.0.0.0:0"])
            .build()
            .await
            .unwrap(),
    );
    let channel = peer.create_data_channel("leo.v4", None).await.unwrap();
    let mut offer = peer.create_offer(None).await.unwrap();
    let local_offer = offer.clone();
    offer.sdp = offer
        .sdp
        .lines()
        .map(|line| {
            if let Some(fingerprint) = line.strip_prefix("a=fingerprint:sha-256 ") {
                format!(
                    "a=fingerprint:sha-256 {}",
                    if mismatched {
                        vec!["AB"; 32].join(":")
                    } else {
                        fingerprint.to_ascii_uppercase()
                    }
                )
            } else {
                line.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join("\r\n")
        + "\r\n";
    let fingerprint = offer
        .sdp
        .lines()
        .find_map(|line| line.strip_prefix("a=fingerprint:"))
        .unwrap();
    let path = format!(
        "/api/installations/{}/direct/authorize",
        relay.session["installations"][0]["id"].as_str().unwrap()
    );
    let response = relay
        .app
        .authenticated(cookie, session, Method::POST, &path)
        .json(&json!({
            "fingerprint": fingerprint,
            "versions": [4],
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let value: serde_json::Value = response.json().await.unwrap();
    let grant: DirectAuthorization = serde_json::from_value(value["grant"].clone()).unwrap();
    peer.set_local_description(local_offer).await.unwrap();
    offer.sdp.push_str(extra_candidates);
    signal_as(
        relay,
        cookie,
        session,
        &grant,
        &DirectSignal::Offer { sdp: offer.sdp },
    )
    .await;
    let path = format!(
        "{}/api/installations/{}/direct/{}/events",
        relay.app.url, grant.claims.installation_id, grant.claims.connection_id
    );
    let mut events = relay
        .app
        .client
        .get(path)
        .header("cookie", cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(events.status(), StatusCode::OK);
    let negotiated = tokio::time::timeout(Duration::from_secs(3), async {
        let mut buffered = String::new();
        loop {
            tokio::select! {
                Some(candidate) = candidates.recv() => signal_as(relay, cookie, session, &grant, &candidate).await,
                chunk = events.chunk() => {
                    buffered.push_str(
                        std::str::from_utf8(&chunk.unwrap().expect("signaling remains open")).unwrap(),
                    );
                    while let Some(end) = buffered.find("\n\n") {
                        let event: String = buffered.drain(..end + 2).collect();
                        for line in event.lines() {
                            if let Some(data) = line.strip_prefix("data: ") {
                                match serde_json::from_str::<DirectSignal>(data).unwrap() {
                                    DirectSignal::Answer { sdp } => peer
                                        .set_remote_description(RTCSessionDescription::answer(sdp).unwrap())
                                        .await
                                        .unwrap(),
                                    DirectSignal::Candidate {
                                        candidate,
                                        sdp_mid,
                                        sdp_m_line_index,
                                    } => {
                                        peer.add_ice_candidate(
                                            webrtc::peer_connection::RTCIceCandidateInit {
                                                candidate,
                                                sdp_mid,
                                                sdp_mline_index: sdp_m_line_index,
                                                ..Default::default()
                                            },
                                        )
                                        .await
                                        .unwrap();
                                    }
                                    _ => panic!("unexpected installation offer"),
                                }
                            }
                        }
                    }
                }
                event = channel.poll() => {
                    if matches!(event, Some(DataChannelEvent::OnOpen)) {
                        return true;
                    }
                    if matches!(
                        event,
                        None | Some(DataChannelEvent::OnClose | DataChannelEvent::OnError)
                    ) {
                        return false;
                    }
                }
            }
        }
    })
    .await;
    if mismatched {
        assert!(
            !matches!(negotiated, Ok(true)),
            "observed DTLS certificate must match the signed fingerprint"
        );
    } else {
        assert!(
            matches!(negotiated, Ok(true)),
            "authorized real DataChannel must open"
        );
    }
    (peer, channel, grant)
}

#[tokio::test]
async fn authorized_client_opens_a_real_installation_data_channel() {
    let mut relay = RelayedInstallation::new(axum::Router::new()).await;
    production_connector(&mut relay).await;
    let (peer, _channel, _grant) = client(&relay).await;
    peer.close().await.unwrap();
    relay.close().await;
}

#[tokio::test]
async fn mixed_usable_and_link_local_offer_opens_the_real_data_channel() {
    let mut relay = RelayedInstallation::new(axum::Router::new()).await;
    production_connector(&mut relay).await;
    let candidates = concat!(
        "a=candidate:1 1 udp 2122260223 192.0.2.1 50000 typ host\r\n",
        "a=candidate:2 1 udp 2122260223 fe80::1 50001 typ host\r\n",
    );
    let (peer, channel, _) =
        client_with_offer_candidates(&relay, &relay.cookie, &relay.session, false, candidates)
            .await;
    send_frame(channel.as_ref(), 1, &request("mixed-offer", "/api/chats")).await;
    assert!(
        matches!(response(channel.as_ref()).await, Frame::Response(reply) if reply.status == 200)
    );
    peer.close().await.unwrap();
    relay.close().await;
}

async fn send_frame(channel: &dyn DataChannel, id: u32, frame: &leo_relay_protocol::Frame) {
    let frame = serde_json::to_vec(frame).unwrap();
    for (index, fragment) in frame.chunks(16_384 - 13).enumerate() {
        let mut packet = vec![1];
        packet.extend(id.to_be_bytes());
        packet.extend((frame.len() as u32).to_be_bytes());
        packet.extend(((index * (16_384 - 13)) as u32).to_be_bytes());
        packet.extend(fragment);
        channel
            .send(bytes::BytesMut::from(packet.as_slice()))
            .await
            .unwrap();
    }
}

async fn response(channel: &dyn DataChannel) -> leo_relay_protocol::Frame {
    tokio::time::timeout(Duration::from_secs(3), async {
        let mut frame = Vec::new();
        loop {
            if let Some(DataChannelEvent::OnMessage(message)) = channel.poll().await {
                assert!(message.data.len() <= 16_384);
                assert_eq!(message.data[0], 1);
                let total = u32::from_be_bytes(message.data[5..9].try_into().unwrap()) as usize;
                let offset = u32::from_be_bytes(message.data[9..13].try_into().unwrap()) as usize;
                assert_eq!(offset, frame.len());
                frame.extend(&message.data[13..]);
                if frame.len() == total {
                    return serde_json::from_slice(&frame).unwrap();
                }
            }
        }
    })
    .await
    .expect("DataChannel application response")
}

fn request(id: &str, path: &str) -> leo_relay_protocol::Frame {
    leo_relay_protocol::Frame::Request(leo_relay_protocol::ApiRequest {
        id: id.into(),
        account_id: "forged-account".into(),
        role: leo_relay_protocol::Role::Member,
        mcp_scopes: Some(vec!["forged-scope".into()]),
        public_artifact: Some("forged-capability".into()),
        method: "GET".into(),
        path: path.into(),
        headers: vec![("x-leo-account-id".into(), "forged-account".into())],
        body: Vec::new(),
    })
}

#[tokio::test]
async fn direct_dispatches_the_same_response_as_relay_using_the_verified_identity() {
    let mut relay = RelayedInstallation::new(axum::Router::new()).await;
    production_connector(&mut relay).await;
    let (peer, channel, _grant) = client(&relay).await;
    let expected = relay
        .get("/chats")
        .send()
        .await
        .unwrap()
        .bytes()
        .await
        .unwrap();
    send_frame(channel.as_ref(), 1, &request("read", "/api/chats")).await;
    let leo_relay_protocol::Frame::Response(reply) = response(channel.as_ref()).await else {
        panic!("API response expected")
    };
    assert_eq!(reply.status, 200);
    assert_eq!(reply.body, expected);
    peer.close().await.unwrap();
    relay.close().await;
}

fn stream_router() -> axum::Router {
    axum::Router::new().route(
        "/api/fixture/stream",
        axum::routing::get(|| async { "hello-stream" }),
    )
}

#[tokio::test]
async fn direct_stream_uses_the_same_body_with_credit_and_cancel_during_fragmentation() {
    let mut relay = RelayedInstallation::new(stream_router()).await;
    production_connector(&mut relay).await;
    let (peer, channel, _) = client(&relay).await;
    let expected = relay
        .get("/fixture/stream")
        .send()
        .await
        .unwrap()
        .bytes()
        .await
        .unwrap();
    send_frame(
        channel.as_ref(),
        1,
        &request("stream", "/api/fixture/stream"),
    )
    .await;
    assert!(
        matches!(response(channel.as_ref()).await, Frame::StreamStart(reply) if reply.status == 200)
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(100), channel.poll())
            .await
            .is_err(),
        "body must wait for credit"
    );
    send_frame(
        channel.as_ref(),
        2,
        &Frame::StreamCredit {
            id: "stream".into(),
        },
    )
    .await;
    assert!(
        matches!(response(channel.as_ref()).await, Frame::StreamChunk { body, .. } if body == expected)
    );
    // Start an incomplete upload, then cancel the stream on an independent transfer.
    let mut partial = vec![1];
    partial.extend(3_u32.to_be_bytes());
    partial.extend(60_000_u32.to_be_bytes());
    partial.extend(0_u32.to_be_bytes());
    partial.extend(b"unfinished request");
    channel
        .send(bytes::BytesMut::from(partial.as_slice()))
        .await
        .unwrap();
    send_frame(
        channel.as_ref(),
        4,
        &Frame::Cancel {
            id: "stream".into(),
        },
    )
    .await;
    let mut abort = vec![1];
    abort.extend(3_u32.to_be_bytes());
    abort.extend([0; 8]);
    channel
        .send(bytes::BytesMut::from(abort.as_slice()))
        .await
        .unwrap();
    send_frame(
        channel.as_ref(),
        5,
        &request("read-after-cancel", "/api/chats"),
    )
    .await;
    assert!(
        matches!(response(channel.as_ref()).await, Frame::Response(reply) if reply.status == 200)
    );
    peer.close().await.unwrap();
    relay.close().await;
}

#[tokio::test]
async fn fragmented_request_and_response_keep_body_bytes_and_verified_capabilities() {
    use axum::{Extension, body::Bytes, routing::post};
    use leo_agent_manager::auth::{InstallationIdentity, InstallationRole};
    let router = axum::Router::new().route(
        "/api/fixture/echo",
        post(
            |Extension(identity): Extension<InstallationIdentity>, bytes: Bytes| async move {
                assert_eq!(identity.role, InstallationRole::Owner);
                assert!(identity.mcp_scopes.is_none());
                assert!(identity.public_artifact.is_none());
                bytes
            },
        ),
    );
    let mut relay = RelayedInstallation::new(router).await;
    production_connector(&mut relay).await;
    let (peer, channel, _) = client(&relay).await;
    let Frame::Request(mut input) = request("large", "/api/fixture/echo") else {
        unreachable!()
    };
    input.method = "POST".into();
    input.body = vec![0xAB; 200_000];
    send_frame(channel.as_ref(), 1, &Frame::Request(input)).await;
    let Frame::Response(reply) = response(channel.as_ref()).await else {
        panic!("finite response expected")
    };
    assert_eq!(reply.status, 200);
    assert_eq!(reply.body, vec![0xAB; 200_000]);
    peer.close().await.unwrap();
    relay.close().await;
}

async fn closed(channel: &dyn DataChannel) {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            match channel.poll().await {
                None | Some(DataChannelEvent::OnClose | DataChannelEvent::OnError) => break,
                Some(DataChannelEvent::OnMessage(_)) => panic!("application data after revocation"),
                _ => {}
            }
        }
    })
    .await
    .expect("revoked DataChannel closes immediately");
}

#[tokio::test]
async fn logout_closes_the_real_peer_and_its_live_stream_without_stopping_the_installation() {
    let mut relay = RelayedInstallation::new(stream_router()).await;
    production_connector(&mut relay).await;
    let (peer, channel, _) = client(&relay).await;
    send_frame(
        channel.as_ref(),
        1,
        &request("stream", "/api/fixture/stream"),
    )
    .await;
    assert!(matches!(
        response(channel.as_ref()).await,
        Frame::StreamStart(_)
    ));
    let response = relay
        .app
        .authenticated(
            &relay.cookie,
            &relay.session,
            Method::POST,
            "/api/account/logout",
        )
        .send()
        .await
        .unwrap();
    assert!(response.status().is_success());
    closed(channel.as_ref()).await;
    assert!(!relay.installation.shutdown.is_cancelled());
    assert_eq!(
        relay.get("/chats").send().await.unwrap().status(),
        StatusCode::UNAUTHORIZED
    );
    peer.close().await.unwrap();
    relay.close().await;
}

#[tokio::test]
async fn direct_stream_quota_and_cancellation_release_capacity() {
    let mut relay = RelayedInstallation::new(stream_router()).await;
    production_connector(&mut relay).await;
    let (peer, channel, _) = client(&relay).await;
    for cycle in 0..2 {
        for index in 0..8 {
            send_frame(
                channel.as_ref(),
                1 + cycle * 100 + index,
                &request(&format!("stream-{cycle}-{index}"), "/api/fixture/stream"),
            )
            .await;
            assert!(
                matches!(response(channel.as_ref()).await, Frame::StreamStart(reply) if reply.status == 200)
            );
        }
        send_frame(
            channel.as_ref(),
            10 + cycle * 100,
            &request("over-quota", "/api/fixture/stream"),
        )
        .await;
        assert!(
            matches!(response(channel.as_ref()).await, Frame::Response(reply) if reply.status == 503)
        );
        for index in 0..8 {
            send_frame(
                channel.as_ref(),
                20 + cycle * 100 + index,
                &Frame::Cancel {
                    id: format!("stream-{cycle}-{index}"),
                },
            )
            .await;
        }
        send_frame(
            channel.as_ref(),
            30 + cycle * 100,
            &request("read", "/api/chats"),
        )
        .await;
        assert!(
            matches!(response(channel.as_ref()).await, Frame::Response(reply) if reply.status == 200)
        );
    }
    peer.close().await.unwrap();
    relay.close().await;
}

#[tokio::test]
async fn excessive_fragment_declaration_closes_only_direct_and_relay_remains_usable() {
    let mut relay = RelayedInstallation::new(axum::Router::new()).await;
    production_connector(&mut relay).await;
    let (peer, channel, _) = client(&relay).await;
    let mut packet = vec![1];
    packet.extend(1_u32.to_be_bytes());
    packet.extend((leo_relay_protocol::MAX_FRAME as u32 + 1).to_be_bytes());
    packet.extend(0_u32.to_be_bytes());
    packet.push(b'{');
    channel
        .send(bytes::BytesMut::from(packet.as_slice()))
        .await
        .unwrap();
    closed(channel.as_ref()).await;
    assert_eq!(
        relay.get("/chats").send().await.unwrap().status(),
        StatusCode::OK
    );
    let (fresh, fresh_channel, _) = client(&relay).await;
    send_frame(fresh_channel.as_ref(), 1, &request("fresh", "/api/chats")).await;
    assert!(
        matches!(response(fresh_channel.as_ref()).await, Frame::Response(reply) if reply.status == 200)
    );
    peer.close().await.unwrap();
    fresh.close().await.unwrap();
    relay.close().await;
}

#[tokio::test]
async fn disabling_direct_refuses_authorization_and_preserves_the_relay() {
    let mut relay = RelayedInstallation::new(axum::Router::new()).await;
    configured_connector(
        &mut relay,
        leo_agent_manager::direct::peer::PeerConfig {
            enabled: false,
            ..Default::default()
        },
    )
    .await;
    let id = relay.session["installations"][0]["id"].as_str().unwrap();
    let fingerprint = format!("sha-256 {}", vec!["AB"; 32].join(":"));
    let response = relay
        .app
        .authenticated(
            &relay.cookie,
            &relay.session,
            Method::POST,
            &format!("/api/installations/{id}/direct/authorize"),
        )
        .json(&json!({
            "fingerprint": fingerprint,
            "versions": [4],
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert_eq!(
        relay.get("/chats").send().await.unwrap().status(),
        StatusCode::OK
    );
    relay.close().await;
}

#[tokio::test]
async fn grant_expiry_closes_an_actual_stream_when_the_official_tunnel_is_down() {
    let mut relay = RelayedInstallation::new(stream_router()).await;
    production_connector(&mut relay).await;
    sqlx_core::query::query(
        "UPDATE web_sessions SET expires_at = clock_timestamp() + interval '5 seconds'",
    )
    .execute(&relay.app.pool)
    .await
    .unwrap();
    let (peer, channel, _) = client(&relay).await;
    send_frame(
        channel.as_ref(),
        1,
        &request("stream", "/api/fixture/stream"),
    )
    .await;
    assert!(matches!(
        response(channel.as_ref()).await,
        Frame::StreamStart(_)
    ));
    relay.app.relay.shutdown();
    relay.app.server.abort();
    tokio::time::timeout(Duration::from_secs(6), async {
        while !matches!(
            channel.poll().await,
            None | Some(DataChannelEvent::OnClose | DataChannelEvent::OnError)
        ) {}
    })
    .await
    .expect("local expiry closes the real channel without a live official tunnel");
    assert!(!relay.installation.shutdown.is_cancelled());
    peer.close().await.unwrap();
    relay.close().await;
}

#[tokio::test]
async fn a_new_official_key_closes_real_peers_and_a_fresh_peer_still_works() {
    let mut relay = RelayedInstallation::new(stream_router()).await;
    production_connector(&mut relay).await;
    let (old_peer, old_channel, grant) = client(&relay).await;
    send_frame(
        old_channel.as_ref(),
        1,
        &request("stream", "/api/fixture/stream"),
    )
    .await;
    assert!(matches!(
        response(old_channel.as_ref()).await,
        Frame::StreamStart(_)
    ));
    // Recreate the official process, retaining accounts and the live installation.
    relay.app.relay.shutdown();
    relay.app.server.abort();
    let _ = (&mut relay.app.server).await;
    let port = reqwest::Url::parse(&relay.app.url).unwrap().port().unwrap();
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
        .await
        .unwrap();
    relay.app.relay = leo_official_service::Relay::default();
    let router = leo_official_service::router_with_relay(
        relay.app.pool.clone(),
        relay.app.mail.clone(),
        relay.app.url.clone(),
        leo_official_service::OAuthProviders::default(),
        relay.app.relay.clone(),
    )
    .await
    .unwrap();
    relay.app.server = tokio::spawn(async move {
        axum::serve(
            listener,
            router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        .unwrap();
    });
    closed(old_channel.as_ref()).await;
    assert!(grant.claims.expires_at > leo_relay_protocol::direct::unix_time());
    tokio::time::timeout(Duration::from_secs(5), async {
        while relay.get("/chats").send().await.unwrap().status() != StatusCode::OK {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let (fresh_peer, fresh_channel, _) = client(&relay).await;
    send_frame(fresh_channel.as_ref(), 1, &request("fresh", "/api/chats")).await;
    assert!(
        matches!(response(fresh_channel.as_ref()).await, Frame::Response(reply) if reply.status == 200)
    );
    assert!(!relay.installation.shutdown.is_cancelled());
    old_peer.close().await.unwrap();
    fresh_peer.close().await.unwrap();
    relay.close().await;
}

#[tokio::test]
async fn a_grant_and_sdp_for_a_different_certificate_cannot_open_the_real_data_channel() {
    let mut relay = RelayedInstallation::new(axum::Router::new()).await;
    production_connector(&mut relay).await;
    let (peer, _, _) = client_with_fingerprint(&relay, &relay.cookie, &relay.session, true).await;
    assert_eq!(
        relay.get("/chats").send().await.unwrap().status(),
        StatusCode::OK
    );
    peer.close().await.unwrap();
    relay.close().await;
}

#[tokio::test]
async fn installation_stream_and_request_limits_span_real_peers_and_release_on_close() {
    let router = stream_router().route(
        "/api/fixture/wait",
        axum::routing::get(|| async {
            std::future::pending::<()>().await;
            "never"
        }),
    );
    let mut relay = RelayedInstallation::new(router).await;
    production_connector(&mut relay).await;
    let cookies = common::stream_accounts_with_members(&relay, 3).await;
    let mut peers = Vec::new();
    for (account, cookie) in cookies.into_iter().enumerate() {
        let session = relay
            .app
            .client
            .get(format!("{}/api/account/session", relay.app.url))
            .header("cookie", &cookie)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let (peer, channel, _) = client_with_fingerprint(&relay, &cookie, &session, false).await;
        for index in 0..if account < 3 { 8 } else { 0 } {
            send_frame(
                channel.as_ref(),
                index + 1,
                &request(&format!("stream-{index}"), "/api/fixture/stream"),
            )
            .await;
            assert!(
                matches!(response(channel.as_ref()).await, Frame::StreamStart(reply) if reply.status == 200)
            );
        }
        peers.push((peer, channel));
    }
    let owner = peers[0].1.as_ref();
    let extra = peers[3].1.as_ref();
    send_frame(extra, 1, &request("over-streams", "/api/fixture/stream")).await;
    assert!(matches!(response(extra).await, Frame::Response(reply) if reply.status == 503));
    for index in 0..8 {
        send_frame(
            owner,
            100 + index,
            &request(&format!("pending-{index}"), "/api/fixture/wait"),
        )
        .await;
    }
    send_frame(owner, 110, &request("over-requests", "/api/chats")).await;
    assert!(matches!(response(owner).await, Frame::Response(reply) if reply.status == 503));
    // Closing a member peer frees both its eight stream slots and request slots.
    peers[2].1.close().await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        for transfer in 111.. {
            send_frame(owner, transfer, &request("after-peer-close", "/api/chats")).await;
            if matches!(response(owner).await, Frame::Response(reply) if reply.status == 200) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("closing the DataChannel releases request and stream capacity");
    send_frame(
        extra,
        2,
        &request("stream-after-close", "/api/fixture/stream"),
    )
    .await;
    assert!(matches!(response(extra).await, Frame::StreamStart(reply) if reply.status == 200));
    assert_eq!(
        relay.get("/chats").send().await.unwrap().status(),
        StatusCode::OK
    );
    for (peer, _) in peers {
        peer.close().await.unwrap();
    }
    relay.close().await;
}

#[tokio::test]
async fn cancelling_then_reusing_a_request_id_keeps_new_stream_credit_and_tracking() {
    let mut relay = RelayedInstallation::new(stream_router()).await;
    production_connector(&mut relay).await;
    let (peer, channel, _) = client(&relay).await;
    for cycle in 0..100 {
        let transfer = 1 + cycle * 5;
        send_frame(
            channel.as_ref(),
            transfer,
            &request("reused", "/api/fixture/stream"),
        )
        .await;
        assert!(matches!(
            response(channel.as_ref()).await,
            Frame::StreamStart(_)
        ));
        send_frame(
            channel.as_ref(),
            transfer + 1,
            &Frame::Cancel {
                id: "reused".into(),
            },
        )
        .await;
        send_frame(
            channel.as_ref(),
            transfer + 2,
            &request("reused", "/api/fixture/stream"),
        )
        .await;
        send_frame(
            channel.as_ref(),
            transfer + 3,
            &Frame::StreamCredit {
                id: "reused".into(),
            },
        )
        .await;
        assert!(matches!(
            response(channel.as_ref()).await,
            Frame::StreamStart(_)
        ));
        assert!(
            matches!(response(channel.as_ref()).await, Frame::StreamChunk { body, .. } if body == b"hello-stream")
        );
        // The existing relay dispatcher requires credit for the EOF read too.
        send_frame(
            channel.as_ref(),
            transfer + 4,
            &Frame::StreamCredit {
                id: "reused".into(),
            },
        )
        .await;
        assert!(matches!(
            response(channel.as_ref()).await,
            Frame::StreamEnd { .. }
        ));
    }
    channel.close().await.unwrap();
    peer.close().await.unwrap();
    relay.close().await;
}

#[tokio::test]
async fn signal_lag_keeps_the_peer_running_and_new_connections_work() {
    let relay = RelayedInstallation::new(axum::Router::new()).await;
    let stop = tokio_util::sync::CancellationToken::new();
    let running = leo_agent_manager::direct::peer::run(
        relay.router.clone(),
        relay.direct.clone(),
        leo_agent_manager::direct::peer::PeerConfig::default(),
        stop.clone(),
    );
    tokio::pin!(running);
    // Subscribe, then pause only the peer while the real tunnel delivers a burst.
    assert!(
        tokio::time::timeout(Duration::from_millis(10), &mut running)
            .await
            .is_err()
    );
    let fingerprint = format!("sha-256 {}", vec!["AB"; 32].join(":"));
    let id = relay.session["installations"][0]["id"].as_str().unwrap();
    let grant: serde_json::Value = relay
        .app
        .authenticated(
            &relay.cookie,
            &relay.session,
            Method::POST,
            &format!("/api/installations/{id}/direct/authorize"),
        )
        .json(&json!({
            "fingerprint": fingerprint,
            "versions": [4],
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let grant: DirectAuthorization = serde_json::from_value(grant["grant"].clone()).unwrap();
    for _ in 0..70 {
        signal_as(
            &relay,
            &relay.cookie,
            &relay.session,
            &grant,
            &DirectSignal::Candidate {
                candidate: String::new(),
                sdp_mid: None,
                sdp_m_line_index: None,
            },
        )
        .await;
    }
    assert!(
        tokio::time::timeout(Duration::from_millis(10), &mut running)
            .await
            .is_err(),
        "lag must not stop the installation peer"
    );
    tokio::select! {
        () = &mut running => panic!("peer stopped after lag"),
        () = async {
            let (peer, channel, _) = client(&relay).await;
            send_frame(channel.as_ref(), 1, &request("after-lag", "/api/chats")).await;

            let reply = response(channel.as_ref()).await;
            assert!(matches!(reply, Frame::Response(reply) if reply.status == 200));
            peer.close().await.unwrap();
        } => {}
    }
    stop.cancel();
    running.await;
    assert_eq!(
        relay.get("/chats").send().await.unwrap().status(),
        StatusCode::OK
    );
    relay.close().await;
}

async fn partial_transfer(channel: &dyn DataChannel, id: u32, total: usize) {
    let mut packet = vec![1];
    packet.extend(id.to_be_bytes());
    packet.extend((total as u32).to_be_bytes());
    packet.extend(0_u32.to_be_bytes());
    packet.push(b'{');
    channel
        .send(bytes::BytesMut::from(packet.as_slice()))
        .await
        .unwrap();
}

#[tokio::test]
async fn a_member_holding_reassembly_capacity_does_not_disconnect_the_owner() {
    let router = axum::Router::new().route(
        "/api/fixture/echo",
        axum::routing::post(|bytes: axum::body::Bytes| async { bytes }),
    );
    let mut relay = RelayedInstallation::new(router).await;
    production_connector(&mut relay).await;
    let cookies = common::stream_accounts_with_members(&relay, 2).await;
    let member_cookie = &cookies[1];
    let member_session = relay
        .app
        .client
        .get(format!("{}/api/account/session", relay.app.url))
        .header("cookie", member_cookie)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let (member, member_channel, _) =
        client_with_fingerprint(&relay, member_cookie, &member_session, false).await;
    let waiting_cookie = &cookies[2];
    let waiting_session = relay
        .app
        .client
        .get(format!("{}/api/account/session", relay.app.url))
        .header("cookie", waiting_cookie)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let (waiting, waiting_channel, _) =
        client_with_fingerprint(&relay, waiting_cookie, &waiting_session, false).await;
    let (owner, owner_channel, _) = client(&relay).await;
    partial_transfer(
        member_channel.as_ref(),
        1,
        leo_relay_protocol::MAX_FRAME - 1024,
    )
    .await;
    send_frame(
        member_channel.as_ref(),
        2,
        &request("barrier", "/api/chats"),
    )
    .await;
    assert!(
        matches!(response(member_channel.as_ref()).await, Frame::Response(reply) if reply.status == 200)
    );
    partial_transfer(waiting_channel.as_ref(), 1, 200_000).await;
    send_frame(
        waiting_channel.as_ref(),
        2,
        &request("waiting-member", "/api/chats"),
    )
    .await;
    assert!(
        tokio::time::timeout(
            Duration::from_millis(100),
            response(waiting_channel.as_ref())
        )
        .await
        .is_err(),
        "members share a bounded reservation pool, preserving the owner's capacity"
    );

    let Frame::Request(mut upload) = request("owner-upload", "/api/fixture/echo") else {
        unreachable!()
    };
    upload.method = "POST".into();
    upload.body = vec![0xAB; 200_000];
    send_frame(owner_channel.as_ref(), 1, &Frame::Request(upload)).await;
    assert!(
        matches!(response(owner_channel.as_ref()).await, Frame::Response(reply) if reply.status == 200 && reply.body == vec![0xAB; 200_000])
    );
    assert_eq!(
        relay.get("/chats").send().await.unwrap().status(),
        StatusCode::OK
    );
    assert!(!relay.installation.shutdown.is_cancelled());
    let logout = relay
        .app
        .authenticated(
            waiting_cookie,
            &waiting_session,
            Method::POST,
            "/api/account/logout",
        )
        .send()
        .await
        .unwrap();
    assert!(logout.status().is_success());
    closed(waiting_channel.as_ref()).await;
    send_frame(
        owner_channel.as_ref(),
        2,
        &request("after-waiter-revocation", "/api/chats"),
    )
    .await;
    assert!(
        matches!(response(owner_channel.as_ref()).await, Frame::Response(reply) if reply.status == 200)
    );
    waiting.close().await.unwrap();
    member.close().await.unwrap();
    owner.close().await.unwrap();
    relay.close().await;
}

#[tokio::test]
async fn concurrent_large_uploads_share_account_capacity_without_closing_either_peer() {
    use leo_relay_protocol::data_channel::EncodedFrame;
    let router = axum::Router::new()
        .route(
            "/api/fixture/upload",
            axum::routing::post(|bytes: axum::body::Bytes| async move { bytes.len().to_string() }),
        )
        .layer(axum::extract::DefaultBodyLimit::max(
            leo_relay_protocol::MAX_BODY,
        ));
    let mut relay = RelayedInstallation::new(router).await;
    production_connector(&mut relay).await;
    let (first, first_channel, _) = client(&relay).await;
    let (second, second_channel, _) = client(&relay).await;
    let upload = |id| {
        let Frame::Request(mut input) = request(id, "/api/fixture/upload") else {
            unreachable!()
        };
        input.method = "POST".into();
        input.body = vec![0xAB; 4 * 1024 * 1024];
        Frame::Request(input)
    };
    let first_frame = EncodedFrame::new(1, &upload("first-upload")).unwrap();
    let mut packets = first_frame.packets();
    first_channel
        .send(bytes::BytesMut::from(packets.next().unwrap().as_slice()))
        .await
        .unwrap();
    send_frame(first_channel.as_ref(), 2, &request("barrier", "/api/chats")).await;
    assert!(
        matches!(response(first_channel.as_ref()).await, Frame::Response(reply) if reply.status == 200)
    );

    let second_upload = async {
        send_frame(second_channel.as_ref(), 1, &upload("second-upload")).await;
        response(second_channel.as_ref()).await
    };
    tokio::pin!(second_upload);
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut second_upload)
            .await
            .is_err(),
        "account reservations must apply backpressure across separate peers"
    );

    let first_upload = async {
        for packet in packets {
            first_channel
                .send(bytes::BytesMut::from(packet.as_slice()))
                .await
                .unwrap();
        }
        response(first_channel.as_ref()).await
    };
    let (first_reply, second_reply) = tokio::join!(first_upload, second_upload);
    for reply in [first_reply, second_reply] {
        let Frame::Response(reply) = reply else {
            panic!("finite upload response expected")
        };
        assert_eq!(reply.status, 200);
        assert_eq!(reply.body, b"4194304");
    }
    for (channel, transfer) in [(&first_channel, 3), (&second_channel, 2)] {
        send_frame(
            channel.as_ref(),
            transfer,
            &request("after-uploads", "/api/chats"),
        )
        .await;
        assert!(
            matches!(response(channel.as_ref()).await, Frame::Response(reply) if reply.status == 200)
        );
    }
    first.close().await.unwrap();
    second.close().await.unwrap();
    relay.close().await;
}

#[tokio::test]
async fn reassembly_waiters_survive_abort_close_and_expiry_of_the_holder() {
    let mut relay = RelayedInstallation::new(axum::Router::new()).await;
    production_connector(&mut relay).await;
    let (holder, holder_channel, _) = client(&relay).await;
    let (waiter, waiter_channel, _) = client(&relay).await;
    // Both peers belong to the same verified account, so they share its cap.
    partial_transfer(
        holder_channel.as_ref(),
        1,
        leo_relay_protocol::MAX_FRAME - 1024,
    )
    .await;
    send_frame(
        holder_channel.as_ref(),
        2,
        &request("barrier", "/api/chats"),
    )
    .await;
    assert!(
        matches!(response(holder_channel.as_ref()).await, Frame::Response(reply) if reply.status == 200)
    );
    partial_transfer(waiter_channel.as_ref(), 1, 200_000).await;
    let mut abort = vec![1];
    abort.extend(1_u32.to_be_bytes());
    abort.extend([0; 8]);
    waiter_channel
        .send(bytes::BytesMut::from(abort.as_slice()))
        .await
        .unwrap();
    send_frame(
        waiter_channel.as_ref(),
        2,
        &request("after-abort", "/api/chats"),
    )
    .await;
    let reply = response(waiter_channel.as_ref());
    tokio::pin!(reply);
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut reply)
            .await
            .is_err()
    );
    holder_channel
        .send(bytes::BytesMut::from(abort.as_slice()))
        .await
        .unwrap();
    assert!(matches!(reply.await, Frame::Response(reply) if reply.status == 200));

    partial_transfer(
        holder_channel.as_ref(),
        3,
        leo_relay_protocol::MAX_FRAME - 1024,
    )
    .await;
    send_frame(
        holder_channel.as_ref(),
        4,
        &request("close-barrier", "/api/chats"),
    )
    .await;
    assert!(
        matches!(response(holder_channel.as_ref()).await, Frame::Response(reply) if reply.status == 200)
    );
    partial_transfer(waiter_channel.as_ref(), 3, 200_000).await;
    let mut abort_waiter = abort.clone();
    abort_waiter[1..5].copy_from_slice(&3_u32.to_be_bytes());
    waiter_channel
        .send(bytes::BytesMut::from(abort_waiter.as_slice()))
        .await
        .unwrap();
    send_frame(
        waiter_channel.as_ref(),
        4,
        &request("after-close", "/api/chats"),
    )
    .await;
    holder_channel.close().await.unwrap();
    assert!(
        matches!(response(waiter_channel.as_ref()).await, Frame::Response(reply) if reply.status == 200)
    );
    holder.close().await.unwrap();

    // Expiry releases this peer's own reservation while it is backpressured.
    // It then drains late fragments and continues serving fresh requests.
    partial_transfer(
        waiter_channel.as_ref(),
        5,
        leo_relay_protocol::MAX_FRAME - 1024,
    )
    .await;
    send_frame(
        waiter_channel.as_ref(),
        6,
        &request("expiry-barrier", "/api/chats"),
    )
    .await;
    assert!(
        matches!(response(waiter_channel.as_ref()).await, Frame::Response(reply) if reply.status == 200)
    );
    partial_transfer(waiter_channel.as_ref(), 7, 200_000).await;
    abort_waiter[1..5].copy_from_slice(&7_u32.to_be_bytes());
    waiter_channel
        .send(bytes::BytesMut::from(abort_waiter.as_slice()))
        .await
        .unwrap();
    send_frame(
        waiter_channel.as_ref(),
        8,
        &request("after-expiry", "/api/chats"),
    )
    .await;
    tokio::time::sleep(Duration::from_secs(31)).await;
    assert!(
        matches!(response(waiter_channel.as_ref()).await, Frame::Response(reply) if reply.status == 200)
    );
    let mut late = vec![1];
    late.extend(5_u32.to_be_bytes());
    late.extend(((leo_relay_protocol::MAX_FRAME - 1024) as u32).to_be_bytes());
    late.extend(1_u32.to_be_bytes());
    late.push(b'}');
    waiter_channel
        .send(bytes::BytesMut::from(late.as_slice()))
        .await
        .unwrap();
    abort_waiter[1..5].copy_from_slice(&5_u32.to_be_bytes());
    waiter_channel
        .send(bytes::BytesMut::from(abort_waiter.as_slice()))
        .await
        .unwrap();
    send_frame(
        waiter_channel.as_ref(),
        9,
        &request("after-expired-tail", "/api/chats"),
    )
    .await;
    assert!(
        matches!(response(waiter_channel.as_ref()).await, Frame::Response(reply) if reply.status == 200)
    );
    waiter.close().await.unwrap();
    assert!(!relay.installation.shutdown.is_cancelled());
    relay.close().await;
}

#[tokio::test]
async fn observed_certificate_is_compared_with_the_grant_without_an_sdp_check() {
    let relay = RelayedInstallation::new(axum::Router::new()).await;
    // Public test certificate, generated with OpenSSL; no SDP or DTLS SDK check.
    // Expected SHA-256 comes from openssl x509 -fingerprint -sha256.
    let observed = include_bytes!("fixtures/direct-certificate.der");
    let fingerprint = "sha-256 2E:13:F5:6D:43:F6:75:E6:CA:09:EC:E3:F2:49:8E:47:F0:22:FE:83:FA:95:72:80:33:86:64:D1:6D:25:AF:A2";
    let id = relay.session["installations"][0]["id"].as_str().unwrap();
    let grant: serde_json::Value = relay
        .app
        .authenticated(
            &relay.cookie,
            &relay.session,
            Method::POST,
            &format!("/api/installations/{id}/direct/authorize"),
        )
        .json(&json!({
            "fingerprint": fingerprint,
            "versions": [4],
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let grant: DirectAuthorization = serde_json::from_value(grant["grant"].clone()).unwrap();
    let mut substituted = observed.to_vec();
    substituted[0] ^= 1;
    assert!(
        relay
            .direct
            .accept_certificate(&grant.claims.connection_id, &substituted)
            .is_err()
    );
    let lease = relay
        .direct
        .accept_certificate(&grant.claims.connection_id, observed)
        .unwrap();
    assert_eq!(lease.claims, grant.claims);
    assert!(
        relay
            .direct
            .accept_certificate(&grant.claims.connection_id, observed)
            .is_err()
    );
    relay.close().await;
}

struct PanicPeerStart(std::sync::Arc<std::sync::atomic::AtomicBool>);

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for PanicPeerStart {
    fn on_event(&self, event: &tracing::Event<'_>, _: tracing_subscriber::layer::Context<'_, S>) {
        if event.metadata().target() == "leo_agent_manager::direct::peer"
            && *event.metadata().level() == tracing::Level::DEBUG
            && self.0.swap(false, std::sync::atomic::Ordering::SeqCst)
        {
            panic!("fixture peer task failure");
        }
    }
}

#[tokio::test]
async fn a_panicking_peer_task_releases_its_authorization_and_preserves_other_peers() {
    use tracing_subscriber::prelude::*;
    let panic_next = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let subscriber = tracing_subscriber::registry().with(PanicPeerStart(panic_next.clone()));
    let _subscriber = tracing::subscriber::set_default(subscriber);
    let relay = RelayedInstallation::new(axum::Router::new()).await;
    let stop = tokio_util::sync::CancellationToken::new();
    let running = tokio::spawn(leo_agent_manager::direct::peer::run(
        relay.router.clone(),
        relay.direct.clone(),
        leo_agent_manager::direct::peer::PeerConfig::default(),
        stop.clone(),
    ));
    let (survivor, survivor_channel, _) = client(&relay).await;
    // Inject a fault through the scoped logging adapter in the actual Tokio peer
    // job. No production fault flag, alternate peer or SDP-induced SDK panic.
    let fingerprint = format!("sha-256 {}", vec!["AB"; 32].join(":"));
    let installation = relay.session["installations"][0]["id"].as_str().unwrap();
    let value: serde_json::Value = relay
        .app
        .authenticated(
            &relay.cookie,
            &relay.session,
            Method::POST,
            &format!("/api/installations/{installation}/direct/authorize"),
        )
        .json(&json!({
            "fingerprint": fingerprint,
            "versions": [4],
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let grant: DirectAuthorization = serde_json::from_value(value["grant"].clone()).unwrap();
    // The lease token observes the same authorization cleanup as a live channel.
    let lease = relay
        .direct
        .accept_peer(&grant, &grant.claims.session_id, &fingerprint)
        .unwrap();
    panic_next.store(true, std::sync::atomic::Ordering::SeqCst);
    signal_as(&relay, &relay.cookie, &relay.session, &grant, &DirectSignal::Offer {
        sdp: format!("v=0\r\ns=-\r\nt=0 0\r\nm=application 9 UDP/DTLS/SCTP webrtc-datachannel\r\na=fingerprint:{fingerprint}\r\na=sctp-port:5000\r\n"),
    }).await;
    tokio::time::timeout(Duration::from_secs(2), lease.closed.cancelled())
        .await
        .expect("JoinError cleanup must cancel the panicking job's lease");
    assert!(
        !panic_next.load(std::sync::atomic::Ordering::SeqCst),
        "fault must execute in the peer task"
    );
    let error = relay
        .direct
        .accept_peer(&grant, &grant.claims.session_id, &grant.claims.fingerprint)
        .err()
        .expect("panicking task must release its authorization");
    assert_eq!(error.message, "Unknown direct authorization.");
    send_frame(
        survivor_channel.as_ref(),
        1,
        &request("after-panic", "/api/chats"),
    )
    .await;
    assert!(
        matches!(response(survivor_channel.as_ref()).await, Frame::Response(reply) if reply.status == 200)
    );
    let (fresh, fresh_channel, _) = client(&relay).await;
    send_frame(fresh_channel.as_ref(), 1, &request("fresh", "/api/chats")).await;
    assert!(
        matches!(response(fresh_channel.as_ref()).await, Frame::Response(reply) if reply.status == 200)
    );
    assert_eq!(
        relay.get("/chats").send().await.unwrap().status(),
        StatusCode::OK
    );
    assert!(!relay.installation.shutdown.is_cancelled());
    survivor.close().await.unwrap();
    fresh.close().await.unwrap();
    stop.cancel();
    running.await.unwrap();
    relay.close().await;
}
