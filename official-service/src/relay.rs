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
    ApiRequest, ApiResponse, Frame, MAX_BODY, MAX_FRAME, MAX_IN_FLIGHT, MAX_STREAM_CHUNK,
    REQUEST_TIMEOUT, Role,
};
use sqlx_core::query_as::query_as;
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::sync::{Notify, OwnedSemaphorePermit, Semaphore, mpsc, oneshot, watch};

struct Pending {
    reply: Option<oneshot::Sender<ApiResponse>>,
    stream: Option<mpsc::Sender<Result<Vec<u8>, std::io::Error>>>,
    // Browser cancellation does not release a slot for work still running remotely.
    _permit: OwnedSemaphorePermit,
}

struct Command {
    request: ApiRequest,
    pending: Pending,
}

struct Tunnel {
    commands: mpsc::Sender<Command>,
    control: mpsc::Sender<Frame>,
    version: u16,
    access: Mutex<Access>,
    access_changed: Notify,
    slots: Arc<Semaphore>,
    // Replacing a connection closes the old generation and its pending replies.
    stop: watch::Sender<bool>,
}

#[derive(Clone, Default)]
pub struct Relay {
    connections: Arc<Mutex<HashMap<String, Arc<Tunnel>>>>,
    closing: Arc<AtomicBool>,
}

#[derive(Default)]
struct Access {
    generations: HashMap<String, u64>,
    streams: HashMap<String, StreamAccess>,
}

struct StreamAccess {
    account: String,
    revoked: watch::Sender<bool>,
}

impl Relay {
    /// End live bodies before the official HTTP server drains on shutdown.
    pub fn shutdown(&self) {
        let mut tunnels = self.connections.lock().unwrap();
        self.closing.store(true, Ordering::SeqCst);
        for (_, tunnel) in tunnels.drain() {
            tunnel.stop.send_replace(true);
        }
    }

    /// Close existing streams when an installation is detached or a member is removed.
    /// Call after committing the access change, before returning its HTTP response.
    pub fn revoke_access(&self, installation: &str, account: Option<&str>) {
        let tunnel = {
            let mut tunnels = self.connections.lock().unwrap();
            if account.is_none() {
                tunnels.remove(installation)
            } else {
                tunnels.get(installation).cloned()
            }
        };
        let Some(tunnel) = tunnel else {
            return;
        };
        if let Some(account) = account {
            let mut access = tunnel.access.lock().unwrap();
            *access.generations.entry(account.to_owned()).or_default() += 1;
            for stream in access
                .streams
                .values()
                .filter(|stream| stream.account == account)
            {
                stream.revoked.send_replace(true);
            }
            // Cancellation must not wait for a backpressured HTTP body to poll.
            tunnel.access_changed.notify_one();
        } else {
            tunnel.stop.send_replace(true);
        }
    }

    pub(super) fn online(&self, installation: &str) -> bool {
        self.connections
            .lock()
            .unwrap()
            .get(installation)
            .is_some_and(|tunnel| !*tunnel.stop.borrow() && !tunnel.commands.is_closed())
    }
}

impl Relay {
    pub(super) fn disconnect(&self, installation: &str) {
        if let Some(tunnel) = self.0.lock().unwrap().remove(installation) {
            let _ = tunnel.stop.send(true);
        }
    }
}

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
    let token_digest = digest(token);
    if !identity_is_current(&service, &installation, &token_digest).await? {
        return Err(ApiError(
            StatusCode::UNAUTHORIZED,
            "Invalid installation identity",
        ));
    }

    Ok(ws
        .max_message_size(MAX_FRAME)
        .max_frame_size(MAX_FRAME)
        .on_upgrade(move |socket| serve_socket(service, installation, token_digest, socket))
        .into_response())
}

async fn identity_is_current(
    service: &Service,
    installation: &str,
    token_digest: &str,
) -> Result<bool, ApiError> {
    let row: Option<(String,)> = query_as(
        "SELECT id FROM installations WHERE id = $1 AND token_digest = $2 AND owner_id IS NOT NULL",
    )
    .bind(installation)
    .bind(token_digest)
    .fetch_optional(&service.pool)
    .await?;
    Ok(row.is_some())
}

