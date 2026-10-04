//! The installation owns the outbound connection and dispatches through its real HTTP router.
use crate::{
    auth::{InstallationIdentity, InstallationRole},
    error::{Error, Result},
    skills::private_dir,
};
use axum::{
    Router,
    body::{Body, to_bytes},
    extract::ConnectInfo,
    http::{HeaderName, HeaderValue, Request},
};
use futures_util::{SinkExt, StreamExt};
use leo_relay_protocol::{
    ApiRequest, ApiResponse, Frame, MAX_BODY, MAX_FRAME, MAX_IN_FLIGHT, MAX_STREAM_CHUNK,
    MIN_PROTOCOL_VERSION, REQUEST_TIMEOUT, Role, SUPPORTED_VERSIONS,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    net::SocketAddr,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::{
    io::AsyncWriteExt,
    sync::{Semaphore, mpsc},
    task::{AbortHandle, JoinSet},
};
use tokio_tungstenite::tungstenite::{
    Message, client::IntoClientRequest, protocol::WebSocketConfig,
};
use tokio_util::sync::CancellationToken;
use tower::ServiceExt;

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Identity {
    origin: String,
    installation_id: String,
    token: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Claimed {
    installation_id: String,
    token: String,
}

fn origin(value: &str) -> Result<url::Url> {
    let url = url::Url::parse(value).map_err(|_| Error::bad("Invalid official origin."))?;
    let loopback = url.host_str().is_some_and(|host| {
        host == "localhost"
            || host
                .parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.is_loopback())
    });
    if !(url.scheme() == "https" || url.scheme() == "http" && loopback)
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(Error::bad(
            "Use an HTTPS official origin (HTTP only on loopback).",
        ));
    }
    Ok(url)
}

/// The claim code is supplied in memory, never logged or included in a URL.
pub async fn claim(official: &str, directory: &Path, code: &str, name: &str) -> Result<()> {
    let official = origin(official)?;
    private_dir(directory).await?;
    let path = directory.join("identity.json");
    let mut file = tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
        .await
        .map_err(|_| Error::conflict("Installation identity exists or is not writable."))?;

    let result = async {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(15))
            .build()
            .map_err(Error::internal)?;
        let response = client
            .post(official.join("api/relay/claim").map_err(Error::internal)?)
            .json(&serde_json::json!({
                "code": code,
                "name": name,
                "protocol": MIN_PROTOCOL_VERSION,
            }))
            .send()
            .await
            .map_err(|_| Error::unavailable("Cannot reach the official service."))?;
        if !response.status().is_success() {
            return Err(Error::bad(
                "Installation claim refused; obtain a new claim code.",
            ));
        }

        let claimed: Claimed = response
            .json()
            .await
            .map_err(|_| Error::bad("Invalid claim response."))?;
        uuid::Uuid::parse_str(&claimed.installation_id)
            .map_err(|_| Error::bad("Invalid installation identity."))?;
        let identity = Identity {
            origin: official.origin().ascii_serialization(),
            installation_id: claimed.installation_id,
            token: claimed.token,
        };
        file.write_all(&serde_json::to_vec(&identity)?).await?;
        file.sync_all().await?;
        Ok(())
    }
    .await;

    if result.is_err() {
        let _ = tokio::fs::remove_file(path).await;
    }

    result
}

/// Reconnect until shutdown; failed in-flight writes are never automatically replayed.
pub async fn connect(directory: PathBuf, router: Router, stop: CancellationToken) -> Result<()> {
    let path = directory.join("identity.json");
    let metadata = tokio::fs::symlink_metadata(&path).await?;
    if !metadata.is_file() || metadata.permissions().mode() & 0o077 != 0 {
        return Err(Error::bad(
            "Installation identity must be a private regular file.",
        ));
    }
    let identity: Identity = serde_json::from_slice(&tokio::fs::read(&path).await?)?;
    let official = origin(&identity.origin)?;
    let mut delay = Duration::from_millis(250);
    loop {
        let started = tokio::time::Instant::now();
        tokio::select! {
            () = stop.cancelled() => return Ok(()),
            result = connected(&identity, &official, router.clone()) => {
                if result.is_err() {
                    tracing::warn!("Installation relay disconnected; retrying");
                }
            }
        }
        if started.elapsed() > Duration::from_secs(30) {
            delay = Duration::from_millis(250);
        }
        tokio::select! {
            () = stop.cancelled() => return Ok(()),
            () = tokio::time::sleep(delay) => {}
        }
        delay = (delay * 2).min(Duration::from_secs(15));
    }
}

