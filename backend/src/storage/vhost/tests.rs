use super::*;
use std::sync::Mutex;
use virtio_bindings::virtio_ring::{VRING_DESC_F_NEXT, VRING_DESC_F_WRITE};

const DESC: u64 = 0x1000;
const AVAIL: u64 = 0x3000;
const USED: u64 = 0x4000;
const HEADER: u64 = 0x8000;
const PAYLOAD: u64 = 0x100000;

struct FixtureDisk {
    memory: GuestMemoryAtomic<Memory>,
    statuses: Vec<GuestAddress>,
    events: Mutex<Vec<String>>,
    fail: bool,
    panic_write: bool,
}

impl Disk for FixtureDisk {
    fn size(&self) -> u64 {
        32 * 1024 * 1024
    }

    fn read_at(&self, _: u64, bytes: &mut [u8]) -> io::Result<()> {
        self.events.lock().unwrap().push("read".into());
        bytes.fill(0x5a);
        Ok(())
    }

    fn write_at(&self, _: u64, _: &[u8]) -> io::Result<()> {
        panic!("Queue must use the durable batch interface")
    }

    fn write_batch(&self, writes: &[DiskWrite<'_>]) -> io::Result<()> {
        assert!(!self.panic_write, "Injected queue worker panic");
        let memory = self.memory.memory();
        let events = &mut *self.events.lock().unwrap();
        let start = events
            .iter()
            .filter(|event| event.starts_with("write:"))
            .map(|event| event[6..].parse::<usize>().unwrap())
            .sum::<usize>();
        for status in &self.statuses[start..start + writes.len()] {
            assert_eq!(
                memory.read_obj::<u8>(*status).unwrap(),
                0xff,
                "ACK preceded durable batch completion"
            );
        }
        assert!(writes.iter().map(|write| write.bytes.len()).sum::<usize>() <= MAX_BYTES);
        for write in writes {
            assert!(write.bytes.iter().all(|byte| *byte == 0x37));
        }
        // The guest can change its pages while the backend owns a payload.
        memory.write_obj(0u8, GuestAddress(PAYLOAD)).unwrap();
        assert_eq!(writes[0].bytes[0], 0x37);
        events.push(format!("write:{}", writes.len()));
        if self.fail {
            return Err(error("Injected batch failure"));
        }
        Ok(())
    }

    fn sync(&self) -> io::Result<()> {
        self.events.lock().unwrap().push("flush".into());
        Ok(())
    }
}

fn descriptor(memory: &Memory, index: u16, address: u64, length: u32, flags: u16) {
    let mut raw = [0u8; 16];
    raw[..8].copy_from_slice(&address.to_le_bytes());
    raw[8..12].copy_from_slice(&length.to_le_bytes());
    raw[12..14].copy_from_slice(&flags.to_le_bytes());
    raw[14..].copy_from_slice(&(index + 1).to_le_bytes());
    memory
        .write_slice(&raw, GuestAddress(DESC + u64::from(index) * 16))
        .unwrap();
}

fn fixture(
    requests: &[(u32, usize)],
    fail: bool,
) -> (Backend, Vring, Arc<FixtureDisk>, Vec<GuestAddress>) {
    let memory = GuestMemoryAtomic::new(
        Memory::from_ranges(&[(GuestAddress(0), 32 * 1024 * 1024)]).unwrap(),
    );
    let view = memory.memory();
    let mut descriptor_index = 0u16;
    let mut payload = PAYLOAD;
    let mut statuses = Vec::new();
    let mut write_statuses = Vec::new();
    for (index, (kind, length)) in requests.iter().enumerate() {
        let header = HEADER + index as u64 * 32;
        let status = GuestAddress(header + 16);
        let mut raw = [0u8; 16];
        raw[..4].copy_from_slice(&kind.to_le_bytes());
        raw[8..].copy_from_slice(&(index as u64).to_le_bytes());
        view.write_slice(&raw, GuestAddress(header)).unwrap();
        view.write_obj(0xffu8, status).unwrap();
        view.write_obj(
            descriptor_index.to_le(),
            GuestAddress(AVAIL + 4 + index as u64 * 2),
        )
        .unwrap();
        descriptor(
            &view,
            descriptor_index,
            header,
            16,
            VRING_DESC_F_NEXT as u16,
        );
        descriptor_index += 1;
        if *length != 0 {
            let flags = VRING_DESC_F_NEXT
                | if *kind == VIRTIO_BLK_T_IN {
                    VRING_DESC_F_WRITE
                } else {
                    0
                };
            descriptor(
                &view,
                descriptor_index,
                payload,
                *length as u32,
                flags as u16,
            );
            view.write_slice(&vec![0x37; *length], GuestAddress(payload))
                .unwrap();
            descriptor_index += 1;
            payload += *length as u64;
        }
        descriptor(
            &view,
            descriptor_index,
            status.0,
            1,
            VRING_DESC_F_WRITE as u16,
        );
        descriptor_index += 1;
        statuses.push(status);
        if *kind == VIRTIO_BLK_T_OUT {
            write_statuses.push(status);
        }
    }
    view.write_obj((requests.len() as u16).to_le(), GuestAddress(AVAIL + 2))
        .unwrap();
    let vring = Vring::new(memory.clone(), 256).unwrap();
    vring.set_queue_info(DESC, AVAIL, USED).unwrap();
    vring.set_queue_size(256);
    vring.set_queue_ready(true);
    vring.set_enabled(true);
    let disk = Arc::new(FixtureDisk {
        memory: memory.clone(),
        statuses: write_statuses,
        events: Mutex::new(Vec::new()),
        fail,
        panic_write: false,
    });
    let backend = Backend {
        disk: disk.clone(),
        memory,
        kill: Arc::new(EventFd::new(libc::EFD_NONBLOCK).unwrap()),
        event_idx: false,
        failed: Arc::new(AtomicBool::new(false)),
    };
    (backend, vring, disk, statuses)
}

#[test]
fn adjacent_writes_share_a_barrier_without_crossing_read_or_flush() {
    let (backend, vring, disk, statuses) =
        fixture(&[(1, 512), (1, 512), (0, 512), (4, 0), (1, 512)], false);
    assert!(backend.process_queue(&vring).unwrap());
    assert_eq!(
        *disk.events.lock().unwrap(),
        ["write:2", "read", "flush", "write:1"]
    );
    let memory = backend.memory.memory();
    for status in statuses {
        assert_eq!(memory.read_obj::<u8>(status).unwrap(), 0);
    }
    assert_eq!(memory.read_obj::<u16>(GuestAddress(USED + 2)).unwrap(), 5);
    assert!(!backend.process_queue(&vring).unwrap());
}

#[test]
fn cumulative_payload_is_bounded_and_all_requests_complete() {
    let six_mib = 6 * 1024 * 1024;
    let (backend, vring, disk, statuses) = fixture(&[(1, six_mib), (1, six_mib), (1, 512)], false);
    backend.process_queue(&vring).unwrap();
    assert_eq!(*disk.events.lock().unwrap(), ["write:1", "write:2"]);
    for status in statuses {
        assert_eq!(backend.memory.memory().read_obj::<u8>(status).unwrap(), 0);
    }
}

#[test]
fn failed_barrier_returns_errors_to_every_write() {
    let (backend, vring, disk, statuses) = fixture(&[(1, 512), (1, 512)], true);
    backend.process_queue(&vring).unwrap();
    assert_eq!(*disk.events.lock().unwrap(), ["write:2"]);
    for status in statuses {
        assert_eq!(backend.memory.memory().read_obj::<u8>(status).unwrap(), 1);
    }
}

#[test]
fn invalid_guest_payload_stops_backend_before_disk_or_ack() {
    let (mut backend, vring, disk, statuses) = fixture(&[(1, 512)], false);
    descriptor(
        &backend.memory.memory(),
        1,
        u64::MAX - 15,
        512,
        VRING_DESC_F_NEXT as u16,
    );
    assert!(backend.handle_event(0, EventSet::IN, &[vring], 0).is_err());
    assert!(backend.failed.load(Ordering::Acquire));
    assert!(disk.events.lock().unwrap().is_empty());
    assert_eq!(
        backend.memory.memory().read_obj::<u8>(statuses[0]).unwrap(),
        0xff
    );
}

#[test]
fn closing_before_frontend_connects_joins_workers_and_removes_socket() {
    let (_, _, disk, _) = fixture(&[], false);
    let directory = tempfile::tempdir().unwrap();
    let socket = directory.path().join("disk.sock");
    let backend = mount_disk(disk, &socket, unsafe { libc::geteuid() }).unwrap();
    assert!(socket.exists());
    backend.close().unwrap();
    assert!(!socket.exists());
}

#[test]
fn queue_worker_panic_is_visible_without_acknowledging_the_write() {
    let (mut backend, vring, _, statuses) = fixture(&[(VIRTIO_BLK_T_OUT, 512)], false);
    backend.disk = Arc::new(FixtureDisk {
        memory: backend.memory.clone(),
        statuses: statuses.clone(),
        events: Mutex::new(Vec::new()),
        fail: false,
        panic_write: true,
    });
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        backend.handle_event(0, EventSet::IN, &[vring], 0)
    }));
    assert!(result.is_err());
    assert!(backend.failed.load(Ordering::Acquire));
    assert_eq!(
        backend.memory.memory().read_obj::<u8>(statuses[0]).unwrap(),
        0xff
    );
}

