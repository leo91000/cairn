use std::{
    collections::{HashMap, VecDeque},
    fs,
    io::{self, Read, Write},
    os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd},
    os::unix::net::UnixStream,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};

pub(super) const MAX_REQUEST: usize = 256 * 1024;
const LOG_LIMIT: usize = 16 * 1024;
const MAX_PROCESSES: usize = 4096;
const TERMINATION_GRACE: Duration = Duration::from_secs(2);

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Request {
    pub command: String,
    #[serde(default)]
    pub environment: crate::process::Environment,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub program: Option<Vec<String>>,
    /// Absolute CLOCK_BOOTTIME deadline shared by this environment and helper.
    pub deadline_boot_ms: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Receipt {
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
    pub elapsed_ms: u64,
    pub stopped: bool,
    pub timed_out: bool,
    pub log_tail: String,
    pub log_truncated: bool,
}

struct Process {
    descriptor: OwnedFd,
}

impl Process {
    fn open(pid: u32) -> io::Result<Self> {
        let descriptor = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
        if descriptor < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            descriptor: unsafe { OwnedFd::from_raw_fd(descriptor as RawFd) },
        })
    }

    fn signal(&self, signal: i32) -> io::Result<()> {
        let result = unsafe {
            libc::syscall(
                libc::SYS_pidfd_send_signal,
                self.descriptor.as_raw_fd(),
                signal,
                std::ptr::null::<libc::siginfo_t>(),
                0,
            )
        };
        if result == 0 || io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
            return Ok(());
        }
        Err(io::Error::last_os_error())
    }
}

/// Blocking entry point, invoked before starting a runtime or helper threads.
/// FD 3 is a private socket inherited solely from the environment supervisor.
/// The helper inherits the workload UID, namespaces, cgroup, cwd and environment.
/// It does not accept credentials, paths or privileges in the job request.
pub fn helper() -> io::Result<()> {
    let mut kind = 0;
    let mut length = std::mem::size_of::<i32>() as libc::socklen_t;
    if unsafe {
        libc::getsockopt(
            3,
            libc::SOL_SOCKET,
            libc::SO_TYPE,
            (&raw mut kind).cast(),
            &raw mut length,
        )
    } != 0
        || kind != libc::SOCK_STREAM
    {
        return Err(io::Error::other("Missing private job control socket"));
    }
    let mut control = unsafe { UnixStream::from_raw_fd(3) };
    // Do not let a command acquire the receipt channel, even if it enumerates FDs.
    if unsafe { libc::fcntl(3, libc::F_SETFD, libc::FD_CLOEXEC) } != 0 {
        return Err(io::Error::last_os_error());
    }
    if unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // The workload must not steal the private receipt socket with ptrace or
    // /proc access merely because it executes under the same unprivileged UID.
    if unsafe { libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0) } != 0 {
        return Err(io::Error::last_os_error());
    }
    control.set_read_timeout(Some(Duration::from_secs(5)))?;
    let request = read_request(&mut control)?;
    control.set_read_timeout(None)?;
    control.set_nonblocking(true)?;
    let receipt = supervise(&mut control, &request)?;
    control.set_nonblocking(false)?;
    control.set_write_timeout(Some(Duration::from_secs(2)))?;
    serde_json::to_writer(&mut control, &receipt)?;
    control.write_all(b"\n")
}

pub(super) fn boot_ms() -> io::Result<u64> {
    let mut time = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    if unsafe { libc::clock_gettime(libc::CLOCK_BOOTTIME, &raw mut time) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok((time.tv_sec as u64)
        .saturating_mul(1000)
        .saturating_add(time.tv_nsec as u64 / 1_000_000))
}

fn cancelled(control: &mut UnixStream) -> io::Result<bool> {
    let mut byte = [0];
    match control.read(&mut byte) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::WouldBlock => Ok(false),
        Err(error) => Err(error),
    }
}

// Snapshot after ECHILD: consume every already queued byte without waiting for
// EOF or following new writes from an externally passed descriptor.
fn drain_final(fd: RawFd, tail: &mut Tail) -> io::Result<()> {
    let mut available: libc::c_int = 0;
    if unsafe { libc::ioctl(fd, libc::FIONREAD, &raw mut available) } < 0 {
        return Err(io::Error::last_os_error());
    }
    let mut remaining = available.max(0) as usize;
    let mut buffer = [0; 4096];
    while remaining > 0 {
        let count =
            unsafe { libc::read(fd, buffer.as_mut_ptr().cast(), buffer.len().min(remaining)) };
        if count == 0 {
            break;
        }
        if count < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            if error.kind() == io::ErrorKind::WouldBlock {
                break;
            }
            return Err(error);
        }
        remaining -= count as usize;
        tail.push(&buffer[..count as usize]);
    }
    Ok(())
}

