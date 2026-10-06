use super::{ApiError, Service, digest, installations};
use axum::{
    body::{Body, to_bytes},
    extract::{
        Path, Request, State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    http::{HeaderMap, HeaderName, HeaderValue, StatusCode},
    response::{Html, IntoResponse, Response},
};
use futures_util::{SinkExt, StreamExt};
use leo_relay_protocol::{
    ApiRequest, ApiResponse, Frame, MAX_BODY, MAX_FRAME, MAX_IN_FLIGHT, MAX_PUBLIC_IN_FLIGHT,
    MAX_STREAM_CHUNK, REQUEST_TIMEOUT, Role,
};
use sqlx_core::query_as::query_as;
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::sync::{Notify, OwnedSemaphorePermit, Semaphore, mpsc, oneshot, watch};

// Keep ordinary API capacity available even when browsers hold idle SSE bodies.
const RESERVED_API_SLOTS: usize = 8;
const MAX_MCP_PER_GRANT: usize = 4;
const MAX_STREAMS_PER_ACCOUNT: usize = 8;
const MCP_BODY_TIMEOUT: Duration = Duration::from_secs(10);

struct Pending {
    account: String,
    access_generation: u64,
    reply: Option<oneshot::Sender<ApiResponse>>,
    stream: Option<mpsc::Sender<Result<Vec<u8>, std::io::Error>>>,
    // Browser cancellation does not release a slot for work still running remotely.
    _permit: OwnedSemaphorePermit,
    _stream_permit: Option<OwnedSemaphorePermit>,
    _account_stream_permit: Option<OwnedSemaphorePermit>,
    _mcp_permit: Option<OwnedSemaphorePermit>,
    public_activity: Option<tokio::time::Instant>,
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
    stream_slots: Arc<Semaphore>,
    public_slots: Arc<Semaphore>,
    // Replacing a connection closes the old generation and its pending replies.
    stop: watch::Sender<bool>,
}

impl Tunnel {
    fn access_revoked(&self, account: &str, generation: u64) -> bool {
        *self
            .access
            .lock()
            .unwrap()
            .generations
            .get(account)
            .unwrap_or(&0)
            != generation
            || *self.stop.borrow()
    }
}

#[derive(Clone, Default)]
pub struct Relay {
    connections: Arc<Mutex<HashMap<String, Arc<Tunnel>>>>,
    closing: Arc<AtomicBool>,
    mcp_slots: Arc<Mutex<HashMap<String, Weak<Semaphore>>>>,
    account_stream_slots: Arc<Mutex<HashMap<String, Weak<Semaphore>>>>,
}

#[derive(Default)]
struct Access {
    generations: HashMap<String, u64>,
    streams: HashMap<String, StreamAccess>,
}

struct StreamAccess {
    account: String,
    session: Option<String>,
    expires_at: Option<tokio::time::Instant>,
    revoked: watch::Sender<bool>,
}

impl Relay {
    /// Close only this browser session; other devices keep their access.
    pub(super) fn revoke_session(&self, session: &str) {
        for tunnel in self.connections.lock().unwrap().values() {
            for stream in tunnel.access.lock().unwrap().streams.values() {
                if stream.session.as_deref() == Some(session) {
                    stream.revoked.send_replace(true);
                }
            }
            tunnel.access_changed.notify_one();
        }
    }

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

fn active_slots(
    registry: &Mutex<HashMap<String, Weak<Semaphore>>>,
    key: &str,
    maximum: usize,
) -> Arc<Semaphore> {
    let mut active = registry.lock().unwrap();
    active.retain(|_, slots| slots.strong_count() > 0);
    let slots = active
        .get(key)
        .and_then(Weak::upgrade)
        .unwrap_or_else(|| Arc::new(Semaphore::new(maximum)));
    active.insert(key.to_owned(), Arc::downgrade(&slots));
    slots
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
        return Err(ApiError::Http(
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

    let (commands, mut receiver) = mpsc::channel::<Command>(MAX_IN_FLIGHT + MAX_PUBLIC_IN_FLIGHT);
    let (stop, mut stopped) = watch::channel(false);
    let (control, mut controls) =
        mpsc::channel::<Frame>((MAX_IN_FLIGHT + MAX_PUBLIC_IN_FLIGHT) * 2);
    let tunnel = Arc::new(Tunnel {
        commands,
        control,
        version,
        access: Mutex::new(Access::default()),
        access_changed: Notify::new(),
        slots: Arc::new(Semaphore::new(MAX_IN_FLIGHT)),
        stream_slots: Arc::new(Semaphore::new(MAX_IN_FLIGHT - RESERVED_API_SLOTS)),
        public_slots: Arc::new(Semaphore::new(MAX_PUBLIC_IN_FLIGHT)),
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

    // A database check must not block expiry or cancellation in the socket loop.
    let check_service = service.clone();
    let check_installation = installation.clone();
    let check_token = token_digest.clone();
    let check_tunnel = tunnel.clone();
    let mut check_stopped = tunnel.stop.subscribe();
    let mut revocation = tokio::spawn(async move {
        let interval = Duration::from_secs(30);
        let mut ticks = tokio::time::interval_at(tokio::time::Instant::now() + interval, interval);
        ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut failures = 0;
        while !*check_stopped.borrow() {
            tokio::select! {
                biased;

                _ = check_stopped.changed() => break,
                _ = ticks.tick() => {
                    let current = tokio::time::timeout(
                        Duration::from_secs(5),
                        identity_is_current(&check_service, &check_installation, &check_token),
                    ).await;

                    let end_connection = match current {
                        Ok(Ok(true)) => {
                            failures = 0;
                            false
                        }
                        Ok(Ok(false)) => true,
                        _ => {
                            failures += 1;
                            tracing::warn!(failures, "Relay identity check unavailable");
                            failures >= 3
                        }
                    };
                    if end_connection {
                        check_tunnel.stop.send_replace(true);
                        break;
                    }
                }
            }
        }
    });
    let mut pending = HashMap::<String, Pending>::new();
    let mut heartbeat = tokio::time::interval(Duration::from_secs(15));
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut public_expiry = tokio::time::interval(Duration::from_secs(1));
    public_expiry.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut received = tokio::time::Instant::now();
    'connection: loop {
        let expiry = tunnel
            .access
            .lock()
            .unwrap()
            .streams
            .values()
            .filter(|stream| !*stream.revoked.borrow())
            .filter_map(|stream| stream.expires_at)
            .min();

        tokio::select! {
            biased;

            _ = stopped.changed() => break,
            _ = async {
                match expiry {
                    Some(deadline) => tokio::time::sleep_until(deadline).await,
                    None => futures_util::future::pending::<()>().await,
                }
            } => {
                let now = tokio::time::Instant::now();
                for stream in tunnel.access.lock().unwrap().streams.values() {
                    if stream.expires_at.is_some_and(|deadline| deadline <= now) {
                        stream.revoked.send_replace(true);
                    }
                }
                tunnel.access_changed.notify_one();
            }
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
            _ = public_expiry.tick() => {
                // Expire public bodies independently of downstream polling,
                // including readers that stopped granting stream credit.
                let expired: Vec<_> = pending.iter()
                    .filter(|(_, request)| {
                        request.public_activity.is_some_and(|activity| activity.elapsed() >= REQUEST_TIMEOUT)
                    })
                    .map(|(id, _)| id.clone())
                    .collect();
                for id in expired {
                    pending.remove(&id);
                    if let Some(stream) = tunnel.access.lock().unwrap().streams.remove(&id) {
                        stream.revoked.send_replace(true);
                    }
                    let message = serde_json::to_string(&Frame::Cancel { id }).unwrap();
                    if socket.send(Message::Text(message.into())).await.is_err() {
                        break 'connection;
                    }
                }
            }
            result = &mut revocation => {
                if result.is_err() {
                    tracing::warn!("Relay identity monitor stopped");
                }
                break;
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
                let Some(mut command) = command else {
                    break;
                };
                let access_revoked = tunnel.access_revoked(&command.pending.account, command.pending.access_generation)
                    || command.pending.stream.is_some()
                    && !tunnel.access.lock().unwrap().streams.get(&command.request.id)
                        .is_some_and(|stream| !*stream.revoked.borrow());
                if access_revoked {
                    if let Some(reply) = command.pending.reply.take() {
                        let _ = reply.send(ApiResponse {
                            id: command.request.id,
                            status: 404,
                            headers: Vec::new(),
                            body: br#"{"error":"Installation access revoked"}"#.to_vec(),
                        });
                    }
                    continue;
                }
                if command.pending.reply.as_ref().is_some_and(oneshot::Sender::is_closed) {
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
                                    if request.public_activity.is_some() {
                                        request.public_activity = Some(tokio::time::Instant::now());
                                    }
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
                                    && pending.get_mut(&id).is_some_and(|request| {
                                        if request.public_activity.is_some() {
                                            request.public_activity = Some(tokio::time::Instant::now());
                                        }
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
    revocation.abort();
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
    let role = installations::role(&service, &installation, &account).await?;

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
        return Err(ApiError::Http(
            StatusCode::NOT_FOUND,
            "Installation API route not found",
        ));
    }
    let target = target.to_owned();
    send(
        &service,
        tunnel,
        access_generation,
        account,
        Capability::Account(role),
        &target,
        request,
    )
    .await
}

pub(super) async fn mcp(
    service: &Service,
    grant: super::mcp::McpAccess,
    credential_digest: String,
    request: Request,
) -> Result<Response, ApiError> {
    // Uploads and remote work share the grant's small allowance, including
    // access tokens from refresh rotation. Idle grants retain no semaphore.
    let slots = active_slots(&service.relay.mcp_slots, &grant.id, MAX_MCP_PER_GRANT);
    let permit = slots
        .try_acquire_owned()
        .map_err(|_| ApiError::Http(StatusCode::TOO_MANY_REQUESTS, "MCP authorization is busy"))?;

    // Read a bounded body before reserving any installation capacity. The total
    // deadline also ends clients that keep trickling bytes without completing.
    let (parts, body) = request.into_parts();
    let body = tokio::time::timeout(MCP_BODY_TIMEOUT, to_bytes(body, MAX_BODY))
        .await
        .map_err(|_| ApiError::Http(StatusCode::REQUEST_TIMEOUT, "MCP request body timed out"))?
        .map_err(|_| ApiError::Http(StatusCode::PAYLOAD_TOO_LARGE, "MCP request is too large"))?;
    if !super::mcp::still_authorized(service, &credential_digest).await? {
        return Ok(super::mcp::unauthorized(service));
    }
    let request = Request::from_parts(parts, Body::from(body));
    let tunnel = service
        .relay
        .connections
        .lock()
        .unwrap()
        .get(&grant.installation)
        .cloned();
    send(
        service,
        tunnel,
        0,
        grant.account,
        Capability::Mcp {
            scopes: grant.scopes,
            credential_digest,
            permit,
        },
        "/api/mcp",
        request,
    )
    .await
}

enum Capability {
    Account(Role),
    Mcp {
        scopes: Vec<String>,
        credential_digest: String,
        permit: OwnedSemaphorePermit,
    },
    PublicArtifact(String),
}

// Only an official availability page may render inline; relayed HTML stays
// an attachment even when an installation sends an error status.
#[derive(Clone)]
pub(super) struct PublicOfflinePage;

pub(super) async fn public_artifact(
    State(service): State<Service>,
    Path((installation, token)): Path<(String, String)>,
    axum::extract::ConnectInfo(peer): axum::extract::ConnectInfo<std::net::SocketAddr>,
    request: Request,
) -> Result<Response, ApiError> {
    super::consume_limit(&service.pool, &format!("public-file:{}", peer.ip()), 30).await?;
    if uuid::Uuid::parse_str(&installation).is_err() || uuid::Uuid::parse_str(&token).is_err() {
        return Err(ApiError::Http(
            StatusCode::NOT_FOUND,
            "Public file not found",
        ));
    }
    let claimed: Option<(String,)> =
        query_as("SELECT id FROM installations WHERE id = $1 AND owner_id IS NOT NULL")
            .bind(&installation)
            .fetch_optional(&service.pool)
            .await?;
    if claimed.is_none() {
        return Err(ApiError::Http(
            StatusCode::NOT_FOUND,
            "Public file not found",
        ));
    }
    let tunnel = service
        .relay
        .connections
        .lock()
        .unwrap()
        .get(&installation)
        .cloned();
    if tunnel.is_none() || tunnel.as_ref().is_some_and(|tunnel| *tunnel.stop.borrow()) {
        let page = Html(
            r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Installation offline</title>
</head>
<body>
<h1>Installation offline</h1>
<p>The public file will be available when it reconnects.</p>
</body>
</html>"#,
        );
        let mut response = (StatusCode::SERVICE_UNAVAILABLE, page).into_response();
        response.extensions_mut().insert(PublicOfflinePage);
        return Ok(response);
    }
    let mut target = format!("/api/shared-artifacts/{token}");
    if let Some(query) = request.uri().query() {
        target.push('?');
        target.push_str(query);
    }
    send(
        &service,
        tunnel,
        0,
        String::new(),
        Capability::PublicArtifact(token),
        &target,
        request,
    )
    .await
}

async fn send(
    service: &Service,
    tunnel: Option<Arc<Tunnel>>,
    access_generation: u64,
    account: String,
    capability: Capability,
    target: &str,
    request: Request,
) -> Result<Response, ApiError> {
    let streaming = leo_relay_protocol::stream_path(target);
    let public_file = matches!(capability, Capability::PublicArtifact(_));
    let browser_session = matches!(capability, Capability::Account(_))
        .then(|| digest(super::session_token(request.headers())));

    // Browser uploads reserve capacity before reading. MCP bodies have already
    // been bounded and reauthorized before they can reserve installation slots.
    let tunnel = tunnel.ok_or(ApiError::Http(
        StatusCode::SERVICE_UNAVAILABLE,
        "Installation unavailable",
    ))?;
    if *tunnel.stop.borrow() {
        return Err(ApiError::Http(
            StatusCode::SERVICE_UNAVAILABLE,
            "Installation unavailable",
        ));
    }
    if streaming && tunnel.version < 2 {
        return Err(ApiError::Http(
            StatusCode::NOT_IMPLEMENTED,
            "Streaming requires relay protocol 2",
        ));
    }
    let slots = if public_file {
        &tunnel.public_slots
    } else {
        &tunnel.slots
    };
    let permit = slots
        .clone()
        .try_acquire_owned()
        .map_err(|_| ApiError::Http(StatusCode::SERVICE_UNAVAILABLE, "Installation busy"))?;

    let stream_permit = if streaming && !public_file {
        Some(
            tunnel
                .stream_slots
                .clone()
                .try_acquire_owned()
                .map_err(|_| {
                    ApiError::Http(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "Installation stream capacity reached",
                    )
                })?,
        )
    } else {
        None
    };

    let account_stream_permit = if streaming && browser_session.is_some() {
        Some(
            active_slots(
                &service.relay.account_stream_slots,
                &account,
                MAX_STREAMS_PER_ACCOUNT,
            )
            .try_acquire_owned()
            .map_err(|_| {
                ApiError::Http(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "Account stream capacity reached",
                )
            })?,
        )
    } else {
        None
    };

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
    let (role, mcp_scopes, public_artifact, mcp_credential, mcp_permit) = match capability {
        Capability::Account(role) => (role, None, None, None, None),
        Capability::Mcp {
            scopes,
            credential_digest,
            permit,
        } => (
            Role::Owner,
            Some(scopes),
            None,
            Some(credential_digest),
            Some(permit),
        ),
        Capability::PublicArtifact(token) => (Role::Member, None, Some(token), None, None),
    };
    let api_request = ApiRequest {
        id: uuid::Uuid::new_v4().to_string(),
        account_id: account.clone(),
        role,
        mcp_scopes,
        public_artifact,
        method: request.method().to_string(),
        path: target.to_owned(),
        headers,
        // GET/HEAD public capabilities have no request payload. Do not let an
        // anonymous slow upload hold a slot before it enters the deadline loop.
        body: if public_file {
            Vec::new()
        } else {
            to_bytes(request.into_body(), MAX_BODY)
                .await
                .map_err(|_| {
                    ApiError::Http(StatusCode::PAYLOAD_TOO_LARGE, "API request is too large")
                })?
                .to_vec()
        },
    };
    // Revoke before dispatch even when the caller suspended its upload after
    // the initial bearer check. Reuse the same current grant/ownership rules.
    if let Some(credential) = mcp_credential
        && !super::mcp::still_authorized(service, &credential).await?
    {
        return Ok(super::mcp::unauthorized(service));
    }

    if tunnel.access_revoked(&account, access_generation) {
        return Err(ApiError::Http(
            StatusCode::NOT_FOUND,
            "Installation access revoked",
        ));
    }

    let (reply, response) = oneshot::channel();
    let id = api_request.id.clone();
    let (chunks, receiver) = mpsc::channel(1);
    let browser = if streaming {
        let (revoked, revocation) = watch::channel(false);
        let mut access = tunnel.access.lock().unwrap();
        if *access.generations.get(&account).unwrap_or(&0) != access_generation
            || *tunnel.stop.borrow()
        {
            return Err(ApiError::Http(
                StatusCode::NOT_FOUND,
                "Installation access revoked",
            ));
        }
        access.streams.insert(
            id.clone(),
            StreamAccess {
                account,
                session: browser_session.clone(),
                expires_at: None,
                revoked,
            },
        );
        Some(BrowserStream {
            receiver,
            tunnel: tunnel.clone(),
            stopped: tunnel.stop.subscribe(),
            revoked: revocation,
            id: id.clone(),
        })
    } else {
        None
    };
    // Register before checking again: logout during an upload either sees the
    // registered stream or removes the persisted session before this check.
    if let Some(session) = browser_session {
        let checked_at = tokio::time::Instant::now();
        let current: Option<(i64,)> = query_as(
            "SELECT (extract(epoch FROM (expires_at - clock_timestamp())) * 1000)::bigint FROM web_sessions WHERE digest = $1 AND expires_at > clock_timestamp()",
        )
        .bind(&session)
        .fetch_optional(&service.pool)
        .await?;
        let Some((remaining,)) = current.filter(|(remaining,)| *remaining > 0) else {
            return Err(ApiError::Http(
                StatusCode::UNAUTHORIZED,
                "Session expired. Please sign in again.",
            ));
        };
        if let Some(stream) = tunnel.access.lock().unwrap().streams.get_mut(&id) {
            stream.expires_at = Some(checked_at + Duration::from_millis(remaining as u64));
            tunnel.access_changed.notify_one();
        }
    }

    let pending_account = api_request.account_id.clone();
    tunnel
        .commands
        .try_send(Command {
            request: api_request,
            pending: Pending {
                account: pending_account,
                access_generation,
                reply: Some(reply),
                stream: streaming.then_some(chunks),
                _permit: permit,
                _stream_permit: stream_permit,
                _account_stream_permit: account_stream_permit,
                _mcp_permit: mcp_permit,
                public_activity: public_file.then(tokio::time::Instant::now),
            },
        })
        .map_err(|_| {
            ApiError::Http(
                StatusCode::SERVICE_UNAVAILABLE,
                "Installation busy or unavailable",
            )
        })?;
    let response = tokio::time::timeout(REQUEST_TIMEOUT, response)
        .await
        .map_err(|_| {
            ApiError::Http(
                StatusCode::GATEWAY_TIMEOUT,
                "Installation request timed out",
            )
        })?
        .map_err(|_| ApiError::Http(StatusCode::BAD_GATEWAY, "Installation connection lost"))?;
    let status = StatusCode::from_u16(response.status)
        .map_err(|_| ApiError::Http(StatusCode::BAD_GATEWAY, "Invalid installation response"))?;
    let is_stream = streaming && response.body.is_empty();
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

// Dropping an HTTP body records cancellation before waking the socket loop.
// Cancellation cannot be lost when the bounded credit queue is full.
struct BrowserStream {
    receiver: mpsc::Receiver<Result<Vec<u8>, std::io::Error>>,
    tunnel: Arc<Tunnel>,
    stopped: watch::Receiver<bool>,
    revoked: watch::Receiver<bool>,
    id: String,
}

impl Drop for BrowserStream {
    fn drop(&mut self) {
        if let Some(stream) = self.tunnel.access.lock().unwrap().streams.get(&self.id) {
            stream.revoked.send_replace(true);
        }
        self.tunnel.access_changed.notify_one();
    }
}
