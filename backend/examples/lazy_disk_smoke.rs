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
        Arc,
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

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let assets = std::env::args()
        .nth(1)
        .ok_or("Missing disposable fixture assets")?;
    let assets = Path::new(&assets).canonicalize()?;
    let root = tempfile::tempdir_in(&assets)?;
    let manifest = snapshots::index(&assets.join("data.ext4")).await?;
    let mut blocks = HashMap::new();
    for block in manifest["blocks"].as_array().ok_or("Missing blocks")? {
        if let Some(hash) = block["hash"].as_str() {
            blocks.insert(
                hash.to_owned(),
                (
                    block["offset"].as_u64().unwrap(),
                    block["size"].as_u64().unwrap() as usize,
                ),
            );
        }
    }
    let remote_bytes: u64 = blocks.values().map(|(_, size)| *size as u64).sum();
    let source = Arc::new(Source {
        disk: LocalDisk::open(&assets.join("data.ext4"), false)?,
        blocks,
        bytes: AtomicU64::new(0),
    });
    let disk = Arc::new(LazyDisk::create(
        &root.path().join("journal"),
        &manifest,
        source.clone(),
    )?);
    let id = "storage-probe";
    let jails = root.path().join("jails");
    let jail = jails.join("firecracker").join(id).join("root");
    std::fs::create_dir_all(jail.join("disk"))?;
    std::fs::hard_link(assets.join("root.ext4"), jail.join("root.ext4"))?;
    std::fs::hard_link(assets.join("vmlinux"), jail.join("vmlinux"))?;
    let mount = storage::fuse::mount_disk(disk.clone(), &jail.join("disk"), 40001)?;
    let config = json!({
        "boot-source":{"kernel_image_path":"vmlinux","boot_args":"console=ttyS0 reboot=k panic=1 pci=off root=/dev/vda ro init=/init"},
        "drives":[{"drive_id":"root","path_on_host":"root.ext4","is_root_device":true,"is_read_only":true}, {"drive_id":"data","path_on_host":"disk/data.ext4","is_root_device":false,"is_read_only":false,"cache_type":"Writeback"}],
        "machine-config":{"vcpu_count":1,"mem_size_mib":128,"smt":false}
    });
    std::fs::write(jail.join("config.json"), serde_json::to_vec(&config)?)?;
    std::os::unix::fs::chown(jail.join("config.json"), Some(40001), Some(40001))?;
    let log = root.path().join("console.log");
    let output = File::create(&log)?;
    let started = Instant::now();
    let mut guest = Guest(
        Command::new("/usr/local/bin/jailer")
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
            .arg(&jails)
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
            .spawn()?,
    );
    loop {
        let console = std::fs::read_to_string(&log)?;
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
    if fetched >= remote_bytes {
        return Err("Cold boot fetched the whole remote disk".into());
    }
    drop(guest);
    mount.close()?;
    drop(disk);
    let disk = LazyDisk::open(&root.path().join("journal"), source)?;
    let restored = root.path().join("restored.ext4");
    storage::export(&disk, &restored)?;
    let check = Command::new("e2fsck")
        .args(["-fy"])
        .arg(&restored)
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
        .arg(&restored)
        .output()?;
    if !String::from_utf8_lossy(&result.stdout).contains("storage-after-guest-sync") {
        return Err(
            "Acknowledged guest write did not survive VMM termination and journal reopen".into(),
        );
    }
    println!(
        "{}",
        json!({"guestReadyMs":ready_ms,"coldFetchedBytes":fetched,"remoteNonzeroBytes":remote_bytes,"virtualDiskBytes":disk.size(),"guestSyncSurvivedKill":true,"source":"local immutable block fixture, not S3"})
    );
    Ok(())
}
