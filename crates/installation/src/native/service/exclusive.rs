//! Native adapter for the shared reservation lifecycle.
use super::Controller;
use crate::{
    error::{Error, Result},
    microvm::suspension::Reason,
    native::{suspension, systemd},
    runner::exclusive::{Acquisition, Ledger, Reservations},
};
use std::{path::Path, time::Duration};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

#[async_trait::async_trait]
impl Reservations for Controller {
    fn state(&self) -> &Path {
        &self.state
    }

    fn admission(&self) -> &Mutex<()> {
        &self.admission
    }

    fn ledger(&self) -> &Mutex<Ledger> {
        &self.exclusive
    }

    fn stop_token(&self) -> &CancellationToken {
        &self.stop
    }

    fn check_exclusive(&self) -> Result<()> {
        Ok(())
    }

    async fn active_attempts(&self) -> Vec<String> {
        self.active.lock().await.keys().cloned().collect()
    }

    async fn validate_members(&self, request: &Acquisition) -> Result<()> {
        let active = self.active.lock().await;
        for id in std::iter::once(&request.owner_attempt_id).chain(&request.member_attempt_ids) {
            if !active
                .get(id)
                .is_some_and(|attempt| !attempt.stop.is_cancelled())
                || !self.leased(id).await
            {
                return Err(Error::conflict(
                    "Exclusive member execution is inactive or its lease expired.",
                ));
            }
        }
        Ok(())
    }

    async fn set_exclusive(&self, _enabled: bool) -> Result<()> {
        // Admission is serialized with the durable reservation ledger. Native
        // attempts share the existing workload slice and keep their ceilings.
        Ok(())
    }

    async fn qualify_exclusive_memory(&self, _ledger: &Ledger, required: u64) -> Result<()> {
        let memory = systemd::memory_current().await?.div_ceil(1_048_576);
        let budget = self.budget.lock().await["limits"]["memoryMiB"]
            .as_u64()
            .ok_or_else(|| Error::unavailable("Native shared memory budget is unavailable."))?;
        let host = crate::microvm::hibernation::available_host_memory_mib()?;
        if budget.saturating_sub(memory).min(host) < required {
            return Err(Error::unavailable(
                "The requested exclusive memory headroom is unavailable; native reservations do not reclaim peer memory.",
            ));
        }
        Ok(())
    }

    async fn transition_exclusive(&self, id: &str, reason: &Reason, paused: bool) -> Result<()> {
        let attempt = self
            .active
            .lock()
            .await
            .get(id)
            .map(|attempt| (attempt.stop.clone(), attempt.suspension.clone()));
        let Some((stop, suspension)) = attempt else {
            return Ok(());
        };
        if stop.is_cancelled() || !self.leased(id).await {
            stop.cancel();
            return self.stop_admitted(id).await;
        }
        tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                if stop.is_cancelled() || !self.leased(id).await {
                    stop.cancel();
                    return self.stop_admitted(id).await;
                }
                if suspension.set_reason(reason, paused).await? {
                    return Ok(());
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .map_err(|_| Error::timeout("Native suspension confirmation timed out."))?
    }

    async fn stop_exclusive_attempt(&self, id: &str) -> Result<()> {
        self.stop_admitted(id).await
    }

    async fn administrative_ms(&self, id: &str) -> Result<u64> {
        suspension::administrative_ms(&self.state, id).await
    }
}
