use crate::{
    auth::{InstallationIdentity, InstallationRole, safe_equal},
    config::now,
    error::{Error, Result},
    execution::secret,
    service::Service,
    validation::{text, uuid},
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
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    net::{IpAddr, SocketAddr},
    sync::{Arc, Mutex},
};
use tower_http::{
    compression::CompressionLayer,
    services::{ServeDir, ServeFile},
};

#[derive(Clone)]
pub struct App {
    pub service: Arc<Service>,
    limits: Arc<Mutex<RateLimits>>,
    maintenance: String,
    pub toolkit: Value,
}

type RateLimits = HashMap<(IpAddr, String), (i64, u32)>;

pub async fn router(service: Arc<Service>) -> Result<Router> {
    let maintenance = secret(&service.config.data_dir, "maintenance-token").await?;
    let toolkit = if let Ok(directory) = std::env::var("LEO_TOOLKIT_DIR") {
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
    Ok(Router::new()
        .route("/health", any(health))
        .route("/mcp", any(crate::mcp_server::handle))
        .route("/mcp-workspace", any(crate::mcp_server::handle))
        .route("/mcp-gateway/{id}", any(crate::mcp_server::handle))
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
        )
        .route("/api/{*path}", any(api))
        .route("/oauth/{*path}", any(oauth))
        .route("/.well-known/{*path}", any(metadata))
        .fallback_service(ServeDir::new("dist").fallback(ServeFile::new("dist/index.html")))
        .layer(CompressionLayer::new())
        .layer(middleware::from_fn_with_state(app.clone(), security))
        .with_state(app))
}

fn header<'a>(headers: &'a HeaderMap, name: &str) -> &'a str {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
}

pub fn cookie(headers: &HeaderMap) -> String {
    header(headers, "cookie")
        .split(';')
        .filter_map(|entry| entry.trim().split_once('='))
        .find(|(name, _)| *name == "leo_session")
        .map(|(_, value)| value.to_owned())
        .unwrap_or_default()
}

async fn security(State(app): State<App>, mut request: Request, next: Next) -> Response {
    let path = request.uri().path().to_owned();
    let peer = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map_or(IpAddr::from([127, 0, 0, 1]), |peer| peer.0.ip());
    let head = request.method() == "HEAD";
    let outcome = async {
        check_security(
            &app,
            request.headers(),
            request.method().as_str(),
            &path,
            peer,
        )?;
        authenticate_api(&app, &mut request).await
    }
    .await;
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
        || path == "/mcp"
        || path == "/mcp-workspace"
        || path.starts_with("/mcp-gateway/")
        || path.starts_with("/oauth/")
        || path.starts_with("/internal/")
        || path == "/health"
}

