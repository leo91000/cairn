//! Direct virtqueue adapter with bounded, durable write batches.
//! Own guest payloads before hashing them; guest RAM is concurrently mutable.
use super::{Disk, DiskWrite};
use std::{
    io,
    os::unix::{fs::PermissionsExt, net::UnixStream},
    path::{Path, PathBuf},
    sync::{
        Arc, RwLock,
        atomic::{AtomicBool, Ordering},
    },
};
use vhost::vhost_user::{
    Listener,
    message::{VhostUserProtocolFeatures, VhostUserVirtioFeatures},
};
use vhost_user_backend::{
    ShutdownHandle, VhostUserBackendMut, VhostUserDaemon, VringRwLock, VringT,
    bitmap::BitmapMmapRegion,
};
use virtio_bindings::{
    virtio_blk::*, virtio_config::VIRTIO_F_VERSION_1, virtio_ring::VIRTIO_RING_F_EVENT_IDX,
};
use virtio_queue::{DescriptorChain, QueueOwnedT};
use vm_memory::{
    Bytes, GuestAddress, GuestAddressSpace, GuestMemory, GuestMemoryAtomic, GuestMemoryMmap,
    Permissions,
};
use vmm_sys_util::{
    epoll::EventSet,
    event::{EventConsumer, EventNotifier},
    eventfd::EventFd,
};

type Memory = GuestMemoryMmap<BitmapMmapRegion>;

type Vring = VringRwLock<GuestMemoryAtomic<Memory>>;

const MAX_BYTES: usize = 8 * 1024 * 1024;
const MAX_REQUESTS: usize = 128;

fn error(value: impl std::fmt::Display) -> io::Error {
    io::Error::other(value.to_string())
}

struct Request {
    head: u16,
    status: GuestAddress,
    kind: u32,
    offset: u64,
    segments: Vec<(GuestAddress, usize)>,
    length: usize,
    bytes: Vec<u8>,
}

impl Request {
    fn parse<M: std::ops::Deref<Target = Memory> + Clone>(
        chain: &DescriptorChain<M>,
        disk_size: u64,
    ) -> io::Result<Self> {
        let head = chain.head_index();
        let memory = chain.memory();
        let descriptors: Vec<_> = chain.clone().take(MAX_REQUESTS + 3).collect();
        if descriptors.len() < 2 || descriptors.len() > MAX_REQUESTS + 2 {
            return Err(error("Invalid block descriptor count"));
        }
        let header = descriptors.first().unwrap();
        let status = descriptors.last().unwrap();
        if header.is_write_only()
            || header.len() < 16
            || !status.is_write_only()
            || status.len() != 1
            || status.has_next()
            || !memory.check_range(status.addr(), 1, Permissions::Write)
        {
            return Err(error("Invalid block header or status descriptor"));
        }
        let mut raw = [0; 16];
        memory.read_slice(&mut raw, header.addr()).map_err(error)?;
        let kind = u32::from_le_bytes(raw[..4].try_into().unwrap());
        let offset = u64::from_le_bytes(raw[8..].try_into().unwrap())
            .checked_mul(512)
            .ok_or_else(|| error("Block sector overflow"))?;
        let mut segments = Vec::new();
        let mut length = 0usize;
        for descriptor in &descriptors[1..descriptors.len() - 1] {
            if matches!(kind, 0 | 8) && !descriptor.is_write_only()
                || kind == 1 && descriptor.is_write_only()
            {
                return Err(error("Invalid block payload direction"));
            }
            let count = descriptor.len() as usize;
            let access = if descriptor.is_write_only() {
                Permissions::Write
            } else {
                Permissions::Read
            };
            if !memory.check_range(descriptor.addr(), count, access) {
                return Err(error("Invalid block payload address"));
            }
            length = length
                .checked_add(count)
                .filter(|size| *size <= MAX_BYTES)
                .ok_or_else(|| error("Block request exceeds byte limit"))?;
            segments.push((descriptor.addr(), count));
        }
        if matches!(kind, 0 | 1) {
            super::device::range(disk_size, offset, length)?;
            if length == 0 || !length.is_multiple_of(512) {
                return Err(error("Invalid block request alignment"));
            }
        }
        if kind == 4 && length != 0 {
            return Err(error("Flush request has a payload"));
        }
        Ok(Self {
            head,
            status: status.addr(),
            kind,
            offset,
            segments,
            length,
            bytes: Vec::new(),
        })
    }

