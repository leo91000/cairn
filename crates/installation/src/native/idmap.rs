//! Idmapped ext4 mounts let a native workload use a VM disk in place. Files the
//! VM agent owns (UID/GID 1000) appear owned by the workload account, and the
//! workload's writes are stored as 1000 again, so a later VM boot needs no
//! ownership rewrite. Root stays root; every other owner appears unmapped.
use std::{
    ffi::CString,
    io,
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd},
        unix::ffi::OsStrExt,
    },
    path::Path,
};

/// The VM agent identity that owns user files on VM disks.
pub(crate) const VM_AGENT: u32 = 1000;

// Linux UAPI values for the mount API (linux/mount.h, linux/fcntl.h).
const FSOPEN_CLOEXEC: libc::c_uint = 0x1;
const FSCONFIG_SET_STRING: libc::c_uint = 1;
const FSCONFIG_CMD_CREATE: libc::c_uint = 6;
const FSMOUNT_CLOEXEC: libc::c_uint = 0x1;
const MOUNT_ATTR_NOSUID: u64 = 0x2;
const MOUNT_ATTR_NODEV: u64 = 0x4;
const MOUNT_ATTR_NOATIME: u64 = 0x10;
const MOUNT_ATTR_IDMAP: u64 = 0x0010_0000;
const MOVE_MOUNT_F_EMPTY_PATH: libc::c_uint = 0x4;

#[repr(C)]
struct MountAttr {
    attr_set: u64,
    attr_clr: u64,
    propagation: u64,
    userns_fd: u64,
}

/// One map line per identity: disk root stays root and the VM agent becomes
/// `workload`. Both columns must stay disjoint, so root cannot be the workload.
pub(crate) fn map(workload: u32) -> io::Result<String> {
    if workload == 0 {
        return Err(io::Error::other(
            "The native workload account must not be root.",
        ));
    }
    Ok(format!("0 0 1\n{VM_AGENT} {workload} 1\n"))
}

fn check(result: libc::c_long) -> io::Result<libc::c_long> {
    if result < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(result)
    }
}

fn path(value: &Path) -> io::Result<CString> {
    Ok(CString::new(value.as_os_str().as_bytes())?)
}

fn write_map(pid: libc::pid_t, name: &str, map: &str) -> io::Result<()> {
    std::fs::write(format!("/proc/{pid}/{name}"), map)
}

