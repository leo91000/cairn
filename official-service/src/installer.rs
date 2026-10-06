use axum::{
    Json, Router,
    http::{StatusCode, header},
    response::IntoResponse,
    routing::get,
};
use serde_json::json;
use sha2::{Digest, Sha256};

/// Public installer assets contain no credentials; the launcher supplies the claim code.
pub fn router(origin: String) -> Router {
    let script = include_str!("../../deploy/installations/install.sh")
        .replace("__LEO_OFFICIAL_ORIGIN__", &shell_quote(&origin))
        .replace(
            "__LEO_HOST_SHA256__",
            &hex::encode(Sha256::digest(include_bytes!(
                "../../deploy/installations/host.py"
            ))),
        )
        .replace(
            "__LEO_NODE_HOST_SHA256__",
            &hex::encode(Sha256::digest(include_bytes!("../../deploy/nodes/host.py"))),
        );
    Router::new()
        .route(
            "/install.sh",
            get(move || async move {
                (
                    [
                        (header::CONTENT_TYPE, "text/x-shellscript; charset=utf-8"),
                        (header::CACHE_CONTROL, "no-store"),
                    ],
                    script,
                )
            }),
        )
        .route(
            "/install/node-host.py",
            get(|| async {
                (
                    [
                        (header::CONTENT_TYPE, "text/x-python; charset=utf-8"),
                        (header::CACHE_CONTROL, "no-store"),
                    ],
                    include_str!("../../deploy/nodes/host.py"),
                )
            }),
        )
        .route(
            "/install/host.py",
            get(|| async {
                (
                    [
                        (header::CONTENT_TYPE, "text/x-python; charset=utf-8"),
                        (header::CACHE_CONTROL, "no-store"),
                    ],
                    include_str!("../../deploy/installations/host.py"),
                )
            }),
        )
}

/// The operator explicitly selects an immutable, tested image; never follow `latest`.
pub fn release_router(image: Option<String>) -> Result<Router, &'static str> {
    if let Some(image) = &image {
        let digest = image
            .strip_prefix("ghcr.io/leo91000/leo-agent-manager@sha256:")
            .ok_or("LEO_INSTALLATION_IMAGE must be an immutable Leo image digest")?;
        if digest.len() != 64
            || !digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err("LEO_INSTALLATION_IMAGE must be an immutable Leo image digest");
        }
    }
    Ok(Router::new().route(
        "/install/release",
        get(move || async move {
            let headers = [(header::CACHE_CONTROL, "no-store")];
            match image {
                Some(image) => (
                    headers,
                    Json(json!({
                        "image": image
                    })),
                )
                    .into_response(),
                None => (
                    StatusCode::SERVICE_UNAVAILABLE,
                    headers,
                    Json(json!({
                        "error": "The operator must configure LEO_INSTALLATION_IMAGE before installing Leo."
                    })),
                )
                    .into_response(),
            }
        }),
    ))
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}
