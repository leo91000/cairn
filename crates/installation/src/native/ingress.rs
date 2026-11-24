//! Admission for a single owned preparation tree. Call before allocating, not
//! after extraction: workload suspension does not stop controller writers.
use super::{disk, workspace};
use crate::{
    error::{Error, Result},
    storage,
};
use std::{
    future::Future,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

/// An isolated runtime makes its blocking filesystem workers inherit the same
/// quota boundary. Never drop capabilities on a reusable controller thread.
/// The future must own its storage/access guards; dropping the receiver cannot
/// release them while this runtime still has filesystem work to drain.
pub(super) async fn write<T: Send + 'static, G: Send + 'static>(
    owner: G,
    operation: impl Future<Output = Result<T>> + Send + 'static,
) -> Result<T> {
    let (send, receive) = tokio::sync::oneshot::channel();
    std::thread::Builder::new()
        .name("native-ingress".into())
        .spawn(move || {
            let owner = owner;
            let result = (|| {
                enforce_quota()?;
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()?;
                let result = runtime.block_on(operation);
                // Runtime drop waits for blocking filesystem operations before
                // acknowledging completion to the controller.
                drop(runtime);
                result
            })();
            drop(owner);
            let _ = send.send(result);
        })?;
    receive.await.map_err(Error::internal)?
}

fn enforce_quota() -> Result<()> {
    #[repr(C)]
    struct Header {
        version: u32,
        pid: i32,
    }
    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct Capabilities {
        effective: u32,
        permitted: u32,
        inheritable: u32,
    }
    const SYS_RESOURCE: u32 = 1 << 24;
    let header = Header {
        version: 0x2008_0522,
        pid: 0,
    };
    let mut caps = [Capabilities::default(); 2];
    if unsafe { libc::syscall(libc::SYS_capget, &header, caps.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    if caps[0].permitted & (1 << 8) != 0
        && unsafe { libc::prctl(libc::PR_CAPBSET_DROP, 24, 0, 0, 0) } != 0
    {
        return Err(std::io::Error::last_os_error().into());
    }
    caps[0].effective &= !SYS_RESOURCE;
    caps[0].permitted &= !SYS_RESOURCE;
    caps[0].inheritable &= !SYS_RESOURCE;
    if unsafe { libc::syscall(libc::SYS_capset, &header, caps.as_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}

pub(crate) struct Admission {
    data: PathBuf,
    state: PathBuf,
    target: AllocationTarget,
    filesystem: PathBuf,
    policy: storage::policy::Policy,
    limit: u64,
    initial_target: u64,
    sampled: u64,
    reservation: u64,
    progress: Option<disk::PreparationProgress>,
    credit_progress: bool,
    staging_reservation: bool,
    submitted: u64,
    observation: Observation,
}

enum AllocationTarget {
    Tree(PathBuf),
    Project(PathBuf),
}

impl AllocationTarget {
    fn allocated(&self) -> Result<u64> {
        match self {
            Self::Tree(path) => allocated(path),
            Self::Project(path) => super::quota::usage(path),
        }
    }
}

struct Observation {
    at: Instant,
    used: u64,
    submitted: u64,
    outstanding: u64,
    peer_reservations: u64,
}

impl Admission {
    /// Connector staging lives on the control filesystem, not the quota pool.
    /// Require persisted controller authority rather than inventing a fallback.
    pub(crate) fn staging(data: &Path, state: &Path, target: &Path) -> Result<Self> {
        Self::staging_with_sample(data, state, target, None)
    }

    pub(crate) fn staging_with_sample(
        data: &Path,
        state: &Path,
        target: &Path,
        sample: Option<(u64, Instant)>,
    ) -> Result<Self> {
        let policy =
            storage::policy::from_persisted(&std::fs::read(state.join("storage-policy.json"))?)?;
        policy.validate().map_err(Error::bad)?;
        let budget =
            serde_json::from_slice(&std::fs::read(state.join(crate::microvm::budget::FILE))?)?;
        let limit = super::native_budget(&budget)?.limits.disk_mi_b * disk::MIB;
        Self::measure_target(
            data,
            state,
            AllocationTarget::Tree(target.to_owned()),
            data,
            limit,
            &policy,
            sample,
        )
    }

    /// `target` is the exclusive operation's own tree, not an agent-controlled
    /// path inferred from a request. `filesystem` must be the unrestricted pool
    /// or controller filesystem: project statvfs can report a virtual quota.
    pub fn new(
        data: &Path,
        state: &Path,
        target: &Path,
        filesystem: &Path,
        limit: u64,
        policy: &storage::policy::Policy,
    ) -> Result<Self> {
        Self::measure_target(
            data,
            state,
            AllocationTarget::Tree(target.to_owned()),
            filesystem,
            limit,
            policy,
            None,
        )
    }

    /// The prepare worker may update several directories of one quota-owned
    /// workload. Use the kernel's project counter throughout these writes.
    pub(super) fn project(
        data: &Path,
        state: &Path,
        workload: PathBuf,
        limit: u64,
        policy: &storage::policy::Policy,
        sample: Option<(u64, Instant)>,
    ) -> Result<Self> {
        Self::measure_target(
            data,
            state,
            AllocationTarget::Project(workload),
            &workspace::data_root(state),
            limit,
            policy,
            sample,
        )
    }

    fn measure_target(
        data: &Path,
        state: &Path,
        target: AllocationTarget,
        filesystem: &Path,
        limit: u64,
        policy: &storage::policy::Policy,
        sample: Option<(u64, Instant)>,
    ) -> Result<Self> {
        let initial_target = target.allocated()?;
        let (used, measured) = match sample {
            Some(sample) => sample,
            None => (disk::usage_sync(data, state)?, Instant::now()),
        };
        let peer_reservations = disk::physical_preparations(state, filesystem, None)?;
        Ok(Self {
            data: data.to_owned(),
            state: state.to_owned(),
            target,
            filesystem: filesystem.to_owned(),
            policy: policy.clone(),
            limit,
            initial_target,
            sampled: used,
            reservation: 0,
            progress: None,
            credit_progress: false,
            staging_reservation: false,
            submitted: 0,
            observation: Observation {
                at: measured,
                used,
                submitted: 0,
                outstanding: 0,
                peer_reservations,
            },
        })
    }

    /// The full usage sample taken at creation, including reservations.
    pub fn sampled_usage(&self) -> u64 {
        self.sampled
    }

    pub(crate) fn reserve_staging(&self, bytes: u64) -> Result<bool> {
        let peers = disk::physical_preparations(&self.state, &self.filesystem, None)?;
        let (total, free) = storage::policy::physical_space(&self.filesystem)?;
        let available = free
            .saturating_sub(self.policy.reserve(total))
            .saturating_sub(storage::cache::pending_write_bytes())
            .saturating_sub(peers);
        Ok(bytes.saturating_add(disk::MIB) <= available)
    }

    pub(super) fn with_reservation(
        mut self,
        reservation: &disk::Preparation,
        credit_progress: bool,
    ) -> Result<Self> {
        let progress = reservation.progress()?;
        self.reservation = reservation.bytes;
        self.observation.used = self.observation.used.saturating_sub(reservation.bytes);
        self.observation.peer_reservations =
            disk::physical_preparations(&self.state, &self.filesystem, Some(&progress.path))?;
        self.progress = Some(progress);
        self.credit_progress = credit_progress;
        Ok(self)
    }

    pub(crate) fn with_new_reservation(mut self, reservation: &disk::Preparation) -> Result<Self> {
        // This sample was taken under the storage lock before publication.
        // Later samples include the reservation and subtract it in admit.
        self.reservation = reservation.bytes;
        self.progress = Some(reservation.progress()?);
        self.credit_progress = true;
        self.staging_reservation = true;
        Ok(self)
    }

    pub(crate) fn retarget_staging(&mut self, target: &Path) -> Result<()> {
        self.initial_target = allocated(target)?;
        self.target = AllocationTarget::Tree(target.to_owned());
        Ok(())
    }

    pub fn admit(&mut self, allocation: u64) -> Result<()> {
        let policy = match std::fs::read(self.state.join("storage-policy.json")) {
            Ok(bytes) => storage::policy::from_persisted(&bytes)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => self.policy.clone(),
            Err(error) => return Err(error.into()),
        };
        policy.validate().map_err(Error::bad)?;
        let limit = match std::fs::read(self.state.join(crate::microvm::budget::FILE)) {
            Ok(bytes) => {
                let value = serde_json::from_slice(&bytes)?;
                super::native_budget(&value)?.limits.disk_mi_b * disk::MIB
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => self.limit,
            Err(error) => return Err(error.into()),
        };

        if self.observation.at.elapsed() >= Duration::from_millis(250) {
            let started = Instant::now();
            let used = if self.staging_reservation {
                let progress = self
                    .progress
                    .as_ref()
                    .expect("staging reservation progress");
                disk::usage_except(&self.data, &self.state, &progress.path)?
            } else {
                disk::usage_sync(&self.data, &self.state)?.saturating_sub(self.reservation)
            };
            let growth = self.target.allocated()?.saturating_sub(self.initial_target);
            if self.credit_progress
                && let Some(progress) = &self.progress
            {
                progress.credit(growth.min(self.submitted))?;
            }
            let peer_reservations = self.peer_reservations()?;
            // Delayed allocation and small-file overhead must not erase a
            // debit merely because a new usage sample does not show it yet.
            let outstanding = self.submitted.saturating_sub(growth);
            if started.elapsed() > Duration::from_secs(5) {
                return Err(Error::unavailable(
                    "Native ingress capacity observation is stale.",
                ));
            }
            self.observation = Observation {
                at: Instant::now(),
                used: used.saturating_add(outstanding),
                submitted: self.submitted,
                outstanding,
                peer_reservations,
            };
        }
        let since = self.submitted.saturating_sub(self.observation.submitted);
        let pending = storage::cache::pending_write_bytes();
        let projected = self
            .observation
            .used
            .saturating_add(since)
            .saturating_add(allocation);
        let (total, free) = storage::policy::physical_space(&self.filesystem)?;
        let physical = free
            .saturating_sub(policy.reserve(total))
            .saturating_sub(pending)
            // Even a serialized fallback can overlap peers that released
            // storage.lock after reserving a bounded transfer.
            .saturating_sub(self.observation.peer_reservations)
            .saturating_sub(self.observation.outstanding)
            .saturating_sub(since);
        let required = allocation.saturating_add(disk::MIB);
        let enough_physical = required <= physical
            || self
                .reconciled_headroom(&policy, || {
                    storage::policy::physical_space(&self.filesystem).map_err(Error::from)
                })?
                .is_some_and(|headroom| required <= headroom);
        if !enough_physical || projected.saturating_add(pending) > limit.saturating_sub(disk::MIB) {
            return Err(Error::new(
                507,
                "Native ingress exceeds the current physical reserve or shared allocated budget.",
            ));
        }
        self.submitted = self.submitted.saturating_add(allocation);
        Ok(())
    }

    fn reconciled_headroom(
        &self,
        policy: &storage::policy::Policy,
        mut sample: impl FnMut() -> Result<(u64, u64)>,
    ) -> Result<Option<u64>> {
        let own = self
            .progress
            .as_ref()
            .map(|progress| progress.path.as_path());
        for _ in 0..3 {
            let before = sample()?;
            let peers = disk::reconciled_physical_preparations(&self.state, &self.filesystem, own)?;
            let growth = self.target.allocated()?.saturating_sub(self.initial_target);
            let after = sample()?;
            if before == after {
                let (total, free) = after;
                return Ok(Some(
                    free.saturating_sub(policy.reserve(total))
                        .saturating_sub(storage::cache::pending_write_bytes())
                        .saturating_sub(peers)
                        .saturating_sub(self.submitted.saturating_sub(growth)),
                ));
            }
        }
        Ok(None)
    }

    fn peer_reservations(&self) -> Result<u64> {
        disk::physical_preparations(
            &self.state,
            &self.filesystem,
            self.progress
                .as_ref()
                .map(|progress| progress.path.as_path()),
        )
    }
}

fn allocated(path: &Path) -> Result<u64> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
            workspace::measure(path, true)
        }
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
            use std::os::unix::fs::MetadataExt;
            Ok(metadata.blocks().saturating_mul(512))
        }
        Ok(_) => Err(Error::conflict("Native ingress target changed type.")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(0),
        Err(error) => Err(error.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn fallback_staging_cannot_consume_a_reserved_peer_streams_physical_headroom() {
        let temp = tempfile::tempdir().unwrap();
        let state = temp.path().join("state");
        let data = temp.path().join("data");
        std::fs::create_dir(&state).unwrap();
        std::fs::create_dir(&data).unwrap();
        let policy = storage::policy::Policy::default();
        let (_, free) = storage::policy::physical_space(&data).unwrap();
        let peer = disk::Preparation::reserve_on(&state, &crate::config::id(), free, &data)
            .await
            .unwrap();
        let mut fallback = Admission::new(&data, &state, &data, &data, u64::MAX, &policy).unwrap();
        assert_eq!(fallback.admit(4096).unwrap_err().status, 507);
        drop(peer);
        let mut available = Admission::new(&data, &state, &data, &data, u64::MAX, &policy).unwrap();
        available.admit(4096).unwrap();
    }

    #[tokio::test]
    #[ignore = "executed by its isolated parent"]
    async fn paused_peer_allocation_reconciles_physical_headroom_child() {
        let temp = tempfile::tempdir().unwrap();
        let state = temp.path().join("state");
        let data = temp.path().join("data");
        let first = data.join("runs/.cairn-seed-first");
        let second = data.join("runs/.cairn-seed-second");
        std::fs::create_dir_all(&first).unwrap();
        std::fs::create_dir_all(&second).unwrap();
        std::fs::create_dir(&state).unwrap();
        let policy = storage::policy::Policy::default();
        let reserve = 12 * disk::MIB;
        let _first_reservation = disk::Preparation::reserve_staging(
            &state,
            &crate::config::id(),
            reserve,
            &data,
            &first,
        )
        .await
        .unwrap();
        let admission = Admission::new(&data, &state, &second, &data, u64::MAX, &policy).unwrap();
        let second_reservation = disk::Preparation::reserve_staging(
            &state,
            &crate::config::id(),
            reserve,
            &data,
            &second,
        )
        .await
        .unwrap();
        let admission = admission.with_new_reservation(&second_reservation).unwrap();

        // The first writer is paused after its data write and before crediting
        // the receipt. There is still room for both full 12 MiB commitments.
        std::fs::write(first.join("payload"), vec![7; (10 * disk::MIB) as usize]).unwrap();
        let total = 1024 * disk::MIB;
        let free = policy.reserve(total) + 16 * disk::MIB;
        assert_eq!(admission.observation.peer_reservations, reserve);
        assert!(free - policy.reserve(total) - reserve < reserve + disk::MIB);
        assert_eq!(
            admission
                .reconciled_headroom(&policy, || Ok((total, free)))
                .unwrap(),
            Some(14 * disk::MIB)
        );
    }

    #[test]
    fn paused_peer_allocation_reconciles_physical_headroom() {
        crate::test_process::run_ignored(
            "native::ingress::tests::paused_peer_allocation_reconciles_physical_headroom_child",
        );
    }

    #[tokio::test]
    async fn bounded_seed_streams_count_materialized_peer_bytes_once() {
        let temp = tempfile::tempdir().unwrap();
        let state = temp.path().join("state");
        let data = temp.path().join("data");
        let first = data.join("runs/.cairn-seed-first");
        let second = data.join("runs/.cairn-seed-second");
        std::fs::create_dir_all(&first).unwrap();
        std::fs::create_dir_all(&second).unwrap();
        std::fs::create_dir(&state).unwrap();
        let policy = storage::policy::Policy::default();
        let limit = 30 * disk::MIB;
        let promised = 12 * disk::MIB;
        let chunk = 10 * disk::MIB;

        let first_admission = Admission::new(&data, &state, &first, &data, limit, &policy).unwrap();
        let first_reservation = disk::Preparation::reserve_staging(
            &state,
            &crate::config::id(),
            promised,
            &data,
            &first,
        )
        .await
        .unwrap();
        let mut first_admission = first_admission
            .with_new_reservation(&first_reservation)
            .unwrap();
        let second_admission =
            Admission::new(&data, &state, &second, &data, limit, &policy).unwrap();
        let second_reservation = disk::Preparation::reserve_staging(
            &state,
            &crate::config::id(),
            promised,
            &data,
            &second,
        )
        .await
        .unwrap();
        let mut second_admission = second_admission
            .with_new_reservation(&second_reservation)
            .unwrap();

        let bytes = vec![7; chunk as usize];
        for (admission, target) in [
            (&mut first_admission, &first),
            (&mut second_admission, &second),
        ] {
            admission.admit(chunk).unwrap();
            std::fs::write(target.join("payload"), &bytes).unwrap();
            admission.observation.at = Instant::now() - Duration::from_secs(1);
        }

        assert_eq!(disk::usage_sync(&data, &state).unwrap(), 2 * promised);
        first_admission.admit(promised - chunk).unwrap();
        second_admission.admit(promised - chunk).unwrap();
    }

    #[tokio::test]
    #[ignore = "requires the disposable ext4 quota pool with a privileged parent"]
    async fn kernel_privileged_parent_cannot_bypass_writer_quotas() {
        use std::io::Write;
        assert_eq!(
            std::env::var("CAIRN_NATIVE_DISPOSABLE_HOST").as_deref(),
            Ok("1")
        );
        assert_eq!(unsafe { libc::geteuid() }, 0);
        let status = std::fs::read_to_string("/proc/thread-self/status").unwrap();
        let caps = status
            .lines()
            .find_map(|line| line.strip_prefix("CapEff:\t"))
            .unwrap();
        assert_ne!(
            u64::from_str_radix(caps, 16).unwrap() & (1 << 24),
            0,
            "fixture masked the parent capability"
        );
        let state = PathBuf::from(std::env::var_os("CAIRN_NATIVE_QUOTA_TEST_STATE").unwrap());
        super::super::quota::require(&state).unwrap();
        for inodes in [false, true] {
            let run = crate::config::id();
            let control = state.join("native").join(&run);
            crate::skills::private_dir(&control).await.unwrap();
            let workload = super::super::workspace::workload(&state, &run).unwrap();
            crate::skills::private_dir(&workload).await.unwrap();
            super::super::quota::prepare(&state, &workload, 4 * disk::MIB)
                .await
                .unwrap();
            let target = workload.clone();
            write((), async move {
                let error = if inodes {
                    let mut failure = None;
                    for index in 0..10_000 {
                        if let Err(error) =
                            std::fs::File::create(target.join(format!("entry-{index}")))
                        {
                            failure = Some(error);
                            break;
                        }
                    }
                    failure.expect("writer bypassed inode quota")
                } else {
                    let mut file = std::fs::File::create(target.join("payload"))?;
                    let mut failure = None;
                    for _ in 0..128 {
                        if let Err(error) = file.write_all(&[42; 65536]) {
                            failure = Some(error);
                            break;
                        }
                    }
                    failure.expect("writer bypassed byte quota")
                };
                assert!(matches!(
                    error.raw_os_error(),
                    Some(libc::EDQUOT | libc::ENOSPC)
                ));
                Ok(())
            })
            .await
            .unwrap();
            super::super::quota::retire(&control).await.unwrap();
            assert!(!workload.exists());
        }
    }

    #[tokio::test]
    async fn owned_writer_runtime_drains_work_before_releasing_ownership() {
        struct Owner(std::sync::Arc<std::sync::atomic::AtomicBool>);
        impl Drop for Owner {
            fn drop(&mut self) {
                self.0.store(true, std::sync::atomic::Ordering::SeqCst);
            }
        }
        let released = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let checked = released.clone();
        write(Owner(released.clone()), async move {
            tokio::task::spawn_blocking(move || {
                assert!(!checked.load(std::sync::atomic::Ordering::SeqCst));
                let status = std::fs::read_to_string("/proc/thread-self/status").unwrap();
                let capabilities = status
                    .lines()
                    .find_map(|line| line.strip_prefix("CapEff:\t"))
                    .unwrap();
                assert_eq!(
                    u64::from_str_radix(capabilities, 16).unwrap() & (1 << 24),
                    0
                );
            })
            .await
            .unwrap();
            Ok(())
        })
        .await
        .unwrap();
        assert!(released.load(std::sync::atomic::Ordering::SeqCst));
    }

    // Admission subtracts process-wide pending cache writes. A concurrent
    // legacy journal write in another test would consume these exact margins.
    #[test]
    fn own_reservation_is_not_charged_twice_while_foreign_reservations_remain() {
        crate::test_process::run_ignored(
            "native::ingress::tests::own_reservation_is_not_charged_twice_while_foreign_reservations_remain_child",
        );
    }

    #[tokio::test]
    #[ignore = "executed by its isolated parent"]
    async fn own_reservation_is_not_charged_twice_while_foreign_reservations_remain_child() {
        let state = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        let own = super::super::disk::Preparation::reserve(
            state.path(),
            &crate::config::id(),
            3 * disk::MIB,
        )
        .await
        .unwrap();
        let foreign = super::super::disk::Preparation::reserve(
            state.path(),
            &crate::config::id(),
            2 * disk::MIB,
        )
        .await
        .unwrap();
        let used = disk::usage_sync(data.path(), state.path()).unwrap();
        let policy =
            storage::policy::for_node(&serde_json::json!({"reserveMiB": 64, "reservePercent": 1}))
                .unwrap();
        let target = state.path().join("incoming");
        let mut admission = Admission::new(
            data.path(),
            state.path(),
            &target,
            state.path(),
            used + disk::MIB,
            &policy,
        )
        .unwrap()
        .with_reservation(&own, true)
        .unwrap();
        admission.admit(own.bytes).unwrap();
        assert_eq!(admission.admit(1).unwrap_err().status, 507);
        drop((own, foreign));
    }

    #[test]
    fn preparation_debits_are_enforced_before_allocating_and_reserve_changes_are_immediate() {
        crate::test_process::run_ignored(
            "native::ingress::tests::preparation_debits_are_enforced_before_allocating_and_reserve_changes_are_immediate_child",
        );
    }

    #[test]
    #[ignore = "executed by its isolated parent"]
    fn preparation_debits_are_enforced_before_allocating_and_reserve_changes_are_immediate_child() {
        let state = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        let target = state.path().join("environments").join("private-build");
        let policy = storage::policy::for_node(&serde_json::json!({
            "reserveMiB": 64,
            "reservePercent": 1,
        }))
        .unwrap();
        let used = disk::usage_sync(data.path(), state.path()).unwrap();
        let mut admission = Admission::new(
            data.path(),
            state.path(),
            &target,
            state.path(),
            used + 3 * disk::MIB,
            &policy,
        )
        .unwrap();
        admission.admit(disk::MIB).unwrap();
        admission.admit(disk::MIB).unwrap();
        assert_eq!(admission.admit(1).unwrap_err().status, 507);
        assert!(!target.exists(), "admission wrote a partial before refusal");
        admission.observation.at = Instant::now() - Duration::from_secs(1);
        assert_eq!(admission.admit(1).unwrap_err().status, 507);
        assert_eq!(admission.observation.outstanding, 2 * disk::MIB);

        let (total, _) = storage::policy::physical_space(state.path()).unwrap();
        let mut reserve = policy.clone();
        reserve.reserve_mi_b = total / disk::MIB + 1;
        std::fs::write(
            state.path().join("storage-policy.json"),
            serde_json::to_vec(&reserve).unwrap(),
        )
        .unwrap();
        assert_eq!(admission.admit(0).unwrap_err().status, 507);
    }
}
