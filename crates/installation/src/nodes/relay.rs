//! Node-side HTTP relay. The master can address only the private VM controller.
use super::executor::Executor;
use crate::{
    error::{Error, Result},
    validation::text,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use futures_util::StreamExt;
use serde::Serialize;
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use tokio_util::sync::CancellationToken;

const MAX_CONCURRENT_COMMANDS: usize = 64;
const FRAME_BYTES: usize = 65536;
const MAX_PAYLOAD_BYTES: usize = 2_000_000;

/// The response head, sent before any body frame.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Head<'a> {
    id: &'a str,
    sequence: u64,
    status: u16,
    length: Option<u64>,
    content_type: &'a str,
}

#[derive(Serialize)]
struct Chunk<'a> {
    id: &'a str,
    sequence: u64,
    data: String,
}

/// The final frame of a response. A complete response without a head carries its status.
#[derive(Serialize)]
struct End<'a> {
    id: &'a str,
    sequence: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    status: Option<u16>,
    done: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<String>,
}

impl<'a> End<'a> {
    const fn with_status(id: &'a str, status: u16) -> Self {
        Self {
            id,
            sequence: 0,
            status: Some(status),
            done: true,
            data: None,
        }
    }
}

/// The master connection a relayed command answers through.
struct Master<'a> {
    client: &'a reqwest::Client,
    controller: &'a reqwest::Client,
    url: &'a url::Url,
    token: &'a str,
}

impl Master<'_> {
    async fn send(&self, frame: &impl Serialize) -> Result<()> {
        let response = self
            .client
            .post(
                self.url
                    .join("internal/nodes/reply")
                    .map_err(Error::internal)?,
            )
            .bearer_auth(self.token)
            .json(frame)
            .timeout(Duration::from_secs(25))
            .send()
            .await
            .map_err(|_| Error::unavailable("Master connection interrupted."))?;
        if !response.status().is_success() {
            return Err(Error::conflict("Execution response rejected."));
        }
        Ok(())
    }
}

fn validate_runner(runner: &url::Url) -> Result<()> {
    let loopback = runner.host_str().is_some_and(|h| {
        h == "localhost"
            || h.parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.is_loopback())
    });
    if runner.scheme() != "http"
        || !loopback
        || !runner.username().is_empty()
        || runner.password().is_some()
        || runner.path() != "/"
        || runner.query().is_some()
        || runner.fragment().is_some()
    {
        return Err(Error::bad(
            "The node relay requires a loopback HTTP VM controller.",
        ));
    }
    Ok(())
}

pub async fn run(
    master: url::Url,
    token: String,
    runner: String,
    runner_token: String,
    stop: CancellationToken,
) -> Result<()> {
    let runner = url::Url::parse(&runner).map_err(|_| Error::bad("Invalid local runner URL."))?;
    validate_runner(&runner)?;
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(Error::internal)?;
    let controller = super::local_client()?;
    let permits = Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT_COMMANDS));
    let mut tasks = tokio::task::JoinSet::new();
    let executor = Arc::new(Executor::default());
    let result = loop {
        let poll = client
            .post(
                master
                    .join("internal/nodes/poll")
                    .map_err(Error::internal)?,
            )
            .bearer_auth(&token)
            .json(&json!({}))
            .timeout(Duration::from_secs(25))
            .send();
        let response = tokio::select! {
            () = stop.cancelled() => break Ok(()),
            result = poll => result,
        };
        let command = match response {
            Ok(response) if response.status() == 401 => {
                break Err(Error::unauthorized("Node identity revoked."));
            }
            Ok(response) if response.status().is_success() => response.json::<Value>().await.ok(),
            _ => None,
        };
        while tasks.try_join_next().is_some() {}
        let Some(command) = command.filter(Value::is_object) else {
            tokio::select! {
                () = stop.cancelled() => break Ok(()),
                () = tokio::time::sleep(Duration::from_millis(250)) => {}
            }
            continue;
        };
        let permit = permits
            .clone()
            .acquire_owned()
            .await
            .map_err(Error::internal)?;
        let (http, master, token, runner, runner_token) = (
            client.clone(),
            master.clone(),
            token.clone(),
            runner.clone(),
            runner_token.clone(),
        );
        let controller = controller.clone();
        let executor = executor.clone();
        tasks.spawn(async move {
            let _permit = permit;
            let master = Master {
                client: &http,
                controller: &controller,
                url: &master,
                token: &token,
            };
            // The master fails the call itself when its reply never completes.
            let _ = forward(&master, &runner, &runner_token, &command, &executor).await;
        });
    };
    tasks.abort_all();
    while tasks.join_next().await.is_some() {}
    for attempt in executor.close().await {
        // Best effort: the controller also stops VMs whose lease expires.
        let _ = controller
            .delete(
                runner
                    .join(&format!("runs/{attempt}"))
                    .map_err(Error::internal)?,
            )
            .bearer_auth(&runner_token)
            .timeout(Duration::from_secs(17))
            .send()
            .await;
    }
    result
}