fn read_request(control: &mut UnixStream) -> io::Result<Request> {
    let mut bytes = Vec::new();
    let mut byte = [0];
    loop {
        control.read_exact(&mut byte)?;
        if byte[0] == b'\n' {
            break;
        }
        bytes.push(byte[0]);
        if bytes.len() > MAX_REQUEST {
            return Err(io::Error::other("Job request is too large"));
        }
    }
    let request: Request = serde_json::from_slice(&bytes)?;
    if (request.command.is_empty() && request.program.as_ref().is_none_or(Vec::is_empty))
        || request.deadline_boot_ms > boot_ms()?.saturating_add(86_400_000)
    {
        return Err(io::Error::other("Invalid job command or lifetime"));
    }
    Ok(request)
}

fn supervise(control: &mut UnixStream, request: &Request) -> io::Result<Receipt> {
    let start = Instant::now();
    let initial_time = boot_ms()?;
    if initial_time >= request.deadline_boot_ms || cancelled(control)? {
        return Ok(Receipt {
            exit_code: None,
            signal: None,
            elapsed_ms: 0,
            stopped: true,
            timed_out: initial_time >= request.deadline_boot_ms,
            log_tail: String::new(),
            log_truncated: false,
        });
    }
    let provider = request.program.is_some();
    let mut command = if let Some(program) = &request.program {
        let mut command = Command::new(&program[0]);
        command.args(&program[1..]);
        command
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit());
        command
    } else {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", &request.command]);
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command
    };
    command.env_clear().envs(
        request
            .environment
            .iter()
            .filter(|(key, _)| !crate::process::storage_key(key)),
    );
    let mut child = command.spawn()?;
    let root = child.id();
    let mut output = Vec::new();
    if !provider {
        output.push(unsafe { OwnedFd::from_raw_fd(child.stdout.take().unwrap().into_raw_fd()) });
        output.push(unsafe { OwnedFd::from_raw_fd(child.stderr.take().unwrap().into_raw_fd()) });
    }
    for stream in &output {
        nonblocking(stream.as_raw_fd())?;
    }
    let mut known = HashMap::new();
    let mut tail = Tail::default();
    let mut root_status = None;
    let mut terminating = None;
    let mut stopped = false;
    let mut timed_out = false;
    loop {
        for stream in &mut output {
            drain(stream.as_raw_fd(), &mut tail)?;
        }
        let mut command = [0; 32];
        match control.read(&mut command) {
            Ok(0) | Ok(_) => stopped = true,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
            Err(_) => stopped = true,
        }
        timed_out |= boot_ms()? >= request.deadline_boot_ms;
        // An incomplete inventory cannot prove termination. Let the trusted
        // environment observe helper failure and fence its owned cgroup.
        collect_owned(std::process::id(), &mut known)?;
        if (stopped || timed_out || root_status.is_some()) && terminating.is_none() {
            terminating = Some(Instant::now());
        }
        if let Some(since) = terminating {
            let signal = if since.elapsed() < TERMINATION_GRACE {
                libc::SIGTERM
            } else {
                libc::SIGKILL
            };
            for process in known.values() {
                process.signal(signal)?;
            }
        }
        // Reap only after acquiring descendant identities. A shell may exit
        // while a detached child is being adopted by this subreaper.
        let mut has_children = true;
        loop {
            let mut status = 0;
            let pid = unsafe { libc::waitpid(-1, &raw mut status, libc::WNOHANG) };
            if pid > 0 {
                known.remove(&(pid as u32));
                if pid as u32 == root {
                    root_status = Some(status);
                }
                continue;
            }
            if pid == -1 {
                let error = io::Error::last_os_error();
                if error.raw_os_error() != Some(libc::ECHILD) {
                    return Err(error);
                }
                has_children = false;
            }
            break;
        }
        if !has_children {
            // No living descendant can inherit the pipes now. Drain once more;
            // never wait for EOF from a descriptor passed to a foreign process.
            for stream in &mut output {
                drain_final(stream.as_raw_fd(), &mut tail)?;
            }
            let status = root_status.ok_or_else(|| io::Error::other("Missing job exit status"))?;
            return Ok(Receipt {
                exit_code: libc::WIFEXITED(status).then(|| libc::WEXITSTATUS(status)),
                signal: libc::WIFSIGNALED(status).then(|| libc::WTERMSIG(status)),
                elapsed_ms: start.elapsed().as_millis().min(u64::MAX as u128) as u64,
                stopped,
                timed_out,
                log_tail: String::from_utf8_lossy(&tail.bytes.into_iter().collect::<Vec<_>>())
                    .into_owned(),
                log_truncated: tail.truncated,
            });
        }
        let mut poll = libc::pollfd {
            fd: control.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // Bounded cadence also discovers children that detach and close output.
        unsafe { libc::poll(&raw mut poll, 1, 20) };
    }
}

