//! Reclaim paused, live VMM pages without resetting devices or agent streams.
//!
//! Firecracker snapshots reset vsock and do not preserve active nested KVM.
//! Linux paging keeps the original process, KVM state and transports alive.
//! This module never discards pages, kills a VMM or claims a RAM reservation:
//! the controller must still verify host and cgroup headroom before granting it.

use super::{suspension, vm::AttemptRecord};
use crate::error::{Error, Result};
use serde::Serialize;
use std::{
    fs, io,
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd},
        unix::fs::{FileTypeExt, MetadataExt},
    },
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use tokio_util::sync::CancellationToken;

const MAX_RECLAIM_TIME: Duration = Duration::from_secs(20);
const ADVICE_CHUNK: u64 = 128 * 1_048_576;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReclaimReport {
    pub vm_id: String,
    pub resident_before_bytes: u64,
    pub resident_after_bytes: u64,
    pub swap_before_bytes: u64,
    pub swap_after_bytes: u64,
    pub advised_bytes: u64,
    pub reclaimed_resident_bytes: u64,
    pub elapsed_ms: u64,
}

/// The host ceiling complements the controller's cgroup ceiling. In particular,
/// zram can reduce a cgroup's resident usage without freeing equally much RAM.
pub fn available_host_memory_mib() -> Result<u64> {
    let memory = fs::read_to_string("/proc/meminfo")?;
    Ok(kib_field(&memory, "MemAvailable:")? / 1_048_576)
}

/// Hold the administrative reason and physical VM ownership until reclamation
/// finishes. A checkpoint or a concurrent release cannot resume it mid-probe.
pub async fn reclaim_attempt(
    state: &Path,
    attempt: &str,
    reason: &suspension::Reason,
    target_bytes: u64,
    stop: &CancellationToken,
) -> Result<ReclaimReport> {
    if target_bytes == 0 {
        return Err(Error::bad("Memory reclamation requires a positive target."));
    }
    let paused = suspension::hold_paused(state, attempt, reason).await?;
    let record: AttemptRecord =
        serde_json::from_slice(&tokio::fs::read(state.join(format!("{attempt}.vm.json"))).await?)?;
    crate::validation::uuid(&record.vm_id)?;
    let jail = state
        .join("jails/firecracker")
        .join(&record.vm_id)
        .join("root");
    let vm_id = record.vm_id;
    let stop = stop.clone();
    // Cancelling the caller must not release the paused ownership while the
    // kernel is still working. The blocking operation owns that guard itself.
    tokio::task::spawn_blocking(move || {
        let _paused = paused;
        reclaim(&jail, vm_id, target_bytes, &stop)
    })
    .await
    .map_err(Error::internal)?
}

fn kib_field(text: &str, field: &str) -> Result<u64> {
    text.lines()
        .find_map(|line| line.strip_prefix(field))
        .and_then(|value| value.split_whitespace().next())
        .and_then(|value| value.parse::<u64>().ok())
        .and_then(|value| value.checked_mul(1024))
        .ok_or_else(|| Error::unavailable("VM memory accounting is unavailable."))
}

#[derive(Debug, PartialEq, Eq)]
struct Swap {
    path: PathBuf,
    free_bytes: u64,
    partition: bool,
}

fn parse_swaps(text: &str) -> Result<Vec<Swap>> {
    let mut lines = text.lines();
    if !lines.next().is_some_and(|line| {
        line.split_whitespace().collect::<Vec<_>>()
            == ["Filename", "Type", "Size", "Used", "Priority"]
    }) {
        return Err(Error::unavailable("Node swap accounting is unavailable."));
    }
    let mut swaps = Vec::new();
    for line in lines.filter(|line| !line.trim().is_empty()) {
        let fields = line.split_whitespace().collect::<Vec<_>>();
        if fields.len() != 5 || !matches!(fields[1], "partition" | "file") {
            return Err(Error::unavailable("Node swap accounting is invalid."));
        }
        let size = fields[2]
            .parse::<u64>()
            .map_err(|_| Error::unavailable("Node swap size is invalid."))?;
        let used = fields[3]
            .parse::<u64>()
            .map_err(|_| Error::unavailable("Node swap usage is invalid."))?;
        let free_bytes = size
            .checked_sub(used)
            .and_then(|bytes| bytes.checked_mul(1024))
            .ok_or_else(|| Error::unavailable("Node swap accounting is invalid."))?;
        swaps.push(Swap {
            path: PathBuf::from(fields[0]),
            free_bytes,
            partition: fields[1] == "partition",
        });
    }
    Ok(swaps)
}

