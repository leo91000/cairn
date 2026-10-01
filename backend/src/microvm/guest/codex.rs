//! The guest owns a private, unprivileged service for its entire VM lifetime.
//! Attempt output is attached only while a runner owns that service; idle logs
//! are drained without retaining an account or an unbounded output buffer.
use super::{AGENT_ID, AUTH_SOCKET, forward_event, unprivileged};
use crate::{
    error::{Error, Result},
    microvm::{plan::ENTRYPOINT, protocol::Event},
};
use std::{
    process::Stdio,
    sync::{
        Arc, Mutex as StdMutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, BufReader},
    process::{Child, Command},
    sync::{Mutex, mpsc, oneshot},
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;

pub(super) const SOCKET: &str = "/run/leo-codex/service.sock";
const START_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Clone)]
struct Output {
    events: mpsc::Sender<Event>,
    stop: CancellationToken,
}

type Sink = Arc<StdMutex<Option<Output>>>;

/// Detaching also cancels a pending send, so draining an exited runner cannot
/// wait forever on the still-live service's output sender.
pub(super) struct OutputLease(Sink);

impl Drop for OutputLease {
    fn drop(&mut self) {
        if let Some(output) = self.0.lock().unwrap().take() {
            output.stop.cancel();
        }
    }
}

struct Service {
    stop: CancellationToken,
    task: JoinHandle<Result<()>>,
}

impl Drop for Service {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}

#[derive(Default)]
pub(super) struct Codex {
    service: Mutex<Option<Service>>,
    started: AtomicBool,
    ready: Arc<AtomicBool>,
    output: Sink,
}

impl Codex {
    pub fn ready(&self) -> bool {
        self.ready.load(Ordering::Acquire)
    }

    pub fn socket(&self) -> Result<Option<&'static str>> {
        if !self.started.load(Ordering::Acquire) {
            return Ok(None);
        }
        if !self.ready() {
            return Err(Error::unavailable("Guest Codex service stopped."));
        }
        Ok(Some(SOCKET))
    }

    /// The caller serializes warmup with running attempts. No caller account,
    /// storage authority, or MCP environment is inherited by this process.
    pub async fn warm(&self, stop: CancellationToken) -> Result<()> {
        let directory = std::path::Path::new(SOCKET).parent().unwrap();
        crate::skills::private_dir(directory).await?;
        std::os::unix::fs::chown(directory, Some(AGENT_ID), Some(AGENT_ID))?;
        let environment = crate::toolkit::toolchain_environment(
            std::path::Path::new("/home/node"),
            crate::process::Environment::from([
                ("CODEX_HOME".into(), "/home/node/.codex".into()),
                ("LEO_AUTH_SOCKET".into(), AUTH_SOCKET.into()),
                ("LEO_TOOLKIT_DIR".into(), "/opt/leo-toolkit".into()),
                ("PNPM_HOME".into(), "/pnpm".into()),
                (
                    "PATH".into(),
                    "/usr/local/bin:/pnpm/bin:/pnpm:/usr/bin:/bin:/usr/sbin:/sbin".into(),
                ),
            ]),
        );
        let mut command = Command::new(ENTRYPOINT);
        command
            .args(["codex-service", SOCKET])
            .env_clear()
            .envs(environment)
            .current_dir("/home/node");
        unprivileged(&mut command);
        self.start(command, stop, START_TIMEOUT).await
    }

    async fn start(
        &self,
        mut command: Command,
        stop: CancellationToken,
        timeout: Duration,
    ) -> Result<()> {
        let mut service = self.service.lock().await;
        if self.started.swap(true, Ordering::AcqRel) {
            return self.socket().map(|_| ());
        }
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let child = command.spawn()?;
        let (notify, readiness) = oneshot::channel();
        let stopping = stop.child_token();
        let task = tokio::spawn(supervise(
            child,
            self.ready.clone(),
            self.output.clone(),
            stopping.clone(),
            timeout,
            notify,
        ));
        // Retain ownership before waiting: cancellation of this warm request
        // cannot orphan a service in the guest or release its running process.
        *service = Some(Service {
            stop: stopping,
            task,
        });
        readiness
            .await
            .map_err(|_| Error::unavailable("Guest Codex initialization stopped."))?
    }

    pub fn attach(&self, events: mpsc::Sender<Event>) -> Result<OutputLease> {
        if self.socket()?.is_none() {
            return Err(Error::unavailable(
                "Guest Codex service was not initialized.",
            ));
        }
        let mut output = self.output.lock().unwrap();
        if output.is_some() {
            return Err(Error::conflict("Guest Codex output is already leased."));
        }
        *output = Some(Output {
            events,
            stop: CancellationToken::new(),
        });
        Ok(OutputLease(self.output.clone()))
    }

    pub async fn close(&self) {
        self.ready.store(false, Ordering::Release);
        let Some(mut service) = self.service.lock().await.take() else {
            return;
        };
        service.stop.cancel();
        let _ = (&mut service.task).await;
    }
}

