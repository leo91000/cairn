//! One booted Firecracker VM and the attempt it executes.
pub(super) mod snapshots;

use super::{
    host::{self, call, connect, import, valid_runtime_name},
    mcp,
    network::Network,
    plan::{CHAT_INBOX, ENTRYPOINT, HOME, Import, Plan},
    protocol::{Event, GuestRequest, GuestStatus, Reply},
    wire,
};
use crate::{
    error::{Error, Result},
    nodes::Resources,
    performance::Operation,
    provider::Provider,
    skills::{atomic_write, private_dir},
    storage::Disk,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    os::fd::AsRawFd,
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
    time::Duration,
};
use tokio::{
    io::BufReader,
    net::{UnixListener, UnixStream},
    process::Command,
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;

/// Exit code of an attempt stopped by the controller.
const STOPPED: i32 = 143;
const MAX_OUTPUT_BYTES: usize = 100_000_000;

#[derive(Clone, Copy)]
enum Startup<'a> {
    Cold,
    Template,
    Restore(&'a Path),
}

/// Balloon inflation touches guest pages even when their virtual address space
/// has never occupied host RAM. Bound idle work by the physical working set,
/// rather than walking tens of GiB of unused address space under CPU pressure.
fn idle_balloon_target(memory_mib: u64, actual: u64, available: u64, resident_bytes: u64) -> u64 {
    let physical_mib = resident_bytes.div_ceil(1_048_576);
    let reclaim_mib = available.saturating_sub(768).min(physical_mib).min(2048);
    actual
        .saturating_add(reclaim_mib)
        .min(memory_mib.saturating_sub(1024))
}

fn balloon_client(jail: &Path) -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .unix_socket(jail.join("api.sock"))
        .timeout(Duration::from_millis(500))
        .build()
        .map_err(Error::internal)
}

/// Inflates the idle balloon of the VM in `vm` and waits until its guest
/// acknowledges the returned target. Hold the returned ownership until the
/// CPUs pause: pressure rebalancing may otherwise lower the target first.
async fn reclaim_idle_memory(
    vm: &Path,
    memory_mib: u64,
    active_bytes: u64,
) -> Result<(crate::file_lock::Guard, u64)> {
    // Pressure rebalancing locks the same VM directory for at most one
    // bounded API exchange.
    let owner = crate::file_lock::exclusive_directory_after_release(vm).await?;

    let started = std::time::Instant::now();
    let vm_id = vm
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("unknown");
    let client = balloon_client(&vm.join("root"))?;

    let stats: Value = client
        .get("http://localhost/balloon/statistics")
        .send()
        .await
        .map_err(Error::internal)?
        .error_for_status()
        .map_err(Error::internal)?
        .json()
        .await
        .map_err(Error::internal)?;
    let actual = stats["actual_mib"].as_u64().unwrap_or(0);
    let available = stats["available_memory"].as_u64().unwrap_or(0) / 1_048_576;
    // Keep guest working-memory headroom, but reclaim only a bounded physical
    // working set. Free-page reporting returns unused backing independently
    // of the remaining virtual size; admission still measures actual bytes.
    let target = idle_balloon_target(memory_mib, actual, available, active_bytes);
    if target > actual {
        client
            .patch("http://localhost/balloon")
            .json(&json!({ "amount_mib": target }))
            .send()
            .await
            .map_err(Error::internal)?
            .error_for_status()
            .map_err(Error::internal)?;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
        loop {
            let stats: Value = client
                .get("http://localhost/balloon/statistics")
                .send()
                .await
                .map_err(Error::internal)?
                .error_for_status()
                .map_err(Error::internal)?
                .json()
                .await
                .map_err(Error::internal)?;
            if stats["actual_mib"].as_u64().unwrap_or(0) >= target.saturating_sub(16) {
                break;
            }
            if tokio::time::Instant::now() >= deadline {
                tracing::warn!(target: "cairn_performance", operation = "vm_retention", event = "balloon_unacknowledged", vm_id, requested_mib = target, actual_mib = stats["actual_mib"].as_u64(), active_bytes, elapsed_ms = started.elapsed().as_millis() as u64);
                return Err(Error::unavailable(
                    "Idle balloon reclamation was not acknowledged.",
                ));
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }
    Ok((owner, target))
}

/// `{attempt}.vm.json`: which VM executes an attempt, for pause and erasure.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AttemptRecord {
    #[serde(default)]
    pub vm_id: String,
    #[serde(default)]
    pub run_id: String,
}

/// `runtime.json` in a disk directory: the image a conversation's disk was created with.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RuntimeRecord {
    #[serde(default)]
    runtime_id: Option<String>,
}

