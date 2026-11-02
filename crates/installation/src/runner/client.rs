//! Guest-side `runner` client: starts an attempt and relays its output and exit.
use super::CONTROLLER_INTERRUPTED;
use crate::{
    error::{Error, Result},
    microvm::{protocol::Event, wire},
    run_output,
    validation::uuid,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use futures_util::StreamExt;
use reqwest::StatusCode;
use serde_json::{Value, json};
use std::{future::Future, time::Duration};
use tokio::io::{AsyncWriteExt, BufReader};
use tokio_util::sync::CancellationToken;

/// Exit code reported when this client is asked to stop.
const STOPPED: i32 = 143;
const ERROR_BODY_LIMIT: usize = 10_000;
const LOCAL_OUTPUT_FAILURE: &str = "Local output forwarding failed.";
/// Terminal reconciliation after output EOF is bounded independently of execution.
pub(crate) const RECONCILE_TIMEOUT: Duration = Duration::from_secs(5);

struct Controller {
    http: reqwest::Client,
    url: String,
    token: String,
}

/// Current controllers report occupied native slots as plain 503 errors.
/// Normalize known capacity refusals into the manager's wait contract.
fn startup_rejection(status: StatusCode, reason: Option<&str>) -> String {
    let mut message = format!("Execution controller rejected startup (HTTP {status}).");
    if let Some(reason) = reason {
        message.push(' ');
        message.push_str(reason);
    }
    let capacity = status == StatusCode::SERVICE_UNAVAILABLE
        && matches!(
            reason,
            Some(
                "All VM slots are occupied."
                    | "The native execution slot is occupied."
                    | "Shared node resources are under pressure."
            )
        );
    if capacity {
        return format!(
            "{} can accept this execution right now. {message}",
            crate::nodes::placement::NO_CAPACITY,
        );
    }
    message
}

impl Controller {
    async fn start(&self) -> Result<()> {
        let mut response = self
            .http
            .post(&self.url)
            .bearer_auth(&self.token)
            .timeout(Duration::from_secs(30))
            .send()
            .await
            .map_err(|_| Error::unavailable("Execution controller could not start the run."))?;
        if !response.status().is_success() {
            let status = response.status();
            let mut body = Vec::new();
            while let Ok(Some(chunk)) = response.chunk().await {
                if body.len() + chunk.len() > ERROR_BODY_LIMIT {
                    body.clear();
                    break;
                }
                body.extend_from_slice(&chunk);
            }

            let error = serde_json::from_slice::<Value>(&body).unwrap_or_default();
            let message = startup_rejection(status, error["error"].as_str());
            let message = run_output::redact(&message, std::slice::from_ref(&self.token));

            // Only an explicit rejection proves that the native agent never started.
            // Transport failures deliberately leave the launch outcome uncertain.
            let event = json!({
                "type": "runner.start_rejected",
                "message": message,
            });

            let mut stdout = tokio::io::stdout();
            stdout.write_all(format!("{event}\n").as_bytes()).await?;
            stdout.flush().await?;
            return Err(Error::unavailable(message));
        }
        Ok(())
    }

    async fn logs(&self, offset: u64) -> Result<reqwest::Response> {
        let request = self
            .http
            .get(format!("{}/logs?cursor=1&offset={offset}", self.url))
            .bearer_auth(&self.token)
            .send();
        let logs = tokio::time::timeout(Duration::from_secs(10), request)
            .await
            .map_err(|_| Error::unavailable("Execution output connection timed out."))?
            .map_err(|_| Error::unavailable("Could not read execution output."))?;
        if !logs.status().is_success() {
            return Err(Error::unavailable("Could not read execution output."));
        }
        Ok(logs)
    }

    async fn wait(&self) -> Result<ControllerResult> {
        wait_for_exit(&self.http, &self.url, &self.token).await
    }

    async fn output(&self) -> Result<()> {
        relay_attempt_output(
            |offset| self.logs(offset),
            &mut tokio::io::stdout(),
            &mut tokio::io::stderr(),
        )
        .await
    }

    async fn run(&self) -> Result<i32> {
        self.start().await?;
        let output = self.output();
        let (output, status) = collect_completion(output, self.wait()).await;
        if let Err(error) = output {
            eprintln!("{}", error.message);
            // Report the missing output independently of the controller result.
            // A leading newline separates this marker from a truncated provider frame.
            let mut stdout = tokio::io::stdout();
            stdout
                .write_all(b"\n{\"type\":\"runner.output_interrupted\"}\n")
                .await?;
            stdout.flush().await?;
        }
        if let Ok(result) = &status
            && result.physical_finalized
        {
            // Emit only after output has drained, so this trusted controller
            // observation cannot interleave with an incomplete provider frame.
            let event = json!({
                "type": "runner.finalized",
                "attemptId": self.url.rsplit('/').next(),
                "physicalFinalized": true,
                "finalizationFailed": result.finalization_failed,
            });
            tokio::io::stdout()
                .write_all(format!("\n{event}\n").as_bytes())
                .await?;
        }
        // Preserve the last complete frames even when the next one was cut.
        let _ = tokio::io::stdout().flush().await;
        let _ = tokio::io::stderr().flush().await;
        match status {
            Ok(result) => Ok(result.code),
            Err(error) => Err(error),
        }
    }

    async fn stop(&self) {
        let _ = self
            .http
            .delete(&self.url)
            .bearer_auth(&self.token)
            .timeout(Duration::from_secs(17))
            .send()
            .await;
    }
}

#[derive(Default)]
struct OutputProgress {
    first_packet: Option<tokio::time::Instant>,
    observed_span: Duration,
}

impl OutputProgress {
    fn received(&mut self) {
        let now = tokio::time::Instant::now();
        let first = self.first_packet.get_or_insert(now);
        self.observed_span = now.duration_since(*first);
    }
}

async fn relay_attempt_output<F, Fut>(
    mut connect: F,
    stdout: &mut (impl tokio::io::AsyncWrite + Unpin),
    stderr: &mut (impl tokio::io::AsyncWrite + Unpin),
) -> Result<()>
where
    F: FnMut(u64) -> Fut,
    Fut: Future<Output = Result<reqwest::Response>>,
{
    let mut offset = 0;
    let mut failures = 0;
    loop {
        let mut progress = OutputProgress::default();
        let result = match connect(offset).await {
            Ok(logs)
                if logs
                    .headers()
                    .get("content-type")
                    .and_then(|value| value.to_str().ok())
                    == Some(super::logs::CONTENT_TYPE) =>
            {
                relay_cursor_output(logs, &mut offset, stdout, stderr, &mut progress).await
            }
            // Old controllers cannot resume safely: use the legacy reader once.
            Ok(logs) if offset == 0 => return relay_output(logs, stdout, stderr).await,
            Ok(_) => {
                return Err(Error::unavailable(
                    "Execution output cursor support changed.",
                ));
            }
            Err(error) => Err(error),
        };
        if result.as_ref().is_ok()
            || result
                .as_ref()
                .is_err_and(|error| error.message == LOCAL_OUTPUT_FAILURE)
        {
            return result;
        }
        if progress.observed_span >= Duration::from_secs(15) {
            failures = 0;
        }
        if failures >= 3 {
            return result;
        }
        failures += 1;
        tracing::warn!(
            offset,
            failures,
            "Reconnecting output of the same execution attempt"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

/// A failed channel cannot truncate its intact peer. Output readers bound
/// transport inactivity per record, so an actively draining backlog can finish.
async fn collect_completion<T>(
    output: impl Future<Output = Result<()>>,
    status: impl Future<Output = Result<T>>,
) -> (Result<()>, Result<T>) {
    tokio::pin!(output, status);
    tokio::select! {
        output = &mut output => {
            let status = if output.is_ok() {
                tokio::time::timeout(RECONCILE_TIMEOUT, status).await
                    .unwrap_or_else(|_| Err(Error::unavailable("Execution completion is not confirmed.")))
            } else {
                status.await
            };
            (output, status)
        }
        status = &mut status => {
            (output.await, status)
        }
    }
}

/// Read only the terminal result of this exact attempt. Both controller backends
/// publish it after finalization and physical ownership release.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ControllerResult {
    pub code: i32,
    pub finalization_failed: bool,
    pub physical_finalized: bool,
}

pub(crate) async fn wait_for_exit(
    http: &reqwest::Client,
    url: &str,
    token: &str,
) -> Result<ControllerResult> {
    let response = http
        .post(format!("{url}/wait"))
        .bearer_auth(token)
        .send()
        .await
        .map_err(|_| Error::unavailable("Could not wait for execution."))?;
    if !response.status().is_success() {
        return Err(Error::unavailable("Could not wait for execution."));
    }
    let value: Value = response
        .json()
        .await
        .map_err(|_| Error::unavailable("Execution completion connection was interrupted."))?;
    let code = value["StatusCode"]
        .as_i64()
        .filter(|n| (0..=255).contains(n))
        .map(|n| n as i32)
        .ok_or_else(|| Error::bad("Invalid execution exit status."))?;
    let finalization_failed =
        value["FinalizationFailed"] == true && code == super::CONTROLLER_FAILED;
    Ok(ControllerResult {
        code,
        finalization_failed,
        physical_finalized: value["PhysicalFinalized"] == true
            && value["FinalizationFailed"] != true,
    })
}

/// A cursor is acknowledged only after the complete frame reached its output.
async fn relay_cursor_output(
    logs: reqwest::Response,
    offset: &mut u64,
    stdout: &mut (impl tokio::io::AsyncWrite + Unpin),
    stderr: &mut (impl tokio::io::AsyncWrite + Unpin),
    progress: &mut OutputProgress,
) -> Result<()> {
    let stream = logs
        .bytes_stream()
        .map(|result| result.map_err(std::io::Error::other));
    let mut reader = BufReader::new(tokio_util::io::StreamReader::new(stream));
    loop {
        let packet = tokio::time::timeout(Duration::from_secs(10), wire::read(&mut reader))
            .await
            .map_err(|_| {
                Error::unavailable("Execution output stopped making transport progress.")
            })??
            .ok_or_else(|| {
                Error::unavailable("Execution output ended without a completion marker.")
            })?;
        match packet["type"].as_str() {
            Some("heartbeat") => {
                progress.received();
                continue;
            }
            Some("end") if packet["offset"].as_u64() == Some(*offset) => {
                stdout.flush().await?;
                stderr.flush().await?;
                return Ok(());
            }
            Some("log") => {
                let next = packet["offset"]
                    .as_u64()
                    .filter(|next| *next > *offset)
                    .ok_or_else(|| Error::bad("Invalid execution output cursor."))?;
                let event: Event =
                    wire::decode(packet["event"].clone(), "Invalid execution output.")?;
                if let Event::Output {
                    stderr: to_stderr,
                    data,
                } = event
                {
                    let bytes = STANDARD
                        .decode(data)
                        .map_err(|_| Error::bad("Invalid execution output."))?;
                    if to_stderr {
                        stderr
                            .write_all(&bytes)
                            .await
                            .map_err(|_| Error::bad(LOCAL_OUTPUT_FAILURE))?;
                        stderr
                            .flush()
                            .await
                            .map_err(|_| Error::bad(LOCAL_OUTPUT_FAILURE))?;
                    } else {
                        stdout
                            .write_all(&bytes)
                            .await
                            .map_err(|_| Error::bad(LOCAL_OUTPUT_FAILURE))?;
                        stdout
                            .flush()
                            .await
                            .map_err(|_| Error::bad(LOCAL_OUTPUT_FAILURE))?;
                    }
                }
                *offset = next;
                progress.received();
            }
            _ => return Err(Error::bad("Invalid execution output packet.")),
        }
    }
}

/// Writes attempt output to this process's stdout and stderr.
async fn relay_output(
    logs: reqwest::Response,
    stdout: &mut (impl tokio::io::AsyncWrite + Unpin),
    stderr: &mut (impl tokio::io::AsyncWrite + Unpin),
) -> Result<()> {
    let stream = logs
        .bytes_stream()
        .map(|r| r.map_err(std::io::Error::other));
    let mut reader = BufReader::new(tokio_util::io::StreamReader::new(stream));
    loop {
        let event = tokio::time::timeout(Duration::from_secs(10), wire::read(&mut reader))
            .await
            .map_err(|_| Error::unavailable("Execution output stopped making transport progress."))?
            .map_err(|error| {
                if error.status == 500 {
                    Error::unavailable("Execution output connection was interrupted.")
                } else {
                    error
                }
            })?;
        let Some(event) = event else {
            stdout.flush().await?;
            stderr.flush().await?;
            return Ok(());
        };
        let Event::Output {
            stderr: to_stderr,
            data,
        } = wire::decode(event, "Invalid execution output.")?
        else {
            continue;
        };
        let bytes = STANDARD
            .decode(data)
            .map_err(|_| Error::bad("Invalid execution output."))?;
        if to_stderr {
            stderr.write_all(&bytes).await?;
        } else {
            stdout.write_all(&bytes).await?;
        }
    }
}

pub async fn client(id: &str, stop: CancellationToken) -> Result<i32> {
    uuid(id)?;
    let base = std::env::var("RUNNER_URL").map_err(|_| Error::bad("Missing runner URL."))?;
    let token =
        std::env::var("RUNNER_TOKEN").map_err(|_| Error::bad("Missing runner credential."))?;
    let http = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(Error::internal)?;
    let controller = Controller {
        http,
        url: format!("{base}/runs/{id}"),
        token,
    };
    let result = tokio::select! {
        () = stop.cancelled() => Ok(STOPPED),
        result = controller.run() => result,
    };
    // An uncertain transport result is not a request to stop the workload.
    // The manager observes this same attempt before its existing fencing path.
    if stop.is_cancelled() {
        controller.stop().await;
    }
    match result {
        Err(error) if error.is_unavailable() => {
            eprintln!("{}", error.message);
            Ok(CONTROLLER_INTERRUPTED)
        }
        Ok(STOPPED) if !stop.is_cancelled() => Ok(CONTROLLER_INTERRUPTED),
        result => result,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn broken_output_preserves_terminal_status_in_both_orders() {
        use axum::{
            Router,
            body::Body,
            routing::{get, post},
        };
        for code in [0, super::super::CONTROLLER_FAILED] {
            for logs_first in [true, false] {
                let logs = move || async move {
                    if !logs_first {
                        tokio::time::sleep(Duration::from_millis(30)).await;
                    }
                    // A partial final frame is a real relay_output failure.
                    Body::from("{\"type\":")
                };
                let wait = move || async move {
                    if logs_first {
                        tokio::time::sleep(Duration::from_millis(30)).await;
                    }
                    axum::Json(json!({ "StatusCode": code }))
                };
                let app = Router::new()
                    .route("/runs/fixture", post(|| async {}))
                    .route("/runs/fixture/logs", get(logs))
                    .route("/runs/fixture/wait", post(wait));
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                let address = listener.local_addr().unwrap();
                let server = tokio::spawn(async move {
                    axum::serve(listener, app).await.unwrap();
                });
                let controller = Controller {
                    http: reqwest::Client::new(),
                    url: format!("http://{address}/runs/fixture"),
                    token: "fixture".into(),
                };
                assert_eq!(
                    controller.run().await.unwrap(),
                    code,
                    "logs_first={logs_first}"
                );
                server.abort();
            }
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_failed_channel_or_confirmed_exit_preserves_its_intact_peer() {
        let started = tokio::time::Instant::now();
        let (output, status) = collect_completion(
            async { Err(Error::unavailable("fixture output cut")) },
            async {
                tokio::time::sleep(RECONCILE_TIMEOUT * 2).await;
                Ok(0)
            },
        )
        .await;
        assert!(output.is_err());
        assert_eq!(status.unwrap(), 0);
        assert_eq!(started.elapsed(), RECONCILE_TIMEOUT * 2);
        let (output, status) = collect_completion(
            async {
                tokio::time::sleep(RECONCILE_TIMEOUT * 2).await;
                Ok(())
            },
            async { Ok(0) },
        )
        .await;
        assert!(output.is_ok());
        assert_eq!(status.unwrap(), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn failed_wait_never_truncates_an_intact_attempt_output_channel() {
        let started = tokio::time::Instant::now();
        let duration = RECONCILE_TIMEOUT * 2;
        let (output, status) = collect_completion::<i32>(
            async {
                tokio::time::sleep(duration).await;
                Ok(())
            },
            async { Err(Error::unavailable("fixture wait transport failed")) },
        )
        .await;
        assert!(output.is_ok());
        assert!(status.is_err());
        assert!(started.elapsed() >= duration);
    }

    #[tokio::test]
    async fn failed_initial_wait_does_not_issue_unreserved_reads() {
        use axum::{
            Router,
            routing::{get, post},
        };
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };
        let requests = Arc::new(AtomicUsize::new(0));
        let count = requests.clone();
        let app = Router::new()
            .route("/runs/fixture", post(|| async {}))
            .route("/runs/fixture/logs", get(|| async { "" }))
            .route(
                "/runs/fixture/wait",
                post(move || {
                    let count = count.clone();
                    async move {
                        count.fetch_add(1, Ordering::SeqCst);
                        StatusCode::SERVICE_UNAVAILABLE
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let controller = Controller {
            http: reqwest::Client::new(),
            url: format!("http://{address}/runs/fixture"),
            token: "fixture".into(),
        };
        assert!(controller.run().await.unwrap_err().is_unavailable());
        assert_eq!(requests.load(Ordering::SeqCst), 1);
        server.abort();
    }

    #[tokio::test(start_paused = true)]
    async fn delayed_headers_and_silent_bodies_cannot_reset_connection_failures() {
        let mut requests = 0;
        let connect = |_offset| {
            requests += 1;
            async {
                tokio::time::sleep(Duration::from_secs(6)).await;
                let body = reqwest::Body::wrap_stream(futures_util::stream::pending::<
                    std::io::Result<bytes::Bytes>,
                >());
                let response = axum::http::Response::builder()
                    .status(200)
                    .header("content-type", super::super::logs::CONTENT_TYPE)
                    .body(body)
                    .unwrap();
                Ok(reqwest::Response::from(response))
            }
        };
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let result = tokio::time::timeout(
            Duration::from_secs(100),
            relay_attempt_output(connect, &mut stdout, &mut stderr),
        )
        .await;
        assert!(
            result.is_ok(),
            "silent attempts must exhaust the reconnect ceiling"
        );
        assert!(result.unwrap().is_err());
        assert_eq!(requests, 4);
        assert!(stdout.is_empty() && stderr.is_empty());
    }

    #[test]
    fn startup_rejections_are_backend_neutral_and_preserve_capacity_detection() {
        let message = startup_rejection(
            StatusCode::CONFLICT,
            Some("Native workspace is still active."),
        );
        assert!(message.starts_with("Execution controller rejected startup"));
        assert!(message.ends_with("Native workspace is still active."));
        for reason in [
            "All VM slots are occupied.",
            "The native execution slot is occupied.",
        ] {
            let message = startup_rejection(StatusCode::SERVICE_UNAVAILABLE, Some(reason));
            assert!(message.starts_with(crate::nodes::placement::NO_CAPACITY));
        }
    }
}

#[cfg(test)]
mod physical_result_tests {
    use super::*;

    #[tokio::test]
    async fn exit_code_and_legacy_markers_never_substitute_for_a_physical_certificate() {
        use axum::{Router, routing::post};
        for (value, confirmed) in [
            (json!({"StatusCode":0}), false),
            (json!({"StatusCode":0,"PhysicalFinalized":false}), false),
            (json!({"StatusCode":17,"PhysicalFinalized":true}), true),
            (json!({"StatusCode":143,"PhysicalFinalized":true}), true),
            (
                json!({"StatusCode":70,"PhysicalFinalized":true,"FinalizationFailed":true}),
                false,
            ),
        ] {
            let app = Router::new().route(
                "/wait",
                post(move || {
                    let value = value.clone();
                    async move { axum::Json(value) }
                }),
            );
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let server = tokio::spawn(async move {
                axum::serve(listener, app).await.unwrap();
            });
            let result = wait_for_exit(
                &reqwest::Client::new(),
                &format!("http://{address}"),
                "fixture",
            )
            .await
            .unwrap();
            assert_eq!(result.physical_finalized, confirmed);
            server.abort();
        }
    }
}
