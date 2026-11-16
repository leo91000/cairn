use std::{
    collections::HashMap,
    os::fd::AsRawFd,
    path::{Path, PathBuf},
    process::Stdio,
};

use serde::{Deserialize, Serialize};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::UnixStream,
    sync::watch,
};
use tokio_util::sync::CancellationToken;

use super::{Receipt, Request};
use crate::{
    error::{Error, Result},
    process::Environment,
};

const MAX_JOBS: usize = 64;
const RECEIPT_LIMIT: u64 = 128 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Outcome {
    Running,
    Finished {
        receipt: Receipt,
    },
    /// Helper failure cannot prove that its descendants have stopped. The
    /// environment still owns and must fence the workload before releasing it.
    Unknown {
        message: String,
    },
}

struct Job {
    id: String,
    name: String,
    command: String,
    result: watch::Receiver<Outcome>,
    stop: CancellationToken,
}

/// Owned by one admitted environment, never shared between run generations.
/// This value must be retained through provider detach and environment fencing.
pub struct Registry {
    executable: PathBuf,
    cwd: PathBuf,
    environment: Environment,
    deadline_boot_ms: u64,
    jobs: HashMap<String, Job>,
    stopped: CancellationToken,
    poisoned: CancellationToken,
}

impl Registry {
    pub fn new(
        executable: &Path,
        cwd: &Path,
        environment: Environment,
        deadline_boot_ms: u64,
    ) -> Self {
        Self {
            executable: executable.to_owned(),
            cwd: cwd.to_owned(),
            environment,
            deadline_boot_ms,
            jobs: HashMap::new(),
            stopped: CancellationToken::new(),
            poisoned: CancellationToken::new(),
        }
    }

