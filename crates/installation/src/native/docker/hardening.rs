//! Install proc/sys/cgroup mount restrictions after RootlessKit has created its
//! user/PID/network namespaces. Inheriting systemd's masked submounts prevents
//! unprivileged procfs/sysfs creation and leaves a misleading host proc view.
use crate::error::{Error, Result};
use std::{ffi::CString, os::unix::fs::MetadataExt};

#[repr(C)]
struct Attributes {
    set: u64,
    clear: u64,
    propagation: u64,
    userns: u64,
}

pub(super) fn apply() -> Result<()> {
    if std::fs::metadata("/proc/1/ns/pid")?.ino() != std::fs::metadata("/proc/self/ns/pid")?.ino()
        || !std::fs::read_to_string("/proc/1/stat")?.starts_with("1 ")
    {
        return Err(Error::unavailable(
            "RootlessKit could not create a private procfs; native execution refused.",
        ));
    }
    // Docker configures only this enclave's network namespace. Preserve a
    // separate net sysctl mount before making global sysctls read-only; engine
    // UID1000 has neither its owner identity nor CAP_NET_ADMIN to write it.
    bind("/proc/sys/net")?;
    for path in [
        "/proc/sys",
        "/proc/sysrq-trigger",
        "/proc/irq",
        "/proc/bus",
        "/sys",
    ] {
        let recursive = path != "/proc/sys";
        bind(path)?;
        let path = CString::new(path).map_err(Error::internal)?;
        let attributes = Attributes {
            set: 1,
            clear: 0,
            propagation: 0,
            userns: 0,
        };
        if unsafe {
            libc::syscall(
                libc::SYS_mount_setattr,
                libc::AT_FDCWD,
                path.as_ptr(),
                if recursive { 0x8000 } else { 0 },
                &raw const attributes,
                std::mem::size_of::<Attributes>(),
            )
        } != 0
        {
            return Err(std::io::Error::last_os_error().into());
        }
    }
    Ok(())
}

fn bind(path: &str) -> Result<()> {
    let path = CString::new(path).map_err(Error::internal)?;
    if unsafe {
        libc::mount(
            path.as_ptr(),
            path.as_ptr(),
            std::ptr::null(),
            libc::MS_BIND | libc::MS_REC,
            std::ptr::null(),
        )
    } != 0
    {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}
