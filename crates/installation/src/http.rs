use crate::{
    auth::{InstallationIdentity, InstallationRole, safe_equal},
    config::now,
    error::{Error, Result},
    execution::secret,
    service::Service,
    validation::uuid,
};
use axum::{
    Json, Router,
    body::{Body, to_bytes},
    extract::{ConnectInfo, Request, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::any,
};
use serde::Serialize;
use serde_json::Value;
use std::{
    collections::HashMap,
    net::{IpAddr, SocketAddr},
    sync::{Arc, Mutex},
};
use tower_http::compression::CompressionLayer;

#[derive(Clone)]
pub struct App {
    pub service: Arc<Service>,
    limits: Arc<Mutex<RateLimits>>,
    maintenance: String,
    pub toolkit: Value,
}

#[derive(Clone, PartialEq, Eq, Hash)]
enum RateLimitSubject {
    Peer(IpAddr),
    Account(String),
}

type RateLimits = HashMap<(RateLimitSubject, String), (i64, u32)>;

pub async fn router(service: Arc<Service>) -> Result<Router> {
    let maintenance = secret(&service.config.data_dir, "maintenance-token").await?;
    let toolkit = if let Ok(directory) = std::env::var("CAIRN_TOOLKIT_DIR") {
        serde_json::from_slice(
            &tokio::fs::read(std::path::Path::new(&directory).join("manifest.json")).await?,
        )?
    } else {
        Value::Null
    };
    let app = App {
        service,
        maintenance,
        toolkit,
        limits: Arc::default(),
    };
    // Node and execution traffic is never compressed: it carries binary transfers whose
    // readers require the declared Content-Length, and a proxy in front of the public
    // origin may add Accept-Encoding to these machine requests.
    let internal = Router::new()
        .route("/internal/deployment-lease", any(lease))
        .route(
            "/internal/nodes/release",
            any(crate::nodes::maintenance::downloads),
        )
        .route(
            "/internal/nodes/install.sh",
            any(crate::nodes::maintenance::downloads),
        )
        .route(
            "/internal/nodes/host.py",
            any(crate::nodes::maintenance::downloads),
        )
        .route(
            "/internal/nodes/stream/{id}",
            any(crate::nodes::transport::stream),
        )
        .route("/internal/nodes/{*path}", any(crate::nodes::internal))
        .route(
            "/internal/node-restore/{*path}",
            any(crate::nodes::restore::handle),
        )
        .route(
            "/internal/node-workspace/{*path}",
            any(crate::nodes::workspace::handle),
        )
        .route(
            "/internal/execution/{*path}",
            any(crate::nodes::transport::proxy),
        );

    Ok(Router::new()
        .route("/health", any(health))
        .route("/api/mcp", any(crate::mcp_server::handle))
        .route("/mcp-workspace", any(crate::mcp_server::handle))
        .route("/mcp-gateway/{id}", any(crate::mcp_server::handle))
        .route("/api/{*path}", any(api))
        .fallback(|| async { Error::unauthorized("Access this installation through the Beacon.") })
        .layer(CompressionLayer::new())
        .merge(internal)
        .layer(middleware::from_fn_with_state(app.clone(), security))
        .with_state(app))
}

fn header<'a>(headers: &'a HeaderMap, name: &str) -> &'a str {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
}

async fn security(State(app): State<App>, mut request: Request, next: Next) -> Response {
    let path = request.uri().path().to_owned();
    let peer = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map_or(IpAddr::from([127, 0, 0, 1]), |peer| peer.0.ip());
    let subject = request
        .extensions()
        .get::<InstallationIdentity>()
        .map_or_else(
            || RateLimitSubject::Peer(peer),
            |identity| RateLimitSubject::Account(identity.oauth_binding().to_owned()),
        );
    let head = request.method() == "HEAD";
    let outcome = (|| {
        check_security(&app, request.headers(), &path, &subject)?;
        authenticate_installation(&request)
    })();
    let mut response = match outcome {
        Ok(()) => {
            if head {
                *request.method_mut() = axum::http::Method::GET;
            }
            next.run(request).await
        }
        Err(error) => error.into_response(),
    };
    for (name, value) in [
        ("x-content-type-options", "nosniff"),
        ("referrer-policy", "same-origin"),
        ("x-frame-options", "DENY"),
        (
            "content-security-policy",
            "default-src 'self'; script-src 'self' 'wasm-unsafe-eval'; style-src 'self' \
            'unsafe-inline'; font-src 'self' data:; img-src 'self' data: blob:; connect-src \
            'self'; object-src 'none'; base-uri 'none'; frame-ancestors 'none'; form-action \
            'self'",
        ),
    ] {
        if !response.headers().contains_key(name) {
            response
                .headers_mut()
                .insert(name, HeaderValue::from_static(value));
        }
    }
    let cache = cache_policy(&path, &response);
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
    if head {
        *response.body_mut() = Body::empty();
    }
    response
}

