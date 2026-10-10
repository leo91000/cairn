//! A run's MCP channel: run-scoped MCP served during an attempt on a private
//! socket in the run home (`microvm::mcp::SOCKET`). The VM controller relays
//! its guest's loopback origin there. The manager serves it for its local
//! runner; a remote node's connector forwards it over the node's session.
use crate::{
    error::{Error, Result},
    service::Service,
    skills::private_dir,
};
use axum::{
    Router,
    body::{Body, to_bytes},
    extract::{Request, State},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio::net::UnixListener;
use tokio_util::sync::CancellationToken;

/// Request headers an MCP client needs; the run token travels in `authorization`.
const REQUEST_HEADERS: [&str; 6] = [
    "authorization",
    "content-type",
    "accept",
    "mcp-protocol-version",
    "mcp-method",
    "mcp-session-id",
];

const RESPONSE_HEADERS: [&str; 3] = ["content-type", "mcp-protocol-version", "cache-control"];
/// Above the manager's own MCP body limit, which still applies once forwarded.
const MAX_REQUEST_BYTES: usize = 1_000_000;

/// One MCP request forwarded by a remote node to the manager.
#[derive(Serialize, Deserialize)]
struct Forwarded {
    method: String,
    path: String,
    headers: BTreeMap<String, String>,
    body: String,
}

/// Serves the run's channel until dropped.
pub struct Channel {
    stop: CancellationToken,
    path: PathBuf,
}

impl Drop for Channel {
    fn drop(&mut self) {
        self.stop.cancel();
        let _ = std::fs::remove_file(&self.path);
    }
}

async fn bind(path: &Path) -> Result<UnixListener> {
    if let Some(home) = path.parent() {
        private_dir(home).await?;
    }
    match tokio::fs::remove_file(path).await {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let listener = UnixListener::bind(path)?;
    tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).await?;
    Ok(listener)
}

/// The manager side: answers run-scoped MCP for `run` in `home`.
pub async fn serve(s: &Arc<Service>, home: &Path, run: &str) -> Result<Channel> {
    let path = home.join(crate::microvm::mcp::SOCKET);
    let listener = bind(&path).await?;
    let stop = CancellationToken::new();
    let app = Router::new()
        .fallback(answer)
        .with_state((s.clone(), run.to_owned()));
    let stopping = stop.clone();
    let shutdown = s.shutdown.clone();
    tokio::spawn(async move {
        let stopped = async move {
            tokio::select! {
                () = stopping.cancelled() => {},
                () = shutdown.cancelled() => {},
            }
        };
        if let Err(error) = axum::serve(listener, app)
            .with_graceful_shutdown(stopped)
            .await
        {
            tracing::warn!(%error, "Run MCP channel stopped");
        }
    });
    Ok(Channel { stop, path })
}

async fn answer(State((s, run)): State<(Arc<Service>, String)>, request: Request) -> Response {
    crate::mcp_server::run_scoped(&s, &run, request)
        .await
        .into_response()
}

/// The manager side of a remote node's channel. The caller has checked that the
/// node owns the attempt executing `run`.
pub(crate) async fn relayed(s: &Arc<Service>, run: &str, forwarded: &Value) -> Result<Response> {
    let invalid = || Error::bad("Invalid MCP request.");
    let forwarded = Forwarded::deserialize(forwarded).map_err(|_| invalid())?;
    let mut request = Request::builder()
        .method(forwarded.method.as_str())
        .uri(forwarded.path.as_str());
    for (name, value) in &forwarded.headers {
        if REQUEST_HEADERS.contains(&name.as_str()) {
            request = request.header(name, value);
        }
    }
    let request = request
        .body(Body::from(forwarded.body))
        .map_err(|_| invalid())?;
    crate::mcp_server::run_scoped(s, run, request).await
}

/// The node connection an attempt's channel forwards through.
struct Session {
    client: reqwest::Client,
    master: url::Url,
    token: String,
    attempt: String,
}

/// The node side: forwards the attempt's MCP requests to the manager over the
/// node's authenticated session until `stop`. No other network path is opened.
pub async fn forward(
    path: PathBuf,
    client: reqwest::Client,
    master: url::Url,
    token: String,
    attempt: String,
    stop: CancellationToken,
) -> Result<()> {
    let listener = bind(&path).await?;
    let session = Arc::new(Session {
        client,
        master,
        token,
        attempt,
    });
    let app = Router::new().fallback(send).with_state(session);
    let result = axum::serve(listener, app)
        .with_graceful_shutdown(stop.cancelled_owned())
        .await;
    let _ = tokio::fs::remove_file(path).await;
    result.map_err(Error::from)
}

async fn send(State(session): State<Arc<Session>>, request: Request) -> Response {
    send_request(&session, request).await.into_response()
}

async fn send_request(session: &Session, request: Request) -> Result<Response> {
    let (parts, body) = request.into_parts();
    let body = to_bytes(body, MAX_REQUEST_BYTES)
        .await
        .map_err(|_| Error::too_large("Request body is too large."))?;
    let headers = REQUEST_HEADERS
        .iter()
        .filter_map(|name| {
            let value = parts.headers.get(*name)?.to_str().ok()?;
            Some(((*name).to_owned(), value.to_owned()))
        })
        .collect();
    let forwarded = Forwarded {
        method: parts.method.as_str().to_owned(),
        path: parts.uri.path().to_owned(),
        headers,
        body: String::from_utf8(body.to_vec()).map_err(|_| Error::bad("Invalid MCP request."))?,
    };

    let url = session
        .master
        .join(&format!("internal/node-workspace/{}/mcp", session.attempt))
        .map_err(Error::internal)?;
    let response = session
        .client
        .post(url)
        .bearer_auth(&session.token)
        .json(&forwarded)
        .send()
        .await
        .map_err(|_| Error::unavailable("Workspace connection interrupted."))?;

    // The manager's answer is the MCP response, rejections included.
    let mut answer = Response::builder().status(response.status().as_u16());
    for name in RESPONSE_HEADERS {
        if let Some(value) = response.headers().get(name) {
            answer = answer.header(name, value.as_bytes());
        }
    }
    let bytes = response
        .bytes()
        .await
        .map_err(|_| Error::unavailable("Workspace connection interrupted."))?;
    answer.body(Body::from(bytes)).map_err(Error::internal)
}