/// Owns every resource of a booted VM. A prepared disk can be adopted once only.
pub struct Vm {
    pub socket: PathBuf,
    jail: PathBuf,
    lock: Option<std::fs::File>,
    child: Option<tokio::process::Child>,
    consoles: Vec<JoinHandle<()>>,
    network: Network,
    mounted: Option<crate::storage::transport::MountedDisk>,
    volume: Option<Arc<crate::storage::runtime::Volume>>,
    uid: u32,
    warmed: bool,
    idle: bool,
    memory_mib: u64,
}

impl Drop for Vm {
    fn drop(&mut self) {
        // Mounted transports also own the Volume. Cancelling before dropping
        // them releases remote reads even when boot/run was cancelled before
        // shutdown could await the VMM and close the backend normally.
        if let Some(volume) = &self.volume {
            volume.stop.cancel();
        }
        if let Some(child) = &mut self.child {
            let _ = child.start_kill();
        }
    }
}

fn console(
    mut stream: impl tokio::io::AsyncRead + Unpin + Send + 'static,
    path: PathBuf,
) -> JoinHandle<()> {
    use tokio::io::AsyncReadExt;
    tokio::spawn(async move {
        if let Ok(mut log) = tokio::fs::File::create(path).await {
            let _ = tokio::io::copy(&mut (&mut stream).take(4 * 1024 * 1024), &mut log).await;
        }
        // Drain excess guest console output without allowing it to fill host storage.
        let _ = tokio::io::copy(&mut stream, &mut tokio::io::sink()).await;
    })
}

fn resources(resources: Option<&Value>) -> Result<Resources> {
    let resources = match resources {
        Some(value) => {
            Resources::deserialize(value).map_err(|_| Error::bad("Invalid VM resources."))?
        }
        None => crate::nodes::placement::defaults(),
    };
    resources.validate()?;
    Ok(resources)
}

fn lock_disk(disk_dir: &Path) -> Result<std::fs::File> {
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
    Ok(lock)
}

/// A disk keeps booting the image it was created with; a new disk adopts `image`.
async fn retained_image(state: &Path, image: &Path, disk_dir: &Path) -> Result<PathBuf> {
    let runtime_file = disk_dir.join("runtime.json");
    let retained = if runtime_file.exists() {
        let runtime: RuntimeRecord =
            serde_json::from_slice(&tokio::fs::read(&runtime_file).await?)?;
        let name = runtime.runtime_id.unwrap_or_default();
        if name.is_empty() || !valid_runtime_name(&name) {
            return Err(Error::bad("Invalid retained runtime."));
        }
        state.join("images").join(name)
    } else {
        image.to_owned()
    };
    if !retained.join("root.ext4").exists() || !retained.join("vmlinux").exists() {
        return Err(Error::conflict(
            "The conversation requires an unavailable retained VM runtime.",
        ));
    }
    if !runtime_file.exists() {
        let record = RuntimeRecord {
            runtime_id: retained
                .file_name()
                .and_then(|name| name.to_str())
                .map(str::to_owned),
        };
        atomic_write(&runtime_file, &serde_json::to_vec(&record)?).await?;
    }
    Ok(retained)
}

fn firecracker_config(
    network: &Network,
    resources: &Resources,
    slot: usize,
    drives: &[Value],
) -> Value {
    // Virtio guests have no PS/2 devices. Keep warnings and errors on the
    // serial console without paying for informational output during boot.
    let boot_args = format!(
        concat!(
            "console=ttyS0 loglevel=5 i8042.nokbd i8042.noaux ",
            "reboot=k panic=1 pci=off root=/dev/vda ro ",
            "init=/sbin/cairn-init ip={}::{}:255.255.255.252:cairn:eth0:off"
        ),
        network.guest, network.gateway
    );
    let mut configured_drives = vec![json!({
        "drive_id": "root",
        "path_on_host": "root.ext4",
        "is_root_device": true,
        "is_read_only": true
    })];
    configured_drives.extend_from_slice(drives);
    json!({
        "boot-source": { "kernel_image_path": "vmlinux", "boot_args": boot_args },
        "drives": configured_drives,
        "machine-config": {
            "vcpu_count": resources.cpu,
            "mem_size_mib": resources.memory_mi_b,
            "smt": false
        },
        "balloon": {
            "amount_mib": 0,
            "deflate_on_oom": true,
            "stats_polling_interval_s": 1,
            "free_page_reporting": true
        },
        "network-interfaces": [
            { "iface_id": "net", "host_dev_name": network.tap, "guest_mac": network.mac }
        ],
        "vsock": { "guest_cid": slot + 3, "uds_path": "v.sock" }
    })
}