fn swap_device(path: &Path) -> Result<Option<PathBuf>> {
    match fs::metadata(path) {
        Ok(metadata) if metadata.file_type().is_block_device() => {
            let device = metadata.rdev();
            return Ok(Some(PathBuf::from(format!(
                "/sys/dev/block/{}:{}",
                libc::major(device),
                libc::minor(device)
            ))));
        }
        Ok(_) => return Ok(None),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    // The controller deliberately has no access to host swap devices. Resolve
    // kernel-published block identity through read-only sysfs instead of adding
    // those devices to the container or relaxing its device permissions.
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return Ok(None);
    };
    if path.parent() == Some(Path::new("/dev")) {
        let sys = PathBuf::from("/sys/class/block").join(name);
        return Ok(sys.exists().then_some(sys));
    }
    if path.parent() == Some(Path::new("/dev/mapper")) {
        for entry in fs::read_dir("/sys/class/block")? {
            let entry = entry?;
            if fs::read_to_string(entry.path().join("dm/name"))
                .is_ok_and(|device_name| device_name.trim() == name)
            {
                return Ok(Some(entry.path()));
            }
        }
    }
    Ok(None)
}

fn private_swap(swap: &Swap) -> Result<bool> {
    if !swap.partition {
        // A regular swap file may live on plaintext or an unrelated mount.
        // Do not move conversation credentials there without qualification.
        return Ok(false);
    }
    let Some(sys) = swap_device(&swap.path)? else {
        return Ok(false);
    };
    let name = fs::canonicalize(&sys)?;
    if name
        .file_name()
        .and_then(|value| value.to_str())
        .is_some_and(|value| {
            value.strip_prefix("zram").is_some_and(|suffix| {
                !suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_digit())
            })
        })
    {
        // zram stays in host memory. Its actual gain is measured independently
        // through MemAvailable, never inferred from swap size or compression.
        // Optional zram writeback can store decompressed pages on plaintext
        // disk. Only a device with no writeback backing remains qualified.
        return zram_without_writeback(&name);
    }
    let uuid = fs::read_to_string(sys.join("dm/uuid")).unwrap_or_default();
    Ok(uuid.starts_with("CRYPT-LUKS") || uuid.starts_with("CRYPT-PLAIN-"))
}

fn zram_without_writeback(device: &Path) -> Result<bool> {
    match fs::read_to_string(device.join("backing_dev")) {
        Ok(value) => Ok(value.trim() == "none"),
        // Kernels without CONFIG_ZRAM_WRITEBACK have no backing_dev attribute.
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(true),
        Err(error) => Err(error.into()),
    }
}

fn qualify_swap() -> Result<()> {
    let swaps = parse_swaps(&fs::read_to_string("/proc/swaps")?)?;
    if swaps.is_empty() || swaps.iter().all(|swap| swap.free_bytes == 0) {
        return Err(Error::unavailable(
            "Memory reclamation requires available compressed or encrypted swap.",
        ));
    }
    // Linux can choose any enabled swap area. Every one must preserve privacy,
    // including lower-priority areas reached once the primary one fills up.
    for swap in swaps {
        if !private_swap(&swap)? {
            return Err(Error::unavailable(
                "Memory reclamation refuses unqualified plaintext swap.",
            ));
        }
    }
    Ok(())
}

fn find_vmm(jail: &Path) -> Result<u32> {
    let expected = fs::metadata(jail)?;
    let mut found = None;
    for entry in fs::read_dir("/proc")? {
        let entry = entry?;
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<u32>() else {
            continue;
        };
        let process = entry.path();
        let Ok(root) = fs::metadata(process.join("root")) else {
            continue;
        };
        if root.dev() != expected.dev() || root.ino() != expected.ino() {
            continue;
        }
        let executable = fs::read_link(process.join("exe"))?;
        let command = fs::read_to_string(process.join("comm"))?;
        if executable.file_name().and_then(|name| name.to_str()) != Some("firecracker")
            || command.trim() != "firecracker"
            || found.replace(pid).is_some()
        {
            return Err(Error::conflict("Paused VM process ownership is ambiguous."));
        }
    }
    found.ok_or_else(|| Error::unavailable("Paused VM process is unavailable."))
}

fn pidfd(pid: u32) -> Result<OwnedFd> {
    let descriptor = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
    if descriptor < 0 {
        return Err(Error::unavailable(
            "The node cannot pin a live VM process for memory reclamation.",
        ));
    }
    Ok(unsafe { OwnedFd::from_raw_fd(descriptor as i32) })
}

#[derive(Debug, PartialEq, Eq)]
struct Mapping {
    start: u64,
    length: u64,
    resident: u64,
}

