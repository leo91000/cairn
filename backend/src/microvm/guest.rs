//! Guest-only bridge. All filesystem operations here run inside the microVM.
use super::{
    plan::{CHAT_INBOX, Plan},
    protocol::{ArchiveFrame, Encoding, Event, GuestRequest, GuestStatus, Reply},
    wire,
};
use crate::{
    error::{Error, Result},
    execution::Sandbox,
    skills::atomic_write,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use std::{
    os::unix::fs::DirBuilderExt,
    path::Path,
    process::Stdio,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncBufRead, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader},
    net::UnixListener,
    process::{Child, Command},
    sync::{Mutex, mpsc},
};
use tokio_util::sync::CancellationToken;
use tokio_vsock::{VsockAddr, VsockListener, VsockStream};

static PROJECT_IMPORT_CONTROL: Mutex<()> = Mutex::const_new(());
static FILESYSTEM_CONTROL: Mutex<()> = Mutex::const_new(());
static FREEZE_GENERATION: AtomicU64 = AtomicU64::new(0);
const INITIALIZED: &str = "/var/lib/leo/initialized";
const AUTH_SOCKET: &str = "/run/leo-auth.sock";
const PLAN_FILE: &str = "/run/leo-plan.json";
const DATA_MOUNT: &str = "/oldroot/run/data";
const MAX_RESULT_BYTES: usize = 1_000_000;
/// The unprivileged agent user and group.
const AGENT_ID: u32 = 1000;

pub async fn serve(stop: CancellationToken) -> Result<()> {
    let listener = VsockListener::bind(VsockAddr::new(libc::VMADDR_CID_ANY, wire::PORT))?;
    let auth = UnixListener::bind(AUTH_SOCKET)?;
    std::os::unix::fs::chown(AUTH_SOCKET, Some(AGENT_ID), Some(AGENT_ID))?;
    tokio::spawn(relay_auth(auth, stop.clone()));
    let running = Arc::new(Mutex::new(()));
    loop {
        let accepted = tokio::select! {
            () = stop.cancelled() => break,
            accepted = listener.accept() => accepted,
        };
        let (stream, _) = accepted?;
        let running = running.clone();
        let stopping = stop.clone();
        tokio::spawn(async move {
            if let Err(error) = handle(stream, running, stopping).await {
                tracing::warn!(message = %error.message, "Guest operation failed");
            }
        });
    }
    Ok(())
}

/// Forwards agent authentication requests to the host relay.
async fn relay_auth(auth: UnixListener, stop: CancellationToken) {
    loop {
        let accepted = tokio::select! {
            () = stop.cancelled() => break,
            accepted = auth.accept() => accepted,
        };
        let Ok((mut client, _)) = accepted else {
            break;
        };
        tokio::spawn(async move {
            let host = VsockAddr::new(2, wire::PORT + 1);
            if let Ok(mut remote) = VsockStream::connect(host).await {
                let relay = tokio::io::copy_bidirectional(&mut client, &mut remote);
                let _ = tokio::time::timeout(Duration::from_secs(45), relay).await;
            }
        });
    }
}

