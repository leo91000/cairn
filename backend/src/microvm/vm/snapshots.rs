//! Node-local, immutable anonymous templates. Conversation VMs are never copied.
use super::*;
use crate::storage::{bootstrap, layout::Layout, policy::Policy};
use reqwest::Method;
use std::{io, os::unix::fs::PermissionsExt};

pub(super) async fn link(template: &Path, jail: &Path) -> Result<()> {
    for name in ["snapshot.state", "snapshot.mem"] {
        let source = template.join(name);
        let metadata = tokio::fs::symlink_metadata(&source).await?;
        if !metadata.is_file() || metadata.permissions().mode() & 0o222 != 0 {
            return Err(Error::conflict("Snapshot template is not immutable."));
        }
        tokio::fs::hard_link(source, jail.join(name)).await?;
    }
    Ok(())
}

async fn api(
    jail: &Path,
    method: Method,
    path: &str,
    body: &Value,
    timeout: Duration,
) -> Result<()> {
    let client = reqwest::Client::builder()
        .no_proxy()
        .unix_socket(jail.join("api.sock"))
        .timeout(timeout)
        .build()
        .map_err(Error::internal)?;
    let response = client
        .request(method, format!("http://localhost{path}"))
        .json(body)
        .send()
        .await
        .map_err(|_| Error::unavailable("Firecracker snapshot request failed."))?;
    if !response.status().is_success() {
        return Err(Error::unavailable(format!(
            "Firecracker snapshot operation {path} failed: {}",
            response.status()
        )));
    }
    Ok(())
}

fn entropy() -> Result<[u8; 32]> {
    let mut bytes = [0; 32];
    let mut position = 0;
    while position < bytes.len() {
        let read = unsafe {
            libc::getrandom(
                bytes[position..].as_mut_ptr().cast(),
                bytes.len() - position,
                0,
            )
        };
        if read < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error.into());
        }
        if read == 0 {
            return Err(Error::unavailable("Host entropy is unavailable."));
        }
        position += read as usize;
    }
    Ok(bytes)
}

