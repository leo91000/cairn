//! Disposable qualification harness for the managed ublk and vhost transports.
//! Only explicitly provided fixture paths are touched.
use leo_agent_manager::storage::{self, BlockSource, LazyDisk};
use libublk::ctrl::UblkCtrlBuilder;
use serde_json::json;
use std::{
    collections::HashMap,
    fs::{self, File},
    io,
    os::unix::fs::FileExt,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

const BLOCK: usize = 4 * 1024 * 1024;

struct FileSource {
    file: File,
    extents: HashMap<String, (u64, usize)>,
    origin: PathBuf,
}

impl BlockSource for FileSource {
    fn fetch(&self, hash: &str) -> io::Result<Vec<u8>> {
        let published = self.origin.join(hash);
        if published.exists() {
            return fs::read(published);
        }
        let (offset, length) = self
            .extents
            .get(hash)
            .ok_or_else(|| io::Error::other("Missing fixture block"))?;
        let mut bytes = vec![0; *length];
        self.file.read_exact_at(&mut bytes, *offset)?;
        Ok(bytes)
    }
}

fn disk(base: &Path, directory: &Path) -> io::Result<Arc<LazyDisk>> {
    let file = File::open(base)?;
    let size = file.metadata()?.len();
    if size == 0 || size > 8 * 1024 * 1024 * 1024 {
        return Err(io::Error::other(
            "Expected a nonempty fixture image <=8 GiB",
        ));
    }
    let mut extents = HashMap::new();
    let mut blocks = Vec::new();
    for offset in (0..size).step_by(BLOCK) {
        let length = (size - offset).min(BLOCK as u64) as usize;
        let mut bytes = vec![0; length];
        file.read_exact_at(&mut bytes, offset)?;
        let hash = bytes
            .iter()
            .any(|byte| *byte != 0)
            .then(|| storage::digest::block(&bytes));
        if let Some(hash) = &hash {
            extents.insert(hash.clone(), (offset, length));
        }
        blocks.push(json!({"offset": offset, "size": length, "hash": hash}));
    }
    let origin = base
        .parent()
        .ok_or_else(|| io::Error::other("Missing fixture base directory"))?
        .join("source-origin");
    fs::create_dir_all(&origin)?;
    let source = Arc::new(FileSource {
        file,
        extents,
        origin,
    });
    let restore = base.with_extension("restore-manifest.json");
    let manifest = if restore.exists() {
        serde_json::from_slice(&fs::read(restore)?)?
    } else {
        json!({"version": 1, "size": size, "blockSize": BLOCK, "blocks": blocks})
    };
    let disk = if directory.join("journal-v2.sqlite").exists() {
        LazyDisk::open(directory, source)?
    } else {
        LazyDisk::create(directory, &manifest, source)?
    };
    disk.set_context(&json!({"policy": storage::policy::Policy::default()}))?;
    Ok(Arc::new(disk))
}

fn operation(
    disk: &LazyDisk,
    origin: &Path,
    metrics: &str,
    request: &serde_json::Value,
) -> io::Result<serde_json::Value> {
    let generation = || {
        request["generation"]
            .as_i64()
            .filter(|value| *value > 0)
            .ok_or_else(|| io::Error::other("Missing generation"))
    };
    match request["op"].as_str() {
        Some("seal") => Ok(json!({"generation": disk.seal_completed()?})),
        Some("capture") => {
            let generation = generation()?;
            let manifest = disk.capture(generation)?;
            for block in manifest["blocks"]
                .as_array()
                .ok_or_else(|| io::Error::other("Missing captured blocks"))?
            {
                let Some(hash) = block["hash"].as_str() else {
                    continue;
                };
                let bytes = disk.captured_block(generation, hash)?;
                let target = origin.join(hash);
                if target.exists() {
                    continue;
                }
                let temporary = origin.join(format!("{hash}.partial"));
                fs::write(&temporary, bytes)?;
                File::open(&temporary)?.sync_all()?;
                fs::rename(temporary, target)?;
            }
            File::open(origin)?.sync_all()?;
            let target = format!("{metrics}.capture-{generation}.json");
            fs::write(&target, manifest.to_string())?;
            File::open(&target)?.sync_all()?;
            File::open(Path::new(&target).parent().unwrap())?.sync_all()?;
            Ok(json!({"manifest": manifest}))
        }
        Some("published") => {
            let id = request["backupId"]
                .as_str()
                .ok_or_else(|| io::Error::other("Missing backup ID"))?;
            disk.commit_published(generation()?, id)?;
            Ok(disk.accounting()?)
        }
        Some("status") => Ok(
            json!({"accounting": disk.accounting()?, "completed": disk.completed()?, "performance": disk.performance()}),
        ),
        Some("export") => {
            let target = PathBuf::from(format!("{metrics}.ext4"));
            storage::export(disk, &target)?;
            Ok(json!({"path": target}))
        }
        _ => Err(io::Error::other("Unknown fixture control operation")),
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().collect();
    if args.get(1).is_some_and(|value| value == "inspect-device") {
        let control = UblkCtrlBuilder::default().id(args[2].parse()?).build()?;
        let info = control.dev_info();
        println!(
            "{}",
            json!({
                "deviceId": info.dev_id,
                "token": info.ublksrv_flags.to_string(),
                "state": info.state,
                "pid": info.ublksrv_pid
            })
        );
        return Ok(());
    }
    if args.get(1).is_some_and(|value| value == "cleanup-managed") {
        let state = Path::new(&args[2]);
        let _lock = leo_agent_manager::file_lock::exclusive(
            &state.join("controller.lock"),
            "Fixture controller is active.",
        )?;
        storage::ublk::cleanup_stale(state)?;
        return Ok(());
    }

    if args.get(1).is_some_and(|value| value == "export") {
        fs::write(format!("{}.export-request", args[2]), "")?;
        return Ok(());
    }
    if args.len() != 7 || !["vhost", "ublk-managed"].contains(&args[1].as_str()) {
        return Err("Usage: ublk_probe vhost|ublk-managed BASE JOURNAL READY METRICS SOCKET_OR_STATE; cleanup-managed STATE; inspect-device ID; export METRICS".into());
    }
    let disk = disk(Path::new(&args[2]), Path::new(&args[3]))?;
    let metrics = args[5].clone();
    let observer = disk.clone();
    let origin = Path::new(&args[2]).parent().unwrap().join("source-origin");
    std::thread::spawn(move || -> io::Result<()> {
        loop {
            let control = format!("{metrics}.control-request");
            if Path::new(&control).exists() {
                let request: serde_json::Value = serde_json::from_slice(&fs::read(&control)?)?;
                let result = operation(&observer, &origin, &metrics, &request);
                let reply = match result {
                    Ok(value) => json!({"id": request["id"], "ok": true, "result": value}),
                    Err(error) => {
                        json!({"id": request["id"], "ok": false, "error": error.to_string()})
                    }
                };
                fs::write(format!("{metrics}.control-reply.tmp"), reply.to_string())?;
                fs::remove_file(control)?;
                fs::rename(
                    format!("{metrics}.control-reply.tmp"),
                    format!("{metrics}.control-reply"),
                )?;
            }
            let request = format!("{metrics}.export-request");
            if Path::new(&request).exists() {
                // The harness freezes the guest and pauses its CPUs first. This
                // snapshot export never accesses another conversation's journal.
                let result =
                    storage::export(observer.as_ref(), Path::new(&format!("{metrics}.ext4")));
                fs::write(format!("{metrics}.export-done"), json!({"ok": result.is_ok(), "error": result.err().map(|error| error.to_string())}).to_string()).unwrap();
                fs::remove_file(request).unwrap();
            }
            let temporary = format!("{metrics}.tmp");
            fs::write(&temporary, observer.performance().to_string()).unwrap();
            fs::rename(&temporary, &metrics).unwrap();
            std::thread::sleep(Duration::from_millis(200));
        }
    });

    if args[1] == "ublk-managed" {
        let state = Path::new(&args[6]);
        fs::create_dir_all(state)?;
        let _lock = leo_agent_manager::file_lock::exclusive(
            &state.join("controller.lock"),
            "Fixture controller is active.",
        )?;
        storage::ublk::cleanup_stale(state)?;
        let jail = state
            .join("jails/firecracker")
            .join(uuid::Uuid::new_v4().to_string())
            .join("root");
        fs::create_dir_all(&jail)?;
        let mounted = storage::ublk::mount_disk(disk, state, &jail, 40001)?;
        fs::write(
            &args[4],
            jail.join("disk.blk")
                .to_str()
                .ok_or("Invalid private fixture jail")?,
        )?;
        loop {
            if Path::new(&format!("{}.managed-stop", args[5])).exists() {
                mounted.close()?;
                return Ok(());
            }
            if mounted.failed() {
                return Err("Managed ublk backend stopped".into());
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    let mounted = storage::vhost::mount_disk(disk, Path::new(&args[6]), 40001)?;
    fs::write(&args[4], &args[6])?;
    loop {
        if mounted.failed() {
            return Err("vhost queue failed".into());
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}
