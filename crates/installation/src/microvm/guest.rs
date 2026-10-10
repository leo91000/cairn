//! Guest-only bridge. All filesystem operations here run inside the microVM.
mod codex;
mod filesystems;
mod restored;

use super::{
    plan::{CHAT_INBOX, Plan},
    protocol::{ArchiveFrame, Encoding, Event, GuestRequest, GuestStatus, Reply},
    wire,
};
use crate::{
    error::{Error, Result},
    execution::Sandbox,
    performance::{Operation, StreamMetrics},
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
    time::{Duration, Instant},
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
const INITIALIZED: &str = "/var/lib/cairn/initialized";
const AUTH_SOCKET: &str = "/run/cairn-auth.sock";
const PLAN_FILE: &str = "/run/cairn-plan.json";
const MAX_RESULT_BYTES: usize = 1_000_000;
/// The unprivileged agent user and group.
const AGENT_ID: u32 = 1000;

pub async fn serve(stop: CancellationToken) -> Result<()> {
    let listener = VsockListener::bind(VsockAddr::new(libc::VMADDR_CID_ANY, wire::PORT))?;
    let auth = UnixListener::bind(AUTH_SOCKET)?;
    std::os::unix::fs::chown(AUTH_SOCKET, Some(AGENT_ID), Some(AGENT_ID))?;
    tokio::spawn(relay_auth(auth, stop.clone()));
    tokio::spawn(super::mcp::serve_guest(stop.clone()));
    let running = Arc::new(Mutex::new(()));
    let codex = Arc::new(codex::Codex::default());
    // Native exports can arrive after an attempt detaches its output lease.
    // The VM owns this collector so those timings still reach its private console.
    let telemetry = crate::performance::native::Collector::start().await.ok();
    let result = super::listener::serve(
        || listener.accept(),
        |(stream, _)| {
            let running = running.clone();
            let stopping = stop.clone();
            let codex = codex.clone();
            let timing_endpoint = telemetry
                .as_ref()
                .map(|collector| collector.endpoint().to_owned());
            async move {
                if let Err(error) = handle(stream, running, codex, stopping, timing_endpoint).await
                {
                    tracing::warn!(message = %error.message, "Guest operation failed");
                }
            }
        },
        32,
        stop.clone(),
    )
    .await;
    codex.close().await;
    result.map_err(Error::from)
}

/// Forwards agent authentication requests to the host relay.
async fn relay_auth(auth: UnixListener, stop: CancellationToken) {
    let result = super::listener::serve(
        || auth.accept(),
        |(mut client, _)| async move {
            let relay = async {
                let host = VsockAddr::new(2, wire::PORT + 1);
                let mut remote = VsockStream::connect(host).await?;
                tokio::io::copy_bidirectional(&mut client, &mut remote).await
            };
            let _ = tokio::time::timeout(Duration::from_secs(45), relay).await;
        },
        32,
        stop,
    )
    .await;
    if let Err(error) = result {
        tracing::warn!(%error, "Guest authentication listener stopped");
    }
}

async fn handle(
    stream: VsockStream,
    running: Arc<Mutex<()>>,
    codex: Arc<codex::Codex>,
    stop: CancellationToken,
    timing_endpoint: Option<String>,
) -> Result<()> {
    let (read, mut write) = tokio::io::split(stream);
    let mut read = BufReader::new(read);
    let request = tokio::time::timeout(Duration::from_secs(10), wire::read(&mut read))
        .await
        .map_err(|_| Error::bad("Guest request timed out."))??
        .ok_or_else(|| Error::bad("Missing guest request."))?;
    let request: GuestRequest = wire::decode(request, "Unknown guest operation.")?;
    match request {
        GuestRequest::Freeze => freeze(&mut write, true).await,
        GuestRequest::Thaw => freeze(&mut write, false).await,
        GuestRequest::MountWorkspace => {
            let _running = running
                .try_lock()
                .map_err(|_| Error::conflict("Guest already running."))?;
            let _filesystem = FILESYSTEM_CONTROL.lock().await;
            filesystems::mount().await?;
            wire::write(&mut write, &Reply::ok(true)).await
        }
        GuestRequest::RestoreClone { identity } => {
            let _running = running
                .try_lock()
                .map_err(|_| Error::conflict("Guest already running."))?;
            let _filesystem = FILESYSTEM_CONTROL.lock().await;
            if Path::new(INITIALIZED).exists() || filesystems::mounted() {
                return Err(Error::conflict(
                    "Only an anonymous unmounted VM can be cloned.",
                ));
            }
            restored::validate(&identity)?;
            let mut timing =
                crate::performance::Operation::new("guest_clone", &identity.id, "unfreeze");
            filesystems::freeze(false).await?;
            timing.next("entropy_and_network");
            restored::renew(&identity).await?;
            // VMGenID cannot reset randomness cached by an arbitrary userspace
            // library. Recreate the native process before granting an account.
            timing.next("native_restart");
            codex.restart(stop, timing_endpoint.as_deref()).await?;
            timing.finish();
            wire::write(&mut write, &Reply::ok(true)).await
        }
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
                codex_service: true,
                codex_ready: codex.ready(),
                workspace_disks: true,
                snapshot_clones: restored::kvm_is_deferred(),
            };
            wire::write(&mut write, &status).await
        }
        GuestRequest::WarmCodex => {
            let _guard = running
                .try_lock()
                .map_err(|_| Error::conflict("Guest already running."))?;
            if codex.socket()?.is_none() {
                prepare_anonymous_codex().await?;
                if let Err(error) = codex.warm(stop, timing_endpoint.as_deref()).await {
                    codex.close().await;
                    return Err(error);
                }
            }
            wire::write(&mut write, &Reply::ok(true)).await
        }
        GuestRequest::Import {
            target,
            replace,
            encoding,
            trace_id,
        } => {
            if codex.socket()?.is_some() {
                protect_codex_import(&target, replace)?;
            }
            let import = ImportRequest {
                target: &target,
                replace,
                read_only: false,
                encoding,
                trace_id: trace_id.as_deref(),
            };
            import_archive(&mut read, &mut write, &import).await
        }
        GuestRequest::ProjectImport {
            target,
            read_only,
            encoding,
            trace_id,
        } => {
            if codex.socket()?.is_some() {
                protect_codex_import(&target, false)?;
            }
            let import = ImportRequest {
                target: &target,
                replace: false,
                read_only,
                encoding,
                trace_id: trace_id.as_deref(),
            };
            import_project(&mut read, &mut write, &import).await
        }
        GuestRequest::Run { plan } => {
            let _guard = running
                .try_lock()
                .map_err(|_| Error::conflict("Guest already running."))?;
            let plan = Plan::new(plan);
            prepare_run(&plan).await?;
            let (child, events, output) = spawn_agent(&plan, &codex, timing_endpoint.as_deref())?;
            let result = tokio::select! {
                result = stream_agent(&mut write, child, events, output, &plan) => result,
                () = stop.cancelled() => Ok(()),
            };
            if result.is_err() || stop.is_cancelled() {
                codex.close().await;
            }
            result
        }
        GuestRequest::Shutdown => {
            wire::write(&mut write, &Reply::ok(true)).await?;
            stop.cancel();
            Ok(())
        }
    }
}

