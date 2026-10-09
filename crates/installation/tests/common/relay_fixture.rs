//! Test transport for installation handlers. This is a synthetic authenticated
//! relay seam, never a browser login, OAuth authority or application web host.
//! The browser journeys use the real official and installation binaries.
use axum::{
    Json, Router,
    extract::{Request, State},
    http::{HeaderMap, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{any, get},
};
use cairn_installation::{
    auth::{InstallationIdentity, InstallationRole, digest, token},
    error::{Error, Result},
    service::Service,
    validation::text,
};
use cairn_protocol::{TaskAuthorGrant, TaskAuthorPolicy};
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::io::AsyncWriteExt;
use tower::ServiceExt;

pub async fn context(service: &Service) -> Value {
    let value = token();
    service
        .store
        .set(&format!("test-relay:{}", digest(&value)), json!({}), None)
        .await
        .unwrap();
    json!({ "value": value })
}

pub async fn grant(service: &Service, scopes: &[&str]) -> String {
    let value = token();
    service
        .store
        .set(
            &format!("test-relay:{}", digest(&value)),
            json!({ "scopes": scopes }),
            None,
        )
        .await
        .unwrap();
    value
}

pub async fn router(service: Arc<Service>) -> Result<Router> {
    claimed(&service).await?;
    installation_router(service).await
}

async fn installation_router(service: Arc<Service>) -> Result<Router> {
    let installation = cairn_installation::http::router(service.clone()).await?;
    let mcp = installation.clone();
    // The synthetic authority advertises /mcp, just like the official authority.
    // Dispatch its verified request through the real installation MCP route.
    Ok(installation
        .route(
            "/mcp",
            any(move |request: Request| {
                let installation = mcp.clone();
                async move { installation.oneshot(request).await.unwrap() }
            }),
        )
        .layer(middleware::from_fn_with_state(service, transport)))
}

async fn transport(
    State(service): State<Arc<Service>>,
    mut request: Request,
    next: Next,
) -> Response {
    let path = request.uri().path().to_owned();
    let bearer = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "));
    let supplied = request
        .headers()
        .get("x-test-relay-token")
        .and_then(|value| value.to_str().ok())
        .or(if path == "/mcp" { bearer } else { None });
    if let Some(value) = supplied
        && request.extensions().get::<InstallationIdentity>().is_none()
    {
        match service
            .store
            .kv(&format!("test-relay:{}", digest(value)))
            .await
        {
            Ok(Some(context)) => {
                let mut identity =
                    InstallationIdentity::trusted(InstallationRole::Owner, "fixture-owner");
                if path == "/mcp" {
                    identity.mcp_scopes = serde_json::from_value(context["scopes"].clone()).ok();
                    *request.uri_mut() = "/api/mcp".parse().unwrap();
                }
                request.extensions_mut().insert(identity);
            }
            Ok(None) => {}
            Err(error) => return error.into_response(),
        }
    }
    // Test recipients use the same public capability as the official relay.
    if path.starts_with("/api/public/installations/") {
        let value = path.rsplit('/').next().unwrap();
        let mut identity = InstallationIdentity::trusted(InstallationRole::Member, "");
        identity.public_artifact = Some(value.into());
        request.extensions_mut().insert(identity);
        request.headers_mut().remove(header::ORIGIN);
        *request.uri_mut() = format!("/api/shared-artifacts/{value}").parse().unwrap();
    }
    next.run(request).await
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

/// Synthetic authority for installation-only worker/handler tests.
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
    // Handler tests keep their synthetic owner, but exercise the real
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
