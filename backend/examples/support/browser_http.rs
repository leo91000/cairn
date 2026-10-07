//! Synthetic local browser sessions are confined to this example executable.
//! All application handlers and workers remain the real Rust implementation.
pub mod legacy_auth;
mod legacy_http;

pub fn auth(service: &Service) -> legacy_auth::Auth {
    legacy_auth::Auth::new(service.store.clone(), service.config.public_url.clone())
}

use axum::{
    Json, Router,
    extract::{Request, State},
    http::{HeaderMap, HeaderValue, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::get,
};
use leo_agent_manager::{
    auth::{InstallationIdentity, InstallationRole, safe_equal},
    error::{Error, Result},
    http::{Input, cookie},
    service::Service,
    validation::text,
};
use leo_relay_protocol::{TaskAuthorGrant, TaskAuthorPolicy};
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::io::AsyncWriteExt;
use tower::ServiceExt;
use tower_http::services::{ServeDir, ServeFile};

pub async fn router(service: Arc<Service>) -> Result<Router> {
    claimed(&service).await?;
    installation_router(service).await
}

async fn installation_router(service: Arc<Service>) -> Result<Router> {
    Ok(leo_agent_manager::http::router(service.clone())
        .await?
        .layer(middleware::from_fn_with_state(service, authenticate)))
}

fn signed_in(session: &Value) -> Response {
    let mut response = Json(json!({
        "authenticated": true,
        "csrf": session["csrf"],
    }))
    .into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        HeaderValue::from_str(&format!(
            "leo_session={}; HttpOnly; SameSite=Lax; Path=/; Max-Age=604800",
            text(session, "value"),
        ))
        .unwrap(),
    );
    response
}

async fn authenticate(
    State(service): State<Arc<Service>>,
    mut request: Request,
    next: Next,
) -> Response {
    if request.uri().path().starts_with("/installations/")
        && request.uri().path().ends_with("/mcps/callback")
    {
        let query = request.uri().query().unwrap_or("");
        *request.uri_mut() = format!("/oauth/mcp/callback?{query}").parse().unwrap();
    }

    let path = request.uri().path();
    if !path.starts_with("/api/")
        && !path.starts_with("/internal/")
        && !path.starts_with("/oauth/")
        && !path.starts_with("/.well-known/")
        && path != "/mcp"
        && path != "/mcp-workspace"
        && !path.starts_with("/mcp-gateway/")
        && path != "/health"
    {
        return ServeDir::new("dist")
            .fallback(ServeFile::new("dist/index.html"))
            .oneshot(request)
            .await
            .unwrap()
            .into_response();
    }

    let legacy_management_mcp = path == "/mcp";

    match legacy_http::handle(&service, &mut request).await {
        Ok(Some(response)) => return response,
        Ok(None) => {}
        Err(error) => return error.into_response(),
    }
    match authorize(&service, &mut request).await {
        Ok(Some(response)) => response,
        Ok(None) if legacy_management_mcp => match leo_agent_manager::http::router(service).await {
            Ok(app) => app.oneshot(request).await.unwrap(),
            Err(error) => error.into_response(),
        },
        Ok(None) => next.run(request).await,
        Err(error) => error.into_response(),
    }
}