fn protect_codex_import(target: &str, replace: bool) -> Result<()> {
    let target = Path::new(target);
    for protected in ["/home/node/.codex", "/run/cairn-entrypoint"] {
        let protected = Path::new(protected);
        let keeps_home = target == Path::new(super::plan::HOME) && !replace;
        if target.starts_with(protected) || (protected.starts_with(target) && !keeps_home) {
            return Err(Error::conflict(
                "Import would replace active Codex state or code.",
            ));
        }
    }
    Ok(())
}

async fn freeze(write: &mut (impl AsyncWrite + Unpin), freeze: bool) -> Result<()> {
    let _guard = FILESYSTEM_CONTROL.lock().await;
    let generation = FREEZE_GENERATION.fetch_add(1, Ordering::SeqCst) + 1;
    let frozen = filesystems::freeze(freeze).await?;
    if freeze && frozen {
        // A lost host control connection must not freeze the guest indefinitely.
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(300)).await;
            let _guard = FILESYSTEM_CONTROL.lock().await;
            if FREEZE_GENERATION.load(Ordering::SeqCst) != generation {
                return;
            }
            let _ = filesystems::freeze(false).await;
        });
    }
    wire::write(write, &Reply::ok(frozen)).await
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
    trace_id: Option<&'a str>,
}

