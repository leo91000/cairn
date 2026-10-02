//! Disposable transport prototype. Only explicitly provided fixture paths/devices
//! are touched. Both transports use the same production LazyDisk implementation.
use leo_agent_manager::storage::{self, BlockSource, Disk, DiskView, DiskWrite, LazyDisk};
use libublk::{BufDesc, UblkFlags, ctrl::UblkCtrlBuilder, helpers::IoBuf, io::UblkDev};
use serde_json::json;
use std::{
    collections::HashMap,
    fs::{self, File},
    io,
    os::fd::AsRawFd,
    os::unix::fs::FileExt,
    path::{Path, PathBuf},
    sync::{Arc, mpsc},
    time::Duration,
};
use tokio::sync::oneshot;
use vmm_sys_util::eventfd::EventFd;

const BLOCK: usize = 4 * 1024 * 1024;
const DEPTH: u16 = 64;
const IO_BYTES: u32 = 512 * 1024;
const MAX_BATCH_BYTES: usize = 8 * 1024 * 1024;

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

enum Command {
    Read { offset: u64, length: usize },
    Write { offset: u64, bytes: Vec<u8> },
    Flush,
}

struct Request {
    command: Command,
    done: oneshot::Sender<io::Result<Vec<u8>>>,
}

fn write_group(disk: &dyn Disk, group: &mut Vec<Request>, wake: &EventFd) {
    if group.is_empty() {
        return;
    }
    let writes: Vec<_> = group
        .iter()
        .map(|request| {
            let Command::Write { offset, bytes } = &request.command else {
                unreachable!()
            };
            DiskWrite {
                offset: *offset,
                bytes,
            }
        })
        .collect();
    // The production batch barrier persists every payload before any ACK.
    let result = disk.write_batch(&writes);
    for request in group.drain(..) {
        let reply = result
            .as_ref()
            .map(|()| Vec::new())
            .map_err(|error| io::Error::other(error.to_string()));
        let _ = request.done.send(reply);
    }
    wake.write(1).expect("wake durable batch completion");
}

fn worker(disk: &dyn Disk, receiver: &mpsc::Receiver<Request>, wake: &EventFd) {
    while let Ok(first) = receiver.recv() {
        let mut requests = vec![first];
        requests.extend(receiver.try_iter().take(usize::from(DEPTH) - 1));
        let mut writes = Vec::new();
        let mut bytes = 0;
        for request in requests {
            match &request.command {
                Command::Write { bytes: payload, .. } => {
                    if bytes + payload.len() > MAX_BATCH_BYTES {
                        write_group(disk, &mut writes, wake);
                        bytes = 0;
                    }
                    bytes += payload.len();
                    writes.push(request);
                }
                _ => {
                    write_group(disk, &mut writes, wake);
                    bytes = 0;
                    let result = match request.command {
                        Command::Read { offset, length } => {
                            let mut bytes = vec![0; length];
                            disk.read_at(offset, &mut bytes).map(|()| bytes)
                        }
                        Command::Flush => disk.sync().map(|()| Vec::new()),
                        Command::Write { .. } => unreachable!(),
                    };
                    let _ = request.done.send(result);
                    wake.write(1).expect("wake journal completion");
                }
            }
        }
        write_group(disk, &mut writes, wake);
    }
}