impl Vm {
    pub(super) async fn load_snapshot(&mut self) -> Result<()> {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while !self.jail.join("api.sock").exists() {
            if self.child.as_mut().unwrap().try_wait()?.is_some()
                || tokio::time::Instant::now() >= deadline
            {
                return Err(Error::unavailable("Snapshot VMM did not start."));
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        api(
            &self.jail,
            Method::PUT,
            "/snapshot/load",
            &json!({
                "snapshot_path": "snapshot.state",
                "mem_backend": { "backend_type": "File", "backend_path": "snapshot.mem" },
                "track_dirty_pages": false,
                "resume_vm": false,
                "network_overrides": [{ "iface_id": "net", "host_dev_name": self.network.tap }],
                "vsock_override": { "uds_path": "v.sock" },
                "clock_realtime": true
            }),
            Duration::from_secs(10),
        )
        .await?;
        for (id, path) in [("data", "disk.blk"), ("conversation", "workspace.blk")] {
            api(
                &self.jail,
                Method::PATCH,
                &format!("/drives/{id}"),
                &json!({
                    "drive_id": id,
                    "path_on_host": path
                }),
                Duration::from_secs(5),
            )
            .await?;
        }
        host::set_vm_state(&self.jail.join("api.sock"), "Resumed").await
    }

    pub(super) async fn renew_clone(&self) -> Result<()> {
        self.synchronize_clock().await?;
        let request = GuestRequest::RestoreClone {
            identity: super::super::protocol::CloneIdentity {
                id: self.id().to_owned(),
                guest: self.network.guest.clone(),
                gateway: self.network.gateway.clone(),
                mac: self.network.mac.clone(),
                entropy: STANDARD.encode(entropy()?),
            },
        };
        let reply: Reply =
            tokio::time::timeout(Duration::from_secs(70), call(&self.socket, &request))
                .await
                .map_err(|_| Error::unavailable("Snapshot clone renewal timed out."))??;
        if !reply.succeeded() || !host::status(&self.socket).await?.codex_ready {
            return Err(Error::unavailable(
                "Snapshot clone renewal was not acknowledged.",
            ));
        }
        Ok(())
    }

    /// Only background preparation calls this, before any assignment or account.
    pub(in crate::microvm) async fn capture_template(
        &mut self,
        destination: &Path,
        policy: &Policy,
        stop: &CancellationToken,
    ) -> Result<()> {
        let status = host::status(&self.socket).await?;
        let volume = self.volume.as_ref().unwrap().clone();
        if !self.warmed
            || status.initialized
            || !status.codex_ready
            || !status.snapshot_clones
            || !self.mounted.as_ref().unwrap().paired()
            || volume.source.authorization()?.is_some()
        {
            return Err(Error::conflict(
                "Only an anonymous paired ready VM can become a template.",
            ));
        }
        // Even a lost freeze reply can leave the anonymous guest frozen. Its
        // owner always tears it down by killing and reaping the VMM.
        self.idle = true;
        let reply: Reply = tokio::select! {
            biased;
            () = stop.cancelled() => return Err(Error::unavailable("Template preparation cancelled.")),
            result = tokio::time::timeout(Duration::from_secs(10), call(&self.socket, &GuestRequest::Freeze)) => {
                result.map_err(|_| Error::unavailable("Template freeze timed out."))??
            }
        };
        if !reply.succeeded() {
            return Err(Error::unavailable("Template freeze failed."));
        }
        host::set_vm_state(&self.jail.join("api.sock"), "Paused").await?;
        let generation = volume.seal().await?;
        let request = json!({
            "snapshot_type": "Diff",
            "snapshot_path": "snapshot.state",
            "mem_file_path": "snapshot.mem",
            "sync_snapshot_files": true
        });
        let capture = api(
            &self.jail,
            Method::PUT,
            "/snapshot/create",
            &request,
            Duration::from_secs(120),
        );
        tokio::select! {
            biased;
            () = stop.cancelled() => return Err(Error::unavailable("Template preparation cancelled.")),
            result = capture => result?,
        }
        private_dir(destination).await?;
        for name in ["snapshot.state", "snapshot.mem"] {
            let path = self.jail.join(name);
            std::os::unix::fs::chown(&path, Some(0), Some(0))?;
            tokio::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o444)).await?;
            tokio::fs::hard_link(&path, destination.join(name)).await?;
        }
        bootstrap::prepare_template(&destination.join("disk"), volume, generation, policy, stop)
            .await?;
        Ok(())
    }

    pub(in crate::microvm) async fn boot_template(
        state: &Path,
        image: &Path,
        disk: PathBuf,
        slot: usize,
        stop: &CancellationToken,
        resources: &Value,
    ) -> Result<Self> {
        Self::boot_start(
            state,
            image,
            disk,
            slot,
            stop,
            Some(resources),
            Startup::Template,
        )
        .await
    }