#[test]
fn shared_guest_accounting_counts_allocated_pages_once_and_observes_reclamation() {
    use std::os::unix::{fs::MetadataExt, io::AsRawFd};
    use vm_memory::FileOffset;
    let file = tempfile::tempfile().unwrap();
    file.set_len(8192).unwrap();
    let memory = Memory::from_ranges_with_files(&[
        (
            GuestAddress(0),
            4096,
            Some(FileOffset::new(file.try_clone().unwrap(), 0)),
        ),
        (
            GuestAddress(8192),
            4096,
            Some(FileOffset::new(file.try_clone().unwrap(), 4096)),
        ),
    ])
    .unwrap();
    assert_eq!(
        allocated_memory_bytes(&memory).unwrap(),
        0,
        "Sparse virtual memory is not committed RAM"
    );
    memory.write_obj(1u8, GuestAddress(0)).unwrap();
    memory.write_obj(2u8, GuestAddress(8192)).unwrap();
    let active = file.metadata().unwrap().blocks() * 512;
    assert!(active >= 8192);
    assert_eq!(
        allocated_memory_bytes(&memory).unwrap(),
        active,
        "Two mappings of one memfd must not double its cost"
    );
    assert_eq!(
        unsafe {
            libc::fallocate(
                file.as_raw_fd(),
                libc::FALLOC_FL_PUNCH_HOLE | libc::FALLOC_FL_KEEP_SIZE,
                0,
                8192,
            )
        },
        0
    );
    assert_eq!(
        allocated_memory_bytes(&memory).unwrap(),
        0,
        "Free page reporting returns allocated pages to the host"
    );
    assert!(
        allocated_memory_bytes(&Memory::new()).is_err(),
        "Unknown memory is never treated as free"
    );
}

