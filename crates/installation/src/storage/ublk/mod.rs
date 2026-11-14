//! Kernel block devices with durable ownership independent of device ID or PID reuse.
//! A caller closes only after fencing its VMM. Drop stops I/O but leaves the
//! registration for verified startup cleanup; it never guesses that a VM died.
mod io;
mod namespace;

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

pub(crate) fn prepare_native_io_threads() -> std::io::Result<()> {
    let release = fs::read_to_string("/proc/sys/kernel/osrelease")?;
    if !async_retirement_kernel(&release) {
        return Err(stdio::Error::other(
            "Native ublk hosts require Linux 6.11 or newer for nonblocking device retirement",
        ));
    }
    io::enable_io_flusher()
}

fn async_retirement_kernel(release: &str) -> bool {
    let mut parts = release.trim().split('.');
    let major = parts.next().and_then(|part| part.parse::<u32>().ok());
    let minor = parts.next().and_then(|part| part.parse::<u32>().ok());
    matches!((major, minor), (Some(major), Some(minor)) if major > 6 || major == 6 && minor >= 11)
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

fn retire(
    state: &Path,
    record: &Record,
    control: &libublk::ctrl::UblkCtrl,
    reaped: bool,
    wait: bool,
) -> stdio::Result<()> {
    stop_owned(state, record, control, reaped)?;
    if wait {
        return control.del_dev().map_err(error).map(|_| ());
    }
    // Synchronous DEL_DEV waits for the final kernel reference, including a
    // task-less mount namespace retained by an FD. Queue deletion, then retain
    // our durable receipt until GET_DEV_INFO proves this nonce is gone.
    control.del_dev_async().map_err(error)?;
    let id = i32::try_from(control.dev_info().dev_id).map_err(stdio::Error::other)?;
    deletion_complete(
        record.token,
        UblkCtrlBuilder::default()
            .id(id)
            .build()
            .map(|next| next.dev_info().ublksrv_flags)
            .map_err(error),
    )
}

fn deletion_complete(token: u64, observed: stdio::Result<u64>) -> stdio::Result<()> {
    match observed {
        Ok(found) if found == token => Err(stdio::Error::new(
            stdio::ErrorKind::WouldBlock,
            "ublk deletion is pending; external references and ownership are retained",
        )),
        Ok(_) => Ok(()), // Reuse of the numeric ID proves the preceding device retired.
        Err(error) if matches!(error.raw_os_error(), Some(libc::ENOENT | libc::ENODEV)) => Ok(()),
        Err(error) => Err(error),
    }
}

fn cleanup(state: &Path, record: &Record, joined: bool, wait: bool) -> stdio::Result<()> {
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
                retire(state, record, &control, joined, wait)?;
            }
        }
        return forget(state, record);
    };
    let control = match UblkCtrlBuilder::default().id(device_id).build() {
        Ok(control) => control,
        Err(error) => {
            let error = self::error(error);
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
    retire(state, record, &control, joined, wait)?;
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

/// Native recovery owns both disk leases and has requested workload termination.
/// Abort only dead, nonce-verified backends so blocked consumers can finish exiting;
/// keep kernel devices and receipts until fencing and unmount are confirmed.
pub(crate) fn quiesce_stale_host_disk(state: &Path, run: &str) -> stdio::Result<()> {
    if !quiesce_if_owned_host_disk(state, run)? {
        return Err(stdio::Error::other(
            "Mounted native disk has no current ownership receipt",
        ));
    }
    Ok(())
}

/// A persistent attachment can outlive a reboot or completed retirement. Never
/// infer a current device's ownership from that stale major/minor alone.
pub(crate) fn quiesce_if_owned_host_disk(state: &Path, run: &str) -> stdio::Result<bool> {
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
            continue;
        }
        stop_owned(state, &record, &control, false)?;
        owned = true;
    }
    Ok(owned)
}

/// Called only under the controller lock, after fencing consumers and unmounting
/// host filesystems. Never retires an unrecorded owner or a live backend/VMM.
pub fn cleanup_stale(state: &Path) -> stdio::Result<()> {
    cleanup_stale_with(state, true)
}

pub(crate) fn cleanup_stale_native(state: &Path) -> stdio::Result<()> {
    cleanup_stale_with(state, false)
}

