//! Node-side HTTP relay. The master can address only the private VM controller.
use super::executor::Executor;
use crate::{
    error::{Error, Result},
    validation::text,
};
use base64::{Engine, engine::general_purpose::STANDARD};
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
    let (path, query) = path
        .split_once('?')
        .map_or((path, None), |(path, query)| (path, Some(query)));
    if query.is_some() {
        if method != "GET" || !path.ends_with("/logs") {
            return Err(Error::bad("Unsupported execution query."));
        }
        crate::runner::logs::offset(query)?;
    }
    let parts = path.trim_start_matches('/').split('/').collect::<Vec<_>>();
    let uuid = |id: &str| crate::validation::uuid(id).is_ok();
    let allowed = match parts.as_slice() {
        ["health"] => method == "GET",
        ["exclusive"] => method == "GET",
        ["exclusive", "acquire" | "release"] => method == "POST",
        ["runs", id] => uuid(id) && ["POST", "DELETE"].contains(&method),
        ["runs", id, operation] => {
            uuid(id)
                && matches!(
                    (method, *operation),
                    ("GET", "logs" | "administrative-pause")
                        | ("POST", "wait")
                        | ("POST", "artifact")
                )
        }
        ["runs", id, "projects", project] => method == "POST" && uuid(id) && uuid(project),
        ["storage-policy"] | ["storage-policy", "probe"] => method == "POST",
        ["disks", id, operation] => {
            method == "POST"
                && uuid(id)
                && ["delete", "prune", "storage-status", "publication-fence"].contains(operation)
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
    if native && crate::nodes::is_project_import_request(method, path) {
        return crate::nodes::PROJECT_OPEN_RELAY_TIMEOUT;
    }
    let seconds = if method == "POST" && matches!(path, "/exclusive/acquire" | "/exclusive/release")
    {
        150
    } else if path.starts_with("/prepare/")
        || native
            && method == "DELETE"
            && path.starts_with("/runs/")
            && path.matches('/').count() == 2
    {
        300
    } else {
        25
    };
    Duration::from_secs(seconds)
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
    let native = executor.native();
    let project_import = native && crate::nodes::is_project_import_request(method, path);
    let operation = call_controller(master, runner, runner_token, command, executor);
    let mut response =
        match tokio::time::timeout(controller_timeout(method, path, native), operation).await {
            Ok(Ok(response)) => response,
            Ok(Err(error)) if project_import => {
                tracing::warn!(
                    status = error.status,
                    error = %error,
                    "Native project import failed before reaching the controller"
                );
                return master.send(&End::with_status(id, error.status)).await;
            }
            Err(_) if project_import => return master.send(&End::with_status(id, 504)).await,
            _ => {
                let unavailable = End {
                    data: Some(STANDARD.encode(b"VM controller unavailable")),
                    ..End::with_status(id, 503)
                };
                return master.send(&unavailable).await;
            }
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
    if bytes.len() > MAX_PAYLOAD_BYTES {
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

#[cfg(test)]
mod tests {
    use super::*;

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
}

#[cfg(test)]
#[path = "relay/project_import_tests.rs"]
mod project_import_tests;
