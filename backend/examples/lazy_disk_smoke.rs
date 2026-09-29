//! Real Firecracker/jailer probe. Run as root with disposable fixture assets;
//! never use a production controller directory. See tests/lazy-disk-smoke.sh.
use leo_agent_manager::{
    nodes::snapshots,
    storage::{self, BlockSource, Disk, LazyDisk, LocalDisk},
};
use serde_json::json;
use std::{
    collections::HashMap,
    fs::File,
    io,
    path::Path,
    process::{Child, Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

struct Source {
    disk: LocalDisk,
    blocks: HashMap<String, (u64, usize)>,
    bytes: AtomicU64,
}

impl BlockSource for Source {
    fn fetch(&self, hash: &str) -> io::Result<Vec<u8>> {
        let (offset, size) = self
            .blocks
            .get(hash)
            .ok_or_else(|| io::Error::other("Unknown block"))?;
        let mut bytes = vec![0; *size];
        self.disk.read_at(*offset, &mut bytes)?;
        self.bytes.fetch_add(*size as u64, Ordering::SeqCst);
        Ok(bytes)
    }
}

struct Guest(Child);

impl Drop for Guest {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

// The jailer cannot see a FUSE mount created after it enters its mount namespace.
// Keep the mount anonymous until the prepared guest waits before its first disk I/O.
struct AssignedDisk {
    size: u64,
    disk: Mutex<Option<Arc<LazyDisk>>>,
    premature_io: AtomicU64,
}

impl AssignedDisk {
    fn current(&self) -> io::Result<Arc<LazyDisk>> {
        if let Some(disk) = self
            .disk
            .lock()
            .map_err(|e| io::Error::other(e.to_string()))?
            .clone()
        {
            Ok(disk)
        } else {
            self.premature_io.fetch_add(1, Ordering::SeqCst);
            Err(io::Error::other(
                "Prepared guest accessed its disk before assignment",
            ))
        }
    }
}

impl Disk for AssignedDisk {
    fn size(&self) -> u64 {
        self.size
    }

    fn read_at(&self, offset: u64, bytes: &mut [u8]) -> io::Result<()> {
        self.current()?.read_at(offset, bytes)
    }

    fn write_at(&self, offset: u64, bytes: &[u8]) -> io::Result<()> {
        self.current()?.write_at(offset, bytes)
    }

    fn sync(&self) -> io::Result<()> {
        self.current()?.sync()
    }
}

type Failure = Box<dyn std::error::Error>;

const JAIL_UID: u32 = 40001;

#[tokio::main]
async fn main() -> Result<(), Failure> {
    let assets = std::env::args()
        .nth(1)
        .ok_or("Missing disposable fixture assets")?;
    let assets = Path::new(&assets).canonicalize()?;
    if std::env::args().any(|arg| arg == "--cancel") {
        return cancelled_boot(&assets).await;
    }
    let prepared = std::env::args().any(|arg| arg == "--prepared");
    probe(&assets, prepared).await
}

/// Serves the fixture image's blocks, counting the bytes fetched.
fn fixture_source(
    assets: &Path,
    manifest: &serde_json::Value,
) -> Result<(Arc<Source>, u64), Failure> {
    let mut blocks = HashMap::new();
    for block in manifest["blocks"].as_array().ok_or("Missing blocks")? {
        if let Some(hash) = block["hash"].as_str() {
            let offset = block["offset"].as_u64().unwrap();
            let size = block["size"].as_u64().unwrap() as usize;
            blocks.insert(hash.to_owned(), (offset, size));
        }
    }
    let remote_bytes: u64 = blocks.values().map(|(_, size)| *size as u64).sum();
    let source = Arc::new(Source {
        disk: LocalDisk::open(&assets.join("data.ext4"), false)?,
        blocks,
        bytes: AtomicU64::new(0),
    });
    Ok((source, remote_bytes))
}

/// Links the kernel and root image into the jail and writes the VM configuration.
fn prepare_jail(assets: &Path, jail: &Path, prepared: bool) -> Result<(), Failure> {
    std::fs::create_dir_all(jail.join("disk"))?;
    let root_image = if prepared {
        "root-prepared.ext4"
    } else {
        "root.ext4"
    };
    std::fs::hard_link(assets.join(root_image), jail.join("root.ext4"))?;
    std::fs::hard_link(assets.join("vmlinux"), jail.join("vmlinux"))?;
    if prepared {
        let placeholder = jail.join("placeholder.ext4");
        File::create(&placeholder)?.set_len(4 * 1024 * 1024)?;
        std::os::unix::fs::chown(&placeholder, Some(JAIL_UID), Some(JAIL_UID))?;
    }
    let drive = if prepared {
        "placeholder.ext4"
    } else {
        "disk/data.ext4"
    };
    let config = json!({
        "boot-source": {
            "kernel_image_path": "vmlinux",
            "boot_args": "console=ttyS0 reboot=k panic=1 pci=off root=/dev/vda ro init=/init",
        },
        "drives": [
            {
                "drive_id": "root",
                "path_on_host": "root.ext4",
                "is_root_device": true,
                "is_read_only": true,
            },
            {
                "drive_id": "data",
                "path_on_host": drive,
                "is_root_device": false,
                "is_read_only": false,
                "cache_type": "Writeback",
            },
        ],
        "machine-config": { "vcpu_count": 1, "mem_size_mib": 128, "smt": false },
    });
    std::fs::write(jail.join("config.json"), serde_json::to_vec(&config)?)?;
    std::os::unix::fs::chown(jail.join("config.json"), Some(JAIL_UID), Some(JAIL_UID))?;
    Ok(())
}

fn spawn_guest(jails: &Path, id: &str, log: &Path) -> Result<Guest, Failure> {
    let output = File::create(log)?;
    let child = Command::new("/usr/local/bin/jailer")
        .args([
            "--id",
            id,
            "--exec-file",
            "/usr/local/bin/firecracker",
            "--uid",
            "40001",
            "--gid",
            "40001",
            "--cgroup-version",
            "2",
            "--chroot-base-dir",
        ])
        .arg(jails)
        .args([
            "--",
            "--api-sock",
            "api.sock",
            "--config-file",
            "config.json",
        ])
        .stdin(Stdio::null())
        .stdout(output.try_clone()?)
        .stderr(output)
        .spawn()?;
    Ok(Guest(child))
}

/// Checks the exported journal is a clean filesystem holding the guest's write.
fn verify_restored(restored: &Path) -> Result<(), Failure> {
    let check = Command::new("e2fsck")
        .args(["-fy"])
        .arg(restored)
        .output()?;
    if !matches!(check.status.code(), Some(0..=2)) {
        return Err(format!(
            "Guest disk check failed: {}",
            String::from_utf8_lossy(&check.stderr)
        )
        .into());
    }
    let result = Command::new("debugfs")
        .args(["-R", "cat /probe"])
        .arg(restored)
        .output()?;
    if !String::from_utf8_lossy(&result.stdout).contains("storage-after-guest-sync") {
        return Err(
            "Acknowledged guest write did not survive VMM termination and journal reopen".into(),
        );
    }
    Ok(())
}

async fn probe(assets: &Path, prepared: bool) -> Result<(), Failure> {
    let root = tempfile::tempdir_in(assets)?;
    let manifest = snapshots::index(&assets.join("data.ext4")).await?;
    let (source, remote_bytes) = fixture_source(assets, &manifest)?;
    let disk = Arc::new(LazyDisk::create(
        &root.path().join("journal"),
        &manifest,
        source.clone(),
    )?);
    let id = "storage-probe";
    let jails = root.path().join("jails");
    let jail = jails.join("firecracker").join(id).join("root");
    prepare_jail(assets, &jail, prepared)?;
    let assigned = prepared.then(|| {
        Arc::new(AssignedDisk {
            size: disk.size(),
            disk: Mutex::new(None),
            premature_io: AtomicU64::new(0),
        })
    });
    let mounted_disk: Arc<dyn Disk> = match &assigned {
        Some(assigned) => assigned.clone(),
        None => disk.clone(),
    };
    let mount = storage::fuse::mount_disk(mounted_disk, &jail.join("disk"), JAIL_UID)?;
    let log = root.path().join("console.log");
    let started = Instant::now();
    let mut guest = spawn_guest(&jails, id, &log)?;
    let mut prepared_ms = None;
    loop {
        let console = std::fs::read_to_string(&log)?;
        if prepared && prepared_ms.is_none() && console.contains("LEO_STORAGE_GUEST_PREPARED") {
            prepared_ms = Some(started.elapsed().as_millis());
            if source.bytes.load(Ordering::SeqCst) != 0 {
                return Err("Prepared guest fetched disk data before assignment".into());
            }
            *assigned.as_ref().unwrap().disk.lock().unwrap() = Some(disk.clone());
            patch_drive(&jail.join("api.sock"))?;
        }
        if console.contains("LEO_STORAGE_GUEST_SYNCED") {
            break;
        }
        if guest.0.try_wait()?.is_some() || started.elapsed() > Duration::from_secs(120) {
            return Err(format!("Guest failed to boot or sync: {console}").into());
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let ready_ms = started.elapsed().as_millis();
    let fetched = source.bytes.load(Ordering::SeqCst);
    if assigned
        .as_ref()
        .is_some_and(|disk| disk.premature_io.load(Ordering::SeqCst) != 0)
    {
        return Err("Prepared guest used the target disk before assignment".into());
    }
    if fetched >= remote_bytes {
        return Err("Guest fetched the whole remote disk before becoming ready".into());
    }
    drop(guest);
    mount.close()?;
    if let Some(assigned) = &assigned {
        assigned.disk.lock().unwrap().take();
    }
    drop(disk);
    let disk = LazyDisk::open(&root.path().join("journal"), source)?;
    let restored = root.path().join("restored.ext4");
    storage::export(&disk, &restored)?;
    verify_restored(&restored)?;
    let mode = if prepared {
        "prepared-drive-swap"
    } else {
        "cold"
    };
    let report = json!({
        "mode": mode,
        "preparedMs": prepared_ms,
        "guestReadyMs": ready_ms,
        "afterPreparedMs": prepared_ms.map(|ms| ready_ms - ms),
        "coldFetchedBytes": fetched,
        "remoteNonzeroBytes": remote_bytes,
        "virtualDiskBytes": disk.size(),
        "guestSyncSurvivedKill": true,
        "source": "local immutable block fixture, not S3",
    });
    println!("{report}");
    Ok(())
}

fn patch_drive(socket: &Path) -> Result<(), Failure> {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::os::unix::net::UnixStream;
    let mut stream = UnixStream::connect(socket)?;
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    let body = json!({
        "drive_id": "data",
        "path_on_host": "disk/data.ext4"
    })
    .to_string();
    write!(
        stream,
        "PATCH /drives/data HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )?;
    let mut reader = BufReader::new(stream);
    let mut status = String::new();
    reader.read_line(&mut status)?;
    if !status.starts_with("HTTP/1.1 204") && !status.starts_with("HTTP/1.1 200") {
        let mut content_length = 0;
        loop {
            let mut header = String::new();
            if reader.read_line(&mut header)? == 0 {
                return Err("Firecracker returned an incomplete response".into());
            }
            if header == "\r\n" {
                break;
            }
            if let Some(length) = header.to_ascii_lowercase().strip_prefix("content-length:") {
                content_length = length.trim().parse::<usize>()?.min(4096);
            }
        }
        let mut body = vec![0; content_length];
        reader.read_exact(&mut body)?;
        return Err(format!(
            "Firecracker rejected drive replacement: {status} {}",
            String::from_utf8_lossy(&body)
        )
        .into());
    }
    Ok(())
}

// Exercise the production VM lifecycle while its first disk read is unavailable.
async fn cancelled_boot(assets: &Path) -> Result<(), Failure> {
    use leo_agent_manager::microvm::host::Vm;
    use tokio_util::sync::CancellationToken;
    let root = tempfile::tempdir_in(assets)?;
    let state = root.path().join("state");
    let image = state.join("images/fixture");
    let directory = state.join("disks/fixture");
    std::fs::create_dir_all(&image)?;
    std::fs::create_dir_all(&directory)?;
    std::fs::copy(assets.join("root.ext4"), image.join("root.ext4"))?;
    std::fs::hard_link(assets.join("vmlinux"), image.join("vmlinux"))?;
    for instruction in ["mkdir /sbin", "symlink /sbin/leo-init /init"] {
        let result = Command::new("debugfs")
            .args(["-w", "-R", instruction])
            .arg(image.join("root.ext4"))
            .output()?;
        if !result.status.success() {
            return Err("Fixture initialization failed".into());
        }
    }
    let manifest = snapshots::index(&assets.join("data.ext4")).await?;
    let context = json!({
        "master": "http://127.0.0.1:1/",
        "grant": "unavailable-fixture",
        "policy": {
            "enabled": true,
            "cacheMiB": 8,
            "reserveMiB": 64,
            "reservePercent": 1,
            "backupSeconds": 60,
            "maxDirtySeconds": 300,
        },
    });
    let source = Arc::new(storage::remote::RemoteSource::new(
        &context,
        tokio::runtime::Handle::current(),
        CancellationToken::new(),
    )?);
    let disk = LazyDisk::create(&directory.join("lazy"), &manifest, source)?;
    disk.set_context(&context)?;
    drop(disk);
    let volume = storage::runtime::load(&directory).await?;
    let cancel = CancellationToken::new();
    let stopping = cancel.clone();
    let mut boot = tokio::spawn(async move {
        Vm::boot(
            &state,
            &image,
            directory,
            41,
            &stopping,
            Some(&json!({
                "cpu": 1,
                "memoryMiB": 128,
                "diskMiB": 256
            })),
        )
        .await
    });
    let ready = tokio::time::timeout(Duration::from_secs(30), async {
        while !volume.source.waiting() && !boot.is_finished() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    if ready.is_err() || !volume.source.waiting() {
        cancel.cancel();
        volume.stop.cancel();
        if let Ok(Ok(mut vm)) = boot.await {
            vm.shutdown().await;
        }
        return Err("Fixture did not reach the remote disk read".into());
    }
    let started = Instant::now();
    cancel.cancel();
    let result = tokio::time::timeout(Duration::from_secs(12), &mut boot).await;
    // Always release the fixture, including on the pre-fix failure path.
    volume.stop.cancel();
    match result {
        Ok(Ok(Err(error))) if error.message.contains("VM preparation stopped") => {}
        Ok(Ok(Ok(mut vm))) => {
            vm.shutdown().await;
            return Err("Cancelled boot unexpectedly completed".into());
        }
        Ok(Ok(Err(error))) => return Err(error.message.into()),
        Ok(Err(error)) => return Err(error.into()),
        Err(_) => {
            let _ = boot.await;
            return Err("Cancelled boot retained a blocked disk beyond 12 seconds".into());
        }
    }
    let report = json!({
        "mode": "cancel-blocked-boot",
        "cancelledMs": started.elapsed().as_millis(),
        "status": "passed",
    });
    println!("{report}");
    Ok(())
}