#[test]
fn guest_reads_overlap_without_crossing_writes_or_flushes() {
    use std::sync::{Condvar, atomic::AtomicUsize};
    use std::time::{Duration, Instant};

    struct SlowDisk {
        arrived: Mutex<usize>,
        release: Condvar,
        failed: AtomicBool,
        active: AtomicUsize,
        peak: AtomicUsize,
    }
    impl Disk for SlowDisk {
        fn size(&self) -> u64 {
            32 * 1024 * 1024
        }

        fn read_at(&self, _: u64, bytes: &mut [u8]) -> io::Result<()> {
            let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            self.peak.fetch_max(active, Ordering::SeqCst);
            let mut arrived = self.arrived.lock().unwrap();
            *arrived += 1;
            let deadline = Instant::now() + Duration::from_secs(2);
            while *arrived < 16 && !self.failed.load(Ordering::SeqCst) {
                let remaining = deadline.saturating_duration_since(Instant::now());
                let (guard, timeout) = self.release.wait_timeout(arrived, remaining).unwrap();
                arrived = guard;
                if timeout.timed_out() {
                    self.failed.store(true, Ordering::SeqCst);
                    break;
                }
            }
            self.release.notify_all();
            drop(arrived);
            bytes.fill(0x5a);
            self.active.fetch_sub(1, Ordering::SeqCst);
            Ok(())
        }

        fn write_at(&self, _: u64, _: &[u8]) -> io::Result<()> {
            panic!("Unexpected write")
        }

        fn sync(&self) -> io::Result<()> {
            assert_eq!(
                self.active.load(Ordering::SeqCst),
                0,
                "flush crossed active reads"
            );
            assert_eq!(*self.arrived.lock().unwrap(), 17);
            Ok(())
        }
    }
    let mut requests = vec![(VIRTIO_BLK_T_IN, 512); 17];
    requests.push((VIRTIO_BLK_T_FLUSH, 0));
    let (mut backend, vring, _, statuses) = fixture(&requests, false);
    let disk = Arc::new(SlowDisk {
        arrived: Mutex::new(0),
        release: Condvar::new(),
        failed: AtomicBool::new(false),
        active: AtomicUsize::new(0),
        peak: AtomicUsize::new(0),
    });
    backend.disk = disk.clone();
    backend.process_queue(&vring).unwrap();
    assert!(
        !disk.failed.load(Ordering::SeqCst),
        "the guest queue serialized cold reads"
    );
    assert_eq!(disk.peak.load(Ordering::SeqCst), 16);
    for status in statuses {
        assert_eq!(backend.memory.memory().read_obj::<u8>(status).unwrap(), 0);
    }
}

#[test]
fn read_failures_are_per_request_and_panics_fail_the_queue_without_killing_workers() {
    struct FaultDisk {
        panic: bool,
    }
    impl Disk for FaultDisk {
        fn size(&self) -> u64 {
            32 * 1024 * 1024
        }

        fn read_at(&self, offset: u64, bytes: &mut [u8]) -> io::Result<()> {
            if offset == 0 {
                assert!(!self.panic, "Injected read worker panic");
                return Err(error("Injected read error"));
            }
            bytes.fill(0x5a);
            Ok(())
        }

        fn write_at(&self, _: u64, _: &[u8]) -> io::Result<()> {
            panic!("Unexpected write")
        }

        fn sync(&self) -> io::Result<()> {
            Ok(())
        }
    }
    for panic in [false, true, false] {
        let (mut backend, vring, _, statuses) = fixture(&[(VIRTIO_BLK_T_IN, 512); 2], false);
        backend.disk = Arc::new(FaultDisk { panic });
        let result = backend.handle_event(0, EventSet::IN, &[vring], 0);
        assert_eq!(result.is_err(), panic);
        assert_eq!(backend.failed.load(Ordering::Acquire), panic);
        let expected = if panic { [0xff, 0xff] } else { [1, 0] };
        for (status, expected) in statuses.into_iter().zip(expected) {
            assert_eq!(
                backend.memory.memory().read_obj::<u8>(status).unwrap(),
                expected
            );
        }
    }
}
