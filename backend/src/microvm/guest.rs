//! Guest-only bridge. All filesystem operations here run inside the microVM.
use super::wire;
use crate::{
    error::{Error, Result},
    skills::atomic_write,
    validation::text,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use std::{path::Path, process::Stdio, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt, BufReader},
    net::UnixListener,
    process::Command,
    sync::mpsc,
};
use tokio_util::sync::CancellationToken;
use tokio_vsock::{VsockAddr, VsockListener, VsockStream};

static PROJECT_IMPORT_CONTROL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
static FILESYSTEM_CONTROL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
static FREEZE_GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
const INITIALIZED: &str = "/var/lib/leo/initialized";

pub async fn serve(stop: CancellationToken) -> Result<()> {
    let listener = VsockListener::bind(VsockAddr::new(libc::VMADDR_CID_ANY, wire::PORT))?;
    let auth = UnixListener::bind("/run/leo-auth.sock")?;
    std::os::unix::fs::chown("/run/leo-auth.sock", Some(1000), Some(1000))?;
    let auth_stop = stop.clone();
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = auth_stop.cancelled() => break,
                accepted = auth.accept() => {
                    let Ok((mut client, _)) = accepted else { break };
                    tokio::spawn(async move {
                        if let Ok(mut remote) = VsockStream::connect(VsockAddr::new(2, wire::PORT + 1)).await {
                            let _ = tokio::time::timeout(Duration::from_secs(45), tokio::io::copy_bidirectional(&mut client, &mut remote)).await;
                        }
                    });
                }
            }
        }
    });
    let running = std::sync::Arc::new(tokio::sync::Mutex::new(()));
    loop {
        tokio::select! {
            _ = stop.cancelled() => break,
            accepted = listener.accept() => {
                let (stream, _) = accepted?;
                let running = running.clone();
                let stopping = stop.clone();
                tokio::spawn(async move {
                    if let Err(error) = handle(stream, running, stopping).await {
                        tracing::warn!(message = %error.message, "Guest operation failed");
                    }
                });
            }
        }
    }
    Ok(())
}