fn route(method: &str, path: &str) -> Result<()> {
    let parts = path.trim_start_matches('/').split('/').collect::<Vec<_>>();
    let uuid = |id: &str| crate::validation::uuid(id).is_ok();
    let allowed = match parts.as_slice() {
        ["health"] => method == "GET",
        ["exclusive"] => method == "GET",
        ["exclusive", "acquire" | "release"] => method == "POST",
        ["snapshots", id, hash] => {
            uuid(id)
                && ((method == "GET" && super::snapshots::valid_hash(hash))
                    || (method == "POST"
                        && matches!(
                            *hash,
                            "blocks" | "publication" | "direct-upload" | "direct-reader"
                        ))
                    || (method == "DELETE" && matches!(*hash, "discard" | "direct-reader")))
        }
        ["runs", id] => uuid(id) && ["POST", "DELETE"].contains(&method),
        ["runs", id, operation] => {
            uuid(id)
                && matches!(
                    (method, *operation),
                    ("GET", "logs" | "administrative-pause")
                        | ("POST", "wait")
                        | ("POST", "artifact")
                        | ("POST", "snapshot")
                )
        }
        ["runs", id, "projects", project] => method == "POST" && uuid(id) && uuid(project),
        ["storage-policy"] | ["storage-policy", "probe"] => method == "POST",
        ["disks", id, operation] => {
            method == "POST"
                && uuid(id)
                && [
                    "delete",
                    "prune",
                    "restore",
                    "snapshot",
                    "snapshot-completed",
                    "published",
                    "publication-fence",
                    "storage-status",
                    "evict",
                ]
                .contains(operation)
        }
        _ => false,
    };
    if !allowed {
        return Err(Error::bad("Unsupported execution operation."));
    }
    Ok(())
}

/// How long the controller may take to answer, by operation.
fn controller_timeout(method: &str, path: &str, native: bool) -> Duration {
    if method == "POST" && path.ends_with("/direct-reader") {
        return super::snapshots::DIRECT_PUBLICATION_STARTUP;
    }
    Duration::from_secs(if method == "POST" && path.ends_with("/direct-upload") {
        crate::storage::direct::TRANSFER_SECONDS + 60
    } else if method == "POST" && path.ends_with("/publication") {
        super::snapshots::PUBLICATION_STARTUP.as_secs()
    } else if path.ends_with("/restore") {
        120
    } else if method == "POST" && matches!(path, "/exclusive/acquire" | "/exclusive/release") {
        // Match the manager's acknowledgement window: suspension and memory
        // reclamation can legitimately exceed the ordinary control deadline.
        150
    } else if path.ends_with("/evict")
        || path.ends_with("snapshot")
        || path.ends_with("snapshot-completed")
        || native
            && method == "DELETE"
            && path.starts_with("/runs/")
            && path.matches('/').count() == 2
    {
        300
    } else {
        25
    })
}

