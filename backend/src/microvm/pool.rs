//! Bounded execution slots; each reservation boots its own S3-backed VM.
use super::host::Vm;
use crate::{
    error::{Error, Result},
    skills::private_dir,
    validation::text,
};
use serde_json::{Value, json};
use std::{path::PathBuf, sync::Arc};
use tokio::sync::{Mutex, OnceCell};
use tokio_util::{sync::CancellationToken, task::TaskTracker};

#[derive(Default)]
struct Slots {
    occupied: Vec<bool>,
}

impl Slots {
    fn reserve(&mut self, capacity: usize) -> Option<usize> {
        let index = match self.occupied.iter().position(|v| !v) {
            Some(index) => index,
            None if self.occupied.len() < capacity => {
                self.occupied.push(false);
                self.occupied.len() - 1
            }
            None => return None,
        };
        self.occupied[index] = true;
        Some(index + 1)
    }
}

pub struct Reservation {
    pool: Arc<Pool>,
    released: bool,
    slot: usize,
    vm: Option<Vm>,
}

pub struct Pool {
    capacity: usize,
    state: PathBuf,
    image: PathBuf,
    slots: Mutex<Slots>,
    stop: CancellationToken,
    cleanup: TaskTracker,
}

impl Pool {
    pub async fn new(
        state: PathBuf,
        image: PathBuf,
        stop: CancellationToken,
        capacity: usize,
    ) -> Result<Arc<Self>> {
        if capacity == 0 {
            return Err(Error::bad("VM concurrency must be a positive integer."));
        }
        // The controller lock and PID namespace fence all previous owners before this cleanup.
        let prepared = state.join("prepared");
        if prepared.exists() {
            tokio::fs::remove_dir_all(&prepared).await?;
        }
        private_dir(&state.join("disks")).await?;
        Ok(Arc::new(Self {
            capacity,
            state,
            image,
            slots: Mutex::new(Slots::default()),
            stop,
            cleanup: TaskTracker::new(),
        }))
    }

    pub async fn health(&self) -> Value {
        let slots = self.slots.lock().await;
        json!({
            "capacity": self.capacity,
            "occupied": slots.occupied.iter().filter(|v| **v).count(),
            "ready": 0,
            "preparing": false
        })
    }

    pub async fn reserve(self: &Arc<Self>, _run_id: &str) -> Result<Reservation> {
        let mut slots = self.slots.lock().await;
        if self.stop.is_cancelled() {
            return Err(Error::unavailable("VM controller is stopping."));
        }
        let slot = slots
            .reserve(self.capacity)
            .ok_or_else(|| Error::unavailable("All VM slots are occupied."))?;
        Ok(Reservation {
            pool: self.clone(),
            released: false,
            slot,
            vm: None,
        })
    }

    pub async fn drain(&self) {
        self.cleanup.close();
        self.cleanup.wait().await;
    }
}

impl Reservation {
    pub async fn execute(
        mut self,
        plan: Value,
        socket: Arc<OnceCell<PathBuf>>,
        stop: CancellationToken,
    ) -> Result<i32> {
        let disk = self.pool.state.join("disks").join(text(&plan, "runId"));
        if stop.is_cancelled() {
            self.finish().await;
            return Ok(143);
        }
        let operation = async {
            let mut timing = crate::performance::Operation::new(
                "conversation_start",
                text(&plan, "runId"),
                "prepare_disk",
            );
            if !plan["storage"].is_object() {
                return Err(Error::conflict(
                    "S3-backed storage is required for VM execution.",
                ));
            }
            let size = plan["resources"]["diskMiB"].as_u64().unwrap_or(32768) * 1024 * 1024;
            crate::storage::bootstrap::prepare(&disk, size, &plan["storage"], &stop).await?;
            timing.next("boot_vm");
            self.vm = Some(
                Vm::boot(
                    &self.pool.state,
                    &self.pool.image,
                    disk,
                    self.slot,
                    &stop,
                    plan.get("resources"),
                )
                .await?,
            );
            let vm = self.vm.as_mut().unwrap();
            let _ = socket.set(vm.socket.clone());
            timing.finish();
            vm.execute(&plan, &self.pool.state, stop.clone()).await
        };
        // Do not drop boot or cleanup futures on cancellation: their resource ownership must drain.
        let result = operation.await;
        // Captures share this attempt's lifetime. Release pending thaw retries
        // before shutdown removes the guest socket, including normal completion.
        stop.cancel();
        self.finish().await;
        result
    }

