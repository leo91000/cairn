//! Kernel block devices with durable ownership independent of device ID or PID reuse.
//! A caller closes only after fencing its VMM. Drop stops I/O but leaves the
//! registration for verified startup cleanup; it never guesses that a VM died.
mod io;
mod namespace;
mod observation;

pub(crate) use namespace::BackendNamespace;

use super::Disk;
use libublk::{UblkError, UblkFlags, ctrl::UblkCtrlBuilder};
use serde::{Deserialize, Serialize};
use std::{
    ffi::CString,
    fs::{self, File},
    io::{self as stdio, Write},
    os::unix::{
        ffi::OsStrExt,
        fs::{DirBuilderExt, FileTypeExt, MetadataExt},
    },
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread::JoinHandle,
    time::Duration,
};

/// Initialize the runner before its runtime starts. Journal writes, capture and
/// cache maintenance can all hold locks needed by block I/O; every runtime
/// thread must inherit writeback progress, not just the ublk queue thread.
pub fn prepare_io_threads() -> std::io::Result<()> {
    io::enable_io_flusher()
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Record {
    version: u8,
    device_id: Option<i32>,
    token: u64,
    boot_id: String,
    vm_id: String,
    owner: Owner,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Owner {
    pid: u32,
    started: u64,
    namespace: u64,
}

fn process_started(pid: u32) -> stdio::Result<u64> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat"))?;
    // comm can contain spaces and parentheses. Field 22 follows the final
    // parenthesis, at index 19 counting the state as index zero.
    stat.rsplit_once(')')
        .and_then(|(_, fields)| fields.split_whitespace().nth(19))
        .ok_or_else(|| stdio::Error::other("Invalid ublk process identity"))?
        .parse()
        .map_err(stdio::Error::other)
}

impl Owner {
    fn current() -> stdio::Result<Self> {
        let pid = std::process::id();
        Ok(Self {
            pid,
            started: process_started(pid)?,
            namespace: fs::metadata("/proc/self/ns/pid")?.ino(),
        })
    }

    fn alive(&self) -> stdio::Result<bool> {
        if fs::metadata("/proc/self/ns/pid")?.ino() != self.namespace {
            // Startup holds the controller lock after fencing the preceding
            // container/PID namespace. Its private backend threads died with it.
            return Ok(false);
        }
        match process_started(self.pid) {
            Ok(started) => Ok(started == self.started),
            Err(error) if error.kind() == stdio::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error),
        }
    }
}

impl Record {
    fn validate(&self) -> stdio::Result<()> {
        if self.version != 1
            || self.device_id.is_some_and(|id| id < 0)
            || self.token == 0
            || self.owner.pid == 0
            || self.owner.started == 0
            || self.owner.namespace == 0
        {
            return Err(stdio::Error::other("Invalid ublk ownership record"));
        }
        for id in [&self.boot_id, &self.vm_id] {
            let canonical = uuid::Uuid::parse_str(id)
                .map_err(stdio::Error::other)?
                .to_string();
            if canonical != *id {
                return Err(stdio::Error::other("Invalid ublk ownership identity"));
            }
        }
        Ok(())
    }

    fn path(&self, state: &Path) -> PathBuf {
        let name = match self.device_id {
            Some(id) => format!("{id}.json"),
            None => format!("pending-{}-{}.json", self.vm_id, self.token),
        };
        state.join("ublk-devices").join(name)
    }

    fn device_id(&self) -> stdio::Result<i32> {
        self.device_id
            .ok_or_else(|| stdio::Error::other("ublk device is not yet registered"))
    }
}

enum Event {
    Added(Record),
    Ready,
    Failed(stdio::Error),
}

pub(super) fn error(error: UblkError) -> stdio::Error {
    match error {
        UblkError::IOError(error) => error,
        UblkError::OtherError(code) | UblkError::UringIOError(code) if code < 0 => {
            stdio::Error::from_raw_os_error(-code)
        }
        error => stdio::Error::other(error),
    }
}

fn boot_id() -> stdio::Result<String> {
    Ok(fs::read_to_string("/proc/sys/kernel/random/boot_id")?
        .trim()
        .to_owned())
}

fn persist(state: &Path, record: &Record) -> stdio::Result<()> {
    let directory = state.join("ublk-devices");
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&directory)?;
    let metadata = fs::symlink_metadata(&directory)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(stdio::Error::other("ublk registry must not be an alias"));
    }
    let mut file = tempfile::NamedTempFile::new_in(&directory)?;
    serde_json::to_writer(&mut file, record)?;
    file.flush()?;
    file.as_file().sync_all()?;
    file.persist_noclobber(record.path(state))
        .map_err(|error| error.error)?;
    File::open(directory)?.sync_all()
}