async fn forward(
    master: &Master<'_>,
    runner: &url::Url,
    runner_token: &str,
    command: &Value,
    executor: &Arc<Executor>,
) -> Result<()> {
    let id = text(command, "id");
    crate::validation::uuid(id)?;
    let method = text(command, "method");
    let path = text(command, "path");
    if let Some(attempt) = path.strip_prefix("/prepare/") {
        crate::validation::uuid(attempt)?;
        let prepared = tokio::time::timeout(
            Duration::from_secs(280),
            executor.prepare(master.client, master.url, master.token, attempt),
        )
        .await;
        let status = match prepared {
            Ok(Ok(())) => 200,
            Ok(Err(error)) => {
                tracing::warn!(attempt, status = error.status, error = %error, "Node workspace preparation failed");
                error.status
            }
            Err(_) => 504,
        };
        return master.send(&End::with_status(id, status)).await;
    }
    let operation = call_controller(master, runner, runner_token, command, executor);
    let Ok(Ok(mut response)) = tokio::time::timeout(
        controller_timeout(method, path, crate::native::enabled()),
        operation,
    )
    .await
    else {
        let unavailable = End {
            data: Some(STANDARD.encode(b"VM controller unavailable")),
            ..End::with_status(id, 503)
        };
        return master.send(&unavailable).await;
    };
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_owned();
    let head = Head {
        id,
        sequence: 0,
        status: response.status().as_u16(),
        length: response.content_length(),
        content_type: &content_type,
    };
    master.send(&head).await?;
    // Bulk data uses one continuous, backpressured request. Keep control/log
    // frames and old masters on the existing protocol (including /wait's result).
    let streams_snapshot_body = path.starts_with("/snapshots/")
        && (method == "GET"
            || (method == "POST" && (path.ends_with("/blocks") || path.ends_with("/publication"))));
    // Reader heartbeats stay on acknowledged control frames. A lost manager
    // must end their ownership within the control round-trip deadline, rather
    // than buffering tiny heartbeat bytes in a long bulk upload.
    if command["streamBody"] == true && streams_snapshot_body {
        return stream_body(master, id, response).await;
    }
    let mut sequence = 1u64;
    while let Some(bytes) = response
        .chunk()
        .await
        .map_err(|_| Error::unavailable("VM response interrupted."))?
    {
        for chunk in bytes.chunks(FRAME_BYTES) {
            let frame = Chunk {
                id,
                sequence,
                data: STANDARD.encode(chunk),
            };
            master.send(&frame).await?;
            sequence += 1;
        }
    }
    if let Some(attempt) = path
        .strip_prefix("/runs/")
        .and_then(|v| v.strip_suffix("/wait"))
    {
        Executor::result(master.client, master.url, master.token, attempt).await?;
    }
    let end = End {
        id,
        sequence,
        status: None,
        done: true,
        data: None,
    };
    master.send(&end).await
}

/// Validates a relayed command and sends it to the local controller.
async fn call_controller(
    master: &Master<'_>,
    runner: &url::Url,
    runner_token: &str,
    command: &Value,
    executor: &Executor,
) -> Result<reqwest::Response> {
    let method = text(command, "method");
    let path = text(command, "path");
    route(method, path)?;
    let mut bytes = STANDARD
        .decode(text(command, "body"))
        .map_err(|_| Error::bad("Invalid execution payload."))?;
    let limit = if path.ends_with("/restore") || path.ends_with("/publication") {
        super::snapshots::MAX_MANIFEST_BYTES
    } else {
        MAX_PAYLOAD_BYTES
    };
    if bytes.len() > limit {
        return Err(Error::bad("Execution payload exceeds limit."));
    }

    if method == "POST" && path.starts_with("/disks/") && path.ends_with("/restore") {
        let mut body: Value = serde_json::from_slice(&bytes)?;
        let restore = body
            .as_object_mut()
            .ok_or_else(|| Error::bad("Invalid disk restore request."))?;
        restore.insert("master".into(), master.url.as_str().into());
        bytes = serde_json::to_vec(&body)?;
    }

    executor
        .before(
            master.client,
            master.url,
            master.token,
            method,
            path,
            &bytes,
        )
        .await?;
    master
        .controller
        .request(
            method.parse().map_err(Error::internal)?,
            runner.join(path).map_err(Error::internal)?,
        )
        .bearer_auth(runner_token)
        .header("content-type", "application/json")
        .body(bytes)
        .send()
        .await
        .map_err(|_| Error::unavailable("Local VM controller unavailable."))
}

