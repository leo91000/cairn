use super::*;
use axum::{
    Json,
    extract::DefaultBodyLimit,
    response::sse::{Event, Sse},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use leo_relay_protocol::direct::{
    DIRECT_TTL, DIRECT_VERSION, DirectAuthorization, DirectClaims, DirectSignal,
    MAX_DIRECT_CONNECTIONS, MAX_DIRECT_PER_ACCOUNT, signing_bytes, unix_time, valid_fingerprint,
};
use ring::signature::KeyPair;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::broadcast;

pub(super) struct Connection {
    pub authorization: DirectAuthorization,
    pub session_digest: String,
    pub session_deadline: tokio::time::Instant,
    pub reply: Option<oneshot::Sender<bool>>,
    pub accepted: bool,
    pub signals: broadcast::Sender<DirectSignal>,
    signals_reader: Arc<tokio::sync::Mutex<broadcast::Receiver<DirectSignal>>>,
    pub revoked: watch::Sender<bool>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AuthorizationRequest {
    fingerprint: String,
    versions: Vec<u16>,
}

pub(crate) fn body_limit() -> DefaultBodyLimit {
    DefaultBodyLimit::max(32_768)
}

pub(crate) async fn authorize(
    State(service): State<Service>,
    Path(installation): Path<String>,
    headers: HeaderMap,
    Json(input): Json<AuthorizationRequest>,
) -> Result<Json<Value>, ApiError> {
    issue(&service, &installation, &headers, input, None).await
}

pub(crate) async fn renew(
    State(service): State<Service>,
    Path((installation, id)): Path<(String, String)>,
    headers: HeaderMap,
    Json(input): Json<AuthorizationRequest>,
) -> Result<Json<Value>, ApiError> {
    issue(&service, &installation, &headers, input, Some(&id)).await
}

async fn issue(
    service: &Service,
    installation: &str,
    headers: &HeaderMap,
    input: AuthorizationRequest,
    renew: Option<&str>,
) -> Result<Json<Value>, ApiError> {
    let account = installations::account(service, headers, &axum::http::Method::POST).await?;
    super::super::consume_limit(
        &service.pool,
        &format!("direct-authorization:{account}"),
        30,
    )
    .await?;
    let tunnel = service
        .relay
        .connections
        .lock()
        .unwrap()
        .get(installation)
        .cloned();
    let generation = tunnel.as_ref().map_or(0, |tunnel| {
        *tunnel
            .access
            .lock()
            .unwrap()
            .generations
            .get(&account)
            .unwrap_or(&0)
    });
    let role = installations::role(service, installation, &account).await?;
    if input.versions.len() > 4 || !valid_fingerprint(&input.fingerprint) {
        return Err(ApiError::Http(
            StatusCode::BAD_REQUEST,
            "Invalid direct authorization request",
        ));
    }
    let Some(tunnel) = tunnel else {
        return Ok(Json(json!({ "available": false })));
    };
    if tunnel.version < DIRECT_VERSION || !input.versions.contains(&DIRECT_VERSION) {
        return Ok(Json(json!({ "available": false })));
    }
    let session_digest = digest(super::super::session_token(headers));
    let row: Option<(String, i64)> = query_as("SELECT id, floor(extract(epoch FROM expires_at))::bigint FROM web_sessions WHERE digest = $1 AND expires_at > clock_timestamp()")
        .bind(&session_digest).fetch_optional(&service.pool).await?;
    let Some((session_id, expires_at)) = row else {
        return Err(ApiError::Http(StatusCode::UNAUTHORIZED, "Session expired"));
    };
    let claims = DirectClaims {
        connection_id: renew
            .map(str::to_owned)
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
        installation_id: installation.to_owned(),
        account_id: account.clone(),
        role,
        session_id,
        generation,
        fingerprint: input.fingerprint,
        expires_at: (expires_at as u64).min(unix_time() + DIRECT_TTL),
        nonce: uuid::Uuid::new_v4().to_string(),
    };
    if claims.expires_at <= unix_time() {
        return Err(ApiError::Http(StatusCode::UNAUTHORIZED, "Session expired"));
    }
    let authorization = DirectAuthorization {
        signature: URL_SAFE_NO_PAD.encode(tunnel.signing.sign(&signing_bytes(&claims)).as_ref()),
        claims,
    };
    let id = authorization.claims.connection_id.clone();
    let (reply, accepted) = oneshot::channel();
    {
        let mut access = tunnel.access.lock().unwrap();
        if *tunnel.stop.borrow() || *access.generations.get(&account).unwrap_or(&0) != generation {
            return Err(ApiError::Http(
                StatusCode::NOT_FOUND,
                "Installation access revoked",
            ));
        }
        if renew.is_some() {
            let Some(connection) = access.direct.get_mut(&id) else {
                return Err(ApiError::Http(
                    StatusCode::NOT_FOUND,
                    "Direct connection not found",
                ));
            };
            let previous = &connection.authorization.claims;
            if connection.session_digest != session_digest
                || previous.account_id != account
                || previous.role != role
                || previous.fingerprint != authorization.claims.fingerprint
                || connection.reply.is_some()
                || !connection.accepted
            {
                return Err(ApiError::Http(
                    StatusCode::NOT_FOUND,
                    "Direct connection not found",
                ));
            }
            connection.authorization = authorization.clone();
            connection.reply = Some(reply);
        } else {
            if access.direct.len() >= MAX_DIRECT_CONNECTIONS
                || access
                    .direct
                    .values()
                    .filter(|connection| connection.authorization.claims.account_id == account)
                    .count()
                    >= MAX_DIRECT_PER_ACCOUNT
            {
                return Err(ApiError::Http(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "Direct connection capacity reached",
                ));
            }
            let (signals, signals_reader) = broadcast::channel(16);
            access.direct.insert(
                id.clone(),
                Connection {
                    authorization: authorization.clone(),
                    session_digest: session_digest.clone(),
                    session_deadline: tokio::time::Instant::now()
                        + leo_relay_protocol::direct::until_expiry(expires_at as u64),
                    reply: Some(reply),
                    accepted: false,
                    signals,
                    signals_reader: Arc::new(tokio::sync::Mutex::new(signals_reader)),
                    revoked: watch::channel(false).0,
                },
            );
        }
    }
    tunnel.access_changed.notify_one();
    // Register before rechecking persisted access. Concurrent logout/removal either
    // sees this connection, or these reads deny it before the tunnel dispatches it.
    let checked = async {
        installations::account(service, headers, &axum::http::Method::POST).await?;
        installations::role(service, installation, &account).await?;
        if tunnel.access_revoked(&account, generation)
            || !tunnel.access.lock().unwrap().direct.contains_key(&id)
        {
            return Err(ApiError::Http(
                StatusCode::NOT_FOUND,
                "Installation access revoked",
            ));
        }
        let frame = if renew.is_some() {
            Frame::DirectRenew {
                id: authorization.claims.nonce.clone(),
                authorization: authorization.clone(),
            }
        } else {
            Frame::DirectAuthorize {
                id: authorization.claims.nonce.clone(),
                authorization: authorization.clone(),
            }
        };
        tunnel
            .control
            .try_send(frame)
            .map_err(|_| ApiError::Http(StatusCode::SERVICE_UNAVAILABLE, "Signaling busy"))?;
        Ok::<_, ApiError>(())
    }
    .await;
    if let Err(error) = checked {
        failed_authorization(&tunnel, &id, renew.is_some());
        return Err(error);
    }
    if !matches!(
        tokio::time::timeout(Duration::from_secs(5), accepted).await,
        Ok(Ok(true))
    ) {
        failed_authorization(&tunnel, &id, renew.is_some());
        return Err(ApiError::Http(
            StatusCode::FORBIDDEN,
            "Installation refused direct authorization",
        ));
    }
    if tunnel.access_revoked(&account, generation)
        || !tunnel.access.lock().unwrap().direct.contains_key(&id)
    {
        return Err(ApiError::Http(
            StatusCode::NOT_FOUND,
            "Installation access revoked",
        ));
    }
    Ok(Json(json!({ "available": true, "grant": authorization })))
}

fn failed_authorization(tunnel: &Tunnel, id: &str, renewal: bool) {
    if renewal {
        // A lost acknowledgement must not forget an existing direct peer:
        // logout/access changes still need to find and revoke its session.
        if let Some(connection) = tunnel.access.lock().unwrap().direct.get_mut(id) {
            connection.reply = None;
        }
        return;
    }
    if let Some(connection) = tunnel.access.lock().unwrap().direct.remove(id) {
        connection.revoked.send_replace(true);
    }
}

async fn connection(
    service: &Service,
    installation: &str,
    id: &str,
    headers: &HeaderMap,
    method: &axum::http::Method,
) -> Result<Arc<Tunnel>, ApiError> {
    let account = installations::account(service, headers, method).await?;
    let role = installations::role(service, installation, &account).await?;
    let tunnel = service
        .relay
        .connections
        .lock()
        .unwrap()
        .get(installation)
        .cloned()
        .ok_or(ApiError::Http(
            StatusCode::NOT_FOUND,
            "Direct connection not found",
        ))?;
    let access = tunnel.access.lock().unwrap();
    let permitted = access.direct.get(id).is_some_and(|connection| {
        let claims = &connection.authorization.claims;
        connection.accepted
            && claims.account_id == account
            && claims.role == role
            && claims.expires_at > unix_time()
            && connection.session_digest == digest(super::super::session_token(headers))
            && !*connection.revoked.borrow()
    });
    if !permitted || *tunnel.stop.borrow() {
        return Err(ApiError::Http(
            StatusCode::NOT_FOUND,
            "Direct connection not found",
        ));
    }
    drop(access);
    Ok(tunnel)
}

pub(crate) async fn signal(
    State(service): State<Service>,
    Path((installation, id)): Path<(String, String)>,
    headers: HeaderMap,
    Json(signal): Json<DirectSignal>,
) -> Result<StatusCode, ApiError> {
    let account = installations::account(&service, &headers, &axum::http::Method::POST).await?;
    super::super::consume_limit(&service.pool, &format!("direct-signaling:{account}"), 120).await?;
    let tunnel = connection(
        &service,
        &installation,
        &id,
        &headers,
        &axum::http::Method::POST,
    )
    .await?;
    let access = tunnel.access.lock().unwrap();
    let valid = access.direct.get(&id).is_some_and(|connection| {
        signal.valid()
            && signal.matches_client_fingerprint(&connection.authorization.claims.fingerprint)
    });
    if !valid {
        return Err(ApiError::Http(
            StatusCode::BAD_REQUEST,
            "Invalid direct signal",
        ));
    }
    tunnel
        .control
        .try_send(Frame::DirectSignal { id, signal })
        .map_err(|_| ApiError::Http(StatusCode::SERVICE_UNAVAILABLE, "Signaling busy"))?;
    Ok(StatusCode::NO_CONTENT)
}

pub(crate) async fn events(
    State(service): State<Service>,
    Path((installation, id)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let tunnel = connection(
        &service,
        &installation,
        &id,
        &headers,
        &axum::http::Method::GET,
    )
    .await?;
    let (signals, revoked) = {
        let access = tunnel.access.lock().unwrap();
        let connection = access.direct.get(&id).ok_or(ApiError::Http(
            StatusCode::NOT_FOUND,
            "Direct connection not found",
        ))?;
        (
            connection
                .signals_reader
                .clone()
                .try_lock_owned()
                .map_err(|_| {
                    ApiError::Http(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "Signaling reader already open",
                    )
                })?,
            connection.revoked.subscribe(),
        )
    };
    let stopped = tunnel.stop.subscribe();
    let stream = futures_util::stream::unfold(
        (signals, revoked, stopped),
        |(mut signals, mut revoked, mut stopped)| async move {
            if *revoked.borrow() || *stopped.borrow() {
                return None;
            }
            let signal = tokio::select! {
                biased;
                _ = revoked.changed() => None,
                _ = stopped.changed() => None,
                signal = signals.recv() => signal.ok(),
            }?;
            Some((
                Ok::<_, std::convert::Infallible>(
                    Event::default().event("signal").json_data(signal).unwrap(),
                ),
                (signals, revoked, stopped),
            ))
        },
    );
    Ok(Sse::new(stream)
        .keep_alive(axum::response::sse::KeepAlive::default())
        .into_response())
}

pub(super) fn acknowledge(tunnel: &Tunnel, id: &str, accepted: bool) {
    let mut access = tunnel.access.lock().unwrap();
    if let Some(connection) = access
        .direct
        .values_mut()
        .find(|connection| connection.authorization.claims.nonce == id)
    {
        // Refusing a renewal leaves the previously verified lease in force.
        // Keep tracking it so a retry and scoped revocation remain possible.
        connection.accepted |= accepted;
        if let Some(reply) = connection.reply.take() {
            let _ = reply.send(accepted);
        }
    }
}

pub(super) fn receive_signal(tunnel: &Tunnel, id: &str, signal: DirectSignal) {
    {
        let mut window = tunnel.signaling_window.lock().unwrap();
        if window.0.elapsed() >= Duration::from_secs(60) {
            *window = (tokio::time::Instant::now(), 0);
        }
        window.1 += 1;
        if window.1 > 120 {
            return;
        }
    }
    let access = tunnel.access.lock().unwrap();
    if let Some(connection) = access.direct.get(id)
        && connection.accepted
        && connection.authorization.claims.expires_at > unix_time()
        && signal.valid()
    {
        let _ = connection.signals.send(signal);
    }
}

pub(super) fn public_key(tunnel: &Tunnel) -> String {
    URL_SAFE_NO_PAD.encode(tunnel.signing.public_key().as_ref())
}