fn is_dynamic(path: &str) -> bool {
    path.starts_with("/api/")
        || path == "/mcp-workspace"
        || path.starts_with("/mcp-gateway/")
        || path.starts_with("/internal/")
        || path == "/health"
}

/// Dynamic responses are never cached; the app shell revalidates; assets are cached.
fn cache_policy(path: &str, response: &Response) -> &'static str {
    if is_dynamic(path) || !response.status().is_success() {
        return "no-store";
    }
    let html = response
        .headers()
        .get(header::CONTENT_TYPE)
        .is_some_and(|v| v.to_str().unwrap_or("").starts_with("text/html"));
    if html || ["/theme.js", "/sw.js", "/manifest.webmanifest"].contains(&path) {
        return "no-cache";
    }
    "public, max-age=3600"
}

fn is_node_traffic(path: &str) -> bool {
    path.starts_with("/internal/nodes/")
        || path.starts_with("/internal/execution/")
        || path.starts_with("/internal/node-restore/")
        || path.starts_with("/internal/node-workspace/")
}

/// Requests per minute shared by every path of a Cairn account or machine peer: `(bucket, limit)`.
fn general_limit(path: &str) -> (&'static str, u32) {
    if is_node_traffic(path) {
        ("nodes", 100_000)
    } else if path.starts_with("/mcp-gateway/") {
        ("", 600)
    } else {
        ("", 300)
    }
}

fn too_many_requests() -> Error {
    Error::too_many_requests("Too many requests. Try again later.")
}

fn rate_limit(app: &App, subject: &RateLimitSubject, path: &str) -> Result<()> {
    let mut limits = app.limits.lock().unwrap();
    // Expiration also bounds memory used by unauthenticated clients.
    if limits.len() > 10000 {
        limits.retain(|_, (expires, _)| *expires > now());
        if limits.len() > 10000 {
            return Err(too_many_requests());
        }
    }
    let (bucket, max) = general_limit(path);
    let buckets = std::iter::once((bucket, max, 60_000));
    for (key, max, window) in buckets {
        let entry = limits
            .entry((subject.clone(), key.to_owned()))
            .or_insert((now() + window, 0));
        if entry.0 <= now() {
            *entry = (now() + window, 0);
        }
        entry.1 += 1;
        if entry.1 > max {
            return Err(too_many_requests());
        }
    }
    Ok(())
}

fn development_origin(origin: &str) -> bool {
    std::env::var("NODE_ENV").unwrap_or_default() != "production"
        && ["http://localhost:5178", "http://127.0.0.1:5178"].contains(&origin)
}

fn check_security(
    app: &App,
    headers: &HeaderMap,
    path: &str,
    subject: &RateLimitSubject,
) -> Result<()> {
    let origin = url::Url::parse(&app.service.config.public_url).unwrap();
    let host = header(headers, "host");
    let authority = host
        .parse::<axum::http::uri::Authority>()
        .map_err(|_| Error::forbidden("Unexpected host."))?;
    // A node's private TLS proxy can preserve a LAN/VPN Host unrelated to
    // PUBLIC_URL. These routes authenticate their own codes, bearer tokens or
    // disk grants; Host is not a node credential. Browser Origin checks remain.
    if !is_node_traffic(path)
        && ![
            origin.host_str().unwrap_or(""),
            "localhost",
            "127.0.0.1",
            "[::1]",
        ]
        .contains(&authority.host())
    {
        return Err(Error::forbidden("Unexpected host."));
    }
    let requested = header(headers, "origin");
    if !requested.is_empty()
        && requested != app.service.config.public_url
        && !development_origin(requested)
    {
        return Err(Error::forbidden("Unexpected origin."));
    }
    rate_limit(app, subject, path)?;
    Ok(())
}