use std::os::fd::IntoRawFd;

fn nonblocking(fd: RawFd) -> io::Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[derive(Default)]
struct Tail {
    bytes: VecDeque<u8>,
    truncated: bool,
}

impl Tail {
    fn push(&mut self, bytes: &[u8]) {
        for byte in bytes {
            if self.bytes.len() == LOG_LIMIT {
                self.bytes.pop_front();
                self.truncated = true;
            }
            self.bytes.push_back(*byte);
        }
    }
}

fn drain(fd: RawFd, tail: &mut Tail) -> io::Result<()> {
    let mut buffer = [0; 4096];
    // A continuous output producer cannot starve cancellation or deadline checks.
    for _ in 0..16 {
        let count = unsafe { libc::read(fd, buffer.as_mut_ptr().cast(), buffer.len()) };
        if count > 0 {
            tail.push(&buffer[..count as usize]);
            continue;
        }
        if count == 0 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        if error.kind() == io::ErrorKind::WouldBlock {
            return Ok(());
        }
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
    Ok(())
}

fn identity(pid: u32) -> io::Result<u64> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat"))?;
    let (_, fields) = stat
        .rsplit_once(')')
        .ok_or_else(|| io::Error::other("Invalid process identity"))?;
    fields
        .split_whitespace()
        .nth(19)
        .and_then(|value| value.parse().ok())
        .ok_or_else(|| io::Error::other("Missing process start time"))
}