async fn serve_socket(
    service: Service,
    installation: String,
    token_digest: String,
    mut socket: WebSocket,
) {
    let relay = &service.relay;
    let hello = tokio::time::timeout(Duration::from_secs(5), socket.next()).await;
    let Ok(Some(Ok(Message::Text(hello)))) = hello else {
        return;
    };
    let Ok(Frame::Hello { versions }) = serde_json::from_str::<Frame>(&hello) else {
        return;
    };
    let Some(version) = leo_relay_protocol::negotiate(&versions) else {
        let _ = socket.close().await;
        return;
    };
    let welcome = serde_json::to_string(&Frame::Welcome { version }).unwrap();
    if socket.send(Message::Text(welcome.into())).await.is_err() {
        return;
    }

    let (commands, mut receiver) = mpsc::channel::<Command>(MAX_IN_FLIGHT);
    let (stop, mut stopped) = watch::channel(false);
    let (control, mut controls) = mpsc::channel::<Frame>(MAX_IN_FLIGHT * 2);
    let tunnel = Arc::new(Tunnel {
        commands,
        control,
        version,
        access: Mutex::new(Access::default()),
        access_changed: Notify::new(),
        slots: Arc::new(Semaphore::new(MAX_IN_FLIGHT)),
        stop,
    });
    {
        let mut tunnels = relay.connections.lock().unwrap();
        if relay.closing.load(Ordering::SeqCst) {
            return;
        }
        if let Some(previous) = tunnels.insert(installation.clone(), tunnel.clone()) {
            previous.stop.send_replace(true);
        }
    }

    // Register before rechecking the persisted identity: detach may have raced
    // the HTTP upgrade/negotiation. Either it removes this generation or this
    // check stops it. An old generation never removes a replacement.
    if !matches!(
        identity_is_current(&service, &installation, &token_digest).await,
        Ok(true)
    ) {
        let _ = tunnel.stop.send(true);
    }

    let mut revocation = tokio::time::interval(Duration::from_secs(1));
    revocation.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut pending = HashMap::<String, Pending>::new();
    let mut heartbeat = tokio::time::interval(Duration::from_secs(15));
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut received = tokio::time::Instant::now();
    'connection: loop {
        tokio::select! {
            biased;

            _ = stopped.changed() => break,
            _ = tunnel.access_changed.notified() => {
                let revoked = {
                    let mut access = tunnel.access.lock().unwrap();
                    let ids: Vec<_> = access.streams.iter()
                        .filter(|(_, stream)| *stream.revoked.borrow())
                        .map(|(id, _)| id.clone())
                        .collect();
                    for id in &ids {
                        access.streams.remove(id);
                    }
                    ids
                };
                for id in revoked {
                    // A queued request is rejected below before dispatch. A
                    // dispatched request releases its slot and is cancelled now.
                    if pending.remove(&id).is_none() {
                        continue;
                    }
                    let message = serde_json::to_string(&Frame::Cancel { id }).unwrap();
                    if socket.send(Message::Text(message.into())).await.is_err() {
                        break 'connection;
                    }
                }
            }
            _ = revocation.tick() => {
                // Owner deletion can originate in account management (#59),
                // another process, or an operator's transaction. The database
                // remains authoritative even for an already open connection.
                if !matches!(
                    identity_is_current(&service, &installation, &token_digest).await,
                    Ok(true)
                ) {
                    break;
                }
            }
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
                let access_revoked = command.pending.stream.is_some()
                    && !tunnel.access.lock().unwrap().streams.get(&command.request.id)
                        .is_some_and(|stream| !*stream.revoked.borrow());
                if access_revoked
                    || command.pending.reply.as_ref().is_some_and(oneshot::Sender::is_closed)
                {
                    continue;
                }

                let id = command.request.id.clone();
                let message = serde_json::to_string(&Frame::Request(command.request)).unwrap();
                pending.insert(id, command.pending);
                if socket.send(Message::Text(message.into())).await.is_err() {
                    break;
                }
            }
            Some(frame) = controls.recv(), if version >= 2 => {
                let id = match &frame {
                    Frame::Cancel { id } | Frame::StreamCredit { id } => id,
                    _ => continue,
                };
                if !pending.contains_key(id) {
                    continue;
                }
                if matches!(frame, Frame::Cancel { .. }) {
                    pending.remove(id);
                }
                let message = serde_json::to_string(&frame).unwrap();
                if socket.send(Message::Text(message.into())).await.is_err() {
                    break;
                }
            }
            message = socket.next() => {
                received = tokio::time::Instant::now();
                match message {
                    Some(Ok(Message::Text(message))) => {
                        let Ok(frame) = serde_json::from_str::<Frame>(&message) else {
                            break;
                        };
                        let cancel = match frame {
                            Frame::Response(response) => {
                                if let Some(mut completed) = pending.remove(&response.id) {
                                    if let Some(reply) = completed.reply.take() {
                                        let _ = reply.send(response);
                                    } else if let Some(stream) = completed.stream {
                                        let _ = stream.try_send(Err(std::io::Error::other("Installation handler failed")));
                                    }
                                }
                                None
                            }
                            Frame::StreamStart(response) if version >= 2 => {
                                let id = response.id.clone();
                                if let Some(request) = pending.get_mut(&id) {
                                    let valid_stream = request.stream.is_some() && response.body.is_empty();
                                    if valid_stream && let Some(reply) = request.reply.take() {
                                        if reply.send(response).is_ok() {
                                            None
                                        } else {
                                            Some(id)
                                        }
                                    } else {
                                        Some(id)
                                    }
                                } else {
                                    Some(id)
                                }
                            }
                            Frame::StreamChunk { id, body } if version >= 2 => {
                                let delivered = body.len() <= MAX_STREAM_CHUNK
                                    && pending.get(&id).is_some_and(|request| {
                                        request.reply.is_none()
                                            && request.stream.as_ref().is_some_and(|stream| {
                                                stream.try_send(Ok(body)).is_ok()
                                            })
                                    });
                                if delivered {
                                    None
                                } else {
                                    Some(id)
                                }
                            }
                            Frame::StreamEnd { id, failed } if version >= 2 => {
                                if let Some(completed) = pending.remove(&id)
                                    && failed
                                    && let Some(stream) = completed.stream
                                {
                                    let _ = stream.try_send(Err(std::io::Error::other("Installation stream failed")));
                                }
                                None
                            }
                            _ => break,
                        };
                        if let Some(id) = cancel {
                            pending.remove(&id);
                            let message = serde_json::to_string(&Frame::Cancel { id }).unwrap();
                            if socket.send(Message::Text(message.into())).await.is_err() {
                                break;
                            }
                        }
                    }
                    Some(Ok(Message::Ping(_) | Message::Pong(_))) => {}
                    _ => break,
                }
            }
        }
    }
    let _ = tunnel.stop.send(true);
    // An old connection must never remove the replacement's registry entry.
    let mut connections = relay.connections.lock().unwrap();
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
    // Snapshot the access generation before checking ownership. Revocation
    // racing a slow upload must also prevent that stream from opening later.
    let tunnel = service
        .relay
        .connections
        .lock()
        .unwrap()
        .get(&installation)
        .cloned();
    let access_generation = tunnel.as_ref().map_or(0, |tunnel| {
        *tunnel
            .access
            .lock()
            .unwrap()
            .generations
            .get(&account)
            .unwrap_or(&0)
    });
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
    let streaming = leo_relay_protocol::stream_path(target);

    // Reserve capacity before reading the body, including requests not yet sent.
    let tunnel = tunnel.ok_or(ApiError(
        StatusCode::SERVICE_UNAVAILABLE,
        "Installation unavailable",
    ))?;
    if *tunnel.stop.borrow() {
        return Err(ApiError(
            StatusCode::SERVICE_UNAVAILABLE,
            "Installation unavailable",
        ));
    }
    if streaming && tunnel.version < 2 {
        return Err(ApiError(
            StatusCode::NOT_IMPLEMENTED,
            "Streaming requires relay protocol 2",
        ));
    }
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
        account_id: account.clone(),
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
    let id = api_request.id.clone();
    let (chunks, receiver) = mpsc::channel(1);
    let browser = if streaming {
        let (revoked, revocation) = watch::channel(false);
        let mut access = tunnel.access.lock().unwrap();
        if *access.generations.get(&account).unwrap_or(&0) != access_generation
            || *tunnel.stop.borrow()
        {
            return Err(ApiError(
                StatusCode::NOT_FOUND,
                "Installation access revoked",
            ));
        }
        access
            .streams
            .insert(id.clone(), StreamAccess { account, revoked });
        Some(BrowserStream {
            receiver,
            tunnel: tunnel.clone(),
            stopped: tunnel.stop.subscribe(),
            revoked: revocation,
            id,
        })
    } else {
        None
    };
    tunnel
        .commands
        .try_send(Command {
            request: api_request,
            pending: Pending {
                reply: Some(reply),
                stream: streaming.then_some(chunks),
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
    let is_stream = response
        .headers
        .iter()
        .any(|(name, value)| name == "content-type" && value.starts_with("text/event-stream"));
    let body = if is_stream && let Some(browser) = browser {
        let stream = futures_util::stream::unfold(browser, |mut browser| async move {
            if *browser.stopped.borrow() || *browser.revoked.borrow() {
                return None;
            }
            if browser
                .tunnel
                .control
                .try_send(Frame::StreamCredit {
                    id: browser.id.clone(),
                })
                .is_err()
            {
                return None;
            }
            let chunk = tokio::select! {
                biased;
                _ = browser.stopped.changed() => None,
                _ = browser.revoked.changed() => None,
                chunk = browser.receiver.recv() => chunk,
            };
            chunk.map(|chunk| (chunk, browser))
        });
        Body::from_stream(stream)
    } else {
        Body::from(response.body)
    };
    let mut output = (status, body).into_response();
    if is_stream {
        output
            .headers_mut()
            .insert("x-accel-buffering", HeaderValue::from_static("no"));
    }
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

// Dropping an HTTP body cancels only this remote subscription. Control messages
// use their own bounded queue so a stalled stream cannot stall the tunnel.
struct BrowserStream {
    receiver: mpsc::Receiver<Result<Vec<u8>, std::io::Error>>,
    tunnel: Arc<Tunnel>,
    stopped: watch::Receiver<bool>,
    revoked: watch::Receiver<bool>,
    id: String,
}

impl Drop for BrowserStream {
    fn drop(&mut self) {
        self.tunnel.access.lock().unwrap().streams.remove(&self.id);
        let _ = self.tunnel.control.try_send(Frame::Cancel {
            id: self.id.clone(),
        });
    }
}
