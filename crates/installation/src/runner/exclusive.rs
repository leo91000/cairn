//! Durable node-local exclusivity. Admission is closed before any VM is paused.
use super::broker::Broker;
use crate::{
    config::now,
    error::{Error, Result},
    microvm::suspension::{self, Reason},
    skills::atomic_write,
    validation::uuid,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{collections::BTreeMap, time::Duration};

const FILE: &str = "exclusive-reservation.json";
const MAX_DURATION_MS: u64 = 15 * 60_000;
const MAX_LIFETIME_MS: u64 = 60 * 60_000;
const MAX_MEMBERS: usize = 32;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Acquisition {
    pub id: String,
    pub epoch: u64,
    pub owner_attempt_id: String,
    #[serde(default)]
    pub member_attempt_ids: Vec<String>,
    pub expires_at: i64,
    /// Remaining manager-side duration. Absolute expiry is retained for display.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remaining_ms: Option<u64>,
    pub reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub minimum_available_memory_mi_b: Option<u64>,
}

impl Acquisition {
    fn duration_ms(&self, time: i64) -> Result<u64> {
        let duration = match self.remaining_ms {
            Some(duration) => duration,
            None => self
                .expires_at
                .checked_sub(time)
                .and_then(|duration| u64::try_from(duration).ok())
                .ok_or_else(|| Error::bad("Invalid exclusive reservation duration."))?,
        };
        if duration == 0 || duration > MAX_DURATION_MS {
            return Err(Error::bad("Invalid exclusive reservation duration."));
        }
        Ok(duration)
    }

    fn validate(&self, time: i64) -> Result<()> {
        self.duration_ms(time)?;
        uuid(&self.id)?;
        uuid(&self.owner_attempt_id)?;
        let mut members = std::collections::HashSet::new();
        for id in &self.member_attempt_ids {
            uuid(id)?;
            if id == &self.owner_attempt_id || !members.insert(id) {
                return Err(Error::bad(
                    "Exclusive members must be distinct execution attempts.",
                ));
            }
        }
        if self.epoch == 0
            || self.member_attempt_ids.len() > MAX_MEMBERS
            || self.reason.trim().is_empty()
            || self.reason.chars().count() > 500
        {
            return Err(Error::bad("Invalid exclusive reservation."));
        }
        Ok(())
    }

    fn contains(&self, attempt: &str) -> bool {
        self.owner_attempt_id == attempt || self.member_attempt_ids.iter().any(|id| id == attempt)
    }

    fn same_identity(&self, other: &Self) -> bool {
        let mut left = self.clone();
        let mut right = other.clone();
        left.expires_at = 0;
        right.expires_at = 0;
        left.remaining_ms = None;
        right.remaining_ms = None;
        left.member_attempt_ids.sort();
        right.member_attempt_ids.sort();
        left == right
    }

    fn pause_reason(&self) -> Reason {
        Reason::Exclusive {
            reservation_id: self.id.clone(),
            epoch: self.epoch,
        }
    }
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Release {
    pub id: String,
    pub epoch: u64,
}

#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Ledger {
    #[serde(default)]
    highest_epoch: u64,
    #[serde(default)]
    reservation: Option<Reservation>,
    #[serde(default)]
    last_released: Option<(String, u64)>,
    #[serde(skip)]
    loaded: bool,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct Reservation {
    request: Acquisition,
    created_at: i64,
    #[serde(default)]
    created_boot_ms: u64,
    #[serde(default)]
    deadline_boot_ms: u64,
    state: String,
    #[serde(default)]
    #[serde(rename = "pausedAttemptIds")]
    paused_attempts: Vec<String>,
}

impl Reservation {
    fn public(&self) -> Result<Value> {
        let mut response = serde_json::to_value(&self.request)?;
        response.as_object_mut().unwrap().remove("remainingMs");
        response["createdAt"] = self.created_at.into();
        response["state"] = self.state.clone().into();
        response["pausedAttemptIds"] = self.paused_attempts.clone().into();
        Ok(response)
    }

    fn expired(&self, boot: u64) -> bool {
        boot == u64::MAX || boot < self.created_boot_ms || boot >= self.deadline_boot_ms
    }

    fn renew(&mut self, request: &Acquisition, deadline: u64) -> Result<()> {
        // Replays carry a decreasing remaining duration and the original expiry.
        // Deriving another deadline for them would extend ownership on every poll.
        if request.expires_at <= self.request.expires_at {
            return Ok(());
        }
        let lifetime = self
            .created_boot_ms
            .checked_add(MAX_LIFETIME_MS)
            .ok_or_else(|| Error::bad("Exclusive reservation lifetime is invalid."))?;
        if deadline > lifetime {
            return Err(Error::bad(
                "Exclusive reservation renewal exceeds its lifetime.",
            ));
        }
        self.request.expires_at = request.expires_at;
        self.request.remaining_ms = request.remaining_ms;
        self.deadline_boot_ms = self.deadline_boot_ms.max(deadline);
        Ok(())
    }
}

fn boot_deadline(duration: u64) -> Result<(u64, u64)> {
    let boot = crate::nodes::boot_ms();
    if boot == u64::MAX {
        return Err(Error::unavailable("Node monotonic clock is unavailable."));
    }
    let deadline = boot
        .checked_add(duration)
        .ok_or_else(|| Error::bad("Exclusive reservation deadline is invalid."))?;
    Ok((boot, deadline))
}

impl Broker {
    async fn read_exclusive(&self, ledger: &mut Ledger) -> Result<()> {
        if ledger.loaded {
            return Ok(());
        }
        match tokio::fs::read(self.state.join(FILE)).await {
            Ok(bytes) => *ledger = serde_json::from_slice(&bytes)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        ledger.loaded = true;
        Ok(())
    }

    async fn save_exclusive(&self, ledger: &Ledger) -> Result<()> {
        atomic_write(&self.state.join(FILE), &serde_json::to_vec(ledger)?).await?;
        let state = self.state.clone();
        tokio::task::spawn_blocking(move || std::fs::File::open(state)?.sync_all())
            .await
            .map_err(Error::internal)??;
        Ok(())
    }

    /// Startup has already fenced the previous controller's complete PID namespace.
    pub async fn recover_exclusive(&self) -> Result<()> {
        let _admission = self.admission.lock().await;
        let mut ledger = self.exclusive.lock().await;
        self.read_exclusive(&mut ledger).await?;
        if let Some(reservation) = ledger.reservation.take() {
            ledger.last_released = Some((reservation.request.id, reservation.request.epoch));
            self.save_exclusive(&ledger).await?;
        }
        Ok(())
    }

    /// Called while holding admission, including throughout slow pool reservation.
    pub async fn allow_exclusive_admission(&self, attempt: &str) -> Result<()> {
        let mut ledger = self.exclusive.lock().await;
        self.read_exclusive(&mut ledger).await?;
        if ledger.reservation.as_ref().is_some_and(|reservation| {
            reservation.state != "acquired" || !reservation.request.contains(attempt)
        }) {
            return Err(Error::conflict(
                "Node admission is reserved for another execution.",
            ));
        }
        Ok(())
    }

    pub async fn acquire_exclusive(&self, mut request: Acquisition) -> Result<Value> {
        request.minimum_available_memory_mi_b = request
            .minimum_available_memory_mi_b
            .filter(|value| *value > 0);
        let received_at = now();
        request.validate(received_at)?;
        let (received_boot_ms, deadline_boot_ms) =
            boot_deadline(request.duration_ms(received_at)?)?;
        let _admission = self.admission.lock().await;
        let mut ledger = self.exclusive.lock().await;
        self.read_exclusive(&mut ledger).await?;
        if self.stop.is_cancelled() {
            return Err(Error::conflict("Node controller is stopping."));
        }
        if let Some(current) = &mut ledger.reservation {
            if !request.same_identity(&current.request) {
                return Err(Error::conflict(
                    "The node already has an exclusive reservation.",
                ));
            }
            if current.state != "acquired" || current.expired(crate::nodes::boot_ms()) {
                return Err(Error::conflict(
                    "The exclusive reservation is being reconciled.",
                ));
            }
            self.validate_members(&request).await?;
            current.renew(&request, deadline_boot_ms)?;
            self.save_exclusive(&ledger).await?;
            return self.exclusive_response(&ledger).await;
        }
        if request.epoch <= ledger.highest_epoch {
            return Err(Error::conflict("The exclusive reservation epoch is stale."));
        }
        self.validate_members(&request).await?;
        ledger.highest_epoch = request.epoch;
        ledger.reservation = Some(Reservation {
            request,
            created_at: received_at,
            created_boot_ms: received_boot_ms,
            deadline_boot_ms,
            state: "acquiring".into(),
            paused_attempts: Vec::new(),
        });
        self.save_exclusive(&ledger).await?;
        let result = tokio::time::timeout(
            Duration::from_secs(120),
            self.confirm_exclusive(&mut ledger),
        )
        .await
        .map_err(|_| Error::unavailable("Exclusive acquisition timed out."))
        .and_then(std::convert::identity);
        if let Err(error) = result {
            if let Err(rollback) = self.release_current(&mut ledger, true).await {
                tracing::warn!(message = %rollback.message, "Exclusive acquisition rollback remains pending");
            }
            return Err(error);
        }
        self.exclusive_response(&ledger).await
    }

    async fn validate_members(&self, request: &Acquisition) -> Result<()> {
        let active = self.active.lock().await;
        for id in std::iter::once(&request.owner_attempt_id).chain(&request.member_attempt_ids) {
            let attempt = active
                .get(id)
                .filter(|attempt| !attempt.stop.is_cancelled())
                .ok_or_else(|| Error::conflict("Exclusive member attempt is no longer active."))?;
            if attempt.plan.node_lease_required()
                && !self
                    .leases
                    .lock()
                    .await
                    .get(id)
                    .is_some_and(|until| *until > crate::nodes::boot_ms())
            {
                return Err(Error::conflict(
                    "Exclusive member execution lease is missing or expired.",
                ));
            }
        }
        Ok(())
    }

    async fn confirm_exclusive(&self, ledger: &mut Ledger) -> Result<()> {
        self.pool.set_exclusive(true).await?;
        let request = ledger.reservation.as_ref().unwrap().request.clone();
        let attempts = self
            .active
            .lock()
            .await
            .keys()
            .filter(|id| !request.contains(id))
            .cloned()
            .collect::<Vec<_>>();
        for attempt in attempts {
            // Persist every target before its CPU transition, including uncertain responses.
            ledger
                .reservation
                .as_mut()
                .unwrap()
                .paused_attempts
                .push(attempt.clone());
            self.save_exclusive(ledger).await?;
            self.transition_exclusive(&attempt, &request.pause_reason(), true)
                .await?;
        }
        self.validate_members(&request).await?;
        if ledger
            .reservation
            .as_ref()
            .unwrap()
            .expired(crate::nodes::boot_ms())
            || self.stop.is_cancelled()
        {
            return Err(Error::conflict(
                "Exclusive acquisition expired before confirmation.",
            ));
        }
        if let Some(required) = request.minimum_available_memory_mi_b {
            self.qualify_exclusive_memory(ledger, required).await?;
        }
        self.validate_members(&request).await?;
        if ledger
            .reservation
            .as_ref()
            .unwrap()
            .expired(crate::nodes::boot_ms())
            || self.stop.is_cancelled()
        {
            return Err(Error::conflict(
                "Exclusive acquisition expired during memory qualification.",
            ));
        }
        self.pool.ensure_not_quarantined()?;
        ledger.reservation.as_mut().unwrap().state = "acquired".into();
        self.save_exclusive(ledger).await
    }

    async fn exclusive_memory_headroom(&self) -> Result<u64> {
        let budget = self.pool.budget().await.ok_or_else(|| {
            Error::unavailable("Exclusive memory admission requires shared node budgets.")
        })?;
        let (usage, _) = self.pool.usage().await?;
        let memory = usage["memoryMiB"]
            .as_u64()
            .ok_or_else(|| Error::unavailable("Node memory usage is unavailable."))?;
        let host = crate::microvm::hibernation::available_host_memory_mib()?;
        Ok(budget.limits.memory_mi_b.saturating_sub(memory).min(host))
    }

    async fn qualify_exclusive_memory(&self, ledger: &Ledger, required: u64) -> Result<()> {
        let reservation = ledger.reservation.as_ref().unwrap();
        let reason = reservation.request.pause_reason();
        let mut available = self.exclusive_memory_headroom().await?;
        for attempt in &reservation.paused_attempts {
            if available >= required {
                return Ok(());
            }
            let bytes = required.saturating_sub(available).saturating_mul(1_048_576);
            crate::microvm::hibernation::reclaim_attempt(
                &self.state,
                attempt,
                &reason,
                bytes,
                &self.stop,
            )
            .await?;
            available = self.exclusive_memory_headroom().await?;
        }
        if available < required {
            return Err(Error::unavailable(
                "The requested exclusive memory headroom is unavailable.",
            ));
        }
        Ok(())
    }

    async fn transition_exclusive(&self, id: &str, reason: &Reason, paused: bool) -> Result<()> {
        let state = self.active.lock().await.get(id).map(|attempt| {
            (
                attempt.stop.clone(),
                attempt.control.clone(),
                attempt.plan.node_lease_required(),
            )
        });
        let Some((stop, control, leased)) = state else {
            return Ok(());
        };
        if stop.is_cancelled() {
            return self.stop(id).await;
        }
        let transition = async {
            let _control = control.lock().await;
            if !paused
                && leased
                && !self
                    .leases
                    .lock()
                    .await
                    .get(id)
                    .is_some_and(|until| *until > crate::nodes::boot_ms())
            {
                suspension::settle_reason(&self.state, id, &Reason::Revoked, true, &self.stop)
                    .await?;
                stop.cancel();
                return Err(Error::conflict(
                    "Suspended execution lease expired before resume.",
                ));
            }
            let status = suspension::settle_reason(&self.state, id, reason, paused, &stop).await?;
            if !status.acknowledged {
                return Err(Error::unavailable("VM suspension has not been confirmed."));
            }
            Ok(())
        };
        tokio::time::timeout(Duration::from_secs(30), transition)
            .await
            .map_err(|_| Error::unavailable("VM suspension confirmation timed out."))?
    }

    pub async fn release_exclusive(&self, request: Release) -> Result<Value> {
        uuid(&request.id)?;
        let _admission = self.admission.lock().await;
        let mut ledger = self.exclusive.lock().await;
        self.read_exclusive(&mut ledger).await?;
        if let Some(current) = &ledger.reservation {
            if current.request.id != request.id || current.request.epoch != request.epoch {
                return Err(Error::conflict("Exclusive reservation was replaced."));
            }
            self.release_current(&mut ledger, false).await?;
        } else if ledger.last_released.as_ref() != Some(&(request.id.clone(), request.epoch)) {
            if request.epoch == 0 || request.epoch < ledger.highest_epoch {
                return Err(Error::conflict("Exclusive reservation was replaced."));
            }
            // A release that overtakes its acquisition installs a durable tombstone.
            // A late HTTP request with this epoch can never close admission again.
            ledger.highest_epoch = request.epoch;
            ledger.last_released = Some((request.id, request.epoch));
            self.save_exclusive(&ledger).await?;
        }
        self.exclusive_response(&ledger).await
    }

    /// Fence the privileged group before returning peers to the shared envelope.
    async fn release_current(&self, ledger: &mut Ledger, rollback: bool) -> Result<()> {
        let Some(reservation) = ledger.reservation.as_mut() else {
            return Ok(());
        };
        let request = reservation.request.clone();
        reservation.state = "releasing".into();
        self.save_exclusive(ledger).await?;
        if !rollback {
            for id in std::iter::once(&request.owner_attempt_id).chain(&request.member_attempt_ids)
            {
                if self
                    .transition_exclusive(id, &request.pause_reason(), true)
                    .await
                    .is_err()
                {
                    // An uncertain CPU fence is settled by reaping the privileged VMM.
                    self.stop(id).await?;
                }
            }
        }
        let peers = ledger.reservation.as_ref().unwrap().paused_attempts.clone();
        for id in peers {
            if self
                .transition_exclusive(&id, &request.pause_reason(), false)
                .await
                .is_err()
            {
                // A failed VM cannot hold healthy neighbors suspended indefinitely.
                // Reap it before considering its CPU fence released.
                self.stop(&id).await?;
            }
        }
        for id in std::iter::once(&request.owner_attempt_id).chain(&request.member_attempt_ids) {
            self.transition_exclusive(id, &request.pause_reason(), false)
                .await?;
        }
        self.pool.set_exclusive(false).await?;
        ledger.last_released = Some((request.id, request.epoch));
        ledger.reservation = None;
        self.save_exclusive(ledger).await
    }

    pub async fn administrative_pause(&self, attempt: &str) -> Result<Value> {
        uuid(attempt)?;
        Ok(json!({
            "administrativePausedMs": suspension::administrative_suspended_ms(&self.state, attempt).await?
        }))
    }

    async fn exclusive_response(&self, ledger: &Ledger) -> Result<Value> {
        let attempts = self.active.lock().await.keys().cloned().collect::<Vec<_>>();
        let mut administrative = BTreeMap::new();
        for attempt in attempts {
            administrative.insert(
                attempt.clone(),
                suspension::administrative_suspended_ms(&self.state, &attempt).await?,
            );
        }
        Ok(json!({
            "reservation": ledger.reservation.as_ref().map(Reservation::public).transpose()?,
            "highestEpoch": ledger.highest_epoch,
            "administrativePausedMs": administrative
        }))
    }

    pub async fn inspect_exclusive(&self) -> Result<Value> {
        // Read the last durable transition without waiting behind a slow VM pause.
        let ledger = match tokio::fs::read(self.state.join(FILE)).await {
            Ok(bytes) => serde_json::from_slice(&bytes)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ledger::default(),
            Err(error) => return Err(error.into()),
        };
        self.exclusive_response(&ledger).await
    }

    pub async fn monitor_exclusive(self) {
        loop {
            tokio::select! {
                () = self.stop.cancelled() => return,
                () = tokio::time::sleep(Duration::from_millis(500)) => {}
            }
            let _admission = self.admission.lock().await;
            let mut ledger = self.exclusive.lock().await;
            if let Err(error) = self.read_exclusive(&mut ledger).await {
                tracing::error!(message = %error.message, "Could not load exclusive reservation");
                continue;
            }
            let release = match &ledger.reservation {
                Some(reservation) => {
                    reservation.state != "acquired"
                        || reservation.expired(crate::nodes::boot_ms())
                        || self.validate_members(&reservation.request).await.is_err()
                }
                None => false,
            };
            if release && let Err(error) = self.release_current(&mut ledger, false).await {
                tracing::warn!(message = %error.message, "Exclusive release remains pending");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::microvm::{plan::Plan, pool::Pool};
    use std::{path::Path, sync::Arc};
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::UnixListener,
        sync::{Mutex, watch},
    };
    use tokio_util::sync::CancellationToken;

    fn request() -> Acquisition {
        Acquisition {
            id: crate::config::id(),
            epoch: 1,
            owner_attempt_id: crate::config::id(),
            member_attempt_ids: vec![crate::config::id()],
            expires_at: 1_001,
            remaining_ms: None,
            reason: "Build the workspace".into(),
            minimum_available_memory_mi_b: None,
        }
    }

    #[test]
    fn acquisition_is_bounded_and_members_are_distinct() {
        let mut value = request();
        value.validate(1_000).unwrap();
        value.expires_at = 1_000;
        assert!(value.validate(1_000).is_err());
        value.expires_at = 1_000 + i64::try_from(MAX_DURATION_MS).unwrap() + 1;
        assert!(value.validate(1_000).is_err());
        value.expires_at = 1_001;
        value
            .member_attempt_ids
            .push(value.owner_attempt_id.clone());
        assert!(value.validate(1_000).is_err());
        value.member_attempt_ids = vec![value.member_attempt_ids[0].clone(); 2];
        assert!(value.validate(1_000).is_err());
        value.member_attempt_ids.clear();
        value.epoch = 0;
        assert!(value.validate(1_000).is_err());
    }

    #[test]
    fn renewal_cannot_change_identity_or_privileged_members() {
        let value = request();
        let mut renewal = value.clone();
        renewal.expires_at += 1;
        assert!(value.same_identity(&renewal));
        renewal.member_attempt_ids.push(crate::config::id());
        assert!(!value.same_identity(&renewal));
        renewal = value.clone();
        renewal.epoch += 1;
        assert!(!value.same_identity(&renewal));
        renewal = value.clone();
        renewal.reason.push_str(" changed");
        assert!(!value.same_identity(&renewal));
    }

    #[test]
    fn remaining_duration_is_independent_of_manager_and_controller_wall_clocks() {
        let mut value = request();
        value.remaining_ms = Some(60_000);
        for clock in [-7_200_000, 0, 7_200_000] {
            value.validate(clock).unwrap();
            assert_eq!(value.duration_ms(clock).unwrap(), 60_000);
        }
        value.remaining_ms = Some(0);
        assert!(value.validate(0).is_err());
        value.remaining_ms = Some(MAX_DURATION_MS + 1);
        assert!(value.validate(0).is_err());
        value.remaining_ms = None;
        value.validate(1_000).unwrap();
        assert!(value.validate(7_200_000).is_err());
    }

    #[test]
    fn replays_cannot_extend_monotonic_expiry_and_renewals_respect_total_lifetime() {
        let mut value = request();
        value.remaining_ms = Some(60_000);
        let mut reservation = Reservation {
            request: value.clone(),
            created_at: -7_200_000,
            created_boot_ms: 1_000,
            deadline_boot_ms: 61_000,
            state: "acquired".into(),
            paused_attempts: Vec::new(),
        };
        let mut replay = value.clone();
        replay.remaining_ms = Some(30_000);
        assert!(value.same_identity(&replay));
        // A delayed replay could otherwise move expiry forward due to transit time.
        reservation.renew(&replay, 62_000).unwrap();
        assert_eq!(reservation.deadline_boot_ms, 61_000);
        assert!(!reservation.expired(60_999));
        assert!(reservation.expired(61_000));
        assert!(reservation.expired(999));
        assert!(reservation.expired(u64::MAX));

        replay.expires_at += 10_000;
        reservation.renew(&replay, 71_000).unwrap();
        assert_eq!(reservation.deadline_boot_ms, 71_000);
        replay.expires_at += 1;
        assert!(reservation.renew(&replay, 1_001 + MAX_LIFETIME_MS).is_err());
        assert_eq!(reservation.deadline_boot_ms, 71_000);
        let restored: Reservation =
            serde_json::from_slice(&serde_json::to_vec(&reservation).unwrap()).unwrap();
        assert_eq!(restored.created_boot_ms, 1_000);
        assert_eq!(restored.deadline_boot_ms, 71_000);
        let public = restored.public().unwrap();
        assert_eq!(public["expiresAt"], value.expires_at + 10_000);
        assert!(public.get("remainingMs").is_none());
        assert!(public.get("deadlineBootMs").is_none());
    }

    #[test]
    fn ledger_preserves_fencing_after_restart() {
        let value = request();
        let ledger = Ledger {
            highest_epoch: value.epoch,
            last_released: Some((value.id.clone(), value.epoch)),
            loaded: true,
            reservation: None,
        };
        let restored: Ledger =
            serde_json::from_slice(&serde_json::to_vec(&ledger).unwrap()).unwrap();
        assert_eq!(restored.highest_epoch, value.epoch);
        assert_eq!(restored.last_released, ledger.last_released);
        assert!(!restored.loaded);
    }

    async fn broker(root: &Path) -> Broker {
        let state = root.join("state");
        let data = root.join("data");
        crate::skills::private_dir(&state).await.unwrap();
        crate::skills::private_dir(&data).await.unwrap();
        let stop = CancellationToken::new();
        let pool = Pool::new(state.clone(), root.to_owned(), stop.clone(), 3)
            .await
            .unwrap();
        Broker::new(data, state, pool, stop)
    }

    async fn active_vm(broker: &Broker, changes: Arc<Mutex<Vec<String>>>) -> String {
        let id = crate::config::id();
        let vm = crate::config::id();
        let jail = broker
            .state
            .join("jails/firecracker")
            .join(&vm)
            .join("root");
        crate::skills::private_dir(&jail).await.unwrap();
        let listener = UnixListener::bind(jail.join("api.sock")).unwrap();
        let name = id.clone();
        tokio::spawn(async move {
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut bytes = vec![0; 4096];
                let size = socket.read(&mut bytes).await.unwrap();
                let message = String::from_utf8_lossy(&bytes[..size]);
                let state = if message.contains("Paused") {
                    "paused"
                } else {
                    "resumed"
                };
                changes.lock().await.push(format!("{name}:{state}"));
                socket
                    .write_all(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\n\r\n")
                    .await
                    .unwrap();
            }
        });
        suspension::prepare_attempt(&broker.state, &id, &vm)
            .await
            .unwrap();
        atomic_write(
            &broker.state_file(&id, "vm.json"),
            &serde_json::to_vec(&json!({
                "vmId": vm,
                "runId": crate::config::id()
            }))
            .unwrap(),
        )
        .await
        .unwrap();
        suspension::bind_attempt(&broker.state, &id, &vm)
            .await
            .unwrap();
        let (done, receiver) = watch::channel(true);
        // This synthetic VMM acknowledges CPU transitions and has no guest process to reap.
        drop(done);
        broker.active.lock().await.insert(
            id.clone(),
            super::super::broker::Attempt {
                stop: CancellationToken::new(),
                done: receiver,
                physical_teardown: None,
                socket: Arc::default(),
                plan: Plan::new(json!({ "runId": crate::config::id() })),
                imports: Arc::default(),
                control: Arc::default(),
            },
        );
        id
    }

    #[tokio::test]
    async fn acquisition_acknowledges_all_peers_and_release_fences_owner_first() {
        let root = tempfile::tempdir().unwrap();
        let broker = broker(root.path()).await;
        let changes: Arc<tokio::sync::Mutex<Vec<String>>> = Arc::default();
        let owner = active_vm(&broker, changes.clone()).await;
        let peer = active_vm(&broker, changes.clone()).await;
        let mut request = request();
        request.owner_attempt_id = owner.clone();
        request.member_attempt_ids.clear();
        request.expires_at = now() + 60_000;
        let response = broker.acquire_exclusive(request.clone()).await.unwrap();
        assert_eq!(response["reservation"]["state"], "acquired");
        assert!(
            suspension::inspect(&broker.state, &peer)
                .await
                .unwrap()
                .paused
        );
        assert!(
            !suspension::inspect(&broker.state, &owner)
                .await
                .unwrap()
                .paused
        );
        assert!(
            broker
                .allow_exclusive_admission(&crate::config::id())
                .await
                .is_err()
        );
        broker.allow_exclusive_admission(&owner).await.unwrap();
        let mut renewed = request.clone();
        renewed.expires_at += 1;
        broker.acquire_exclusive(renewed).await.unwrap();
        broker
            .release_exclusive(Release {
                id: request.id.clone(),
                epoch: request.epoch,
            })
            .await
            .unwrap();
        broker
            .release_exclusive(Release {
                id: request.id,
                epoch: request.epoch,
            })
            .await
            .unwrap();
        assert_eq!(
            changes.lock().await.as_slice(),
            &[
                format!("{peer}:paused"),
                format!("{owner}:paused"),
                format!("{peer}:resumed"),
                format!("{owner}:resumed"),
            ]
        );
        assert!(
            !suspension::inspect(&broker.state, &peer)
                .await
                .unwrap()
                .paused
        );
        assert!(broker.inspect_exclusive().await.unwrap()["reservation"].is_null());
    }

    #[tokio::test]
    async fn manager_clock_skew_preserves_public_expiry_without_affecting_node_deadline() {
        let root = tempfile::tempdir().unwrap();
        let broker = broker(root.path()).await;
        let changes: Arc<Mutex<Vec<String>>> = Arc::default();
        let owner = active_vm(&broker, changes).await;
        for (epoch, skew) in [(1, -7_200_000), (2, 7_200_000)] {
            let mut acquisition = request();
            acquisition.epoch = epoch;
            acquisition.owner_attempt_id = owner.clone();
            acquisition.member_attempt_ids.clear();
            acquisition.expires_at = now() + skew;
            acquisition.remaining_ms = Some(60_000);
            let boot = crate::nodes::boot_ms();
            let response = broker.acquire_exclusive(acquisition.clone()).await.unwrap();
            assert_eq!(response["reservation"]["expiresAt"], acquisition.expires_at);
            assert_eq!(response["reservation"]["state"], "acquired");
            let initial_deadline = broker
                .exclusive
                .lock()
                .await
                .reservation
                .as_ref()
                .unwrap()
                .deadline_boot_ms;
            assert!(initial_deadline >= boot + 60_000);
            assert!(initial_deadline <= crate::nodes::boot_ms() + 60_000);
            acquisition.remaining_ms = Some(59_999);
            broker.acquire_exclusive(acquisition.clone()).await.unwrap();
            assert_eq!(
                broker
                    .exclusive
                    .lock()
                    .await
                    .reservation
                    .as_ref()
                    .unwrap()
                    .deadline_boot_ms,
                initial_deadline
            );
            broker
                .release_exclusive(Release {
                    id: acquisition.id,
                    epoch: acquisition.epoch,
                })
                .await
                .unwrap();
        }
    }

    #[tokio::test]
    async fn stale_requests_and_durable_admission_cannot_replace_a_reservation() {
        let root = tempfile::tempdir().unwrap();
        let broker = broker(root.path()).await;
        let changes: Arc<tokio::sync::Mutex<Vec<String>>> = Arc::default();
        let owner = active_vm(&broker, changes).await;
        let mut request = request();
        request.owner_attempt_id = owner;
        request.member_attempt_ids.clear();
        request.expires_at = now() + 60_000;
        broker.acquire_exclusive(request.clone()).await.unwrap();
        let replacement = Broker::new(
            broker.data.clone(),
            broker.state.clone(),
            broker.pool.clone(),
            broker.stop.clone(),
        );
        assert!(
            replacement
                .allow_exclusive_admission(&crate::config::id())
                .await
                .is_err()
        );
        assert!(
            broker
                .release_exclusive(Release {
                    id: request.id.clone(),
                    epoch: request.epoch + 1
                })
                .await
                .is_err()
        );
        assert_eq!(
            broker.inspect_exclusive().await.unwrap()["reservation"]["id"],
            request.id
        );
        broker
            .release_exclusive(Release {
                id: request.id.clone(),
                epoch: request.epoch,
            })
            .await
            .unwrap();
        assert!(broker.acquire_exclusive(request.clone()).await.is_err());
        request.id = crate::config::id();
        request.epoch += 1;
        broker.acquire_exclusive(request.clone()).await.unwrap();
        broker
            .release_exclusive(Release {
                id: request.id,
                epoch: request.epoch,
            })
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn release_before_acquisition_fences_a_late_request_across_restart() {
        let root = tempfile::tempdir().unwrap();
        let broker = broker(root.path()).await;
        let mut request = request();
        request.expires_at = now() + 60_000;
        broker
            .release_exclusive(Release {
                id: request.id.clone(),
                epoch: request.epoch,
            })
            .await
            .unwrap();
        broker
            .release_exclusive(Release {
                id: request.id.clone(),
                epoch: request.epoch,
            })
            .await
            .unwrap();
        let restored = Broker::new(
            broker.data.clone(),
            broker.state.clone(),
            broker.pool.clone(),
            broker.stop.clone(),
        );
        assert!(restored.acquire_exclusive(request).await.is_err());
        restored
            .allow_exclusive_admission(&crate::config::id())
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn release_preserves_an_independent_storage_suspension() {
        let root = tempfile::tempdir().unwrap();
        let broker = broker(root.path()).await;
        let changes: Arc<tokio::sync::Mutex<Vec<String>>> = Arc::default();
        let owner = active_vm(&broker, changes.clone()).await;
        let peer = active_vm(&broker, changes.clone()).await;
        suspension::set_reason(&broker.state, &peer, &Reason::Storage, true)
            .await
            .unwrap();
        let mut request = request();
        request.owner_attempt_id = owner;
        request.member_attempt_ids.clear();
        request.expires_at = now() + 60_000;
        broker.acquire_exclusive(request.clone()).await.unwrap();
        broker
            .release_exclusive(Release {
                id: request.id,
                epoch: request.epoch,
            })
            .await
            .unwrap();
        let status = suspension::inspect(&broker.state, &peer).await.unwrap();
        assert!(status.paused);
        assert!(status.acknowledged);
        assert_eq!(status.reasons, vec![Reason::Storage]);
        assert_eq!(
            changes
                .lock()
                .await
                .iter()
                .filter(|change| change.starts_with(&peer))
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn unqualified_memory_request_rolls_back_acknowledged_peer_suspension() {
        let root = tempfile::tempdir().unwrap();
        let broker = broker(root.path()).await;
        let changes: Arc<tokio::sync::Mutex<Vec<String>>> = Arc::default();
        let owner = active_vm(&broker, changes.clone()).await;
        let peer = active_vm(&broker, changes.clone()).await;
        let mut request = request();
        request.owner_attempt_id = owner;
        request.member_attempt_ids.clear();
        request.expires_at = now() + 60_000;
        request.minimum_available_memory_mi_b = Some(1);
        assert!(broker.acquire_exclusive(request).await.is_err());
        assert!(broker.inspect_exclusive().await.unwrap()["reservation"].is_null());
        assert!(
            !suspension::inspect(&broker.state, &peer)
                .await
                .unwrap()
                .paused
        );
        assert_eq!(
            changes.lock().await.as_slice(),
            &[format!("{peer}:paused"), format!("{peer}:resumed"),]
        );
        broker
            .allow_exclusive_admission(&crate::config::id())
            .await
            .unwrap();
    }
}
