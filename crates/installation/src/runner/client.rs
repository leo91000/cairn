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
/// Only recovery/draining is bounded; an intact attempt may run normally.
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

    async fn logs(&self) -> Result<reqwest::Response> {
        let logs = self
            .http
            .get(format!("{}/logs", self.url))
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(|_| Error::unavailable("Could not read execution output."))?;
        if !logs.status().is_success() {
            return Err(Error::unavailable("Could not read execution output."));
        }
        Ok(logs)
    }

    async fn wait(&self) -> Result<i32> {
        wait_for_exit(&self.http, &self.url, &self.token).await
    }

    async fn run(&self) -> Result<i32> {
        self.start().await?;
        let output = async { relay_output(self.logs().await?).await };
        let (output, status) = collect_completion(output, self.wait()).await;
        if let Err(error) = output {
            eprintln!("{}", error.message);
        }
        // Preserve the last complete frames even when the next one was cut.
        let _ = tokio::io::stdout().flush().await;
        let _ = tokio::io::stderr().flush().await;
        match status {
            Ok(code) => Ok(code),
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

/// Keep both observations even when one fails. Once either channel closes,
/// allow the other a bounded drain, without reconnecting or replaying logs.
async fn collect_completion(
    output: impl Future<Output = Result<()>>,
    status: impl Future<Output = Result<i32>>,
) -> (Result<()>, Result<i32>) {
    tokio::pin!(output, status);
    tokio::select! {
        output = &mut output => {
            let status = tokio::time::timeout(RECONCILE_TIMEOUT, status).await
                .unwrap_or_else(|_| Err(Error::unavailable("Execution completion is not confirmed.")));
            (output, status)
        }
        status = &mut status => {
            let output = tokio::time::timeout(RECONCILE_TIMEOUT, output).await
                .unwrap_or_else(|_| Err(Error::unavailable("Execution output drain was interrupted.")));
            (output, status)
        }
    }
}

/// Read only the terminal result of this exact attempt. Both controller backends
/// publish it after finalization and physical ownership release.
pub(crate) async fn wait_for_exit(http: &reqwest::Client, url: &str, token: &str) -> Result<i32> {
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
    value["StatusCode"]
        .as_i64()
        .filter(|n| (0..=255).contains(n))
        .map(|n| n as i32)
        .ok_or_else(|| Error::bad("Invalid execution exit status."))
}

/// Writes attempt output to this process's stdout and stderr.
async fn relay_output(logs: reqwest::Response) -> Result<()> {
    let stream = logs
        .bytes_stream()
        .map(|r| r.map_err(std::io::Error::other));
    let mut reader = BufReader::new(tokio_util::io::StreamReader::new(stream));
    let mut stdout = tokio::io::stdout();
    let mut stderr = tokio::io::stderr();
    loop {
        let event = wire::read(&mut reader).await.map_err(|error| {
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
    async fn a_cut_before_completion_is_bounded_and_never_claims_success() {
        let started = tokio::time::Instant::now();
        let (output, status) = collect_completion(
            async { Err(Error::unavailable("fixture output cut")) },
            std::future::pending(),
        )
        .await;
        assert!(output.is_err());
        assert!(status.is_err());
        assert_eq!(started.elapsed(), RECONCILE_TIMEOUT);
        let (output, status) = collect_completion(std::future::pending(), async { Ok(0) }).await;
        assert!(output.is_err());
        assert_eq!(status.unwrap(), 0);
    }

    #[tokio::test]
    async fn failed_initial_wait_does_not_issue_unreserved_reads() {
        use axum::{Router, routing::{get, post}};
        use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};
        let requests = Arc::new(AtomicUsize::new(0));
        let count = requests.clone();
        let app = Router::new()
            .route("/runs/fixture", post(|| async {}))
            .route("/runs/fixture/logs", get(|| async { "" }))
            .route("/runs/fixture/wait", post(move || {
                let count = count.clone();
                async move {
                    count.fetch_add(1, Ordering::SeqCst);
                    StatusCode::SERVICE_UNAVAILABLE
                }
            }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap(); });
        let controller = Controller {
            http: reqwest::Client::new(),
            url: format!("http://{address}/runs/fixture"),
            token: "fixture".into(),
        };
        assert!(controller.run().await.unwrap_err().is_unavailable());
        assert_eq!(requests.load(Ordering::SeqCst), 1);
        server.abort();
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