/// Uploads the whole response body to the master in one streaming request.
async fn stream_body(master: &Master<'_>, id: &str, response: reqwest::Response) -> Result<()> {
    let (progress, observed) = tokio::sync::watch::channel(0u64);
    let chunks = futures_util::stream::try_unfold(
        (response.bytes_stream(), progress),
        |(mut stream, progress)| async move {
            match tokio::time::timeout(Duration::from_secs(60), stream.next()).await {
                Ok(Some(Ok(bytes))) => {
                    if !bytes.is_empty() {
                        progress.send_modify(|total| *total += bytes.len() as u64);
                    }
                    Ok(Some((bytes, (stream, progress))))
                }
                Ok(None) => Ok(None),
                _ => Err(std::io::Error::other("VM response interrupted")),
            }
        },
    );
    let upload = master
        .client
        .post(
            master
                .url
                .join(&format!("internal/nodes/stream/{id}"))
                .map_err(Error::internal)?,
        )
        .bearer_auth(master.token)
        .header("content-type", "application/octet-stream")
        .body(reqwest::Body::wrap_stream(chunks))
        .send();
    let uploaded = progressing_upload(
        async {
            upload
                .await
                .map_err(|_| Error::unavailable("Master upload interrupted."))
        },
        observed,
    )
    .await?;
    if !uploaded.status().is_success() {
        return Err(Error::conflict("Execution stream rejected."));
    }
    Ok(())
}