async fn handle(
    stream: VsockStream,
    running: Arc<Mutex<()>>,
    stop: CancellationToken,
) -> Result<()> {
    let (read, mut write) = tokio::io::split(stream);
    let mut read = BufReader::new(read);
    let request = wire::read(&mut read)
        .await?
        .ok_or_else(|| Error::bad("Missing guest request."))?;
    let request: GuestRequest = wire::decode(request, "Unknown guest operation.")?;
    match request {
        GuestRequest::Freeze => freeze(&mut write, true).await,
        GuestRequest::Thaw => freeze(&mut write, false).await,
        GuestRequest::ArtifactExport { path, root } => {
            export_artifact(&mut write, Path::new(&path), &root).await
        }
        GuestRequest::Clock { epoch_ms } => set_clock(&mut write, epoch_ms).await,
        GuestRequest::Status => {
            let status = GuestStatus {
                version: 1,
                binary_imports: true,
                filesystem_snapshots: true,
                initialized: Path::new(INITIALIZED).exists(),
            };
            wire::write(&mut write, &status).await
        }
        GuestRequest::Import {
            target,
            replace,
            encoding,
        } => {
            let import = ImportRequest {
                target: &target,
                replace,
                read_only: false,
                encoding,
            };
            import_archive(&mut read, &mut write, &import).await
        }
        GuestRequest::ProjectImport {
            target,
            read_only,
            encoding,
        } => {
            let import = ImportRequest {
                target: &target,
                replace: false,
                read_only,
                encoding,
            };
            import_project(&mut read, &mut write, &import).await
        }
        GuestRequest::Run { plan } => {
            let _guard = running
                .try_lock()
                .map_err(|_| Error::conflict("Guest already running."))?;
            let plan = Plan::new(plan);
            prepare_run(&plan).await?;
            let (child, events) = spawn_agent(&plan)?;
            tokio::select! {
                result = stream_agent(&mut write, child, events, &plan) => result,
                () = stop.cancelled() => Ok(()),
            }
        }
        GuestRequest::Shutdown => {
            wire::write(&mut write, &Reply::ok(true)).await?;
            stop.cancel();
            Ok(())
        }
    }
}

async fn fsfreeze(flag: &str) -> std::io::Result<std::process::ExitStatus> {
    Command::new("fsfreeze")
        .arg(flag)
        .arg(DATA_MOUNT)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await
}

async fn freeze(write: &mut (impl AsyncWrite + Unpin), freeze: bool) -> Result<()> {
    let _guard = FILESYSTEM_CONTROL.lock().await;
    let generation = FREEZE_GENERATION.fetch_add(1, Ordering::SeqCst) + 1;
    let status = fsfreeze(if freeze { "--freeze" } else { "--unfreeze" }).await?;
    if freeze && status.success() {
        // A lost host control connection must not freeze the guest indefinitely.
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(300)).await;
            let _guard = FILESYSTEM_CONTROL.lock().await;
            if FREEZE_GENERATION.load(Ordering::SeqCst) != generation {
                return;
            }
            let _ = fsfreeze("--unfreeze").await;
        });
    }
    wire::write(write, &Reply::ok(status.success() || !freeze)).await
}

async fn export_artifact(
    write: &mut (impl AsyncWrite + Unpin),
    path: &Path,
    root: &Path,
) -> Result<()> {
    let export = async {
        let (snapshot, size) = crate::artifacts::file::snapshot(path, root).await?;
        wire::write(write, &Reply::export(size)).await?;
        let mut file = tokio::fs::File::from_std(snapshot.reopen()?).take(size);
        tokio::io::copy(&mut file, write).await?;
        Ok::<(), Error>(())
    };
    if let Ok(Ok(())) = tokio::time::timeout(Duration::from_secs(300), export).await {
        return Ok(());
    }
    wire::write(write, &Reply::ok(false)).await
}

async fn set_clock(write: &mut (impl AsyncWrite + Unpin), epoch_ms: i64) -> Result<()> {
    if epoch_ms <= 0 {
        return Err(Error::bad("Invalid guest clock."));
    }
    let time = libc::timespec {
        tv_sec: epoch_ms / 1000,
        tv_nsec: (epoch_ms % 1000) * 1_000_000,
    };
    if unsafe { libc::clock_settime(libc::CLOCK_REALTIME, &time) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    wire::write(write, &Reply::ok(true)).await
}

struct ImportRequest<'a> {
    target: &'a str,
    replace: bool,
    read_only: bool,
    encoding: Encoding,
}

fn import_target(target: &str) -> Result<&Path> {
    let path = Path::new(target);
    let escapes = path
        .components()
        .any(|part| matches!(part, std::path::Component::ParentDir));
    if !path.is_absolute() || escapes {
        return Err(Error::bad("Invalid guest import."));
    }
    Ok(path)
}

