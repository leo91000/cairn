//! Trusted per-attempt turn observations, separate from physical retirement.
use super::{
    channel::Channel,
    protocol::{Event, Request},
};
use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, sync::Arc};
use tokio::sync::{Mutex, Notify};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TurnReceipt {
    pub attempt_id: String,
    pub generation: String,
    pub turn_id: String,
    pub code: i32,
    pub log_start: u64,
    pub log_end: u64,
}

#[derive(Default, Clone)]
pub struct Control {
    state: Arc<Mutex<State>>,
    changed: Arc<Notify>,
    transition: Arc<Mutex<()>>,
    file_refresh: Arc<Mutex<()>>,
}

#[derive(Default)]
struct State {
    channel: Option<Channel>,
    guest_socket: Option<std::path::PathBuf>,
    generation: Option<String>,
    turns: HashMap<String, TurnReceipt>,
    starts: HashMap<String, u64>,
    failure: Option<String>,
    canonical_plan: Option<serde_json::Value>,
    native_bindings: Vec<NativeBinding>,
}

#[derive(Clone)]
struct NativeBinding {
    parent: Arc<std::fs::File>,
    name: std::ffi::CString,
    file: Arc<std::fs::File>,
}

impl NativeBinding {
    fn refresh(&self) -> std::io::Result<()> {
        use std::{
            io::Read,
            os::{
                fd::{AsRawFd, FromRawFd},
                unix::fs::FileExt,
            },
        };
        // Pin the source parent before the workload starts. Never follow a
        // replaced parent, symlink, FIFO or device while renewing credentials.
        let fd = unsafe {
            libc::openat(
                self.parent.as_raw_fd(),
                self.name.as_ptr(),
                libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        let source = unsafe { std::fs::File::from_raw_fd(fd) };
        if !source.metadata()?.is_file() {
            return Err(std::io::Error::other(
                "Runtime source is not a regular file.",
            ));
        }
        let mut bytes = Vec::new();
        source.take(2_000_001).read_to_end(&mut bytes)?;
        if bytes.len() > 2_000_000 {
            return Err(std::io::Error::other("Runtime source exceeds its limit."));
        }
        self.file.write_all_at(&bytes, 0)?;
        self.file.set_len(bytes.len() as u64)
    }
}

impl Control {
    pub async fn attach(&self, channel: Channel, generation: String) -> Result<()> {
        let mut state = self.state.lock().await;
        if state.channel.is_some() || state.guest_socket.is_some() || state.failure.is_some() {
            return Err(Error::conflict("Environment channel already attached."));
        }
        state.channel = Some(channel);
        state.generation = Some(generation);
        self.changed.notify_waiters();
        Ok(())
    }

    pub async fn attach_guest(&self, socket: std::path::PathBuf, generation: String) -> Result<()> {
        let mut state = self.state.lock().await;
        if state.channel.is_some() || state.guest_socket.is_some() || state.failure.is_some() {
            return Err(Error::conflict("Environment already attached."));
        }
        state.guest_socket = Some(socket);
        state.generation = Some(generation);
        Ok(())
    }

    pub(crate) async fn bind_native_files(
        &self,
        bindings: &[crate::native::home::Binding],
    ) -> Result<()> {
        use std::os::unix::{
            ffi::OsStrExt,
            fs::{MetadataExt, OpenOptionsExt},
        };
        let mut files = Vec::new();
        for binding in bindings {
            if !binding.read_only || !binding.source.is_file() {
                continue;
            }
            let file = std::fs::OpenOptions::new()
                .write(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
                .open(&binding.source)?;
            if !file.metadata()?.is_file() || file.metadata()?.nlink() != 1 {
                return Err(Error::forbidden(
                    "Native runtime binding is not a private regular file.",
                ));
            }
            let parent = std::fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
                .open(
                    binding
                        .source
                        .parent()
                        .ok_or_else(|| Error::bad("Missing runtime parent."))?,
                )?;
            let name = std::ffi::CString::new(
                binding
                    .source
                    .file_name()
                    .ok_or_else(|| Error::bad("Missing runtime filename."))?
                    .as_bytes(),
            )
            .map_err(Error::internal)?;
            files.push(NativeBinding {
                parent: Arc::new(parent),
                name,
                file: Arc::new(file),
            });
        }
        self.state.lock().await.native_bindings = files;
        Ok(())
    }

    pub async fn canonical_plan(&self, plan: serde_json::Value) {
        self.state.lock().await.canonical_plan = Some(plan);
    }

    pub async fn request(&self, mut request: Request) -> Result<Event> {
        let _transition = if matches!(request, Request::ResumeTurn { .. } | Request::Close { .. }) {
            Some(self.transition.lock().await)
        } else {
            None
        };
        let (channel, guest_socket, native_bindings, first_turn) = {
            let state = self.state.lock().await;
            if let Some(error) = &state.failure {
                return Err(Error::unavailable(error.clone()));
            }
            if let Request::ResumeTurn { plan, .. } = &mut request {
                if state
                    .starts
                    .keys()
                    .any(|turn| !state.turns.contains_key(turn))
                {
                    return Err(Error::conflict(
                        "The current provider turn is still active.",
                    ));
                }
                if let Some(canonical) = &state.canonical_plan {
                    if plan["id"] != canonical["id"] || plan["runId"] != canonical["runId"] {
                        return Err(Error::forbidden(
                            "Continuation belongs to another environment.",
                        ));
                    }
                    let mut updated = canonical.clone();
                    let mut chat = plan["chat"].clone();
                    if let Some(directory) = canonical["chat"].get("claudeCredentialsDirectory") {
                        chat["claudeCredentialsDirectory"] = directory.clone();
                    }
                    updated["chat"] = chat;
                    updated["mcpEnv"] = plan["mcpEnv"].clone();
                    if let Some(origin) =
                        canonical["mcpEnv"][crate::plugins::cli::ORIGIN_ENV].as_str()
                    {
                        replace_origin(&mut updated["chat"], origin);
                        updated["mcpEnv"][crate::plugins::cli::ORIGIN_ENV] = origin.into();
                    }
                    *plan = updated;
                }
            }
            (
                state.channel.clone(),
                state.guest_socket.clone(),
                state.native_bindings.clone(),
                state.starts.is_empty(),
            )
        };
        if let Some(channel) = channel {
            if !first_turn && matches!(request, Request::ResumeTurn { .. }) {
                // Bind mounts keep an inode, not its path. Refresh through the
                // controller-owned handle after the previous provider stopped.
                // Source replacements therefore cannot redirect the write.
                let ownership = self.file_refresh.clone().lock_owned().await;
                let refresh = tokio::task::spawn_blocking(move || {
                    let _ownership = ownership;
                    for binding in native_bindings {
                        binding.refresh()?;
                    }
                    Ok::<_, std::io::Error>(())
                })
                .await;
                if !matches!(refresh, Ok(Ok(()))) {
                    self.fail(
                        "Native runtime file refresh failed; environment fencing required.".into(),
                    )
                    .await;
                    return Err(Error::unavailable("Native runtime file refresh failed."));
                }
            }
            return channel.request(request).await;
        }
        if let Some(socket) = guest_socket {
            if !first_turn && matches!(request, Request::ResumeTurn { .. }) {
                let plan = self
                    .state
                    .lock()
                    .await
                    .canonical_plan
                    .clone()
                    .ok_or_else(|| Error::conflict("Missing environment plan."))?;
                crate::microvm::vm::refresh_imports(
                    &socket,
                    &crate::microvm::plan::Plan::new(plan),
                )
                .await?;
            }
            if let Request::ResumeTurn { plan, .. } = &mut request {
                // Host import paths and storage authority never cross into the guest.
                *plan = crate::microvm::plan::Plan::new(plan.clone()).for_guest();
            }
            let reply = crate::microvm::host::guest_request(
                &socket,
                &crate::microvm::protocol::GuestRequest::Environment { request },
            )
            .await?;
            return serde_json::from_value(reply).map_err(Error::from);
        }
        Err(Error::unavailable("Execution environment is not ready."))
    }

    pub async fn begin(&self, turn: &str, offset: u64) -> Result<()> {
        let mut state = self.state.lock().await;
        if state.failure.is_some() || state.generation.is_none() {
            return Err(Error::conflict("Environment is not active."));
        }
        if let Some(previous) = state.starts.get(turn) {
            if *previous != offset {
                return Err(Error::conflict("Conflicting turn start."));
            }
            return Ok(());
        }
        if state.starts.len() >= 4096 {
            return Err(Error::too_many_requests("Environment turn limit reached."));
        }
        state.starts.insert(turn.into(), offset);
        self.changed.notify_waiters();
        Ok(())
    }

    pub async fn finished(&self, attempt: &str, turn: &str, code: i32, log_end: u64) -> Result<()> {
        let mut state = self.state.lock().await;
        if state.failure.is_some() {
            return Err(Error::conflict(
                "Environment failed before turn completion.",
            ));
        }
        let generation = state
            .generation
            .clone()
            .ok_or_else(|| Error::conflict("Environment identity is missing."))?;
        let log_start = *state
            .starts
            .get(turn)
            .ok_or_else(|| Error::conflict("Turn completion without a start."))?;
        if log_end < log_start {
            return Err(Error::conflict("Invalid turn log boundary."));
        }
        let receipt = TurnReceipt {
            attempt_id: attempt.into(),
            generation,
            turn_id: turn.into(),
            code,
            log_start,
            log_end,
        };
        if let Some(previous) = state.turns.get(turn) {
            if previous.code != receipt.code || previous.log_end != receipt.log_end {
                return Err(Error::conflict("Conflicting turn completion."));
            }
            return Ok(());
        }
        state.turns.insert(turn.into(), receipt);
        self.changed.notify_waiters();
        Ok(())
    }

    pub async fn wait(&self, turn: &str) -> Result<TurnReceipt> {
        loop {
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            {
                let state = self.state.lock().await;
                if let Some(receipt) = state.turns.get(turn) {
                    return Ok(receipt.clone());
                }
                if let Some(error) = &state.failure {
                    return Err(Error::unavailable(error.clone()));
                }
            }
            changed.await;
        }
    }

    pub async fn start_offset(&self, turn: &str) -> Result<u64> {
        loop {
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            {
                let state = self.state.lock().await;
                if let Some(error) = &state.failure {
                    return Err(Error::unavailable(error.clone()));
                }
                if let Some(offset) = state.starts.get(turn) {
                    return Ok(*offset);
                }
            }
            changed.await;
        }
    }

    pub async fn fail(&self, message: String) {
        // A cancelled HTTP request cannot release captured inodes while its
        // blocking refresh still writes them. Keep physical teardown behind it.
        let _refresh = self.file_refresh.lock().await;
        let mut state = self.state.lock().await;
        state.failure = Some(message);
        state.channel = None;
        state.guest_socket = None;
        state.native_bindings.clear();
        state.canonical_plan = None;
        self.changed.notify_waiters();
    }
}

fn replace_origin(value: &mut serde_json::Value, origin: &str) {
    match value {
        serde_json::Value::String(value) => {
            *value = value.replace(crate::microvm::mcp::ORIGIN, origin);
        }
        serde_json::Value::Array(values) => {
            for value in values {
                replace_origin(value, origin);
            }
        }
        serde_json::Value::Object(values) => {
            for value in values.values_mut() {
                replace_origin(value, origin);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn turn_receipts_require_a_start_and_preserve_exact_log_bounds() {
        let control = Control::default();
        assert!(control.begin("first", 10).await.is_err());
        control
            .attach_guest("unused.sock".into(), "generation".into())
            .await
            .unwrap();
        assert!(control.finished("attempt", "first", 0, 20).await.is_err());
        control.begin("first", 10).await.unwrap();
        assert!(control.begin("first", 11).await.is_err());
        assert!(control.finished("attempt", "first", 0, 9).await.is_err());
        control.finished("attempt", "first", 0, 20).await.unwrap();
        let receipt = control.wait("first").await.unwrap();
        assert_eq!((receipt.log_start, receipt.log_end), (10, 20));
        control.begin("second", 20).await.unwrap();
        control.finished("attempt", "second", 1, 30).await.unwrap();
        assert_eq!(control.wait("second").await.unwrap().log_start, 20);
    }

    #[tokio::test]
    async fn native_file_refresh_keeps_the_original_bound_inode() {
        use std::os::unix::fs::MetadataExt;
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("runtime");
        std::fs::write(&source, b"original").unwrap();
        let held = std::fs::File::open(&source).unwrap();
        let inode = held.metadata().unwrap().ino();
        let control = Control::default();
        control
            .bind_native_files(&[crate::native::home::Binding {
                source: source.clone(),
                target: "/home/node/config".into(),
                read_only: true,
            }])
            .await
            .unwrap();
        let replacement = root.path().join("replacement");
        std::fs::write(&replacement, b"new bytes").unwrap();
        std::fs::rename(&replacement, &source).unwrap();
        assert_ne!(std::fs::metadata(&source).unwrap().ino(), inode);
        let files = control.state.lock().await.native_bindings.clone();
        assert_eq!(files[0].file.metadata().unwrap().ino(), inode);
        control.fail("stopped".into()).await;
        assert!(control.state.lock().await.native_bindings.is_empty());
    }

    #[tokio::test]
    async fn credential_refresh_rejects_redirects_and_keeps_original_parent() {
        use std::os::unix::fs::FileExt;
        let root = tempfile::tempdir().unwrap();
        let parent = root.path().join("parent");
        std::fs::create_dir(&parent).unwrap();
        let path = parent.join("credential");
        std::fs::write(&path, b"old").unwrap();
        let retained = std::fs::File::open(&path).unwrap();
        let control = Control::default();
        control
            .bind_native_files(&[crate::native::home::Binding {
                source: path.clone(),
                target: "/home/node/config".into(),
                read_only: true,
            }])
            .await
            .unwrap();
        let binding = control.state.lock().await.native_bindings[0].clone();
        let original = root.path().join("original");
        std::fs::rename(&parent, &original).unwrap();
        std::fs::create_dir(&parent).unwrap();
        std::fs::write(&path, b"foreign").unwrap();
        std::fs::write(original.join("replacement"), b"updated").unwrap();
        std::fs::rename(original.join("replacement"), original.join("credential")).unwrap();
        binding.refresh().unwrap();
        let mut bytes = [0; 7];
        retained.read_exact_at(&mut bytes, 0).unwrap();
        assert_eq!(&bytes, b"updated");
        assert_eq!(std::fs::read(path).unwrap(), b"foreign");
        std::fs::remove_file(original.join("credential")).unwrap();
        std::os::unix::fs::symlink("/dev/zero", original.join("credential")).unwrap();
        assert!(binding.refresh().is_err());
    }

    #[tokio::test]
    async fn fencing_waits_for_cancelled_request_file_work() {
        let control = Control::default();
        let ownership = control.file_refresh.clone().lock_owned().await;
        let closed = control.clone();
        let mut closing = tokio::spawn(async move { closed.fail("closed".into()).await });
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(10), &mut closing)
                .await
                .is_err()
        );
        drop(ownership);
        closing.await.unwrap();
        assert!(control.state.lock().await.failure.is_some());
    }

    #[tokio::test]
    async fn terminal_failure_releases_waiting_start_and_completion_observers() {
        let control = Control::default();
        let start = control.clone();
        let finish = control.clone();
        let starting = tokio::spawn(async move { start.start_offset("turn").await });
        let finishing = tokio::spawn(async move { finish.wait("turn").await });
        tokio::task::yield_now().await;
        control.fail("VM stopped".into()).await;
        assert!(
            tokio::time::timeout(std::time::Duration::from_secs(1), starting)
                .await
                .unwrap()
                .unwrap()
                .is_err()
        );
        assert!(
            tokio::time::timeout(std::time::Duration::from_secs(1), finishing)
                .await
                .unwrap()
                .unwrap()
                .is_err()
        );
    }

    #[tokio::test]
    async fn every_waiter_observes_completion_and_failed_channels_cannot_reattach() {
        let control = Control::default();
        control
            .attach_guest("unused.sock".into(), "generation".into())
            .await
            .unwrap();
        let mut waiting = Vec::new();
        for _ in 0..64 {
            let control = control.clone();
            waiting.push(tokio::spawn(async move { control.wait("turn").await }));
        }
        control.begin("turn", 0).await.unwrap();
        control.finished("attempt", "turn", 0, 0).await.unwrap();
        for waiting in waiting {
            tokio::time::timeout(std::time::Duration::from_secs(1), waiting)
                .await
                .unwrap()
                .unwrap()
                .unwrap();
        }
        control.fail("lost".into()).await;
        assert!(control.start_offset("missing").await.is_err());
        assert!(
            control
                .attach_guest("replacement.sock".into(), "generation".into())
                .await
                .is_err()
        );
    }
}