    fn load_payload(&mut self, memory: &Memory) -> io::Result<()> {
        self.bytes.resize(self.length, 0);
        if self.kind == VIRTIO_BLK_T_OUT {
            let mut copied = 0;
            for (address, count) in &self.segments {
                memory
                    .read_slice(&mut self.bytes[copied..copied + count], *address)
                    .map_err(error)?;
                copied += count;
            }
        }
        Ok(())
    }
}

struct Backend {
    disk: Arc<dyn Disk>,
    memory: GuestMemoryAtomic<Memory>,
    kill: Arc<EventFd>,
    event_idx: bool,
    failed: Arc<AtomicBool>,
}

struct QueueHealth<'a> {
    failed: &'a AtomicBool,
    succeeded: bool,
}

impl Drop for QueueHealth<'_> {
    fn drop(&mut self) {
        if !self.succeeded {
            self.failed.store(true, Ordering::Release);
        }
    }
}

impl Backend {
    fn complete(&self, vring: &Vring, request: &Request, status: u8, used: u32) -> io::Result<()> {
        let memory = self.memory.memory();
        memory.write_obj(status, request.status).map_err(error)?;
        vring.add_used(request.head, used).map_err(error)
    }

    fn write_group(&self, vring: &Vring, requests: &mut Vec<Request>) -> io::Result<()> {
        if requests.is_empty() {
            return Ok(());
        }
        let writes: Vec<_> = requests
            .iter()
            .map(|request| DiskWrite {
                offset: request.offset,
                bytes: &request.bytes,
            })
            .collect();
        let status = if self.disk.write_batch(&writes).is_ok() {
            0
        } else {
            1
        };
        for request in requests.drain(..) {
            self.complete(vring, &request, status, 1)?;
        }
        Ok(())
    }

    fn process_queue(&self, vring: &Vring) -> io::Result<bool> {
        let memory = self.memory.memory();
        let requests = {
            let mut state = vring.get_mut();
            state
                .get_queue_mut()
                .iter(memory.clone())
                .map_err(error)?
                .take(MAX_REQUESTS)
                .map(|chain| Request::parse(&chain, self.disk.size()))
                .collect::<io::Result<Vec<_>>>()?
        };
        if requests.is_empty() {
            return Ok(false);
        }
        let mut pending = Vec::new();
        let mut pending_bytes = 0usize;
        for mut request in requests {
            if request.kind == 1 {
                if pending_bytes + request.length > MAX_BYTES {
                    self.write_group(vring, &mut pending)?;
                    pending_bytes = 0;
                }
                request.load_payload(&memory)?;
                pending_bytes += request.length;
                pending.push(request);
                continue;
            }
            self.write_group(vring, &mut pending)?;
            pending_bytes = 0;
            let (status, used) = match request.kind {
                VIRTIO_BLK_T_IN => {
                    request.load_payload(&memory)?;
                    match self.disk.read_at(request.offset, &mut request.bytes) {
                        Ok(()) => {
                            let mut copied = 0;
                            for (address, count) in &request.segments {
                                memory
                                    .write_slice(&request.bytes[copied..copied + count], *address)
                                    .map_err(error)?;
                                copied += count;
                            }
                            (0, request.bytes.len() as u32 + 1)
                        }
                        Err(_) => (1, 1),
                    }
                }
                4 => (u8::from(self.disk.sync().is_err()), 1),
                8 => (2, 1),
                _ => (2, 1),
            };
            self.complete(vring, &request, status, used)?;
        }
        self.write_group(vring, &mut pending)?;
        if !self.event_idx || vring.needs_notification().map_err(error)? {
            vring.signal_used_queue().map_err(error)?;
        }
        Ok(true)
    }
}