fn collect_owned(root: u32, known: &mut HashMap<u32, Process>) -> io::Result<()> {
    // Signal only direct children. A killed parent makes its children ours via
    // subreaper adoption on the next pass. Traversing a reaped grandchild's PID
    // could otherwise follow a reused process identity outside this job.
    for thread in fs::read_dir(format!("/proc/{root}/task"))? {
        let path = thread?.path().join("children");
        let children = match fs::read_to_string(&path) {
            Ok(children) => children,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        for child in children.split_whitespace() {
            let pid = child
                .parse()
                .map_err(|_| io::Error::other("Invalid child identity"))?;
            if known.contains_key(&pid) {
                continue;
            }
            if known.len() >= MAX_PROCESSES {
                return Err(io::Error::other("Job process inventory exceeded its limit"));
            }
            let before = match identity(pid) {
                Ok(before) => before,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error),
            };
            let process = match Process::open(pid) {
                Ok(process) => process,
                Err(error) if error.raw_os_error() == Some(libc::ESRCH) => continue,
                Err(error) => return Err(error),
            };
            // Re-read the ownership edge and start time around pidfd_open. Direct
            // children cannot have their PID recycled until this helper reaps.
            if identity(pid).ok() != Some(before)
                || !fs::read_to_string(&path)?
                    .split_whitespace()
                    .any(|value| value == child)
            {
                continue;
            }
            known.insert(pid, process);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::process::CommandExt;

    #[test]
    #[ignore = "isolated helper process"]
    fn helper_child() {
        helper().unwrap();
    }

    fn launch(command: &str, remaining_ms: u64) -> (std::process::Child, UnixStream) {
        let (mut parent, child) = UnixStream::pair().unwrap();
        let fd = child.as_raw_fd();
        let mut process = Command::new(std::env::current_exe().unwrap());
        process
            .args([
                "--exact",
                "background_jobs::process::tests::helper_child",
                "--ignored",
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        unsafe {
            process.pre_exec(move || {
                if libc::dup2(fd, 3) < 0 || libc::fcntl(3, libc::F_SETFD, 0) < 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let process = process.spawn().unwrap();
        drop(child);
        serde_json::to_writer(
            &mut parent,
            &Request {
                command: command.into(),
                program: None,
                environment: crate::process::Environment::new(),
                deadline_boot_ms: boot_ms().unwrap() + remaining_ms,
            },
        )
        .unwrap();
        parent.write_all(b"\n").unwrap();
        parent
            .set_read_timeout(Some(Duration::from_secs(8)))
            .unwrap();
        (process, parent)
    }

    fn receipt(child: std::process::Child, mut control: UnixStream) -> Receipt {
        let mut bytes = String::new();
        control.read_to_string(&mut bytes).unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_str(&bytes).unwrap()
    }

    #[test]
    fn unreadable_inventory_fails_instead_of_waiting_for_command_exit() {
        let mut known = HashMap::new();
        let result = collect_owned(u32::MAX, &mut known);
        assert!(result.is_err());
        assert!(known.is_empty());
    }

    #[test]
    fn expired_admission_never_launches_command() {
        let directory = tempfile::tempdir().unwrap();
        let marker = directory.path().join("not-started");
        let (child, control) = launch(&format!("touch {}", marker.display()), 0);
        let result = receipt(child, control);
        assert!(result.timed_out);
        assert_eq!(result.exit_code, None);
        assert!(!marker.exists());
    }

    #[test]
    fn final_drain_keeps_tail_beyond_normal_iteration_limit() {
        let (mut sender, receiver) = UnixStream::pair().unwrap();
        let bytes = [vec![b'x'; 150000], b"final-marker".to_vec()].concat();
        assert!(
            unsafe {
                libc::setsockopt(
                    sender.as_raw_fd(),
                    libc::SOL_SOCKET,
                    libc::SO_SNDBUF,
                    (&1_000_000_i32 as *const i32).cast(),
                    std::mem::size_of::<i32>() as libc::socklen_t,
                )
            } == 0
        );
        sender
            .set_write_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        sender.write_all(&bytes).unwrap();
        nonblocking(receiver.as_raw_fd()).unwrap();
        let mut tail = Tail::default();
        drain_final(receiver.as_raw_fd(), &mut tail).unwrap();
        let output = tail.bytes.into_iter().collect::<Vec<_>>();
        assert!(output.ends_with(b"final-marker"));
        assert_eq!(output.len(), LOG_LIMIT);
    }

    #[test]
    fn records_exit_and_bounded_log() {
        let (child, control) = launch(
            "printf first; head -c 32768 /dev/zero | tr '\\000' x; printf last; exit 7",
            5000,
        );
        let result = receipt(child, control);
        assert_eq!(result.exit_code, Some(7));
        assert!(result.log_truncated);
        assert!(result.log_tail.ends_with("last"));
        assert_eq!(result.log_tail.len(), LOG_LIMIT);
        assert!(!result.stopped);
        assert!(!result.timed_out);
    }

    #[test]
    fn stops_adopted_session_after_shell_exit() {
        let directory = tempfile::tempdir().unwrap();
        let marker = directory.path().join("pid");
        let command = format!(
            "setsid sh -c 'echo $$ > {}; exec sleep 30' </dev/null >/dev/null 2>&1 & sleep 0.1",
            marker.display()
        );
        let (child, control) = launch(&command, 5000);
        let result = receipt(child, control);
        assert_eq!(result.exit_code, Some(0));
        let pid: u32 = fs::read_to_string(marker).unwrap().trim().parse().unwrap();
        assert!(!std::path::Path::new(&format!("/proc/{pid}")).exists());
        assert!(result.elapsed_ms < 4000);
    }

    #[test]
    fn deadline_stops_a_job_ignoring_term() {
        let (child, control) = launch("trap '' TERM; while :; do sleep 1; done", 100);
        let result = receipt(child, control);
        assert!(result.timed_out);
        assert_eq!(result.signal, Some(libc::SIGKILL));
        assert!(result.elapsed_ms >= 2000 && result.elapsed_ms < 5000);
    }

    #[test]
    fn control_cancellation_preempts_continuous_output() {
        let (child, control) = launch("yes", 5000);
        control.shutdown(std::net::Shutdown::Write).unwrap();
        let result = receipt(child, control);
        assert!(result.stopped);
        assert!(!result.timed_out);
        assert!(result.elapsed_ms < 4000);
        assert!(result.log_tail.len() <= LOG_LIMIT);
    }
}
