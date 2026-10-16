//! Bounded process supervision. Installation supplies authorized host operations.
use cairn_plugin_api::{API_VERSION, Frame, MAX_FRAME_BYTES, MAX_SOURCE_BYTES, Manifest, REJECTED};
use serde_json::Value;
use std::{future::Future, path::Path, process::Stdio, sync::Arc, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, ChildStdout, Command},
    sync::{OwnedSemaphorePermit, Semaphore},
};

/// A refused host call. Only `code` reaches the plugin; keep details in the host.
pub struct Denied(pub &'static str);

pub struct Host {
    slots: Arc<Semaphore>,
    deadline: Duration,
}

impl Default for Host {
    fn default() -> Self {
        Self::with_limits(32, Duration::from_secs(35))
    }
}

impl Host {
    /// `concurrency` bounds live workers; `deadline` bounds loading and each event.
    pub fn with_limits(concurrency: usize, deadline: Duration) -> Self {
        Self {
            slots: Arc::new(Semaphore::new(concurrency)),
            deadline,
        }
    }

    /// Starts a long-lived worker with an empty environment and private directory,
    /// then loads the package. The session ends when dropped or stopped.
    pub async fn start(
        &self,
        executable: &Path,
        manifest: &Manifest,
        source: &str,
    ) -> Result<Session, String> {
        manifest.validate().map_err(str::to_owned)?;
        if source.len() > MAX_SOURCE_BYTES {
            return Err("Plugin source is too large.".into());
        }
        let permit = self
            .slots
            .clone()
            .try_acquire_owned()
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
        let input = child.stdin.take().ok_or("Plugin input unavailable.")?;
        let output = BufReader::new(child.stdout.take().ok_or("Plugin output unavailable.")?);
        let mut session = Session {
            child,
            input,
            output,
            manifest: manifest.clone(),
            deadline: self.deadline,
            broken: false,
            _permit: permit,
            _directory: directory,
        };
        let deadline_ms = self.deadline.as_millis() as u64;
        let loaded = tokio::time::timeout(self.deadline, async {
            session
                .write(&Frame::Load {
                    api_version: API_VERSION,
                    source: source.to_owned(),
                    deadline_ms,
                })
                .await?;
            match session.read().await? {
                Frame::Ready => Ok(()),
                _ => Err("Plugin package failed to load.".to_owned()),
            }
        })
        .await
        .unwrap_or_else(|_| Err("Plugin deadline exceeded.".into()));
        if let Err(error) = loaded {
            session.stop().await;
            return Err(error);
        }
        Ok(session)
    }

    /// One-shot invocation (commands, tools, views, actions, tests): load, run one
    /// handler, stop.
    pub async fn invoke<F, Fut>(
        &self,
        executable: &Path,
        manifest: &Manifest,
        source: &str,
        event: &str,
        input: Value,
        call: F,
    ) -> Result<Value, String>
    where
        F: FnMut(String, Value) -> Fut,
        Fut: Future<Output = Result<Value, Denied>>,
    {
        let mut session = self.start(executable, manifest, source).await?;
        let result = session.dispatch(event, input, call).await;
        session.stop().await;
        result
    }
}

pub struct Session {
    child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
    manifest: Manifest,
    deadline: Duration,
    broken: bool,
    _permit: OwnedSemaphorePermit,
    _directory: tempfile::TempDir,
}

impl Session {
    /// False once the worker exited, overran a deadline or violated the protocol.
    /// A broken session must be replaced; its script state is gone.
    pub fn alive(&self) -> bool {
        !self.broken
    }

    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    /// Runs one handler. Calls outside the manifest's capabilities end the session;
    /// refused calls are returned to the script as `Denied` codes.
    pub async fn dispatch<F, Fut>(
        &mut self,
        event: &str,
        payload: Value,
        mut call: F,
    ) -> Result<Value, String>
    where
        F: FnMut(String, Value) -> Fut,
        Fut: Future<Output = Result<Value, Denied>>,
    {
        if self.broken {
            return Err("Plugin worker stopped.".into());
        }
        let manifest = self.manifest.clone();
        let deadline = self.deadline;
        let operation = async {
            self.write(&Frame::Event {
                name: event.to_owned(),
                payload,
            })
            .await?;
            for _ in 0..100 {
                match self.read().await? {
                    Frame::Call { method, args } => {
                        if !manifest.permits(&method) {
                            return Err("Plugin capability is not granted.".to_owned());
                        }
                        let frame = match call(method, args).await {
                            Ok(value) => Frame::Reply { value },
                            Err(Denied(code)) => Frame::Denied { code: code.into() },
                        };
                        self.write(&frame).await?;
                    }
                    Frame::Done { value } => return Ok(Ok(value)),
                    Frame::Failure => return Ok(Err("Plugin execution failed.".to_owned())),
                    Frame::Rejected { message } => {
                        let message = message
                            .chars()
                            .filter(|c| !c.is_control())
                            .take(300)
                            .collect::<String>();
                        return Ok(Err(format!("{REJECTED}{message}")));
                    }
                    _ => return Err("Invalid plugin response.".to_owned()),
                }
            }
            Err("Plugin operation limit exceeded.".to_owned())
        };
        match tokio::time::timeout(deadline, operation).await {
            // A handler error leaves the worker usable for the next event.
            Ok(Ok(result)) => result,
            Ok(Err(error)) => {
                self.broken = true;
                Err(error)
            }
            Err(_) => {
                self.broken = true;
                Err("Plugin deadline exceeded.".into())
            }
        }
    }

    /// Always terminates and reaps the worker, including stuck scripts.
    pub async fn stop(mut self) {
        let _ = self.child.kill().await;
        let _ = self.child.wait().await;
    }

    async fn write(&mut self, frame: &Frame) -> Result<(), String> {
        let mut bytes = serde_json::to_vec(frame).map_err(|_| "Invalid plugin input.")?;
        if bytes.len() >= MAX_FRAME_BYTES {
            return Err("Plugin frame is too large.".into());
        }
        bytes.push(b'\n');
        self.input
            .write_all(&bytes)
            .await
            .map_err(|_| "Plugin input interrupted.".into())
    }

    async fn read(&mut self) -> Result<Frame, String> {
        let mut bytes = Vec::new();
        (&mut self.output)
            .take(MAX_FRAME_BYTES as u64 + 1)
            .read_until(b'\n', &mut bytes)
            .await
            .map_err(|_| "Plugin output interrupted.")?;
        if bytes.len() > MAX_FRAME_BYTES || bytes.last() != Some(&b'\n') {
            return Err("Invalid plugin response.".into());
        }
        serde_json::from_slice(&bytes).map_err(|_| "Invalid plugin response.".into())
    }
}