async fn handle(
    stream: VsockStream,
    running: std::sync::Arc<tokio::sync::Mutex<()>>,
    stop: CancellationToken,
) -> Result<()> {
    let (read, mut write) = tokio::io::split(stream);
    let mut read = BufReader::new(read);
    let request = wire::read(&mut read)
        .await?
        .ok_or_else(|| Error::bad("Missing guest request."))?;
    match text(&request, "op") {

        "freeze"|"thaw"=> {
            let _guard=FILESYSTEM_CONTROL.lock().await;
            let freeze=request["op"]=="freeze";
            let generation=FREEZE_GENERATION.fetch_add(1,std::sync::atomic::Ordering::SeqCst)+1;
            let status=Command::new("fsfreeze").arg(if freeze {"--freeze"} else {"--unfreeze"}).arg(super::era::DATA_MOUNT).stdout(Stdio::null()).stderr(Stdio::null()).status().await?;
            if freeze && status.success() {
                // A lost host control connection must not freeze the guest indefinitely.
                tokio::spawn(async move {tokio::time::sleep(Duration::from_secs(300)).await;let _guard=FILESYSTEM_CONTROL.lock().await;if FREEZE_GENERATION.load(std::sync::atomic::Ordering::SeqCst)!=generation {return;}let _=Command::new("fsfreeze").args(["--unfreeze",super::era::DATA_MOUNT]).stdout(Stdio::null()).stderr(Stdio::null()).status().await;});
            }
            wire::write(&mut write,&json!({"ok":status.success() || !freeze})).await
        }


        "written" => {
            // Called by the host between freeze and thaw; any failure means a full copy.
            let _guard=FILESYSTEM_CONTROL.lock().await;
            let reply=super::era::written(request["since"].as_u64()).await.unwrap_or_else(|_| json!({"ok":false}));
            wire::write(&mut write,&reply).await
        }
        "artifact-export" => {
            let export=async {
                let (snapshot,size)=crate::artifacts::file::snapshot(Path::new(text(&request,"path")),Path::new(text(&request,"root"))).await?;
                wire::write(&mut write,&json!({"ok":true,"size":size})).await?;
                tokio::io::copy(&mut tokio::fs::File::from_std(snapshot.reopen()?).take(size),&mut write).await?;
                Ok::<(),Error>(())
            };
            match tokio::time::timeout(Duration::from_secs(300),export).await {
                Ok(Ok(()))=>Ok(()),
                _=>wire::write(&mut write,&json!({"ok":false})).await,
            }
        }
        "prepare" => {
            let _guard = running.try_lock().map_err(|_| Error::new(409,"Guest already running."))?;
            if Path::new(INITIALIZED).exists() || Path::new("/home/node/.codex/auth.json").exists() {
                return Err(Error::bad("Only a virgin VM may be prepared."));
            }
            let mut command = Command::new("/usr/local/bin/leo");
            command.arg("guest-warm").env("HOME","/home/node").env("CODEX_HOME","/home/node/.codex")
                .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).kill_on_drop(true);
            unsafe { command.pre_exec(|| {
                if libc::setgroups(0,std::ptr::null()) != 0 || libc::setgid(1000) != 0 || libc::setuid(1000) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            }); }
            let status = command.status().await?;
            wire::write(&mut write,&json!({"ok":status.success()})).await
        }
        "clock" => {
            let epoch = request["epochMs"].as_i64().filter(|v| *v > 0).ok_or_else(|| Error::bad("Invalid guest clock."))?;
            let time = libc::timespec { tv_sec: epoch / 1000, tv_nsec: (epoch % 1000) * 1_000_000 };
            if unsafe { libc::clock_settime(libc::CLOCK_REALTIME,&time) } != 0 { return Err(std::io::Error::last_os_error().into()); }
            wire::write(&mut write,&json!({"ok":true})).await
        }
        "status" => {
            wire::write(
                &mut write,
                &json!({"version":1,"binaryImports":true,"filesystemSnapshots":true,"writeTracking":super::era::active().await,"initialized":Path::new(INITIALIZED).exists()}),
            )
            .await
        }
        "import" | "project-import" => {
            let project = request["op"] == "project-import";
            let _project_guard = if project { Some(PROJECT_IMPORT_CONTROL.lock().await) } else { None };
            let target = Path::new(text(&request, "target"));
            if !target.is_absolute()
                || target
                    .components()
                    .any(|p| matches!(p, std::path::Component::ParentDir))
            {
                return Err(Error::bad("Invalid guest import."));
            }
            if project && super::projects::reopen(target, request["readOnly"] == true).await? {
                return wire::write(&mut write, &json!({"ok":true})).await;
            }
            let destination = target.to_owned();
            let staging = target.with_file_name(format!(
                ".leo-import-{}",
                crate::auth::hex_digest(text(&request, "target"))
            ));
            if project {
                if staging.exists() {
                    tokio::fs::remove_dir_all(&staging).await?;
                }
                use std::os::unix::fs::DirBuilderExt;
                std::fs::create_dir_all(staging.parent().unwrap())?;
                std::fs::DirBuilder::new().mode(0o700).create(&staging)?;
                wire::write(&mut write, &json!({"ready":true})).await?;
            }
            let content = staging.join("content");
            let target = if project { content.as_path() } else { target };
            if request["replace"] == true && target.exists() {
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
            let binary = request["encoding"] == "binary";
            let mut buffer = vec![0; wire::MAX_CHUNK];
            loop {
                if binary {
                    let count = wire::read_chunk(&mut read, &mut buffer).await?;
                    if count == 0 {
                        break;
                    }
                    input.write_all(&buffer[..count]).await?;
                    continue;
                }
                let chunk = wire::read(&mut read)
                    .await?
                    .ok_or_else(|| Error::bad("Guest import was interrupted."))?;
                if chunk["type"] == "end" {
                    break;
                }
                if chunk["type"] != "chunk" {
                    return Err(Error::bad("Invalid import chunk."));
                }
                let bytes = STANDARD
                    .decode(text(&chunk, "data"))
                    .map_err(|_| Error::bad("Invalid import bytes."))?;
                input.write_all(&bytes).await?;
            }
            drop(input);
            if !child.wait().await?.success() {
                return Err(Error::bad("Guest import failed."));
            }
            let status = Command::new("chown")
                .args([
                    "-R",
                    if target == Path::new("/run/leo-chat") {
                        "0:0"
                    } else {
                        "1000:1000"
                    },
                ])
                .arg(target)
                .status()
                .await?;
            if !status.success() {
                return Err(Error::bad("Guest import ownership failed."));
            }
            if target == Path::new("/run/leo-chat") {
                Command::new("chmod")
                    .args(["-R", "u=rwX,go=rX", "/run/leo-chat"])
                    .status()
                    .await?;
            }
            if project {
                // Publish only complete data with its access policy already applied.
                super::projects::publish(target, &destination, request["readOnly"] == true).await?;
                if request["readOnly"] != true { tokio::fs::remove_dir(&staging).await?; }
                Command::new("sync").status().await?;
            }
            wire::write(&mut write, &json!({"ok":true})).await
        }
        "run" => {
            let _guard = running
                .try_lock()
                .map_err(|_| Error::new(409, "Guest already running."))?;
            let plan = &request["plan"];
            tokio::fs::create_dir_all("/var/lib/leo").await?;
            atomic_write(Path::new(INITIALIZED), b"1").await?;
            atomic_write(Path::new("/run/leo-plan.json"), &serde_json::to_vec(plan)?).await?;
            std::os::unix::fs::chown("/run/leo-plan.json", Some(1000), Some(1000))?;
            if plan["sandbox"] != "yolo" {
                let _ = tokio::fs::remove_file("/etc/sudoers.d/leo").await;
            }
            for import in plan["imports"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|m| m["readOnly"] == true && m["target"] != "/run/leo-chat")
            {
                let target = text(import, "target");
                for args in [
                    vec!["--bind", target, target],
                    vec!["-o", "remount,bind,ro", target],
                ] {
                    if !Command::new("mount").args(args).status().await?.success() {
                        return Err(Error::bad("Could not apply guest read-only policy."));
                    }
                }
            }
            {
                let _guard = PROJECT_IMPORT_CONTROL.lock().await;
                super::projects::restore().await?;
            }
            let invocation = plan["command"]
                .as_array()
                .map(|args| args.iter().filter_map(Value::as_str).collect::<Vec<_>>())
                .unwrap_or_else(|| vec!["/usr/local/bin/leo", "runner-entry"]);
            let binary = invocation
                .first()
                .ok_or_else(|| Error::bad("Missing guest command."))?;
            let mut command = Command::new(binary);
            command
                .args(&invocation[1..])
                .env("LEO_AUTH_SOCKET", "/run/leo-auth.sock")
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
                        || libc::setgid(1000) != 0
                        || libc::setuid(1000) != 0
                        || libc::setsid() < 0
                    {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
            let mut child = command.spawn()?;
            let (tx, mut events) = mpsc::channel::<Value>(32);
            for (error, stream) in [
                (
                    false,
                    Box::new(child.stdout.take().unwrap())
                        as Box<dyn tokio::io::AsyncRead + Unpin + Send>,
                ),
                (true, Box::new(child.stderr.take().unwrap())),
            ] {
                let tx = tx.clone();
                tokio::spawn(async move {
                    let mut stream = stream;
                    let mut buffer = vec![0; 32768];
                    while let Ok(count) = stream.read(&mut buffer).await {
                        if count == 0 || tx.send(json!({"type":"output","stderr":error,"data":STANDARD.encode(&buffer[..count])})).await.is_err() { break; }
                    }
                });
            }
            drop(tx);
            let result = async {
                while let Some(event) = events.recv().await {
                    wire::write(&mut write, &event).await?;
                }
                let code = child.wait().await?.code().unwrap_or(1);
                let output = text(&plan["chat"], "output");
                let result = if output.is_empty() {
                    String::new()
                } else {
                    let file = tokio::fs::File::open(output).await;
                    let mut data = Vec::new();
                    if let Ok(file) = file {
                        file.take(1_000_001).read_to_end(&mut data).await?;
                    }
                    if data.len() > 1_000_000 {
                        return Err(Error::bad("Guest result exceeds limit."));
                    }
                    String::from_utf8_lossy(&data).into_owned()
                };
                Command::new("sync").status().await?;
                wire::write(
                    &mut write,
                    &json!({"type":"exit","code":code,"result":result}),
                )
                .await
            };
            tokio::select! { result = result => result, _ = stop.cancelled() => Ok(()) }
        }
        "shutdown" => {
            // A clean stop freezes the data filesystem and archives the last era, so the
            // host can list this boot's writes without booting the disk again.
            let sealed = if super::era::active().await {
                let _guard = FILESYSTEM_CONTROL.lock().await;
                super::era::seal().await.ok()
            } else {
                None
            };
            wire::write(&mut write, &json!({"ok":true,"sealed":sealed})).await?;
            stop.cancel();
            Ok(())
        }
        _ => Err(Error::bad("Unknown guest operation.")),
    }
}
