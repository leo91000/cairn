//! Independent root-owned lease enforcement. A stalled controller cannot extend a lease.
use super::atomic_write;
use crate::error::{Error, Result};
use serde_json::json;
use std::{os::linux::net::SocketAddrExt, path::Path, time::Duration};
use tokio_util::sync::CancellationToken;

pub async fn grant(path: &Path, until: u64) -> Result<()> {
    let boot = tokio::fs::read_to_string("/proc/sys/kernel/random/boot_id").await?;
    atomic_write(
        path,
        &serde_json::to_vec(&json!({ "boot": boot.trim(), "until": until }))?,
    )
    .await
}

fn notify() -> Result<()> {
    let socket = std::env::var("NOTIFY_SOCKET")
        .map_err(|_| Error::bad("Missing systemd watchdog socket."))?;
    let address = if let Some(name) = socket.strip_prefix('@') {
        std::os::unix::net::SocketAddr::from_abstract_name(name)?
    } else {
        std::os::unix::net::SocketAddr::from_pathname(socket)?
    };
    let socket = std::os::unix::net::UnixDatagram::unbound()?;
    socket.send_to_addr(b"READY=1\nWATCHDOG=1", &address)?;
    Ok(())
}

pub async fn serve(path: &Path, attempt: &str, stop: CancellationToken) -> Result<()> {
    crate::validation::uuid(attempt)?;
    if unsafe { libc::geteuid() } != 0 {
        return Err(Error::forbidden(
            "The native lease guardian requires its root-owned service.",
        ));
    }
    let boot = tokio::fs::read_to_string("/proc/sys/kernel/random/boot_id").await?;
    loop {
        let valid = async {
            let bytes =
                crate::process::read_bounded(tokio::fs::File::open(path).await?, 4096).await?;
            let value: serde_json::Value = serde_json::from_slice(&bytes)?;
            Ok::<_, Error>(
                value["boot"] == boot.trim()
                    && value["until"].as_u64().is_some_and(|until| {
                        until > crate::nodes::boot_ms()
                            && until <= crate::nodes::boot_ms().saturating_add(300_000)
                    }),
            )
        }
        .await
        .unwrap_or(false);
        if !valid || stop.is_cancelled() {
            break;
        }
        notify()?;
        tokio::select! {
            () = stop.cancelled() => break,
            () = tokio::time::sleep(Duration::from_millis(250)) => {}
        }
    }
    // BindsTo also stops the workload if this guardian dies or its own 5 s
    // systemd watchdog expires, even if this explicit stop cannot complete.
    super::systemd::stop_workload(attempt).await
}