fn cleanup_stale_with(state: &Path, wait: bool) -> stdio::Result<()> {
    for record in records(state)? {
        cleanup(state, &record, false, wait)?;
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
    /// VM callers consume their handle and retain the existing synchronous close
    /// contract. Native callers retain/retry close_in_place instead.
    pub fn close(mut self) -> stdio::Result<()> {
        self.close_with(true)
    }

    /// The caller has fenced all consumers and unmounted any host filesystem.
    /// Retaining the handle on error permits safe retry of kernel retirement.
    pub(crate) fn close_in_place(&mut self) -> stdio::Result<()> {
        self.close_with(false)
    }

    fn close_with(&mut self, wait: bool) -> stdio::Result<()> {
        if self.thread.is_some() {
            self.stop()?;
            self.thread
                .take()
                .unwrap()
                .join()
                .map_err(|_| stdio::Error::other("ublk backend panicked"))??;
        }
        cleanup(&self.state, &self.record, true, wait)
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
            if let Err(error) = cleanup(&server_state, &record, true, true) {
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
    fn unsupported_kernels_refuse_admission_before_the_first_device() {
        for release in ["6.8.0-ubuntu", "6.10.12", "invalid"] {
            assert!(!async_retirement_kernel(release));
        }
        for release in ["6.11.0", "6.11.0-rc1", "6.12.0-custom", "7.2.11-arch1-1"] {
            assert!(async_retirement_kernel(release));
        }
    }

    #[test]
    #[ignore = "requires disposable CAP_SYS_ADMIN/CAP_SYS_RESOURCE and a private mount namespace"]
    fn isolated_retirement_qualification() {
        prepare_io_threads().unwrap();
        for namespace_reference in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let state = root.path().join("state");
            let jail = state
                .join("jails/firecracker")
                .join(uuid::Uuid::new_v4().to_string())
                .join("root");
            fs::create_dir_all(&jail).unwrap();
            let image = root.path().join("disk.img");
            File::create(&image)
                .unwrap()
                .set_len(64 * 1024 * 1024)
                .unwrap();
            if namespace_reference {
                assert!(
                    std::process::Command::new("mkfs.ext4")
                        .args(["-q", "-F"])
                        .arg(&image)
                        .status()
                        .unwrap()
                        .success()
                );
            }
            let disk = Arc::new(super::super::device::LocalDisk::open(&image, true).unwrap());
            let mut mounted = mount_disk(disk, &state, &jail, unsafe { libc::geteuid() }).unwrap();
            let reference = if namespace_reference {
                let target = root.path().join("mounted");
                fs::create_dir(&target).unwrap();
                let source = CString::new(jail.join("disk.blk").as_os_str().as_bytes()).unwrap();
                let destination = CString::new(target.as_os_str().as_bytes()).unwrap();
                assert_eq!(
                    unsafe {
                        libc::mount(
                            source.as_ptr(),
                            destination.as_ptr(),
                            c"ext4".as_ptr(),
                            libc::MS_NODEV | libc::MS_NOSUID,
                            std::ptr::null(),
                        )
                    },
                    0
                );
                let reference = std::thread::spawn(|| {
                    assert_eq!(unsafe { libc::unshare(libc::CLONE_FS) }, 0);
                    private_mount_namespace(&[]).unwrap();
                    File::open("/proc/thread-self/ns/mnt").unwrap()
                })
                .join()
                .unwrap();
                // The worker exited. Only this FD retains its filesystem mount.
                assert_eq!(unsafe { libc::umount2(destination.as_ptr(), 0) }, 0);
                reference
            } else {
                File::open(jail.join("disk.blk")).unwrap()
            };
            let started = std::time::Instant::now();
            assert_eq!(
                mounted.close_in_place().unwrap_err().kind(),
                stdio::ErrorKind::WouldBlock
            );
            assert!(
                started.elapsed() < Duration::from_secs(3),
                "retirement blocked on an external reference"
            );
            assert!(
                mounted.record.path(&state).exists(),
                "pending device lost its ownership receipt"
            );
            assert_eq!(
                mounted.close_in_place().unwrap_err().kind(),
                stdio::ErrorKind::WouldBlock
            );
            if !namespace_reference {
                let receipt = mounted.record.path(&state);
                let release = std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_millis(200));
                    drop(reference);
                });
                let waiting = std::time::Instant::now();
                // This is the consuming close used by the VM transport. It must
                // retain its receipt and caller's lease until the last FD exits.
                mounted.close().unwrap();
                assert!(waiting.elapsed() >= Duration::from_millis(150));
                release.join().unwrap();
                assert!(!receipt.exists());
                println!(
                    "native_retirement consuming_vm_close elapsed_ms={}",
                    started.elapsed().as_millis()
                );
                continue;
            }
            drop(reference);
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            loop {
                match mounted.close_in_place() {
                    Ok(()) => break,
                    Err(error)
                        if error.kind() == stdio::ErrorKind::WouldBlock
                            && std::time::Instant::now() < deadline =>
                    {
                        std::thread::sleep(Duration::from_millis(20));
                    }
                    Err(error) => {
                        panic!("retirement did not finish after reference release: {error}")
                    }
                }
            }
            assert!(!mounted.record.path(&state).exists());
            println!(
                "native_retirement namespace_reference={namespace_reference} elapsed_ms={}",
                started.elapsed().as_millis()
            );
        }
    }

    #[test]
    fn pending_deletion_keeps_the_receipt_until_the_owned_nonce_disappears() {
        assert_eq!(
            deletion_complete(42, Ok(42)).unwrap_err().kind(),
            stdio::ErrorKind::WouldBlock
        );
        assert!(deletion_complete(42, Ok(43)).is_ok());
        for errno in [libc::ENOENT, libc::ENODEV] {
            assert!(deletion_complete(42, Err(stdio::Error::from_raw_os_error(errno))).is_ok());
        }
        assert!(deletion_complete(42, Err(stdio::Error::from_raw_os_error(libc::EACCES))).is_err());
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
        assert!(!quiesce_if_owned_host_disk(root.path(), &record.vm_id).unwrap());
        assert!(record.path(root.path()).exists());
        cleanup_stale(root.path()).unwrap();
        assert!(!record.path(root.path()).exists());
        // A crash after retiring/forgetting the device but before unlinking its
        // persistent attachment likewise has no current device to fence.
        assert!(!quiesce_if_owned_host_disk(root.path(), &record.vm_id).unwrap());
    }
}
