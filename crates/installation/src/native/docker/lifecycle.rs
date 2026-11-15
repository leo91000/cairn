//! Graceful container shutdown precedes the mandatory systemd cgroup fence.
use crate::{
    error::{Error, Result},
    validation::{text, uuid},
};
use futures_util::{StreamExt, stream};
use serde_json::Value;
use std::{path::Path, time::Duration};

async fn stop_socket(socket: &Path) -> Result<()> {
    let client = crate::native::controller_client(socket)?;
    let mut response = client
        .get("http://localhost/containers/json?all=true")
        .send()
        .await
        .map_err(Error::internal)?;
    if !response.status().is_success() {
        return Err(Error::unavailable(
            "Unable to list native Docker containers before stopping.",
        ));
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(Error::internal)? {
        if bytes.len() + chunk.len() > 1_000_000 {
            return Err(Error::unavailable(
                "Native Docker container inventory exceeds its limit.",
            ));
        }
        bytes.extend_from_slice(&chunk);
    }
    let containers: Vec<Value> = serde_json::from_slice(&bytes)?;
    let results: Vec<_> = stream::iter(containers)
        .map(|container| {
            let client = client.clone();
            async move {
                let id = text(&container, "Id");
                if !super::containers::canonical(id) {
                    return Err(Error::unavailable(
                        "Invalid native Docker container identity.",
                    ));
                }
                if matches!(text(&container, "State"), "created" | "exited" | "dead") {
                    return Ok(());
                }
                if text(&container, "State") == "paused" {
                    let response = client
                        .post(format!("http://localhost/containers/{id}/unpause"))
                        .send()
                        .await
                        .map_err(Error::internal)?;
                    if !response.status().is_success()
                        && response.status() != reqwest::StatusCode::NOT_MODIFIED
                    {
                        return Err(Error::unavailable(format!(
                            "Native Docker container {id} could not be unpaused for shutdown."
                        )));
                    }
                }
                let response = client
                    .post(format!("http://localhost/containers/{id}/stop?t=1"))
                    .send()
                    .await
                    .map_err(Error::internal)?;
                if !response.status().is_success()
                    && response.status() != reqwest::StatusCode::NOT_FOUND
                    && response.status() != reqwest::StatusCode::NOT_MODIFIED
                {
                    return Err(Error::unavailable(format!(
                        "Native Docker container {id} did not stop cleanly."
                    )));
                }
                Ok(())
            }
        })
        .buffer_unordered(32)
        .collect()
        .await;
    for result in results {
        result?;
    }
    Ok(())
}

/// Failure here never substitutes for the positive systemd fence. Do not start
/// Docker just to stop it, and do not let a broken socket block lease fencing.
pub(crate) async fn stop_attempt(state: &Path, attempt: &str) -> Result<()> {
    uuid(attempt)?;
    let run = match tokio::fs::read_to_string(state.join(format!("{attempt}.active"))).await {
        Ok(run) => run,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    uuid(&run)?;
    let runtime = super::super::workspace::directory(state, &run)?.join("docker-runtime/runtime");
    stop_runtime(&runtime).await
}

async fn stop_runtime(runtime: &Path) -> Result<()> {
    tokio::time::timeout(Duration::from_secs(3), async {
        let private = runtime.join("private/docker.sock");
        let socket = if tokio::fs::try_exists(&private).await? {
            private
        } else if tokio::fs::try_exists(runtime.join("start.sock")).await? {
            // The rootless gateway starts dockerd for every forwarded request.
            // It must never be used to stop a daemon which has not started.
            return Ok(());
        } else if tokio::fs::try_exists(runtime.join("daemon.started")).await? {
            runtime.join("docker.sock")
        } else {
            return Ok(());
        };
        if tokio::fs::try_exists(&socket).await? {
            stop_socket(&socket).await?;
        }
        Ok(())
    })
    .await
    .map_err(|_| {
        Error::timeout(
            "Native Docker graceful shutdown timed out; cgroup fencing is still required.",
        )
    })?
}

/// systemd ExecStop runs before killing descendants, including when the engine
/// exits normally. The '!' command prefix supplies host root without removing mount bindings for both stores.
pub async fn stop_current() -> Result<()> {
    stop_runtime(Path::new(super::RUNTIME)).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Router,
        routing::{get, post},
    };
    use std::sync::Arc;
    use tokio::sync::Mutex;

    #[tokio::test]
    async fn stopping_a_lazy_rootless_gateway_never_starts_the_daemon() {
        let root = tempfile::tempdir().unwrap();
        tokio::fs::write(root.path().join("start.sock"), b"gateway fixture")
            .await
            .unwrap();
        let listener = tokio::net::UnixListener::bind(root.path().join("docker.sock")).unwrap();
        stop_runtime(root.path()).await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(30), listener.accept())
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn graceful_shutdown_stops_the_running_inventory_before_returning() {
        let root = tempfile::tempdir().unwrap();
        let socket = root.path().join("docker.sock");
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        let stopped = Arc::new(Mutex::new(Vec::new()));
        let ids = ["a".repeat(64), "b".repeat(64)];
        let inventory = serde_json::json!([{ "Id": ids[0], "State": "running" }, { "Id": ids[1], "State": "paused" }]);
        let unpaused = Arc::new(Mutex::new(Vec::new()));
        let router = Router::new()
            .route(
                "/containers/json",
                get(move || async move { axum::Json(inventory) }),
            )
            .route(
                "/containers/{id}/unpause",
                post({
                    let unpaused = unpaused.clone();
                    move |axum::extract::Path(id): axum::extract::Path<String>| {
                        let unpaused = unpaused.clone();
                        async move {
                            unpaused.lock().await.push(id);
                            axum::http::StatusCode::NO_CONTENT
                        }
                    }
                }),
            )
            .route(
                "/containers/{id}/stop",
                post({
                    let stopped = stopped.clone();
                    move |axum::extract::Path(id): axum::extract::Path<String>,
                          axum::extract::Query(query): axum::extract::Query<
                        std::collections::HashMap<String, String>,
                    >| {
                        let stopped = stopped.clone();
                        async move {
                            assert_eq!(query["t"], "1");
                            stopped.lock().await.push(id);
                            axum::http::StatusCode::NO_CONTENT
                        }
                    }
                }),
            );
        let server = tokio::spawn(async move { axum::serve(listener, router).await });
        stop_socket(&socket).await.unwrap();
        let mut actual = stopped.lock().await.clone();
        actual.sort();
        assert_eq!(actual, ids);
        assert_eq!(*unpaused.lock().await, vec!["b".repeat(64)]);
        server.abort();
    }
}
