//! One provider callback per Beacon, independent of installation IDs and slugs.
//! State selects only a relay destination; it never grants access to that host.
use crate::{ApiError, Service, installations, relay};
use axum::{
    body::{Body, to_bytes},
    extract::{Path, Request, State},
    http::StatusCode,
    response::Response,
};
use serde_json::Value;
use std::time::Duration;

pub(super) async fn callback(
    State(service): State<Service>,
    request: Request,
) -> Result<Response, ApiError> {
    if !service.hosts.central(request.headers()) {
        return Err(ApiError::Http(
            StatusCode::NOT_FOUND,
            "Use the central Cairn account origin",
        ));
    }
    // Authenticate and check CSRF before reading the provider response. A state
    // copied from a different account must never authorize a relay request.
    let account = installations::account(&service, request.headers(), request.method()).await?;
    let (mut parts, body) = request.into_parts();
    let bytes = tokio::time::timeout(Duration::from_secs(10), to_bytes(body, 16_384))
        .await
        .map_err(|_| ApiError::Http(StatusCode::REQUEST_TIMEOUT, "Sign-in response timed out"))?
        .map_err(|_| {
            ApiError::Http(
                StatusCode::PAYLOAD_TOO_LARGE,
                "Sign-in response is too large",
            )
        })?;
    let invalid = || ApiError::Http(StatusCode::BAD_REQUEST, "Invalid plugin sign-in state");
    let parameters: Value = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
    let state = parameters["state"].as_str().ok_or_else(invalid)?;
    let mut fields = state.split(':');
    let (Some("plugin"), Some(installation), Some(nonce), None) =
        (fields.next(), fields.next(), fields.next(), fields.next())
    else {
        return Err(invalid());
    };
    let id = uuid::Uuid::parse_str(installation).map_err(|_| invalid())?;
    if id.to_string() != installation
        || nonce.len() != 43
        || !nonce
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(invalid());
    }
    if installations::role(&service, installation, &account).await? != cairn_protocol::Role::Owner {
        return Err(ApiError::Http(
            StatusCode::FORBIDDEN,
            "Only the installation owner can connect a plugin account",
        ));
    }

    // Reuse the ordinary relay, including its access-generation/revocation
    // checks. No provider credentials or pending flows are stored on Beacon.
    parts.uri = format!("/api/installations/{installation}/api/plugins/oauth/callback")
        .parse()
        .map_err(|_| invalid())?;
    relay::forward(
        State(service),
        Path((installation.to_owned(), "plugins/oauth/callback".to_owned())),
        Request::from_parts(parts, Body::from(bytes)),
    )
    .await
}