fn forget(state: &Path, record: &Record) -> stdio::Result<()> {
    let path = record.path(state);
    match fs::remove_file(&path) {
        Ok(()) => File::open(path.parent().unwrap())?.sync_all(),
        Err(error) if error.kind() == stdio::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn node(path: &Path, sysfs: &Path, character: bool, uid: Option<u32>) -> stdio::Result<()> {
    let address = fs::read_to_string(sysfs.join("dev"))?;
    let (major, minor) = address
        .trim()
        .split_once(':')
        .ok_or_else(|| stdio::Error::other("Invalid ublk device address"))?;
    let device = libc::makedev(
        major.parse().map_err(stdio::Error::other)?,
        minor.parse().map_err(stdio::Error::other)?,
    );
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            let kind = if character {
                metadata.file_type().is_char_device()
            } else {
                metadata.file_type().is_block_device()
            };
            if !kind || metadata.rdev() != device {
                return Err(stdio::Error::other("ublk device path is already occupied"));
            }
        }
        Err(error) if error.kind() == stdio::ErrorKind::NotFound => {
            let target = CString::new(path.as_os_str().as_bytes()).map_err(stdio::Error::other)?;
            let kind = if character {
                libc::S_IFCHR
            } else {
                libc::S_IFBLK
            };
            if unsafe { libc::mknod(target.as_ptr(), kind | 0o600, device) } != 0 {
                return Err(stdio::Error::last_os_error());
            }
        }
        Err(error) => return Err(error),
    }
    if let Some(uid) = uid {
        std::os::unix::fs::chown(path, Some(uid), Some(uid))?;
    }
    Ok(())
}

