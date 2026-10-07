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
    relay.stop.cancel();
    (&mut relay.connector).await.unwrap().unwrap();
    relay.stop = tokio_util::sync::CancellationToken::new();
    relay.connector = tokio::spawn(leo_agent_manager::relay::connect(
        relay
            .installation
            .config
            .data_dir
            .join("installation-relay"),
        relay.router.clone(),
        relay.installation.clone(),
        relay.stop.clone(),
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

async fn signal(relay: &RelayedInstallation, grant: &DirectAuthorization, signal: &DirectSignal) {
    let path = format!(
        "/api/installations/{}/direct/{}/signal",
        grant.claims.installation_id, grant.claims.connection_id
    );
    let response = relay
        .app
        .authenticated(&relay.cookie, &relay.session, Method::POST, &path)
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
    let (events, mut candidates) = mpsc::channel(32);
    let peer: Arc<dyn PeerConnection> = Arc::new(
        PeerConnectionBuilder::new()
            .with_handler(Arc::new(ClientEvents(events)))
            .with_udp_addrs(vec!["127.0.0.1:0"])
            .build()
            .await
            .unwrap(),
    );
    let channel = peer.create_data_channel("leo.v4", None).await.unwrap();
    let mut offer = peer.create_offer(None).await.unwrap();
    offer.sdp = offer
        .sdp
        .lines()
        .map(|line| {
            if let Some(fingerprint) = line.strip_prefix("a=fingerprint:sha-256 ") {
                format!("a=fingerprint:sha-256 {}", fingerprint.to_ascii_uppercase())
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
        .authenticated(&relay.cookie, &relay.session, Method::POST, &path)
        .json(&json!({ "fingerprint": fingerprint, "versions": [4] }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let value: serde_json::Value = response.json().await.unwrap();
    let grant: DirectAuthorization = serde_json::from_value(value["grant"].clone()).unwrap();
    peer.set_local_description(offer.clone()).await.unwrap();
    signal(relay, &grant, &DirectSignal::Offer { sdp: offer.sdp }).await;
    let path = format!(
        "{}/api/installations/{}/direct/{}/events",
        relay.app.url, grant.claims.installation_id, grant.claims.connection_id
    );
    let mut events = relay
        .app
        .client
        .get(path)
        .header("cookie", &relay.cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(events.status(), StatusCode::OK);
    tokio::time::timeout(Duration::from_secs(5), async {
        let mut buffered = String::new();
        loop {
            tokio::select! {
                Some(candidate) = candidates.recv() => signal(relay, &grant, &candidate).await,
                chunk = events.chunk() => {
                    buffered.push_str(std::str::from_utf8(&chunk.unwrap().expect("signaling remains open")).unwrap());
                    while let Some(end) = buffered.find("\n\n") {
                        let event: String = buffered.drain(..end + 2).collect();
                        for line in event.lines() {
                            if let Some(data) = line.strip_prefix("data: ") {
                                match serde_json::from_str::<DirectSignal>(data).unwrap() {
                                    DirectSignal::Answer { sdp } => peer.set_remote_description(RTCSessionDescription::answer(sdp).unwrap()).await.unwrap(),
                                    DirectSignal::Candidate { candidate, sdp_mid, sdp_m_line_index } => {
                                        peer.add_ice_candidate(webrtc::peer_connection::RTCIceCandidateInit {
                                            candidate, sdp_mid, sdp_mline_index: sdp_m_line_index, ..Default::default()
                                        }).await.unwrap();
                                    }
                                    _ => panic!("unexpected installation offer"),
                                }
                            }
                        }
                    }
                }
                event = channel.poll() => {
                    if matches!(event, Some(DataChannelEvent::OnOpen)) { break; }
                }
            }
        }
    }).await.expect("authorized real DataChannel must open");
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