fn mappings(smaps: &str) -> Result<Vec<Mapping>> {
    let mut result = Vec::new();
    let mut current: Option<Mapping> = None;
    let mut eligible = false;
    let finish = |current: &mut Option<Mapping>, eligible, result: &mut Vec<Mapping>| {
        if let Some(mapping) = current.take()
            && eligible
            && mapping.resident > 0
        {
            result.push(mapping);
        }
    };
    for line in smaps.lines() {
        let fields = line.split_whitespace().collect::<Vec<_>>();
        if fields.len() >= 5 && fields[1].len() == 4 && fields[0].contains('-') {
            finish(&mut current, eligible, &mut result);
            let (start, end) = fields[0]
                .split_once('-')
                .ok_or_else(|| Error::unavailable("VM memory mappings are invalid."))?;
            let start = u64::from_str_radix(start, 16)
                .map_err(|_| Error::unavailable("VM memory mappings are invalid."))?;
            let end = u64::from_str_radix(end, 16)
                .map_err(|_| Error::unavailable("VM memory mappings are invalid."))?;
            let length = end
                .checked_sub(start)
                .filter(|length| *length > 0)
                .ok_or_else(|| Error::unavailable("VM memory mappings are invalid."))?;
            let name = fields.get(5).copied().unwrap_or("");
            let memory_backing = name.is_empty()
                || name.starts_with('[')
                || name.starts_with("/memfd:")
                || name.ends_with("/snapshot.mem");
            eligible = fields[1].starts_with("rw") && memory_backing;
            current = Some(Mapping {
                start,
                length,
                resident: 0,
            });
        } else if line.starts_with("Rss:") {
            if let Some(mapping) = &mut current {
                mapping.resident = kib_field(line, "Rss:")?;
            }
        } else if let Some(flags) = line.strip_prefix("VmFlags:")
            && flags
                .split_whitespace()
                .any(|flag| matches!(flag, "lo" | "ht" | "io" | "pf"))
        {
            eligible = false;
        }
    }
    finish(&mut current, eligible, &mut result);
    // Guest RAM dominates residency. Evict it before small VMM control pages.
    result.sort_unstable_by_key(|mapping| std::cmp::Reverse(mapping.resident));
    Ok(result)
}

fn advise(descriptor: &OwnedFd, start: u64, length: u64) -> Result<u64> {
    let address = usize::try_from(start)
        .map_err(|_| Error::unavailable("VM mapping address is unsupported."))?;
    let length = usize::try_from(length)
        .map_err(|_| Error::unavailable("VM mapping length is unsupported."))?;
    let range = libc::iovec {
        iov_base: address as *mut libc::c_void,
        iov_len: length,
    };
    let bytes = unsafe {
        libc::syscall(
            libc::SYS_process_madvise,
            descriptor.as_raw_fd(),
            &raw const range,
            1usize,
            libc::MADV_PAGEOUT,
            0u32,
        )
    };
    if bytes < 0 {
        let error = io::Error::last_os_error();
        return Err(Error::unavailable(match error.raw_os_error() {
            Some(libc::ENOSYS) => "The node kernel does not support live VM memory reclamation.",
            Some(libc::EPERM | libc::EACCES) => {
                "The node requires SYS_PTRACE and SYS_NICE to reclaim paused VM memory."
            }
            _ => "Paused VM memory reclamation failed; its process has been preserved.",
        }));
    }
    Ok(bytes as u64)
}