    /// The caller supplies its stable tool-call identity. Replayed delivery
    /// returns the same job; changing the payload under that identity is refused.
    pub async fn start(&mut self, call_id: &str, name: &str, command: &str) -> Result<String> {
        if call_id.is_empty()
            || call_id.len() > 256
            || name.trim().is_empty()
            || name.len() > 256
            || command.is_empty()
            || command.len() > 128 * 1024
        {
            return Err(Error::bad("Invalid job name, command or call identity."));
        }
        if let Some(job) = self.jobs.get(call_id) {
            if job.name != name || job.command != command {
                return Err(Error::conflict(
                    "This tool call already started a different job.",
                ));
            }
            return Ok(job.id.clone());
        }
        if self.stopped.is_cancelled() {
            return Err(Error::conflict("The environment is stopping."));
        }
        if self.jobs.len() >= MAX_JOBS {
            return Err(Error::too_many_requests(
                "This environment has reached its managed job limit.",
            ));
        }
        let now = super::process::boot_ms()?;
        if now >= self.deadline_boot_ms {
            return Err(Error::conflict(
                "The environment's work deadline has expired.",
            ));
        }
        let request = serde_json::to_vec(&Request {
            command: command.into(),
            program: None,
            environment: self.environment.clone(),
            deadline_boot_ms: self.deadline_boot_ms.min(now.saturating_add(86_400_000)),
        })?;
        if request.len() > super::process::MAX_REQUEST {
            return Err(Error::too_large(
                "The encoded job request exceeds its limit.",
            ));
        }
        let (parent, child_control) = std::os::unix::net::UnixStream::pair()?;
        parent.set_nonblocking(true)?;
        let mut control = UnixStream::from_std(parent)?;
        let descriptor = child_control.as_raw_fd();
        let mut process = crate::process::command(
            self.executable
                .to_str()
                .ok_or_else(|| Error::bad("Invalid job supervisor path."))?,
            &["background-job-helper".into()],
            &Environment::from([
                ("PATH".into(), "/usr/bin:/bin".into()),
                ("LANG".into(), "C.UTF-8".into()),
            ]),
            Some(&self.cwd),
        );
        process
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(false);
        unsafe {
            process.pre_exec(move || {
                if libc::dup2(descriptor, 3) < 0 || libc::fcntl(3, libc::F_SETFD, 0) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = process.spawn()?;
        drop(child_control);
        let id = uuid::Uuid::new_v4().to_string();
        let (sender, result) = watch::channel(Outcome::Running);
        let stop = self.stopped.child_token();
        self.jobs.insert(
            call_id.into(),
            Job {
                id: id.clone(),
                name: name.into(),
                command: command.into(),
                result,
                stop: stop.clone(),
            },
        );
        // Register before delivery. An ambiguous write failure must not allow a
        // repeated request to start a second command under the same identity.
        let admission = self.stopped.clone();
        let poisoned = self.poisoned.clone();
        tokio::spawn(async move {
            let observed = observe(child, &mut control, &request, stop).await;
            let outcome = match observed {
                Ok(receipt) => Outcome::Finished { receipt },
                Err(error) => Outcome::Unknown {
                    message: error.message,
                },
            };
            if matches!(outcome, Outcome::Unknown { .. }) {
                admission.cancel();
                poisoned.cancel();
            }
            let _ = sender.send(outcome);
        });
        Ok(id)
    }

    pub fn lookup(&self, call_id: &str) -> Option<String> {
        self.jobs.get(call_id).map(|job| job.id.clone())
    }

    pub fn status(&self, id: &str) -> Result<Outcome> {
        self.jobs
            .values()
            .find(|job| job.id == id)
            .map(|job| job.result.borrow().clone())
            .ok_or_else(|| Error::not_found("Job does not belong to this environment."))
    }

    /// Internal environment observation; callers cannot infer another run's job
    /// IDs because this registry belongs to exactly one admitted environment.
    pub fn subscribe(&self, id: &str) -> Result<watch::Receiver<Outcome>> {
        self.jobs
            .values()
            .find(|job| job.id == id)
            .map(|job| job.result.clone())
            .ok_or_else(|| Error::not_found("Job does not belong to this environment."))
    }

    pub(crate) fn executable(&self) -> &Path {
        &self.executable
    }

    pub(crate) fn poisoned(&self) -> CancellationToken {
        self.poisoned.clone()
    }

    pub fn deadline_boot_ms(&self) -> u64 {
        self.deadline_boot_ms
    }

    pub fn active(&self) -> bool {
        self.jobs
            .values()
            .any(|job| !matches!(*job.result.borrow(), Outcome::Finished { .. }))
    }

    pub async fn close(&mut self) -> Result<()> {
        self.stop();
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(4);
        for job in self.jobs.values_mut() {
            loop {
                match job.result.borrow().clone() {
                    Outcome::Finished { .. } => break,
                    Outcome::Unknown { .. } => {
                        return Err(Error::unavailable(
                            "Job termination is unknown; environment fencing is required.",
                        ));
                    }
                    Outcome::Running => {}
                }
                match tokio::time::timeout_at(deadline, job.result.changed()).await {
                    Ok(Ok(())) => {}
                    _ => {
                        return Err(Error::unavailable(
                            "Job termination was not confirmed; environment fencing is required.",
                        ));
                    }
                }
            }
        }
        Ok(())
    }

    pub fn stop(&self) {
        self.stopped.cancel();
        for job in self.jobs.values() {
            job.stop.cancel();
        }
    }
}

impl Drop for Registry {
    fn drop(&mut self) {
        self.stop();
    }
}

async fn observe(
    mut child: tokio::process::Child,
    control: &mut UnixStream,
    request: &[u8],
    stop: CancellationToken,
) -> Result<Receipt> {
    if stop.is_cancelled() {
        control.shutdown().await?;
        let _ = child.wait().await;
        return Ok(Receipt {
            exit_code: None,
            signal: None,
            elapsed_ms: 0,
            stopped: true,
            timed_out: false,
            log_tail: String::new(),
            log_truncated: false,
        });
    }
    let delivery = async {
        control.write_all(request).await?;
        control.write_all(b"\n").await
    };
    tokio::select! {
        biased;
        () = stop.cancelled() => {
            control.shutdown().await?;
            let _ = child.wait().await;
            return Err(Error::unavailable("Job delivery was interrupted; environment fencing is required."));
        }
        result = delivery => result?,
    }
    let (reader, mut writer) = control.split();
    let mut bytes = Vec::new();
    let mut reader = reader.take(RECEIPT_LIMIT + 1);
    let read = reader.read_to_end(&mut bytes);
    tokio::pin!(read);
    tokio::select! {
        result = &mut read => { result?; }
        () = stop.cancelled() => {
            // Keep observing after cancellation: a request to stop is not proof
            // that the process and all descendants have actually stopped.
            writer.shutdown().await?;
            read.await?;
        }
    }
    let status = child.wait().await?;
    if !status.success() || bytes.len() as u64 > RECEIPT_LIMIT {
        return Err(Error::unavailable(
            "The job supervisor did not confirm process termination.",
        ));
    }
    serde_json::from_slice(&bytes).map_err(|_| {
        Error::unavailable("The job supervisor returned an invalid completion receipt.")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn stop_refuses_new_job_before_spawning() {
        let mut jobs = Registry::new(
            Path::new("/nonexistent/helper"),
            Path::new("/"),
            Environment::new(),
            super::super::process::boot_ms().unwrap() + 5000,
        );
        jobs.stop();
        let error = jobs
            .start("call", "name", "touch unexpected")
            .await
            .unwrap_err();
        assert_eq!(error.status, 409);
        assert!(jobs.jobs.is_empty());
    }

    #[tokio::test]
    async fn cancelled_before_delivery_never_sends_command() {
        let (parent, child_socket) = std::os::unix::net::UnixStream::pair().unwrap();
        parent.set_nonblocking(true).unwrap();
        let mut parent = UnixStream::from_std(parent).unwrap();
        let fd = child_socket.as_raw_fd();
        let directory = tempfile::tempdir().unwrap();
        let marker = directory.path().join("unexpected");
        let mut command = tokio::process::Command::new("/bin/bash");
        command.args([
            "-c",
            "if read -r -u 3 line; then touch \"$1\"; fi",
            "job-test",
            marker.to_str().unwrap(),
        ]);
        unsafe {
            command.pre_exec(move || {
                if libc::dup2(fd, 3) < 0 || libc::fcntl(3, libc::F_SETFD, 0) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = command.spawn().unwrap();
        drop(child_socket);
        let stop = CancellationToken::new();
        stop.cancel();
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(3),
            observe(child, &mut parent, b"execute", stop),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(result.stopped);
        assert!(!marker.exists());
    }

    #[tokio::test]
    async fn oversized_encoding_is_refused_before_launch() {
        let mut jobs = Registry::new(
            Path::new("/nonexistent/helper"),
            Path::new("/"),
            Environment::new(),
            super::super::process::boot_ms().unwrap() + 5000,
        );
        let error = jobs
            .start("call", "name", &format!("#{}", "\u{1}".repeat(45000)))
            .await
            .unwrap_err();
        assert_eq!(error.status, 413);
        assert!(jobs.jobs.is_empty());
    }
}
