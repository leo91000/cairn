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
    super::config::fields(value, &["HostConfig"])?;
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
        let mut value = super::config::decode(&bytes)?;
        rewrite(&policy, &mut value).await?;
        *request.body_mut() = Body::from(serde_json::to_vec(&value)?);
        request.headers_mut().remove(header::CONTENT_LENGTH);
        request.headers_mut().remove(header::TRANSFER_ENCODING);
    }
    let method = request.method().clone();
    super::containers::pin_uri(request.uri_mut(), &method, Path::new(super::DATA))?;
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

#[derive(Clone)]
struct Gateway {
    policy: Policy,
    api_version: String,
}

async fn handler(State(gateway): State<Gateway>, request: Request) -> Response {
    // API negotiation is a client probe, not a reason to create a daemon. The
    // installer records this version from the checksum-pinned paired CLI.
    if request.uri().path() == "/_ping" && (request.method() == "GET" || request.method() == "HEAD")
    {
        return Response::builder()
            .header("API-Version", &gateway.api_version)
            .header("Docker-Experimental", "false")
            .header("OSType", "linux")
            .body(Body::from(if request.method() == "HEAD" {
                ""
            } else {
                "OK"
            }))
            .unwrap();
    }
    let policy = gateway.policy;
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
    let metadata: Value =
        serde_json::from_slice(&tokio::fs::read("/opt/cairn-docker/engine-api.json").await?)?;
    let api_version = metadata["apiVersion"]
        .as_str()
        .filter(|version| {
            version.split_once('.').is_some_and(|(major, minor)| {
                !major.is_empty()
                    && !minor.is_empty()
                    && major
                        .bytes()
                        .chain(minor.bytes())
                        .all(|byte| byte.is_ascii_digit())
            })
        })
        .ok_or_else(|| Error::unavailable("Managed Docker API metadata is invalid."))?
        .to_owned();
    let router = Router::new().fallback(handler).with_state(Gateway {
        policy,
        api_version,
    });
    tokio::spawn(async move {
        let _ = axum::serve(listener, router)
            .with_graceful_shutdown(stop.cancelled_owned())
            .await;
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[tokio::test]
    async fn noncanonical_container_controls_never_reach_mount_rewriting() {
        let policy = Policy {
            plan: Arc::new(serde_json::json!({})),
        };
        let mut value = serde_json::json!({"hostConfig": {"Binds": ["/:/host"]}});
        assert!(rewrite(&policy, &mut value).await.is_err());
        assert!(
            super::super::config::decode(
                br#"{"HostConfig":{"Binds":["/:/host"]},"HostConfig":{}}"#
            )
            .is_err()
        );
    }

    #[tokio::test]
    async fn client_negotiation_does_not_need_a_start_socket_or_daemon() {
        for method in ["GET", "HEAD"] {
            let gateway = Gateway {
                policy: Policy {
                    plan: Arc::new(serde_json::json!({})),
                },
                api_version: "1.55".into(),
            };
            let request = Request::builder()
                .method(method)
                .uri("/_ping")
                .body(Body::empty())
                .unwrap();
            let response = handler(State(gateway), request).await;
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(response.headers()["API-Version"], "1.55");
        }
    }
}
