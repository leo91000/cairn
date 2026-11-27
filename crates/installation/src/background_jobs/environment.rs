//! Environment-resident ownership of jobs and successive provider turns.
use std::{
    collections::{HashMap, HashSet},
    future::Future,
    os::fd::AsRawFd,
    path::PathBuf,
    pin::Pin,
    sync::Arc,
    time::Duration,
};

use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::Value;
use tokio::{
    io::{AsyncRead, AsyncReadExt, BufReader},
    net::UnixStream,
    process::Command,
    sync::{Mutex, mpsc},
};
use tokio_util::sync::CancellationToken;

use super::{
    Registry,
    protocol::{Event, Request},
};
use crate::{
    error::{Error, Result},
    microvm::wire,
};

pub struct Launch {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub environment: crate::process::Environment,
    pub prompt: String,
}

type Preparation = Pin<Box<dyn Future<Output = Result<Launch>> + Send>>;

/// Pin the managed credential directory before a provider can change paths.
/// The environment, which outlives that provider, removes its access snapshot
/// before publishing completion, including when the provider was killed.
struct TurnCredentials(Option<(std::fs::File, PathBuf)>);

impl TurnCredentials {
    fn capture(plan: &Value, launch: &Launch) -> Result<Self> {
        use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
        if plan["chat"]["claudeManagedAuth"] != true {
            return Ok(Self(None));
        }
        let dedicated = plan["chat"]["claudeCredentialsDirectory"].as_str();
        let path = dedicated
            .or_else(|| {
                launch
                    .environment
                    .get("CLAUDE_CONFIG_DIR")
                    .map(String::as_str)
            })
            .ok_or_else(|| Error::bad("Missing managed credential directory."))?;
        let mut options = std::fs::OpenOptions::new();
        options
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC);
        let directory = match options.open(path) {
            Err(error)
                if error.kind() == std::io::ErrorKind::NotFound
                    && dedicated.is_none()
                    && plan["backend"] == "firecracker" =>
            {
                // A retained disk may be switching to Claude for the first
                // time. Create its private home without importing over saved state.
                match std::fs::DirBuilder::new().mode(0o700).create(path) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                    Err(error) => return Err(error.into()),
                }
                options.open(path)?
            }
            result => result?,
        };
        Ok(Self(Some((directory, path.into()))))
    }

    fn clear(&self) -> Result<()> {
        use std::os::unix::fs::MetadataExt;
        let Some((directory, path)) = &self.0 else {
            return Ok(());
        };
        let result =
            unsafe { libc::unlinkat(directory.as_raw_fd(), c".credentials.json".as_ptr(), 0) };
        if result != 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() != std::io::ErrorKind::NotFound {
                return Err(error.into());
            }
        }
        let pinned = directory.metadata()?;
        let current = std::fs::symlink_metadata(path)?;
        if !current.is_dir() || current.dev() != pinned.dev() || current.ino() != pinned.ino() {
            return Err(Error::conflict(
                "Managed credential directory changed; environment fencing is required.",
            ));
        }
        Ok(())
    }
}

impl Drop for TurnCredentials {
    fn drop(&mut self) {
        let _ = self.clear();
    }
}