fn reclaim(
    jail: &Path,
    vm_id: String,
    target_bytes: u64,
    stop: &CancellationToken,
) -> Result<ReclaimReport> {
    qualify_swap()?;
    let pid = find_vmm(jail)?;
    let descriptor = pidfd(pid)?;
    // Revalidate after pinning: PID reuse between discovery and pidfd_open must
    // never let a reclamation target another conversation.
    if find_vmm(jail)? != pid {
        return Err(Error::conflict("Paused VM process ownership changed."));
    }
    let status_path = PathBuf::from(format!("/proc/{pid}/status"));
    let status = fs::read_to_string(&status_path)?;
    let resident_before_bytes = kib_field(&status, "VmRSS:")?;
    let swap_before_bytes = kib_field(&status, "VmSwap:")?;
    let memory = mappings(&fs::read_to_string(format!("/proc/{pid}/smaps"))?)?;
    let started = Instant::now();
    let mut advised_bytes = 0;
    let mut resident_after_bytes = resident_before_bytes;
    for mapping in memory {
        let mut position = mapping.start;
        let end = mapping.start + mapping.length;
        while position < end {
            if stop.is_cancelled() {
                return Err(Error::conflict(
                    "Paused VM memory reclamation was cancelled.",
                ));
            }
            if started.elapsed() >= MAX_RECLAIM_TIME
                || resident_before_bytes.saturating_sub(resident_after_bytes) >= target_bytes
            {
                break;
            }
            let length = (end - position).min(ADVICE_CHUNK);
            let advised = advise(&descriptor, position, length)?;
            if advised == 0 {
                return Err(Error::unavailable(
                    "Paused VM memory advice made no progress.",
                ));
            }
            advised_bytes += advised;
            position += advised;
            resident_after_bytes = kib_field(&fs::read_to_string(&status_path)?, "VmRSS:")?;
        }
        if started.elapsed() >= MAX_RECLAIM_TIME
            || resident_before_bytes.saturating_sub(resident_after_bytes) >= target_bytes
        {
            break;
        }
    }
    let status = fs::read_to_string(status_path)?;
    let resident_after_bytes = kib_field(&status, "VmRSS:")?;
    let swap_after_bytes = kib_field(&status, "VmSwap:")?;
    Ok(ReclaimReport {
        vm_id,
        resident_before_bytes,
        resident_after_bytes,
        swap_before_bytes,
        swap_after_bytes,
        advised_bytes,
        reclaimed_resident_bytes: resident_before_bytes.saturating_sub(resident_after_bytes),
        elapsed_ms: started.elapsed().as_millis() as u64,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn swap_accounting_never_treats_capacity_as_actual_reclaimed_memory() {
        let areas = parse_swaps(
            "Filename\tType\tSize\tUsed\tPriority\n/dev/zram0 partition 8192 2048 100\n/dev/dm-0 partition 16384 0 -1\n",
        )
        .unwrap();
        assert_eq!(areas[0].free_bytes, 6144 * 1024);
        assert_eq!(areas[1].free_bytes, 16384 * 1024);
        for invalid in [
            "",
            "bad header\n",
            "Filename Type Size Used Priority\n/dev/zram0 partition 1 2 100\n",
            "Filename Type Size Used Priority\n/dev/zram0 partition -1 0 100\n",
        ] {
            assert!(parse_swaps(invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn mapping_selection_keeps_devices_locked_pages_and_executable_code_untouched() {
        let smaps = concat!(
            "1000-9000 rw-p 00000000 00:00 0\nRss: 32 kB\nVmFlags: rd wr mr mw me ac sd\n",
            "a000-b000 rw-s 00000000 00:01 123 /memfd:guest-ram (deleted)\nRss: 4 kB\nVmFlags: rd wr sh mr mw me ms sd\n",
            "c000-d000 rw-p 00000000 00:00 0 [heap]\nRss: 4 kB\nVmFlags: rd wr lo\n",
            "e000-f000 rw-p 00000000 00:01 12 /snapshot.mem\nRss: 4 kB\nVmFlags: rd wr mr mw\n",
            "10000-11000 r-xp 00000000 00:01 13 /firecracker\nRss: 4 kB\nVmFlags: rd ex mr mw\n",
            "12000-13000 rw-s 00000000 00:01 14 /disk.blk\nRss: 4 kB\nVmFlags: rd wr sh\n",
            "14000-15000 rw-p 00000000 00:00 0\nRss: 4 kB\nVmFlags: rd wr pf\n",
            "16000-17000 rw-p 00000000 00:00 0\nRss: 0 kB\nVmFlags: rd wr\n",
        );
        assert_eq!(
            mappings(smaps).unwrap(),
            [
                Mapping {
                    start: 0x1000,
                    length: 0x8000,
                    resident: 32768
                },
                Mapping {
                    start: 0xa000,
                    length: 4096,
                    resident: 4096
                },
                Mapping {
                    start: 0xe000,
                    length: 4096,
                    resident: 4096
                },
            ]
        );
        assert!(mappings("9000-1000 rw-p 00000000 00:00 0\nRss: 4 kB\n").is_err());
    }

    #[test]
    fn memory_readings_require_real_fields_without_overflow() {
        assert_eq!(
            kib_field("VmRSS:\t32 kB\nVmSwap:\t4 kB", "VmSwap:").unwrap(),
            4096
        );
        assert!(kib_field("VmRSS: missing", "VmRSS:").is_err());
        assert!(kib_field("VmRSS: 18446744073709551615 kB", "VmRSS:").is_err());
    }

    #[test]
    fn regular_swap_files_are_not_qualified_by_their_name_or_private_permissions() {
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("encrypted-swap");
        fs::write(&file, b"not an encrypted swap device").unwrap();
        assert!(
            !private_swap(&Swap {
                path: file,
                free_bytes: 4096,
                partition: false,
            })
            .unwrap()
        );
    }

    #[test]
    fn zram_with_disk_writeback_is_not_treated_as_private_host_memory() {
        let root = tempfile::tempdir().unwrap();
        assert!(zram_without_writeback(root.path()).unwrap());
        fs::write(root.path().join("backing_dev"), "none\n").unwrap();
        assert!(zram_without_writeback(root.path()).unwrap());
        fs::write(root.path().join("backing_dev"), "/dev/sda2\n").unwrap();
        assert!(!zram_without_writeback(root.path()).unwrap());
    }
}