impl ImportRequest<'_> {
    fn trace_id(&self) -> &str {
        self.trace_id
            .filter(|id| uuid::Uuid::parse_str(id).is_ok())
            .unwrap_or("legacy")
    }
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
    let mut timing = Operation::new("guest_import", import.trace_id(), "extract");
    let target = import_target(import.target)?;
    extract(read, target, import, &mut timing).await?;
    timing.next("send_reply");
    wire::write(write, &Reply::ok(true)).await?;
    timing.finish();
    Ok(())
}

/// Projects are staged privately and published with their access policy applied.
async fn import_project(
    read: &mut (impl AsyncBufRead + Unpin),
    write: &mut (impl AsyncWrite + Unpin),
    import: &ImportRequest<'_>,
) -> Result<()> {
    let mut timing = Operation::new("guest_project_import", import.trace_id(), "project_lock");
    let _guard = PROJECT_IMPORT_CONTROL.lock().await;
    timing.next("reopen");
    let destination = import_target(import.target)?;
    if super::projects::reopen(destination, import.read_only).await? {
        timing.next("send_reply");
        wire::write(write, &Reply::ok(true)).await?;
        timing.finish();
        return Ok(());
    }
    timing.next("prepare_staging");
    let staging = destination.with_file_name(format!(
        ".cairn-import-{}",
        crate::auth::hex_digest(import.target)
    ));
    if staging.exists() {
        tokio::fs::remove_dir_all(&staging).await?;
    }
    std::fs::create_dir_all(staging.parent().unwrap())?;
    std::fs::DirBuilder::new().mode(0o700).create(&staging)?;
    timing.next("send_ready");
    wire::write(write, &Reply::ready()).await?;
    let content = staging.join("content");
    extract(read, &content, import, &mut timing).await?;
    // Publish only complete data with its access policy already applied.
    timing.next("publish");
    super::projects::publish(&content, destination, import.read_only).await?;
    if !import.read_only {
        tokio::fs::remove_dir(&staging).await?;
    }
    timing.next("sync");
    Command::new("sync").status().await?;
    timing.next("send_reply");
    wire::write(write, &Reply::ok(true)).await?;
    timing.finish();
    Ok(())
}

async fn extract(
    read: &mut (impl AsyncBufRead + Unpin),
    target: &Path,
    import: &ImportRequest<'_>,
    timing: &mut Operation,
) -> Result<()> {
    timing.next("prepare_target");
    if import.replace && target.exists() {
        tokio::fs::remove_dir_all(target).await?;
    }
    tokio::fs::create_dir_all(target).await?;
    timing.next("spawn_tar");
    let mut child = Command::new("tar")
        .args(["--no-same-owner", "-xf", "-", "-C"])
        .arg(target)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()?;
    let mut input = child.stdin.take().unwrap();
    timing.next("receive_archive");
    let metrics = match import.encoding {
        Encoding::Binary => receive_binary(read, &mut input).await?,
        Encoding::Json => receive_frames(read, &mut input).await?,
    };
    metrics.record(timing.id(), "guest");
    drop(input);
    timing.next("wait_tar");
    if !child.wait().await?.success() {
        return Err(Error::bad("Guest import failed."));
    }
    timing.next("chown");
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
        timing.next("chmod");
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
) -> Result<StreamMetrics> {
    let mut buffer = vec![0; wire::MAX_CHUNK];
    let mut metrics = StreamMetrics::default();
    loop {
        let read_started = Instant::now();
        let count = wire::read_chunk(read, &mut buffer).await?;
        metrics.read += read_started.elapsed();
        if count == 0 {
            return Ok(metrics);
        }
        let write_started = Instant::now();
        input.write_all(&buffer[..count]).await?;
        metrics.write += write_started.elapsed();
        metrics.bytes += count as u64;
        metrics.chunks += 1;
    }
}

async fn receive_frames(
    read: &mut (impl AsyncBufRead + Unpin),
    input: &mut (impl AsyncWrite + Unpin),
) -> Result<StreamMetrics> {
    let mut metrics = StreamMetrics::default();
    loop {
        let read_started = Instant::now();
        let frame = wire::read(read)
            .await?
            .ok_or_else(|| Error::bad("Guest import was interrupted."))?;
        let data = match wire::decode(frame, "Invalid import chunk.")? {
            ArchiveFrame::End => {
                metrics.read += read_started.elapsed();
                return Ok(metrics);
            }
            ArchiveFrame::Chunk { data } => data,
        };
        let bytes = STANDARD
            .decode(data)
            .map_err(|_| Error::bad("Invalid import bytes."))?;
        metrics.read += read_started.elapsed();
        let write_started = Instant::now();
        input.write_all(&bytes).await?;
        metrics.write += write_started.elapsed();
        metrics.bytes += bytes.len() as u64;
        metrics.chunks += 1;
    }
}