/// Must be called inside the admitted execution namespaces and workload UID.
/// The private controller socket outlives any individual provider process.
pub async fn serve(
    stream: UnixStream,
    initial: Value,
    jobs: Registry,
    stop: CancellationToken,
    prepare: impl Fn(&Value) -> Preparation,
) -> Result<()> {
    let attempt = initial["id"]
        .as_str()
        .ok_or_else(|| Error::bad("Missing environment attempt."))?
        .to_owned();
    let generation = uuid::Uuid::new_v4().to_string();
    let policy = immutable_policy(&initial);
    let deadline = jobs.deadline_boot_ms();
    let poisoned = jobs.poisoned();
    let executable = jobs.executable().to_owned();
    let jobs = Arc::new(Mutex::new(jobs));
    let lifetime = stop.child_token();
    let _lifetime_guard = lifetime.clone().drop_guard();
    let deadline_stop = lifetime.clone();
    let watchdog = tokio_util::task::AbortOnDropHandle::new(tokio::spawn(async move {
        let mut timer = tokio::time::interval(Duration::from_millis(100));
        loop {
            timer.tick().await;
            if super::process::boot_ms().map_or(true, |now| now >= deadline) {
                deadline_stop.cancel();
                break;
            }
        }
    }));
    let (read, mut write) = stream.into_split();
    let mut read = BufReader::new(read);
    let (requests, mut pending_requests) = mpsc::channel(16);
    let reading_stop = lifetime.clone();
    let disconnected = CancellationToken::new();
    let reader_disconnected = disconnected.clone();
    let reader = tokio_util::task::AbortOnDropHandle::new(tokio::spawn(async move {
        loop {
            let request = tokio::select! {
                () = reading_stop.cancelled() => break,
                request = wire::read(&mut read) => request,
            };
            let finished = !matches!(request, Ok(Some(_)));
            if finished {
                reader_disconnected.cancel();
            }
            if requests.send(request).await.is_err() || finished {
                break;
            }
        }
    }));
    let (events, mut received) = mpsc::channel(32);
    wire::write(
        &mut write,
        &Event::Ready {
            attempt_id: attempt,
            generation,
        },
    )
    .await?;
    let mut watched_jobs = HashSet::new();
    let mut turns = HashMap::<String, Value>::new();
    let mut job_admission =
        initial["chat"]["planning"] != true && initial["chat"]["sandbox"] != "read-only";
    let mut turn = None;
    let mut turn_stop = CancellationToken::new();
    let operation = async {
        loop {
            tokio::select! {
                () = stop.cancelled() => break Ok(()),
                event = received.recv() => {
                    let Some(event) = event else { break Err(Error::unavailable("Environment event channel closed.")); };
                    let failed = matches!(event, Event::TurnFailed { .. } | Event::JobChanged { outcome: super::Outcome::Unknown { .. }, .. });
                    if matches!(event, Event::TurnFinished { .. }) { turn = None; }
                    if let Err(error) = wire::write(&mut write, &event).await { break Err(error); }
                    if failed { break Err(Error::unavailable("Workload termination was not confirmed; environment fencing is required.")); }
                }
                message = pending_requests.recv() => {
                    let value = match message {
                        Some(Ok(Some(value))) => value,
                        Some(Err(error)) => break Err(error),
                        _ => break Err(Error::unavailable("Environment controller disconnected.")),
                    };
                    let decoded = serde_json::from_value::<Request>(value).map_err(Error::from);
                    let response = match decoded {
                        Err(error) => break Err(error),
                        Ok(Request::StartJob { request_id, name, command }) => {
                            if !job_admission {
                                wire::write(&mut write, &Event::Refused { request_id, message: "Background jobs require a writable non-planning provider turn.".into() }).await?;
                                continue;
                            }
                            let mut jobs = jobs.lock().await;
                            match jobs.start(&request_id, &name, &command).await {
                                Ok(job_id) => {
                                    if watched_jobs.insert(job_id.clone()) {
                                        let mut result = jobs.subscribe(&job_id)?;
                                        let events = events.clone();
                                        let watched = job_id.clone();
                                        let cancel = lifetime.clone();
                                        tokio::spawn(async move {
                                            loop {
                                                tokio::select! {
                                                    () = cancel.cancelled() => break,
                                                    changed = result.changed() => { if changed.is_err() { break; } }
                                                }
                                                let outcome = result.borrow_and_update().clone();
                                                if events.send(Event::JobChanged { job_id: watched.clone(), outcome }).await.is_err() { break; }
                                            }
                                        });
                                    }
                                    Event::JobStarted { request_id, job_id }
                                }
                                Err(error) => Event::Refused { request_id, message: error.message },
                            }
                        }
                        Ok(Request::LookupJob { request_id, call_id }) => {
                            Event::JobLookup { request_id, job_id: jobs.lock().await.lookup(&call_id) }
                        }
                        Ok(Request::Activity { request_id }) => {
                            let active = jobs.lock().await.active();
                            match crate::native::docker::lifecycle::active_current(initial["backend"] == "firecracker").await {
                                Ok(containers) => Event::Activity { request_id, jobs: active, containers },
                                Err(error) => Event::Refused { request_id, message: error.message },
                            }
                        }
                        Ok(Request::JobStatus { request_id, job_id }) => {
                            match jobs.lock().await.status(&job_id) {
                                Ok(outcome) => Event::JobStatus { request_id, job_id, outcome },
                                Err(error) => Event::Refused { request_id, message: error.message },
                            }
                        }
                        Ok(Request::ResumeTurn { request_id, turn_id, plan }) => {
                            if let Some(previous) = turns.get(&turn_id) {
                                if previous == &plan { Event::Accepted { request_id } }
                                else { Event::Refused { request_id, message: "This turn already has a different plan.".into() } }
                            } else if turn.is_some() || immutable_policy(&plan) != policy || turns.len() >= 4096
                                || (plan["chat"]["sandbox"] != initial["chat"]["sandbox"]
                                    && plan["chat"]["sandbox"] != "read-only"
                                    && plan["chat"]["sandbox"] != initial["sandbox"]) {
                                Event::Refused { request_id, message: "Environment is busy, full or its execution policy changed.".into() }
                            } else {
                                match prepare(&plan).await {
                                    Err(error) => Event::Refused { request_id, message: error.message },
                                    Ok(command) => {
                                        if poisoned.is_cancelled() { return Err(Error::unavailable("Workload ownership is unknown; environment fencing is required.")); }
                                        let credentials = TurnCredentials::capture(&plan, &command)?;
                                        // Publish the boundary before spawning: the controller must
                                        // record the log cursor before any provider output arrives.
                                        wire::write(&mut write, &Event::TurnStarted { turn_id: turn_id.clone() }).await?;
                                        turn_stop = lifetime.child_token();
                                        turn = Some(spawn_turn((command, credentials), &executable, deadline, turn_id.clone(), events.clone(), turn_stop.clone(), poisoned.clone())?);
                                        job_admission = plan["chat"]["planning"] != true && plan["chat"]["sandbox"] != "read-only";
                                        turns.insert(turn_id, plan);
                                        Event::Accepted { request_id }
                                    }
                                }
                            }
                        }
                        Ok(Request::Close { request_id }) => {
                            wire::write(&mut write, &Event::Accepted { request_id }).await?;
                            break Ok(());
                        }
                    };
                    wire::write(&mut write, &response).await?;
                }
            }
        }
    };
    let result = tokio::select! {
        biased;
        () = disconnected.cancelled() => Err(Error::unavailable("Environment controller disconnected; workload fencing is required.")),
        () = poisoned.cancelled() => Err(Error::unavailable("Workload ownership is unknown; environment fencing is required.")),
        () = lifetime.cancelled() => Ok(()),
        result = operation => result,
    };
    watchdog.abort();
    reader.abort();
    turn_stop.cancel();
    let jobs_closed = jobs.lock().await.close().await;
    let provider_closed = if let Some(turn) = turn {
        tokio::time::timeout(Duration::from_secs(4), turn)
            .await
            .map_err(|_| {
                Error::unavailable(
                    "Provider termination is unknown; environment fencing is required.",
                )
            })?
            .map_err(Error::internal)?
    } else {
        Ok(())
    };
    result.and(jobs_closed).and(provider_closed)
}

