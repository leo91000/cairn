//! Guest-side `runner` client: starts an attempt and relays its output and exit.
use super::CONTROLLER_INTERRUPTED;
use crate::{
    error::{Error, Result},
    microvm::{protocol::Event, wire},
    validation::uuid,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use futures_util::StreamExt;
use serde_json::Value;
use std::time::Duration;
use tokio::io::{AsyncWriteExt, BufReader};
use tokio_util::sync::CancellationToken;

/// Exit code reported when this client is asked to stop.
const STOPPED: i32 = 143;

struct Controller {
    http: reqwest::Client,
    url: String,
    token: String,
}

impl Controller {
    async fn start(&self) -> Result<()> {
        let response = self
            .http
            .post(&self.url)
            .bearer_auth(&self.token)
            .timeout(Duration::from_secs(30))
            .send()
            .await
            .map_err(|_| Error::unavailable("VM controller could not start the run."))?;
        if !response.status().is_success() {
            return Err(Error::unavailable("VM controller could not start the run."));
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
            .map_err(|_| Error::unavailable("Could not read VM output."))?;
        if !logs.status().is_success() {
            return Err(Error::unavailable("Could not read VM output."));
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
            .map_err(|_| Error::unavailable("Could not wait for VM."))?;
        let value: Value = response
            .json()
            .await
            .map_err(|_| Error::unavailable("VM completion connection was interrupted."))?;
        value["StatusCode"]
            .as_i64()
            .filter(|n| (0..=255).contains(n))
            .map(|n| n as i32)
            .ok_or_else(|| Error::bad("Invalid VM exit status."))
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
                Error::unavailable("VM output connection was interrupted.")
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
        } = wire::decode(event, "Invalid VM output.")?
        else {
            continue;
        };
        let bytes = STANDARD
            .decode(data)
            .map_err(|_| Error::bad("Invalid VM output."))?;
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