    pub(in crate::microvm) async fn restore_template(
        state: &Path,
        image: &Path,
        disk: PathBuf,
        slot: usize,
        stop: &CancellationToken,
        resources: &Value,
        template: &Path,
    ) -> Result<Self> {
        Self::boot_start(
            state,
            image,
            disk,
            slot,
            stop,
            Some(resources),
            Startup::Restore(template),
        )
        .await
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Key {
    version: u32,
    runtime: String,
    kernel: String,
    entrypoint: String,
    firecracker: String,
    host_cpu: String,
    cpu: u32,
    memory_mib: u64,
    disk_mib: u64,
    layout: Layout,
    policy: Value,
}

impl Key {
    fn name(&self) -> Result<String> {
        Ok(blake3::hash(&serde_json::to_vec(self)?)
            .to_hex()
            .to_string())
    }
}

pub(in crate::microvm) struct Templates {
    state: PathBuf,
    image: PathBuf,
    profile: Option<Key>,
    build: tokio::sync::Mutex<()>,
    cache: tokio::sync::RwLock<()>,
}

fn hash_file(path: &Path) -> io::Result<String> {
    use std::io::Read;
    let mut file = std::fs::File::open(path)?;
    let mut digest = blake3::Hasher::new();
    let mut buffer = [0; 65536];
    loop {
        let length = file.read(&mut buffer)?;
        if length == 0 {
            break;
        }
        digest.update(&buffer[..length]);
    }
    Ok(digest.finalize().to_hex().to_string())
}

impl Templates {
    pub(in crate::microvm) async fn new(state: &Path, image: &Path) -> Result<Self> {
        // Pool initialization has fenced/reaped the previous controller's VMs.
        // A crash can leave only unpublished staging directories behind.
        clear_incomplete(&state.join("templates")).await?;
        let enabled = match std::env::var("LEO_VM_SNAPSHOTS").as_deref() {
            Ok("true") => true,
            Ok("false") | Err(std::env::VarError::NotPresent) => false,
            _ => return Err(Error::bad("LEO_VM_SNAPSHOTS must be true or false.")),
        };
        let profile = if enabled {
            if !cfg!(feature = "ublk")
                || std::env::var("LEO_BLOCK_TRANSPORT").as_deref() != Ok("ublk")
                || std::env::var("LEO_DISK_LAYOUT").as_deref() != Ok("paired-ext4-v1")
            {
                return Err(Error::bad(
                    "VM snapshots require native ublk and paired-ext4-v1.",
                ));
            }
            let image = image.to_owned();
            Some(
                tokio::task::spawn_blocking(move || -> Result<Key> {
                    let output = std::process::Command::new("/usr/local/bin/firecracker")
                        .arg("--version")
                        .output()?;
                    if !output.status.success() {
                        return Err(Error::unavailable("Firecracker version unavailable."));
                    }
                    let firecracker = String::from_utf8(output.stdout).map_err(Error::internal)?;
                    let cpu = std::fs::read_to_string("/proc/cpuinfo")?;
                    let mut properties = cpu
                        .lines()
                        .filter(|line| {
                            matches!(
                                line.split(':').next().unwrap_or("").trim(),
                                "vendor_id"
                                    | "cpu family"
                                    | "model"
                                    | "stepping"
                                    | "microcode"
                                    | "flags"
                            )
                        })
                        .collect::<Vec<_>>();
                    properties.sort_unstable();
                    properties.dedup();
                    let kernel_host = std::fs::read_to_string("/proc/sys/kernel/osrelease")?;
                    let cpu = format!("{}\n{}", properties.join("\n"), kernel_host);
                    Ok(Key {
                        version: 1,
                        runtime: image.file_name().unwrap().to_str().unwrap().to_owned(),
                        kernel: hash_file(&image.join("vmlinux"))?,
                        entrypoint: hash_file(&std::env::current_exe()?)?,
                        firecracker: format!(
                            "{}:{}",
                            firecracker.trim(),
                            hash_file(Path::new("/usr/local/bin/firecracker"))?
                        ),
                        host_cpu: blake3::hash(cpu.as_bytes()).to_hex().to_string(),
                        cpu: 0,
                        memory_mib: 0,
                        disk_mib: 0,
                        layout: Layout::Flat,
                        policy: Value::Null,
                    })
                })
                .await
                .map_err(Error::internal)??,
            )
        } else {
            None
        };
        Ok(Self {
            state: state.to_owned(),
            image: image.to_owned(),
            profile,
            build: tokio::sync::Mutex::new(()),
            cache: tokio::sync::RwLock::new(()),
        })
    }

    pub(in crate::microvm) async fn prepare(
        &self,
        disk: &Path,
        slot: usize,
        stop: &CancellationToken,
        resources: &Value,
        policy: &Policy,
    ) -> Result<Option<Vm>> {
        self.prepare_inner(disk, slot, stop, resources, policy, true)
            .await
    }

    /// A foreground arrival never starts or awaits template construction.
    pub(in crate::microvm) async fn restore_cached(
        &self,
        disk: &Path,
        slot: usize,
        stop: &CancellationToken,
        resources: &Value,
        policy: &Policy,
    ) -> Result<Option<Vm>> {
        self.prepare_inner(disk, slot, stop, resources, policy, false)
            .await
    }

    async fn prepare_inner(
        &self,
        disk: &Path,
        slot: usize,
        stop: &CancellationToken,
        resources: &Value,
        policy: &Policy,
        build_missing: bool,
    ) -> Result<Option<Vm>> {
        let Some(profile) = &self.profile else {
            return Ok(None);
        };
        let state = &self.state;
        let image = &self.image;
        let resources_typed = super::resources(Some(resources))?;
        let mut key = profile.clone();
        key.cpu = resources_typed.cpu;
        key.memory_mib = resources_typed.memory_mi_b;
        key.disk_mib = resources_typed.disk_mi_b;
        key.layout = Layout::paired(key.disk_mib * 1_048_576)?;
        key.policy = serde_json::to_value(policy)?;
        let name = key.name()?;
        let root = state.join("templates");
        private_dir(&root).await?;
        let template = root.join(&name);
        let _build = if build_missing {
            Some(tokio::select! {
                biased;
                () = stop.cancelled() => return Err(Error::unavailable("Template preparation cancelled.")),
                guard = self.build.lock() => guard,
            })
        } else {
            None
        };
        // Readers can restore in parallel while another configuration builds.
        // Only atomic installation/eviction needs the short exclusive guard.
        let early_pin = if build_missing {
            None
        } else {
            Some(self.pin(stop).await?)
        };
        if !build_missing && !template.exists() {
            return Ok(None);
        }
        let mut timing = Operation::new("vm_template", &name, "lookup");
        if !template.exists() {
            let staging = tempfile::Builder::new()
                .prefix("partial-")
                .tempdir_in(&root)?;
            bootstrap::prepare_unassigned(disk, key.disk_mib * 1_048_576, policy, stop).await?;
            let mut vm =
                Vm::boot_template(state, image, disk.to_owned(), slot, stop, resources).await?;
            let result = async {
                timing.next("native_initialize");
                if !vm.warm_codex(state, stop).await? {
                    return Err(Error::unavailable(
                        "Guest cannot prebuild an anonymous template.",
                    ));
                }
                timing.next("capture");
                vm.capture_template(staging.path(), policy, stop).await
            }
            .await;
            vm.shutdown().await;
            result?;
            // The source VM has stopped and its exact sealed journal now lives
            // in staging. Never copy writable files while their DB is open.
            tokio::fs::remove_dir_all(disk).await?;
            atomic_write(&staging.path().join("key.json"), &serde_json::to_vec(&key)?).await?;
            atomic_write(
                &staging.path().join("disk/runtime.json"),
                &serde_json::to_vec(&RuntimeRecord {
                    runtime_id: Some(key.runtime.clone()),
                })?,
            )
            .await?;
            tokio::fs::File::open(staging.path())
                .await?
                .sync_all()
                .await?;
            if stop.is_cancelled() {
                return Err(Error::unavailable("Template preparation stopped."));
            }
            {
                let _mutation = tokio::select! {
                    biased;
                    () = stop.cancelled() => return Err(Error::unavailable("Template preparation cancelled.")),
                    guard = self.cache.write() => guard,
                };
                tokio::fs::rename(staging.path(), &template).await?;
                tokio::fs::File::open(&root).await?.sync_all().await?;
                Self::prune(&root, &name).await?;
            }
            tracing::info!(target: "leo_performance", operation = "vm_template", event = "built", id = name);
        }
        let _pin = match early_pin {
            Some(pin) => pin,
            None => self.pin(stop).await?,
        };
        let stored: Key =
            serde_json::from_slice(&tokio::fs::read(template.join("key.json")).await?)?;
        if stored != key {
            return Err(Error::conflict("Snapshot template configuration changed."));
        }
        timing.next("clone_journal");
        Self::clone_disk(&template.join("disk"), disk, stop).await?;
        // Move cache recency without changing any immutable memory/journal bytes.
        atomic_write(
            &template.join("last-used"),
            &crate::config::now().to_le_bytes(),
        )
        .await?;
        timing.next("restore");
        let vm = Vm::restore_template(
            state,
            image,
            disk.to_owned(),
            slot,
            stop,
            resources,
            &template,
        )
        .await?;
        timing.finish();
        Ok(Some(vm))
    }

    async fn pin(&self, stop: &CancellationToken) -> Result<tokio::sync::RwLockReadGuard<'_, ()>> {
        tokio::select! {
            biased;
            () = stop.cancelled() => Err(Error::unavailable("Template preparation cancelled.")),
            guard = self.cache.read() => Ok(guard),
        }
    }

    async fn clone_disk(source: &Path, target: &Path, stop: &CancellationToken) -> Result<()> {
        if target.exists() {
            return Err(Error::conflict("Snapshot destination already exists."));
        }
        private_dir(target).await?;
        let mut command = Command::new("cp");
        command
            .args(["-a", "--reflink=auto", "--"])
            .arg(source.join("lazy"))
            .arg(target.join("lazy"))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        let mut child = command.spawn()?;
        let result = tokio::select! {
            result = child.wait() => result?,
            () = stop.cancelled() => {
                let _ = child.kill().await;
                let _ = child.wait().await;
                return Err(Error::unavailable("Snapshot disk clone stopped."));
            }
        };
        if !result.success() {
            return Err(Error::unavailable("Snapshot journal cloning failed."));
        }
        // Flush only the copied journal, never unrelated conversations or
        // pending host writeback through a node-wide syncfs operation.
        let copied = target.to_owned();
        tokio::task::spawn_blocking(move || sync_tree(&copied))
            .await
            .map_err(Error::internal)??;
        atomic_write(
            &target.join("runtime.json"),
            &tokio::fs::read(source.join("runtime.json")).await?,
        )
        .await?;
        Ok(())
    }

    async fn prune(root: &Path, current: &str) -> Result<()> {
        let mut entries = tokio::fs::read_dir(root).await?;
        let mut old = Vec::new();
        while let Some(entry) = entries.next_entry().await? {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name == current
                || name.len() != 64
                || !name.bytes().all(|b| b.is_ascii_hexdigit())
                || !entry.file_type().await?.is_dir()
            {
                continue;
            }
            let metadata = tokio::fs::metadata(entry.path().join("last-used"))
                .await
                .or_else(|_| std::fs::metadata(entry.path().join("key.json")))?;
            old.push((metadata.modified()?, entry.path()));
        }
        old.sort_by_key(|(modified, _)| *modified);
        // Two configurations at most. Live clones retain hard links to their
        // memory/state files, so removing an old cache entry cannot break them.
        while old.len() > 1 {
            tokio::fs::remove_dir_all(old.remove(0).1).await?;
        }
        Ok(())
    }
}

async fn clear_incomplete(root: &Path) -> Result<()> {
    let mut entries = match tokio::fs::read_dir(root).await {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    let mut removed = false;
    while let Some(entry) = entries.next_entry().await? {
        if entry.file_name().to_string_lossy().starts_with("partial-")
            && entry.file_type().await?.is_dir()
        {
            tokio::fs::remove_dir_all(entry.path()).await?;
            removed = true;
        }
    }
    if removed {
        tokio::fs::File::open(root).await?.sync_all().await?;
    }
    Ok(())
}

fn sync_tree(root: &Path) -> io::Result<()> {
    let mut directories = vec![root.to_owned()];
    let mut index = 0;
    while index < directories.len() {
        for entry in std::fs::read_dir(&directories[index])? {
            let entry = entry?;
            let kind = entry.file_type()?;
            if kind.is_dir() {
                directories.push(entry.path());
            } else if kind.is_file() {
                std::fs::File::open(entry.path())?.sync_all()?;
            } else {
                return Err(io::Error::other("Invalid offline template journal entry"));
            }
        }
        index += 1;
    }
    for directory in directories.iter().rev() {
        std::fs::File::open(directory)?.sync_all()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn cancelled_reader_does_not_wait_for_cache_eviction_or_block_later_restores() {
        let templates = Arc::new(Templates {
            state: PathBuf::new(),
            image: PathBuf::new(),
            profile: None,
            build: tokio::sync::Mutex::new(()),
            cache: tokio::sync::RwLock::new(()),
        });
        let eviction = templates.cache.write().await;
        let stop = CancellationToken::new();
        let reader = {
            let templates = templates.clone();
            let stop = stop.clone();
            tokio::spawn(async move { templates.pin(&stop).await.map(|_| ()) })
        };
        tokio::task::yield_now().await;
        assert!(!reader.is_finished());
        stop.cancel();
        assert!(
            tokio::time::timeout(Duration::from_secs(1), reader)
                .await
                .unwrap()
                .unwrap()
                .is_err()
        );
        drop(eviction);
        let stop = CancellationToken::new();
        assert!(
            tokio::time::timeout(Duration::from_secs(1), templates.pin(&stop))
                .await
                .unwrap()
                .is_ok()
        );
    }

    #[tokio::test]
    async fn startup_reclaims_only_incomplete_templates() {
        let root = tempfile::tempdir().unwrap();
        let unfinished = root.path().join("partial-crashed");
        let published = root.path().join("a".repeat(64));
        private_dir(&unfinished).await.unwrap();
        private_dir(&published).await.unwrap();
        atomic_write(&unfinished.join("snapshot.mem"), b"incomplete")
            .await
            .unwrap();
        atomic_write(&published.join("snapshot.mem"), b"qualified")
            .await
            .unwrap();
        clear_incomplete(root.path()).await.unwrap();
        assert!(!unfinished.exists());
        assert_eq!(
            tokio::fs::read(published.join("snapshot.mem"))
                .await
                .unwrap(),
            b"qualified"
        );
        clear_incomplete(&root.path().join("absent")).await.unwrap();
    }

    #[test]
    fn cache_identity_covers_binary_kernel_hardware_layout_and_policy() {
        let key = Key {
            version: 1,
            runtime: "qualified-runtime".into(),
            kernel: "kernel-a".into(),
            entrypoint: "entrypoint-a".into(),
            firecracker: "fc-a".into(),
            host_cpu: "host-a".into(),
            cpu: 2,
            memory_mib: 4096,
            disk_mib: 32768,
            layout: Layout::paired(32768 * 1_048_576).unwrap(),
            policy: json!({ "reserveMiB": 64 }),
        };
        let original = key.name().unwrap();
        assert_eq!(original.len(), 64);
        assert_eq!(key.name().unwrap(), original);
        for field in [
            "version",
            "runtime",
            "kernel",
            "entrypoint",
            "firecracker",
            "hostCpu",
            "cpu",
            "memoryMib",
            "diskMib",
            "layout",
            "policy",
        ] {
            let mut encoded = serde_json::to_value(&key).unwrap();
            match field {
                "version" | "cpu" | "memoryMib" | "diskMib" => {
                    encoded[field] = json!(encoded[field].as_u64().unwrap() + 1);
                }
                "layout" => encoded[field] = json!({ "kind": "flat-ext4-v1" }),
                "policy" => encoded[field] = json!({ "reserveMiB": 128 }),
                _ => encoded[field] = json!("changed"),
            }
            let changed: Key = serde_json::from_value(encoded).unwrap();
            assert_ne!(changed.name().unwrap(), original, "{field}");
        }
    }

    #[tokio::test]
    async fn journal_clone_is_independent_and_memory_links_keep_the_same_inode() {
        use std::os::unix::fs::MetadataExt;
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("template");
        let journal = source.join("disk/lazy/segments");
        private_dir(&journal).await.unwrap();
        atomic_write(&journal.join("segment"), b"sealed-journal")
            .await
            .unwrap();
        atomic_write(
            &source.join("disk/runtime.json"),
            b"{\"runtimeId\":\"qualified\"}",
        )
        .await
        .unwrap();
        let stop = CancellationToken::new();
        let clone = root.path().join("clone");
        Templates::clone_disk(&source.join("disk"), &clone, &stop)
            .await
            .unwrap();
        assert_ne!(
            std::fs::metadata(journal.join("segment")).unwrap().ino(),
            std::fs::metadata(clone.join("lazy/segments/segment"))
                .unwrap()
                .ino()
        );
        tokio::fs::write(clone.join("lazy/segments/segment"), b"clone-update")
            .await
            .unwrap();
        assert_eq!(
            tokio::fs::read(journal.join("segment")).await.unwrap(),
            b"sealed-journal"
        );
        assert!(
            Templates::clone_disk(&source.join("disk"), &clone, &stop)
                .await
                .is_err()
        );
        for name in ["snapshot.mem", "snapshot.state"] {
            atomic_write(&source.join(name), b"immutable-template")
                .await
                .unwrap();
            tokio::fs::set_permissions(source.join(name), std::fs::Permissions::from_mode(0o444))
                .await
                .unwrap();
        }
        let jail = root.path().join("jail");
        private_dir(&jail).await.unwrap();
        link(&source, &jail).await.unwrap();
        assert_eq!(
            std::fs::metadata(source.join("snapshot.mem"))
                .unwrap()
                .ino(),
            std::fs::metadata(jail.join("snapshot.mem")).unwrap().ino()
        );
        tokio::fs::remove_dir_all(&source).await.unwrap();
        assert_eq!(
            tokio::fs::read(jail.join("snapshot.mem")).await.unwrap(),
            b"immutable-template"
        );
    }
}
