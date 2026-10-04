//! Synthetic local browser sessions are confined to this example executable.
//! All application handlers and workers remain the real Rust implementation.
use axum::{
    Json, Router,
    extract::{Request, State},
    http::{HeaderValue, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
};
use leo_agent_manager::{
    auth::{InstallationIdentity, InstallationRole, safe_equal},
    error::{Error, Result},
    http::{Input, cookie},
    service::Service,
    validation::text,
};
use serde_json::{Value, json};
use std::sync::Arc;
use tower::ServiceExt;
use tower_http::services::{ServeDir, ServeFile};

pub async fn router(service: Arc<Service>) -> Result<Router> {
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
    let path = request.uri().path();
    if !path.starts_with("/api/")
        && !path.starts_with("/internal/")
        && !path.starts_with("/oauth/")
        && !path.starts_with("/.well-known/")
        && !path.starts_with("/mcp")
        && path != "/health"
    {
        return ServeDir::new("dist")
            .fallback(ServeFile::new("dist/index.html"))
            .oneshot(request)
            .await
            .unwrap()
            .into_response();
    }
    match authorize(&service, &mut request).await {
        Ok(Some(response)) => response,
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
                let session = service.auth.read(&cookie(&input.headers)).await?;
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
                service.auth.setup(input.string("password", 200)?).await?;
                signed_in(&service.auth.session().await?)
            }
            ("POST", "/api/login") => {
                signed_in(&service.auth.login(input.string("password", 200)?).await?)
            }
            ("POST", "/api/logout") => {
                let session = service
                    .auth
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
                service.auth.logout(&cookie(&input.headers)).await?;
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
        let session = service
            .auth
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
            .insert(InstallationIdentity::trusted(
                InstallationRole::Owner,
                text(&session, "csrf"),
            ));
    } else if !path.starts_with("/internal/")
        && path != "/health"
        && (path.starts_with("/oauth/")
            || path.starts_with("/.well-known/")
            || path == "/mcp"
            || path.starts_with("/api/public/"))
    {
        // Fixture-only adapters for the legacy OAuth/public-link journeys. The
        // production binary rejects these even with an old local credential.
        request
            .extensions_mut()
            .insert(InstallationIdentity::trusted(
                InstallationRole::Owner,
                "fixture-owner",
            ));
    }
    Ok(None)
}