impl VhostUserBackendMut for Backend {
    type Bitmap = BitmapMmapRegion;

    type Vring = Vring;

    fn num_queues(&self) -> usize {
        1
    }

    fn max_queue_size(&self) -> usize {
        256
    }

    fn features(&self) -> u64 {
        (1 << VIRTIO_BLK_F_SEG_MAX)
            | (1 << VIRTIO_BLK_F_BLK_SIZE)
            | (1 << VIRTIO_BLK_F_FLUSH)
            | (1 << VIRTIO_RING_F_EVENT_IDX)
            | (1 << VIRTIO_F_VERSION_1)
            | VhostUserVirtioFeatures::PROTOCOL_FEATURES.bits()
    }

    fn protocol_features(&self) -> VhostUserProtocolFeatures {
        VhostUserProtocolFeatures::CONFIG
    }

    fn set_event_idx(&mut self, enabled: bool) {
        self.event_idx = enabled;
    }

    fn get_config(&self, offset: u32, size: u32) -> Vec<u8> {
        let mut config = [0u8; 64];
        config[..8].copy_from_slice(&(self.disk.size() / 512).to_le_bytes());
        config[12..16].copy_from_slice(&(MAX_REQUESTS as u32).to_le_bytes());
        config[20..24].copy_from_slice(&512u32.to_le_bytes());
        let Some(end) = offset.checked_add(size) else {
            return Vec::new();
        };
        config
            .get(offset as usize..end as usize)
            .unwrap_or(&[])
            .to_vec()
    }

    fn update_memory(&mut self, memory: GuestMemoryAtomic<Memory>) -> io::Result<()> {
        self.memory = memory;
        Ok(())
    }

    fn handle_event(
        &mut self,
        event: u16,
        events: EventSet,
        vrings: &[Vring],
        _: usize,
    ) -> io::Result<()> {
        // Report both ordinary errors and unwinding worker failures to the VM watcher.
        let mut health = QueueHealth {
            failed: &self.failed,
            succeeded: false,
        };
        let result = (|| {
            if event != 0 || events != EventSet::IN || vrings.len() != 1 {
                return Err(error("Invalid block queue event"));
            }
            let vring = &vrings[0];
            if self.event_idx {
                loop {
                    vring.disable_notification().map_err(error)?;
                    self.process_queue(vring)?;
                    if !vring.enable_notification().map_err(error)? {
                        break;
                    }
                }
            } else {
                while self.process_queue(vring)? {}
            }
            Ok(())
        })();
        health.succeeded = result.is_ok();
        result
    }

    fn exit_event(&self, _: usize) -> Option<(EventConsumer, EventNotifier)> {
        use std::os::fd::{FromRawFd, IntoRawFd};
        // Owned eventfd clones, converted to the library's directional handles.
        unsafe {
            Some((
                EventConsumer::from_raw_fd(self.kill.try_clone().ok()?.into_raw_fd()),
                EventNotifier::from_raw_fd(self.kill.try_clone().ok()?.into_raw_fd()),
            ))
        }
    }
}

pub struct MountedDisk {
    socket: PathBuf,
    stopped: Arc<AtomicBool>,
    kill: Arc<EventFd>,
    thread: Option<std::thread::JoinHandle<io::Result<()>>>,
    shutdown: Arc<RwLock<Option<ShutdownHandle>>>,
    failed: Arc<AtomicBool>,
    memory: GuestMemoryAtomic<Memory>,
}

impl MountedDisk {
    /// Actual allocated shared pages, including pages absent from the VMM's
    /// RSS. Guest regions can share one memfd; count its inode only once.
    pub fn allocated_memory_bytes(&self) -> io::Result<u64> {
        allocated_memory_bytes(&self.memory.memory())
    }

