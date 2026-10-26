//! Installation-owned program cache daemons. Cairn never caches command output;
//! it owns each daemon's private home/socket and lifetime.
use crate::error::{Error, Result};
use cairn_plugin_api::ProgramCache;
use std::{
    collections::HashMap,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
use tempfile::TempDir;
use tokio::{process::Child, sync::OnceCell};
use tokio_util::sync::CancellationToken;

const IDLE: Duration = Duration::from_secs(600);
const RESIDENT: usize = 64;

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct Identity {
    pub connection: String,
    pub revision: String,
    pub package: String,
    pub program: String,
    pub executable: PathBuf,
    /// A digest, never the environment's credentials or other plaintext values.
    pub environment: String,
}

struct Entry {
    identity: Identity,
    daemon: OnceCell<Daemon>,
    used: Mutex<Instant>,
    stop: CancellationToken,
}

impl Entry {
    fn available(&self) -> bool {
        !self.stop.is_cancelled() && self.daemon.get().is_none_or(Daemon::alive)
    }

    fn stop(&self) {
        self.stop.cancel();
        if let Some(daemon) = self.daemon.get() {
            daemon.stop();
        }
    }
}

#[derive(Default)]
pub(crate) struct Caches {
    entries: Mutex<HashMap<(String, String), Arc<Entry>>>,
    shutdown: CancellationToken,
    generation: AtomicU64,
}

impl Caches {
    /// Capture before reading authoritative state; verify under the pool lock.
    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }

    pub fn changed(&self) {
        let _entries = self.entries.lock().unwrap();
        self.generation.fetch_add(1, Ordering::AcqRel);
    }

    pub async fn acquire(
        &self,
        identity: Identity,
        config: &ProgramCache,
        reusable: bool,
        generation: u64,
    ) -> Result<Lease> {
        let entry = {
            let mut entries = self.entries.lock().unwrap();
            if self.shutdown.is_cancelled() {
                return Err(Error::unavailable("The installation is shutting down."));
            }
            if generation != self.generation() {
                return Err(Error::conflict(
                    "The plugin configuration changed during preparation. Retry the operation.",
                ));
            }
            let key = (identity.connection.clone(), identity.program.clone());
            let current = reusable.then(|| entries.get(&key)).flatten();
            // Revision/package transitions belong to authoritative invalidation,
            // not to callers whose preparation may have outlived that transition.
            // In particular, a late old call must never cancel a newer reader.
            if current.is_some_and(|entry| {
                entry.identity.revision != identity.revision
                    || entry.identity.package != identity.package
            }) {
                return Err(Error::conflict(
                    "The program cache belongs to a different connection revision or package. Retry the operation.",
                ));
            }
            if current
                .is_some_and(|entry| entry.identity != identity && Arc::strong_count(entry) > 1)
            {
                return Err(Error::unavailable(
                    "The program cache is busy with a different environment or executable. Retry the operation.",
                ));
            }
            if let Some(entry) = current.filter(|e| e.identity == identity && e.available()) {
                entry.clone()
            } else {
                if reusable && let Some(previous) = entries.remove(&key) {
                    previous.stop();
                }
                let entry = Arc::new(Entry {
                    identity,
                    daemon: OnceCell::new(),
                    used: Mutex::new(Instant::now()),
                    stop: self.shutdown.child_token(),
                });
                if reusable {
                    if entries.len() >= RESIDENT {
                        let oldest = entries
                            .iter()
                            .filter(|(_, entry)| Arc::strong_count(entry) == 1)
                            .min_by_key(|(_, e)| *e.used.lock().unwrap())
                            .map(|(key, _)| key.clone());
                        let Some(key) = oldest else {
                            return Err(Error::unavailable(
                                "The program cache capacity is busy. Retry the operation.",
                            ));
                        };
                        entries.remove(&key);
                    }
                    entries.insert(key, entry.clone());
                }
                entry
            }
        };
        let initialize = entry
            .daemon
            .get_or_try_init(|| Daemon::start(&entry.identity.executable, config));
        tokio::select! {
            () = entry.stop.cancelled() => return Err(Error::conflict("The program cache was invalidated. Retry the operation.")),
            result = initialize => { result?; }
        }
        if !entry.available() {
            entry.stop();
            return Err(Error::unavailable(
                "The program cache is unavailable. Retry the operation.",
            ));
        }
        *entry.used.lock().unwrap() = Instant::now();
        Ok(Lease { entry })
    }

    pub fn forget(&self, connection: &str) {
        let mut entries = self.entries.lock().unwrap();
        self.generation.fetch_add(1, Ordering::AcqRel);
        entries.retain(|(id, _), entry| {
            if id != connection {
                return true;
            }
            entry.stop();
            false
        });
    }

    /// Reuses the plugin supervisor's authoritative enabled-instance/package snapshot.
    pub fn reconcile(&self, generation: u64, valid: impl Fn(&Identity) -> bool) {
        let mut entries = self.entries.lock().unwrap();
        if generation != self.generation() {
            return;
        }
        let before = entries.len();
        entries.retain(|_, entry| {
            let idle =
                entry.used.lock().unwrap().elapsed() >= IDLE && Arc::strong_count(entry) == 1;
            if valid(&entry.identity) && !idle && entry.available() {
                return true;
            }
            entry.stop();
            false
        });
        if entries.len() != before {
            self.generation.fetch_add(1, Ordering::AcqRel);
        }
    }

    /// Terminal shutdown: reject later acquisitions and cancel transient leases too.
    pub fn clear(&self) {
        let mut entries = self.entries.lock().unwrap();
        self.shutdown.cancel();
        for entry in entries.values() {
            entry.stop();
        }
        entries.clear();
    }
}

impl Drop for Caches {
    fn drop(&mut self) {
        self.clear();
    }
}

