use crate::{
    error::{Error, Result},
    process::{Environment, command},
};
use std::{
    os::fd::{AsRawFd, FromRawFd},
    path::Path,
    process::Stdio,
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::UnixStream,
    process::Child,
};

pub struct Supervised {
    pub child: Child,
    control: UnixStream,
}

impl Supervised {
    pub async fn spawn(
        binary: &str,
        args: &[String],
        env: &Environment,
        cwd: &Path,
    ) -> Result<Self> {
        let (parent, child_control) = std::os::unix::net::UnixStream::pair()?;
        parent.set_nonblocking(true)?;
        let executable = std::env::current_exe()?;
        let mut command = command(
            executable
                .to_str()
                .ok_or_else(|| Error::internal("Invalid executable path"))?,
            &[vec!["supervise".into(), binary.into()], args.to_vec()].concat(),
            env,
            Some(cwd),
        );
        // Dropping the owner closes the control socket. The supervisor must stay
        // alive long enough to terminate and reap its entire child process group.
        command
            .stdin(Stdio::piped())
            .process_group(0)
            .kill_on_drop(false);
        let fd = child_control.as_raw_fd();
        // Only async-signal-safe syscalls run between fork and exec. FD 3 is a
        // private parent-liveness channel and never reaches the agent process.
        unsafe {
            command.pre_exec(move || {
                if libc::dup2(fd, 3) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::fcntl(3, libc::F_SETFD, 0) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = command.spawn()?;
        drop(child_control);
        Ok(Self {
            child,
            control: UnixStream::from_std(parent)?,
        })
    }

    pub async fn start(&mut self, prompt: String) -> Result<()> {
        self.control.write_all(b"start\n").await?;
        if let Some(mut stdin) = self.child.stdin.take() {
            tokio::spawn(async move {
                let _ = stdin.write_all(prompt.as_bytes()).await;
            });
        }
        Ok(())
    }

    pub async fn stop(&mut self) {
        let _ = self.control.shutdown().await;
        if let Some(pid) = self.child.id() {
            unsafe {
                libc::kill(pid as i32, libc::SIGTERM);
            }
        }
        if tokio::time::timeout(Duration::from_secs(4), self.child.wait())
            .await
            .is_err()
        {
            let _ = self.child.kill().await;
        }
    }
}

fn signal_group(pid: u32, signal: i32) {
    unsafe {
        libc::kill(-(pid as i32), signal);
    }
}

pub async fn entry(binary: &str, args: &[String]) -> Result<i32> {
    let stream = unsafe { std::os::unix::net::UnixStream::from_raw_fd(3) };
    stream.set_nonblocking(true)?;
    // Child executables must not inherit the ownership channel.
    unsafe {
        if libc::fcntl(3, libc::F_SETFD, libc::FD_CLOEXEC) < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
    }
    let mut control = BufReader::new(UnixStream::from_std(stream)?);
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let mut interrupt = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
    let mut start = String::new();
    tokio::select! {
        read = control.read_line(&mut start) => {
            read?;
            if start != "start\n" {
                return Ok(143);
            }
        }
        _ = terminate.recv() => return Ok(143),
        _ = interrupt.recv() => return Ok(143),
    }
    let mut child = tokio::process::Command::new(binary)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .process_group(0)
        .kill_on_drop(true)
        .spawn()
        .map_err(|_| Error::new(503, "Unable to start the run process."))?;
    let pid = child.id().unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let input = tokio::spawn(async move {
        let _ = tokio::io::copy(&mut tokio::io::stdin(), &mut stdin).await;
    });
    let mut byte = [0; 1];
    let result = tokio::select! {
        result = child.wait() => Some(result?),
        _ = control.read(&mut byte) => None,
        _ = terminate.recv() => None,
        _ = interrupt.recv() => None,
    };
    signal_group(pid, libc::SIGTERM);
    let status = if let Some(status) = result {
        status
    } else {
        match tokio::time::timeout(Duration::from_secs(2), child.wait()).await {
            Ok(result) => result?,
            Err(_) => {
                signal_group(pid, libc::SIGKILL);
                child.wait().await?
            }
        }
    };
    // Reap a command's remaining background descendants before closing output.
    signal_group(pid, libc::SIGKILL);
    input.abort();
    Ok(status.code().unwrap_or(143))
}