    pub fn failed(&self) -> bool {
        self.failed.load(Ordering::Acquire)
            || self
                .thread
                .as_ref()
                .is_some_and(std::thread::JoinHandle::is_finished)
    }

    pub fn close(mut self) -> io::Result<()> {
        self.close_inner()
    }

    fn close_inner(&mut self) -> io::Result<()> {
        let Some(thread) = self.thread.take() else {
            return Ok(());
        };
        self.stopped.store(true, Ordering::Release);
        let wake = self.kill.write(1);
        if let Some(handle) = self.shutdown.read().unwrap().as_ref() {
            handle.shutdown();
        }
        let _ = UnixStream::connect(&self.socket);
        let result = thread.join().map_err(|_| error("Block backend panicked"));
        let _ = std::fs::remove_file(&self.socket);
        wake?;
        result?
    }
}

impl Drop for MountedDisk {
    fn drop(&mut self) {
        let _ = self.close_inner();
    }
}

pub fn mount_disk(disk: Arc<dyn Disk>, socket: &Path, uid: u32) -> io::Result<MountedDisk> {
    let path = socket
        .to_str()
        .ok_or_else(|| error("Invalid block socket path"))?;
    let mut listener = Listener::new(path, false).map_err(error)?;
    std::fs::set_permissions(socket, std::fs::Permissions::from_mode(0o600))?;
    std::os::unix::fs::chown(socket, Some(uid), Some(uid))?;
    let memory = GuestMemoryAtomic::new(Memory::new());
    let kill = Arc::new(EventFd::new(libc::EFD_NONBLOCK)?);
    let failed = Arc::new(AtomicBool::new(false));
    let backend = Arc::new(RwLock::new(Backend {
        disk,
        memory: memory.clone(),
        kill: kill.clone(),
        event_idx: false,
        failed: failed.clone(),
    }));
    let mut daemon =
        VhostUserDaemon::new("leo-block".to_owned(), backend, memory.clone()).map_err(error)?;
    let stopped = Arc::new(AtomicBool::new(false));
    let stop = stopped.clone();
    let exit = kill.clone();
    let shutdown = Arc::new(RwLock::new(None));
    let control = shutdown.clone();
    let failure = failed.clone();
    let thread = std::thread::spawn(move || {
        let result = (|| {
            daemon.start(&mut listener).map_err(error)?;
            *control.write().unwrap() = daemon.shutdown_handle();
            if stop.load(Ordering::Acquire) {
                daemon.request_shutdown();
            }
            daemon.wait().map_err(error)
        })();
        if !stop.load(Ordering::Acquire) {
            failure.store(true, Ordering::Release);
        }
        let _ = exit.write(1);
        result
    });
    Ok(MountedDisk {
        socket: socket.to_owned(),
        stopped,
        kill,
        thread: Some(thread),
        shutdown,
        failed,
        memory,
    })
}

fn allocated_memory_bytes(memory: &Memory) -> io::Result<u64> {
    use std::{collections::HashSet, os::unix::fs::MetadataExt};
    use vm_memory::{GuestMemoryBackend, GuestMemoryRegion};
    let mut files = HashSet::new();
    let mut bytes = 0u64;
    for region in memory.iter() {
        let file = region
            .file_offset()
            .ok_or_else(|| error("Guest memory is not file-backed"))?
            .file();
        let metadata = file.metadata()?;
        if files.insert((metadata.dev(), metadata.ino())) {
            bytes = bytes
                .checked_add(
                    metadata
                        .blocks()
                        .checked_mul(512)
                        .ok_or_else(|| error("Guest memory accounting overflow"))?,
                )
                .ok_or_else(|| error("Guest memory accounting overflow"))?;
        }
    }
    if files.is_empty() {
        return Err(error("Guest memory has not been registered"));
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests;
