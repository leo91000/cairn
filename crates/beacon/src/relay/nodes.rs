//! Nodes added without a manager address. Beacon introduces a node to its
//! manager: it forwards the node's WebRTC offer and the installation's answer,
//! after which they talk directly. When the networks prevent that, Beacon
//! pairs the node's WebSocket with one the installation opens and copies the
//! bytes: TLS that ends on the manager certificate the node pinned, so Beacon
//! cannot read them. The manager still authenticates every node request.
use super::{Tunnel, active_slots, identity_is_current};
use crate::{ApiError, Service, digest};
use axum::{
    Json,
    extract::{
        ConnectInfo, DefaultBodyLimit, Path, State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use cairn_protocol::{
    Frame, MAX_NODE_CHANNEL_MESSAGE, MAX_NODE_CHANNELS_PER_ADDRESS, NODE_CHANNEL_VERSION,
};
use futures_util::{
    SinkExt, StreamExt,
    stream::{SplitSink, SplitStream},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{net::SocketAddr, sync::Arc, time::Duration};
use tokio::sync::{oneshot, watch};

const ACCEPT_TIMEOUT: Duration = Duration::from_secs(10);
const HEARTBEAT: Duration = Duration::from_secs(15);
const SILENCE: Duration = Duration::from_secs(45);
// Python supervisors open one channel per request; this still bounds abuse.
const OPENED_PER_MINUTE: i32 = 600;
// A node offers again only when its direct connection ends.
const OFFERS_PER_MINUTE: i32 = 30;
const ANSWER_TIMEOUT: Duration = Duration::from_secs(15);

const OFFLINE: ApiError = ApiError::Http(
    StatusCode::SERVICE_UNAVAILABLE,
    "This installation is offline or must be updated before nodes can connect through Cairn.",
);

const BUSY: ApiError = ApiError::Http(
    StatusCode::SERVICE_UNAVAILABLE,
    "Too many node connections to this installation. Try again later.",
);

fn installation_id(installation: &str) -> Result<(), ApiError> {
    uuid::Uuid::parse_str(installation)
        .map(|_| ())
        .map_err(|_| ApiError::Http(StatusCode::NOT_FOUND, "Installation not found"))
}

/// The STUN server a node uses to find its public address before offering.
pub(crate) async fn ice(
    State(service): State<Service>,
    Path(installation): Path<String>,
) -> Result<Json<Value>, ApiError> {
    installation_id(&installation)?;
    Ok(Json(json!({
        "iceServers": [{ "urls": [service.relay.stun_url(&service.origin)] }],
    })))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Offer {
    sdp: String,
}

pub(crate) fn offer_limit() -> DefaultBodyLimit {
    DefaultBodyLimit::max(cairn_protocol::nodes::MAX_SDP + 1024)
}

/// Forwards a node's offer, with its candidates, and returns the
/// installation's answer. The pinned DTLS certificate in that answer is what
/// the node trusts, not Beacon.
pub(crate) async fn offer(
    State(service): State<Service>,
    Path(installation): Path<String>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Json(input): Json<Offer>,
) -> Result<Json<Value>, ApiError> {
    installation_id(&installation)?;
    if input.sdp.len() > cairn_protocol::nodes::MAX_SDP {
        return Err(ApiError::Http(StatusCode::BAD_REQUEST, "Offer too large"));
    }
    let address = crate::network::rate_limit_address(peer.ip());
    crate::consume_limit(
        &service.pool,
        &format!("node-offer:{address}"),
        OFFERS_PER_MINUTE,
    )
    .await?;
    let tunnel = current_tunnel(&service, &installation)?;
    let id = uuid::Uuid::new_v4().to_string();
    let (reply, answer) = oneshot::channel();
    {
        let mut offers = tunnel.node_offers.lock().unwrap();
        if offers.len() >= cairn_protocol::nodes::MAX_PEERS {
            return Err(BUSY);
        }
        offers.insert(id.clone(), reply);
    }
    let request = Frame::NodeOffer {
        id: id.clone(),
        sdp: input.sdp,
        address: Some(peer.ip().to_string()),
    };
    let sent = tunnel.node_requests.try_send(request).is_ok();
    let answer = if sent {
        tokio::time::timeout(ANSWER_TIMEOUT, answer).await.ok()
    } else {
        None
    };
    tunnel.node_offers.lock().unwrap().remove(&id);
    match answer {
        Some(Ok(Some(sdp))) => Ok(Json(json!({ "sdp": sdp }))),
        _ => Err(ApiError::Http(
            StatusCode::SERVICE_UNAVAILABLE,
            "The installation did not accept a direct connection.",
        )),
    }
}

fn current_tunnel(service: &Service, installation: &str) -> Result<Arc<Tunnel>, ApiError> {
    service
        .relay
        .connections
        .lock()
        .unwrap()
        .get(installation)
        .cloned()
        .filter(|tunnel| !*tunnel.stop.borrow() && tunnel.version >= NODE_CHANNEL_VERSION)
        .ok_or(OFFLINE)
}

/// The node side. Anyone may ask; the manager behind it still requires its
/// enrollment code, node token or disk grant for every request.
pub(crate) async fn connect(
    State(service): State<Service>,
    Path(installation): Path<String>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    ws: WebSocketUpgrade,
) -> Result<Response, ApiError> {
    installation_id(&installation)?;
    let address = crate::network::rate_limit_address(peer.ip());
    crate::consume_limit(
        &service.pool,
        &format!("node-channel:{address}"),
        OPENED_PER_MINUTE,
    )
    .await?;

    let tunnel = current_tunnel(&service, &installation)?;
    let permit = tunnel
        .node_slots
        .clone()
        .try_acquire_owned()
        .map_err(|_| BUSY)?;
    let address_permit = active_slots(
        &tunnel.node_address_slots,
        &address,
        MAX_NODE_CHANNELS_PER_ADDRESS,
    )
    .try_acquire_owned()
    .map_err(|_| BUSY)?;

    let id = uuid::Uuid::new_v4().to_string();
    let (accepted, acceptance) = oneshot::channel();
    tunnel
        .node_channels
        .lock()
        .unwrap()
        .insert(id.clone(), accepted);
    let request = Frame::NodeChannel {
        id: id.clone(),
        address: Some(peer.ip().to_string()),
    };
    if tunnel.node_requests.try_send(request).is_err() {
        tunnel.node_channels.lock().unwrap().remove(&id);
        return Err(BUSY);
    }

    // Answer the node only once the installation is connected, so a refusal
    // is an HTTP error the node can report instead of a closed socket.
    let installation_socket = tokio::time::timeout(ACCEPT_TIMEOUT, acceptance).await;
    tunnel.node_channels.lock().unwrap().remove(&id);
    let Ok(Ok(installation_socket)) = installation_socket else {
        return Err(OFFLINE);
    };

    Ok(ws
        .max_message_size(MAX_NODE_CHANNEL_MESSAGE)
        .max_frame_size(MAX_NODE_CHANNEL_MESSAGE)
        .on_upgrade(move |node| async move {
            let _permits = (permit, address_permit);
            splice(node, installation_socket, &tunnel).await;
        })
        .into_response())
}

/// The installation side, authenticated by its current identity.
pub(crate) async fn accept(
    State(service): State<Service>,
    Path((installation, channel)): Path<(String, String)>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Result<Response, ApiError> {
    let token = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .unwrap_or("");
    if !identity_is_current(&service, &installation, &digest(token)).await? {
        return Err(ApiError::Http(
            StatusCode::UNAUTHORIZED,
            "Invalid installation identity",
        ));
    }

    let accepted = service
        .relay
        .connections
        .lock()
        .unwrap()
        .get(&installation)
        .and_then(|tunnel| tunnel.node_channels.lock().unwrap().remove(&channel))
        .ok_or(ApiError::Http(
            StatusCode::NOT_FOUND,
            "Unknown node channel",
        ))?;

    Ok(ws
        .max_message_size(MAX_NODE_CHANNEL_MESSAGE)
        .max_frame_size(MAX_NODE_CHANNEL_MESSAGE)
        .on_upgrade(move |socket| async move {
            let _ = accepted.send(socket);
        })
        .into_response())
}

/// Copies binary messages both ways until either side closes or goes silent,
/// or the installation's tunnel ends (disconnection, detach or revocation).
async fn splice(node: WebSocket, installation: WebSocket, tunnel: &Arc<Tunnel>) {
    let mut stopped: watch::Receiver<bool> = tunnel.stop.subscribe();
    if *stopped.borrow() {
        return;
    }
    let (node_sink, node_stream) = node.split();
    let (installation_sink, installation_stream) = installation.split();

    tokio::select! {
        () = forward(node_stream, installation_sink) => {}
        () = forward(installation_stream, node_sink) => {}
        _ = stopped.changed() => {}
    }
}

/// Each direction pings its destination and watches its source: a pong from
/// the destination arrives on the opposite direction's source.
async fn forward(mut from: SplitStream<WebSocket>, mut to: SplitSink<WebSocket, Message>) {
    let mut heartbeat =
        tokio::time::interval_at(tokio::time::Instant::now() + HEARTBEAT, HEARTBEAT);
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut received = tokio::time::Instant::now();

    loop {
        tokio::select! {
            message = from.next() => {
                received = tokio::time::Instant::now();
                match message {
                    Some(Ok(Message::Binary(bytes))) => {
                        if to.send(Message::Binary(bytes)).await.is_err() {
                            return;
                        }
                    }
                    Some(Ok(Message::Ping(_) | Message::Pong(_))) => {}
                    _ => {
                        let _ = to.send(Message::Close(None)).await;
                        return;
                    }
                }
            }
            _ = heartbeat.tick() => {
                if received.elapsed() > SILENCE {
                    return;
                }
                if to.send(Message::Ping(Vec::new().into())).await.is_err() {
                    return;
                }
            }
        }
    }
}