fn immutable_policy(plan: &Value) -> Value {
    serde_json::json!({
        "id": plan["id"], "runId": plan["runId"], "cwd": plan["cwd"],
        "projectRoot": plan["projectRoot"], "sandbox": plan["sandbox"],
        "nativeMode": plan["nativeMode"], "resources": plan["resources"],
        "imports": plan["imports"], "runtimeId": plan["runtimeId"],
        "provider": plan["chat"]["provider"],
        "providerWritableRoots": plan["chat"]["writableRoots"],
    })
}

fn spawn_turn(
    prepared: (Launch, TurnCredentials),
    executable: &std::path::Path,
    deadline_boot_ms: u64,
    turn_id: String,
    events: mpsc::Sender<Event>,
    stop: CancellationToken,
    poisoned: CancellationToken,
) -> Result<tokio::task::JoinHandle<Result<()>>> {
    let (launch, credentials) = prepared;
    let args = [vec![launch.program], launch.args].concat();
    let (parent, child_control) = std::os::unix::net::UnixStream::pair()?;
    parent.set_nonblocking(true)?;
    let control = UnixStream::from_std(parent)?;
    let fd = child_control.as_raw_fd();
    let mut helper = Command::new(executable);
    helper.arg("background-job-helper");
    helper
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("LANG", "C.UTF-8");
    helper.current_dir(launch.cwd);
    unsafe {
        helper.pre_exec(move || {
            if libc::dup2(fd, 3) < 0 || libc::fcntl(3, libc::F_SETFD, 0) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let request = super::Request {
        command: String::new(),
        environment: launch.environment,
        program: Some(args),
        deadline_boot_ms,
    };
    helper
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .process_group(0)
        .kill_on_drop(true);
    let encoded = serde_json::to_vec(&request)?;
    if encoded.len() > super::process::MAX_REQUEST {
        return Err(Error::too_large(
            "Provider launch request exceeds its limit.",
        ));
    }
    let mut child = helper.spawn()?;
    drop(child_control);
    let mut input = child.stdin.take().unwrap();
    let stdin = tokio_util::task::AbortOnDropHandle::new(tokio::spawn(async move {
        use tokio::io::AsyncWriteExt;
        let _ = input.write_all(launch.prompt.as_bytes()).await;
    }));
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    Ok(tokio::spawn(async move {
        use tokio::io::AsyncWriteExt;
        let mut control = control;
        let delivered = tokio::select! {
            () = stop.cancelled() => Err(std::io::Error::other("Provider launch cancelled")),
            result = async {
                control.write_all(&encoded).await?;
                control.write_all(b"\n").await
            } => result,
        };
        if delivered.is_err() {
            poisoned.cancel();
            let _ = control.shutdown().await;
            let _ = events
                .send(Event::TurnFailed {
                    turn_id,
                    message: "Provider supervisor did not start.".into(),
                })
                .await;
            return Err(Error::unavailable(
                "Provider supervisor did not start; environment fencing is required.",
            ));
        }
        let out = tokio_util::task::AbortOnDropHandle::new(tokio::spawn(forward(
            stdout,
            false,
            turn_id.clone(),
            events.clone(),
        )));
        let err = tokio_util::task::AbortOnDropHandle::new(tokio::spawn(forward(
            stderr,
            true,
            turn_id.clone(),
            events.clone(),
        )));
        let (read, mut write) = control.split();
        let mut receipt = Vec::new();
        let mut read = read.take(128 * 1024 + 1);
        let receiving = read.read_to_end(&mut receipt);
        tokio::pin!(receiving);
        let result = tokio::select! {
            result = &mut receiving => result,
            () = stop.cancelled() => {
                let _ = write.shutdown().await;
                receiving.await
            }
        };
        let status = child.wait().await;
        stdin.abort();
        let receipt = result
            .ok()
            .and_then(|_| serde_json::from_slice::<super::Receipt>(&receipt).ok());
        let code = receipt
            .as_ref()
            .and_then(|receipt| receipt.exit_code)
            .unwrap_or(143);
        let confirmed = receipt.is_some() && status.is_ok_and(|status| status.success());
        let readers = async {
            out.await.map_err(Error::internal)??;
            err.await.map_err(Error::internal)??;
            Ok::<_, Error>(())
        };
        let credential_error = credentials.clear().err();
        let closed = confirmed
            && credential_error.is_none()
            && matches!(
                tokio::time::timeout(Duration::from_secs(2), readers).await,
                Ok(Ok(()))
            );
        if !closed {
            poisoned.cancel();
        }
        let event = match closed {
            true => Event::TurnFinished { turn_id, code },
            _ => Event::TurnFailed {
                turn_id,
                message: if credential_error.is_some() {
                    "Managed provider credentials could not be cleared; environment fencing is required.".into()
                } else {
                    "Provider output did not finish before its deadline.".into()
                },
            },
        };
        let delivered = matches!(
            tokio::time::timeout(Duration::from_secs(2), events.send(event)).await,
            Ok(Ok(()))
        );
        if !delivered {
            poisoned.cancel();
        }
        if closed && delivered {
            Ok(())
        } else {
            Err(Error::unavailable(
                "Provider termination was not confirmed; environment fencing is required.",
            ))
        }
    }))
}

async fn forward(
    mut stream: impl AsyncRead + Unpin,
    stderr: bool,
    turn_id: String,
    events: mpsc::Sender<Event>,
) -> Result<()> {
    let mut bytes = [0; 16384];
    loop {
        let count = stream.read(&mut bytes).await?;
        if count == 0 {
            return Ok(());
        }
        let event = Event::Output {
            turn_id: turn_id.clone(),
            stderr,
            data: STANDARD.encode(&bytes[..count]),
        };
        events
            .send(event)
            .await
            .map_err(|_| Error::unavailable("Provider output channel closed."))?;
    }
}

#[cfg(test)]
mod credential_tests {
    use super::*;

    #[test]
    fn manages_claude_credentials_after_switching_a_retained_firecracker_home() {
        use std::os::unix::fs::PermissionsExt;

        let root = tempfile::tempdir().unwrap();
        let codex = root.path().join(".codex");
        std::fs::create_dir(&codex).unwrap();
        std::fs::write(codex.join("session"), b"saved").unwrap();
        let claude = root.path().join(".claude");
        let launch = Launch {
            program: String::new(),
            args: vec![],
            cwd: root.path().into(),
            environment: [("CLAUDE_CONFIG_DIR".into(), claude.to_str().unwrap().into())].into(),
            prompt: String::new(),
        };
        let plan = serde_json::json!({
            "backend": "firecracker",
            "chat": {"provider": "claude", "claudeManagedAuth": true}
        });

        let guard = TurnCredentials::capture(&plan, &launch).unwrap();
        assert_eq!(
            std::fs::metadata(&claude).unwrap().permissions().mode() & 0o777,
            0o700
        );
        std::fs::write(claude.join(".credentials.json"), b"managed").unwrap();
        std::fs::write(claude.join("session"), b"continued").unwrap();
        guard.clear().unwrap();
        assert!(!claude.join(".credentials.json").exists());
        assert_eq!(std::fs::read(claude.join("session")).unwrap(), b"continued");
        assert_eq!(std::fs::read(codex.join("session")).unwrap(), b"saved");
    }

    #[test]
    fn managed_credentials_clear_the_pinned_directory_and_preserve_unmanaged_files() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("credentials");
        std::fs::create_dir(&directory).unwrap();
        let launch = Launch {
            program: String::new(),
            args: vec![],
            cwd: root.path().into(),
            environment: crate::process::Environment::new(),
            prompt: String::new(),
        };
        let mut plan = serde_json::json!({"chat":{"claudeManagedAuth":true,"claudeCredentialsDirectory":directory}});
        let guard = TurnCredentials::capture(&plan, &launch).unwrap();
        std::fs::write(directory.join(".credentials.json"), "managed").unwrap();
        let moved = root.path().join("moved");
        std::fs::rename(&directory, &moved).unwrap();
        std::fs::create_dir(&directory).unwrap();
        std::fs::write(directory.join(".credentials.json"), "replacement").unwrap();
        assert!(guard.clear().is_err());
        assert!(!moved.join(".credentials.json").exists());
        assert_eq!(
            std::fs::read_to_string(directory.join(".credentials.json")).unwrap(),
            "replacement"
        );
        plan["chat"]["claudeManagedAuth"] = false.into();
        TurnCredentials::capture(&plan, &launch)
            .unwrap()
            .clear()
            .unwrap();
        assert!(directory.join(".credentials.json").exists());
        std::fs::write(moved.join(".credentials.json"), "managed-again").unwrap();
        drop(guard);
        assert!(!moved.join(".credentials.json").exists());
    }
}
