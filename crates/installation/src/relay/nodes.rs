//! The installation side of a node channel. Beacon pairs a node's connection
//! with a WebSocket opened here; the manager answers the node routes on it as
//! if the node had connected directly. Each request still carries the node's
//! own enrollment code, token or disk grant.
use crate::error::{Error, Result};
use axum::{
    Router,
    body::Body,
    extract::ConnectInfo,
    http::{HeaderValue, Request, Response, StatusCode},
};
use cairn_protocol::MAX_NODE_CHANNEL_MESSAGE;
use futures_util::{SinkExt, StreamExt};
use hyper::server::conn::http1;
use hyper_util::{rt::TokioIo, service::TowerToHyperService};
use std::{
    convert::Infallible,
    net::{IpAddr, SocketAddr},
    time::Duration,
};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio_tungstenite::{
    WebSocketStream,
    tungstenite::{Message, client::IntoClientRequest, protocol::WebSocketConfig},
};
use tower::ServiceExt;

const CHUNK: usize = 65_536;
const BUFFER: usize = 256 * 1024;
// Beacon pings every 15 seconds; silence beyond this means the path is gone.
const SILENCE: Duration = Duration::from_secs(60);

/// Only routes a remote node calls with its own credentials. The manager's own
/// `/internal/execution/` calls, the API and every other machine channel stay
/// unreachable through Beacon.
fn node_route(path: &str) -> bool {
    (path.starts_with("/internal/nodes/")
        || path.starts_with("/internal/node-restore/")
        || path.starts_with("/internal/node-workspace/"))
        && !path
            .split('/')
            .any(|segment| segment == "." || segment == "..")
        && !path.contains('\\')
        // Encoded dots and separators have no use in node routes.
        && !["%2e", "%2f", "%5c"]
            .iter()
            .any(|encoded| path.to_ascii_lowercase().contains(encoded))
}

/// Opens this installation's side of channel `id` and serves it until either side closes.
pub(super) async fn accept(
    beacon: &url::Url,
    installation: &str,
    token: &str,
    id: &str,
    address: Option<&str>,
    router: Router,
) -> Result<()> {
    crate::validation::uuid(id)?;
    let mut url = beacon
        .join(&format!("api/relay/{installation}/nodes/{id}/accept"))
        .map_err(Error::internal)?;
    let scheme = if beacon.scheme() == "https" {
        "wss"
    } else {
        "ws"
    };
    url.set_scheme(scheme)
        .map_err(|()| Error::bad("Invalid relay origin."))?;
    let mut request = url
        .as_str()
        .into_client_request()
        .map_err(Error::internal)?;
    request.headers_mut().insert(
        "authorization",
        HeaderValue::from_str(&format!("Bearer {token}"))
            .map_err(|_| Error::bad("Invalid installation identity."))?,
    );
    let config = WebSocketConfig::default()
        .max_message_size(Some(MAX_NODE_CHANNEL_MESSAGE))
        .max_frame_size(Some(MAX_NODE_CHANNEL_MESSAGE));
    let (socket, _) = tokio::time::timeout(
        Duration::from_secs(10),
        tokio_tungstenite::connect_async_with_config(request, Some(config), false),
    )
    .await
    .map_err(|_| Error::unavailable("Node channel connection timed out."))?
    .map_err(|_| Error::unavailable("Node channel refused."))?;

    let address = address
        .and_then(|value| value.parse::<IpAddr>().ok())
        .unwrap_or(IpAddr::from([0, 0, 0, 0]));
    serve(socket, router, SocketAddr::new(address, 0)).await;
    Ok(())
}

/// Answers HTTP/1.1 carried as binary messages on `socket`. `peer` is the
/// node's address as Beacon saw it, used for rate limiting.
pub(crate) async fn serve<S>(socket: WebSocketStream<S>, router: Router, peer: SocketAddr)
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let (http_side, channel_side) = tokio::io::duplex(BUFFER);
    let (mut from_channel, mut to_http) = tokio::io::split(channel_side);
    let (mut sink, mut stream) = socket.split();

    // Pings are answered by tungstenite while reading.
    let inbound = async move {
        while let Ok(Some(Ok(message))) = tokio::time::timeout(SILENCE, stream.next()).await {
            match message {
                Message::Binary(bytes) => {
                    if to_http.write_all(&bytes).await.is_err() {
                        break;
                    }
                }
                Message::Ping(_) | Message::Pong(_) => {}
                _ => break,
            }
        }
        let _ = to_http.shutdown().await;
    };

    // Drains every response byte before closing, so a finished connection is never truncated.
    let outbound = async move {
        let mut buffer = vec![0; CHUNK];
        loop {
            match from_channel.read(&mut buffer).await {
                Ok(0) | Err(_) => break,
                Ok(read) => {
                    let message = Message::Binary(buffer[..read].to_vec().into());
                    if sink.send(message).await.is_err() {
                        return;
                    }
                }
            }
        }
        let _ = sink.send(Message::Close(None)).await;
    };

    let service = tower::service_fn(move |mut request: Request<hyper::body::Incoming>| {
        let router = router.clone();
        async move {
            if !node_route(request.uri().path()) {
                let refused = Response::builder()
                    .status(StatusCode::NOT_FOUND)
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"error":"Only node routes are available through Beacon."}"#,
                    ))
                    .unwrap();
                return Ok::<_, Infallible>(refused);
            }
            request.extensions_mut().insert(ConnectInfo(peer));
            router.oneshot(request.map(Body::new)).await
        }
    });
    let connection = http1::Builder::new()
        .serve_connection(TokioIo::new(http_side), TowerToHyperService::new(service));
    let connection = async move {
        if let Err(error) = connection.await {
            tracing::debug!(%error, "Node channel connection closed");
        }
    };

    tokio::join!(connection, inbound, outbound);
}

#[cfg(test)]
mod tests {
    use super::node_route;

    #[test]
    fn only_node_routes_cross_beacon() {
        for path in [
            "/internal/nodes/enroll",
            "/internal/nodes/heartbeat",
            "/internal/node-restore/abc",
            "/internal/node-workspace/abc/blocks",
        ] {
            assert!(node_route(path), "{path}");
        }
        for path in [
            "/api/chats",
            "/internal/execution/health",
            "/internal/nodes",
            "/internal/nodes/../execution/health",
            "/internal/nodes/./heartbeat",
            "/internal/nodes/a\\..\\execution",
            "/internal/nodesx/enroll",
            "/internal/nodes/%2e%2e/execution/health",
            "/internal/nodes/..%2Fexecution",
        ] {
            assert!(!node_route(path), "{path}");
        }
    }
}