async fn connected(identity: &Identity, official: &url::Url, router: Router) -> Result<()> {
    let mut url = official
        .join(&format!("api/relay/{}/connect", identity.installation_id))
        .map_err(Error::internal)?;
    let scheme = if official.scheme() == "https" {
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
        HeaderValue::from_str(&format!("Bearer {}", identity.token))
            .map_err(|_| Error::bad("Invalid installation identity."))?,
    );
    let config = WebSocketConfig::default()
        .max_message_size(Some(MAX_FRAME))
        .max_frame_size(Some(MAX_FRAME));
    let (mut socket, _) = tokio::time::timeout(
        Duration::from_secs(15),
        tokio_tungstenite::connect_async_with_config(request, Some(config), false),
    )
    .await
    .map_err(|_| Error::unavailable("Relay connection timed out."))?
    .map_err(|_| Error::unavailable("Relay connection refused."))?;

    socket
        .send(Message::Text(
            serde_json::to_string(&Frame::Hello {
                versions: SUPPORTED_VERSIONS.to_vec(),
            })?
            .into(),
        ))
        .await
        .map_err(Error::internal)?;

    let welcome = tokio::time::timeout(Duration::from_secs(5), socket.next())
        .await
        .map_err(|_| Error::unavailable("Relay negotiation timed out."))?;
    let Some(Ok(Message::Text(welcome))) = welcome else {
        return Err(Error::unavailable("Relay negotiation failed."));
    };
    let Frame::Welcome { version } = serde_json::from_str::<Frame>(&welcome)? else {
        return Err(Error::bad("Incompatible relay protocol."));
    };
    if !SUPPORTED_VERSIONS.contains(&version) {
        return Err(Error::bad("Incompatible relay protocol."));
    }

    let mut requests = JoinSet::new();
    let mut request_ids = HashMap::new();
    let mut active = HashMap::<String, (AbortHandle, std::sync::Arc<Semaphore>)>::new();
    let (output, mut frames) = mpsc::channel::<Frame>(MAX_IN_FLIGHT);
    loop {
        tokio::select! {
            result = requests.join_next_with_id(), if !requests.is_empty() => {
                let completed = result
                    .ok_or_else(|| Error::unavailable("Relay request stopped."))?;
                let (task_id, result) = match completed {
                    Ok((task_id, result)) => (task_id, result),
                    Err(error) => (
                        error.id(),
                        Err(Error::bad_gateway("Installation handler failed.")),
                    ),
                };
                let request_id = request_ids
                    .remove(&task_id)
                    .ok_or_else(|| Error::bad_gateway("Unknown relay request."))?;
                active.remove(&request_id);
                let response = match result {
                    Ok(Some(response)) => response,
                    Ok(None) => continue,
                    Err(error) => request_failure(request_id, error.status, &error.message),
                };

                let frame = serde_json::to_string(&Frame::Response(response))?;
                socket
                    .send(Message::Text(frame.into()))
                    .await
                    .map_err(Error::internal)?;
            }
            Some(frame) = frames.recv() => {
                socket.send(Message::Text(serde_json::to_string(&frame)?.into()))
                    .await.map_err(Error::internal)?;
            }
            message = tokio::time::timeout(Duration::from_secs(45), socket.next()) => {
                let message = message.map_err(|_| Error::unavailable("Relay heartbeat lost."))?;
                match message {
                    Some(Ok(Message::Text(message))) => {
                        let request = match serde_json::from_str::<Frame>(&message)? {
                            Frame::Request(request) => request,
                            Frame::StreamCredit { id } if version >= 2 => {
                                if let Some((_, credit)) = active.get(&id)
                                    && credit.available_permits() == 0 {
                                    credit.add_permits(1);
                                }
                                continue;
                            }
                            Frame::Cancel { id } if version >= 2 => {
                                if let Some((task, _)) = active.remove(&id) {
                                    task.abort();
                                }
                                continue;
                            }
                            _ => return Err(Error::bad("Unexpected relay frame.")),
                        };
                        if requests.len() >= MAX_IN_FLIGHT {
                            let response = request_failure(request.id, 503, "Installation busy.");
                            let frame = serde_json::to_string(&Frame::Response(response))?;
                            socket
                                .send(Message::Text(frame.into()))
                                .await
                                .map_err(Error::internal)?;
                            continue;
                        }

                        let request_id = request.id.clone();
                        let router = router.clone();
                        let credit = std::sync::Arc::new(Semaphore::new(0));
                        let streaming = if version >= 2 {
                            Some(StreamOutput {
                                frames: output.clone(),
                                credit: credit.clone(),
                            })
                        } else {
                            None
                        };
                        let task = requests.spawn(dispatch(router, request, streaming));
                        active.insert(request_id.clone(), (task.clone(), credit));
                        request_ids.insert(task.id(), request_id);
                    }
                    Some(Ok(Message::Ping(bytes))) => {
                        socket.send(Message::Pong(bytes)).await.map_err(Error::internal)?;
                    }
                    Some(Ok(Message::Pong(_))) => {}
                    _ => return Err(Error::unavailable("Relay connection closed.")),
                }
            }
        }
    }
}