fn jailer(id: &str, uid: u32, state: &Path, disk_bytes: u64, configured: bool) -> Command {
    let uid = uid.to_string();
    // Firecracker writes a regular FUSE file at guest block offsets. A fixed
    // limit below the retained disk size terminates the VMM with SIGXFSZ.
    let file_limit = format!("fsize={disk_bytes}");
    let mut command = Command::new("/usr/local/bin/jailer");
    command
        .args([
            "--id",
            id,
            "--exec-file",
            "/usr/local/bin/firecracker",
            "--uid",
            &uid,
            "--gid",
            &uid,
            "--cgroup-version",
            "2",
            "--chroot-base-dir",
            state.join("jails").to_str().unwrap(),
            "--resource-limit",
            &file_limit,
            "--resource-limit",
            "no-file=256",
            "--",
            "--api-sock",
            "api.sock",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if configured {
        command.args(["--config-file", "config.json"]);
    }
    command
}

/// Whether an earlier import already copied `source` to `target` as part of its tree.
fn already_imported(imported: &[Import<'_>], source: &Path, target: &str) -> bool {
    imported.iter().any(|parent| {
        source
            .strip_prefix(parent.source)
            .is_ok_and(|relative| Path::new(parent.target).join(relative) == Path::new(target))
    })
}

/// Relays the guest's provider authentication socket to this run's manager socket.
fn auth_relay(
    listener: UnixListener,
    manager: Option<PathBuf>,
    stop: CancellationToken,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            let accepted = tokio::select! {
                () = stop.cancelled() => break,
                accepted = listener.accept() => accepted,
            };
            let Ok((mut guest, _)) = accepted else {
                break;
            };
            let Some(path) = &manager else {
                continue;
            };
            // Bound concurrency and lifetime; only this run's manager socket is reachable.
            if let Ok(mut manager) = UnixStream::connect(path).await {
                let relay = tokio::io::copy_bidirectional(&mut guest, &mut manager);
                let _ = tokio::time::timeout(Duration::from_secs(45), relay).await;
            }
        }
    })
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
        Self::boot_start(state, image, disk_dir, slot, stop, resources, Startup::Cold).await
    }

    async fn boot_start(
        state: &Path,
        image: &Path,
        disk_dir: PathBuf,
        slot: usize,
        stop: &CancellationToken,
        resources: Option<&Value>,
        startup: Startup<'_>,
    ) -> Result<Self> {
        // GC cannot unlink a runtime between selecting it, publishing the disk
        // pin and linking the immutable files into the new jail.
        let _images = super::images::IMAGE_LIFECYCLE.read().await;
        let run_id = disk_dir.file_name().and_then(|v| v.to_str()).unwrap_or("");
        let mut timing = Operation::new("vm_boot", run_id, "prepare");
        let resources = self::resources(resources)?;
        let network = Network::new(slot)?;
        private_dir(&disk_dir).await?;
        let lock = lock_disk(&disk_dir)?;
        let id = crate::config::id();
        let current_kernel = image.join("vmlinux");
        let image = retained_image(state, image, &disk_dir).await?;
        if disk_dir.join("restore.pending").exists() {
            return Err(Error::conflict("VM restore is incomplete."));
        }

        timing.next("open_journal");
        let volume = crate::storage::runtime::load(&disk_dir).await?;
        if volume.disk.size() != resources.disk_mi_b * 1024 * 1024 {
            return Err(Error::conflict(
                "VM disk size does not match its S3-backed journal.",
            ));
        }
        let jail = state.join("jails/firecracker").join(&id).join("root");
        let mut vm = Self {
            socket: jail.join("v.sock"),
            jail,
            lock: Some(lock),
            child: None,
            consoles: Vec::new(),
            network,
            mounted: None,
            volume: Some(volume),
            uid: 40000 + slot as u32,
            warmed: false,
            idle: false,
            memory_mib: resources.memory_mi_b,
        };
        let result = tokio::select! {
            result = vm.launch(state, (&image, &current_kernel, startup), &id, &resources, slot, &mut timing) => result,
            () = stop.cancelled() => Err(Error::unavailable("VM preparation stopped.")),
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

    async fn launch(
        &mut self,
        state: &Path,
        (image, kernel, startup): (&Path, &Path, Startup<'_>),
        id: &str,
        resources: &Resources,
        slot: usize,
        timing: &mut Operation,
    ) -> Result<()> {
        timing.next("mount_and_network");
        let jail = &self.jail;
        private_dir(jail).await?;
        self.mounted = Some(
            crate::storage::transport::mount(
                self.volume.as_ref().unwrap().clone(),
                state,
                jail,
                self.uid,
            )
            .await?,
        );
        tokio::fs::hard_link(image.join("root.ext4"), jail.join("root.ext4")).await?;
        tokio::fs::copy(kernel, jail.join("vmlinux")).await?;
        self.network.create(self.uid).await?;

        timing.next("spawn_and_guest_ready");
        let mounted = self.mounted.as_ref().unwrap();
        let paired = mounted.paired();
        if !matches!(startup, Startup::Cold) && !paired {
            return Err(Error::bad("Snapshots require paired native disks."));
        }
        let mut config = firecracker_config(&self.network, resources, slot, &mounted.drives());
        if matches!(startup, Startup::Template) {
            // The first diff is a standalone image of a freshly booted VM.
            // Avoid faulting/writing the entire sparse RAM ceiling on a node.
            config["machine-config"]["track_dirty_pages"] = true.into();
            // Never capture enabled L2 virtualization hardware: its VMX/SVM
            // state is not part of a Firecracker snapshot. The guest enables
            // it on the first nested VM creation after restore instead.
            let boot_args = config["boot-source"]["boot_args"].as_str().unwrap();
            config["boot-source"]["boot_args"] =
                format!("{boot_args} kvm.enable_virt_at_load=0").into();
        }
        if let Startup::Restore(template) = startup {
            snapshots::link(template, jail).await?;
        }
        atomic_write(&jail.join("config.json"), &serde_json::to_vec(&config)?).await?;
        std::os::unix::fs::chown(jail.join("config.json"), Some(self.uid), Some(self.uid))?;
        let disk_bytes = self.volume.as_ref().unwrap().disk.size();
        // A vhost frontend also creates a shared guest-memory file. Shared
        // node limits may allocate more RAM than the conversation's disk size.
        let file_bytes = disk_bytes.max(resources.memory_mi_b * 1024 * 1024);
        tracing::info!(target: "cairn_performance", operation = "vm_configuration", id,
            cpu = resources.cpu, memory_mib = resources.memory_mi_b, disk_bytes,
            file_limit_bytes = file_bytes, vhost = mounted.vhost());
        let child = self.child.insert(
            jailer(
                id,
                self.uid,
                state,
                file_bytes,
                !matches!(startup, Startup::Restore(_)),
            )
            .spawn()?,
        );
        self.consoles.push(console(
            child.stdout.take().unwrap(),
            state.join(format!("{id}.boot.log")),
        ));
        self.consoles.push(console(
            child.stderr.take().unwrap(),
            state.join(format!("{id}.vmm.log")),
        ));
        if matches!(startup, Startup::Restore(_)) {
            // The restored anonymous guest inherits a frozen OS. A cancelled
            // renewal must kill it rather than wait for guest-side shutdown.
            self.idle = true;
            timing.next("snapshot_load_and_drive_patch");
            self.load_snapshot().await?;
        }
        let status = self.wait_for_guest().await?;
        if status.version != 1 {
            // An incompatible image never succeeds on retry.
            return Err(Error::bad_gateway("Unsupported guest protocol."));
        }
        if matches!(startup, Startup::Restore(_)) {
            if !status.snapshot_clones || status.initialized || !status.codex_ready {
                return Err(Error::unavailable(
                    "Snapshot is not an anonymous ready guest.",
                ));
            }
            timing.next("clone_identity_and_native_restart");
            self.renew_clone().await?;
            self.warmed = true;
            self.idle = false;
        }
        if paired && !matches!(startup, Startup::Template) {
            timing.next("mount_workspace");
            if !status.workspace_disks {
                return Err(Error::bad_gateway(
                    "Guest image does not support paired disks.",
                ));
            }
            let reply =
                host::guest_request(&self.socket, &json!({ "op": "mount-workspace" })).await?;
            if reply["ok"] != true {
                return Err(Error::unavailable("Guest workspace mount failed."));
            }
        }
        Ok(())
    }

    async fn wait_for_guest(&mut self) -> Result<GuestStatus> {
        let mut deadline = tokio::time::Instant::now() + Duration::from_secs(60);
        loop {
            if self
                .mounted
                .as_ref()
                .is_some_and(crate::storage::transport::MountedDisk::failed)
            {
                return Err(Error::unavailable("VM block backend stopped."));
            }
            if self.child.as_mut().unwrap().try_wait()?.is_some() {
                return Err(Error::unavailable(
                    "Firecracker exited before the guest was ready. Check the VM boot log.",
                ));
            }
            let probe = host::status(&self.socket);
            if let Ok(Ok(status)) = tokio::time::timeout(Duration::from_secs(1), probe).await {
                return Ok(status);
            }
            // A remote disk read is still waiting; guest boot has not stalled.
            if self.volume.as_ref().is_some_and(|v| v.source.waiting()) {
                deadline = tokio::time::Instant::now() + Duration::from_secs(60);
            }
            if tokio::time::Instant::now() > deadline {
                return Err(Error::unavailable("Guest startup timed out."));
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    async fn synchronize_clock(&self) -> Result<()> {
        let request = GuestRequest::Clock {
            epoch_ms: crate::config::now(),
        };
        let reply: Reply =
            tokio::time::timeout(Duration::from_secs(5), call(&self.socket, &request))
                .await
                .map_err(|_| Error::unavailable("Guest clock synchronization timed out."))??;
        if !reply.succeeded() {
            return Err(Error::unavailable("Guest clock synchronization failed."));
        }
        Ok(())
    }

    pub async fn shutdown(&mut self) {
        let id = self.id().to_owned();
        let mut timing = Operation::new("vm_shutdown", &id, "guest_shutdown");
        let blocked = self
            .volume
            .as_ref()
            .is_some_and(|volume| volume.source.waiting() || volume.paused());
        let mut acknowledged = false;
        if !blocked && !self.idle {
            // Healthy guests get a bounded graceful stop.
            let request = call::<Reply>(&self.socket, &GuestRequest::Shutdown);
            if let Ok(Ok(reply)) = tokio::time::timeout(Duration::from_secs(10), request).await {
                acknowledged = reply.succeeded();
            }
        }
        // A VMM blocked in FUSE may not exit even after SIGKILL until its read
        // returns. Release remote reads and reserve waits before awaiting it.
        // All previously acknowledged disk writes remain in the durable journal.
        let volume = self.volume.take();
        if let Some(volume) = &volume {
            tracing::info!(target: "cairn_performance", operation = "disk_io", id = id.as_str(),
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
        if let Some(mounted) = self.mounted.take()
            && let Ok(Err(error)) = tokio::task::spawn_blocking(move || mounted.close()).await
        {
            tracing::warn!(%error, "Could not close VM block backend");
        }

        timing.next("network_cleanup");
        self.network.remove().await;
        if let Err(error) = tokio::fs::remove_dir_all(self.jail.parent().unwrap()).await
            && error.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(%error, "Could not remove VM jail");
        }
        self.lock.take();
        timing.finish();
    }

    pub(super) fn id(&self) -> &str {
        self.jail
            .parent()
            .and_then(Path::file_name)
            .and_then(|name| name.to_str())
            .unwrap_or("unknown")
    }

    pub(super) fn accepts_retained(&self, plan: &Plan) -> bool {
        // A resident receives per-thread configuration, but cannot apply new
        // process-level CLI options. Cold adapters start afresh on every turn.
        !self.warmed
            || plan.as_value()["chat"]["args"]
                .as_array()
                .is_none_or(Vec::is_empty)
    }

    /// Only a confirmed process exit permits retiring an unpublished idle VM.
    /// Uncertain process state keeps its physical ownership and journal pinned.
    pub(super) fn exited(&mut self) -> bool {
        self.child
            .as_mut()
            .is_some_and(|child| matches!(child.try_wait(), Ok(Some(_))))
    }

    /// No CPU or guest background process may run without an attempt lease.
    pub(super) async fn suspend_idle(&mut self) -> Result<()> {
        let active_bytes = self
            .resident_bytes()
            .ok_or_else(|| Error::unavailable("VM resident memory is unavailable."))?;
        let started = std::time::Instant::now();
        // Ownership of the balloon is held until the CPUs pause.
        let (_owner, target) =
            reclaim_idle_memory(self.jail.parent().unwrap(), self.memory_mib, active_bytes).await?;

        self.idle = true;
        host::set_vm_state(&self.jail.join("api.sock"), "Paused").await?;
        let retained_bytes = self
            .resident_bytes()
            .ok_or_else(|| Error::unavailable("Retained VM memory is unavailable."))?;
        tracing::info!(target: "cairn_performance", operation = "vm_retention", event = "memory_reclaimed", vm_id = self.id(), active_bytes, retained_bytes, reclaimed_bytes = active_bytes.saturating_sub(retained_bytes), balloon_mib = target, elapsed_ms = started.elapsed().as_millis() as u64);
        Ok(())
    }

    pub(super) async fn resume_idle(&mut self) -> Result<()> {
        // Restore usable guest address space on demand. This does not reserve
        // the whole virtual size in RAM; the shared cgroup remains the ceiling.
        balloon_client(&self.jail)?
            .patch("http://localhost/balloon")
            .json(&json!({ "amount_mib": 0 }))
            .send()
            .await
            .map_err(Error::internal)?
            .error_for_status()
            .map_err(Error::internal)?;
        host::set_vm_state(&self.jail.join("api.sock"), "Resumed").await?;
        self.idle = false;
        Ok(())
    }

    /// Allocated shared guest pages plus the VMM's other resident pages. Plain
    /// RSS alone can miss shared memfd pages held by KVM or the block backend.
    pub(super) fn resident_bytes(&self) -> Option<u64> {
        let pid = self.child.as_ref()?.id()?;
        let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
        let field = |key| {
            status.lines().find_map(|line| {
                line.strip_prefix(key)?
                    .split_whitespace()
                    .next()?
                    .parse::<u64>()
                    .ok()?
                    .checked_mul(1024)
            })
        };
        let rss = field("VmRSS:")?;
        let mapped_shared = field("RssShmem:").unwrap_or(0);
        let allocated = self.mounted.as_ref()?.allocated_memory_bytes().ok()?;
        Some(rss.saturating_add(allocated.saturating_sub(mapped_shared)))
    }

    pub async fn execute(
        &mut self,
        plan: &Plan,
        state: &Path,
        stop: CancellationToken,
    ) -> Result<i32> {
        let mut timing = Operation::new("guest_prepare", plan.run_id(), "clock_and_auth");
        self.synchronize_clock().await?;
        let record = AttemptRecord {
            vm_id: self.id().to_owned(),
            run_id: plan.run_id().to_owned(),
        };
        atomic_write(
            &state.join(format!("{}.vm.json", plan.id())),
            &serde_json::to_vec(&record)?,
        )
        .await?;
        let relay_path = self.jail.join("v.sock_5201");
        let listener = UnixListener::bind(&relay_path)?;
        std::os::unix::fs::chown(&relay_path, Some(self.uid), Some(self.uid))?;
        let mcp_path = self.jail.join(format!("v.sock_{}", mcp::PORT));
        let mcp_listener = UnixListener::bind(&mcp_path)?;
        std::os::unix::fs::chown(&mcp_path, Some(self.uid), Some(self.uid))?;

        let auth_socket = match plan.chat_provider() {
            Provider::Claude => ".claude/cairn-auth.sock",
            Provider::Codex => ".codex/cairn-auth.sock",
        };
        let home = plan.import_to(HOME);
        let manager = home.map(|home| home.source.join(auth_socket));
        let channel = home.map(|home| home.source.join(mcp::SOCKET));
        let relay_stop = CancellationToken::new();
        let relay = auth_relay(listener, manager, relay_stop.clone());
        // Only this run's MCP channel is reachable: the guest firewall is unchanged.
        let mcp_relay = mcp::relay(mcp_listener, channel, relay_stop.clone());
        let socket = &self.socket;
        let warmed = self.warmed;
        let operation = async {
            timing.next("imports");
            let status = host::status(socket).await?;
            if warmed && !status.codex_ready {
                return Err(Error::unavailable(
                    "Prepared Codex service is no longer ready.",
                ));
            }
            if status.initialized {
                refresh_imports(socket, plan).await?;
            } else {
                import_workspace(socket, plan).await?;
            }
            if plan.command().is_none() && !status.codex_ready {
                timing.next("current_entrypoint");
                let target = Path::new(ENTRYPOINT).parent().unwrap().to_str().unwrap();
                host::import(socket, &state.join("entrypoint"), target).await?;
            }
            run(socket, plan, state, timing).await
        };
        let result = tokio::select! {
            result = operation => result,
            () = stop.cancelled() => Ok(STOPPED),
            result = watch_backend(self.mounted.as_ref()) => result,
        };
        relay_stop.cancel();
        relay.abort();
        let _ = relay.await;
        let _ = mcp_relay.await;
        let _ = tokio::fs::remove_file(relay_path).await;
        let _ = tokio::fs::remove_file(mcp_path).await;
        result
    }

    /// Warm only an anonymous local disk. A live native process must never be
    /// initialized over a previous conversation or a borrowed storage grant.
    /// Older guest images return false and remain eligible for the cold path.
    pub async fn warm_codex(&mut self, state: &Path, stop: &CancellationToken) -> Result<bool> {
        if self
            .volume
            .as_ref()
            .unwrap()
            .source
            .authorization()?
            .is_some()
        {
            return Err(Error::conflict("Only an anonymous VM can prewarm Codex."));
        }
        let status = host::status(&self.socket).await?;
        if !status.codex_service {
            return Ok(false);
        }
        if status.initialized {
            return Err(Error::conflict("VM already contains a conversation."));
        }
        let mut timing = Operation::new("vm_codex_warm", self.id(), "clock");
        self.synchronize_clock().await?;
        timing.next("current_entrypoint");
        let target = Path::new(ENTRYPOINT).parent().unwrap().to_str().unwrap();
        host::import(&self.socket, &state.join("entrypoint"), target).await?;
        // Any uncertain outcome retires this VM; execute cannot silently start
        // a second native process over databases opened by the first one.
        self.warmed = true;
        timing.next("native_initialize");
        let result = tokio::select! {
            () = stop.cancelled() => Err(Error::unavailable("VM Codex warmup cancelled.")),
            result = tokio::time::timeout(Duration::from_secs(65), call::<Reply>(&self.socket, &GuestRequest::WarmCodex)) => {
                result.map_err(|_| Error::gateway_timeout("VM Codex warmup timed out."))?
            },
        }?;
        if !result.succeeded() || !host::status(&self.socket).await?.codex_ready {
            return Err(Error::unavailable("VM Codex warmup was not acknowledged."));
        }
        timing.finish();
        Ok(true)
    }

    /// A speculative guest can be discarded on an uncertain probe, before any
    /// account, conversation or remote grant has been assigned to it.
    pub async fn codex_ready(&self) -> bool {
        matches!(tokio::time::timeout(Duration::from_secs(1), host::status(&self.socket)).await,
            Ok(Ok(status)) if status.codex_ready && !status.initialized)
    }
}

async fn watch_backend(backend: Option<&crate::storage::transport::MountedDisk>) -> Result<i32> {
    let Some(backend) = backend else {
        return std::future::pending().await;
    };
    while !backend.failed() {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    Err(Error::unavailable(
        "VM block backend stopped. Its workspace disk has been preserved.",
    ))
}

/// First boot of a disk: copy every import into the guest.
async fn import_workspace(socket: &Path, plan: &Plan) -> Result<()> {
    let mut imported = Vec::new();
    for import in plan.imports() {
        // The workspace root already includes its projects. Separate
        // entries still carry their read-only policy, but need no second archive.
        if already_imported(&imported, import.source, import.target) {
            continue;
        }
        host::import(socket, import.source, import.target).await?;
        imported.push(import);
    }
    Ok(())
}

/// Credentials and the volatile inbox are refreshed, never the saved workspaces.
async fn refresh_imports(socket: &Path, plan: &Plan) -> Result<()> {
    for import in plan.imports() {
        if import.target == CHAT_INBOX {
            host::import(socket, import.source, CHAT_INBOX).await?;
        }
        if import.target != HOME {
            continue;
        }
        if plan.chat_provider() == Provider::Claude {
            let source = import.source.join(".claude");
            if source.exists() {
                host::import(socket, &source, "/home/node/.claude").await?;
            }
        }
        let source = import.source.join(".config/gh");
        if source.exists() {
            host::import(socket, &source, "/home/node/.config/gh").await?;
        }
    }
    Ok(())
}

/// Starts the agent and records its output until the guest reports its exit.
async fn run(socket: &Path, plan: &Plan, state: &Path, mut timing: Operation) -> Result<i32> {
    timing.next("run_connect");
    let mut stream = connect(socket).await?;
    let request = GuestRequest::Run {
        plan: plan.for_guest(),
    };
    timing.next("run_request");
    wire::write(stream.get_mut(), &request).await?;
    timing.finish();

    let mut inbox = Inbox::new(plan);
    let mut logs = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(state.join(format!("{}.log", plan.id())))
        .await?;
    let mut total = 0usize;
    loop {
        let event = next_event(&mut stream, socket, &mut inbox).await?;
        match event {
            Event::Output { ref data, .. } => {
                let bytes = STANDARD
                    .decode(data)
                    .map_err(|_| Error::bad("Invalid guest output."))?;
                total += bytes.len();
                if total > MAX_OUTPUT_BYTES {
                    return Err(Error::bad("Guest output exceeded the run limit."));
                }
                wire::write(&mut logs, &event).await?;
            }
            Event::Exit { code, result } => {
                let code = code
                    .filter(|n| (0..=255).contains(n))
                    .ok_or_else(|| Error::bad("Invalid guest exit status."))?;
                if let Some(output) = plan.chat_output().filter(|output| !output.is_empty()) {
                    atomic_write(Path::new(output), result.as_bytes()).await?;
                    std::os::unix::fs::chown(output, Some(1000), Some(1000))?;
                }
                return Ok(code as i32);
            }
            Event::Heartbeat | Event::Unknown => return Err(Error::bad("Unknown guest event.")),
        }
    }
}

/// Reads the next run event while forwarding chat inbox changes to the guest.
async fn next_event(
    stream: &mut BufReader<UnixStream>,
    socket: &Path,
    inbox: &mut Inbox,
) -> Result<Event> {
    // Keep the read future alive across inbox ticks: dropping it halfway
    // through a fragmented frame would discard already-consumed bytes.
    let next = wire::read(stream);
    tokio::pin!(next);
    let event = loop {
        tokio::select! {
            event = &mut next => break event,
            _ = inbox.timer.tick() => inbox.forward(socket).await?,
        }
    };
    let event = event?.ok_or_else(|| {
        Error::unavailable("Guest disconnected. Its workspace disk has been preserved.")
    })?;
    wire::decode(event, "Unknown guest event.")
}

struct Inbox {
    timer: tokio::time::Interval,
    source: Option<PathBuf>,
    last: Vec<u8>,
}

impl Inbox {
    fn new(plan: &Plan) -> Self {
        Self {
            timer: tokio::time::interval(Duration::from_millis(500)),
            source: plan
                .import_to(CHAT_INBOX)
                .map(|import| import.source.to_owned()),
            last: Vec::new(),
        }
    }

    async fn forward(&mut self, socket: &Path) -> Result<()> {
        let Some(source) = &self.source else {
            return Ok(());
        };
        let content = tokio::fs::read(source.join("messages.json"))
            .await
            .unwrap_or_default();
        if content != self.last {
            import(socket, source, CHAT_INBOX).await?;
            self.last = content;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::microvm::budget;
    use std::os::unix::process::CommandExt;

    #[test]
    fn idle_balloon_does_not_walk_unallocated_large_guest_memory() {
        let physical_bytes = 1322995712;
        assert_eq!(idle_balloon_target(35840, 0, 35000, physical_bytes), 1262);
        assert_eq!(idle_balloon_target(9728, 0, 9000, physical_bytes), 1262);
        assert_eq!(
            idle_balloon_target(35840, 0, 35000, 32 * 1_048_576 * 1024),
            2048
        );
    }

    #[test]
    fn idle_balloon_preserves_guest_headroom_and_existing_inflation() {
        let physical_bytes = 2 * 1024 * 1_048_576;
        assert_eq!(idle_balloon_target(4096, 256, 768, physical_bytes), 256);
        assert_eq!(idle_balloon_target(4096, 256, 900, physical_bytes), 388);
        assert_eq!(idle_balloon_target(2048, 256, 2000, physical_bytes), 1024);
        assert_eq!(idle_balloon_target(4096, 256, 3500, 0), 256);
        assert_eq!(idle_balloon_target(1024, 0, 1000, physical_bytes), 0);
    }

    /// Firecracker's balloon API with a guest that applies each target at once.
    #[derive(Default)]
    struct Balloon {
        target_mib: u64,
        actual_mib: u64,
        monitored: bool,
    }

    #[tokio::test]
    async fn pressure_rebalancing_never_replaces_an_unacknowledged_idle_target() {
        use axum::{
            Json, Router,
            extract::State,
            http::StatusCode,
            routing::{get, patch},
        };
        use std::sync::Mutex;

        let state = tempfile::tempdir().unwrap();
        let vm = state
            .path()
            .join("jails/firecracker")
            .join(uuid::Uuid::new_v4().to_string());
        std::fs::create_dir_all(vm.join("root")).unwrap();
        let listener = UnixListener::bind(vm.join("root/api.sock")).unwrap();
        let balloon = Arc::new(Mutex::new(Balloon::default()));
        let monitor_state = state.path().to_owned();

        let statistics = |State(balloon): State<Arc<Mutex<Balloon>>>| async move {
            let balloon = balloon.lock().unwrap();
            Json(json!({
                "target_mib": balloon.target_mib,
                "actual_mib": balloon.actual_mib,
                "available_memory": 3u64 << 30,
            }))
        };
        let resize = move |State(balloon): State<Arc<Mutex<Balloon>>>, Json(body): Json<Value>| async move {
            let first = {
                let mut balloon = balloon.lock().unwrap();
                balloon.target_mib = body["amount_mib"].as_u64().unwrap();
                balloon.actual_mib = balloon.target_mib;
                !std::mem::replace(&mut balloon.monitored, true)
            };

            // A monitor tick lands after the guest reaches the idle target,
            // before retention reads that acknowledgement back.
            if first {
                budget::rebalance(&monitor_state, 0, 4096).await.unwrap();
            }
            StatusCode::NO_CONTENT
        };
        let app = Router::new()
            .route("/", get(|| async { Json(json!({ "state": "Running" })) }))
            .route("/balloon/statistics", get(statistics))
            .route("/balloon", patch(resize))
            .with_state(balloon.clone());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let reclaimed = reclaim_idle_memory(&vm, 3584, 600 * 1_048_576).await;

        assert_eq!(reclaimed.unwrap().1, 600);
        assert_eq!(balloon.lock().unwrap().target_mib, 600);
        server.abort();
    }

    #[test]
    fn jailer_allows_writes_to_the_end_of_a_grown_disk() {
        let directory = tempfile::tempdir().unwrap();
        let disk_bytes = 64_u64 * 1024 * 1024 * 1024;
        let disk = directory.path().join("data.ext4");
        std::fs::File::create(&disk)
            .unwrap()
            .set_len(disk_bytes)
            .unwrap();
        let command = jailer("test", 1000, directory.path(), disk_bytes, true);
        let limit = command
            .as_std()
            .get_args()
            .find_map(|arg| arg.to_str()?.strip_prefix("fsize="))
            .unwrap()
            .parse::<libc::rlim_t>()
            .unwrap();
        let mut write = std::process::Command::new("dd");
        write
            .args([
                "if=/dev/zero",
                "bs=1",
                "count=1",
                "conv=notrunc",
                "status=none",
            ])
            .arg(format!("of={}", disk.display()))
            .arg(format!("seek={}", disk_bytes - 1));
        // Apply the actual jailer limit only in the child performing the write.
        unsafe {
            write.pre_exec(move || {
                let limits = libc::rlimit {
                    rlim_cur: limit,
                    rlim_max: limit,
                };
                if libc::setrlimit(libc::RLIMIT_FSIZE, &limits) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let result = write.output().unwrap();
        assert!(
            result.status.success(),
            "disk write failed: {:?}",
            result.status
        );
    }
}
