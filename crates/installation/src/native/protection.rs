//! Keep storage CPU protection independent of slow workspace accounting/capture.
use super::{suspension::Suspension, systemd};
use crate::microvm::suspension::Reason;
use crate::{error::Result, storage::runtime::Volume};
use std::{sync::Arc, time::Duration};
use tokio_util::sync::CancellationToken;

pub async fn monitor(
    volume: Arc<Volume>,
    attempt: &str,
    suspension: Arc<Suspension>,
    stop: CancellationToken,
) -> Result<()> {
    systemd::wait_started(attempt, &stop).await?;
    suspension.bind().await?;
    volume.activate_attempt(attempt)?;
    let mut confirmed = false;
    volume.set_paused(false);
    let mut tick = tokio::time::interval(Duration::from_millis(250));
    loop {
        tokio::select! {
            () = stop.cancelled() => return Ok(()),
            _ = tick.tick() => {},
        }
        let reason = volume.cpu_pause_reason()?;
        let blocked = reason.is_some();
        if blocked != confirmed {
            // The backend/controller remain runnable while every process in the
            // workload cgroup freezes. Publication can free journal headroom.
            let transition = suspension.set_reason(&Reason::Storage, blocked).await;
            if stop.is_cancelled() {
                return Ok(());
            }
            transition?;
            confirmed = blocked;
            volume.set_paused(blocked);
            tracing::info!(
                attempt,
                paused = blocked,
                reason = reason.unwrap_or("ready"),
                "Native storage CPU state confirmed"
            );
        }
    }
}
