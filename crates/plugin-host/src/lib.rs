//! Bounded process supervision. Installation supplies authorized host operations.
use cairn_plugin_api::{API_VERSION, Frame, MAX_FRAME_BYTES, MAX_SOURCE_BYTES, Manifest};
use serde_json::Value;
use std::{future::Future, path::Path, process::Stdio, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    process::Command,
    sync::Semaphore,
};

pub struct Host {
    slots: Semaphore,
    deadline: Duration,
}

impl Default for Host {
    fn default() -> Self {
        Self {
            slots: Semaphore::new(8),
            deadline: Duration::from_secs(35),
        }
    }
}

impl Host {
    pub fn with_limits(concurrency: usize, deadline: Duration) -> Self {
        Self {
            slots: Semaphore::new(concurrency),
            deadline,
        }
    }

    pub async fn invoke<F, Fut>(
        &self,
        executable: &Path,
        manifest: &Manifest,
        source: &str,
        input: Value,
        mut call: F,
    ) -> Result<Value, String>
    where
        F: FnMut(String, Value) -> Fut,
        Fut: Future<Output = Result<Value, String>>,
    {
        manifest.validate().map_err(str::to_owned)?;
        if source.len() > MAX_SOURCE_BYTES {
            return Err("Plugin source is too large.".into());
        }
        let _permit = self
            .slots
            .try_acquire()
            .map_err(|_| "Plugin workers are busy.")?;
        let directory = tempfile::tempdir().map_err(|_| "Plugin workspace is unavailable.")?;
        let mut child = Command::new(executable)
            .arg("plugin-worker")
            .env_clear()
            .current_dir(directory.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|_| "Plugin worker could not start.")?;
        let mut input_pipe = child.stdin.take().ok_or("Plugin input unavailable.")?;
        let mut output = BufReader::new(child.stdout.take().ok_or("Plugin output unavailable.")?);
        let operation = async {
            write_frame(
                &mut input_pipe,
                &Frame::Invoke {
                    api_version: API_VERSION,
                    source: source.to_owned(),
                    input,
                },
            )
            .await?;
            for _ in 0..100 {
                let mut bytes = Vec::new();
                (&mut output)
                    .take(MAX_FRAME_BYTES as u64 + 1)
                    .read_until(b'\n', &mut bytes)
                    .await
                    .map_err(|_| "Plugin output interrupted.")?;
                if bytes.len() > MAX_FRAME_BYTES || bytes.last() != Some(&b'\n') {
                    return Err("Invalid plugin response.".into());
                }
                let frame: Frame =
                    serde_json::from_slice(&bytes).map_err(|_| "Invalid plugin response.")?;
                match frame {
                    Frame::Call { method, args } => {
                        if !manifest.permits(&method) {
                            return Err("Plugin capability is not granted.".into());
                        }
                        // Keep host errors in the host; they may carry private details.
                        let value = call(method, args).await?;
                        write_frame(&mut input_pipe, &Frame::Reply { value }).await?;
                    }
                    Frame::Finished { value } => return Ok(value),
                    _ => return Err("Plugin execution failed.".into()),
                }
            }
            Err("Plugin operation limit exceeded.".into())
        };
        let result = tokio::time::timeout(self.deadline, operation)
            .await
            .unwrap_or_else(|_| Err("Plugin deadline exceeded.".into()));
        // Always terminate and reap, including malformed output and stuck scripts.
        let _ = child.kill().await;
        let _ = child.wait().await;
        result
    }
}

async fn write_frame(
    writer: &mut (impl AsyncWriteExt + Unpin),
    frame: &Frame,
) -> Result<(), String> {
    let mut bytes = serde_json::to_vec(frame).map_err(|_| "Invalid plugin input.")?;
    if bytes.len() >= MAX_FRAME_BYTES {
        return Err("Plugin frame is too large.".into());
    }
    bytes.push(b'\n');
    writer
        .write_all(&bytes)
        .await
        .map_err(|_| "Plugin input interrupted.".into())
}