pub(crate) struct Lease {
    entry: Arc<Entry>,
}

impl Lease {
    pub fn home(&self) -> &Path {
        self.entry
            .daemon
            .get()
            .expect("initialized cache")
            .home
            .path()
    }

    pub fn socket(&self) -> &Path {
        &self.entry.daemon.get().expect("initialized cache").socket
    }

    pub fn stop(&self) -> &CancellationToken {
        &self.entry.stop
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        *self.entry.used.lock().unwrap() = Instant::now();
    }
}

struct Daemon {
    child: Mutex<Child>,
    socket: PathBuf,
    home: TempDir,
}

impl Daemon {
    async fn start(executable: &Path, config: &ProgramCache) -> Result<Self> {
        let home = tempfile::Builder::new()
            .prefix("cairn-cache-")
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir()?;
        let socket = home.path().join("cache.sock");
        let mut command = tokio::process::Command::new(executable);
        command
            .args(&config.daemon_args)
            .env_clear()
            .env("HOME", home.path())
            .env("TMPDIR", home.path())
            .env(&config.socket_env, &socket)
            .current_dir(home.path())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .kill_on_drop(true);
        #[cfg(target_os = "linux")]
        {
            let parent = std::process::id() as libc::pid_t;
            // The foreground cache cannot outlive a killed standalone manager.
            unsafe {
                command.pre_exec(move || {
                    if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    if libc::getppid() != parent {
                        return Err(std::io::Error::other("Program cache parent exited"));
                    }
                    Ok(())
                });
            }
        }
        let child = command
            .spawn()
            .map_err(|_| Error::unavailable("The program cache could not start."))?;
        let daemon = Self {
            child: Mutex::new(child),
            socket,
            home,
        };
        let ready = async {
            loop {
                if !daemon.alive() {
                    return Err(Error::unavailable(
                        "The program cache exited before it was ready.",
                    ));
                }
                if let Ok(stream) = tokio::net::UnixStream::connect(&daemon.socket).await {
                    #[cfg(target_os = "linux")]
                    if stream
                        .peer_cred()
                        .ok()
                        .and_then(|c| c.pid())
                        .map(|pid| pid as u32)
                        != daemon.child.lock().unwrap().id()
                    {
                        return Err(Error::unavailable(
                            "The cache socket is not owned by its foreground daemon.",
                        ));
                    }
                    return Ok(());
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        };
        tokio::time::timeout(Duration::from_secs(3), ready)
            .await
            .map_err(|_| Error::unavailable("The program cache did not become ready."))??;
        Ok(daemon)
    }

    fn alive(&self) -> bool {
        matches!(self.child.lock().unwrap().try_wait(), Ok(None))
    }

    fn stop(&self) {
        // A client may have restarted its vendor daemon after an unexpected exit.
        // Restrict cleanup to the peer of this mode-0700 private socket directory.
        self.stop_socket_peer();
        let mut child = self.child.lock().unwrap();
        if matches!(child.try_wait(), Ok(None)) {
            let _ = child.start_kill();
        }
    }

    #[cfg(target_os = "linux")]
    fn stop_socket_peer(&self) {
        use std::os::{
            fd::{AsRawFd, FromRawFd, OwnedFd},
            unix::ffi::OsStrExt,
        };
        let raw = unsafe {
            libc::socket(
                libc::AF_UNIX,
                libc::SOCK_STREAM | libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK,
                0,
            )
        };
        if raw < 0 {
            return;
        }
        let stream = unsafe { OwnedFd::from_raw_fd(raw) };
        let mut address = unsafe { std::mem::zeroed::<libc::sockaddr_un>() };
        address.sun_family = libc::AF_UNIX as libc::sa_family_t;
        let path = self.socket.as_os_str().as_bytes();
        if path.len() >= address.sun_path.len() {
            return;
        }
        for (slot, byte) in address.sun_path.iter_mut().zip(path) {
            *slot = *byte as libc::c_char;
        }
        let length =
            (std::mem::offset_of!(libc::sockaddr_un, sun_path) + path.len() + 1) as libc::socklen_t;
        if unsafe {
            libc::connect(
                stream.as_raw_fd(),
                (&address as *const libc::sockaddr_un).cast(),
                length,
            )
        } != 0
        {
            return;
        }
        let mut credentials = libc::ucred {
            pid: 0,
            uid: 0,
            gid: 0,
        };
        let mut length = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
        let ok = unsafe {
            libc::getsockopt(
                stream.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                (&mut credentials as *mut libc::ucred).cast(),
                &mut length,
            )
        };
        if ok != 0 || credentials.pid <= 0 || credentials.uid != unsafe { libc::geteuid() } {
            return;
        }
        // SO_PEERPIDFD binds the signal to this socket's actual peer, without a
        // PID reuse race or /proc reads (op deliberately makes itself non-dumpable).
        // Older kernels still reap the owned foreground child below; they cannot
        // safely identify an independently restarted vendor daemon this way.
        let mut fd = -1;
        let mut length = std::mem::size_of::<i32>() as libc::socklen_t;
        if unsafe {
            libc::getsockopt(
                stream.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_PEERPIDFD,
                (&mut fd as *mut i32).cast(),
                &mut length,
            )
        } != 0
            || fd < 0
        {
            return;
        }
        let fd = unsafe { OwnedFd::from_raw_fd(fd) };
        unsafe {
            libc::syscall(
                libc::SYS_pidfd_send_signal,
                fd.as_raw_fd(),
                libc::SIGKILL,
                std::ptr::null::<libc::siginfo_t>(),
                0,
            )
        };
    }

    #[cfg(not(target_os = "linux"))]
    fn stop_socket_peer(&self) {}
}

impl Drop for Daemon {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests;