/// Browser credentials never authenticate an installation. Only the outbound
/// relay attaches verified in-process context; machine channels keep their own
/// credentials and remain independent of beacon-account availability.
fn authenticate_installation(request: &Request) -> Result<()> {
    let path = request.uri().path();
    let machine = path == "/health"
        || path == "/internal/deployment-lease"
        || path.starts_with("/internal/nodes/")
        || path.starts_with("/internal/node-restore/")
        || path.starts_with("/internal/node-workspace/")
        || path.starts_with("/internal/execution/")
        || path == "/mcp-workspace"
        || path.starts_with("/mcp-gateway/");
    if machine {
        return Ok(());
    }
    let identity = request
        .extensions()
        .get::<InstallationIdentity>()
        .ok_or_else(|| Error::unauthorized("Access this installation through the Beacon."))?;
    if let Some(token) = &identity.public_artifact {
        if path == format!("/api/shared-artifacts/{token}")
            && crate::artifacts::sharing::public_read(path, request.method().as_str())
        {
            return Ok(());
        }
        return Err(Error::forbidden(
            "This public link only permits reading its file.",
        ));
    }
    if identity.role != InstallationRole::Owner && owner_operation(request.method().as_str(), path)
    {
        return Err(Error::forbidden(
            "Only the installation owner can manage this resource.",
        ));
    }
    Ok(())
}

/// Installation management permissions are declared here, before dispatch.
fn owner_operation(method: &str, path: &str) -> bool {
    let segments = path
        .trim_start_matches("/api/")
        .split('/')
        .collect::<Vec<_>>();
    let read = matches!(method, "GET" | "HEAD");
    match segments.as_slice() {
        // Storage configuration lives under nodes; credentials also include
        // MCP connection flows and per-agent GitHub tokens below.
        [
            "nodes" | "accounts" | "onepassword" | "mcps" | "connections" | "settings" | "audit"
            | "github" | "agent-avatars",
            ..,
        ] => true,
        ["agents"] | ["agents", _, "avatar"] => !read,
        ["skills" | "projects", ..] => !read,
        ["agents", ..] => true,
        ["tasks", _] => method == "DELETE",
        ["task-authors", ..] => true,
        ["runs", _, "artifacts", _, "visibility"] => !read,
        _ => false,
    }
}

pub struct Input {
    pub method: String,
    pub path: String,
    pub query: HashMap<String, String>,
    pub headers: HeaderMap,
    pub body: Value,
    pub identity: Option<InstallationIdentity>,
}

impl Input {
    pub async fn read(request: Request) -> Result<Self> {
        let (parts, body) = request.into_parts();
        let query = serde_urlencoded::from_str(parts.uri.query().unwrap_or(""))
            .map_err(|_| Error::bad("Invalid query parameters."))?;
        let path = parts.uri.path();
        let large = path.ends_with("/result") || path.ends_with("/mcp");
        let limit = if path.starts_with("/internal/node-workspace/") && large {
            2_000_000
        } else {
            150000
        };
        let bytes = to_bytes(body, limit)
            .await
            .map_err(|_| Error::too_large("Request body is too large."))?;
        let body = if bytes.is_empty() {
            Value::Null
        } else if header(&parts.headers, "content-type")
            .starts_with("application/x-www-form-urlencoded")
        {
            serde_json::to_value(
                serde_urlencoded::from_bytes::<HashMap<String, String>>(&bytes)
                    .map_err(|_| Error::bad("Invalid form body."))?,
            )?
        } else {
            serde_json::from_slice(&bytes).map_err(|_| Error::bad("Invalid JSON body."))?
        };
        Ok(Self {
            method: parts.method.to_string(),
            path: parts.uri.path().to_owned(),
            query,
            headers: parts.headers,
            body,
            identity: parts.extensions.get::<InstallationIdentity>().cloned(),
        })
    }

