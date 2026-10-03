use super::{ApiError, Service, digest, installations};
use axum::{
    body::{Body, to_bytes},
    extract::{
        Path, Request, State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    http::{HeaderMap, HeaderName, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
};
use futures_util::{SinkExt, StreamExt};
use leo_relay_protocol::{
    ApiRequest, ApiResponse, Frame, MAX_BODY, MAX_FRAME, MAX_IN_FLIGHT, PROTOCOL_VERSION,
    REQUEST_TIMEOUT, Role,
};
use sqlx_core::query_as::query_as;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc, oneshot, watch};

struct Pending {
    reply: oneshot::Sender<ApiResponse>,
    // Browser cancellation does not release a slot for work still running remotely.
    _permit: OwnedSemaphorePermit,
}

struct Command {
    request: ApiRequest,
    pending: Pending,
}

struct Tunnel {
    commands: mpsc::Sender<Command>,
    slots: Arc<Semaphore>,
    // Replacing a connection closes the old generation and its pending replies.
    stop: watch::Sender<bool>,
}

#[derive(Clone, Default)]
pub(super) struct Relay(Arc<Mutex<HashMap<String, Arc<Tunnel>>>>);

pub(super) async fn upgrade(
    State(service): State<Service>,
    Path(installation): Path<String>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Result<Response, ApiError> {
    let token = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .unwrap_or("");
    let row: Option<(String,)> =
        query_as("SELECT id FROM installations WHERE id = $1 AND token_digest = $2")
            .bind(&installation)
            .bind(digest(token))
            .fetch_optional(&service.pool)
            .await?;
    if row.is_none() {
        return Err(ApiError(
            StatusCode::UNAUTHORIZED,
            "Invalid installation identity",
        ));
    }

    Ok(ws
        .max_message_size(MAX_FRAME)
        .max_frame_size(MAX_FRAME)
        .on_upgrade(move |socket| serve_socket(service.relay, installation, socket))
        .into_response())
}

async fn serve_socket(relay: Relay, installation: String, mut socket: WebSocket) {
    let hello = tokio::time::timeout(Duration::from_secs(5), socket.next()).await;
    let Ok(Some(Ok(Message::Text(hello)))) = hello else {
        return;
    };
    let Ok(Frame::Hello { versions }) = serde_json::from_str::<Frame>(&hello) else {
        return;
    };
    if !versions.contains(&PROTOCOL_VERSION) {
        let _ = socket.close().await;
        return;
    }
    let welcome = serde_json::to_string(&Frame::Welcome {
        version: PROTOCOL_VERSION,
    })
    .unwrap();
    if socket.send(Message::Text(welcome.into())).await.is_err() {
        return;
    }

    let (commands, mut receiver) = mpsc::channel::<Command>(MAX_IN_FLIGHT);
    let (stop, mut stopped) = watch::channel(false);
    let tunnel = Arc::new(Tunnel {
        commands,
        slots: Arc::new(Semaphore::new(MAX_IN_FLIGHT)),
        stop,
    });
    if let Some(previous) = relay
        .0
        .lock()
        .unwrap()
        .insert(installation.clone(), tunnel.clone())
    {
        let _ = previous.stop.send(true);
    }

    let mut pending = HashMap::<String, Pending>::new();
    let mut heartbeat = tokio::time::interval(Duration::from_secs(15));
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut received = tokio::time::Instant::now();
    loop {
        tokio::select! {
            _ = stopped.changed() => break,
            _ = heartbeat.tick() => {
                if received.elapsed() > Duration::from_secs(45) {
                    break;
                }
                if socket.send(Message::Ping(Vec::new().into())).await.is_err() {
                    break;
                }
            }
            command = receiver.recv() => {
                let Some(command) = command else {
                    break;
                };
                if command.pending.reply.is_closed() {
                    continue;
                }

                let id = command.request.id.clone();
                let message = serde_json::to_string(&Frame::Request(command.request)).unwrap();
                pending.insert(id, command.pending);
                if socket.send(Message::Text(message.into())).await.is_err() {
                    break;
                }
            }
            message = socket.next() => {
                received = tokio::time::Instant::now();
                match message {
                    Some(Ok(Message::Text(message))) => {
                        let Ok(Frame::Response(response)) = serde_json::from_str::<Frame>(&message) else {
                            break;
                        };
                        if response.body.len() > MAX_BODY {
                            break;
                        }

                        if let Some(completed) = pending.remove(&response.id) {
                            let _ = completed.reply.send(response);
                        }
                    }
                    Some(Ok(Message::Ping(_) | Message::Pong(_))) => {}
                    _ => break,
                }
            }
        }
    }
    // An old connection must never remove the replacement's registry entry.
    let mut connections = relay.0.lock().unwrap();
    if connections
        .get(&installation)
        .is_some_and(|current| Arc::ptr_eq(current, &tunnel))
    {
        connections.remove(&installation);
    }
}

pub(super) async fn forward(
    State(service): State<Service>,
    Path((installation, path)): Path<(String, String)>,
    request: Request,
) -> Result<Response, ApiError> {
    let account = installations::account(&service, request.headers(), request.method()).await?;
    let owner: Option<(String,)> =
        query_as("SELECT owner_id FROM installations WHERE id = $1 AND owner_id = $2")
            .bind(&installation)
            .bind(&account)
            .fetch_optional(&service.pool)
            .await?;
    if owner.is_none() {
        return Err(ApiError(StatusCode::NOT_FOUND, "Installation not found"));
    }

    // Preserve the original encoding and query; Path decoding is only for routing.
    let prefix = format!("/api/installations/{installation}");
    let target = request
        .uri()
        .path_and_query()
        .unwrap()
        .as_str()
        .strip_prefix(&prefix)
        .unwrap_or("");
    if !leo_relay_protocol::api_path(target) || path.is_empty() {
        return Err(ApiError(
            StatusCode::NOT_FOUND,
            "Installation API route not found",
        ));
    }
    if target.split('?').next().unwrap_or("").ends_with("/stream") {
        return Err(ApiError(
            StatusCode::NOT_IMPLEMENTED,
            "Streaming relay is not available yet",
        ));
    }

    // Reserve capacity before reading the body, including requests not yet sent.
    let tunnel = service
        .relay
        .0
        .lock()
        .unwrap()
        .get(&installation)
        .cloned()
        .ok_or(ApiError(
            StatusCode::SERVICE_UNAVAILABLE,
            "Installation unavailable",
        ))?;
    let permit = tunnel
        .slots
        .clone()
        .try_acquire_owned()
        .map_err(|_| ApiError(StatusCode::SERVICE_UNAVAILABLE, "Installation busy"))?;

    let headers = request
        .headers()
        .iter()
        .filter(|(name, _)| leo_relay_protocol::request_header(name.as_str()))
        .filter_map(|(name, value)| {
            value
                .to_str()
                .ok()
                .map(|value| (name.to_string(), value.to_owned()))
        })
        .collect();
    let api_request = ApiRequest {
        id: uuid::Uuid::new_v4().to_string(),
        account_id: account,
        role: Role::Owner,
        method: request.method().to_string(),
        path: target.to_owned(),
        headers,
        body: to_bytes(request.into_body(), MAX_BODY)
            .await
            .map_err(|_| ApiError(StatusCode::PAYLOAD_TOO_LARGE, "API request is too large"))?
            .to_vec(),
    };
    let (reply, response) = oneshot::channel();
    tunnel
        .commands
        .try_send(Command {
            request: api_request,
            pending: Pending {
                reply,
                _permit: permit,
            },
        })
        .map_err(|_| {
            ApiError(
                StatusCode::SERVICE_UNAVAILABLE,
                "Installation busy or unavailable",
            )
        })?;
    let response = tokio::time::timeout(REQUEST_TIMEOUT, response)
        .await
        .map_err(|_| {
            ApiError(
                StatusCode::GATEWAY_TIMEOUT,
                "Installation request timed out",
            )
        })?
        .map_err(|_| ApiError(StatusCode::BAD_GATEWAY, "Installation connection lost"))?;
    let status = StatusCode::from_u16(response.status)
        .map_err(|_| ApiError(StatusCode::BAD_GATEWAY, "Invalid installation response"))?;
    let mut output = (status, Body::from(response.body)).into_response();
    for (name, value) in response.headers {
        if leo_relay_protocol::response_header(&name)
            && let (Ok(name), Ok(value)) =
                (HeaderName::try_from(name), HeaderValue::try_from(value))
        {
            output.headers_mut().append(name, value);
        }
    }

    // Apply this even to JSON: peers can send ambiguous content types that
    // browsers interpret differently. Fetching API data is unaffected by CSP.
    output.headers_mut().insert(
        "content-security-policy",
        HeaderValue::from_static("sandbox"),
    );
    output.headers_mut().insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    output
        .headers_mut()
        .insert("cache-control", HeaderValue::from_static("no-store"));

    Ok(output)
}