async fn supervise(
    mut child: Child,
    ready: Arc<AtomicBool>,
    output: Sink,
    stop: CancellationToken,
    timeout: Duration,
    notify: oneshot::Sender<Result<()>>,
) -> Result<()> {
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let draining = CancellationToken::new();
    let stderr = tokio::spawn(drain(
        child.stderr.take().unwrap(),
        output,
        draining.clone(),
    ));
    let started = tokio::select! {
        () = stop.cancelled() => Err(Error::unavailable("Guest Codex warmup was cancelled.")),
        result = tokio::time::timeout(timeout, read_ready(&mut stdout)) => {
            result.unwrap_or_else(|_| Err(Error::gateway_timeout("Guest Codex warmup timed out.")))
        },
    };
    ready.store(started.is_ok(), Ordering::Release);
    let initialized = started.is_ok();
    let owner_present = notify.send(started).is_ok();
    let result = if initialized && owner_present {
        tokio::select! {
            () = stop.cancelled() => Ok(()),
            result = child.wait() => {
                result.map_err(Error::from).and_then(|_| Err(Error::unavailable("Guest Codex service exited.")))
            },
            _ = stdout.read_u8() => Err(Error::unavailable("Guest Codex readiness stream closed.")),
        }
    } else {
        Err(Error::unavailable("Guest Codex warmup failed."))
    };
    ready.store(false, Ordering::Release);
    // SIGTERM lets the Rust adapter close and reap its native process. Killing
    // only the adapter on request cancellation would leave native work running.
    terminate(&mut child).await;
    draining.cancel();
    let _ = stderr.await;
    result
}

async fn read_ready(stdout: &mut (impl AsyncRead + Unpin)) -> Result<()> {
    let mut bytes = Vec::new();
    for _ in 0..64 {
        let byte = stdout.read_u8().await?;
        bytes.push(byte);
        if byte == b'\n' {
            break;
        }
    }
    if bytes != b"{\"ready\":true}\n" {
        return Err(Error::unavailable("Invalid guest Codex readiness message."));
    }
    Ok(())
}

async fn terminate(child: &mut Child) {
    if let Some(pid) = child.id() {
        unsafe { libc::kill(pid as i32, libc::SIGTERM) };
    }
    if !matches!(
        tokio::time::timeout(Duration::from_secs(3), child.wait()).await,
        Ok(Ok(_))
    ) {
        if let Some(pid) = child.id() {
            // The service owns a process group, including native Codex. Never
            // leave that native process behind after an unresponsive adapter.
            unsafe { libc::kill(-(pid as i32), libc::SIGKILL) };
        }
        let _ = child.kill().await;
    }
}

async fn drain(mut stderr: impl AsyncRead + Unpin, output: Sink, stop: CancellationToken) {
    let mut buffer = vec![0; 32768];
    loop {
        let count = tokio::select! {
            () = stop.cancelled() => return,
            result = stderr.read(&mut buffer) => match result {
                Ok(0) | Err(_) => return,
                Ok(count) => count,
            },
        };
        let current = output.lock().unwrap().clone();
        let Some(current) = current else {
            continue;
        };
        tokio::select! {
            () = stop.cancelled() => return,
            () = current.stop.cancelled() => {},
            _ = current.events.send(forward_event(true, &buffer[..count])) => {},
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(script: &str) -> Command {
        let mut command = Command::new("sh");
        command.args(["-c", script]);
        command
    }

    #[tokio::test]
    async fn exited_runner_finishes_while_native_service_remains_ready() {
        let codex = Codex::default();
        codex
            .start(
                fixture("printf '{\"ready\":true}\\n'; exec sleep 60"),
                CancellationToken::new(),
                Duration::from_secs(2),
            )
            .await
            .unwrap();
        let (sender, events) = mpsc::channel(1);
        let lease = codex.attach(sender).unwrap();
        let runner = Command::new("true").spawn().unwrap();
        let mut output = Vec::new();
        let status = tokio::time::timeout(
            Duration::from_secs(1),
            super::super::wait_agent(&mut output, runner, events, Some(lease)),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(status.success());
        assert!(
            codex.ready(),
            "Completing an attempt retains the VM's initialized native process"
        );
        codex.close().await;
    }

    #[tokio::test]
    async fn service_requires_native_ready_and_is_reaped_on_guest_shutdown() {
        let root = tempfile::tempdir().unwrap();
        let pid = root.path().join("pid");
        let script = format!(
            "echo $$ > '{}'; printf '{{\"ready\":true}}\\n'; exec sleep 60",
            pid.display()
        );
        let codex = Codex::default();
        codex
            .start(
                fixture(&script),
                CancellationToken::new(),
                Duration::from_secs(2),
            )
            .await
            .unwrap();
        assert!(codex.ready());
        assert_eq!(codex.socket().unwrap(), Some(SOCKET));
        let pid: i32 = std::fs::read_to_string(pid)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        codex.close().await;
        assert!(!codex.ready());
        assert!(
            codex.socket().is_err(),
            "An initialized service never falls back to cold Codex"
        );
        assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
    }

    #[tokio::test]
    async fn unready_or_invalid_services_cannot_be_claimed() {
        for script in ["exec sleep 60", "printf 'invalid\\n'"] {
            let codex = Codex::default();
            assert!(
                codex
                    .start(
                        fixture(script),
                        CancellationToken::new(),
                        Duration::from_millis(30)
                    )
                    .await
                    .is_err()
            );
            assert!(!codex.ready());
            assert!(codex.socket().is_err());
            tokio::time::timeout(Duration::from_secs(1), codex.close())
                .await
                .unwrap();
        }
    }

    #[tokio::test]
    async fn detaching_a_full_output_queue_releases_pending_service_senders() {
        let codex = Codex::default();
        codex.started.store(true, Ordering::Release);
        codex.ready.store(true, Ordering::Release);
        let (sender, mut events) = mpsc::channel(1);
        sender.send(forward_event(true, b"existing")).await.unwrap();
        let lease = codex.attach(sender).unwrap();
        let output = codex.output.clone();
        let draining = tokio::spawn(async move {
            drain(
                b"resident timing".as_slice(),
                output,
                CancellationToken::new(),
            )
            .await;
        });
        tokio::task::yield_now().await;
        drop(lease);
        tokio::time::timeout(Duration::from_secs(1), draining)
            .await
            .unwrap()
            .unwrap();
        assert!(events.recv().await.is_some());
        assert!(events.recv().await.is_none());
    }
}