    pub fn number(&self, name: &str, default: i64, min: i64, max: i64) -> Result<i64> {
        let n = self
            .query
            .get(name)
            .map(|n| {
                n.parse::<i64>()
                    .map_err(|_| Error::bad("Invalid numeric parameter."))
            })
            .transpose()?
            .unwrap_or(default);
        if n < min || n > max {
            return Err(Error::bad(
                "Numeric parameter is outside the permitted range.",
            ));
        }
        Ok(n)
    }

    pub fn string(&self, name: &str, max: usize) -> Result<&str> {
        let text = self.body[name]
            .as_str()
            .ok_or_else(|| Error::bad(format!("{name}: expected text")))?;
        if text.chars().count() > max {
            return Err(Error::bad(format!("{name}: text is too long")));
        }
        Ok(text)
    }

    pub fn boolean(&self, name: &str) -> Result<bool> {
        self.body[name]
            .as_bool()
            .ok_or_else(|| Error::bad(format!("{name}: expected a boolean")))
    }
}

#[derive(Serialize)]
struct Execution {
    backend: &'static str,
    ready: bool,
}

#[derive(Serialize)]
struct Tools {
    codex: Option<String>,
    gh: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Health<'a> {
    status: &'static str,
    commit: String,
    runtime_id: String,
    base_image: Option<String>,
    tools: Tools,
    toolkit: &'a Value,
    execution: Execution,
    active_runs: usize,
    maintenance: bool,
}

fn env(key: &str) -> Option<String> {
    std::env::var(key).ok()
}

/// The Firecracker runner is ready when it reports this release's runtime.
async fn runner_ready(app: &App) -> bool {
    let response = app
        .service
        .http
        .get(format!("{}/health", app.service.config.runner_url))
        .timeout(std::time::Duration::from_secs(2))
        .send()
        .await;
    let Ok(response) = response else {
        return false;
    };
    if !response.status().is_success() {
        return false;
    }
    let Ok(health) = response.json::<Value>().await else {
        return false;
    };
    let runtime = env("APP_RUNTIME_ID").unwrap_or_else(|| "development".into());
    health["backend"] == "firecracker" && health["status"] == "ok" && health["runtimeId"] == runtime
}

