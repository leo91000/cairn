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
use std::time::Duration;
use tokio::io::{AsyncWriteExt, BufReader};
use tokio_util::sync::CancellationToken;

/// Exit code reported when this client is asked to stop.
const STOPPED: i32 = 143;
const ERROR_BODY_LIMIT: usize = 10_000;

struct Controller {
    http: reqwest::Client,
    url: String,
    token: String,
}

/// Older controllers report admission pressure as a plain 503 error. Normalize
/// only their known capacity refusals into the manager's existing wait contract.
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
        let response = self
            .http
            .post(format!("{}/wait", self.url))
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(|_| Error::unavailable("Could not wait for execution."))?;
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

    async fn run(&self) -> Result<i32> {
        self.start().await?;
        let logs = self.logs().await?;
        let ((), code) = tokio::try_join!(relay_output(logs), self.wait())?;
        Ok(code)
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
    controller.stop().await;
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
