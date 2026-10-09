//! Ordered, bounded durable I/O; kernel queues never wait on a disk operation.
use super::super::{Disk, DiskWrite};
use libublk::{BufDesc, ctrl::UblkCtrl, helpers::IoBuf, io::UblkDev};
use serde_json::json;
use std::{
    cell::{Cell, RefCell},
    io,
    os::fd::AsRawFd,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
};
use tokio::sync::oneshot;
use vmm_sys_util::eventfd::EventFd;

pub(super) const DEPTH: u16 = 64;
const IO_BYTES: u32 = 512 * 1024;
const MAX_BATCH_BYTES: usize = 8 * 1024 * 1024;

/// Journal writes service another block device's writeback. Ordinary dirty-page
/// throttling can wait for those same requests, creating a circular dependency.
/// Set this before creating the device so its writer and queue threads inherit
/// the flag. libublk tries this for its queue alone and ignores permission errors.
pub(super) fn enable_io_flusher() -> io::Result<()> {
    // Linux prctl ABI, available since 5.6; absent from libc's Linux constants.
    const PR_SET_IO_FLUSHER: i32 = 57;
    const PR_GET_IO_FLUSHER: i32 = 58;
    if unsafe { libc::prctl(PR_SET_IO_FLUSHER, 1_i64, 0_i64, 0_i64, 0_i64) } != 0 {
        let error = io::Error::last_os_error();
        return Err(io::Error::new(
            error.kind(),
            format!("ublk I/O threads require CAP_SYS_RESOURCE for IO_FLUSHER: {error}"),
        ));
    }
    if unsafe { libc::prctl(PR_GET_IO_FLUSHER, 0_i64, 0_i64, 0_i64, 0_i64) } != 1 {
        return Err(io::Error::other("ublk IO_FLUSHER state was not enabled"));
    }
    Ok(())
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
    let _ = wake.write(1);
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
                    let _ = wake.write(1);
                }
            }
        }
        write_group(disk, &mut writes, wake);
    }
}

pub(super) fn serve(
    disk: Arc<dyn Disk>,
    control: UblkCtrl,
    ready: std::sync::mpsc::Sender<super::Event>,
    stopped: Arc<AtomicBool>,
) -> io::Result<()> {
    let size = disk.size();
    let wake = Arc::new(EventFd::new(libc::EFD_CLOEXEC | libc::EFD_NONBLOCK)?);
    let writer_wake = wake.clone();
    let (sender, receiver) = mpsc::sync_channel(usize::from(DEPTH));
    let worker = std::thread::Builder::new()
        .name("cairn-ublk-journal".into())
        .spawn(move || -> io::Result<()> {
            enable_io_flusher()?;
            worker(disk.as_ref(), &receiver, &writer_wake);
            Ok(())
        })?;
    let result = control
        .run_target(
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
                dev.set_target_json(json!({ "backend": "cairn-journal" }));
                Ok(())
            },
            move |qid, dev| {
                let sender = sender.clone();
                let remaining = Rc::new(Cell::new(DEPTH));
                let wakeup = Rc::new(RefCell::new(None));
                let result = libublk::UblkRuntime::run_io_tasks(dev, qid, move |queue, tag| {
                    let sender = sender.clone();
                    let wake = wake.clone();
                    let remaining = remaining.clone();
                    let wakeup = wakeup.clone();
                    async move {
                        if tag == 0 {
                            // A channel waker alone cannot interrupt io_uring_enter.
                            // Poll completion eventfd in the same queue ring, avoiding
                            // libublk's one-second safety wakeup on idle queues.
                            let poll_wake = wake.clone();
                            let poll_remaining = remaining.clone();
                            *wakeup.borrow_mut() =
                                Some(libublk::executor::spawn_local(async move {
                                    loop {
                                        let event = libublk::ops::poll_add(
                                            libublk::ops::TgtFd::Raw(poll_wake.as_raw_fd()),
                                            libc::POLLIN as u32,
                                        )
                                        .expect("arm completion wakeup");
                                        if event.await < 0 || poll_remaining.get() == 0 {
                                            break;
                                        }
                                        let _ = poll_wake.read();
                                    }
                                }));
                        }
                        let result = async {
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
                                                    buffer.as_mut_slice()[..length]
                                                        .copy_from_slice(&bytes);
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
                                    .submit_io_commit_cmd(
                                        tag,
                                        BufDesc::Slice(buffer.as_slice()),
                                        result,
                                    )
                                    .await?;
                            }
                            #[allow(unreachable_code)]
                            Ok::<(), libublk::UblkError>(())
                        }
                        .await;
                        remaining.set(remaining.get() - 1);
                        if remaining.get() == 0 {
                            // Drain the last POLL_ADD before the queue runtime
                            // exits. Dropping a pending op makes libublk leak
                            // its slab deliberately to protect in-flight buffers.
                            // Keep the wakeup until every slot finishes: an
                            // aborted slot can still await its journal worker.
                            let _ = wake.write(1);
                            let wakeup = wakeup.borrow_mut().take();
                            if let Some(wakeup) = wakeup {
                                wakeup.await;
                            }
                        }
                        result
                    }
                });
                if let Err(error) = result {
                    tracing::debug!(%error, "ublk queue ended");
                }
            },
            move |control| {
                if stopped.load(Ordering::Acquire) || ready.send(super::Event::Ready).is_err() {
                    stopped.store(true, Ordering::Release);
                    let _ = control.kill_dev();
                }
            },
        )
        .map_err(super::error);
    if result.is_err() {
        // run_target can fail after spawning queues. Stop those queues before
        // joining the disk worker, including when kernel START was rejected.
        let _ = control.stop_dev();
    }
    drop(control);
    worker
        .join()
        .map_err(|_| io::Error::other("ublk journal worker panicked"))??;
    result.map(|_| ())
}