/// Marks the disk initialized, saves the plan and applies its filesystem policy.
async fn prepare_run(plan: &Plan) -> Result<()> {
    if plan.as_value()["chat"].is_object()
        && plan.chat_provider() == crate::provider::Provider::Codex
    {
        let timing = Operation::new("codex_state_seed", plan.run_id(), "install");
        let count = seed_codex().await?;
        tracing::info!(target: "cairn_performance", operation = "codex_state_seed", id = plan.run_id(), databases = count);
        timing.finish();
    }
    tokio::fs::create_dir_all("/var/lib/cairn").await?;
    atomic_write(Path::new(INITIALIZED), b"1").await?;
    atomic_write(Path::new(PLAN_FILE), &serde_json::to_vec(plan.as_value())?).await?;
    std::os::unix::fs::chown(PLAN_FILE, Some(AGENT_ID), Some(AGENT_ID))?;
    if plan.sandbox() != Some(Sandbox::Yolo) {
        // Absent on disks where sudo was already revoked.
        let _ = tokio::fs::remove_file("/etc/sudoers.d/cairn").await;
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

async fn seed_codex() -> Result<usize> {
    tokio::task::spawn_blocking(|| {
        super::codex_state::install(
            Path::new("/home/node/.codex"),
            Path::new("/opt/cairn-codex-state"),
            AGENT_ID,
            AGENT_ID,
        )
    })
    .await
    .map_err(|_| Error::unavailable("Codex schema preparation failed."))?
    .map_err(Error::from)
}

/// Warmup never reads a previous conversation or an imported configuration.
async fn prepare_anonymous_codex() -> Result<()> {
    if filesystems::mounted() {
        return Err(Error::conflict(
            "Only a fresh VM can initialize anonymous Codex.",
        ));
    }

    prepare_anonymous_codex_home(
        Path::new("/home/node/.codex"),
        Path::new("/opt/cairn-codex-state"),
        Path::new(INITIALIZED),
        AGENT_ID,
        AGENT_ID,
    )
    .await
}

async fn prepare_anonymous_codex_home(
    home: &Path,
    templates: &Path,
    initialized: &Path,
    uid: u32,
    gid: u32,
) -> Result<()> {
    if initialized.exists() {
        return Err(Error::conflict(
            "Only a fresh VM can initialize anonymous Codex.",
        ));
    }

    match tokio::fs::symlink_metadata(home).await {
        Ok(metadata) if metadata.is_dir() => {
            // This home belongs to the immutable image, not a conversation.
            // Discard inherited CLI/build state before installing account-free schemas.
            tokio::fs::remove_dir_all(home).await?;
            tokio::fs::create_dir(home).await?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            tokio::fs::create_dir(home).await?;
        }
        _ => {
            return Err(Error::conflict(
                "Codex home must be an anonymous directory.",
            ));
        }
    }

    tokio::fs::set_permissions(home, std::os::unix::fs::PermissionsExt::from_mode(0o700)).await?;
    std::os::unix::fs::chown(home, Some(uid), Some(gid))?;

    // This is the same account-free credential-store policy used by managed
    // homes. External tokens still arrive only through an active attempt relay.
    atomic_write(
        &home.join("config.toml"),
        b"cli_auth_credentials_store = \"file\"\n",
    )
    .await?;
    std::os::unix::fs::chown(home.join("config.toml"), Some(uid), Some(gid))?;
    atomic_write(&home.join("cairn-managed-auth"), b"1").await?;

    let home = home.to_owned();
    let templates = templates.to_owned();
    tokio::task::spawn_blocking(move || super::codex_state::install(&home, &templates, uid, gid))
        .await
        .map_err(Error::internal)??;
    Ok(())
}

/// Starts the agent process and streams its output as run events.
fn spawn_agent(
    plan: &Plan,
    codex: &codex::Codex,
    timing_endpoint: Option<&str>,
) -> Result<(Child, mpsc::Receiver<Event>, Option<codex::OutputLease>)> {
    let invocation = plan
        .command()
        .unwrap_or_else(|| vec!["/usr/local/bin/cairn", "runner-entry"]);
    let binary = invocation
        .first()
        .ok_or_else(|| Error::bad("Missing guest command."))?;
    let mut command = Command::new(binary);
    command
        .args(&invocation[1..])
        .env("CAIRN_AUTH_SOCKET", AUTH_SOCKET)
        .env("HOME", "/home/node")
        .env("CODEX_HOME", "/home/node/.codex")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if let Some(endpoint) = timing_endpoint {
        command.env("CAIRN_CODEX_TIMING_ENDPOINT", endpoint);
    }
    let socket = codex.socket()?;
    if let Some(socket) = socket {
        if !plan.as_value()["chat"].is_object()
            || plan.chat_provider() != crate::provider::Provider::Codex
            || plan.command() != Some(vec![super::plan::ENTRYPOINT, "runner-entry"])
        {
            return Err(Error::conflict(
                "Prepared Codex VM requires a managed Codex chat.",
            ));
        }
        command.env("CAIRN_CODEX_SERVICE", socket);
    }
    unprivileged(&mut command);
    let (tx, events) = mpsc::channel(32);
    let output = socket.map(|_| codex.attach(tx.clone())).transpose()?;
    let mut child = command.spawn()?;
    forward_output(child.stdout.take().unwrap(), false, tx.clone());
    forward_output(child.stderr.take().unwrap(), true, tx);
    Ok((child, events, output))
}

fn unprivileged(command: &mut Command) {
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
}

fn forward_event(stderr: bool, bytes: &[u8]) -> Event {
    Event::Output {
        stderr,
        data: STANDARD.encode(bytes),
    }
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
            let event = forward_event(stderr, &buffer[..count]);
            if events.send(event).await.is_err() {
                break;
            }
        }
    });
}