/// Dynamic responses are never cached; the app shell revalidates; assets are cached.
fn cache_policy(path: &str, response: &Response) -> &'static str {
    if is_dynamic(path) {
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

/// Requests per minute shared by every path of a peer: `(bucket, limit)`.
fn general_limit(path: &str) -> (&'static str, u32) {
    if is_node_traffic(path) {
        ("nodes", 100_000)
    } else if path.starts_with("/mcp-gateway/") {
        ("", 600)
    } else {
        ("", 300)
    }
}

/// Stricter limits on credential endpoints: `(limit, window in ms)`.
fn endpoint_limit(path: &str) -> Option<(u32, i64)> {
    match path {
        "/api/setup" => Some((5, 60_000)),
        "/api/login" => Some((10, 60_000)),
        "/oauth/register" => Some((10, 3_600_000)),
        _ => None,
    }
}

fn too_many_requests() -> Error {
    Error::too_many_requests("Too many requests. Try again later.")
}

fn rate_limit(app: &App, peer: IpAddr, path: &str) -> Result<()> {
    let mut limits = app.limits.lock().unwrap();
    // Expiration also bounds memory used by unauthenticated clients.
    if limits.len() > 10000 {
        limits.retain(|_, (expires, _)| *expires > now());
        if limits.len() > 10000 {
            return Err(too_many_requests());
        }
    }
    let (bucket, max) = general_limit(path);
    let buckets = std::iter::once((bucket, max, 60_000))
        .chain(endpoint_limit(path).map(|(max, window)| (path, max, window)));
    for (key, max, window) in buckets {
        let entry = limits
            .entry((peer, key.to_owned()))
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
    method: &str,
    path: &str,
    peer: IpAddr,
) -> Result<()> {
    let origin = url::Url::parse(&app.service.config.public_url).unwrap();
    let host = header(headers, "host");
    let authority = host
        .parse::<axum::http::uri::Authority>()
        .map_err(|_| Error::forbidden("Unexpected host."))?;
    if ![
        origin.host_str().unwrap_or(""),
        "localhost",
        "127.0.0.1",
        "[::1]",
    ]
    .contains(&authority.host())
    {
        return Err(Error::forbidden("Unexpected host."));
    }
    let public_artifact = crate::artifacts::sharing::public_read(path, method);
    let requested = header(headers, "origin");
    if !public_artifact
        && !requested.is_empty()
        && requested != app.service.config.public_url
        && !development_origin(requested)
    {
        return Err(Error::forbidden("Unexpected origin."));
    }
    rate_limit(app, peer, path)?;
    Ok(())
}

/// The authentication seam for every protected API route, including raw bodies
/// and SSE. Only trusted in-process context can replace local session auth.
async fn authenticate_api(app: &App, request: &mut Request) -> Result<()> {
    let path = request.uri().path();
    let method = request.method().as_str();
    if !path.starts_with("/api/")
        || ["/api/session", "/api/setup", "/api/login"].contains(&path)
        || crate::artifacts::sharing::public_read(path, method)
    {
        return Ok(());
    }

    let identity = if let Some(identity) = request.extensions().get::<InstallationIdentity>() {
        identity.clone()
    } else {
        let headers = request.headers();
        let session = app
            .service
            .auth
            .read(&cookie(headers))
            .await?
            .ok_or_else(|| Error::unauthorized("Please sign in."))?;
        if !["GET", "HEAD", "OPTIONS"].contains(&method)
            && !safe_equal(header(headers, "x-csrf-token"), text(&session, "csrf"))
        {
            return Err(Error::forbidden(
                "Invalid CSRF token. Refresh the page and try again.",
            ));
        }
        InstallationIdentity::trusted(InstallationRole::Owner)
    };

    if identity.role != InstallationRole::Owner && owner_operation(method, path) {
        return Err(Error::forbidden(
            "Only the installation owner can manage this resource.",
        ));
    }
    request.extensions_mut().insert(identity);
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
        // MCP grants, connection flows and per-agent GitHub tokens below.
        [
            "nodes" | "accounts" | "onepassword" | "mcps" | "tokens" | "oauth" | "connections"
            | "settings" | "audit" | "agent-avatars",
            ..,
        ] => true,
        ["agents"] | ["agents", _, "avatar"] => !read,
        ["agents", ..] => true,
        _ => false,
    }
}

pub struct Input {
    pub method: String,
    pub path: String,
    pub query: HashMap<String, String>,
    pub headers: HeaderMap,
    pub body: Value,
}

impl Input {
    pub async fn read(request: Request) -> Result<Self> {
        let (parts, body) = request.into_parts();
        let query = serde_urlencoded::from_str(parts.uri.query().unwrap_or(""))
            .map_err(|_| Error::bad("Invalid query parameters."))?;
        let limit = if parts.uri.path().starts_with("/internal/node-workspace/")
            && parts.uri.path().ends_with("/result")
        {
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

fn session_response(app: &App, session: &Value) -> Response {
    let secure = if app.service.config.public_url.starts_with("https:") {
        "; Secure"
    } else {
        ""
    };
    let cookie = format!(
        "leo_session={}; HttpOnly; SameSite=Lax; Path=/; Max-Age=604800{secure}",
        text(session, "value"),
    );
    let mut response = Json(json!({
        "authenticated": true,
        "csrf": session["csrf"]
    }))
    .into_response();
    response
        .headers_mut()
        .insert(header::SET_COOKIE, HeaderValue::from_str(&cookie).unwrap());
    response
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
        ["", "api", "public", "artifacts", token] => {
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

/// Sign-in routes, reachable without a session.
async fn session_route(app: &App, input: &Input) -> Result<Option<Response>> {
    let s = &app.service;
    let response = match (input.method.as_str(), input.path.as_str()) {
        ("GET", "/api/session") => {
            let session = s.auth.read(&cookie(&input.headers)).await?;
            let mut result = json!({
                "authenticated": session.is_some(),
                "setupRequired": s.store.kv("admin").await?.is_none(),
            });
            if let Some(session) = session {
                result["csrf"] = session["csrf"].clone();
            }
            Json(result).into_response()
        }
        ("POST", "/api/setup") => {
            let token = input.string("setupToken", 200)?;
            if s.config.setup_token.is_empty() || !safe_equal(token, &s.config.setup_token) {
                return Err(Error::forbidden("Incorrect setup token."));
            }
            s.auth.setup(input.string("password", 200)?).await?;
            s.store.audit("admin.setup", json!({})).await?;
            session_response(app, &s.auth.session().await?)
        }
        ("POST", "/api/login") => {
            let session = s.auth.login(input.string("password", 200)?).await?;
            session_response(app, &session)
        }
        ("POST", "/api/logout") => {
            s.auth.logout(&cookie(&input.headers)).await?;
            let mut response = Json(json!({ "ok": true })).into_response();
            response.headers_mut().insert(
                header::SET_COOKIE,
                HeaderValue::from_static("leo_session=; Path=/; Max-Age=0; HttpOnly; SameSite=Lax"),
            );
            response
        }
        _ => return Ok(None),
    };
    Ok(Some(response))
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
    if let Some(response) = session_route(&app, &input).await? {
        return Ok(response);
    }
    let s = &app.service;
    if let Some(response) = run_pages(s, &input).await? {
        return Ok(response);
    }
    Ok(Json(crate::api::dispatch(s, &input).await?).into_response())
}

/// Shown in the browser tab after the Android app captured an MCP OAuth callback.
fn native_callback_page() -> Response {
    let headers = [
        (header::CONTENT_TYPE, "text/html; charset=utf-8"),
        (header::CACHE_CONTROL, "no-store"),
        (header::REFERRER_POLICY, "no-referrer"),
        (
            header::CONTENT_SECURITY_POLICY,
            "default-src 'none'; frame-ancestors 'none'",
        ),
    ];
    let page = "<!doctype html><html lang=fr><meta name=viewport content='width=device-width,initial-scale=1'><title>Leo</title><h1>Revenez dans Leo</h1><p>Fermez cet onglet pour terminer la connexion dans l’application Android.</p></html>";
    (headers, page).into_response()
}

async fn oauth(State(app): State<App>, request: Request) -> Result<Response> {
    let input = Input::read(request).await?;
    let auth = &app.service.auth;
    if input.method == "GET" && input.path == "/oauth/mcp/callback" {
        if app
            .service
            .mcps
            .capture_native_callback(&app.service, &input.query)
            .await?
        {
            return Ok(native_callback_page());
        }
        let result = if let Some(session) = auth.read(&cookie(&input.headers)).await? {
            app.service
                .mcps
                .callback(&app.service, &input.query, text(&session, "csrf"))
                .await
                .unwrap_or_else(|_| "expired".into())
        } else {
            "expired".into()
        };
        let mut response =
            axum::response::Redirect::temporary(&format!("/mcps?oauth={result}")).into_response();
        response
            .headers_mut()
            .insert("referrer-policy", HeaderValue::from_static("no-referrer"));
        return Ok(response);
    }
    let result = match (input.method.as_str(), input.path.as_str()) {
        ("POST", "/oauth/register") => {
            return Ok(
                (StatusCode::CREATED, Json(auth.register(input.body).await?)).into_response(),
            );
        }
        ("GET", "/oauth/authorize") => {
            let parameters = serde_json::to_value(&input.query)?;
            auth.authorization(&parameters).await?;
            let location = format!(
                "/authorize?{}",
                serde_urlencoded::to_string(&input.query).map_err(Error::internal)?
            );
            return Ok(axum::response::Redirect::temporary(&location).into_response());
        }
        ("POST", "/oauth/token") => auth.exchange(input.body).await?,
        ("POST", "/oauth/revoke") => {
            auth.revoke_token(
                input.string("token", 10000)?,
                input.string("client_id", 200)?,
            )
            .await?;
            json!({})
        }
        _ => return Err(Error::not_found("Not found")),
    };
    Ok(Json(result).into_response())
}

async fn metadata(State(app): State<App>, request: Request) -> Result<Json<Value>> {
    if request.method() != "GET" {
        return Err(Error::method_not_allowed("Method not allowed."));
    }
    let url = &app.service.config.public_url;
    let scopes = ["read", "run", "manage"];
    match request.uri().path() {
        "/.well-known/oauth-protected-resource" | "/.well-known/oauth-protected-resource/mcp" => {
            Ok(Json(json!({
                "resource": format!("{url}/mcp"),
                "authorization_servers": [url],
                "scopes_supported": scopes,
                "bearer_methods_supported": ["header"],
                "resource_name": "Leo Agent Manager",
            })))
        }
        "/.well-known/oauth-authorization-server" => Ok(Json(json!({
            "issuer": url,
            "authorization_endpoint": format!("{url}/oauth/authorize"),
            "token_endpoint": format!("{url}/oauth/token"),
            "registration_endpoint": format!("{url}/oauth/register"),
            "revocation_endpoint": format!("{url}/oauth/revoke"),
            "response_types_supported": ["code"],
            "grant_types_supported": ["authorization_code", "refresh_token"],
            "code_challenge_methods_supported": ["S256"],
            "token_endpoint_auth_methods_supported": ["none"],
            "scopes_supported": scopes,
        }))),
        _ => Err(Error::not_found("Not found")),
    }
}
