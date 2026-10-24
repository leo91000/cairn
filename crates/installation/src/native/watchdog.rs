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

/// Keep lease enforcement independent of the controller's poll scheduling.
async fn lease_stop_reason(
    path: &Path,
    boot: &str,
    clock: impl FnOnce() -> u64,
) -> Option<&'static str> {
    let value = async {
        let bytes = crate::process::read_bounded(tokio::fs::File::open(path).await?, 4096).await?;
        serde_json::from_slice::<serde_json::Value>(&bytes).map_err(Error::from)
    }
    .await;
    let Ok(value) = value else {
        return Some("lease_invalid");
    };
    // File I/O can outlast the lease. Validate against the clock after reading.
    let now = clock();
    if value["boot"] != boot {
        return Some("lease_invalid");
    }
    let Some(until) = value["until"].as_u64() else {
        return Some("lease_invalid");
    };
    if until <= now {
        return Some("lease_expired");
    }
    if until > now.saturating_add(300_000) {
        return Some("lease_invalid");
    }
    None
}

async fn record_lease_stop(path: &Path, attempt: &str, reason: &str) {
    let details = json!({
        "reason": reason,
        "at": crate::config::now(),
        "usage": null,
    });
    tracing::warn!(attempt, diagnostics = %details, "Native lease guardian stopping workload");
    // A failed/stalled diagnostic write must not prevent independent fencing.
    let recorded = tokio::time::timeout(Duration::from_millis(100), async {
        super::record_stop_once(&path.with_extension("stop-reason.json"), &details).await
    })
    .await;
    if !matches!(recorded, Ok(Ok(()))) {
        tracing::warn!(
            attempt,
            reason,
            "Native lease stop diagnostic persistence failed; proceeding with fencing"
        );
    }
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
        if let Some(reason) = lease_stop_reason(path, boot.trim(), crate::nodes::boot_ms).await {
            record_lease_stop(path, attempt, reason).await;
            break;
        }
        if stop.is_cancelled() {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn guardian_preserves_an_earlier_quota_trigger_and_measurements() {
        let directory = tempfile::tempdir().unwrap();
        let attempt = crate::config::id();
        let path = directory.path().join(format!("{attempt}.lease"));
        let reason_path = path.with_extension("stop-reason.json");
        let quota = json!({
            "reason": "conversation_disk_limit",
            "at": 123,
            "usage": { "conversationBytes": 48, "conversationLimitBytes": 32 },
        });
        super::super::record_stop_once(&reason_path, &quota)
            .await
            .unwrap();
        record_lease_stop(&path, &attempt, "lease_expired").await;
        let saved: serde_json::Value =
            serde_json::from_slice(&tokio::fs::read(&reason_path).await.unwrap()).unwrap();
        assert_eq!(saved, quota);
    }

    #[tokio::test]
    async fn guardian_records_expiry_at_its_initiating_path_without_controller_polling() {
        let directory = tempfile::tempdir().unwrap();
        let attempt = crate::config::id();
        let path = directory.path().join(format!("{attempt}.lease"));
        atomic_write(&path, br#"{"boot":"fixture","until":100}"#)
            .await
            .unwrap();
        assert_eq!(lease_stop_reason(&path, "fixture", || 99).await, None);
        let reason = lease_stop_reason(&path, "fixture", || 100).await.unwrap();
        assert_eq!(reason, "lease_expired");
        record_lease_stop(&path, &attempt, reason).await;
        // Persistence may outlive its bounded caller while retaining ownership.
        tokio::time::timeout(Duration::from_secs(5), async {
            while !path.with_extension("stop-reason.json").exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let saved: serde_json::Value = serde_json::from_slice(
            &tokio::fs::read(path.with_extension("stop-reason.json"))
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(saved["reason"], "lease_expired");
        assert!(saved["at"].as_i64().unwrap() > 0);
        assert_eq!(
            lease_stop_reason(&path, "different-boot", || 99).await,
            Some("lease_invalid")
        );
        assert_eq!(lease_stop_reason(&path, "fixture", || 0).await, None);
        atomic_write(&path, br#"{"boot":"fixture","until":300001}"#)
            .await
            .unwrap();
        assert_eq!(
            lease_stop_reason(&path, "fixture", || 0).await,
            Some("lease_invalid")
        );
        atomic_write(&path, b"malformed").await.unwrap();
        assert_eq!(
            lease_stop_reason(&path, "fixture", || 0).await,
            Some("lease_invalid")
        );
    }
}