async fn stream_agent(
    write: &mut (impl AsyncWrite + Unpin),
    child: Child,
    events: mpsc::Receiver<Event>,
    output: Option<codex::OutputLease>,
    plan: &Plan,
) -> Result<()> {
    let code = wait_agent(write, child, events, output)
        .await?
        .code()
        .unwrap_or(1);
    let mut timing = Operation::new("guest_finalize", plan.run_id(), "read_result");
    let result = read_result(plan).await?;
    timing.next("codex_state_metadata");
    let usage =
        tokio::task::spawn_blocking(|| super::codex_state::usage(Path::new("/home/node/.codex")))
            .await
            .unwrap_or_default();
    tracing::info!(
        target: "cairn_performance",
        operation = "codex_state_usage",
        id = plan.run_id(),
        state_bytes = usage.state,
        logs_bytes = usage.logs,
        history_bytes = usage.history,
        other_bytes = usage.other,
        complete = usage.complete,
    );
    timing.next("sync_disk");
    sync_disk("/usr/bin/sync").await?;
    timing.next("send_exit");
    let exit = Event::Exit {
        code: Some(code.into()),
        result,
    };
    wire::write(write, &exit).await?;
    timing.finish();
    Ok(())
}

async fn wait_agent(
    write: &mut (impl AsyncWrite + Unpin),
    mut child: Child,
    mut events: mpsc::Receiver<Event>,
    output: Option<codex::OutputLease>,
) -> Result<std::process::ExitStatus> {
    let mut output = output;
    let mut status = None;
    let mut drained = false;
    while status.is_none() || !drained {
        tokio::select! {
            result = child.wait(), if status.is_none() => {
                status = Some(result?);
                drop(output.take());
            },
            event = events.recv(), if !drained => match event {
                Some(event) => wire::write(write, &event).await?,
                None => drained = true,
            },
        }
    }
    Ok(status.unwrap())
}