fn pipe() -> io::Result<(OwnedFd, OwnedFd)> {
    let mut ends = [0; 2];
    if unsafe { libc::pipe2(ends.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { (OwnedFd::from_raw_fd(ends[0]), OwnedFd::from_raw_fd(ends[1])) })
}

/// A user namespace holding `uid_map`/`gid_map`, kept alive by its descriptor.
/// The helper child only performs raw system calls between fork and exit.
pub(crate) fn user_namespace(uid_map: &str, gid_map: &str) -> io::Result<OwnedFd> {
    let (ready_read, ready_write) = pipe()?;
    let (hold_read, hold_write) = pipe()?;

    let pid = unsafe { libc::fork() };
    if pid < 0 {
        return Err(io::Error::last_os_error());
    }
    if pid == 0 {
        // Child: async-signal-safe calls only, then exit without unwinding. It
        // must not keep the hold pipe's write end, or it would wait forever.
        unsafe {
            libc::close(hold_write.as_raw_fd());
            libc::close(ready_read.as_raw_fd());
            let status: u8 = if libc::unshare(libc::CLONE_NEWUSER) == 0 {
                1
            } else {
                0
            };
            libc::write(ready_write.as_raw_fd(), (&raw const status).cast(), 1);
            let mut byte = 0u8;
            libc::read(hold_read.as_raw_fd(), (&raw mut byte).cast(), 1);
            libc::_exit(0);
        }
    }

    // Without our copies of these ends, a dead child reads as end of file and
    // dropping the hold pipe's write end on every path releases the child.
    drop(ready_write);
    drop(hold_read);
    let result = (|| {
        let mut status = 0u8;
        let read = unsafe { libc::read(ready_read.as_raw_fd(), (&raw mut status).cast(), 1) };
        if read != 1 || status != 1 {
            return Err(io::Error::other("Could not create a user namespace."));
        }
        write_map(pid, "uid_map", uid_map)?;
        write_map(pid, "gid_map", gid_map)?;
        let namespace = std::fs::File::open(format!("/proc/{pid}/ns/user"))?;
        Ok(OwnedFd::from(namespace))
    })();
    drop(hold_write);
    let mut status = 0;
    unsafe { libc::waitpid(pid, &raw mut status, 0) };
    result
}

/// Mounts the ext4 filesystem on `source` at `target` without ever attaching
/// an unmapped view. Ownership is translated for the `(uid, gid)` workload.
pub(crate) fn mount_ext4(source: &Path, target: &Path, workload: (u32, u32)) -> io::Result<()> {
    let namespace = user_namespace(&map(workload.0)?, &map(workload.1)?)?;
    attach(c"ext4", Some(source), target, Some(&namespace))
}

fn attach(
    kind: &std::ffi::CStr,
    source: Option<&Path>,
    target: &Path,
    namespace: Option<&OwnedFd>,
) -> io::Result<()> {
    let context = check(unsafe { libc::syscall(libc::SYS_fsopen, kind.as_ptr(), FSOPEN_CLOEXEC) })?;
    let context = unsafe { OwnedFd::from_raw_fd(context as i32) };
    if let Some(source) = source {
        let source = path(source)?;
        check(unsafe {
            libc::syscall(
                libc::SYS_fsconfig,
                context.as_raw_fd(),
                FSCONFIG_SET_STRING,
                c"source".as_ptr(),
                source.as_ptr(),
                0,
            )
        })?;
    }
    check(unsafe {
        libc::syscall(
            libc::SYS_fsconfig,
            context.as_raw_fd(),
            FSCONFIG_CMD_CREATE,
            std::ptr::null::<libc::c_char>(),
            std::ptr::null::<libc::c_void>(),
            0,
        )
    })?;
    let mount = check(unsafe {
        libc::syscall(
            libc::SYS_fsmount,
            context.as_raw_fd(),
            FSMOUNT_CLOEXEC,
            MOUNT_ATTR_NODEV | MOUNT_ATTR_NOSUID | MOUNT_ATTR_NOATIME,
        )
    })?;
    let mount = unsafe { OwnedFd::from_raw_fd(mount as i32) };

    // Idmapping is only allowed while the new mount is still detached.
    if let Some(namespace) = namespace {
        let attributes = MountAttr {
            attr_set: MOUNT_ATTR_IDMAP,
            attr_clr: 0,
            propagation: 0,
            userns_fd: namespace.as_raw_fd() as u64,
        };
        check(unsafe {
            libc::syscall(
                libc::SYS_mount_setattr,
                mount.as_raw_fd(),
                c"".as_ptr(),
                libc::AT_EMPTY_PATH,
                &raw const attributes,
                std::mem::size_of::<MountAttr>(),
            )
        })?;
    }
    let target = path(target)?;
    check(unsafe {
        libc::syscall(
            libc::SYS_move_mount,
            mount.as_raw_fd(),
            c"".as_ptr(),
            libc::AT_FDCWD,
            target.as_ptr(),
            MOVE_MOUNT_F_EMPTY_PATH,
        )
    })?;
    Ok(())
}

/// Whether this host lets the controller create the user namespaces that
/// idmapped VM disks need. Only the root controller can map other identities.
pub(crate) fn supported() -> bool {
    static SUPPORTED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *SUPPORTED.get_or_init(|| {
        let root = unsafe { libc::geteuid() } == 0;
        root && user_namespace("0 0 1\n", "0 0 1\n").is_ok()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_keep_root_and_translate_only_the_vm_agent() {
        assert_eq!(map(936).unwrap(), "0 0 1\n1000 936 1\n");
        assert_eq!(map(1000).unwrap(), "0 0 1\n1000 1000 1\n");
        assert!(map(0).is_err());
    }

    /// Run inside `unshare -Urm` with `--ignored`: tmpfs supports idmapped
    /// mounts in an unprivileged user namespace; ext4 additionally needs root.
    #[test]
    #[ignore = "requires a private user and mount namespace"]
    fn idmapped_mounts_attach_only_their_mapped_view() {
        use std::os::unix::fs::MetadataExt;

        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("mounted");
        std::fs::create_dir(&target).unwrap();
        let namespace = user_namespace("0 0 1\n", "0 0 1\n").unwrap();
        attach(c"tmpfs", None, &target, Some(&namespace)).unwrap();

        let mounts = std::fs::read_to_string("/proc/self/mountinfo").unwrap();
        let line = mounts
            .lines()
            .find(|line| line.split_whitespace().nth(4) == target.to_str())
            .unwrap();
        assert!(line.contains("idmapped"));
        assert!(line.contains("nosuid") && line.contains("nodev") && line.contains("noatime"));

        std::fs::write(target.join("file"), b"written through the mapped view").unwrap();
        assert_eq!(std::fs::metadata(target.join("file")).unwrap().uid(), 0);
        let path = CString::new(target.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::umount2(path.as_ptr(), 0) }, 0);
    }
}