async fn authorize(service: &Service, request: &mut Request) -> Result<Option<Response>> {
    let path = request.uri().path().to_owned();
    if ["/api/session", "/api/setup", "/api/login", "/api/logout"].contains(&path.as_str()) {
        // Swap the body only for the synthetic fixture's sign-in endpoints.
        let owned = std::mem::replace(request, Request::new(axum::body::Body::empty()));
        let input = Input::read(owned).await?;
        let response = match (input.method.as_str(), path.as_str()) {
            ("GET", "/api/session") => {
                let session = auth(service).read(&cookie(&input.headers)).await?;
                Json(json!({
                    "authenticated": session.is_some(),
                    "csrf": session.as_ref().map(|value| value["csrf"].clone()),
                    "setupRequired": service.store.kv("admin").await?.is_none(),
                }))
                .into_response()
            }
            ("POST", "/api/setup") => {
                if input.string("setupToken", 200)? != "browser-test-setup" {
                    return Err(Error::forbidden("Incorrect fixture setup token."));
                }
                auth(service).setup(input.string("password", 200)?).await?;
                signed_in(&auth(service).session().await?)
            }
            ("POST", "/api/login") => {
                signed_in(&auth(service).login(input.string("password", 200)?).await?)
            }
            ("POST", "/api/logout") => {
                let session = auth(service)
                    .read(&cookie(&input.headers))
                    .await?
                    .ok_or_else(|| Error::unauthorized("Please sign in."))?;
                if input
                    .headers
                    .get("x-csrf-token")
                    .and_then(|v| v.to_str().ok())
                    != session["csrf"].as_str()
                {
                    return Err(Error::forbidden("Invalid CSRF token."));
                }
                auth(service).logout(&cookie(&input.headers)).await?;
                let mut response = Json(json!({ "ok": true })).into_response();
                response.headers_mut().insert(
                    header::SET_COOKIE,
                    HeaderValue::from_static(
                        "leo_session=; Path=/; Max-Age=0; HttpOnly; SameSite=Lax",
                    ),
                );
                response
            }
            _ => return Err(Error::not_found("Not found")),
        };
        return Ok(Some(response));
    }
    if request.extensions().get::<InstallationIdentity>().is_some() {
        return Ok(None);
    }
    if path.starts_with("/api/")
        && !leo_agent_manager::artifacts::sharing::public_read(&path, request.method().as_str())
    {
        let session = auth(service)
            .read(&cookie(request.headers()))
            .await?
            .ok_or_else(|| Error::unauthorized("Please sign in."))?;
        if !["GET", "HEAD", "OPTIONS"].contains(&request.method().as_str()) {
            let supplied = request
                .headers()
                .get("x-csrf-token")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("");
            if !safe_equal(supplied, text(&session, "csrf")) {
                return Err(Error::forbidden("Invalid CSRF token."));
            }
        }
        request
            .extensions_mut()
            .insert(fixture_identity(text(&session, "csrf")));
    } else if !path.starts_with("/internal/")
        && path != "/health"
        && (path.starts_with("/oauth/")
            || path.starts_with("/.well-known/")
            || path == "/mcp"
            || path.starts_with("/api/public/"))
    {
        // Fixture-only adapters for the legacy OAuth/public-link journeys. The
        // production binary rejects these even with an old local credential.
        let session = auth(service).read(&cookie(request.headers())).await?;
        let account_id = session
            .as_ref()
            .map_or("fixture-owner", |session| text(session, "csrf"));
        request
            .extensions_mut()
            .insert(fixture_identity(account_id));
    }
    Ok(None)
}

fn fixture_identity(binding: &str) -> InstallationIdentity {
    let mut identity = InstallationIdentity::trusted(InstallationRole::Owner, binding);
    identity.account_id = "fixture-owner".to_owned();
    identity
}

async fn fixture_task_authors(headers: HeaderMap) -> Result<Json<TaskAuthorPolicy>> {
    if headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        != Some("Bearer fixture-only")
    {
        return Err(Error::unauthorized("Incorrect fixture installation token."));
    }

    Ok(Json(TaskAuthorPolicy {
        owner: TaskAuthorGrant {
            account_id: "fixture-owner".to_owned(),
            access_id: "fixture-owner".to_owned(),
        },
        members: Vec::new(),
    }))
}

/// Synthetic identity for historical installation-only browser fixtures.
pub async fn claimed(service: &Service) -> Result<()> {
    let directory = service.config.data_dir.join("installation-relay");
    tokio::fs::create_dir_all(&directory).await?;
    let path = directory.join("identity.json");
    let previous_origin = if path.exists() {
        if service.synchronize_task_authors().await.is_ok() {
            return Ok(());
        }
        let previous: Value = serde_json::from_slice(&tokio::fs::read(&path).await?)?;
        Some(text(&previous, "origin").to_owned())
    } else {
        None
    };
    // Rebind the same fixture origin after a process restart so existing links
    // remain readable. Another router in the same runtime reuses the live server.
    let port = previous_origin
        .as_ref()
        .and_then(|origin| url::Url::parse(origin).ok())
        .and_then(|origin| origin.port())
        .unwrap_or(0);
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await?;
    let origin = format!("http://{}", listener.local_addr()?);
    // Historical journeys keep their synthetic owner, but exercise the real
    // installation handlers and the same HTTP policy lookup as production.
    let app = installation_router(Arc::new(service.clone())).await?.route(
        "/api/relay/00000000-0000-4000-8000-000000000055/task-authors",
        get(fixture_task_authors),
    );
    let shutdown = service.shutdown.clone();

    let identity = serde_json::to_vec(&json!({
        "origin": origin,
        "installationId": "00000000-0000-4000-8000-000000000055",
        "token": "fixture-only",
    }))?;

    let mut file = tokio::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .await?;
    file.write_all(&identity).await?;
    file.flush().await?;

    tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(shutdown.cancelled_owned())
            .await
            .unwrap();
    });
    Ok(())
}