/// Flush the filesystem holding every persistent overlay. Unlike global sync(),
/// syncfs() reports writeback errors and does not flush unrelated guest mounts.
async fn sync_disk(program: &str) -> Result<()> {
    filesystems::sync(program).await
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn anonymous_warmup_replaces_preinstalled_home_without_inheriting_state() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let templates = root.path().join("schemas");
        tokio::fs::create_dir(&home).await.unwrap();
        tokio::fs::create_dir(&templates).await.unwrap();
        tokio::fs::write(home.join("auth.json"), b"inherited-secret")
            .await
            .unwrap();
        tokio::fs::write(home.join("state_5.sqlite"), b"inherited-state")
            .await
            .unwrap();
        tokio::fs::write(templates.join("state_5.sqlite"), b"schema-only")
            .await
            .unwrap();
        tokio::fs::write(
            templates.join("manifest.json"),
            br#"{"version":1,"files":["state_5.sqlite"]}"#,
        )
        .await
        .unwrap();
        let initialized = root.path().join("initialized");

        prepare_anonymous_codex_home(
            &home,
            &templates,
            &initialized,
            unsafe { libc::geteuid() },
            unsafe { libc::getegid() },
        )
        .await
        .unwrap();

        assert!(!home.join("auth.json").exists());
        assert_eq!(
            tokio::fs::read(home.join("state_5.sqlite")).await.unwrap(),
            b"schema-only"
        );
        assert_eq!(
            tokio::fs::read(home.join("config.toml")).await.unwrap(),
            b"cli_auth_credentials_store = \"file\"\n"
        );

        tokio::fs::write(&initialized, b"1").await.unwrap();

        assert!(
            prepare_anonymous_codex_home(
                &home,
                &templates,
                &initialized,
                unsafe { libc::geteuid() },
                unsafe { libc::getegid() }
            )
            .await
            .is_err()
        );
        assert_eq!(
            tokio::fs::read(home.join("state_5.sqlite")).await.unwrap(),
            b"schema-only"
        );
    }

    #[test]
    fn resident_imports_preserve_open_database_and_executable_paths() {
        for target in [
            "/",
            "/home",
            "/home/node/.codex",
            "/home/node/.codex/state_5.sqlite",
            "/run",
            "/run/cairn-entrypoint",
            "/run/cairn-entrypoint/cairn",
        ] {
            assert!(protect_codex_import(target, false).is_err(), "{target}");
        }
        assert!(protect_codex_import("/home/node", true).is_err());
        for target in [
            "/home/node",
            "/workspaces",
            "/run/cairn-chat",
            "/home/node/project",
        ] {
            assert!(protect_codex_import(target, false).is_ok(), "{target}");
        }
    }

    #[tokio::test]
    async fn failed_disk_flush_is_propagated() {
        assert!(sync_disk("false").await.is_err());
        assert!(sync_disk("true").await.is_ok());
    }

    #[tokio::test]
    async fn measured_import_streams_preserve_bytes_and_reject_missing_end_markers() {
        let chunks: [&[u8]; 2] = [b"archive\0\n{}", b"second chunk"];
        for encoding in [Encoding::Binary, Encoding::Json] {
            let mut encoded = Vec::new();
            for chunk in chunks {
                match encoding {
                    Encoding::Binary => wire::write_chunk(&mut encoded, chunk).await.unwrap(),
                    Encoding::Json => {
                        let frame = ArchiveFrame::Chunk {
                            data: STANDARD.encode(chunk),
                        };
                        wire::write(&mut encoded, &frame).await.unwrap();
                    }
                }
            }
            let truncated = encoded.clone();
            match encoding {
                Encoding::Binary => wire::write_chunk(&mut encoded, &[]).await.unwrap(),
                Encoding::Json => wire::write(&mut encoded, &ArchiveFrame::End).await.unwrap(),
            }
            let mut output = Vec::new();
            let mut read = encoded.as_slice();
            let metrics = match encoding {
                Encoding::Binary => receive_binary(&mut read, &mut output).await.unwrap(),
                Encoding::Json => receive_frames(&mut read, &mut output).await.unwrap(),
            };
            assert_eq!(output, chunks.concat());
            assert_eq!(metrics.bytes, output.len() as u64);
            assert_eq!(metrics.chunks, chunks.len() as u64);

            let mut read = truncated.as_slice();
            let result = match encoding {
                Encoding::Binary => receive_binary(&mut read, &mut Vec::new()).await,
                Encoding::Json => receive_frames(&mut read, &mut Vec::new()).await,
            };
            assert!(result.is_err());
        }
    }

    #[test]
    fn guest_correlation_rejects_arbitrary_strings() {
        for trace_id in [None, Some("credential-or-path"), Some("\nforged-log")] {
            let import = ImportRequest {
                target: "/run/example",
                replace: false,
                read_only: false,
                encoding: Encoding::Binary,
                trace_id,
            };
            assert_eq!(import.trace_id(), "legacy");
        }
    }
}