fn vm_alive(state: &Path, record: &Record) -> stdio::Result<bool> {
    let jail = state
        .join("jails/firecracker")
        .join(&record.vm_id)
        .join("root");
    for entry in fs::read_dir("/proc")? {
        let entry = entry?;
        if entry
            .file_name()
            .to_str()
            .is_none_or(|id| id.parse::<u32>().is_err())
        {
            continue;
        }
        match fs::read_link(entry.path().join("root")) {
            Ok(root) if root == jail => return Ok(true),
            Ok(_) => {}
            Err(error) if error.kind() == stdio::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Ok(false)
}

fn stop_owned(
    state: &Path,
    record: &Record,
    control: &libublk::ctrl::UblkCtrl,
    reaped: bool,
) -> stdio::Result<()> {
    // Normal close has awaited its VMM and joined its backend. Startup has
    // neither handle, so it must prove both fences before STOP/DEL. A scan of
    // unrelated live jails is unnecessary (and can need ptrace permission)
    // when the caller already reaped this exact VMM.
    if !reaped && (record.owner.alive()? || vm_alive(state, record)?) {
        return Err(stdio::Error::new(
            stdio::ErrorKind::WouldBlock,
            "ublk backend or VMM is still active",
        ));
    }
    if u32::from(control.dev_info().state) != libublk::sys::UBLK_S_DEV_DEAD {
        // A dead backend can leave the kernel device LIVE. Ownership and both
        // execution fences are proven above; STOP, then confirm before DEL.
        control.kill_dev().map_err(error)?;
        let confirmed = UblkCtrlBuilder::default()
            .id(i32::try_from(control.dev_info().dev_id).map_err(stdio::Error::other)?)
            .build()
            .map_err(error)?;
        if confirmed.dev_info().ublksrv_flags != record.token
            || u32::from(confirmed.dev_info().state) != libublk::sys::UBLK_S_DEV_DEAD
        {
            return Err(stdio::Error::other("ublk stop is not confirmed"));
        }
    }
    Ok(())
}

fn delete_native(
    asynchronous: impl FnOnce() -> stdio::Result<()>,
    synchronous: impl FnOnce() -> stdio::Result<()>,
) -> stdio::Result<()> {
    match asynchronous() {
        Ok(()) => Ok(()),
        Err(error)
            if matches!(
                error.raw_os_error(),
                Some(libc::EOPNOTSUPP | libc::ENOTTY | 524)
            ) =>
        {
            synchronous()
        }
        Err(error) => Err(error),
    }
}

fn retire_with_mode(
    state: &Path,
    record: &Record,
    control: &libublk::ctrl::UblkCtrl,
    reaped: bool,
    native: bool,
) -> stdio::Result<()> {
    stop_owned(state, record, control, reaped)?;
    if !native {
        return control.del_dev().map_err(error).map(|_| ());
    }
    let device_id = i32::try_from(control.dev_info().dev_id).map_err(stdio::Error::other)?;
    // Older kernels use synchronous deletion on this same isolated worker.
    delete_native(
        || control.del_dev_async().map_err(error).map(|_| ()),
        || control.del_dev().map_err(error).map(|_| ()),
    )?;
    // Async acknowledgement schedules teardown. Keep Cairn's receipt until a
    // subsequent control lookup proves absence or a different nonce.
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        match observation::native_control(device_id) {
            Ok(current) if current.dev_info().ublksrv_flags != record.token => return Ok(()),
            Ok(_) => {}
            Err(failure) => {
                if matches!(failure.raw_os_error(), Some(libc::ENOENT | libc::ENODEV)) {
                    return Ok(());
                }
                return Err(failure);
            }
        }
        if std::time::Instant::now() >= deadline {
            return Err(stdio::Error::new(
                stdio::ErrorKind::WouldBlock,
                "Native kernel device retirement is still pending",
            ));
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn cleanup(state: &Path, record: &Record, joined: bool) -> stdio::Result<()> {
    cleanup_with_mode(state, record, joined, false)
}

fn cleanup_with_mode(
    state: &Path,
    record: &Record,
    joined: bool,
    native: bool,
) -> stdio::Result<()> {
    record.validate()?;
    if record.boot_id != boot_id()? {
        return forget(state, record);
    }
    let Some(device_id) = record.device_id else {
        if record.owner.alive()? || vm_alive(state, record)? {
            return Err(stdio::Error::new(
                stdio::ErrorKind::WouldBlock,
                "ublk registration owner is still active",
            ));
        }
        // This durable intent predates ADD. Recover a crash before the assigned
        // ID was recorded by its immutable nonce; never STOP an unowned device.
        for entry in fs::read_dir("/sys/class/ublk-char")? {
            let entry = entry?;
            let name = entry.file_name();
            let Some(id) = name
                .to_str()
                .and_then(|name| name.strip_prefix("ublkc"))
                .and_then(|id| id.parse::<i32>().ok())
            else {
                continue;
            };
            let control = match UblkCtrlBuilder::default().id(id).build() {
                Ok(control) => control,
                Err(error) => {
                    let error = self::error(error);
                    if matches!(error.raw_os_error(), Some(libc::ENOENT | libc::ENODEV)) {
                        continue;
                    }
                    return Err(error);
                }
            };
            if control.dev_info().ublksrv_flags == record.token {
                retire_with_mode(state, record, &control, joined, native)?;
            }
        }
        return forget(state, record);
    };
    let opened = if native {
        observation::native_control(device_id)
    } else {
        UblkCtrlBuilder::default()
            .id(device_id)
            .build()
            .map_err(error)
    };
    let control = match opened {
        Ok(control) => control,
        Err(error) => {
            if matches!(error.raw_os_error(), Some(libc::ENOENT | libc::ENODEV)) {
                return forget(state, record);
            }
            return Err(error);
        }
    };
    let info = control.dev_info();
    if info.ublksrv_flags != record.token {
        // The kernel reused this numeric ID. The new device is not ours.
        return forget(state, record);
    }
    retire_with_mode(state, record, &control, joined, native)?;
    forget(state, record)
}

fn records(state: &Path) -> stdio::Result<Vec<Record>> {
    let directory = state.join("ublk-devices");
    let entries = match fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == stdio::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let mut records = Vec::new();
    for entry in entries {
        let entry = entry?;
        if entry
            .path()
            .extension()
            .is_none_or(|extension| extension != "json")
        {
            continue;
        }
        let metadata = fs::symlink_metadata(entry.path())?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return Err(stdio::Error::other("Invalid ublk registry entry"));
        }
        let file = File::open(entry.path())?;
        if file.metadata()?.len() > 4096 {
            return Err(stdio::Error::other("ublk ownership record exceeds limit"));
        }
        let record: Record = serde_json::from_reader(file)?;
        if record.path(state) != entry.path() {
            return Err(stdio::Error::other(
                "ublk registry identity does not match its path",
            ));
        }
        record.validate()?;
        records.push(record);
    }
    Ok(records)
}

/// Verify a native stale attachment before holder cleanup changes a process.
/// The durable nonce, boot and dead-owner checks also apply without a host mount.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HostDiskIdentity {
    Current,
    PreviousBoot,
    Absent,
}

pub(crate) fn verify_stale_host_disk(state: &Path, run: &str) -> stdio::Result<HostDiskIdentity> {
    let current_boot = boot_id()?;
    let records = records(state)?;
    let previous_boot = records.iter().any(|record| record.vm_id == run)
        && records
            .iter()
            .filter(|record| record.vm_id == run)
            .all(|record| record.boot_id != current_boot);
    if previous_boot {
        // A retained mount cannot survive a reboot. Its old dev_t may now name
        // an unrelated live disk; never inspect, stop or delete that new owner.
        return Ok(HostDiskIdentity::PreviousBoot);
    }
    let mut found = false;
    for record in records {
        if record.vm_id != run || record.boot_id != current_boot {
            continue;
        }
        let Some(id) = record.device_id else {
            continue;
        };
        if record.owner.alive()? || vm_alive(state, &record)? {
            return Err(stdio::Error::new(
                stdio::ErrorKind::WouldBlock,
                "Native backend remains active",
            ));
        }
        let control = match observation::native_control(id) {
            Ok(control) => control,
            Err(failure) => {
                if matches!(failure.raw_os_error(), Some(libc::ENOENT | libc::ENODEV)) {
                    continue;
                }
                return Err(failure);
            }
        };
        if control.dev_info().ublksrv_flags != record.token {
            continue;
        }
        let attachment =
            fs::symlink_metadata(state.join("native-block").join(run).join("root/disk.blk"))?;
        // STOP removes the gendisk before DEL removes the nonce-owned control
        // device. GET_PARAMS then reports a zero disk address; older receipts
        // retain that address only in the private block attachment.
        let mut parameters = libublk::sys::ublk_params {
            len: std::mem::size_of::<libublk::sys::ublk_params>() as u32,
            types: libublk::sys::UBLK_PARAM_TYPE_DEVT,
            ..Default::default()
        };
        control.get_params(&mut parameters).map_err(error)?;
        let address = attachment.rdev();
        let active_address = if parameters.devt.disk_major == 0 {
            match fs::metadata(format!(
                "/sys/dev/block/{}:{}",
                libc::major(address),
                libc::minor(address)
            )) {
                Ok(_) => true,
                Err(error) if error.kind() == stdio::ErrorKind::NotFound => false,
                Err(error) => return Err(error),
            }
        } else {
            true
        };
        let expected = retained_device_address(
            &parameters,
            control.dev_info().state,
            address,
            active_address,
        )?;
        if !attachment.file_type().is_block_device() || attachment.rdev() != expected {
            return Err(stdio::Error::other(
                "Native block attachment differs from its owned device",
            ));
        }
        found = true;
    }
    if !found {
        let path = state.join("native-block").join(run).join("root/disk.blk");
        let address = fs::symlink_metadata(path)?.rdev();
        let sysfs = format!(
            "/sys/dev/block/{}:{}",
            libc::major(address),
            libc::minor(address)
        );
        if Path::new(&sysfs).exists() {
            return Err(stdio::Error::other(
                "Native attachment device exists without a matching ownership receipt",
            ));
        }
    }
    Ok(if found {
        HostDiskIdentity::Current
    } else {
        HostDiskIdentity::Absent
    })
}

/// A root-overmounted namespace can hide a retained filesystem from mountinfo.
/// ext4 keeps this registration until put_super starts, independently of task roots.
/// Absence is not proof that journal destruction or kernel retirement has completed.
pub(crate) fn require_host_filesystems_unmounted(state: &Path, run: &str) -> stdio::Result<()> {
    let boot = boot_id()?;
    let ids = records(state)?
        .into_iter()
        .filter(|record| record.vm_id == run && record.boot_id == boot)
        .filter_map(|record| record.device_id)
        .collect::<Vec<_>>();
    let root = Path::new("/sys/fs/ext4");
    let path = CString::new(root.as_os_str().as_bytes()).map_err(stdio::Error::other)?;
    let mut status = std::mem::MaybeUninit::<libc::statfs>::uninit();
    if unsafe { libc::statfs(path.as_ptr(), status.as_mut_ptr()) } != 0 {
        return Err(stdio::Error::last_os_error());
    }
    if unsafe { status.assume_init() }.f_type != 0x6265_6572 {
        return Err(stdio::Error::other("Native ext4 inventory is not on sysfs"));
    }
    require_ext4_registrations_absent(root, &ids)
}

fn require_ext4_registrations_absent(root: &Path, ids: &[i32]) -> stdio::Result<()> {
    // Reading the authoritative directory must succeed even when no entry matches.
    // Without a current receipt, conservatively refuse any retained ublk filesystem;
    // a numeric ID inferred from an attachment alone would not prove ownership.
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(id) = name
            .to_str()
            .and_then(|name| name.strip_prefix("ublkb"))
            .and_then(|id| id.parse::<i32>().ok())
        else {
            continue;
        };
        if ids.is_empty() || ids.contains(&id) {
            return Err(stdio::Error::other(format!(
                "Native ext4 filesystem ublkb{id} remains registered after unmount; a hidden namespace may retain it; ownership retained"
            )));
        }
    }
    Ok(())
}

fn retained_device_address(
    parameters: &libublk::sys::ublk_params,
    state: u16,
    attachment: u64,
    active_address: bool,
) -> stdio::Result<u64> {
    if parameters.types & libublk::sys::UBLK_PARAM_TYPE_DEVT == 0 {
        return Err(stdio::Error::other("Native device address is unavailable"));
    }
    if parameters.devt.disk_major == 0 {
        if u32::from(state) != libublk::sys::UBLK_S_DEV_DEAD
            || active_address
            || libc::major(attachment) == 0
        {
            return Err(stdio::Error::other(
                "Stopped native device address cannot be verified",
            ));
        }
        return Ok(attachment);
    }
    Ok(libc::makedev(
        parameters.devt.disk_major,
        parameters.devt.disk_minor,
    ))
}

/// Native recovery owns both disk leases and has requested workload termination.
/// Abort only dead, nonce-verified backends so blocked consumers can finish exiting;
/// keep kernel devices and receipts until fencing and unmount are confirmed.
pub(crate) fn quiesce_stale_host_disk(state: &Path, run: &str) -> stdio::Result<()> {
    let current_boot = boot_id()?;
    let mut owned = false;
    for record in records(state)? {
        if record.vm_id != run || record.boot_id != current_boot {
            continue;
        }
        let Some(id) = record.device_id else { continue };
        if record.owner.alive()? || vm_alive(state, &record)? {
            return Err(stdio::Error::new(
                stdio::ErrorKind::WouldBlock,
                "ublk backend or VMM is still active",
            ));
        }
        owned = true;
        let control = match UblkCtrlBuilder::default().id(id).build() {
            Ok(control) => control,
            Err(failure) => {
                let failure = error(failure);
                if matches!(failure.raw_os_error(), Some(libc::ENOENT | libc::ENODEV)) {
                    continue;
                }
                return Err(failure);
            }
        };
        if control.dev_info().ublksrv_flags != record.token {
            return Err(stdio::Error::other("Native ublk ownership changed"));
        }
        stop_owned(state, &record, &control, false)?;
    }
    if !owned {
        return Err(stdio::Error::other(
            "Mounted native disk has no current ownership receipt",
        ));
    }
    Ok(())
}

/// Inventory without kernel control operations. Native recovery schedules each
/// run independently and keeps its durable quarantine until all receipts retire.
pub(crate) fn stale_runs(state: &Path) -> stdio::Result<Vec<String>> {
    let mut runs = records(state)?
        .into_iter()
        .map(|record| record.vm_id)
        .collect::<Vec<_>>();
    runs.sort();
    runs.dedup();
    Ok(runs)
}

pub(crate) fn cleanup_native_run(state: &Path, run: &str) -> stdio::Result<()> {
    let records = records(state)?
        .into_iter()
        .filter(|record| record.vm_id == run)
        .collect::<Vec<_>>();
    let boot = boot_id()?;
    if records.iter().any(|record| record.boot_id == boot) {
        require_host_filesystems_unmounted(state, run)?;
    }
    for record in records {
        cleanup_with_mode(state, &record, false, true)?;
    }
    Ok(())
}

/// Called only under the controller lock, after fencing consumers and unmounting
/// host filesystems. Never retires an unrecorded owner or a live backend/VMM.
pub fn cleanup_stale(state: &Path) -> stdio::Result<()> {
    for record in records(state)? {
        cleanup(state, &record, false)?;
    }
    Ok(())
}

pub struct MountedDisk {
    state: PathBuf,
    record: Record,
    stopped: Arc<AtomicBool>,
    failed: Arc<AtomicBool>,
    thread: Option<JoinHandle<stdio::Result<()>>>,
}

pub(crate) fn supports_growth() -> bool {
    libublk::ctrl::UblkCtrl::get_features().unwrap_or_default()
        & u64::from(libublk::sys::UBLK_F_UPDATE_SIZE)
        != 0
}

impl MountedDisk {
    /// Serialize with close/abort through the handle's lifecycle mutex. Some
    /// kernels cannot safely accept UPDATE_SIZE concurrently with STOP_DEV.
    pub(crate) fn grow(&mut self, size: u64) -> stdio::Result<()> {
        if self.stopped.load(Ordering::Acquire) || self.failed() || !supports_growth() {
            return Err(stdio::Error::other("Live ublk growth is unavailable"));
        }
        let control = UblkCtrlBuilder::default()
            .id(self.record.device_id()?)
            .build()
            .map_err(error)?;
        if control.dev_info().ublksrv_flags != self.record.token
            || self.record.boot_id != boot_id()?
        {
            return Err(stdio::Error::other("ublk ownership changed"));
        }
        control.update_size(size).map_err(error)?;
        Ok(())
    }

    pub fn failed(&self) -> bool {
        self.failed.load(Ordering::Acquire)
            || self.thread.as_ref().is_some_and(JoinHandle::is_finished)
    }

    fn stop(&self) -> stdio::Result<()> {
        self.stopped.store(true, Ordering::Release);
        let control = UblkCtrlBuilder::default()
            .id(self.record.device_id()?)
            .build()
            .map_err(error)?;
        if control.dev_info().ublksrv_flags != self.record.token
            || self.record.boot_id != boot_id()?
        {
            return Err(stdio::Error::other("ublk ownership changed"));
        }
        control.kill_dev().map_err(error)?;
        Ok(())
    }

    /// Abort only this nonce-verified device to release blocked I/O after a
    /// failed consumer fence. Registration stays owned until normal retirement.
    pub(crate) fn abort_io(&self) -> stdio::Result<()> {
        self.stop()
    }

    /// Caller has already awaited its VMM exit and cancelled blocked source reads.
    pub fn close(mut self) -> stdio::Result<()> {
        self.close_in_place()
    }

    /// The caller has fenced all consumers and unmounted any host filesystem.
    /// Retaining the handle on error permits safe retry of kernel retirement.
    pub(crate) fn close_in_place(&mut self) -> stdio::Result<()> {
        self.close_with_mode(false)
    }

    pub(crate) fn close_native(&mut self) -> stdio::Result<()> {
        self.close_with_mode(true)
    }

    fn close_with_mode(&mut self, native: bool) -> stdio::Result<()> {
        if self.thread.is_some() {
            self.stop()?;
            self.thread
                .take()
                .unwrap()
                .join()
                .map_err(|_| stdio::Error::other("ublk backend panicked"))??;
        }
        cleanup_with_mode(&self.state, &self.record, true, native)
    }
}

impl Drop for MountedDisk {
    fn drop(&mut self) {
        if self.thread.is_some() {
            // A cancelled future may not yet have reaped its VMM. STOP only;
            // persistent ownership survives until cleanup can prove both dead.
            if let Err(error) = self.stop() {
                tracing::warn!(%error, device_id = ?self.record.device_id, "Could not stop owned ublk device");
            }
        }
    }
}

/// Must run off the async executor. Readiness includes kernel START and the
/// jail's block node; an abandoned mount is stopped before any VMM can use it.
pub fn mount_disk(
    disk: Arc<dyn Disk>,
    state: &Path,
    jail: &Path,
    uid: u32,
) -> stdio::Result<MountedDisk> {
    mount(disk, state, jail, uid, None)
}

/// Host consumers must not share the backend's mount namespace: namespace exit
/// could otherwise flush a filesystem through the very backend that is exiting.
/// Every backend enters the clean startup namespace before adding its device;
/// it never copies an existing or concurrently closing host consumer.
pub(crate) fn mount_host_disk(
    disk: Arc<dyn Disk>,
    state: &Path,
    jail: &Path,
    namespace: &BackendNamespace,
    directory: &Path,
) -> stdio::Result<MountedDisk> {
    let entry = namespace.prepare(state, &[state, directory, &directory.join("lazy"), jail])?;
    mount(disk, state, jail, 0, Some(entry))
}

#[cfg(test)]
pub(crate) fn private_mount_namespace(inherited_mounts: &[PathBuf]) -> stdio::Result<()> {
    if unsafe { libc::unshare(libc::CLONE_NEWNS) } != 0 {
        return Err(stdio::Error::last_os_error());
    }
    if unsafe {
        libc::mount(
            std::ptr::null(),
            c"/".as_ptr(),
            std::ptr::null(),
            libc::MS_REC | libc::MS_PRIVATE,
            std::ptr::null(),
        )
    } != 0
    {
        return Err(stdio::Error::last_os_error());
    }
    // A new namespace also copies earlier consumers. They pin those devices
    // through DEL_DEV even after the controller unmounts its own view. Drop
    // only the caller's verified copies, after making propagation private.
    for path in inherited_mounts {
        let target = CString::new(path.as_os_str().as_bytes())?;
        if unsafe { libc::umount2(target.as_ptr(), 0) } != 0 {
            return Err(stdio::Error::last_os_error());
        }
    }
    Ok(())
}

fn mount(
    disk: Arc<dyn Disk>,
    state: &Path,
    jail: &Path,
    uid: u32,
    private_namespace: Option<namespace::Entry>,
) -> stdio::Result<MountedDisk> {
    if disk.size() == 0 || !disk.size().is_multiple_of(512) {
        return Err(stdio::Error::other("Invalid ublk disk size"));
    }
    let vm_id = jail
        .parent()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        .ok_or_else(|| stdio::Error::other("Invalid VM jail"))?
        .to_owned();
    let state = state.to_owned();
    let server_state = state.clone();
    let stopped = Arc::new(AtomicBool::new(false));
    let failed = Arc::new(AtomicBool::new(false));
    let server_stop = stopped.clone();
    let server_failed = failed.clone();
    let (sender, receiver) = mpsc::channel();
    let thread = std::thread::spawn(move || {
        let mut tracked = None;
        let mut pending = None;
        let mut registered = false;
        let result = (|| {
            // Fail before ADD without the required capability. All newly
            // created I/O threads inherit the validated flusher state.
            io::enable_io_flusher()?;
            if let Some(namespace) = private_namespace {
                namespace.enter()?;
            }
            let token = (uuid::Uuid::new_v4().as_u128() as u64).max(1);
            let mut record = Record {
                version: 1,
                device_id: None,
                token,
                boot_id: boot_id()?,
                vm_id,
                owner: Owner::current()?,
            };
            record.validate()?;
            persist(&server_state, &record)?;
            pending = Some(record.clone());
            let control = UblkCtrlBuilder::default()
                .name("cairn_journal")
                .ctrl_target_flags(token)
                .nr_queues(1)
                .depth(io::DEPTH)
                .io_buf_bytes(512 * 1024)
                .dev_flags(UblkFlags::UBLK_DEV_F_ADD_DEV)
                .build()
                .map_err(error)?;
            record.device_id =
                Some(i32::try_from(control.dev_info().dev_id).map_err(stdio::Error::other)?);
            record.validate()?;
            persist(&server_state, &record)?;
            tracked = Some(record.clone());
            forget(&server_state, pending.as_ref().unwrap())?;
            pending = None;
            node(
                Path::new(&control.get_cdev_path()),
                &PathBuf::from(format!("/sys/class/ublk-char/ublkc{}", record.device_id()?)),
                true,
                None,
            )?;
            if server_stop.load(Ordering::Acquire)
                || sender.send(Event::Added(record.clone())).is_err()
            {
                control.del_dev().map_err(error)?;
                forget(&server_state, &record)?;
                return Ok(());
            }
            // All subsequent removals use the durable kernel token and verify
            // VMM death. The library's implicit ADD-owner deletion cannot do so.
            control.disown();
            registered = true;
            io::serve(disk, control, sender.clone(), server_stop.clone())
        })();
        if !registered && let Some(record) = tracked {
            // The ADD guard has already removed a failed pre-start device.
            // Forget its receipt now so an in-process retry can reuse the ID.
            if let Err(error) = cleanup(&server_state, &record, true) {
                tracing::warn!(%error, "Could not retire failed ublk registration");
            }
        }
        if let Some(record) = pending
            && let Err(error) = forget(&server_state, &record)
        {
            tracing::warn!(%error, "Could not retire failed ublk registration intent");
        }
        if !server_stop.load(Ordering::Acquire) {
            server_failed.store(true, Ordering::Release);
        }
        if let Err(error) = &result {
            let _ = sender.send(Event::Failed(stdio::Error::new(
                error.kind(),
                error.to_string(),
            )));
        }
        result
    });
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    let added = receiver.recv_timeout(Duration::from_secs(30));
    let record = match added {
        Ok(Event::Added(record)) => record,
        Ok(Event::Failed(error)) => return Err(error),
        _ => {
            stopped.store(true, Ordering::Release);
            return Err(stdio::Error::other("ublk registration did not complete"));
        }
    };
    let mounted = MountedDisk {
        state,
        record,
        stopped,
        failed,
        thread: Some(thread),
    };
    let result = (|| {
        match receiver.recv_timeout(deadline.saturating_duration_since(std::time::Instant::now())) {
            Ok(Event::Ready) => {}
            Ok(Event::Failed(error)) => return Err(error),
            _ => return Err(stdio::Error::other("ublk startup did not complete")),
        }
        node(
            &jail.join("disk.blk"),
            &PathBuf::from(format!(
                "/sys/class/block/ublkb{}",
                mounted.record.device_id()?
            )),
            false,
            Some(uid),
        )
    })();
    if let Err(error) = result {
        let _ = mounted.close();
        return Err(error);
    }
    Ok(mounted)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retained_ext4_registration_refuses_retirement_even_without_visible_holders() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("features")).unwrap();
        require_ext4_registrations_absent(root.path(), &[7]).unwrap();
        fs::create_dir(root.path().join("ublkb7")).unwrap();
        assert!(require_ext4_registrations_absent(root.path(), &[7]).is_err());
        require_ext4_registrations_absent(root.path(), &[8]).unwrap();
        assert!(require_ext4_registrations_absent(root.path(), &[]).is_err());
        fs::remove_dir(root.path().join("ublkb7")).unwrap();
        require_ext4_registrations_absent(root.path(), &[7]).unwrap();
        assert!(require_ext4_registrations_absent(&root.path().join("missing"), &[7]).is_err());
    }

    #[test]
    fn stopped_device_identity_does_not_require_a_live_sysfs_block_entry() {
        let mut parameters = libublk::sys::ublk_params::default();
        let attachment = libc::makedev(259, 17);
        let dead = libublk::sys::UBLK_S_DEV_DEAD as u16;
        assert!(retained_device_address(&parameters, dead, attachment, false).is_err());
        parameters.types = libublk::sys::UBLK_PARAM_TYPE_DEVT;
        assert_eq!(
            retained_device_address(&parameters, dead, attachment, false).unwrap(),
            attachment
        );
        assert!(retained_device_address(&parameters, dead, attachment, true).is_err());
        assert!(
            retained_device_address(
                &parameters,
                libublk::sys::UBLK_S_DEV_LIVE as u16,
                attachment,
                false
            )
            .is_err()
        );
        parameters.devt.disk_major = 259;
        parameters.devt.disk_minor = 17;
        assert_eq!(
            retained_device_address(&parameters, dead, attachment, true).unwrap(),
            attachment
        );
        parameters.types = 0;
        assert!(retained_device_address(&parameters, dead, attachment, false).is_err());
    }

    #[test]
    fn stale_native_attachment_without_a_receipt_has_no_owned_device() {
        let root = tempfile::tempdir().unwrap();
        let run = uuid::Uuid::new_v4().to_string();
        let attachment = root.path().join("native-block").join(&run).join("root");
        fs::create_dir_all(&attachment).unwrap();
        fs::write(attachment.join("disk.blk"), b"obsolete attachment metadata").unwrap();
        assert_eq!(
            verify_stale_host_disk(root.path(), &run).unwrap(),
            HostDiskIdentity::Absent
        );
    }

    #[test]
    #[ignore = "requires CAP_SYS_ADMIN; uses disposable private mount namespaces"]
    fn private_backend_namespace_releases_only_selected_inherited_mounts() {
        std::thread::spawn(|| {
            private_mount_namespace(&[]).unwrap();
            let root = tempfile::tempdir().unwrap();
            let first = root.path().join("first");
            let unrelated = root.path().join("unrelated");
            for path in [&first, &unrelated] {
                fs::create_dir(path).unwrap();
                let target = CString::new(path.as_os_str().as_bytes()).unwrap();
                assert_eq!(
                    unsafe {
                        libc::mount(
                            c"tmpfs".as_ptr(),
                            target.as_ptr(),
                            c"tmpfs".as_ptr(),
                            0,
                            std::ptr::null(),
                        )
                    },
                    0
                );
                fs::write(path.join("sentinel"), b"retained in parent").unwrap();
            }
            let (copied, retained) = (first.clone(), unrelated.clone());
            let child = std::thread::spawn(move || {
                private_mount_namespace(std::slice::from_ref(&copied)).unwrap();
                assert!(
                    !copied.join("sentinel").exists(),
                    "backend still pins an earlier filesystem"
                );
                assert!(
                    retained.join("sentinel").exists(),
                    "unrelated mount was removed"
                );
            })
            .join();
            for path in [&first, &unrelated] {
                assert!(path.join("sentinel").exists(), "parent mount was changed");
                let target = CString::new(path.as_os_str().as_bytes()).unwrap();
                assert_eq!(unsafe { libc::umount2(target.as_ptr(), 0) }, 0);
            }
            child.unwrap();
        })
        .join()
        .unwrap();
    }

    #[test]
    fn owner_identity_rejects_pid_reuse_and_pending_intent_stays_durable() {
        let root = tempfile::tempdir().unwrap();
        let mut owner = Owner::current().unwrap();
        assert!(owner.alive().unwrap());
        owner.started = owner.started.wrapping_add(1);
        assert!(!owner.alive().unwrap());
        let record = Record {
            version: 1,
            device_id: None,
            token: u64::MAX,
            boot_id: boot_id().unwrap(),
            vm_id: uuid::Uuid::new_v4().to_string(),
            owner: Owner::current().unwrap(),
        };
        persist(root.path(), &record).unwrap();
        assert_eq!(
            cleanup_stale(root.path()).unwrap_err().kind(),
            stdio::ErrorKind::WouldBlock
        );
        assert!(record.path(root.path()).exists());
    }

    #[test]
    fn durable_registry_refuses_overwrite_and_mismatched_paths() {
        let root = tempfile::tempdir().unwrap();
        let record = Record {
            version: 1,
            device_id: Some(123),
            token: u64::MAX,
            boot_id: boot_id().unwrap(),
            vm_id: uuid::Uuid::new_v4().to_string(),
            owner: Owner::current().unwrap(),
        };
        persist(root.path(), &record).unwrap();
        let bytes = fs::read(record.path(root.path())).unwrap();
        let mut other = record.clone();
        other.token = 1;
        assert!(persist(root.path(), &other).is_err());
        assert_eq!(fs::read(record.path(root.path())).unwrap(), bytes);
        let restored: Record = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(restored.token, u64::MAX);
        fs::rename(
            record.path(root.path()),
            root.path().join("ublk-devices/456.json"),
        )
        .unwrap();
        assert!(
            cleanup_stale(root.path())
                .unwrap_err()
                .to_string()
                .contains("path")
        );
    }

    #[test]
    fn a_previous_boot_forgets_only_its_record_without_kernel_mutation() {
        let root = tempfile::tempdir().unwrap();
        let record = Record {
            version: 1,
            device_id: Some(i32::MAX),
            token: 123,
            boot_id: uuid::Uuid::new_v4().to_string(),
            vm_id: uuid::Uuid::new_v4().to_string(),
            owner: Owner::current().unwrap(),
        };
        persist(root.path(), &record).unwrap();
        // No attachment or device lookup is needed for a positively older boot.
        assert_eq!(
            verify_stale_host_disk(root.path(), &record.vm_id).unwrap(),
            HostDiskIdentity::PreviousBoot
        );
        cleanup_stale(root.path()).unwrap();
        assert!(!record.path(root.path()).exists());
    }
}

#[cfg(test)]
mod native_delete_tests {
    use super::*;

    #[test]
    fn old_kernels_fall_back_only_for_explicit_unsupported_commands() {
        use std::cell::Cell;
        for code in [libc::EOPNOTSUPP, libc::ENOTTY, 524, libc::EIO, libc::EPERM] {
            let fallback = Cell::new(false);
            let result = delete_native(
                || Err(stdio::Error::from_raw_os_error(code)),
                || {
                    fallback.set(true);
                    Ok(())
                },
            );
            let unsupported = matches!(code, libc::EOPNOTSUPP | libc::ENOTTY | 524);
            assert_eq!(fallback.get(), unsupported);
            assert_eq!(result.is_ok(), unsupported);
        }
    }
}