async fn health(State(app): State<App>, request: Request) -> Result<Response> {
    if !["GET", "HEAD"].contains(&request.method().as_str()) {
        return Err(Error::method_not_allowed("Method not allowed."));
    }
    let commit = env("APP_COMMIT").unwrap_or_else(|| "development".into());
    let active_runs = app.service.worker.active.lock().await.len();
    let execution = if app.service.config.runner_url.is_empty() {
        Execution {
            backend: "local",
            ready: true,
        }
    } else {
        Execution {
            backend: "firecracker",
            ready: runner_ready(&app).await,
        }
    };
    let status = if execution.ready {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    let body = Health {
        status: "ok",
        runtime_id: env("APP_RUNTIME_ID").unwrap_or_else(|| commit.clone()),
        commit,
        base_image: env("APP_BASE_IMAGE"),
        tools: Tools {
            codex: env("APP_CODEX_VERSION"),
            gh: env("APP_GH_VERSION"),
        },
        toolkit: &app.toolkit,
        execution,
        active_runs,
        maintenance: app.service.store.kv("deployment-lease").await?.is_some(),
    };
    Ok((status, Json(body)).into_response())
}

async fn lease(State(app): State<App>, request: Request) -> Result<Json<Value>> {
    let input = Input::read(request).await?;
    if !["POST", "DELETE"].contains(&input.method.as_str()) {
        return Err(Error::method_not_allowed("Method not allowed."));
    }
    if !safe_equal(
        header(&input.headers, "authorization"),
        &format!("Bearer {}", app.maintenance),
    ) {
        return Err(Error::unauthorized("Invalid maintenance credential."));
    }
    let owner = input.string("owner", 100)?.to_owned();
    uuid(&owner)?;
    let release = input.method == "DELETE";
    Ok(Json(
        app.service
            .worker
            .deployment_lease(&app.service, owner, release)
            .await?,
    ))
}

/// A request left for the next router stage.
enum Route {
    Done(Response),
    Next(Request),
}

/// Routes that read the raw request body or stream their response.
async fn raw_route(app: &App, path: &str, request: Request) -> Result<Route> {
    let s = &app.service;
    let segments: Vec<_> = path.split('/').collect();
    let response = match segments.as_slice() {
        ["", "api", "agents", agent, "avatar"] => {
            crate::agent_avatars::http(s, agent, request).await?
        }
        ["", "api", "shared-artifacts", token] => {
            crate::artifacts::sharing::http(s, token, request).await?
        }
        ["", "api", "runs", run, rest @ ..] => {
            let active = (*run).to_owned();
            s.store
                .read(move |db| crate::conversation_lifecycle::require_active_run(db, &active))
                .await?;
            match rest {
                ["artifacts", artifact, "visibility"] => {
                    set_artifact_visibility(s, run, artifact, request).await?
                }
                ["artifacts", rest @ ..] if rest.len() <= 1 => {
                    crate::artifacts::http(s, run, rest.first().copied(), request).await?
                }
                ["stream"] => {
                    crate::live::http(s.clone(), "runs", run, Input::read(request).await?).await?
                }
                _ => return Ok(Route::Next(request)),
            }
        }
        ["", "api", "chats", chat, "attachments", id] => {
            s.attachment_http(chat, id, request).await?
        }
        ["", "api", "chats", "stream"] => {
            crate::live::http(s.clone(), "chats", "", Input::read(request).await?).await?
        }
        ["", "api", "chats", id, "stream"] => {
            crate::live::http(s.clone(), "chats", id, Input::read(request).await?).await?
        }
        _ => return Ok(Route::Next(request)),
    };
    Ok(Route::Done(response))
}

async fn set_artifact_visibility(
    s: &Service,
    run: &str,
    artifact: &str,
    request: Request,
) -> Result<Response> {
    if request.method() != "PUT" {
        return Err(Error::method_not_allowed("Method not allowed."));
    }
    let input = Input::read(request).await?;
    let visibility = crate::artifacts::sharing::visibility(&input.body)?;
    let result = crate::artifacts::sharing::set(s, run, artifact, visibility, None).await?;
    Ok(Json(result).into_response())
}

fn json_bytes(bytes: Vec<u8>) -> Response {
    ([(header::CONTENT_TYPE, "application/json")], bytes).into_response()
}

/// Large run listings, serialized from stored JSON without building `Value` trees.
async fn run_pages(s: &Service, input: &Input) -> Result<Option<Response>> {
    if input.method != "GET" {
        return Ok(None);
    }
    if input.path == "/api/runs" {
        let limit = input.number("limit", 40, 1, 100)?;
        let offset = input.number("offset", 0, 0, i64::MAX)?;
        let status = input.query.get("status").cloned();
        let task = input.query.get("taskId").cloned();
        let bytes = s
            .store
            .read(move |db| {
                let page = db.run_page(status.as_deref(), task.as_deref(), limit, offset)?;
                Ok(serde_json::to_vec(&page)?)
            })
            .await?;
        return Ok(Some(json_bytes(bytes)));
    }
    let Some(id) = input
        .path
        .strip_prefix("/api/runs/")
        .and_then(|path| path.strip_suffix("/events"))
        .filter(|id| !id.contains('/'))
    else {
        return Ok(None);
    };
    let after = input.number("after", 0, 0, i64::MAX)?;
    let limit = input.number("limit", 100, 1, 500)?;
    let id = id.to_owned();
    let bytes = s
        .store
        .read(move |db| {
            db.require_run(&id)?;
            Ok(serde_json::to_vec(&db.event_page(&id, after, limit)?)?)
        })
        .await?;
    Ok(Some(json_bytes(bytes)))
}

async fn api(State(app): State<App>, request: Request) -> Result<Response> {
    let path = request.uri().path().to_owned();
    let request = match raw_route(&app, &path, request).await? {
        Route::Done(response) => return Ok(response),
        Route::Next(request) => request,
    };
    let input = Input::read(request).await?;
    let s = &app.service;
    if let Some(response) = run_pages(s, &input).await? {
        return Ok(response);
    }
    Ok(Json(crate::api::dispatch(s, &input).await?).into_response())
}
