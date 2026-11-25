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
// Native startup can spend two minutes waiting for disk ownership before it
// initializes the filesystem and starts the provider.
const TURN_START_TIMEOUT: Duration = Duration::from_secs(180);
/// Terminal reconciliation after output EOF is bounded independently of execution.
pub(crate) const RECONCILE_TIMEOUT: Duration = Duration::from_secs(5);
pub(crate) const NATIVE_CLEANUP_RETIREMENT_QUERY: &str = "cleanupRetirement=1";

struct Controller {
    http: reqwest::Client,
    url: String,
    token: String,
}

/// Current controllers report occupied native slots as plain 503 errors.
/// Normalize known capacity refusals into the manager's wait contract.
fn startup_rejection(status: StatusCode, reason: Option<&str>) -> (String, bool) {
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
        return (
            format!(
                "{} can accept this execution right now. {message}",
                crate::nodes::placement::NO_CAPACITY,
            ),
            true,
        );
    }
    (message, false)
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
            let (message, capacity_rejection) = startup_rejection(status, error["error"].as_str());
            let message = run_output::redact(&message, std::slice::from_ref(&self.token));

            // Only an explicit rejection proves that the native agent never started.
            // Transport failures deliberately leave the launch outcome uncertain.
            let event = json!({
                "type": "runner.start_rejected",
                "message": message,
                "capacityRejection": capacity_rejection,
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
        if let Ok(turn) = std::env::var("CAIRN_PROVIDER_TURN") {
            return self.run_turn(&turn).await;
        }
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

    async fn run_turn(&self, turn: &str) -> Result<i32> {
        uuid(turn)?;
        self.start().await?;
        let start_url = format!("{}/turn/{turn}/start", self.url);
        let offset = tokio::time::timeout(TURN_START_TIMEOUT, async {
            loop {
                let response = self
                    .http
                    .get(&start_url)
                    .bearer_auth(&self.token)
                    .timeout(Duration::from_secs(30))
                    .send()
                    .await
                    .map_err(Error::internal)?;
                match response.status() {
                    StatusCode::SERVICE_UNAVAILABLE | StatusCode::ACCEPTED => {
                        // Observe the same attempt while its environment prepares.
                        tokio::time::sleep(Duration::from_millis(250)).await;
                    }
                    status if status.is_success() => {
                        let started: Value = response.json().await.map_err(Error::internal)?;
                        return started["offset"]
                            .as_u64()
                            .ok_or_else(|| Error::bad("Invalid turn start cursor."));
                    }
                    status => {
                        return Err(Error::unavailable(format!(
                            "Provider turn did not start (HTTP {status})."
                        )));
                    }
                }
            }
        })
        .await
        .map_err(|_| {
            Error::unavailable("Provider turn did not start before the preparation deadline.")
        })??;
        // Keep collecting output while completion is pending. A provider can
        // remain active arbitrarily long; waiting for its receipt first would
        // hide tool activity and progress from the conversation.
        let completion = async {
            let response = loop {
                let response = self
                    .http
                    .get(format!("{}/turn/{turn}", self.url))
                    .bearer_auth(&self.token)
                    .timeout(Duration::from_secs(25))
                    .send()
                    .await
                    .map_err(Error::internal)?;
                if response.status() != StatusCode::ACCEPTED {
                    break response;
                }
            };
            if !response.status().is_success() {
                return Err(Error::unavailable(
                    "Provider turn completion is unavailable.",
                ));
            }
            let receipt: crate::background_jobs::controller::TurnReceipt =
                response.json().await.map_err(Error::internal)?;
            if receipt.turn_id != turn
                || self.url.rsplit('/').next() != Some(receipt.attempt_id.as_str())
                || receipt.log_start != offset
                || receipt.log_end < offset
            {
                return Err(Error::bad("Invalid provider turn receipt."));
            }
            Ok(receipt)
        };
        tokio::pin!(completion);
        self.relay_turn(
            offset,
            &mut completion,
            &mut tokio::io::stdout(),
            &mut tokio::io::stderr(),
        )
        .await
    }

    async fn relay_turn(
        &self,
        mut offset: u64,
        completion: &mut (
                 impl Future<Output = Result<crate::background_jobs::controller::TurnReceipt>> + Unpin
             ),
        stdout: &mut (impl tokio::io::AsyncWrite + Unpin),
        stderr: &mut (impl tokio::io::AsyncWrite + Unpin),
    ) -> Result<i32> {
        let mut receipt = None;
        let mut reconnects = 0u8;
        let mut reader = None;
        loop {
            if let Some(receipt) = &receipt {
                let receipt: &crate::background_jobs::controller::TurnReceipt = receipt;
                if offset == receipt.log_end {
                    return Ok(receipt.code);
                }
                if offset > receipt.log_end {
                    return Err(Error::bad("Output crossed the turn boundary."));
                }
            }
            if reader.is_none() {
                let response = self.logs(offset).await?;
                let stream = response
                    .bytes_stream()
                    .map(|result| result.map_err(std::io::Error::other));
                reader = Some(BufReader::new(tokio_util::io::StreamReader::new(stream)));
            }
            let received = {
                let packet = tokio::time::timeout(
                    Duration::from_secs(10),
                    wire::read(reader.as_mut().unwrap()),
                );
                tokio::pin!(packet);
                loop {
                    tokio::select! {
                        result = &mut *completion, if receipt.is_none() => {
                            let finished = result?;
                            if offset == finished.log_end { return Ok(finished.code); }
                            if offset > finished.log_end { return Err(Error::bad("Output crossed the turn boundary.")); }
                            receipt = Some(finished);
                        }
                        packet = &mut packet => break packet,
                    }
                }
            };
            let packet = match received {
                Ok(Ok(Some(packet))) => packet,
                _ if reconnects < 3 => {
                    reconnects += 1;
                    reader = None;
                    tokio::time::sleep(Duration::from_millis(250)).await;
                    continue;
                }
                _ => return Err(Error::unavailable("Provider turn output was interrupted.")),
            };
            if packet["type"] == "heartbeat" {
                continue;
            }
            let next = packet["offset"]
                .as_u64()
                .filter(|n| *n > offset)
                .ok_or_else(|| Error::bad("Invalid turn log boundary."))?;
            let event: Event = serde_json::from_value(packet["event"].clone())?;
            if let Event::Output {
                stderr: is_error,
                data,
            } = event
            {
                let bytes = STANDARD.decode(data).map_err(Error::internal)?;
                if is_error {
                    stderr.write_all(&bytes).await?;
                } else {
                    stdout.write_all(&bytes).await?;
                }
            }
            offset = next;
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

/// Closed, payload-free reasons issued only by an authenticated controller.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalReason {
    NativeOutputLimit,
}

impl TerminalReason {
    pub(crate) fn message(self) -> &'static str {
        match self {
            Self::NativeOutputLimit => "Native output exceeded its bounded log budget.",
        }
    }
}

/// Read only the terminal result of this exact attempt. Both controller backends
/// publish it after finalization and physical ownership release.
#[derive(Clone, Debug)]
pub(crate) struct ControllerResult {
    pub code: i32,
    pub finalization_failed: bool,
    pub physical_finalized: bool,
    pub terminal_reason: Option<TerminalReason>,
    pub native_cleanup: Option<String>,
}

pub(crate) fn cleanup_result(value: &Value, attempt: &str) -> Option<ControllerResult> {
    if value["StatusCode"] != super::CONTROLLER_FAILED
        || value["FinalizationFailed"] != true
        || value["PhysicalFinalized"] == true
        || value["NativeCleanup"]["attemptId"] != attempt
        || value["NativeCleanup"]["workloadFenced"] != true
    {
        return None;
    }
    let message = value["NativeCleanup"]["message"]
        .as_str()
        .filter(|text| text.len() <= 8192)?;
    Some(ControllerResult {
        code: super::CONTROLLER_FAILED,
        finalization_failed: true,
        physical_finalized: false,
        terminal_reason: None,
        native_cleanup: Some(
            message
                .chars()
                .filter(|c| !c.is_control())
                .take(2048)
                .collect(),
        ),
    })
}

pub(crate) async fn wait_for_exit(
    http: &reqwest::Client,
    url: &str,
    token: &str,
) -> Result<ControllerResult> {
    wait_result(http, url, token, "").await
}

pub(crate) async fn observe_cleanup_retirement(
    http: &reqwest::Client,
    url: &str,
    token: &str,
) -> Result<ControllerResult> {
    wait_result(http, url, token, NATIVE_CLEANUP_RETIREMENT_QUERY).await
}

/// Plain, attempt-scoped flush failure. A structured Native receipt must be
/// evaluated against current cleanup state by the controller's DELETE path.
pub(crate) fn plain_finalization_failure(value: &Value) -> Option<ControllerResult> {
    if !value["NativeCleanup"].is_null() {
        return None;
    }
    terminal_result(value, "")
        .ok()
        .filter(|result| result.finalization_failed)
}

pub(crate) async fn observe_plain_finalization_failure(
    http: &reqwest::Client,
    url: &str,
    token: &str,
) -> Result<Option<ControllerResult>> {
    let value = wait_value(http, url, token, "").await?;
    Ok(plain_finalization_failure(&value))
}

async fn wait_result(
    http: &reqwest::Client,
    url: &str,
    token: &str,
    query: &str,
) -> Result<ControllerResult> {
    let value = wait_value(http, url, token, query).await?;
    let attempt = url.rsplit('/').next().unwrap_or_default();
    terminal_result(&value, attempt)
}

async fn wait_value(http: &reqwest::Client, url: &str, token: &str, query: &str) -> Result<Value> {
    let wait_url = if query.is_empty() {
        format!("{url}/wait")
    } else {
        format!("{url}/wait?{query}")
    };
    let response = http
        .post(wait_url)
        .bearer_auth(token)
        .send()
        .await
        .map_err(|_| Error::unavailable("Could not wait for execution."))?;
    if !response.status().is_success() {
        return Err(Error::unavailable("Could not wait for execution."));
    }
    response
        .json()
        .await
        .map_err(|_| Error::unavailable("Execution completion connection was interrupted."))
}

fn terminal_result(value: &Value, attempt: &str) -> Result<ControllerResult> {
    let code = value["StatusCode"]
        .as_i64()
        .filter(|n| (0..=255).contains(n))
        .map(|n| n as i32)
        .ok_or_else(|| Error::bad("Invalid execution exit status."))?;
    let cleanup = cleanup_result(value, attempt);
    let retired = cleanup.is_some() && value["NativeCleanup"]["retired"] == true;
    let finalization_failed =
        value["FinalizationFailed"] == true && code == super::CONTROLLER_FAILED && !retired;
    let physical_finalized =
        retired || (value["PhysicalFinalized"] == true && value["FinalizationFailed"] != true);
    let terminal_reason = if code == super::CONTROLLER_FAILED && physical_finalized && !retired {
        serde_json::from_value(value["TerminalReason"].clone()).ok()
    } else {
        None
    };
    let native_cleanup = if retired {
        Some("Native disk cleanup completed. The attempt's exit status is unknown, and automatic replay was stopped.".into())
    } else {
        cleanup.and_then(|result| result.native_cleanup)
    };
    Ok(ControllerResult {
        native_cleanup,
        code,
        finalization_failed,
        physical_finalized,
        terminal_reason,
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

    #[test]
    fn plain_finalization_failure_rejects_structured_cleanup_receipts() {
        let mut receipt = json!({
            "StatusCode": super::super::CONTROLLER_FAILED,
            "FinalizationFailed": true,
        });
        assert!(plain_finalization_failure(&receipt).is_some());
        receipt["NativeCleanup"] = json!({
            "attemptId": "different-attempt",
            "workloadFenced": true,
            "message": "Pending Native cleanup",
        });
        assert!(plain_finalization_failure(&receipt).is_none());
    }

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
        let (message, capacity) = startup_rejection(
            StatusCode::CONFLICT,
            Some("Native workspace is still active."),
        );
        assert!(message.starts_with("Execution controller rejected startup"));
        assert!(message.ends_with("Native workspace is still active."));
        assert!(!capacity);
        for reason in [
            "All VM slots are occupied.",
            "The native execution slot is occupied.",
        ] {
            let (message, capacity) =
                startup_rejection(StatusCode::SERVICE_UNAVAILABLE, Some(reason));
            assert!(message.starts_with(crate::nodes::placement::NO_CAPACITY));
            assert!(capacity);
        }
        let (_, capacity) = startup_rejection(
            StatusCode::SERVICE_UNAVAILABLE,
            Some("VM controller unavailable"),
        );
        assert!(!capacity);
    }
}

#[cfg(test)]
mod physical_result_tests {
    use super::*;

    #[test]
    fn cleanup_evidence_is_attempt_bound_and_never_physical_finalization() {
        let attempt = crate::config::id();
        let mut value = json!({"StatusCode":70,"FinalizationFailed":true,"NativeCleanup":{"attemptId":attempt,"workloadFenced":true,"message":"disk held by valkey pid 42"}});
        let result = cleanup_result(&value, &attempt).unwrap();
        assert!(!result.physical_finalized);
        assert_eq!(
            result.native_cleanup.as_deref(),
            Some("disk held by valkey pid 42")
        );
        assert!(cleanup_result(&value, &crate::config::id()).is_none());
        value["PhysicalFinalized"] = true.into();
        assert!(cleanup_result(&value, &attempt).is_none());
        value["PhysicalFinalized"] = false.into();
        value["NativeCleanup"]["workloadFenced"] = false.into();
        assert!(cleanup_result(&value, &attempt).is_none());
    }

    #[tokio::test]
    async fn terminal_reason_requires_a_closed_value_and_failed_physically_finalized_status() {
        use axum::{Router, routing::post};
        for (value, expected) in [
            (
                json!({
                    "StatusCode": 70,
                    "PhysicalFinalized": true,
                    "TerminalReason": "native_output_limit",
                    "futureField": true
                }),
                Some(TerminalReason::NativeOutputLimit),
            ),
            (
                json!({
                    "StatusCode": 70,
                    "PhysicalFinalized": true
                }),
                None,
            ),
            (
                json!({
                    "StatusCode": 70,
                    "PhysicalFinalized": true,
                    "TerminalReason": "future_reason"
                }),
                None,
            ),
            (
                json!({
                    "StatusCode": 70,
                    "PhysicalFinalized": true,
                    "TerminalReason": {
                        "reason": "native_output_limit"
                    }
                }),
                None,
            ),
            (
                json!({
                    "StatusCode": 70,
                    "TerminalReason": "native_output_limit"
                }),
                None,
            ),
            (
                json!({
                    "StatusCode": 0,
                    "PhysicalFinalized": true,
                    "TerminalReason": "native_output_limit"
                }),
                None,
            ),
            (
                json!({
                    "StatusCode": 143,
                    "PhysicalFinalized": true,
                    "TerminalReason": "native_output_limit"
                }),
                None,
            ),
            (
                json!({
                    "StatusCode": 70,
                    "PhysicalFinalized": true,
                    "FinalizationFailed": true,
                    "TerminalReason": "native_output_limit"
                }),
                None,
            ),
        ] {
            let code = value["StatusCode"].as_i64().unwrap() as i32;
            let app = Router::new().route(
                "/wait",
                post(move || {
                    let value = value.clone();
                    async move { axum::Json(value) }
                }),
            );
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
            let result = wait_for_exit(
                &reqwest::Client::new(),
                &format!("http://{address}"),
                "fixture",
            )
            .await;
            server.abort();
            let result = result.unwrap();
            assert_eq!(result.code, code);
            assert_eq!(result.terminal_reason, expected);
        }
    }

    #[tokio::test]
    async fn exit_code_and_legacy_markers_never_substitute_for_a_physical_certificate() {
        use axum::{Router, routing::post};
        for (value, confirmed) in [
            (
                json!({
                    "StatusCode": 70,
                    "FinalizationFailed": true,
                    "NativeCleanup": {
                        "attemptId": "another-attempt",
                        "workloadFenced": true,
                        "message": "Pending cleanup",
                        "retired": true
                    }
                }),
                false,
            ),
            (
                json!({
                    "StatusCode": 70,
                    "FinalizationFailed": true,
                    "NativeCleanup": {
                        "attemptId": "fixture",
                        "workloadFenced": false,
                        "message": "Pending cleanup",
                        "retired": true
                    }
                }),
                false,
            ),
            (json!({"StatusCode":0}), false),
            (json!({"StatusCode":0,"PhysicalFinalized":false}), false),
            (json!({"StatusCode":17,"PhysicalFinalized":true}), true),
            (json!({"StatusCode":143,"PhysicalFinalized":true}), true),
            (
                json!({"StatusCode":70,"PhysicalFinalized":true,"FinalizationFailed":true}),
                false,
            ),
        ] {
            let expected_failure = value["StatusCode"] == super::super::CONTROLLER_FAILED
                && value["FinalizationFailed"] == true;
            let app = Router::new().route(
                "/runs/fixture/wait",
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
                &format!("http://{address}/runs/fixture"),
                "fixture",
            )
            .await
            .unwrap();
            assert_eq!(result.physical_finalized, confirmed);
            assert_eq!(result.finalization_failed, expected_failure);
            assert!(result.native_cleanup.is_none());
            server.abort();
        }
    }

    #[tokio::test]
    async fn provider_turn_can_start_after_a_slow_filesystem_preparation() {
        use axum::{Json, Router, body::Body, response::IntoResponse, routing::get, routing::post};

        let attempt = crate::config::id();
        let turn = crate::config::id();
        let prepared_at = tokio::time::Instant::now() + Duration::from_secs(35);
        let environment_at = prepared_at - Duration::from_secs(2);
        let receipt = json!({
            "attemptId": attempt,
            "generation": "fixture",
            "turnId": turn,
            "code": 0,
            "logStart": 0,
            "logEnd": 0,
        });
        let app = Router::new()
            .route("/runs/{attempt}", post(|| async { StatusCode::OK }))
            .route(
                "/runs/{attempt}/turn/{turn}/start",
                get(move || async move {
                    let now = tokio::time::Instant::now();
                    if now < environment_at {
                        return StatusCode::SERVICE_UNAVAILABLE.into_response();
                    }
                    if now < prepared_at {
                        return (StatusCode::ACCEPTED, Json(json!({"pending": true})))
                            .into_response();
                    }
                    Json(json!({"offset": 0})).into_response()
                }),
            )
            .route(
                "/runs/{attempt}/turn/{turn}",
                get(move || async move { Json(receipt) }),
            )
            .route(
                "/runs/{attempt}/logs",
                get(|| async {
                    Body::from_stream(futures_util::stream::pending::<
                        std::result::Result<Vec<u8>, std::io::Error>,
                    >())
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let controller = Controller {
            http: reqwest::Client::new(),
            url: format!("http://{address}/runs/{attempt}"),
            token: "fixture".into(),
        };
        let result = controller.run_turn(&turn).await;
        server.abort();

        assert_eq!(result.unwrap(), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn completed_turn_does_not_wait_forever_for_missing_log_bytes() {
        use crate::background_jobs::controller::TurnReceipt;
        use axum::{Router, body::Body, routing::get};
        let app = Router::new().route(
            "/logs",
            get(|| async {
                Body::from_stream(futures_util::stream::pending::<
                    std::result::Result<Vec<u8>, std::io::Error>,
                >())
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let controller = Controller {
            http: reqwest::Client::new(),
            url: format!("http://{address}"),
            token: "fixture".into(),
        };
        let completion = async {
            Ok(TurnReceipt {
                attempt_id: "a".into(),
                generation: "g".into(),
                turn_id: "t".into(),
                code: 0,
                log_start: 0,
                log_end: 1,
            })
        };
        tokio::pin!(completion);
        let result = tokio::time::timeout(
            Duration::from_secs(60),
            controller.relay_turn(0, &mut completion, &mut Vec::new(), &mut Vec::new()),
        )
        .await;
        assert!(result.unwrap().is_err());
        server.abort();
    }

    #[tokio::test]
    async fn turn_completion_during_a_partial_log_frame_preserves_output() {
        use crate::background_jobs::controller::TurnReceipt;
        use axum::{Router, body::Body, routing::get};
        use tokio::io::AsyncReadExt;
        let packet = format!(
            "{}\n",
            json!({
                "offset": 17,
                "event": { "type": "output", "stderr": false, "data": STANDARD.encode(b"hello") }
            })
        )
        .into_bytes();
        let half = packet.len() / 2;
        let (first, second) = (packet[..half].to_vec(), packet[half..].to_vec());
        let ready = std::sync::Arc::new(tokio::sync::Notify::new());
        let completed = std::sync::Arc::new(tokio::sync::Notify::new());
        let (r, c) = (ready.clone(), completed.clone());
        let app = Router::new().route(
            "/logs",
            get(move || {
                let (first, second, ready, completed) =
                    (first.clone(), second.clone(), r.clone(), c.clone());
                async move {
                    let stream = futures_util::stream::unfold(0, move |index| {
                        let (first, second, ready, completed) = (
                            first.clone(),
                            second.clone(),
                            ready.clone(),
                            completed.clone(),
                        );
                        async move {
                            match index {
                                0 => Some((Ok::<_, std::io::Error>(first), 1)),
                                1 => {
                                    ready.notify_one();
                                    completed.notified().await;
                                    Some((Ok(second), 2))
                                }
                                _ => None,
                            }
                        }
                    });
                    Body::from_stream(stream)
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
            url: format!("http://{address}"),
            token: "fixture".into(),
        };
        let completion = async move {
            ready.notified().await;
            completed.notify_one();
            Ok(TurnReceipt {
                attempt_id: "attempt".into(),
                generation: "generation".into(),
                turn_id: "turn".into(),
                code: 0,
                log_start: 0,
                log_end: 17,
            })
        };
        tokio::pin!(completion);
        let (mut output, mut reader) = tokio::io::duplex(64);
        let mut errors = Vec::new();
        let code = tokio::time::timeout(
            Duration::from_secs(3),
            controller.relay_turn(0, &mut completion, &mut output, &mut errors),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(code, 0);
        drop(output);
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes).await.unwrap();
        assert_eq!(bytes, b"hello");
        assert!(errors.is_empty());
        server.abort();
    }
}