async fn progressing_upload<T>(
    upload: impl std::future::Future<Output = Result<T>>,
    mut observed: tokio::sync::watch::Receiver<u64>,
) -> Result<T> {
    tokio::pin!(upload);
    // A whole-publication response may legitimately exceed two minutes. Bound
    // stalled uploads and the final acknowledgement while allowing progress.
    loop {
        tokio::select! {
            response = &mut upload => {
                return response;
            }
            progress = tokio::time::timeout(Duration::from_secs(120), observed.changed()) => {
                match progress {
                    Ok(Ok(())) => {},
                    Ok(Err(_)) => {
                        return tokio::time::timeout(Duration::from_secs(120), &mut upload)
                            .await
                            .map_err(|_| Error::unavailable("Master upload interrupted."))?;
                    }
                    Err(_) => return Err(Error::unavailable("Master upload stalled.")),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn storage_pressure_publication_can_open_a_large_stopped_journal() {
        let path = format!("/snapshots/{}/publication", crate::config::id());
        assert!(controller_timeout("POST", &path, true) >= Duration::from_secs(120));
        assert_eq!(
            controller_timeout("POST", "/storage-policy", true),
            Duration::from_secs(25)
        );
    }

    #[test]
    fn idle_cache_eviction_reaches_the_controller_only_with_a_canonical_run_and_post() {
        let path = format!("/disks/{}/evict", crate::config::id());
        route("POST", &path).unwrap();
        assert_eq!(
            controller_timeout("POST", &path, true),
            Duration::from_secs(300)
        );
        for method in ["GET", "PUT", "DELETE"] {
            assert!(route(method, &path).is_err());
        }
        for path in ["/disks/not-a-run/evict", "/disks/../evict", "/disks/evict"] {
            assert!(route("POST", path).is_err());
        }
    }

    #[test]
    fn exclusive_commands_cross_the_outbound_relay_with_their_acknowledgement_deadline() {
        assert!(route("GET", "/exclusive").is_ok());
        assert_eq!(
            controller_timeout("GET", "/exclusive", false),
            Duration::from_secs(25)
        );
        for action in ["acquire", "release"] {
            let path = format!("/exclusive/{action}");
            assert!(route("POST", &path).is_ok());
            assert!(route("GET", &path).is_err());
            assert_eq!(
                controller_timeout("POST", &path, false),
                Duration::from_secs(150)
            );
        }
        for (method, path) in [
            ("POST", "/exclusive"),
            ("DELETE", "/exclusive"),
            ("POST", "/exclusive/renew"),
            ("POST", "/exclusive/acquire/extra"),
        ] {
            assert!(route(method, path).is_err());
        }
    }

    #[tokio::test]
    async fn remote_exclusive_and_pause_inspection_reach_the_authenticated_controller() {
        use axum::{Router, http::StatusCode, routing::get};
        async fn inspect(
            headers: axum::http::HeaderMap,
            uri: axum::http::Uri,
        ) -> (StatusCode, axum::Json<Value>) {
            if headers
                .get("authorization")
                .and_then(|value| value.to_str().ok())
                != Some("Bearer test-controller")
            {
                return (
                    StatusCode::UNAUTHORIZED,
                    axum::Json(json!({ "error": "denied" })),
                );
            }
            let value = if uri.path().ends_with("/administrative-pause") {
                json!({ "administrativePausedMs": 1234 })
            } else {
                json!({ "highestEpoch": 0, "reservation": null })
            };
            (StatusCode::OK, axum::Json(value))
        }
        let controller = Router::new()
            .route("/exclusive", get(inspect))
            .route("/runs/{id}/administrative-pause", get(inspect));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = url::Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
        let stop = tokio_util::sync::CancellationToken::new();
        let stopped = stop.clone();
        let server = tokio::spawn(async move {
            axum::serve(listener, controller)
                .with_graceful_shutdown(stopped.cancelled_owned())
                .await
                .unwrap();
        });
        let http = reqwest::Client::new();
        let master = Master {
            client: &http,
            controller: &http,
            url: &url,
            token: "test-node",
        };
        let executor = Executor::default();
        let pause = format!("/runs/{}/administrative-pause", crate::config::id());
        for (path, expected) in [
            (
                "/exclusive",
                json!({ "highestEpoch": 0, "reservation": null }),
            ),
            (pause.as_str(), json!({ "administrativePausedMs": 1234 })),
        ] {
            let command = json!({ "method": "GET", "path": path, "body": "" });
            let response = call_controller(&master, &url, "test-controller", &command, &executor)
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(response.json::<Value>().await.unwrap(), expected);
            let denied = call_controller(&master, &url, "wrong-controller", &command, &executor)
                .await
                .unwrap();
            assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);
        }
        assert!(route("POST", &pause).is_err());
        assert!(route("GET", "/runs/invalid/administrative-pause").is_err());
        assert!(route("GET", &format!("{pause}/extra")).is_err());
        stop.cancel();
        server.await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn publication_upload_allows_progress_beyond_the_old_deadline() {
        let (progress, observed) = tokio::sync::watch::channel(0);
        let work = async move {
            for value in 1..=3 {
                tokio::time::sleep(Duration::from_secs(59)).await;
                progress.send(value).unwrap();
            }
            Ok(())
        };
        assert!(progressing_upload(work, observed).await.is_ok());
    }

    #[tokio::test(start_paused = true)]
    async fn publication_upload_rejects_a_stall_and_a_missing_final_response() {
        for closed in [false, true] {
            let (progress, observed) = tokio::sync::watch::channel(0);
            let held = (!closed).then_some(progress);
            let started = tokio::time::Instant::now();
            let result: Result<()> = progressing_upload(std::future::pending(), observed).await;
            assert_eq!(result.unwrap_err().status, 503);
            assert_eq!(started.elapsed(), Duration::from_secs(120));
            drop(held);
        }
    }

    #[test]
    fn completed_generation_capture_is_authorized_through_the_outbound_relay() {
        let path = format!("/disks/{}/snapshot-completed", crate::config::id());
        assert!(route("POST", &path).is_ok());
        assert!(route("GET", &path).is_err());
        assert!(route("POST", "/disks/not-a-conversation/snapshot-completed").is_err());
        assert_eq!(
            controller_timeout("POST", &path, false),
            Duration::from_secs(300)
        );
    }

    #[test]
    fn native_stop_allows_the_stopped_checkpoint_to_finish_before_relay_timeout() {
        let path = format!("/runs/{}", crate::config::id());
        assert_eq!(
            controller_timeout("DELETE", &path, true),
            Duration::from_secs(300)
        );
        assert_eq!(
            controller_timeout("DELETE", &path, false),
            Duration::from_secs(25)
        );
        assert_eq!(
            controller_timeout("POST", &format!("{path}/lease"), true),
            Duration::from_secs(25)
        );
    }

    #[test]
    fn publication_reader_is_authorized_through_the_outbound_relay() {
        let path = format!("/snapshots/{}/publication", crate::config::id());
        assert!(route("POST", &path).is_ok());
        assert!(route("GET", &path).is_err());
        assert!(route("POST", "/snapshots/invalid/publication").is_err());
    }
}
