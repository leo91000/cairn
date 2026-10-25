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
    command_with_timeout(binary, args, Duration::from_secs(timeout)).await
}

/// Bulk image operations may need a larger bounded deadline than host setup.
pub(crate) async fn command_with_timeout(
    binary: &str,
    args: &[&str],
    timeout: Duration,
) -> Result<()> {
    let output = Command::new(binary)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .output();
    let result = tokio::time::timeout(timeout, output).await.map_err(|_| {
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
    crate::storage::transport::cleanup_stale(state).await?;
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
                "/opt/cairn-vm/root.ext4.zst",
                "-o",
                temporary.to_str().unwrap(),
            ],
        )
        .await?;
        tokio::fs::rename(temporary, &image).await?;
    }
    tokio::fs::set_permissions(&image, std::fs::Permissions::from_mode(0o444)).await?;
    if !target.join("vmlinux").exists() {
        tokio::fs::copy("/opt/cairn-vm/vmlinux", target.join("vmlinux")).await?;
    }
    install_control_image(
        Path::new("/opt/cairn-vm/control.squashfs"),
        &target.join("control.squashfs"),
    )
    .await?;
    Ok(target)
}

async fn install_control_image(source: &Path, target: &Path) -> Result<()> {
    use std::io::ErrorKind;
    match tokio::fs::symlink_metadata(target).await {
        Ok(metadata) if metadata.is_file() && metadata.permissions().mode() & 0o222 == 0 => {
            return Ok(());
        }
        Ok(_) => return Err(Error::conflict("Invalid guest control image.")),
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let temporary = target.with_extension("squashfs.partial");
    match tokio::fs::symlink_metadata(&temporary).await {
        Ok(metadata) if metadata.is_file() => tokio::fs::remove_file(&temporary).await?,
        Ok(_) => return Err(Error::conflict("Invalid partial guest control image.")),
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    tokio::fs::copy(source, &temporary).await?;
    tokio::fs::set_permissions(&temporary, std::fs::Permissions::from_mode(0o444)).await?;
    tokio::fs::File::open(&temporary).await?.sync_all().await?;
    tokio::fs::rename(&temporary, target).await?;
    tokio::fs::File::open(
        target
            .parent()
            .ok_or_else(|| Error::bad("Missing control directory."))?,
    )
    .await?
    .sync_all()
    .await?;
    Ok(())
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

/// Older guests and pinned images with different runner bytes keep the archive path.
pub(super) async fn prepare_entrypoint(
    socket: &Path,
    state: &Path,
    status: &GuestStatus,
) -> Result<()> {
    if status.entrypoint_reuse
        && let Ok(digest) = super::entrypoint::digest().await
    {
        let reply: Reply = call(
            socket,
            &GuestRequest::ReuseEntrypoint {
                digest: digest.to_owned(),
            },
        )
        .await?;
        if reply.succeeded() {
            tracing::info!(target: "cairn_performance", operation = "vm_entrypoint", reused = true);
            return Ok(());
        }
    }
    tracing::info!(target: "cairn_performance", operation = "vm_entrypoint", reused = false);
    let target = Path::new(ENTRYPOINT).parent().unwrap().to_str().unwrap();
    import(socket, &state.join("entrypoint"), target).await
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
        target: "cairn_performance",
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
    let status = status(socket).await?;
    let binary = status.binary_imports;
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
        replace: empty && !(target == HOME && status.codex_ready),
        encoding: Encoding::of(binary),
        trace_id: Some(timing.id().to_owned()),
    };
    timing.next("send_request");
    wire::write(stream.get_mut(), &request).await?;
    transfer(stream, source, target, status, timing).await
}

/// The manager chooses all paths; guest replies never select a host import.
pub async fn import_project(
    socket: &Path,
    source: &Path,
    target: &str,
    read_only: bool,
) -> Result<Value> {
    let mut timing = import_timing(socket, "vm_project_import", "project");
    let status = status(socket).await?;
    let binary = status.binary_imports;
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
    transfer(stream, source, target, status, timing).await?;
    Ok(json!({ "ok": true, "reused": false }))
}

/// Streams a tar archive of `source` and waits for the guest to apply it.
async fn transfer(
    mut stream: BufReader<UnixStream>,
    source: &Path,
    target: &str,
    status: GuestStatus,
    mut timing: Operation,
) -> Result<()> {
    timing.next("spawn_tar");
    let mut tar = Command::new("tar");
    tar.args(["--exclude=cairn-auth.sock", "--exclude=*.sock"]);
    if target == HOME {
        tar.arg("--exclude=./.codex/auth.json");
        if status.codex_ready {
            tar.arg("--exclude=./.codex");
        }
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
        if status.binary_imports {
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
    if status.binary_imports {
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
    super::suspension::set_reason(state, attempt, &super::suspension::Reason::Revoked, true)
        .await
        .map(|_| ())
}

pub async fn resume_attempt(state: &Path, attempt: &str) -> Result<()> {
    super::suspension::set_reason(state, attempt, &super::suspension::Reason::Storage, false)
        .await
        .map(|_| ())
}

pub(super) async fn set_vm_state(socket: &Path, status: &str) -> Result<()> {
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
    super::suspension::settle_reason(
        state,
        attempt,
        &super::suspension::Reason::Storage,
        paused,
        stop,
    )
    .await
    .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::UnixListener;

    #[tokio::test]
    async fn infrastructure_command_uses_its_explicit_deadline() {
        command_with_timeout("sh", &["-c", "sleep 0.02"], Duration::from_secs(5))
            .await
            .unwrap();

        let error = command_with_timeout("sh", &["-c", "exec sleep 10"], Duration::from_millis(20))
            .await
            .unwrap_err();
        assert_eq!(error.status, 503);
        assert_eq!(error.message, "VM infrastructure operation timed out: sh");

        let error = command_with_timeout("sh", &["-c", "exit 7"], Duration::from_secs(5))
            .await
            .unwrap_err();
        assert_eq!(error.message, "VM infrastructure operation failed: sh");
    }

    async fn request(listener: &UnixListener) -> (BufReader<UnixStream>, GuestRequest) {
        let (stream, _) = listener.accept().await.unwrap();
        let mut stream = BufReader::new(stream);
        let mut connect = String::new();
        stream.read_line(&mut connect).await.unwrap();
        assert_eq!(connect, format!("CONNECT {}\n", wire::PORT));
        stream.get_mut().write_all(b"OK 5200\n").await.unwrap();
        let value = wire::read(&mut stream).await.unwrap().unwrap();
        (stream, serde_json::from_value(value).unwrap())
    }

    #[tokio::test]
    async fn matching_guest_skips_the_archive_entirely() {
        let root = tempfile::tempdir().unwrap();
        let socket = root.path().join("vsock");
        let listener = UnixListener::bind(&socket).unwrap();
        let guest = tokio::spawn(async move {
            let (mut stream, request) = request(&listener).await;
            let GuestRequest::ReuseEntrypoint { digest } = request else {
                panic!("Expected entrypoint verification");
            };
            assert_eq!(digest, super::super::entrypoint::digest().await.unwrap());
            wire::write(stream.get_mut(), &Reply::ok(true))
                .await
                .unwrap();
            // No source directory exists: attempting the archive path would fail.
        });
        prepare_entrypoint(
            &socket,
            root.path(),
            &GuestStatus {
                entrypoint_reuse: true,
                ..GuestStatus::default()
            },
        )
        .await
        .unwrap();
        guest.await.unwrap();
    }

    #[tokio::test]
    async fn legacy_and_mismatched_guests_receive_the_current_archive() {
        for capable in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let source = root.path().join("entrypoint");
            tokio::fs::create_dir(&source).await.unwrap();
            tokio::fs::write(source.join("cairn"), b"current-controller-runner")
                .await
                .unwrap();
            let socket = root.path().join("vsock");
            let listener = UnixListener::bind(&socket).unwrap();
            let guest = tokio::spawn(async move {
                if capable {
                    let (mut stream, message) = request(&listener).await;
                    assert!(matches!(message, GuestRequest::ReuseEntrypoint { .. }));
                    wire::write(stream.get_mut(), &Reply::ok(false))
                        .await
                        .unwrap();
                }
                let (mut stream, message) = request(&listener).await;
                assert!(matches!(message, GuestRequest::Status));
                wire::write(
                    stream.get_mut(),
                    &GuestStatus {
                        binary_imports: true,
                        ..GuestStatus::default()
                    },
                )
                .await
                .unwrap();
                let (mut stream, message) = request(&listener).await;
                let GuestRequest::Import {
                    target, encoding, ..
                } = message
                else {
                    panic!("Expected archive fallback");
                };
                assert_eq!(
                    target,
                    Path::new(ENTRYPOINT).parent().unwrap().to_str().unwrap()
                );
                assert_eq!(encoding, Encoding::Binary);
                let mut chunk = vec![0; wire::MAX_CHUNK];
                let mut archive = Vec::new();
                loop {
                    let count = wire::read_chunk(&mut stream, &mut chunk).await.unwrap();
                    if count == 0 {
                        break;
                    }
                    archive.extend_from_slice(&chunk[..count]);
                }
                assert!(
                    archive
                        .windows(b"current-controller-runner".len())
                        .any(|bytes| bytes == b"current-controller-runner")
                );
                wire::write(stream.get_mut(), &Reply::ok(true))
                    .await
                    .unwrap();
            });
            prepare_entrypoint(
                &socket,
                root.path(),
                &GuestStatus {
                    entrypoint_reuse: capable,
                    ..GuestStatus::default()
                },
            )
            .await
            .unwrap();
            guest.await.unwrap();
        }
    }
}

#[cfg(test)]
mod control_tests {
    use super::*;

    #[tokio::test]
    async fn control_image_replaces_readonly_partial_and_keeps_installed_bytes_immutable() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        let target = root.path().join("control.squashfs");
        let partial = root.path().join("control.squashfs.partial");
        tokio::fs::write(&source, b"complete").await.unwrap();
        tokio::fs::write(&partial, b"interrupted").await.unwrap();
        tokio::fs::set_permissions(&partial, std::fs::Permissions::from_mode(0o444))
            .await
            .unwrap();
        install_control_image(&source, &target).await.unwrap();
        assert_eq!(tokio::fs::read(&target).await.unwrap(), b"complete");
        assert_eq!(
            tokio::fs::metadata(&target)
                .await
                .unwrap()
                .permissions()
                .mode()
                & 0o222,
            0
        );
        assert!(!partial.exists());
        tokio::fs::write(&source, b"other runtime").await.unwrap();
        install_control_image(&source, &target).await.unwrap();
        assert_eq!(tokio::fs::read(&target).await.unwrap(), b"complete");
    }

    #[tokio::test]
    async fn control_image_never_follows_a_final_or_partial_symlink() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        let target = root.path().join("control.squashfs");
        let partial = root.path().join("control.squashfs.partial");
        tokio::fs::write(&source, b"kept").await.unwrap();
        for link in [&partial, &target] {
            std::os::unix::fs::symlink(&source, link).unwrap();
            assert!(install_control_image(&source, &target).await.is_err());
            assert_eq!(tokio::fs::read(&source).await.unwrap(), b"kept");
            tokio::fs::remove_file(link).await.unwrap();
        }
        assert!(
            install_control_image(&root.path().join("absent"), &target)
                .await
                .is_err()
        );
        assert!(!target.exists());
    }
}