/// Extracts the archive that follows an import request directly over its target.
async fn import_archive(
    read: &mut (impl AsyncBufRead + Unpin),
    write: &mut (impl AsyncWrite + Unpin),
    import: &ImportRequest<'_>,
) -> Result<()> {
    let target = import_target(import.target)?;
    extract(read, target, import).await?;
    wire::write(write, &Reply::ok(true)).await
}

/// Projects are staged privately and published with their access policy applied.
async fn import_project(
    read: &mut (impl AsyncBufRead + Unpin),
    write: &mut (impl AsyncWrite + Unpin),
    import: &ImportRequest<'_>,
) -> Result<()> {
    let _guard = PROJECT_IMPORT_CONTROL.lock().await;
    let destination = import_target(import.target)?;
    if super::projects::reopen(destination, import.read_only).await? {
        return wire::write(write, &Reply::ok(true)).await;
    }
    let staging = destination.with_file_name(format!(
        ".leo-import-{}",
        crate::auth::hex_digest(import.target)
    ));
    if staging.exists() {
        tokio::fs::remove_dir_all(&staging).await?;
    }
    std::fs::create_dir_all(staging.parent().unwrap())?;
    std::fs::DirBuilder::new().mode(0o700).create(&staging)?;
    wire::write(write, &Reply::ready()).await?;
    let content = staging.join("content");
    extract(read, &content, import).await?;
    // Publish only complete data with its access policy already applied.
    super::projects::publish(&content, destination, import.read_only).await?;
    if !import.read_only {
        tokio::fs::remove_dir(&staging).await?;
    }
    Command::new("sync").status().await?;
    wire::write(write, &Reply::ok(true)).await
}

async fn extract(
    read: &mut (impl AsyncBufRead + Unpin),
    target: &Path,
    import: &ImportRequest<'_>,
) -> Result<()> {
    if import.replace && target.exists() {
        tokio::fs::remove_dir_all(target).await?;
    }
    tokio::fs::create_dir_all(target).await?;
    let mut child = Command::new("tar")
        .args(["--no-same-owner", "-xf", "-", "-C"])
        .arg(target)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()?;
    let mut input = child.stdin.take().unwrap();
    match import.encoding {
        Encoding::Binary => receive_binary(read, &mut input).await?,
        Encoding::Json => receive_frames(read, &mut input).await?,
    }
    drop(input);
    if !child.wait().await?.success() {
        return Err(Error::bad("Guest import failed."));
    }
    let chat = target == Path::new(CHAT_INBOX);
    let owner = if chat { "0:0" } else { "1000:1000" };
    let status = Command::new("chown")
        .args(["-R", owner])
        .arg(target)
        .status()
        .await?;
    if !status.success() {
        return Err(Error::bad("Guest import ownership failed."));
    }
    if chat {
        Command::new("chmod")
            .args(["-R", "u=rwX,go=rX", CHAT_INBOX])
            .status()
            .await?;
    }
    Ok(())
}

async fn receive_binary(
    read: &mut (impl AsyncRead + Unpin),
    input: &mut (impl AsyncWrite + Unpin),
) -> Result<()> {
    let mut buffer = vec![0; wire::MAX_CHUNK];
    loop {
        let count = wire::read_chunk(read, &mut buffer).await?;
        if count == 0 {
            return Ok(());
        }
        input.write_all(&buffer[..count]).await?;
    }
}

async fn receive_frames(
    read: &mut (impl AsyncBufRead + Unpin),
    input: &mut (impl AsyncWrite + Unpin),
) -> Result<()> {
    loop {
        let frame = wire::read(read)
            .await?
            .ok_or_else(|| Error::bad("Guest import was interrupted."))?;
        let data = match wire::decode(frame, "Invalid import chunk.")? {
            ArchiveFrame::End => return Ok(()),
            ArchiveFrame::Chunk { data } => data,
        };
        let bytes = STANDARD
            .decode(data)
            .map_err(|_| Error::bad("Invalid import bytes."))?;
        input.write_all(&bytes).await?;
    }
}