fn ublk(
    disk: Arc<dyn Disk>,
    device_id: i32,
    ready: PathBuf,
) -> Result<(), Box<dyn std::error::Error>> {
    let size = disk.size();
    let wake = Arc::new(EventFd::new(libc::EFD_CLOEXEC | libc::EFD_NONBLOCK)?);
    let writer_wake = wake.clone();
    let (sender, receiver) = mpsc::sync_channel(usize::from(DEPTH));
    let worker = std::thread::spawn(move || worker(disk.as_ref(), &receiver, &writer_wake));
    let control = UblkCtrlBuilder::default()
        .name("leo_journal_probe")
        .id(device_id)
        .nr_queues(1)
        .depth(DEPTH)
        .io_buf_bytes(IO_BYTES)
        .dev_flags(UblkFlags::UBLK_DEV_F_ADD_DEV)
        .build()?;
    control.run_target(
        move |dev: &mut UblkDev| {
            dev.tgt.dev_size = size;
            dev.tgt.params = libublk::sys::ublk_params {
                types: libublk::sys::UBLK_PARAM_TYPE_BASIC,
                basic: libublk::sys::ublk_param_basic {
                    // Writes are durable even without FUA; retain flush support
                    // so Firecracker's fsync barrier reaches the journal.
                    attrs: libublk::sys::UBLK_ATTR_VOLATILE_CACHE | libublk::sys::UBLK_ATTR_FUA,
                    logical_bs_shift: 9,
                    physical_bs_shift: 12,
                    io_opt_shift: 12,
                    io_min_shift: 9,
                    max_sectors: IO_BYTES >> 9,
                    dev_sectors: size >> 9,
                    ..Default::default()
                },
                ..Default::default()
            };
            dev.set_target_json(json!({"scope": "disposable-leo-journal-prototype"}));
            Ok(())
        },
        move |qid, dev| {
            let sender = sender.clone();
            libublk::UblkRuntime::run_io_tasks(dev, qid, move |queue, tag| {
                let sender = sender.clone();
                let wake = wake.clone();
                async move {
                    if tag == 0 {
                        // A channel waker alone cannot interrupt io_uring_enter.
                        // Poll completion eventfd in the same queue ring, avoiding
                        // libublk's one-second safety wakeup on idle queues.
                        libublk::executor::spawn_local(async move {
                            loop {
                                let event = libublk::ops::poll_add(
                                    libublk::ops::TgtFd::Raw(wake.as_raw_fd()),
                                    libc::POLLIN as u32,
                                )
                                .expect("arm completion wakeup");
                                if event.await < 0 {
                                    break;
                                }
                                let _ = wake.read();
                            }
                        });
                    }
                    let mut buffer = IoBuf::<u8>::new(IO_BYTES as usize);
                    queue
                        .submit_io_prep_cmd(
                            tag,
                            BufDesc::Slice(buffer.as_slice()),
                            0,
                            Some(&buffer),
                        )
                        .await?;
                    loop {
                        let descriptor = queue.get_iod(tag);
                        let operation = descriptor.op_flags & 0xff;
                        let offset = descriptor.start_sector.checked_mul(512);
                        let length = usize::try_from(descriptor.nr_sectors)
                            .unwrap_or(usize::MAX)
                            .saturating_mul(512);
                        let valid = length <= IO_BYTES as usize
                            && offset
                                .and_then(|offset| offset.checked_add(length as u64))
                                .is_some_and(|end| end <= size);
                        let command = match operation {
                            libublk::sys::UBLK_IO_OP_READ if valid => {
                                offset.map(|offset| Command::Read { offset, length })
                            }
                            libublk::sys::UBLK_IO_OP_WRITE if valid => {
                                offset.map(|offset| Command::Write {
                                    offset,
                                    bytes: buffer.as_slice()[..length].to_vec(),
                                })
                            }
                            libublk::sys::UBLK_IO_OP_FLUSH => Some(Command::Flush),
                            _ => None,
                        };
                        let result = if let Some(command) = command {
                            let (done, completion) = oneshot::channel();
                            if sender.try_send(Request { command, done }).is_err() {
                                -libc::EIO
                            } else {
                                match completion.await {
                                    Ok(Ok(bytes)) => {
                                        if operation == libublk::sys::UBLK_IO_OP_READ {
                                            buffer.as_mut_slice()[..length].copy_from_slice(&bytes);
                                        }
                                        if operation == libublk::sys::UBLK_IO_OP_FLUSH {
                                            0
                                        } else {
                                            length as i32
                                        }
                                    }
                                    _ => -libc::EIO,
                                }
                            }
                        } else {
                            -libc::EINVAL
                        };
                        queue
                            .submit_io_commit_cmd(tag, BufDesc::Slice(buffer.as_slice()), result)
                            .await?;
                    }
                    #[allow(unreachable_code)]
                    Ok::<(), libublk::UblkError>(())
                }
            })
            .expect("ublk queue failed");
        },
        move |control| {
            fs::write(&ready, control.get_bdev_path()).expect("write fixture readiness");
            println!("UBLK_READY {}", control.get_bdev_path());
        },
    )?;
    drop(control);
    worker.join().expect("journal worker failed");
    Ok(())
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
    if args.get(1).is_some_and(|value| value == "remove-stopped") {
        // The harness has already confirmed its backend and VMM are dead.
        // STOP alone does not unregister a device whose ADD owner was killed.
        let control = UblkCtrlBuilder::default().id(args[2].parse()?).build()?;
        if u32::from(control.dev_info().state) != libublk::sys::UBLK_S_DEV_DEAD {
            return Err("Refusing to remove an unstopped fixture device".into());
        }
        control.del_dev()?;
        return Ok(());
    }
    if args.get(1).is_some_and(|value| value == "export") {
        fs::write(format!("{}.export-request", args[2]), "")?;
        return Ok(());
    }
    if args.get(1).is_some_and(|value| value == "stop") {
        let control = UblkCtrlBuilder::default().id(args[2].parse()?).build()?;
        control.kill_dev()?;
        return Ok(());
    }
    let pair = args.get(1).is_some_and(|mode| mode == "ublk-pair");
    if args.len() != if pair { 8 } else { 7 }
        || !["ublk", "vhost", "ublk-pair"].contains(&args[1].as_str())
    {
        return Err(
            "Usage: ublk_probe ublk|vhost BASE JOURNAL READY METRICS ID_OR_SOCKET; ublk-pair BASE JOURNAL READY METRICS FIRST_ID SPLIT_BYTES; stop ID".into(),
        );
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
    if pair {
        let first_id: i32 = args[6].parse()?;
        let second_id = first_id
            .checked_add(1)
            .ok_or("Fixture device ID overflow")?;
        let split = args[7].parse()?;
        let os = Arc::new(DiskView::new(disk.clone(), 0, split)?);
        let workspace = Arc::new(DiskView::new(
            disk.clone(),
            split,
            disk.size().checked_sub(split).ok_or("Invalid split")?,
        )?);
        let ready_os = PathBuf::from(format!("{}.0", args[4]));
        let ready_workspace = PathBuf::from(format!("{}.1", args[4]));
        let worker = std::thread::spawn(move || {
            ublk(os, first_id, ready_os).map_err(|error| error.to_string())
        });
        ublk(workspace, second_id, ready_workspace)?;
        worker.join().map_err(|_| "OS device worker panicked")??;
        return Ok(());
    }
    if args[1] == "ublk" {
        return ublk(disk, args[6].parse()?, args[4].clone().into());
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
