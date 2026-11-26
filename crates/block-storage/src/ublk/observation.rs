//! Native retirement must observe the kernel's absence, not a legacy retry errno.
use std::{fs::OpenOptions, io, os::fd::AsRawFd};

use libublk::{
    ctrl::UblkCtrl,
    io_uring::{IoUring, opcode, squeue, types},
    sys,
};

pub(super) fn native_control(id: i32) -> io::Result<UblkCtrl> {
    preserve_absence(
        libublk::ctrl::UblkCtrlBuilder::default()
            .id(id)
            .build()
            .map_err(super::error),
        || encoded_probe(id),
    )
}

fn preserve_absence<T>(
    opened: io::Result<T>,
    probe: impl FnOnce() -> io::Result<bool>,
) -> io::Result<T> {
    let error = match opened {
        Ok(value) => return Ok(value),
        Err(error) => error,
    };
    // libublk retries even ENODEV with the legacy opcode. Kernels built without
    // legacy opcodes return EOPNOTSUPP, hiding the original absence. Never infer
    // absence from that errno or from a missing sysfs path alone.
    if error.raw_os_error() == Some(libc::EOPNOTSUPP) && matches!(probe(), Ok(true)) {
        return Err(io::Error::from_raw_os_error(libc::ENODEV));
    }
    Err(error)
}

fn encoded_probe(id: i32) -> io::Result<bool> {
    let id = u32::try_from(id).map_err(io::Error::other)?;
    let control = OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/ublk-control")?;
    let mut ring = IoUring::<squeue::Entry128>::builder().build(2)?;
    let mut payload = [0u8; 80];
    payload[..4].copy_from_slice(&id.to_ne_bytes());
    payload[4..6].copy_from_slice(&u16::MAX.to_ne_bytes());
    let command = opcode::UringCmd80::new(
        types::Fd(control.as_raw_fd()),
        sys::UBLK_U_CMD_GET_DEV_INFO2,
    )
    .cmd(payload)
    .build();
    // Kernel lookup precedes buffer/permission validation. A zero-length query
    // therefore returns ENODEV only when no live registration is exposed; an
    // existing device returns an error and remains owned. No user pointer is
    // submitted, so uncertain completion cannot outlive a borrowed buffer.
    if unsafe { ring.submission().push(&command) }.is_err() {
        return Err(io::Error::other("Native device observation queue is full"));
    }
    loop {
        if let Some(completion) = ring.completion().next() {
            let result = completion.result();
            return match result {
                code if code == -libc::ENODEV => Ok(true),
                code if code < 0 => Err(io::Error::from_raw_os_error(-code)),
                _ => Ok(false),
            };
        }
        match ring.submit_and_wait(1) {
            Ok(_) => {}
            Err(error) if matches!(error.raw_os_error(), Some(libc::EINTR | libc::EBUSY)) => {}
            Err(error) => return Err(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires the real ublk control device on a disposable host"]
    fn encoded_absence_is_not_replaced_by_a_legacy_opcode_error() {
        // This ID cannot be allocated by the kernel (UBLK_MAX_UBLKS is 2^20).
        assert!(encoded_probe(i32::MAX).unwrap());
        assert_eq!(
            native_control(i32::MAX).err().unwrap().raw_os_error(),
            Some(libc::ENODEV)
        );
        let live = libublk::ctrl::UblkCtrlBuilder::default()
            .name("cairn_absence_probe")
            .dev_flags(libublk::UblkFlags::UBLK_DEV_F_ADD_DEV)
            .build()
            .unwrap();
        let id = i32::try_from(live.dev_info().dev_id).unwrap();
        assert!(!matches!(encoded_probe(id), Ok(true)));
        assert!(native_control(id).is_ok());
        drop(live);
        assert!(encoded_probe(id).unwrap());
    }

    #[test]
    fn masked_absence_requires_an_encoded_kernel_observation() {
        let result =
            preserve_absence::<()>(Err(io::Error::from_raw_os_error(libc::EOPNOTSUPP)), || {
                Ok(true)
            });
        assert_eq!(result.unwrap_err().raw_os_error(), Some(libc::ENODEV));
        for observed in [
            Ok(false),
            Err(io::Error::from_raw_os_error(libc::EPERM)),
            Err(io::Error::from_raw_os_error(libc::ENOENT)),
        ] {
            let result =
                preserve_absence::<()>(Err(io::Error::from_raw_os_error(libc::EOPNOTSUPP)), || {
                    observed
                });
            assert_eq!(result.unwrap_err().raw_os_error(), Some(libc::EOPNOTSUPP));
        }
        let result = preserve_absence::<()>(Err(io::Error::from_raw_os_error(libc::EIO)), || {
            panic!("unrelated errors must not trigger a probe")
        });
        assert_eq!(result.unwrap_err().raw_os_error(), Some(libc::EIO));
    }
}
