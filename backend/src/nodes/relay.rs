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
        let executor = executor.clone();
        tasks.spawn(async move {
            let _permit = permit;
            let master = Master {
                client: &http,
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
        let _ = client
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
        ["snapshots", id, hash] => {
            uuid(id)
                && ((method == "GET" && super::snapshots::valid_hash(hash))
                    || (method == "POST" && matches!(*hash, "blocks" | "publication"))
                    || (method == "DELETE" && *hash == "discard"))
        }
        ["runs", id] => uuid(id) && ["POST", "DELETE"].contains(&method),
        ["runs", id, operation] => {
            uuid(id)
                && matches!(
                    (method, *operation),
                    ("GET", "logs")
                        | ("POST", "wait")
                        | ("POST", "artifact")
                        | ("POST", "snapshot")
                )
        }
        ["runs", id, "projects", project] => method == "POST" && uuid(id) && uuid(project),
        ["storage-policy"] => method == "POST",
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
                    "storage-status",
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
fn controller_timeout(path: &str) -> Duration {
    Duration::from_secs(if path.ends_with("/restore") {
        120
    } else if path.ends_with("snapshot") || path.ends_with("snapshot-completed") {
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
        let status = if matches!(prepared, Ok(Ok(()))) {
            200
        } else {
            503
        };
        return master.send(&End::with_status(id, status)).await;
    }
    let operation = call_controller(master, runner, runner_token, command, executor);
    let Ok(Ok(mut response)) = tokio::time::timeout(controller_timeout(path), operation).await
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
    let bytes = STANDARD
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
        .client
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
        assert_eq!(controller_timeout(&path), Duration::from_secs(300));
    }

    #[test]
    fn publication_reader_is_authorized_through_the_outbound_relay() {
        let path = format!("/snapshots/{}/publication", crate::config::id());
        assert!(route("POST", &path).is_ok());
        assert!(route("GET", &path).is_err());
        assert!(route("POST", "/snapshots/invalid/publication").is_err());
    }
}