    async fn finish(&mut self) {
        if let Some(mut vm) = self.vm.take() {
            vm.shutdown().await;
        }
        self.pool.slots.lock().await.occupied[self.slot - 1] = false;
        self.released = true;
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        if self.released {
            return;
        }
        let pool = self.pool.clone();
        let mut vm = self.vm.take();
        let slot = self.slot;
        self.pool.cleanup.spawn(async move {
            if let Some(vm) = &mut vm {
                vm.shutdown().await;
            }
            pool.slots.lock().await.occupied[slot - 1] = false;
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn finished_execution_cancels_captures_before_releasing_its_slot() {
        let root = tempfile::tempdir().unwrap();
        let pool = Pool::new(
            root.path().into(),
            root.path().into(),
            CancellationToken::new(),
            1,
        )
        .await
        .unwrap();
        let stop = CancellationToken::new();
        let reservation = pool.reserve("run").await.unwrap();
        // Rejecting an invalid plan reaches the same completion/cleanup path as
        // a guest returning its exit status, without requiring KVM in this test.
        assert!(
            reservation
                .execute(json!({"runId":"run"}), Default::default(), stop.clone())
                .await
                .is_err()
        );
        assert!(
            stop.is_cancelled(),
            "captures must not retry a finished VM's deleted socket"
        );
        assert_eq!(pool.health().await["occupied"], 0);
    }

    #[tokio::test]
    async fn abandoned_reservations_release_capacity_and_shutdown_rejects_work() {
        let root = tempfile::tempdir().unwrap();
        let stop = CancellationToken::new();
        let pool = Pool::new(root.path().into(), root.path().into(), stop.clone(), 12)
            .await
            .unwrap();
        let mut reservations = Vec::new();
        assert_eq!(pool.health().await["capacity"], 12);
        for _ in 0..12 {
            reservations.push(pool.reserve("run").await.unwrap());
        }
        assert_eq!(pool.health().await["occupied"], 12);
        assert!(pool.reserve("run").await.is_err());
        drop(reservations.pop());
        tokio::time::timeout(Duration::from_secs(1), async {
            while pool.health().await["occupied"] == 12 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        reservations.push(pool.reserve("run").await.unwrap());
        assert_eq!(pool.health().await["occupied"], 12);
        stop.cancel();
        assert!(pool.reserve("run").await.is_err());
        drop(reservations);
        pool.drain().await;
        assert_eq!(pool.health().await["occupied"], 0);
    }

    #[tokio::test]
    async fn large_capacity_allocates_slots_on_demand_without_truncation() {
        let root = tempfile::tempdir().unwrap();
        let pool = Pool::new(
            root.path().into(),
            root.path().into(),
            CancellationToken::new(),
            usize::MAX,
        )
        .await
        .unwrap();
        assert!(pool.slots.lock().await.occupied.is_empty());
        let mut reservations = Vec::new();
        for slot in 1..=300 {
            let reservation = pool.reserve("run").await.unwrap();
            assert_eq!(reservation.slot, slot);
            reservations.push(reservation);
        }
        drop(reservations);
        pool.drain().await;
        assert_eq!(pool.health().await["occupied"], 0);
        assert!(
            Pool::new(
                root.path().into(),
                root.path().into(),
                CancellationToken::new(),
                0
            )
            .await
            .is_err()
        );
    }

    #[tokio::test]
    async fn restart_removes_only_unassigned_disks() {
        let root = tempfile::tempdir().unwrap();
        for directory in ["prepared/orphan", "disks/conversation"] {
            tokio::fs::create_dir_all(root.path().join(directory))
                .await
                .unwrap();
            tokio::fs::write(root.path().join(directory).join("sentinel"), b"saved")
                .await
                .unwrap();
        }
        let pool = Pool::new(
            root.path().into(),
            root.path().into(),
            CancellationToken::new(),
            4,
        )
        .await
        .unwrap();
        assert!(!root.path().join("prepared/orphan").exists());
        assert_eq!(
            tokio::fs::read(root.path().join("disks/conversation/sentinel"))
                .await
                .unwrap(),
            b"saved"
        );
        assert_eq!(pool.health().await["occupied"], 0);
    }
}
