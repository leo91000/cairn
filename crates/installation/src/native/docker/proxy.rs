//! Streaming Docker API gateway. JSON bind sources become descriptor-pinned
//! private mirrors; builds, downloads and upgraded exec/Buildx streams stay live.
use super::{Policy, RUNTIME, mounts};
use crate::error::{Error, Result};
use axum::{
    Router,
    body::Body,
    extract::{Request, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};
use hyper_util::rt::TokioIo;
use serde_json::Value;
use std::{os::unix::fs::PermissionsExt, path::Path, time::Duration};
use tokio_util::sync::CancellationToken;

const PRIVATE_SOCKET: &str = "/run/cairn-docker/private/docker.sock";

async fn rewrite(policy: &Policy, value: &mut Value) -> Result<()> {
    if !value.is_object() {
        return Err(Error::bad(
            "Docker container configuration must be an object.",
        ));
    }
    let host = &mut value["HostConfig"];
    // Validate all non-mount controls before creating a persistent mirror.
    policy.host_config(host)?;
    if let Some(binds) = host["Binds"].as_array_mut() {
        for value in binds {
            let parts: Vec<_> = value
                .as_str()
                .ok_or_else(|| Error::forbidden("Invalid Docker bind."))?
                .split(':')
                .map(String::from)
                .collect();
            if parts[0].starts_with('/') {
                let options = parts.get(2).map_or("rw", String::as_str);
                let read_only = options.split(',').any(|option| option == "ro");
                let source = mounts::pin(policy, &parts[0], read_only).await?;
                *value = format!("{source}:{}:{options}", parts[1]).into();
            }
        }
    }
    if let Some(values) = host["Mounts"].as_array_mut() {
        for value in values {
            if value["Type"] == "bind" {
                let source = value["Source"]
                    .as_str()
                    .ok_or_else(|| Error::forbidden("Invalid Docker bind source."))?;
                value["Source"] = mounts::pin(policy, source, value["ReadOnly"] == true)
                    .await?
                    .into();
            }
        }
    }
    Ok(())
}

async fn forward(policy: Policy, mut request: Request) -> Result<Response> {
    super::request(b"start").await?;
    let create = request.method() == "POST" && request.uri().path().ends_with("/containers/create");
    if create {
        let bytes = axum::body::to_bytes(
            std::mem::replace(request.body_mut(), Body::empty()),
            16 * 1024 * 1024,
        )
        .await
        .map_err(|_| Error::bad("Docker container configuration exceeds its limit."))?;
        let mut value: Value = serde_json::from_slice(&bytes)?;
        rewrite(&policy, &mut value).await?;
        *request.body_mut() = Body::from(serde_json::to_vec(&value)?);
        request.headers_mut().remove(header::CONTENT_LENGTH);
        request.headers_mut().remove(header::TRANSFER_ENCODING);
    }
    // Capture the frontend's upgrade before transferring the HTTP request.
    let upgrade = hyper::upgrade::on(&mut request);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    let stream = loop {
        match tokio::net::UnixStream::connect(PRIVATE_SOCKET).await {
            Ok(stream) => break stream,
            Err(error) if tokio::time::Instant::now() < deadline => {
                let _ = error;
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            Err(error) => return Err(error.into()),
        }
    };
    let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
        .await
        .map_err(Error::internal)?;
    tokio::spawn(async move {
        let _ = connection.with_upgrades().await;
    });
    let mut response = sender
        .send_request(request)
        .await
        .map_err(Error::internal)?;
    if response.status() == StatusCode::SWITCHING_PROTOCOLS {
        let backend = hyper::upgrade::on(&mut response);
        tokio::spawn(async move {
            if let (Ok(frontend), Ok(backend)) = tokio::join!(upgrade, backend) {
                let mut frontend = TokioIo::new(frontend);
                let mut backend = TokioIo::new(backend);
                let _ = tokio::io::copy_bidirectional(&mut frontend, &mut backend).await;
            }
        });
    }
    Ok(response.map(Body::new))
}

async fn handler(State(policy): State<Policy>, request: Request) -> Response {
    match forward(policy, request).await {
        Ok(response) => response,
        Err(error) => (
            StatusCode::from_u16(error.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            axum::Json(serde_json::json!({ "message": error.message })),
        )
            .into_response(),
    }
}

pub(super) async fn serve(policy: Policy, stop: CancellationToken) -> Result<()> {
    let socket = Path::new(RUNTIME).join("docker.sock");
    let listener = tokio::net::UnixListener::bind(&socket)?;
    std::os::unix::fs::chown(&socket, None, Some(1000))?;
    tokio::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o660)).await?;
    let router = Router::new().fallback(handler).with_state(policy);
    tokio::spawn(async move {
        let _ = axum::serve(listener, router)
            .with_graceful_shutdown(stop.cancelled_owned())
            .await;
    });
    Ok(())
}