fn request_failure(id: String, status: u16, message: &str) -> ApiResponse {
    ApiResponse {
        id,
        status,
        headers: vec![("content-type".into(), "application/json".into())],
        body: serde_json::to_vec(&serde_json::json!({ "error": message })).unwrap(),
    }
}

struct StreamOutput {
    frames: mpsc::Sender<Frame>,
    credit: std::sync::Arc<Semaphore>,
}

async fn dispatch(
    router: Router,
    input: ApiRequest,
    streaming: Option<StreamOutput>,
) -> Result<Option<ApiResponse>> {
    if !leo_relay_protocol::api_path(&input.path) || input.body.len() > MAX_BODY {
        return Err(Error::bad("Invalid relayed API request."));
    }
    if leo_relay_protocol::stream_path(&input.path) && streaming.is_none() {
        return Ok(Some(request_failure(
            input.id,
            501,
            "Streaming requires relay protocol 2.",
        )));
    }
    let role = match input.role {
        Role::Owner => InstallationRole::Owner,
        Role::Member => InstallationRole::Member,
    };
    let mut request = Request::builder()
        .method(input.method.as_str())
        .uri(&input.path)
        .header("host", "localhost")
        .body(Body::from(input.body))
        .map_err(|_| Error::bad_gateway("Invalid installation request."))?;
    for (name, value) in input.headers {
        if leo_relay_protocol::request_header(&name)
            && let (Ok(name), Ok(value)) =
                (HeaderName::try_from(name), HeaderValue::try_from(value))
        {
            request.headers_mut().append(name, value);
        }
    }

    request
        .extensions_mut()
        .insert(ConnectInfo("127.0.0.1:0".parse::<SocketAddr>().unwrap()));
    request
        .extensions_mut()
        .insert(InstallationIdentity::trusted(role, &input.account_id));

    let deadline = tokio::time::Instant::now() + REQUEST_TIMEOUT;
    let response = tokio::time::timeout_at(deadline, router.oneshot(request))
        .await
        .map_err(|_| Error::gateway_timeout("Installation request timed out."))?
        .map_err(|_| Error::bad_gateway("Installation handler failed."))?;
    let status = response.status().as_u16();
    let headers = response
        .headers()
        .iter()
        .filter(|(name, _)| leo_relay_protocol::response_header(name.as_str()))
        .filter_map(|(name, value)| {
            value
                .to_str()
                .ok()
                .map(|value| (name.to_string(), value.to_owned()))
        })
        .collect();
    let is_stream = response
        .headers()
        .get("content-type")
        .is_some_and(|value| value.as_bytes().starts_with(b"text/event-stream"));
    if is_stream && let Some(output) = streaming {
        let id = input.id;
        output
            .frames
            .send(Frame::StreamStart(ApiResponse {
                id: id.clone(),
                status,
                headers,
                body: Vec::new(),
            }))
            .await
            .map_err(|_| Error::unavailable("Relay closed."))?;

        let mut body = response.into_body().into_data_stream();
        let result: Result<()> = async {
            // Acquire credit before polling the installation body. A slow browser
            // therefore pauses this subscription, never the tunnel's receive loop.
            loop {
                output
                    .credit
                    .acquire()
                    .await
                    .map_err(Error::internal)?
                    .forget();
                let Some(chunk) = body.next().await else {
                    break;
                };
                let chunk = chunk.map_err(|_| Error::bad_gateway("Installation stream failed."))?;
                if chunk.is_empty() {
                    output.credit.add_permits(1);
                    continue;
                }
                for (index, piece) in chunk.chunks(MAX_STREAM_CHUNK).enumerate() {
                    if index > 0 {
                        output
                            .credit
                            .acquire()
                            .await
                            .map_err(Error::internal)?
                            .forget();
                    }
                    output
                        .frames
                        .send(Frame::StreamChunk {
                            id: id.clone(),
                            body: piece.to_vec(),
                        })
                        .await
                        .map_err(|_| Error::unavailable("Relay closed."))?;
                }
            }
            Ok(())
        }
        .await;
        let _ = output
            .frames
            .send(Frame::StreamEnd {
                id,
                failed: result.is_err(),
            })
            .await;
        return Ok(None);
    }

    let body = tokio::time::timeout_at(deadline, to_bytes(response.into_body(), MAX_BODY))
        .await
        .map_err(|_| Error::gateway_timeout("Installation response timed out."))?
        .map_err(|error| {
            let oversized = std::error::Error::source(&error)
                .is_some_and(<dyn std::error::Error>::is::<http_body_util::LengthLimitError>);
            if oversized {
                Error::too_large("Installation response is too large.")
            } else {
                Error::bad_gateway("Installation response failed.")
            }
        })?
        .to_vec();

    Ok(Some(ApiResponse {
        id: input.id,
        status,
        headers,
        body,
    }))
}
