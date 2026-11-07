//! Run tests that must own process-global controller state in a fresh process.

pub(crate) fn run_ignored(name: &str) {
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", name, "--ignored", "--nocapture"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success() && stdout.contains("1 passed; 0 failed; 0 ignored"),
        "isolated test {name} did not pass exactly once:\n{stdout}\n{stderr}"
    );
}

/// Run an isolated test with a deadline covering the entire child process,
/// including runtime shutdown and any blocking tasks.
pub(crate) fn run_ignored_with_timeout(name: &str, timeout: std::time::Duration) {
    use std::io::{Read, Seek};

    // Files avoid filling a pipe while the parent waits for the child to exit.
    let mut stdout = tempfile::tempfile().unwrap();
    let mut stderr = tempfile::tempfile().unwrap();
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", name, "--ignored", "--nocapture"])
        .stdout(stdout.try_clone().unwrap())
        .stderr(stderr.try_clone().unwrap())
        .spawn()
        .unwrap();
    let completed = wait_with_timeout(&mut child, timeout).unwrap();
    stdout.rewind().unwrap();
    stderr.rewind().unwrap();
    let mut out = Vec::new();
    let mut err = Vec::new();
    stdout.read_to_end(&mut out).unwrap();
    stderr.read_to_end(&mut err).unwrap();
    let out = String::from_utf8_lossy(&out);
    let err = String::from_utf8_lossy(&err);
    assert!(
        completed
            && child.wait().unwrap().success()
            && out.contains("1 passed; 0 failed; 0 ignored"),
        "isolated test {name} did not pass exactly once (timed out: {}):\n{out}\n{err}",
        !completed
    );
}

fn wait_with_timeout(
    child: &mut std::process::Child,
    timeout: std::time::Duration,
) -> std::io::Result<bool> {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if child.try_wait()?.is_some() {
            return Ok(true);
        }
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            // The child may have exited since try_wait. Always wait after kill,
            // including when kill reports that the process has already exited.
            let kill_result = child.kill();
            child.wait()?;
            if let Err(error) = kill_result
                && error.kind() != std::io::ErrorKind::InvalidInput
            {
                return Err(error);
            }
            return Ok(false);
        }
        std::thread::sleep(remaining.min(std::time::Duration::from_millis(10)));
    }
}

#[cfg(all(test, unix))]
mod tests {
    #[test]
    fn deadline_kills_and_reaps_a_blocked_child() {
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let pid = child.id() as libc::pid_t;
        let started = std::time::Instant::now();
        assert!(
            !super::wait_with_timeout(&mut child, std::time::Duration::from_millis(100)).unwrap()
        );
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
        let mut status = 0;
        // A reaped child is no longer available to waitpid.
        assert_eq!(
            unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) },
            -1
        );
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ECHILD)
        );
    }
}
