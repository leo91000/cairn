//! Renew only an anonymous, unassigned snapshot. Never inherit live connections.
use crate::{
    error::{Error, Result},
    microvm::protocol::CloneIdentity,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use std::{io, net::Ipv4Addr, os::fd::AsRawFd, process::Stdio};
use tokio::process::Command;

#[repr(C)]
struct Entropy {
    bits: i32,
    length: i32,
    bytes: [u8; 32],
}

pub(super) fn kvm_is_deferred() -> bool {
    std::fs::read_to_string("/sys/module/kvm/parameters/enable_virt_at_load")
        .is_ok_and(|value| value.trim() == "N")
}

pub(super) fn validate(identity: &CloneIdentity) -> Result<[u8; 32]> {
    uuid::Uuid::parse_str(&identity.id).map_err(|_| Error::bad("Invalid clone identity."))?;
    let guest: Ipv4Addr = identity
        .guest
        .parse()
        .map_err(|_| Error::bad("Invalid clone address."))?;
    let gateway: Ipv4Addr = identity
        .gateway
        .parse()
        .map_err(|_| Error::bad("Invalid clone gateway."))?;
    let guest = u32::from(guest);
    let gateway = u32::from(gateway);
    if guest >> 24 != 10 || guest % 4 != 2 || gateway.checked_add(1) != Some(guest) {
        return Err(Error::bad("Invalid clone network."));
    }
    let octets = identity.mac.split(':').collect::<Vec<_>>();
    if octets.len() != 6
        || octets[0] != "06"
        || octets
            .iter()
            .any(|part| part.len() != 2 || u8::from_str_radix(part, 16).is_err())
    {
        return Err(Error::bad("Invalid clone MAC address."));
    }
    STANDARD
        .decode(&identity.entropy)
        .map_err(|_| Error::bad("Invalid clone entropy."))?
        .try_into()
        .map_err(|_| Error::bad("Invalid clone entropy length."))
}

async fn ip(arguments: &[&str]) -> Result<()> {
    let output = Command::new("ip")
        .args(arguments)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .status()
        .await?;
    if !output.success() {
        return Err(Error::unavailable("Clone network renewal failed."));
    }
    Ok(())
}

pub(super) async fn renew(identity: &CloneIdentity) -> Result<()> {
    let entropy = validate(identity)?;
    let seed = Entropy {
        bits: 256,
        length: 32,
        bytes: entropy,
    };
    let random = std::fs::OpenOptions::new()
        .write(true)
        .open("/dev/urandom")?;
    // Linux RNDADDENTROPY credits independent host entropy, unlike an ordinary
    // write to /dev/urandom. RNDRESEEDCRNG immediately renews the kernel generator.
    for (request, argument) in [
        (
            0x4008_5203 as libc::c_ulong,
            &seed as *const Entropy as *const libc::c_void,
        ),
        (0x5207 as libc::c_ulong, std::ptr::null()),
    ] {
        if unsafe { libc::ioctl(random.as_raw_fd(), request, argument) } != 0 {
            return Err(io::Error::last_os_error().into());
        }
    }
    ip(&["link", "set", "eth0", "down"]).await?;
    ip(&["addr", "flush", "dev", "eth0"]).await?;
    ip(&["link", "set", "eth0", "address", &identity.mac]).await?;
    let address = format!("{}/30", identity.guest);
    ip(&["addr", "add", &address, "dev", "eth0"]).await?;
    ip(&["link", "set", "eth0", "up"]).await?;
    ip(&[
        "route",
        "replace",
        "default",
        "via",
        &identity.gateway,
        "dev",
        "eth0",
    ])
    .await?;
    // Host and guest identify this clone independently of the inherited boot ID.
    crate::skills::atomic_write(
        std::path::Path::new("/run/leo-clone-id"),
        identity.id.as_bytes(),
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clone_identity_rejects_cross_subnet_routes_and_unbounded_seed() {
        let mut identity = CloneIdentity {
            id: crate::config::id(),
            guest: "10.0.4.6".into(),
            gateway: "10.0.4.5".into(),
            mac: "06:00:00:00:01:01".into(),
            entropy: STANDARD.encode([42; 32]),
        };
        assert_eq!(validate(&identity).unwrap(), [42; 32]);
        identity.gateway = "10.0.8.5".into();
        assert!(validate(&identity).is_err());
        identity.gateway = "10.0.4.5".into();
        identity.guest = "10.0.4.7".into();
        assert!(validate(&identity).is_err());
        identity.guest = "10.0.4.6".into();
        identity.mac = "06:00:00:00:01:01;reboot".into();
        assert!(validate(&identity).is_err());
        identity.mac = "06:00:00:00:01:01".into();
        identity.entropy = STANDARD.encode([42; 31]);
        assert!(validate(&identity).is_err());
        identity.entropy = STANDARD.encode([42; 33]);
        assert!(validate(&identity).is_err());
        identity.entropy = STANDARD.encode([42; 32]);
        identity.id = "previous-conversation".into();
        assert!(validate(&identity).is_err());
    }
}