/// Marks the disk initialized, saves the plan and applies its filesystem policy.
async fn prepare_run(plan: &Plan) -> Result<()> {
    tokio::fs::create_dir_all("/var/lib/leo").await?;
    atomic_write(Path::new(INITIALIZED), b"1").await?;
    atomic_write(Path::new(PLAN_FILE), &serde_json::to_vec(plan.as_value())?).await?;
    std::os::unix::fs::chown(PLAN_FILE, Some(AGENT_ID), Some(AGENT_ID))?;
    if plan.sandbox() != Some(Sandbox::Yolo) {
        // Absent on disks where sudo was already revoked.
        let _ = tokio::fs::remove_file("/etc/sudoers.d/leo").await;
    }
    let read_only = plan
        .imports()
        .filter(|import| import.read_only && import.target != CHAT_INBOX);
    for import in read_only {
        let target = import.target;
        for args in [
            ["--bind", target, target],
            ["-o", "remount,bind,ro", target],
        ] {
            if !Command::new("mount").args(args).status().await?.success() {
                return Err(Error::bad("Could not apply guest read-only policy."));
            }
        }
    }
    let _guard = PROJECT_IMPORT_CONTROL.lock().await;
    super::projects::restore().await
}

/// Starts the agent process and streams its output as run events.
fn spawn_agent(plan: &Plan) -> Result<(Child, mpsc::Receiver<Event>)> {
    let invocation = plan
        .command()
        .unwrap_or_else(|| vec!["/usr/local/bin/leo", "runner-entry"]);
    let binary = invocation
        .first()
        .ok_or_else(|| Error::bad("Missing guest command."))?;
    let mut command = Command::new(binary);
    command
        .args(&invocation[1..])
        .env("LEO_AUTH_SOCKET", AUTH_SOCKET)
        .env("HOME", "/home/node")
        .env("CODEX_HOME", "/home/node/.codex")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    // Clear inherited supplemental groups and give each execution its own process group.
    unsafe {
        command.pre_exec(|| {
            if libc::setgroups(0, std::ptr::null()) != 0
                || libc::setgid(AGENT_ID) != 0
                || libc::setuid(AGENT_ID) != 0
                || libc::setsid() < 0
            {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command.spawn()?;
    let (tx, events) = mpsc::channel(32);
    forward_output(child.stdout.take().unwrap(), false, tx.clone());
    forward_output(child.stderr.take().unwrap(), true, tx);
    Ok((child, events))
}

fn forward_output(
    mut stream: impl AsyncRead + Unpin + Send + 'static,
    stderr: bool,
    events: mpsc::Sender<Event>,
) {
    tokio::spawn(async move {
        let mut buffer = vec![0; 32768];
        while let Ok(count) = stream.read(&mut buffer).await {
            if count == 0 {
                break;
            }
            let event = Event::Output {
                stderr,
                data: STANDARD.encode(&buffer[..count]),
            };
            if events.send(event).await.is_err() {
                break;
            }
        }
    });
}

async fn stream_agent(
    write: &mut (impl AsyncWrite + Unpin),
    mut child: Child,
    mut events: mpsc::Receiver<Event>,
    plan: &Plan,
) -> Result<()> {
    while let Some(event) = events.recv().await {
        wire::write(write, &event).await?;
    }
    let code = child.wait().await?.code().unwrap_or(1);
    let result = read_result(plan).await?;
    Command::new("sync").status().await?;
    let exit = Event::Exit {
        code: Some(code.into()),
        result,
    };
    wire::write(write, &exit).await
}

/// The chat result file written by the agent, if this run is a chat.
async fn read_result(plan: &Plan) -> Result<String> {
    let Some(output) = plan.chat_output().filter(|output| !output.is_empty()) else {
        return Ok(String::new());
    };
    let mut data = Vec::new();
    if let Ok(file) = tokio::fs::File::open(output).await {
        file.take(MAX_RESULT_BYTES as u64 + 1)
            .read_to_end(&mut data)
            .await?;
    }
    if data.len() > MAX_RESULT_BYTES {
        return Err(Error::bad("Guest result exceeds limit."));
    }
    Ok(String::from_utf8_lossy(&data).into_owned())
}
