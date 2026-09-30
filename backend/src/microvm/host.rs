//! Trusted host controller. Only the guest interprets its writable filesystem.
use super::{
    plan::{CHAT_INBOX, ENTRYPOINT, HOME},
    protocol::{ArchiveFrame, Encoding, GuestRequest, GuestStatus, Reply},
    wire,
};
use crate::{
    error::{Error, Result},
    performance::{Operation, StreamMetrics},
    skills::private_dir,
    validation::text,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use std::{
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Stdio,
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::UnixStream,
    process::Command,
};

pub use super::vm::Vm;

pub async fn command(binary: &str, args: &[&str]) -> Result<()> {
    let timeout = if ["ip", "iptables"].contains(&binary) {
        10
    } else {
        180
    };
    let output = Command::new(binary)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .output();
    let result = tokio::time::timeout(Duration::from_secs(timeout), output)
        .await
        .map_err(|_| {
            Error::unavailable(format!("VM infrastructure operation timed out: {binary}"))
        })??;
    // e2fsck returns 1 when it successfully corrected filesystem errors.
    let corrected = binary == "e2fsck" && result.status.code() == Some(1);
    if !result.status.success() && !corrected {
        tracing::warn!(
            binary,
            detail = %String::from_utf8_lossy(&result.stderr),
            "VM infrastructure operation failed"
        );
        return Err(Error::unavailable(format!(
            "VM infrastructure operation failed: {binary}"
        )));
    }
    Ok(())
}

/// Image and runtime identifiers only contain ASCII letters, digits and dashes.
pub(super) fn valid_runtime_name(name: &str) -> bool {
    name.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-')
}

pub async fn assets(state: &Path) -> Result<PathBuf> {
    if !Path::new("/dev/kvm").exists() {
        return Err(Error::unavailable("Firecracker requires /dev/kvm."));
    }
    let version = std::env::var("APP_RUNTIME_ID").unwrap_or_else(|_| "development".into());
    if !valid_runtime_name(&version) {
        return Err(Error::bad("Invalid VM image version."));
    }
    crate::storage::fuse::cleanup_stale(state)?;
    // The exclusive controller lock is already held and its previous container's
    // PID namespace is gone. Remove stale jail hard links before old image caches.
    if state.join("jails").exists() {
        tokio::fs::remove_dir_all(state.join("jails")).await?;
    }
    private_dir(&state.join("images")).await?;
    let target = state.join("images").join(version);
    private_dir(&target).await?;
    let image = target.join("root.ext4");
    if !image.exists() {
        let temporary = target.join("root.ext4.partial");
        command(
            "zstd",
            &[
                "-d",
                "-f",
                "/opt/leo-vm/root.ext4.zst",
                "-o",
                temporary.to_str().unwrap(),
            ],
        )
        .await?;
        tokio::fs::rename(temporary, &image).await?;
    }
    tokio::fs::set_permissions(&image, std::fs::Permissions::from_mode(0o444)).await?;
    if !target.join("vmlinux").exists() {
        tokio::fs::copy("/opt/leo-vm/vmlinux", target.join("vmlinux")).await?;
    }
    Ok(target)
}

pub(super) async fn connect(socket: &Path) -> Result<BufReader<UnixStream>> {
    let mut stream = UnixStream::connect(socket).await?;
    stream
        .write_all(format!("CONNECT {}\n", wire::PORT).as_bytes())
        .await?;
    let mut stream = BufReader::new(stream);
    let mut answer = String::new();
    stream.read_line(&mut answer).await?;
    if !answer.starts_with("OK ") {
        return Err(Error::unavailable("Guest connection is not ready."));
    }
    Ok(stream)
}

pub async fn guest_request(socket: &Path, request: &(impl Serialize + ?Sized)) -> Result<Value> {
    let mut stream = connect(socket).await?;
    wire::write(stream.get_mut(), request).await?;
    wire::read(&mut stream)
        .await?
        .ok_or_else(|| Error::unavailable("Guest disconnected."))
}

/// Sends a request on its own connection and interprets the single reply.
pub(super) async fn call<T: DeserializeOwned>(socket: &Path, request: &GuestRequest) -> Result<T> {
    wire::decode(
        guest_request(socket, request).await?,
        "Invalid guest reply.",
    )
}

pub(super) async fn status(socket: &Path) -> Result<GuestStatus> {
    call(socket, &GuestRequest::Status).await
}

pub(super) async fn read_reply(stream: &mut BufReader<UnixStream>, message: &str) -> Result<Reply> {
    let reply = wire::read(stream)
        .await?
        .ok_or_else(|| Error::bad(message))?;
    wire::decode(reply, message)
}

pub async fn export_artifact(
    socket: &Path,
    path: &str,
    root: &Path,
) -> Result<(BufReader<UnixStream>, u64)> {
    let mut stream = connect(socket).await?;
    let request = GuestRequest::ArtifactExport {
        path: path.to_owned(),
        root: root.to_owned(),
    };
    wire::write(stream.get_mut(), &request).await?;
    let refused = "Guest refused artifact export.";
    let reply = wire::read(&mut stream)
        .await?
        .ok_or_else(|| Error::bad("Guest disconnected."))?;
    let reply: Reply = wire::decode(reply, refused)?;
    let size = reply
        .size
        .filter(|size| *size <= crate::artifacts::file::MAX_FILE)
        .ok_or_else(|| Error::bad(refused))?;
    if !reply.succeeded() {
        return Err(Error::bad(refused));
    }
    Ok((stream, size))
}

fn import_timing(socket: &Path, operation: &'static str, import_kind: &'static str) -> Operation {
    let trace_id = uuid::Uuid::new_v4().to_string();
    let vm_id = socket
        .parent()
        .and_then(Path::parent)
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        .map_or("unknown", crate::performance::identity);
    tracing::info!(
        target: "leo_performance",
        operation,
        id = trace_id,
        vm_id,
        import_kind
    );
    Operation::new(operation, &trace_id, "guest_status")
}

/// Copies `source` over `target` in the guest.
pub(super) async fn import(socket: &Path, source: &Path, target: &str) -> Result<()> {
    let import_kind = if Path::new(ENTRYPOINT).parent() == Some(Path::new(target)) {
        "entrypoint"
    } else if target == HOME {
        "home"
    } else if target == CHAT_INBOX {
        "chat_inbox"
    } else {
        "other"
    };
    let mut timing = import_timing(socket, "vm_import", import_kind);
    let binary = status(socket).await?.binary_imports;
    timing.next("connect");
    let mut stream = connect(socket).await?;
    timing.next("prepare_request");
    let empty = tokio::fs::read_dir(source)
        .await?
        .next_entry()
        .await?
        .is_none();
    let request = GuestRequest::Import {
        target: target.to_owned(),
        replace: empty,
        encoding: Encoding::of(binary),
        trace_id: Some(timing.id().to_owned()),
    };
    timing.next("send_request");
    wire::write(stream.get_mut(), &request).await?;
    transfer(stream, source, target, binary, timing).await
}

/// The manager chooses all paths; guest replies never select a host import.
pub async fn import_project(
    socket: &Path,
    source: &Path,
    target: &str,
    read_only: bool,
) -> Result<Value> {
    let mut timing = import_timing(socket, "vm_project_import", "project");
    let binary = status(socket).await?.binary_imports;
    timing.next("connect");
    let mut stream = connect(socket).await?;
    let request = GuestRequest::ProjectImport {
        target: target.to_owned(),
        read_only,
        encoding: Encoding::of(binary),
        trace_id: Some(timing.id().to_owned()),
    };
    timing.next("send_request");
    wire::write(stream.get_mut(), &request).await?;
    timing.next("wait_ready");
    let response = wire::read(&mut stream)
        .await?
        .ok_or_else(|| Error::unavailable("Guest disconnected."))?;
    let response: Reply = wire::decode(response, "Guest refused project import.")?;
    if response.succeeded() {
        timing.finish();
        return Ok(json!({ "ok": true, "reused": true }));
    }
    if !response.is_ready() {
        return Err(Error::bad("Guest refused project import."));
    }
    transfer(stream, source, target, binary, timing).await?;
    Ok(json!({ "ok": true, "reused": false }))
}

/// Streams a tar archive of `source` and waits for the guest to apply it.
async fn transfer(
    mut stream: BufReader<UnixStream>,
    source: &Path,
    target: &str,
    binary: bool,
    mut timing: Operation,
) -> Result<()> {
    timing.next("spawn_tar");
    let mut tar = Command::new("tar");
    tar.args(["--exclude=leo-auth.sock", "--exclude=*.sock"]);
    if target == HOME {
        tar.arg("--exclude=./.codex/auth.json");
    }
    tar.arg("-C");
    tar.arg(source).args(["-cf", "-", "."]);
    let mut child = tar
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()?;
    let mut archive = child.stdout.take().unwrap();
    let mut buffer = vec![0; wire::MAX_CHUNK];
    let mut metrics = StreamMetrics::default();
    timing.next("stream_archive");
    loop {
        let read_started = Instant::now();
        let count = archive.read(&mut buffer).await?;
        metrics.read += read_started.elapsed();
        if count == 0 {
            break;
        }
        let write_started = Instant::now();
        if binary {
            wire::write_chunk(stream.get_mut(), &buffer[..count]).await?;
        } else {
            let frame = ArchiveFrame::Chunk {
                data: STANDARD.encode(&buffer[..count]),
            };
            wire::write(stream.get_mut(), &frame).await?;
        }
        metrics.write += write_started.elapsed();
        metrics.bytes += count as u64;
        metrics.chunks += 1;
    }
    metrics.record(timing.id(), "host");
    timing.next("send_end");
    if binary {
        wire::write_chunk(stream.get_mut(), &[]).await?;
    } else {
        wire::write(stream.get_mut(), &ArchiveFrame::End).await?;
    }
    timing.next("wait_tar");
    let code = child.wait().await?.code();
    if !matches!(code, Some(0 | 1)) {
        return Err(Error::bad("Workspace import failed."));
    }
    timing.next("wait_guest_reply");
    let result = read_reply(&mut stream, "Guest import disconnected.").await?;
    if !result.succeeded() {
        return Err(Error::bad("Guest import failed."));
    }
    timing.finish();
    Ok(())
}

/// Pause all guest CPUs before lease-loss teardown, including non-agent processes.
pub async fn pause_attempt(state: &Path, attempt: &str) -> Result<()> {
    vm_state(state, attempt, "Paused").await
}

pub async fn resume_attempt(state: &Path, attempt: &str) -> Result<()> {
    vm_state(state, attempt, "Resumed").await
}

async fn vm_state(state: &Path, attempt: &str, status: &str) -> Result<()> {
    let value: Value =
        serde_json::from_slice(&tokio::fs::read(state.join(format!("{attempt}.vm.json"))).await?)?;
    let vm = text(&value, "vmId");
    crate::validation::uuid(vm)?;
    let socket = state
        .join("jails/firecracker")
        .join(vm)
        .join("root/api.sock");
    let request = async {
        let transport = |_| Error::timeout("VM state response was lost; retry confirmation.");
        let mut stream = UnixStream::connect(socket).await.map_err(transport)?;
        let body = json!({ "state": status }).to_string();
        let length = body.len();
        stream
            .write_all(
                format!(
                    "PATCH /vm HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {length}\r\n\r\n{body}"
                )
                .as_bytes(),
            )
            .await.map_err(transport)?;
        let mut read = BufReader::new(stream);
        let mut line = String::new();
        read.read_line(&mut line).await.map_err(transport)?;
        if line.is_empty() {
            return Err(Error::timeout(
                "VM state response was lost; retry confirmation.",
            ));
        }
        if !line.starts_with("HTTP/1.1 204 ") {
            return Err(Error::unavailable("VM pause failed."));
        }
        Ok(())
    };
    tokio::time::timeout(Duration::from_secs(1), request)
        .await
        .map_err(|_| Error::timeout("VM state transition is not yet confirmed."))?
}

/// Checkpoint transitions must settle before sealing/thawing. Repeat the same
/// idempotent desired state after a lost response; cancellation still fences
/// lease loss and shutdown. The monitor uses one bounded request per tick.
pub async fn settle_attempt(
    state: &Path,
    attempt: &str,
    paused: bool,
    stop: &tokio_util::sync::CancellationToken,
) -> Result<()> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        let result = tokio::select! {
            () = stop.cancelled() => return Err(Error::conflict("Execution stopped during VM transition.")),
            result = vm_state(state, attempt, if paused { "Paused" } else { "Resumed" }) => result,
        };
        match result {
            Err(error) if error.status == 408 && tokio::time::Instant::now() < deadline => {}
            result => return result,
        }
        tokio::select! {
            () = stop.cancelled() => return Err(Error::conflict("Execution stopped during VM transition.")),
            () = tokio::time::sleep(Duration::from_millis(100)) => {},
        }
    }
}
