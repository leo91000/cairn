//! Trusted host controller. Only the guest interprets its writable filesystem.
use super::wire;
use crate::{
    error::{Error, Result},
    skills::{atomic_write, private_dir},
    validation::text,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use std::{
    os::fd::AsRawFd,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::{UnixListener, UnixStream},
    process::Command,
};
use tokio_util::sync::CancellationToken;

pub async fn command(binary: &str, args: &[&str]) -> Result<()> {
    let result = tokio::time::timeout(
        Duration::from_secs(if ["ip", "iptables"].contains(&binary) {
            10
        } else {
            180
        }),
        Command::new(binary)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .map_err(|_| {
        Error::unavailable(format!("VM infrastructure operation timed out: {binary}"))
    })??;
    if !result.status.success() {
        tracing::warn!(binary, detail=%String::from_utf8_lossy(&result.stderr), "VM infrastructure operation failed");
        return Err(Error::unavailable(format!(
            "VM infrastructure operation failed: {binary}"
        )));
    }
    Ok(())
}

pub async fn assets(state: &Path) -> Result<PathBuf> {
    if !Path::new("/dev/kvm").exists() {
        return Err(Error::unavailable("Firecracker requires /dev/kvm."));
    }
    let version = std::env::var("APP_RUNTIME_ID").unwrap_or_else(|_| "development".into());
    if !version
        .bytes()
        .all(|c| c.is_ascii_alphanumeric() || c == b'-')
    {
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
    use std::os::unix::fs::PermissionsExt;
    tokio::fs::set_permissions(&image, std::fs::Permissions::from_mode(0o444)).await?;
    if !target.join("vmlinux").exists() {
        tokio::fs::copy("/opt/leo-vm/vmlinux", target.join("vmlinux")).await?;
    }
    Ok(target)
}

async fn connect(socket: &Path) -> Result<BufReader<UnixStream>> {
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

pub async fn guest_request(socket: &Path, request: &Value) -> Result<Value> {
    let mut stream = connect(socket).await?;
    wire::write(stream.get_mut(), request).await?;
    wire::read(&mut stream)
        .await?
        .ok_or_else(|| Error::unavailable("Guest disconnected."))
}

pub async fn export_artifact(
    socket: &Path,
    path: &str,
    root: &Path,
) -> Result<(BufReader<UnixStream>, u64)> {
    let mut stream = connect(socket).await?;
    wire::write(
        stream.get_mut(),
        &json!({"op": "artifact-export","path": path,"root": root}),
    )
    .await?;
    let reply = wire::read(&mut stream)
        .await?
        .ok_or_else(|| Error::bad("Guest disconnected."))?;
    let size = reply["size"]
        .as_u64()
        .filter(|s| *s <= crate::artifacts::file::MAX_FILE)
        .ok_or_else(|| Error::bad("Guest refused artifact export."))?;
    if reply["ok"] != true {
        return Err(Error::bad("Guest refused artifact export."));
    }
    Ok((stream, size))
}

fn console(
    mut stream: impl tokio::io::AsyncRead + Unpin + Send + 'static,
    path: PathBuf,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        use tokio::io::AsyncReadExt;
        if let Ok(mut log) = tokio::fs::File::create(path).await {
            let _ = tokio::io::copy(&mut (&mut stream).take(4 * 1024 * 1024), &mut log).await;
        }
        // Drain excess guest console output without allowing it to fill host storage.
        let _ = tokio::io::copy(&mut stream, &mut tokio::io::sink()).await;
    })
}

async fn import(socket: &Path, source: &Path, target: &str) -> Result<()> {
    let binary = guest_request(socket, &json!({"op": "status"})).await?["binaryImports"] == true;
    let mut stream = connect(socket).await?;
    let empty = tokio::fs::read_dir(source)
        .await?
        .next_entry()
        .await?
        .is_none();
    wire::write(
        stream.get_mut(),
        &json!({
            "op": "import",
            "target": target,
            "replace": empty,
            "encoding": if binary {"binary"} else {"json"}
        }),
    )
    .await?;
    transfer(stream, source, target, binary).await
}

/// The manager chooses all paths; guest replies never select a host import.
pub async fn import_project(
    socket: &Path,
    source: &Path,
    target: &str,
    read_only: bool,
) -> Result<Value> {
    let binary = guest_request(socket, &json!({"op": "status"})).await?["binaryImports"] == true;
    let mut stream = connect(socket).await?;
    wire::write(
        stream.get_mut(),
        &json!({
            "op": "project-import",
            "target": target,
            "readOnly": read_only,
            "encoding": if binary {"binary"} else {"json"}
        }),
    )
    .await?;
    let response = wire::read(&mut stream)
        .await?
        .ok_or_else(|| Error::unavailable("Guest disconnected."))?;
    if response["ok"] == true {
        return Ok(json!({"ok": true,"reused": true}));
    }
    if response["ready"] != true {
        return Err(Error::bad("Guest refused project import."));
    }
    transfer(stream, source, target, binary).await?;
    Ok(json!({"ok": true,"reused": false}))
}

async fn transfer(
    mut stream: BufReader<UnixStream>,
    source: &Path,
    target: &str,
    binary: bool,
) -> Result<()> {
    let mut tar = Command::new("tar");
    tar.args(["--exclude=leo-auth.sock", "--exclude=*.sock"]);
    if target == "/home/node" {
        tar.arg("--exclude=./.codex/auth.json");
    }
    tar.arg("-C");
    tar.arg(source).args(["-cf", "-", "."]);
    let mut child = tar
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()?;
    use tokio::io::AsyncReadExt;
    let mut archive = child.stdout.take().unwrap();
    let mut buffer = vec![0; wire::MAX_CHUNK];
    loop {
        let count = archive.read(&mut buffer).await?;
        if count == 0 {
            break;
        }
        if binary {
            wire::write_chunk(stream.get_mut(), &buffer[..count]).await?;
        } else {
            wire::write(
                stream.get_mut(),
                &json!({"type": "chunk","data": STANDARD.encode(&buffer[..count])}),
            )
            .await?;
        }
    }
    if binary {
        wire::write_chunk(stream.get_mut(), &[]).await?;
    } else {
        wire::write(stream.get_mut(), &json!({"type": "end"})).await?;
    }
    let code = child.wait().await?.code();
    if !matches!(code, Some(0 | 1)) {
        return Err(Error::bad("Workspace import failed."));
    }
    let result = wire::read(&mut stream)
        .await?
        .ok_or_else(|| Error::bad("Guest import disconnected."))?;
    if result["ok"] != true {
        return Err(Error::bad("Guest import failed."));
    }
    Ok(())
}

struct Network {
    tap: String,
    chain: String,
    guest: String,
    gateway: String,
    mac: String,
}

impl Network {
    fn new(slot: usize) -> Result<Self> {
        // Each live slot owns a distinct /30 in private 10.0.0.0/8. Check
        // address exhaustion instead of truncating slot IDs above one byte.
        let subnet = u32::try_from(slot)
            .ok()
            .filter(|slot| *slot > 0)
            .and_then(|slot| slot.checked_mul(4))
            .filter(|subnet| *subnet < (1 << 24))
            .ok_or_else(|| Error::unavailable("VM private IPv4 address space is exhausted."))?;
        let base = u32::from(std::net::Ipv4Addr::new(10, 0, 0, 0)) + subnet;
        let bytes = (slot as u32).to_be_bytes();
        Ok(Self {
            tap: format!("leo{slot}"),
            chain: format!("LEO{slot}"),
            guest: std::net::Ipv4Addr::from(base + 2).to_string(),
            gateway: std::net::Ipv4Addr::from(base + 1).to_string(),
            mac: format!(
                "06:00:{:02x}:{:02x}:{:02x}:{:02x}",
                bytes[0], bytes[1], bytes[2], bytes[3]
            ),
        })
    }

    async fn create(&self, uid: u32) -> Result<()> {
        command(
            "ip",
            &[
                "tuntap",
                "add",
                "dev",
                &self.tap,
                "mode",
                "tap",
                "user",
                &uid.to_string(),
            ],
        )
        .await?;
        command(
            "ip",
            &[
                "addr",
                "add",
                &format!("{}/30", self.gateway),
                "dev",
                &self.tap,
            ],
        )
        .await?;
        command("ip", &["link", "set", &self.tap, "up"]).await?;
        command("iptables", &["-w", "5", "-N", &self.chain]).await?;
        // No VM can contact the runner, a peer VM, LAN, or cloud metadata.
        command(
            "iptables",
            &["-w", "5", "-I", "INPUT", "-i", &self.tap, "-j", "DROP"],
        )
        .await?;
        command(
            "iptables",
            &[
                "-w",
                "5",
                "-I",
                "FORWARD",
                "-i",
                &self.tap,
                "-j",
                &self.chain,
            ],
        )
        .await?;
        command(
            "iptables",
            &[
                "-w",
                "5",
                "-A",
                &self.chain,
                "!",
                "-s",
                &self.guest,
                "-j",
                "DROP",
            ],
        )
        .await?;
        for subnet in [
            "0.0.0.0/8",
            "10.0.0.0/8",
            "100.64.0.0/10",
            "127.0.0.0/8",
            "169.254.0.0/16",
            "172.16.0.0/12",
            "192.168.0.0/16",
            "224.0.0.0/3",
        ] {
            command(
                "iptables",
                &["-w", "5", "-A", &self.chain, "-d", subnet, "-j", "DROP"],
            )
            .await?;
        }
        // Public TCP includes SSH on custom ports; UDP remains limited to DNS.
        // Keep both rules after the private-destination and source-address checks.
        command(
            "iptables",
            &["-w", "5", "-A", &self.chain, "-p", "tcp", "-j", "ACCEPT"],
        )
        .await?;
        command(
            "iptables",
            &[
                "-w",
                "5",
                "-A",
                &self.chain,
                "-p",
                "udp",
                "--dport",
                "53",
                "-j",
                "ACCEPT",
            ],
        )
        .await?;
        command("iptables", &["-w", "5", "-A", &self.chain, "-j", "DROP"]).await?;
        command(
            "iptables",
            &[
                "-w",
                "5",
                "-I",
                "FORWARD",
                "-o",
                &self.tap,
                "-m",
                "conntrack",
                "--ctstate",
                "ESTABLISHED,RELATED",
                "-j",
                "ACCEPT",
            ],
        )
        .await?;
        command(
            "iptables",
            &[
                "-w",
                "5",
                "-t",
                "nat",
                "-A",
                "POSTROUTING",
                "-s",
                &self.guest,
                "-j",
                "MASQUERADE",
            ],
        )
        .await
    }

    async fn remove(&self) {
        for args in [
            vec!["-w", "5", "-D", "INPUT", "-i", &self.tap, "-j", "DROP"],
            vec![
                "-w",
                "5",
                "-D",
                "FORWARD",
                "-i",
                &self.tap,
                "-j",
                &self.chain,
            ],
            vec![
                "-w",
                "5",
                "-D",
                "FORWARD",
                "-o",
                &self.tap,
                "-m",
                "conntrack",
                "--ctstate",
                "ESTABLISHED,RELATED",
                "-j",
                "ACCEPT",
            ],
            vec![
                "-w",
                "5",
                "-t",
                "nat",
                "-D",
                "POSTROUTING",
                "-s",
                &self.guest,
                "-j",
                "MASQUERADE",
            ],
            vec!["-w", "5", "-F", &self.chain],
            vec!["-w", "5", "-X", &self.chain],
        ] {
            let _ = tokio::time::timeout(
                Duration::from_secs(10),
                Command::new("iptables")
                    .args(args)
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .kill_on_drop(true)
                    .status(),
            )
            .await;
        }
        let _ = tokio::time::timeout(
            Duration::from_secs(10),
            Command::new("ip")
                .args(["link", "del", &self.tap])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .kill_on_drop(true)
                .status(),
        )
        .await;
    }
}

/// Owns every resource of a booted VM. A prepared disk can be adopted once only.
pub struct Vm {
    pub socket: PathBuf,
    jail: PathBuf,
    lock: Option<std::fs::File>,
    child: Option<tokio::process::Child>,
    consoles: Vec<tokio::task::JoinHandle<()>>,
    network: Network,
    mounted: Option<crate::storage::fuse::MountedDisk>,
    volume: Option<std::sync::Arc<crate::storage::runtime::Volume>>,
    uid: u32,
}

impl Vm {
    pub async fn boot(
        state: &Path,
        image: &Path,
        disk_dir: PathBuf,
        slot: usize,
        stop: &CancellationToken,
        resources: Option<&Value>,
    ) -> Result<Self> {
        let run_id = disk_dir.file_name().and_then(|v| v.to_str()).unwrap_or("");
        let mut timing = crate::performance::Operation::new("vm_boot", run_id, "prepare");
        let resources: crate::nodes::Resources = serde_json::from_value(
            resources
                .cloned()
                .unwrap_or_else(|| json!(crate::nodes::placement::defaults())),
        )
        .map_err(|_| Error::bad("Invalid VM resources."))?;
        resources.validate()?;
        let network = Network::new(slot)?;
        private_dir(&disk_dir).await?;
        let lock = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(disk_dir.join("lock"))?;
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err(Error::conflict(
                "The previous VM still owns this workspace.",
            ));
        }
        let id = crate::config::id();
        let runtime_file = disk_dir.join("runtime.json");
        let retained_image = if runtime_file.exists() {
            let runtime: Value = serde_json::from_slice(&tokio::fs::read(&runtime_file).await?)?;
            let name = text(&runtime, "runtimeId");
            if name.is_empty() || !name.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-') {
                return Err(Error::bad("Invalid retained runtime."));
            }
            state.join("images").join(name)
        } else {
            image.to_owned()
        };
        if !retained_image.join("root.ext4").exists() || !retained_image.join("vmlinux").exists() {
            return Err(Error::conflict(
                "The conversation requires an unavailable retained VM runtime.",
            ));
        }
        let image = retained_image.as_path();
        if !runtime_file.exists() {
            crate::skills::atomic_write(
                &runtime_file,
                &serde_json::to_vec(
                    &json!({"runtimeId": image.file_name().and_then(|v|v.to_str())}),
                )?,
            )
            .await?;
        }
        if disk_dir.join("restore.pending").exists() {
            return Err(Error::conflict("VM restore is incomplete."));
        }
        timing.next("open_journal");
        let volume = crate::storage::runtime::load(&disk_dir).await?;
        use crate::storage::Disk;
        if volume.disk.size() != resources.disk_mi_b * 1024 * 1024 {
            return Err(Error::conflict(
                "VM disk size does not match its S3-backed journal.",
            ));
        }
        let uid = 40000 + slot as u32;
        let jail = state.join("jails/firecracker").join(&id).join("root");
        let socket = jail.join("v.sock");

        let mut vm = Self {
            socket,
            jail,
            lock: Some(lock),
            child: None,
            consoles: Vec::new(),
            network,
            mounted: None,
            volume: Some(volume),
            uid,
        };
        let jail = &vm.jail;
        let socket = &vm.socket;
        let network = &vm.network;
        let child = &mut vm.child;
        let consoles = &mut vm.consoles;
        let mounted = &mut vm.mounted;
        let volume = &vm.volume;
        let operation = async {
            timing.next("mount_and_network");
            private_dir(jail).await?;
            let target = jail.join("disk");
            private_dir(&target).await?;
            *mounted = Some(crate::storage::fuse::mount_disk(
                volume.as_ref().unwrap().clone(),
                &target,
                uid,
            )?);
            tokio::fs::hard_link(image.join("root.ext4"), jail.join("root.ext4")).await?;
            tokio::fs::copy(image.join("vmlinux"), jail.join("vmlinux")).await?;
            network.create(uid).await?;
            timing.next("spawn_and_guest_ready");
            let config = json!({
                "boot-source": {
                    "kernel_image_path": "vmlinux",
                    "boot_args": format!("console=ttyS0 reboot=k panic=1 pci=off root=/dev/vda ro \
                        init=/sbin/leo-init ip={}::{}:255.255.255.252:leo:eth0:off",network.guest,network.gateway)
                },
                "drives": [{
                    "drive_id": "root",
                    "path_on_host": "root.ext4",
                    "is_root_device": true,
                    "is_read_only": true
                },{
                    "drive_id": "data",
                    "path_on_host": "disk/data.ext4",
                    "is_root_device": false,
                    "is_read_only": false,
                    "cache_type": "Writeback"
                }],
                "machine-config": {"vcpu_count": resources.cpu,"mem_size_mib": resources.memory_mi_b,"smt": false},
                "network-interfaces": [{"iface_id": "net","host_dev_name": network.tap,"guest_mac": network.mac}],
                "vsock": {"guest_cid": slot+3,"uds_path": "v.sock"}
            });
            atomic_write(&jail.join("config.json"), &serde_json::to_vec(&config)?).await?;
            std::os::unix::fs::chown(jail.join("config.json"), Some(uid), Some(uid))?;
            *child = Some(
                Command::new("/usr/local/bin/jailer")
                    .args([
                        "--id",
                        &id,
                        "--exec-file",
                        "/usr/local/bin/firecracker",
                        "--uid",
                        &uid.to_string(),
                        "--gid",
                        &uid.to_string(),
                        "--cgroup-version",
                        "2",
                        "--chroot-base-dir",
                        state.join("jails").to_str().unwrap(),
                        "--resource-limit",
                        "fsize=34359738368",
                        "--resource-limit",
                        "no-file=256",
                        "--",
                        "--api-sock",
                        "api.sock",
                        "--config-file",
                        "config.json",
                    ])
                    .stdin(Stdio::null())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .kill_on_drop(true)
                    .spawn()?,
            );
            consoles.push(console(
                child.as_mut().unwrap().stdout.take().unwrap(),
                state.join(format!("{id}.boot.log")),
            ));
            consoles.push(console(
                child.as_mut().unwrap().stderr.take().unwrap(),
                state.join(format!("{id}.vmm.log")),
            ));
            let mut deadline = tokio::time::Instant::now() + Duration::from_secs(60);
            let status = loop {
                if child.as_mut().unwrap().try_wait()?.is_some() {
                    return Err(Error::unavailable(
                        "Firecracker exited before the guest was ready. Check the VM boot log.",
                    ));
                }
                if let Ok(Ok(status)) = tokio::time::timeout(
                    Duration::from_secs(1),
                    guest_request(socket, &json!({"op": "status"})),
                )
                .await
                {
                    break status;
                }
                if volume.as_ref().is_some_and(|v| v.source.waiting()) {
                    deadline = tokio::time::Instant::now() + Duration::from_secs(60);
                }
                if tokio::time::Instant::now() > deadline {
                    return Err(Error::unavailable("Guest startup timed out."));
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            };
            if status["version"] != 1 {
                return Err(Error::unavailable("Unsupported guest protocol."));
            }

            Ok(())
        };
        let result = tokio::select! {
            r = operation => r,
            _ = stop.cancelled() => Err(Error::unavailable("VM preparation stopped."))
        };
        if let Err(error) = result {
            vm.shutdown().await;
            return Err(Error::new(
                error.status,
                format!("{} (VM {id})", error.message),
            ));
        }
        timing.finish();
        Ok(vm)
    }

    async fn synchronize_clock(&self) -> Result<()> {
        let status = tokio::time::timeout(
            Duration::from_secs(5),
            guest_request(
                &self.socket,
                &json!({"op": "clock","epochMs": crate::config::now()}),
            ),
        )
        .await
        .map_err(|_| Error::unavailable("Guest clock synchronization timed out."))??;
        if status["ok"] != true {
            return Err(Error::unavailable("Guest clock synchronization failed."));
        }
        Ok(())
    }

    pub async fn shutdown(&mut self) {
        let id = self
            .jail
            .parent()
            .and_then(Path::file_name)
            .and_then(|v| v.to_str())
            .unwrap_or("");
        let mut timing = crate::performance::Operation::new("vm_shutdown", id, "guest_shutdown");
        let blocked = self
            .volume
            .as_ref()
            .is_some_and(|volume| volume.source.waiting() || volume.paused());
        let mut acknowledged = false;
        if !blocked {
            // Healthy guests get a bounded graceful stop.
            if let Ok(Ok(reply)) = tokio::time::timeout(
                Duration::from_secs(10),
                guest_request(&self.socket, &json!({"op": "shutdown"})),
            )
            .await
            {
                acknowledged = reply["ok"] == true;
            }
        }
        // A VMM blocked in FUSE may not exit even after SIGKILL until its read
        // returns. Release remote reads and reserve waits before awaiting it.
        // All previously acknowledged disk writes remain in the durable journal.
        let volume = self.volume.take();
        if let Some(volume) = &volume {
            tracing::info!(target: "leo_performance", operation = "disk_io", id,
                metrics = %volume.disk.performance(), blocked, acknowledged);
        }
        timing.next("wait_vmm");
        let cancel_reads = || {
            if let Some(volume) = &volume {
                volume.stop.cancel();
            }
        };
        if let Some(mut child) = self.child.take() {
            if !acknowledged {
                cancel_reads();
                let _ = child.start_kill();
            }
            if tokio::time::timeout(Duration::from_secs(8), child.wait())
                .await
                .is_err()
            {
                cancel_reads();
                let _ = child.kill().await;
                let _ = child.wait().await;
            }
        }
        cancel_reads();
        drop(volume);
        for console in self.consoles.drain(..) {
            let _ = console.await;
        }
        timing.next("unmount");
        if let Some(mounted) = self.mounted.take() {
            let _ = tokio::task::spawn_blocking(move || mounted.close()).await;
        }
        timing.next("network_cleanup");
        self.network.remove().await;
        let _ = tokio::fs::remove_dir_all(self.jail.parent().unwrap()).await;
        self.lock.take();
        timing.finish();
    }

    pub async fn execute(
        &mut self,
        plan: &Value,
        state: &Path,
        stop: CancellationToken,
    ) -> Result<i32> {
        let mut timing = crate::performance::Operation::new(
            "guest_prepare",
            text(plan, "runId"),
            "clock_and_auth",
        );
        self.synchronize_clock().await?;
        let id = text(plan, "id");
        let vm_id = self
            .jail
            .parent()
            .and_then(Path::file_name)
            .and_then(|n| n.to_str())
            .unwrap_or("unknown");
        atomic_write(
            &state.join(format!("{id}.vm.json")),
            &serde_json::to_vec(&json!({"vmId": vm_id,"runId": plan["runId"]}))?,
        )
        .await?;
        let socket = &self.socket;
        let relay_path = self.jail.join("v.sock_5201");
        let auth_listener = UnixListener::bind(&relay_path)?;
        let uid = self.uid;
        std::os::unix::fs::chown(&relay_path, Some(uid), Some(uid))?;
        let auth_path = plan["imports"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|m| m["target"] == "/home/node")
            .map(|m| {
                Path::new(text(m, "source")).join(if plan["chat"]["provider"] == "claude" {
                    ".claude/leo-auth.sock"
                } else {
                    ".codex/leo-auth.sock"
                })
            });
        let relay_stop = CancellationToken::new();
        let relay_cancel = relay_stop.clone();
        let relay = tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = relay_cancel.cancelled() => break,
                    accepted = auth_listener.accept() => {
                        let Ok((mut guest,_))=accepted else {break};
                        if let Some(path)=&auth_path {
                            // Bound concurrency and lifetime; only this run's manager socket is reachable.
                            if let Ok(mut manager)=UnixStream::connect(path).await {
                                let _=tokio::time::timeout(Duration::from_secs(45),tokio::io::copy_bidirectional(&mut guest,&mut manager)).await;
                            }
                        }
                    }
                }
            }
        });

        let operation = async {
            timing.next("imports");
            let status = guest_request(socket, &json!({"op": "status"})).await?;
            if status["initialized"] != true {
                let mut imported = Vec::<(&Path, &str)>::new();
                for mount in plan["imports"].as_array().into_iter().flatten() {
                    let source = Path::new(text(mount, "source"));
                    let target = text(mount, "target");
                    // The workspace root already includes its projects. Separate
                    // entries still carry their read-only policy, but need no second archive.
                    if !imported.iter().any(|(parent, destination)| {
                        source.strip_prefix(parent).is_ok_and(|relative| {
                            Path::new(destination).join(relative) == Path::new(target)
                        })
                    }) {
                        import(socket, source, target).await?;
                        imported.push((source, target));
                    }
                }
            } else {
                // Credentials and the volatile inbox are refreshed, never the saved workspaces.
                for mount in plan["imports"].as_array().into_iter().flatten() {
                    if mount["target"] == "/run/leo-chat" {
                        import(socket, Path::new(text(mount, "source")), "/run/leo-chat").await?;
                    }
                    if mount["target"] == "/home/node" {
                        if plan["chat"]["provider"] == "claude" {
                            let source = Path::new(text(mount, "source")).join(".claude");
                            if source.exists() {
                                import(socket, &source, "/home/node/.claude").await?;
                            }
                        }
                        let source = Path::new(text(mount, "source")).join(".config/gh");
                        if source.exists() {
                            import(socket, &source, "/home/node/.config/gh").await?;
                        }
                    }
                }
            }
            let mut stream = connect(socket).await?;
            let mut guest_plan = plan.clone();
            guest_plan.as_object_mut().unwrap().remove("storage");
            wire::write(stream.get_mut(), &json!({"op": "run","plan": guest_plan})).await?;
            timing.finish();
            let mut timer = tokio::time::interval(Duration::from_millis(500));
            let inbox = plan["imports"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|m| m["target"] == "/run/leo-chat")
                .map(|m| PathBuf::from(text(m, "source")));
            let mut last_inbox = Vec::new();
            let mut logs = tokio::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(state.join(format!("{id}.log")))
                .await?;
            let mut total = 0usize;
            loop {
                // Keep the read future alive across inbox ticks: dropping it halfway
                // through a fragmented frame would discard already-consumed bytes.
                let next = wire::read(&mut stream);
                tokio::pin!(next);
                let event = loop {
                    tokio::select! {
                        event = &mut next => break event,
                        _ = timer.tick() => {
                            if let Some(inbox)=&inbox {
                                let content=tokio::fs::read(inbox.join("messages.json")).await.unwrap_or_default();
                                if content!=last_inbox {
                                    import(socket,inbox,"/run/leo-chat").await?;
                                    last_inbox=content;
                                }
                            }
                        }
                    }
                };
                let event = event?.ok_or_else(|| {
                    Error::unavailable("Guest disconnected. Its workspace disk has been preserved.")
                })?;
                match text(&event, "type") {
                    "output" => {
                        let bytes = STANDARD
                            .decode(text(&event, "data"))
                            .map_err(|_| Error::bad("Invalid guest output."))?;
                        total += bytes.len();
                        if total > 100_000_000 {
                            return Err(Error::bad("Guest output exceeded the run limit."));
                        }
                        wire::write(&mut logs, &event).await?;
                    }
                    "exit" => {
                        let code = event["code"]
                            .as_i64()
                            .filter(|n| (0..=255).contains(n))
                            .ok_or_else(|| Error::bad("Invalid guest exit status."))?;
                        let output = text(&plan["chat"], "output");
                        if !output.is_empty() {
                            atomic_write(Path::new(output), text(&event, "result").as_bytes())
                                .await?;
                            std::os::unix::fs::chown(output, Some(1000), Some(1000))?;
                        }

                        return Ok(code as i32);
                    }
                    _ => return Err(Error::bad("Unknown guest event.")),
                }
            }
        };
        let result =
            tokio::select! { result = operation => result, _ = stop.cancelled() => Ok(143) };
        relay_stop.cancel();
        relay.abort();
        let _ = relay.await;
        let _ = tokio::fs::remove_file(relay_path).await;
        result
    }
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
    tokio::time::timeout(Duration::from_secs(1), async {
        let mut stream = UnixStream::connect(socket).await?;
        let body = json!({"state": status}).to_string();
        stream
            .write_all(
                format!(
                    "PATCH /vm HTTP/1.1\r\nHost: localhost\r\nContent-Type: \
            application/json\r\nContent-Length: {}\r\n\r\n{body}",
                    body.len()
                )
                .as_bytes(),
            )
            .await?;
        let mut read = BufReader::new(stream);
        let mut line = String::new();
        read.read_line(&mut line).await?;
        if !line.starts_with("HTTP/1.1 204 ") {
            return Err(Error::unavailable("VM pause failed."));
        }
        Ok(())
    })
    .await
    .map_err(|_| Error::unavailable("VM pause timed out."))?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slot_networks_remain_distinct_above_four_and_one_byte() {
        let mut addresses = std::collections::HashSet::new();
        let mut macs = std::collections::HashSet::new();
        for slot in [1, 4, 5, 12, 255, 256, 257, 65536, (1 << 22) - 1] {
            let network = Network::new(slot).unwrap();
            assert!(addresses.insert(network.guest.clone()));
            assert!(addresses.insert(network.gateway.clone()));
            assert!(macs.insert(network.mac.clone()));
            assert!(network.tap.len() < 16);
            assert_eq!(network.mac.split(':').count(), 6);
            assert!(network.mac.split(':').all(|octet| octet.len() == 2));
            let guest: std::net::Ipv4Addr = network.guest.parse().unwrap();
            let gateway: std::net::Ipv4Addr = network.gateway.parse().unwrap();
            assert!(guest.is_private());
            assert_eq!(u32::from(guest) - u32::from(gateway), 1);
            assert_eq!(u32::from(guest) / 4, u32::from(gateway) / 4);
        }
        for slot in [0, 1 << 22, usize::MAX] {
            assert!(Network::new(slot).is_err());
        }
    }
}
